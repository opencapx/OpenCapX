import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import DOMPurify from "dompurify";
import { marked } from "marked";
import { t } from "../i18n";
import {
  probeStatusLabel,
  showHealthDialog,
  showInstallPreviewDialog,
  showLifecycleDialog,
  showProbeDialog,
  showUninstallPreviewDialog,
  showWarningConfirmDialog,
  type PluginPreview,
  type UninstallPreview,
} from "./dialogs";
import { showTraceDialog } from "./rpc-trace";
import { esc, escAttr, group, openPluginSettingsPage, refreshPluginConfig, setPluginNameById } from "./shared";

export function renderPlugins(body: HTMLElement): void {
  body.innerHTML =
    group("tabPlugins",
      `<label class="setting-row plugin-policy-row"><div class="setting-info"><span class="setting-label">${esc(t("pluginAllowUnsigned"))}</span><span class="setting-hint">${esc(t("pluginsHint"))}</span></div><input type="checkbox" id="allow-unsigned"/></label>
      <label class="setting-row plugin-policy-row"><div class="setting-info"><span class="setting-label">${esc(t("pluginSandboxEnforcement"))}</span></div><input type="checkbox" id="sandbox-enforcement"/></label>
      <div class="setting-row plugin-install-row"><button class="btn" id="plugin-install" type="button">${esc(t("pluginInstall"))}</button><span class="setting-hint" id="plugin-install-msg"></span></div>`)
    + `<div id="plugin-list" class="plugin-list"></div>`
    + group(null,
      `<div class="setting-row vertical"><span class="setting-hint">${esc(t("depGraphHint"))}</span><div><button class="btn ghost" id="dep-refresh" type="button">${esc(t("depGraphRefresh"))}</button><span class="setting-hint" id="dep-msg"></span></div><div id="dep-graph"></div></div>`)
    + group(null,
      `<div class="setting-row vertical"><span class="setting-hint">${esc(t("lifecyclePlanHint"))}</span><div><button class="btn ghost" id="lifecycle-refresh" type="button">${esc(t("lifecycleRefresh"))}</button><button class="btn ghost" id="lifecycle-start-all" type="button">${esc(t("lifecycleStartAll"))}</button><button class="btn ghost" id="lifecycle-stop-all" type="button">${esc(t("lifecycleStopAll"))}</button><span class="setting-hint" id="lifecycle-msg"></span></div><div id="lifecycle-plan"></div></div>`)
    + group(null,
      `<div class="setting-row vertical"><span class="setting-hint">${esc(t("heatmapHint"))}</span><div><button class="btn ghost" id="heatmap-refresh" type="button">${esc(t("heatmapRefresh"))}</button><span class="setting-hint" id="heatmap-msg"></span></div><div id="heatmap-grid"></div><div class="stat-sub">${esc(t("heatmapTopDenied"))}</div><div id="heatmap-top"></div></div>`);
  document.getElementById("plugin-install")?.addEventListener("click", () => void pickAndInstallPlugin());
  void initAllowUnsigned();
  void initSandboxEnforcement();
  document.getElementById("dep-refresh")?.addEventListener("click", () => void refreshDepGraph());
  document.getElementById("heatmap-refresh")?.addEventListener("click", () => void refreshHeatmap());
  document.getElementById("lifecycle-refresh")?.addEventListener("click", () => void refreshLifecyclePlan());
  document.getElementById("lifecycle-start-all")?.addEventListener("click", () => void onStartAllPlugins());
  document.getElementById("lifecycle-stop-all")?.addEventListener("click", () => void onStopAllPlugins());
  void refreshPlugins();
  void refreshDepGraph();
  void refreshHeatmap();
  void refreshLifecyclePlan();
}

interface PluginStatus {
  id: string;
  name: string;
  description?: string;
  author?: string;
  homepage?: string;
  license?: string;
  version: string;
  status: string;
  capabilities: string[];
  permissions: string[];
  path?: string;
  autoReload: boolean;
  /// Phase 37 — probe status ('passed' / 'failed' / 'pending' / 'skipped'); undefined if never run.
  probeStatus?: string;
  /// Phase 37 — timestamp when probe ran (epoch seconds).
  probeAt?: number;
  /// Phase 38 — plugin self-update channel ('stable' / 'beta' / 'dev'); undefined if unset.
  channel?: string;
  /// S5 — whether a sandbox block is declared (declared != execution enabled).
  sandboxDeclared?: boolean;
  /// Phase 40 — watchdog heartbeat interval (seconds). 0 = ping off, falling back to the 500ms is_alive check.
  healthHeartbeatSec?: number;
  /// Phase 40 — failure retry limit. Exceeding it -> switch to status='error' + emit plugin.watchdog_disabled.
  /// 0 = disable the watchdog.
  healthMaxRetries?: number;
  /// Phase 40 — watchdog master switch. false = the current watchdog exits immediately.
  healthEnabled?: boolean;
  /// F5 — unmet plugin dependencies (missing install or version too low); a non-empty list shows a warning on the plugin row.
  missingDependencies?: { id: string; requirement: string }[];
  /// M4 — revocation hit: non-empty when the publisher key is in the registry revokedKeys; the plugin row shows a disabled banner.
  revokedKey?: string;
  /// M4 — revocation hit time (epoch seconds).
  revokedAt?: number;
}

interface PluginUpdateInfo {
  id: string;
  currentVersion: string;
  latestVersion: string;
  downloadUrl: string;
  sha256: string;
  /// Phase 38 — the channel the update came from, used to color the update badge.
  channel?: string;
}

/// F7 — update preview response (preview_update command): unified rendering + install after confirmation without a second download.
interface UpdatePreviewResponse {
  preview: PluginPreview;
  archivePath: string;
  currentVersion: string;
  publisherChange?: { from?: string | null; to?: string | null } | null;
}

const CHANNEL_OPTIONS: Array<{ key: string; i18nKey: string }> = [
  { key: "stable", i18nKey: "channelStable" },
  { key: "beta", i18nKey: "channelBeta" },
  { key: "dev", i18nKey: "channelDev" },
];

function channelBadgeHtml(channel: string | undefined): string {
  if (!channel) return "";
  return `<span class="channel-badge channel-${escAttr(channel)}" title="${esc(t("channelBadgeTitle"))}">${esc(channel)}</span>`;
}

let pluginUpdates: Record<string, PluginUpdateInfo> = {};

interface PermissionEntry {
  permission: string;
  decision: string;
  default: string;
  high_risk: boolean;
  /// permission-domains §4.3: declared derived permissions are once-only; the UI offers no granted option.
  declared: boolean;
}

interface PluginPermissions {
  plugin_id: string;
  name: string;
  status: string;
  permissions: PermissionEntry[];
}

/// Plugin detail (clicking a list item enters detail): the plugin id of the current detail page, null = list mode.
/// Kept at module level — operations inside the detail (toggle/update/permissions) re-render the whole card, so the selected state must not be lost.
let pluginDetailId: string | null = null;

/// F7 — 'Allow unsigned packages' master switch: read current value + write back (default ON; when OFF, unsigned/unknown-key packages are hard-rejected).
async function initAllowUnsigned(): Promise<void> {
  const box = document.getElementById("allow-unsigned") as HTMLInputElement | null;
  if (!box) return;
  try {
    box.checked = await invoke<boolean>("get_allow_unsigned");
  } catch {
    box.checked = true;
  }
  box.addEventListener("change", async () => {
    try {
      await invoke("set_allow_unsigned", { on: box.checked });
    } catch (e) {
      box.checked = !box.checked;
      const m = document.getElementById("plugin-install-msg");
      if (m) m.textContent = `✗ ${String(e)}`;
    }
  });
}

/// S5b — 'Sandbox execution' toggle (macOS; default OFF = soak first; sandbox-declaring unverified plugins are always forced).
async function initSandboxEnforcement(): Promise<void> {
  const box = document.getElementById("sandbox-enforcement") as HTMLInputElement | null;
  if (!box) return;
  try {
    box.checked = await invoke<boolean>("get_sandbox_enforcement");
  } catch {
    box.checked = false;
  }
  box.addEventListener("change", async () => {
    try {
      await invoke("set_sandbox_enforcement", { on: box.checked });
    } catch (e) {
      box.checked = !box.checked;
      const m = document.getElementById("plugin-install-msg");
      if (m) m.textContent = `✗ ${String(e)}`;
    }
  });
}

/// Shared 'preview -> confirm -> install (including key-change re-confirmation)' sequence: shared by the install path and the local-file update path,
/// guaranteeing the `publisher-key-change-confirm-required` retry logic exists in one place and cannot drift between two.
/// guard: may veto after preview (local-file update uses it to verify the package id against the target plugin id); on refusal it throws a string message.
/// Returns the id after install; cancelling at any step -> null (the caller handles it silently).
async function installPreviewedPlugin(
  path: string,
  opts?: { mode?: "install" | "update"; guard?: (preview: PluginPreview) => string | null },
): Promise<string | null> {
  const preview = await invoke<PluginPreview>("preview_ocplugin", { path });
  const refuse = opts?.guard?.(preview);
  if (refuse) throw refuse;
  const ok = await showInstallPreviewDialog(preview, { mode: opts?.mode ?? "install" });
  if (!ok) return null;
  const softWarn = preview.signature.status === "unsigned" || preview.signature.status === "unknown-key";
  const progress = document.getElementById("plugin-install-msg");
  if (progress) progress.textContent = t("pluginInstalling");
  try {
    return await invoke<string>("install_ocplugin", { path, confirmUnsigned: softWarn });
  } catch (e) {
    const text = String(e);
    if (!text.includes("publisher-key-change-confirm-required")) throw e;
    // F7 — key change = new publisher: after warning confirmation, retry with confirm.
    const go = await showWarningConfirmDialog({
      title: t("pluginRiskConfirmTitle"),
      body: text,
      confirmLabel: t("installPreviewConfirm"),
    });
    if (!go) return null;
    return await invoke<string>("install_ocplugin", {
      path,
      confirmUnsigned: softWarn,
      confirmKeyChange: true,
    });
  }
}

async function pickAndInstallPlugin(): Promise<void> {
  const msg = document.getElementById("plugin-install-msg");
  try {
    const picked = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "OpenCapX Plugin", extensions: ["ocplugin"] }],
    });
    if (!picked) return;
    const path = typeof picked === "string" ? picked : picked;
    // Wow 5 + F7 — preview before installing (tri-state badge + registry verification / official / compatible / diff);
    // warning tiers require an explicit checkbox in the dialog; only after user confirmation is it actually unpacked.
    const id = await installPreviewedPlugin(path);
    if (!id) return;
    if (msg) msg.textContent = `✓ ${id}`;
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
  await refreshPlugins();
}

// Phase 32 — capability dependency graph. Nodes = installed plugins (circle + id), edges = shared capabilities (lines + hover highlight).
interface DependencyNode {
  id: string;
  name: string;
  capabilities: string[];
}
interface DependencyEdge {
  from: string;
  to: string;
  shared: string[];
}
interface DependencyGraph {
  nodes: DependencyNode[];
  edges: DependencyEdge[];
}

function renderDepGraph(graph: DependencyGraph): string {
  const n = graph.nodes.length;
  if (n === 0) return `<p class="empty">${esc(t("depGraphEmpty"))}</p>`;
  const w = 520;
  const h = 360;
  const cx = w / 2;
  const cy = h / 2;
  const r = Math.min(w, h) / 2 - 60;
  // Nodes evenly distributed around the circle
  const pos = new Map<string, { x: number; y: number }>();
  graph.nodes.forEach((nd, i) => {
    const angle = (i / n) * Math.PI * 2 - Math.PI / 2;
    pos.set(nd.id, { x: cx + r * Math.cos(angle), y: cy + r * Math.sin(angle) });
  });
  // Edges
  const edges = graph.edges
    .map((e) => {
      const a = pos.get(e.from);
      const b = pos.get(e.to);
      if (!a || !b) return "";
      return `<line class="dep-edge" data-from="${escAttr(e.from)}" data-to="${escAttr(e.to)}" x1="${a.x.toFixed(1)}" y1="${a.y.toFixed(1)}" x2="${b.x.toFixed(1)}" y2="${b.y.toFixed(1)}"><title>${esc(e.shared.join(", "))}</title></line>`;
    })
    .join("");
  // Nodes
  const nodes = graph.nodes
    .map((nd) => {
      const p = pos.get(nd.id)!;
      const tip = `${nd.name}\n${nd.capabilities.join(", ") || "(no capabilities)"}\nedges: ${graph.edges.filter((e) => e.from === nd.id || e.to === nd.id).length}`;
      const idShort = nd.id.replace(/^com\.opencapx\./, "");
      return `<g class="dep-node" data-id="${escAttr(nd.id)}"><circle cx="${p.x.toFixed(1)}" cy="${p.y.toFixed(1)}" r="22"><title>${esc(tip)}</title></circle><text x="${p.x.toFixed(1)}" y="${(p.y + 38).toFixed(1)}" text-anchor="middle" font-size="11" font-family="ui-monospace,monospace" fill="currentColor">${esc(idShort.slice(0, 22))}</text></g>`;
    })
    .join("");
  return `<svg class="dep-graph" viewBox="0 0 ${w} ${h}" width="100%" style="overflow:visible"><g class="dep-edges">${edges}</g><g class="dep-nodes">${nodes}</g></svg>`;
}

async function refreshDepGraph(): Promise<void> {
  const box = document.getElementById("dep-graph");
  const msg = document.getElementById("dep-msg");
  if (!box) return;
  try {
    const graph = await invoke<DependencyGraph>("list_plugin_dependency_graph");
    if (graph.nodes.length === 0) {
      box.innerHTML = `<p class="empty">${esc(t("depGraphEmpty"))}</p>`;
      if (msg) msg.textContent = "";
      return;
    }
    box.innerHTML = renderDepGraph(graph);
    if (msg) msg.textContent = `${graph.nodes.length} ${esc(t("depGraphNodes"))} · ${graph.edges.length} ${esc(t("depGraphEdges"))}`;
    // hover highlight: mouse enters a node -> highlight its edges + its neighbors
    box.querySelectorAll<SVGGElement>(".dep-node").forEach((g) => {
      const id = g.dataset.id;
      g.addEventListener("mouseenter", () => {
        box.querySelectorAll<SVGLineElement>(".dep-edge").forEach((line) => {
          const touches = line.dataset.from === id || line.dataset.to === id;
          line.classList.toggle("active", touches);
        });
        g.classList.add("active");
      });
      g.addEventListener("mouseleave", () => {
        box.querySelectorAll(".dep-edge.active").forEach((el) => el.classList.remove("active"));
        g.classList.remove("active");
      });
    });
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
}

// Phase 42 — start / stop topological sort plan.
// layers[i] = the set of plugins started in parallel at step i; stop_order = layers reversed + each layer reversed.
interface LifecyclePlan {
  layers: string[][];
  stopOrder: string[];
  edges: DependencyEdge[];
}

interface StartAllResult {
  started: number;
  errors: string[];
}

function renderLifecyclePlan(plan: LifecyclePlan): string {
  if (plan.layers.every((l) => l.length === 0)) {
    return `<p class="empty">${esc(t("lifecycleEmpty"))}</p>`;
  }
  const layerHtml = plan.layers
    .map((layer, idx) => {
      const chips = layer
        .map((id) => `<span class="lifecycle-chip">${esc(id)}</span>`)
        .join(" ");
      const arrow =
        idx < plan.layers.length - 1
          ? `<span class="lifecycle-arrow">↓</span>`
          : "";
      return `<div class="lifecycle-layer"><div class="lifecycle-layer-label">${esc(t("lifecycleLayer"))} ${idx + 1}</div><div class="lifecycle-chips">${chips}</div>${arrow}</div>`;
    })
    .join("");
  const stopHtml = plan.stopOrder
    .map((id) => `<span class="lifecycle-chip stop">${esc(id)}</span>`)
    .join(" → ");
  return `<div class="lifecycle-start"><div class="lifecycle-section-label">${esc(t("lifecycleStartLabel"))}</div>${layerHtml}</div><div class="lifecycle-stop"><div class="lifecycle-section-label">${esc(t("lifecycleStopLabel"))}</div><div class="lifecycle-stop-order">${stopHtml}</div></div>`;
}

async function refreshLifecyclePlan(): Promise<void> {
  const box = document.getElementById("lifecycle-plan");
  const msg = document.getElementById("lifecycle-msg");
  if (!box) return;
  try {
    const plan = await invoke<LifecyclePlan>("get_plugin_lifecycle_plan");
    const totalPlugins = plan.layers.reduce((s, l) => s + l.length, 0);
    if (totalPlugins === 0) {
      box.innerHTML = `<p class="empty">${esc(t("lifecycleEmpty"))}</p>`;
      if (msg) msg.textContent = "";
      return;
    }
    box.innerHTML = renderLifecyclePlan(plan);
    if (msg)
      msg.textContent = `${totalPlugins} ${esc(t("lifecyclePlugins"))} · ${plan.layers.length} ${esc(t("lifecycleLayers"))}`;
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
}

async function onStartAllPlugins(): Promise<void> {
  const msg = document.getElementById("lifecycle-msg");
  const btn = document.getElementById(
    "lifecycle-start-all",
  ) as HTMLButtonElement | null;
  if (!btn) return;
  if (!window.confirm(t("lifecycleStartConfirm"))) return;
  btn.disabled = true;
  try {
    const res = await invoke<StartAllResult>("start_all_plugins");
    if (msg) {
      if (res.errors.length === 0) {
        msg.textContent = `✓ ${res.started} ${esc(t("lifecycleStarted"))}`;
      } else {
        msg.textContent = `${res.started} ✓ · ${res.errors.length} ✗`;
      }
    }
    void refreshPlugins();
    void refreshLifecyclePlan();
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  } finally {
    btn.disabled = false;
  }
}

async function onStopAllPlugins(): Promise<void> {
  const msg = document.getElementById("lifecycle-msg");
  const btn = document.getElementById(
    "lifecycle-stop-all",
  ) as HTMLButtonElement | null;
  if (!btn) return;
  if (!window.confirm(t("lifecycleStopConfirm"))) return;
  btn.disabled = true;
  try {
    const stopped = await invoke<number>("stop_all_plugins");
    if (msg) msg.textContent = `✓ ${stopped} ${esc(t("lifecycleStopped"))}`;
    void refreshPlugins();
    void refreshLifecyclePlan();
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  } finally {
    btn.disabled = false;
  }
}
// Top denied = denied ranking aggregated globally by permission (denied desc, granted desc, name asc).
interface HeatmapCell {
  pluginId: string;
  permission: string;
  decision: string;
  highRisk: boolean;
}
interface TopPermission {
  permission: string;
  grantedCount: number;
  deniedCount: number;
  askCount: number;
  highRisk: boolean;
}
interface PermissionHeatmap {
  cells: HeatmapCell[];
  topDenied: TopPermission[];
}

function renderHeatmap(heatmap: PermissionHeatmap): { grid: string; top: string } {
  if (heatmap.cells.length === 0) {
    return {
      grid: `<p class="empty">${esc(t("heatmapEmpty"))}</p>`,
      top: "",
    };
  }
  // X axis = plugin (row), Y axis = permission (column); aggregated data
  const plugins = Array.from(new Set(heatmap.cells.map((c) => c.pluginId))).sort();
  const perms = Array.from(new Set(heatmap.cells.map((c) => c.permission))).sort();
  const cellMap = new Map<string, HeatmapCell>();
  for (const c of heatmap.cells) cellMap.set(`${c.pluginId}|${c.permission}`, c);

  const headerCells = perms.map((p) => {
    const isHigh = heatmap.topDenied.find((t) => t.permission === p)?.highRisk ?? false;
    const label = p.replace(/^.*\./, ""); // shorten image.read -> read
    const cls = isHigh ? "heatmap-col high-risk" : "heatmap-col";
    return `<th class="${cls}" title="${escAttr(p)}">${esc(label)}</th>`;
  }).join("");

  const rows = plugins.map((plugin) => {
    const idShort = plugin.replace(/^com\.opencapx\./, "");
    const cells = perms.map((perm) => {
      const c = cellMap.get(`${plugin}|${perm}`);
      if (!c) return `<td class="heatmap-cell empty" title="${escAttr(plugin)} · ${escAttr(perm)}">·</td>`;
      const cls = `heatmap-cell ${c.decision}${c.highRisk ? " high-risk" : ""}`;
      const tip = `${plugin} · ${perm} · ${c.decision}${c.highRisk ? " (high-risk)" : ""}`;
      const icon = c.decision === "granted" ? "✓" : c.decision === "denied" ? "✗" : "?";
      return `<td class="${cls}" title="${escAttr(tip)}">${icon}</td>`;
    }).join("");
    return `<tr><th class="heatmap-row" title="${escAttr(plugin)}">${esc(idShort.slice(0, 22))}</th>${cells}</tr>`;
  }).join("");

  const grid = `<table class="heatmap"><thead><tr><th></th>${headerCells}</tr></thead><tbody>${rows}</tbody></table>`;

  const top = heatmap.topDenied.length === 0
    ? `<p class="muted">${esc(t("heatmapNoTop"))}</p>`
    : `<ul class="heatmap-top">${heatmap.topDenied
        .map((t) => {
          const cls = t.highRisk ? "heatmap-top-row high-risk" : "heatmap-top-row";
          const pieces: string[] = [];
          if (t.grantedCount > 0) pieces.push(`<span class="granted">${t.grantedCount} ✓</span>`);
          if (t.deniedCount > 0) pieces.push(`<span class="denied">${t.deniedCount} ✗</span>`);
          if (t.askCount > 0) pieces.push(`<span class="ask">${t.askCount} ?</span>`);
          return `<li class="${cls}"><span class="heatmap-perm">${esc(t.permission)}</span>${pieces.join(" ")}</li>`;
        })
        .join("")}</ul>`;

  return { grid, top };
}

async function refreshHeatmap(): Promise<void> {
  const gridBox = document.getElementById("heatmap-grid");
  const topBox = document.getElementById("heatmap-top");
  const msg = document.getElementById("heatmap-msg");
  if (!gridBox || !topBox) return;
  try {
    const data = await invoke<PermissionHeatmap>("list_permission_heatmap");
    const { grid, top } = renderHeatmap(data);
    gridBox.innerHTML = grid;
    topBox.innerHTML = top;
    if (msg) msg.textContent = `${data.cells.length} ${esc(t("heatmapCells"))}`;
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
}

export async function refreshPlugins(): Promise<void> {
  const box = document.getElementById("plugin-list");
  let plugins: PluginStatus[] = [];
  let perms: PluginPermissions[] = [];
  try {
    plugins = await invoke<PluginStatus[]>("list_plugins");
    perms = await invoke<PluginPermissions[]>("list_permissions");
  } catch {
    /* commands unavailable */
  }
  // Sidebar 'Plugin Settings' section: display names come from this list_plugins call; then refresh that section along with install/uninstall/update
  // (refreshPluginConfig updates the cache and entries, even when not currently on a plugin-related tab).
  setPluginNameById(new Map(plugins.map((p) => [p.id, p.name])));
  void refreshPluginConfig().catch(() => undefined);
  if (!box) return;
  // Check for updates (started in parallel; on failure treat as no update)
  pluginUpdates = {};
  try {
    const updates = await invoke<PluginUpdateInfo[]>("check_plugin_updates");
    for (const u of updates) pluginUpdates[u.id] = u;
  } catch {
    /* marketplace not configured */
  }
  if (plugins.length === 0) {
    box.innerHTML = `<p class="empty">${esc(t("pluginsNone"))}</p>`;
    return;
  }
  const permByPlugin = new Map(perms.map((p) => [p.plugin_id, p]));
  // Detail mode: the list exits and only the selected plugin + README render. If the selected plugin no longer exists (uninstalled) -> fall back to the list.
  let detail = plugins.find((p) => p.id === pluginDetailId) ?? null;
  if (pluginDetailId && !detail) pluginDetailId = null;
  // Single-plugin card HTML: in list mode the title row is clickable to enter details; detail mode adds a row of author/license/homepage meta.
  const pluginCardHtml = (p: PluginStatus, detail: boolean): string => {
      const running = p.status === "running";
      const update = pluginUpdates[p.id];
      const updateBadge = update
        ? `<span class="plugin-update-badge channel-${esc(update.channel ?? "stable")}" title="${esc(t("pluginUpdateAvailable"))}">↑ v${esc(update.latestVersion)}</span>`
          : "";
      const updateBtn = update
        ? `<button class="btn ghost" data-plugin-update="${escAttr(p.id)}" type="button">${esc(t("pluginUpdate"))}</button>`
        : "";
      // Local-file update: packages the channel/marketplace can't see (installed manually from .ocplugin) also get a non-destructive update entry,
      // right next to the channel update button, read as its 'local counterpart'.
      const updateFileBtn = `<button class="btn ghost" data-plugin-update-file="${escAttr(p.id)}" type="button">${esc(t("pluginUpdateFromFile"))}</button>`;
      const probeBadge = p.probeStatus
        ? `<span class="probe-status probe-${escAttr(p.probeStatus)}" title="${esc(p.probeAt ? new Date(p.probeAt * 1000).toLocaleString() : "")}">${esc(probeStatusLabel(p.probeStatus))}</span>`
        : "";
      const channelSel = `<select class="channel-select" data-plugin-channel="${escAttr(p.id)}" title="${esc(t("channelSelectHint"))}">${CHANNEL_OPTIONS.map(
        (o) => `<option value="${escAttr(o.key)}"${(p.channel ?? "stable") === o.key ? " selected" : ""}>${esc(t(o.i18nKey as never))}</option>`,
      ).join("")}</select>`;
      const channelBadge = channelBadgeHtml(p.channel);
      const missingDepsList = p.missingDependencies ?? [];
      const missingDeps = missingDepsList.length > 0
        ? `<div class="plugin-desc warn">${esc(t("pluginMissingDeps"))}: ${missingDepsList.map((d) => `${esc(d.id)} (${esc(d.requirement)})`).join(", ")}</div>`
        : "";
      const revokedBanner = p.revokedKey
        ? `<div class="plugin-desc warn">${esc(t("pluginRevoked"))} (${esc(p.revokedKey)}) <button class="btn ghost" data-plugin-reopen="${escAttr(p.id)}" type="button">${esc(t("pluginReopen"))}</button></div>`
        : "";
      const head = `<div class="plugin-card">
        <div class="plugin-title-line"${detail ? "" : ` data-plugin-open="${escAttr(p.id)}"`}><span class="plugin-name">${esc(p.name)}</span><span class="plugin-status plugin-status-${escAttr(p.status)}">${esc(p.status)}</span>${updateBadge}${probeBadge}${channelBadge}${p.sandboxDeclared ? `<span class="cap-badge">${esc(t("pluginSandboxBadge"))}</span>` : ""}${detail ? `<span class="plugin-detail-open-hint">›</span>` : ""}</div>
        <div class="plugin-sub-line"><span class="plugin-id">${esc(p.id)}</span><span class="plugin-sub-sep">·</span><span class="plugin-ver">v${esc(p.version)}</span>${p.capabilities.length ? `<span class="plugin-caps">${p.capabilities.map((c) => `<span class="cap-badge">${esc(c)}</span>`).join("")}</span>` : ""}</div>
        ${detail && (p.author || p.license || p.homepage) ? `<div class="plugin-meta-line">${esc([p.author, p.license, p.homepage].filter(Boolean).join(" · "))}</div>` : ""}
        <div class="plugin-actions">${detail ? `<button class="btn ghost" data-plugin-cfg="${escAttr(p.id)}" type="button">${esc(t("pluginDetailOpenCfg"))}</button>` : ""}${updateBtn}${updateFileBtn}<button class="btn ghost" data-plugin-toggle="${escAttr(p.id)}" type="button">${esc(running ? t("pluginDisable") : t("pluginEnable"))}</button><button class="btn ghost" data-plugin-probe="${escAttr(p.id)}" data-plugin-probe-name="${escAttr(p.name)}" type="button">${esc(t("probeBtn"))}</button><button class="btn ghost" data-plugin-lifecycle="${escAttr(p.id)}" type="button">${esc(t("lifecycleBtn"))}</button><button class="btn ghost" data-plugin-trace="${escAttr(p.id)}" type="button">${esc(t("traceBtn"))}</button><button class="btn ghost" data-plugin-health="${escAttr(p.id)}" data-plugin-health-name="${escAttr(p.name)}" type="button">${esc(t("healthBtn"))}</button><button class="btn ghost danger" data-plugin-uninstall="${escAttr(p.id)}" type="button">${esc(t("pluginUninstall"))}</button></div>
        <div class="plugin-config-row"><div class="plugin-channel-row"><span class="setting-hint">${esc(t("channelRowLabel"))}</span>${channelSel}</div><label class="plugin-autoreload"><input type="checkbox" data-plugin-autoreload="${escAttr(p.id)}"${p.autoReload ? " checked" : ""}/><span>${esc(t("pluginAutoReload"))}</span></label></div>
        ${missingDeps}${revokedBanner}
        ${p.description ? `<div class="plugin-desc">${esc(p.description)}</div>` : `<div class="plugin-desc muted">${esc(t("pluginNoDescription"))}</div>`}
        ${p.path ? `<div class="plugin-path" title="${escAttr(p.path)}">${esc(p.path)}</div>` : ""}`;
      const entries = permByPlugin.get(p.id)?.permissions ?? [];
      const permRows = entries
        .map((e) => {
          // docs/permission-domains.md §4.3 enforcement point 3: declared derived permissions offer only ask/denied
          // (Core's set_decision also rejects granted; this just avoids a pointless click)
          const noAlways = e.high_risk || e.declared;
          const opts = noAlways && e.decision !== "granted"
            ? [["ask", t("permAsk")], ["denied", t("permDenied")]]
            : [["granted", t("permGranted")], ["ask", t("permAsk")], ["denied", t("permDenied")]];
          const sel = `<select data-perm-plugin="${escAttr(p.id)}" data-perm="${escAttr(e.permission)}">${opts
            .map(([v, l]) => `<option value="${escAttr(v)}"${v === e.decision ? " selected" : ""}>${esc(l)}</option>`)
            .join("")}</select>`;
          const badge = (e.high_risk ? ` <span class="setting-hint">${esc(t("permHighRisk"))}</span>` : "")
            + (e.declared ? ` <span class="setting-hint">${esc(t("permDeclared"))}</span>` : "");
          const reset = `<button class="btn ghost" data-perm-reset="${escAttr(e.permission)}" data-perm-def="${escAttr(e.default)}" data-perm-plugin="${escAttr(p.id)}" type="button">${esc(t("permReset"))}</button>`;
          return `<div class="setting-row"><div class="setting-info"><span class="setting-label">${esc(e.permission)}${badge}</span></div><div class="perm-controls">${sel}${reset}</div></div>`;
        })
        .join("");
      return `${head}${permRows ? `<div class="settings-list plugin-perms">${permRows}</div>` : ""}</div>`;
  };
  box.innerHTML = detail
    ? `<button class="btn ghost plugin-detail-back" data-plugin-back type="button">‹ ${esc(t("pluginDetailBack"))}</button>`
      + pluginCardHtml(detail, true)
      + `<div class="plugin-readme"><div class="plugin-readme-title">${esc(t("pluginReadmeTitle"))}</div><div class="plugin-readme-body" id="plugin-readme-body">${esc(t("pluginReadmeLoading"))}</div></div>`
    : plugins.map((p) => pluginCardHtml(p, false)).join("");
  box.querySelectorAll("button[data-plugin-toggle]").forEach((b) => {
    b.addEventListener("click", async () => {
      await invoke("toggle_plugin", { id: (b as HTMLElement).dataset.pluginToggle });
      await refreshPlugins();
    });
  });
  box.querySelectorAll("button[data-plugin-update]").forEach((b) => {
    b.addEventListener("click", async () => {
      const el = b as HTMLButtonElement;
      const id = el.dataset.pluginUpdate ?? "";
      el.disabled = true;
      el.textContent = t("pluginUpdateChecking");
      try {
        // F7 — unified preview: download + verify + diff / key-change info -> one dialog -> install after confirmation.
        const res = await invoke<UpdatePreviewResponse>("preview_update", { id });
        const ok = await showInstallPreviewDialog(res.preview, {
          mode: "update",
          publisherChange: res.publisherChange ?? null,
        });
        if (!ok) {
          el.disabled = false;
          el.textContent = t("pluginUpdate");
          return;
        }
        const softWarn =
          res.preview.signature.status === "unsigned" ||
          res.preview.signature.status === "unknown-key";
        await invoke("install_ocplugin", {
          path: res.archivePath,
          confirmUnsigned: softWarn,
          confirmKeyChange: res.publisherChange != null,
        });
      } catch (err) {
        const m = document.getElementById("plugin-install-msg");
        if (m) m.textContent = `${t("pluginUpdateFailed")}: ${(err as Error).message ?? err}`;
        el.disabled = false;
        el.textContent = t("pluginUpdate");
        return;
      }
      await refreshPlugins();
    });
  });
  // Local-file update: the same non-destructive install path as 'install' (no uninstall), it just verifies the package id first, then previews and confirms.
  box.querySelectorAll("button[data-plugin-update-file]").forEach((b) => {
    b.addEventListener("click", async () => {
      const el = b as HTMLElement;
      const id = el.dataset.pluginUpdateFile ?? "";
      const msg = document.getElementById("plugin-install-msg");
      try {
        const picked = await open({
          multiple: false,
          directory: false,
          filters: [{ name: "OpenCapX Plugin", extensions: ["ocplugin"] }],
        });
        if (!picked) return; // cancel: silent
        const path = typeof picked === "string" ? picked : picked;
        const installed = await installPreviewedPlugin(path, {
          mode: "update",
          // The package id must equal the target plugin id — never let one plugin's package overwrite another
          guard: (preview) =>
            preview.id === id
              ? null
              : t("pluginUpdateFileIdMismatch").replace("{want}", id).replace("{got}", preview.id),
        });
        if (!installed) return; // cancel in the preview dialog: silent
        if (msg) msg.textContent = `✓ ${installed}`;
      } catch (e) {
        if (msg) msg.textContent = `✗ ${String(e)}`;
      }
      await refreshPlugins();
    });
  });
  box.querySelectorAll("button[data-plugin-reopen]").forEach((b) => {
    b.addEventListener("click", async () => {
      const id = (b as HTMLElement).dataset.pluginReopen ?? "";
      try {
        await invoke("reopen_plugin", { id });
      } catch (err) {
        const m = document.getElementById("plugin-install-msg");
        if (m) m.textContent = `${t("pluginReopenFailed")}: ${(err as Error).message ?? err}`;
      }
      await refreshPlugins();
    });
  });
  box.querySelectorAll("button[data-plugin-lifecycle]").forEach((b) => {
    b.addEventListener("click", () => {
      const id = (b as HTMLElement).dataset.pluginLifecycle ?? "";
      const name = (b as HTMLElement).dataset.pluginLifecycleName ?? id;
      void showLifecycleDialog(id, name);
    });
  });
  box.querySelectorAll("button[data-plugin-trace]").forEach((b) => {
    b.addEventListener("click", () => {
      const el = b as HTMLElement;
      const id = el.dataset.pluginTrace ?? "";
      const name = el.dataset.pluginTraceName ?? id;
      void showTraceDialog(id, name);
    });
  });
  box.querySelectorAll("button[data-plugin-probe]").forEach((b) => {
    b.addEventListener("click", () => {
      const el = b as HTMLElement;
      const id = el.dataset.pluginProbe ?? "";
      const name = el.dataset.pluginProbeName ?? id;
      void showProbeDialog(id, name, refreshPlugins);
    });
  });
  box.querySelectorAll("button[data-plugin-health]").forEach((b) => {
    b.addEventListener("click", () => {
      const el = b as HTMLElement;
      const id = el.dataset.pluginHealth ?? "";
      const name = el.dataset.pluginHealthName ?? id;
      void showHealthDialog(id, name, refreshPlugins);
    });
  });
  box.querySelectorAll("button[data-plugin-uninstall]").forEach((b) => {
    b.addEventListener("click", async () => {
      const el = b as HTMLElement;
      const id = el.dataset.pluginUninstall ?? "";
      let preview: UninstallPreview;
      try {
        preview = await invoke<UninstallPreview>("preview_uninstall_plugin", { id });
      } catch (err) {
        // Backend can't read it -> degrade to a simple confirmation
        if (!window.confirm(`${t("pluginUninstallConfirm")}\n\n${id}`)) return;
        try {
          await invoke("uninstall_plugin", { id });
        } catch (e) {
          const m = document.getElementById("plugin-install-msg");
          if (m) m.textContent = `${t("pluginUninstallFailed")}: ${(e as Error).message ?? e}`;
        }
        await refreshPlugins();
        return;
      }
      const ok = await showUninstallPreviewDialog(preview);
      if (!ok) return;
      try {
        await invoke("uninstall_plugin", { id });
      } catch (err) {
        const m = document.getElementById("plugin-install-msg");
        if (m) m.textContent = `${t("pluginUninstallFailed")}: ${(err as Error).message ?? err}`;
        return;
      }
      await refreshPlugins();
    });
  });
  // Auto-reload toggle: once on, a change to the manifest mtime automatically stops+starts.
  // The poller singleton runs in core (spawn_auto_reload_poller during setup); this just
  // flips the per-manager set + SQL flag. No need to refresh the whole list, since the state is on the input itself.
  box.querySelectorAll("input[data-plugin-autoreload]").forEach((c) => {
    c.addEventListener("change", async () => {
      const el = c as HTMLInputElement;
      const id = el.dataset.pluginAutoreload ?? "";
      try {
        await invoke("set_plugin_auto_reload", { id, on: el.checked });
      } catch (err) {
        el.checked = !el.checked; // failure: roll back the UI
        const m = document.getElementById("plugin-install-msg");
        if (m) m.textContent = `✗ ${String(err)}`;
      }
    });
  });
  box.querySelectorAll("select[data-plugin-channel]").forEach((s) => {
    s.addEventListener("change", async () => {
      const el = s as HTMLSelectElement;
      const id = el.dataset.pluginChannel ?? "";
      try {
        await invoke("set_plugin_channel", { id, channel: el.value });
      } catch (err) {
        const m = document.getElementById("plugin-install-msg");
        if (m) m.textContent = `${t("channelSetFailed")}: ${(err as Error).message ?? err}`;
      }
      await refreshPlugins();
    });
  });
  box.querySelectorAll("select[data-perm]").forEach((s) => {
    s.addEventListener("change", async () => {
      const el = s as HTMLSelectElement;
      const pluginId = el.dataset.permPlugin ?? "";
      const permission = el.dataset.perm ?? "";
      try {
        await invoke("set_permission", { pluginId, permission, decision: el.value });
      } catch {
        /* High-risk rejected changes etc., echoed back on refresh */
      }
      await refreshPlugins();
    });
  });
  box.querySelectorAll("button[data-perm-reset]").forEach((b) => {
    b.addEventListener("click", async () => {
      const el = b as HTMLElement;
      try {
        await invoke("set_permission", {
          pluginId: el.dataset.permPlugin,
          permission: el.dataset.permReset,
          decision: el.dataset.permDef,
        });
      } catch {
        /* ignore */
      }
      await refreshPlugins();
    });
  });
  // ── list<->detail navigation: click the list title row to enter; Back returns; Open config jumps to the config tab filtered by plugin id ──
  box.querySelectorAll("[data-plugin-open]").forEach((el) => {
    el.addEventListener("click", () => {
      pluginDetailId = (el as HTMLElement).dataset.pluginOpen ?? null;
      void refreshPlugins();
    });
  });
  box.querySelectorAll("[data-plugin-back]").forEach((b) => {
    b.addEventListener("click", () => {
      pluginDetailId = null;
      void refreshPlugins();
    });
  });
  box.querySelectorAll("[data-plugin-cfg]").forEach((b) => {
    b.addEventListener("click", () => {
      const id = (b as HTMLElement).dataset.pluginCfg ?? "";
      pluginDetailId = null;
      openPluginSettingsPage(id, "plugins");
    });
  });
  if (detail) {
    void renderPluginReadme(detail.id);
  }
}

/// Detail-page README: three states (loading -> present/absent). Rendered with marked + sanitized with DOMPurify before entering
/// innerHTML — the README is plugin-author content, treated as untrusted. If the detail has switched away by the time it returns (back to
/// the list / another plugin / a re-render), it is discarded, not written.
async function renderPluginReadme(id: string): Promise<void> {
  let md: string | null = null;
  try {
    md = await invoke<string | null>("read_plugin_readme", { id });
  } catch {
    /* Command unavailable: treat as no README */
  }
  const target = document.getElementById("plugin-readme-body");
  if (!target || pluginDetailId !== id) return;
  if (!md) {
    target.textContent = t("pluginReadmeMissing");
    return;
  }
  const html = marked.parse(md, { async: false });
  target.innerHTML = DOMPurify.sanitize(typeof html === "string" ? html : "");
}
