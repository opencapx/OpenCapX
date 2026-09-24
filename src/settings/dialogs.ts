import { invoke } from "@tauri-apps/api/core";
import { t } from "../i18n";
import { esc, formatLifecycleTime } from "./shared";

interface PermissionPreviewItem {
  name: string;
  highRisk: boolean;
}

export interface PluginPreview {
  id: string;
  name: string;
  description?: string;
  version: string;
  type: string;
  capabilities: string[];
  permissions: PermissionPreviewItem[];
  // Wow 6 — signature/integrity status.
  // status ∈ "trusted" | "unsigned" | "tampered" | "bad-signature" | "unknown-key" | "malformed-signature"
  // keyId: signer identifier (returned when trusted / unknown-key)
  signature: { status: string; keyId?: string };
  // F7 — tri-state render facts: registry verification / official flag / core compatibility / permission diff.
  verified?: boolean;
  official?: boolean;
  compat: { ok: boolean; minCoreVersion?: string; current: string };
  permissionDiff?: { added: string[]; removed: string[] };
  /// S5 — whether a sandbox block is declared (used for the forced-execution notice when unsigned).
  sandboxDeclared?: boolean;
}

function signatureBadgeHtml(sig: PluginPreview["signature"]): string {
  const { status, keyId } = sig;
  const cls = `sig-badge sig-${status}`;
  const labelKey = `sigStatus_${status.replace(/-/g, "_")}`;
  let label: string;
  try {
    label = t(labelKey as never);
  } catch {
    label = status;
  }
  const keyTxt = keyId ? ` · ${keyId}` : "";
  return `<span class="${cls}" title="${esc(status)}${esc(keyTxt)}">${esc(label)}${esc(keyTxt)}</span>`;
}

export interface UninstallPreview {
  id: string;
  name: string;
  version: string;
  autoReload: boolean;
  configExists: boolean;
  permissionCount: number;
  capabilityCount: number;
  dependents: string[];
}

interface LifecycleEvent {
  id: string;
  kind: string;
  timestamp: number;
  reason: string;
}

interface ProbeCapability {
  capability: string;
  ok: boolean;
  elapsedMs: number;
  error?: string;
}

interface ProbeReport {
  pluginId: string;
  status: "passed" | "failed" | "skipped";
  ranAt: number;
  capabilities: ProbeCapability[];
  summary: string;
}

function lifecycleKindLabel(kind: string): string {
  // plugin.lifecycle.starting -> starting, plugin.lifecycle.crashed -> crashed, ...
  switch (kind) {
    case "plugin.lifecycle.starting": return t("lifecycleKindStarting");
    case "plugin.lifecycle.running": return t("lifecycleKindRunning");
    case "plugin.lifecycle.stopped": return t("lifecycleKindStopped");
    case "plugin.lifecycle.crashed": return t("lifecycleKindCrashed");
    case "plugin.installed": return t("lifecycleKindInstalled");
    case "plugin.uninstalled": return t("lifecycleKindUninstalled");
    case "plugin.restarting": return t("lifecycleKindRestarting");
    default: return kind;
  }
}

function lifecycleClassFor(kind: string): string {
  if (kind.endsWith("crashed")) return "ev crashed";
  if (kind.endsWith("stopped")) return "ev stopped";
  if (kind.endsWith("running")) return "ev running";
  if (kind.endsWith("starting")) return "ev starting";
  if (kind.endsWith("installed")) return "ev installed";
  if (kind.endsWith("uninstalled")) return "ev uninstalled";
  if (kind.endsWith("restarting")) return "ev restarting";
  return "ev";
}

export async function showLifecycleDialog(pluginId: string, pluginName: string): Promise<void> {
  const overlay = document.createElement("div");
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog lifecycle-dialog" role="dialog" aria-label="${esc(t("lifecycleTitle"))}">
      <h2>${esc(t("lifecycleTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)}</div>
      </div>
      <div class="lifecycle-loading">${esc(t("lifecycleLoading"))}</div>
    </div>`;
  document.body.appendChild(overlay);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) overlay.remove();
  });
  let events: LifecycleEvent[] = [];
  try {
    events = await invoke<LifecycleEvent[]>("list_plugin_lifecycle", { id: pluginId, limit: 100 });
  } catch (err) {
    overlay.innerHTML = `
      <div class="install-dialog lifecycle-dialog" role="dialog" aria-label="${esc(t("lifecycleTitle"))}">
        <h2>${esc(t("lifecycleTitle"))}</h2>
        <p class="muted">${esc(t("lifecycleLoadFailed"))}: ${esc(String((err as Error).message ?? err))}</p>
        <div class="install-actions">
          <button class="btn ghost" id="lifecycle-close" type="button">${esc(t("installPreviewCancel"))}</button>
        </div>
      </div>`;
    overlay.querySelector("#lifecycle-close")?.addEventListener("click", () => overlay.remove());
    return;
  }
  const body = events.length
    ? `<ul class="lifecycle-list">${events
        .map(
          (e) =>
            `<li><span class="${lifecycleClassFor(e.kind)}">${esc(lifecycleKindLabel(e.kind))}</span><span class="lifecycle-time">${esc(formatLifecycleTime(e.timestamp))}</span>${e.reason ? `<span class="lifecycle-reason">${esc(e.reason)}</span>` : ""}</li>`,
        )
        .join("")}</ul>`
    : `<p class="muted">${esc(t("lifecycleEmpty"))}</p>`;
  overlay.innerHTML = `
    <div class="install-dialog lifecycle-dialog" role="dialog" aria-label="${esc(t("lifecycleTitle"))}">
      <h2>${esc(t("lifecycleTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)} · ${events.length} ${esc(t("lifecycleCount"))}</div>
      </div>
      <div class="limeline-body">${body}</div>
      <div class="install-actions">
        <button class="btn ghost" id="lifecycle-close" type="button">${esc(t("installPreviewCancel"))}</button>
      </div>
    </div>`;
  overlay.querySelector("#lifecycle-close")?.addEventListener("click", () => overlay.remove());
}

/// Phase 37 — show the latest capability probe report. An optional 'Re-probe' triggers a backend run.
export async function showProbeDialog(
  pluginId: string,
  pluginName: string,
  refreshParent: () => Promise<void>,
): Promise<void> {
  const overlay = document.createElement("div");
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog probe-dialog" role="dialog" aria-label="${esc(t("probeTitle"))}">
      <h2>${esc(t("probeTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)}</div>
      </div>
      <div class="probe-loading">${esc(t("probeLoading"))}</div>
    </div>`;
  document.body.appendChild(overlay);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) overlay.remove();
  });

  const render = (report: ProbeReport | null, busy = false) => {
    if (!report) {
      overlay.innerHTML = `
        <div class="install-dialog probe-dialog" role="dialog" aria-label="${esc(t("probeTitle"))}">
          <h2>${esc(t("probeTitle"))}</h2>
          <div class="install-head">
            <div class="install-name">${esc(pluginName)}</div>
            <div class="install-id">${esc(pluginId)}</div>
          </div>
          <p class="muted">${esc(t("probeNever"))}</p>
          <div class="install-actions">
            <button class="btn" id="probe-run" type="button"${busy ? " disabled" : ""}>${esc(t("probeRun"))}</button>
            <button class="btn ghost" id="probe-close" type="button">${esc(t("installPreviewCancel"))}</button>
          </div>
        </div>`;
    } else {
      const stamp = new Date(report.ranAt * 1000).toLocaleString();
      const statusBadge = `<span class="probe-status probe-${esc(report.status)}">${esc(probeStatusLabel(report.status))}</span>`;
      const rows = report.capabilities.length
        ? `<ul class="probe-list">${report.capabilities
            .map(
              (c) =>
                `<li class="probe-row probe-row-${c.ok ? "ok" : "fail"}"><span class="probe-cap">${esc(c.capability)}</span><span class="probe-elapsed">${c.elapsedMs}ms</span>${c.error ? `<span class="probe-error">${esc(c.error)}</span>` : `<span class="probe-ok">✓</span>`}</li>`,
            )
            .join("")}</ul>`
        : `<p class="muted">${esc(t("probeNoCapabilities"))}</p>`;
      overlay.innerHTML = `
        <div class="install-dialog probe-dialog" role="dialog" aria-label="${esc(t("probeTitle"))}">
          <h2>${esc(t("probeTitle"))}</h2>
          <div class="install-head">
            <div class="install-name">${esc(pluginName)}</div>
            <div class="install-id">${esc(pluginId)}</div>
          </div>
          <div class="probe-summary">${statusBadge} <span class="probe-ran-at">${esc(stamp)}</span><div class="probe-text">${esc(report.summary)}</div></div>
          ${rows}
          <div class="install-actions">
            <button class="btn" id="probe-run" type="button"${busy ? " disabled" : ""}>${esc(t("probeRun"))}</button>
            <button class="btn ghost" id="probe-close" type="button">${esc(t("installPreviewCancel"))}</button>
          </div>
        </div>`;
    }
    overlay.querySelector("#probe-close")?.addEventListener("click", () => overlay.remove());
    overlay.querySelector("#probe-run")?.addEventListener("click", async () => {
      const btn = overlay.querySelector("#probe-run") as HTMLButtonElement | null;
      if (btn) btn.disabled = true;
      try {
        const fresh = await invoke<ProbeReport>("run_plugin_probe", { id: pluginId });
        await refreshParent();
        render(fresh, false);
      } catch (err) {
        const text = overlay.querySelector(".probe-text");
        if (text) text.textContent = `${t("probeFailed")}: ${(err as Error).message ?? err}`;
        if (btn) btn.disabled = false;
      }
    });
  };

  // Initially take the most recent one
  let report: ProbeReport | null = null;
  try {
    report = await invoke<ProbeReport | null>("get_probe_report", { id: pluginId });
  } catch {
    report = null;
  }
  render(report);
}

export function probeStatusLabel(status: string): string {
  if (status === "passed") return t("probePassed");
  if (status === "failed") return t("probeFailed");
  return t("probeSkipped");
}

// ---- Phase 40 — Health config dialog (per-plugin watchdog / heartbeat strategy) -------

interface HealthConfig {
  heartbeatSec: number;
  pingTimeoutMs: number;
  maxRetries: number;
  backoffInitialMs: number;
  enabled: boolean;
}

const HEALTH_DEFAULTS: HealthConfig = {
  heartbeatSec: 0,
  pingTimeoutMs: 1000,
  maxRetries: 3,
  backoffInitialMs: 1000,
  enabled: true,
};

function clampHealth(n: number, lo: number, hi: number, fallback: number): number {
  if (!Number.isFinite(n)) return fallback;
  return Math.min(hi, Math.max(lo, Math.trunc(n)));
}

export async function showHealthDialog(
  pluginId: string,
  pluginName: string,
  refreshParent: () => Promise<void>,
): Promise<void> {
  let cfg: HealthConfig = { ...HEALTH_DEFAULTS };
  try {
    cfg = await invoke<HealthConfig>("get_plugin_health_config", { id: pluginId });
  } catch {
    cfg = { ...HEALTH_DEFAULTS };
  }
  const overlay = document.createElement("div");
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog health-dialog" role="dialog" aria-label="${esc(t("healthTitle"))}">
      <h2>${esc(t("healthTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)}</div>
      </div>
      <p class="muted">${esc(t("healthHint"))}</p>
      <div class="health-row"><label>${esc(t("healthEnabled"))}</label><input type="checkbox" id="health-enabled"${cfg.enabled ? " checked" : ""}/></div>
      <div class="health-row"><label>${esc(t("healthHeartbeat"))}</label><input type="number" id="health-heartbeat" min="0" max="3600" value="${cfg.heartbeatSec}"/><span class="muted">${esc(t("healthSec"))}</span></div>
      <div class="health-row"><label>${esc(t("healthPingTimeout"))}</label><input type="number" id="health-timeout" min="100" max="60000" step="100" value="${cfg.pingTimeoutMs}"/><span class="muted">${esc(t("healthMs"))}</span></div>
      <div class="health-row"><label>${esc(t("healthMaxRetries"))}</label><input type="number" id="health-retries" min="0" max="100" value="${cfg.maxRetries}"/></div>
      <div class="health-row"><label>${esc(t("healthBackoff"))}</label><input type="number" id="health-backoff" min="0" max="30000" step="100" value="${cfg.backoffInitialMs}"/><span class="muted">${esc(t("healthMs"))}</span></div>
      <div class="install-actions">
        <button class="btn" id="health-save" type="button">${esc(t("healthSave"))}</button>
        <button class="btn ghost" id="health-reset" type="button">${esc(t("healthReset"))}</button>
        <button class="btn ghost" id="health-close" type="button">${esc(t("installPreviewCancel"))}</button>
      </div>
      <p class="muted health-msg" id="health-msg"></p>
    </div>`;
  document.body.appendChild(overlay);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) overlay.remove();
  });
  overlay.querySelector("#health-close")?.addEventListener("click", () => overlay.remove());
  overlay.querySelector("#health-reset")?.addEventListener("click", () => {
    overlay.querySelector<HTMLInputElement>("#health-heartbeat")!.value = String(HEALTH_DEFAULTS.heartbeatSec);
    overlay.querySelector<HTMLInputElement>("#health-timeout")!.value = String(HEALTH_DEFAULTS.pingTimeoutMs);
    overlay.querySelector<HTMLInputElement>("#health-retries")!.value = String(HEALTH_DEFAULTS.maxRetries);
    overlay.querySelector<HTMLInputElement>("#health-backoff")!.value = String(HEALTH_DEFAULTS.backoffInitialMs);
    overlay.querySelector<HTMLInputElement>("#health-enabled")!.checked = HEALTH_DEFAULTS.enabled;
  });
  overlay.querySelector("#health-save")?.addEventListener("click", async () => {
    const enabled = overlay.querySelector<HTMLInputElement>("#health-enabled")!.checked;
    const heartbeat = Number(overlay.querySelector<HTMLInputElement>("#health-heartbeat")!.value);
    const timeout = Number(overlay.querySelector<HTMLInputElement>("#health-timeout")!.value);
    const retries = Number(overlay.querySelector<HTMLInputElement>("#health-retries")!.value);
    const backoff = Number(overlay.querySelector<HTMLInputElement>("#health-backoff")!.value);
    const next: HealthConfig = {
      heartbeatSec: clampHealth(heartbeat, 0, 3600, HEALTH_DEFAULTS.heartbeatSec),
      pingTimeoutMs: clampHealth(timeout, 100, 60000, HEALTH_DEFAULTS.pingTimeoutMs),
      maxRetries: clampHealth(retries, 0, 100, HEALTH_DEFAULTS.maxRetries),
      backoffInitialMs: clampHealth(backoff, 0, 30000, HEALTH_DEFAULTS.backoffInitialMs),
      enabled,
    };
    const saveBtn = overlay.querySelector<HTMLButtonElement>("#health-save");
    if (saveBtn) saveBtn.disabled = true;
    try {
      await invoke("set_plugin_health_config", { id: pluginId, cfg: next });
      const msg = overlay.querySelector("#health-msg");
      if (msg) msg.textContent = t("healthSaved");
      await refreshParent();
    } catch (err) {
      const msg = overlay.querySelector("#health-msg");
      if (msg) msg.textContent = `${t("healthSaveFailed")}: ${(err as Error).message ?? err}`;
      if (saveBtn) saveBtn.disabled = false;
    }
  });
}

export function showUninstallPreviewDialog(p: UninstallPreview): Promise<boolean> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.style.cssText =
      "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
    const depList = p.dependents.length
      ? `<ul class="uninstall-deps">${p.dependents.map((d) => `<li><code>${esc(d)}</code></li>`).join("")}</ul>`
      : `<p class="muted">${esc(t("uninstallNoDependents"))}</p>`;
    overlay.innerHTML = `
      <div class="install-dialog" role="dialog" aria-label="${esc(t("pluginUninstall"))}">
        <h2>${esc(t("pluginUninstall"))}</h2>
        <div class="install-head">
          <div class="install-name">${esc(p.name)}</div>
          <div class="install-id">${esc(p.id)} · v${esc(p.version)}</div>
        </div>
        <div class="uninstall-summary">
          <div class="uninstall-row"><span>${esc(t("uninstallAutoReload"))}</span><b>${p.autoReload ? esc(t("uninstallYes")) : esc(t("uninstallNo"))}</b></div>
          <div class="uninstall-row"><span>${esc(t("uninstallConfig"))}</span><b>${p.configExists ? esc(t("uninstallYes")) : esc(t("uninstallNo"))}</b></div>
          <div class="uninstall-row"><span>${esc(t("uninstallPermissionCount"))}</span><b>${p.permissionCount}</b></div>
          <div class="uninstall-row"><span>${esc(t("uninstallCapabilityCount"))}</span><b>${p.capabilityCount}</b></div>
        </div>
        <div class="install-section">
          <div class="install-section-title">${esc(t("uninstallDependents"))}</div>
          ${depList}
        </div>
        <p class="muted">${esc(t("uninstallHint"))}</p>
        <div class="install-actions">
          <button class="btn ghost" id="uninstall-cancel" type="button">${esc(t("installPreviewCancel"))}</button>
          <button class="btn danger" id="uninstall-confirm" type="button">${esc(t("pluginUninstall"))}</button>
        </div>
      </div>`;
    document.body.appendChild(overlay);
    const close = (v: boolean) => {
      overlay.remove();
      resolve(v);
    };
    overlay.querySelector("#uninstall-cancel")?.addEventListener("click", () => close(false));
    overlay.querySelector("#uninstall-confirm")?.addEventListener("click", () => close(true));
    overlay.addEventListener("click", (ev) => {
      if (ev.target === overlay) close(false);
    });
  });
}

/// F7 — generic warning confirmation dialog (reused by key-change / unsigned confirmation).
export function showWarningConfirmDialog(opts: {
  title: string;
  body: string;
  confirmLabel: string;
}): Promise<boolean> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.id = "warn-confirm-overlay";
    overlay.style.cssText =
      "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
    overlay.innerHTML = `
      <div class="install-dialog" role="dialog" aria-label="${esc(opts.title)}">
        <h2>${esc(opts.title)}</h2>
        <p class="install-desc">${esc(opts.body)}</p>
        <div class="install-actions">
          <button class="btn ghost" id="warn-cancel" type="button">${esc(t("installPreviewCancel"))}</button>
          <button class="btn primary" id="warn-confirm" type="button">${esc(opts.confirmLabel)}</button>
        </div>
      </div>`;
    document.body.appendChild(overlay);
    const close = (v: boolean) => {
      overlay.remove();
      resolve(v);
    };
    overlay.querySelector("#warn-cancel")?.addEventListener("click", () => close(false));
    overlay.querySelector("#warn-confirm")?.addEventListener("click", () => close(true));
    overlay.addEventListener("click", (ev) => {
      if (ev.target === overlay) close(false);
    });
  });
}

export function showInstallPreviewDialog(
  p: PluginPreview,
  opts?: {
    mode?: "install" | "update";
    publisherChange?: { from?: string | null; to?: string | null } | null;
  },
): Promise<boolean> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.id = "install-preview-overlay";
    overlay.style.cssText =
      "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
    const mode = opts?.mode ?? "install";
    const status = p.signature.status;
    const softWarn = status === "unsigned" || status === "unknown-key";
    const hardDeny =
      status === "tampered" || status === "bad-signature" || status === "malformed-signature";
    const caps = p.capabilities.length
      ? p.capabilities.map((c) => `<span class="cap-badge">${esc(c)}</span>`).join("")
      : `<span class="muted">—</span>`;
    const perms = p.permissions.length
      ? p.permissions
          .map(
            (perm) =>
              `<li class="install-perm${perm.highRisk ? " high-risk" : ""}"><span>${esc(perm.name)}</span>${
                perm.highRisk ? `<span class="risk-tag">${esc(t("installPreviewHighRisk"))}</span>` : ""
              }</li>`,
          )
          .join("")
      : `<li class="muted">—</li>`;
    const desc = p.description
      ? `<p class="install-desc">${esc(p.description)}</p>`
      : `<p class="install-desc muted">${esc(t("installPreviewNoDescription"))}</p>`;
    const verifiedBadge = p.official
      ? `<span class="install-verified official">${esc(t("pluginOfficial"))}</span>`
      : p.verified === true
        ? `<span class="install-verified">✓ ${esc(t("pluginVerified"))}</span>`
        : "";
    const compatHtml = p.compat.ok
      ? ""
      : `<div class="plugin-desc warn">${esc(t("pluginCompatWarn"))}: minCore ${esc(p.compat.minCoreVersion ?? "?")} &gt; ${esc(p.compat.current)}</div>`;
    const diff = p.permissionDiff;
    const diffHtml =
      diff && (diff.added.length || diff.removed.length)
        ? `<div class="install-section">
            <div class="install-section-title">${esc(t("pluginPermDiff"))}</div>
            <ul class="install-perms">
              ${diff.added.map((x) => `<li class="install-perm"><span>+ ${esc(x)}</span></li>`).join("")}
              ${diff.removed.map((x) => `<li class="install-perm muted"><span>− ${esc(x)}</span></li>`).join("")}
            </ul>
          </div>`
        : "";
    const publisherHtml = opts?.publisherChange
      ? `<div class="plugin-desc warn">${esc(t("pluginPublisherChangeBody"))} (${esc(opts.publisherChange.from ?? "—")} → ${esc(opts.publisherChange.to ?? "—")})</div>`
      : "";
    // The unsigned confirmation checkbox sits **right next to the confirm button**: the text stays in the explanation area above, the checkbox + hint go into the button area.
    // When placed in the middle of the dialog, a long dialog (capability/permission list) pushes it out of view, and the user sees only an unclickable
    // gray 'Install' — that is exactly where 'I clicked and nothing happened' comes from.
    const softWarnHtml = softWarn
      ? `<div class="plugin-desc warn">⚠ ${esc(t("pluginUnsignedWarn"))}</div>`
      : "";
    const ackHtml = softWarn
      ? `<label class="install-ack-row"><input type="checkbox" id="install-ack"/><span>${esc(t("pluginUnsignedAck"))}</span></label>
         <p class="install-desc" id="install-ack-hint">${esc(t("pluginUnsignedAckHint"))}</p>`
      : "";
    const hardDenyHtml = hardDeny
      ? `<div class="plugin-desc warn">⛔ ${esc(t("pluginHardDeny"))}</div>`
      : "";
    // S5c — unsigned + sandbox declared: forced execution after install (macOS); tell the user in the dialog first.
    const sandboxForcedHtml =
      p.sandboxDeclared && softWarn
        ? `<div class="plugin-desc warn">${esc(t("installSandboxForced"))}</div>`
        : "";
    // review F1 — unverified and no sandbox declared: must not be silent; clearly state it will run with full user permissions.
    const unconfinedHtml =
      p.verified !== true && !p.sandboxDeclared
        ? `<div class="plugin-desc warn">${esc(t("installSandboxUnconfined"))}</div>`
        : "";
    overlay.innerHTML = `
      <div class="install-dialog" role="dialog" aria-label="${esc(t("installPreviewTitle"))}">
        <h2>${esc(mode === "update" ? t("pluginUpdate") : t("installPreviewTitle"))}</h2>
        <div class="install-head">
          <div class="install-name">${esc(p.name)}</div>
          <div class="install-id">${esc(p.id)} · v${esc(p.version)} · ${esc(p.type)}${verifiedBadge ? ` · ${verifiedBadge}` : ""}</div>
          <div class="install-sig">${signatureBadgeHtml(p.signature)}</div>
        </div>
        ${desc}
        ${publisherHtml}
        ${compatHtml}
        ${softWarnHtml}
        ${sandboxForcedHtml}
        ${unconfinedHtml}
        ${hardDenyHtml}
        <div class="install-section">
          <div class="install-section-title">${esc(t("installPreviewCapabilities"))}</div>
          <div class="install-caps">${caps}</div>
        </div>
        <div class="install-section">
          <div class="install-section-title">${esc(t("installPreviewPermissions"))}</div>
          <ul class="install-perms">${perms}</ul>
        </div>
        ${diffHtml}
        <div class="install-actions">
          ${ackHtml}
          <div class="install-buttons">
            <button class="btn ghost" id="install-preview-cancel" type="button">${esc(t("installPreviewCancel"))}</button>
            ${
              hardDeny
                ? ""
                : `<button class="btn primary" id="install-preview-confirm" type="button" ${softWarn ? "disabled" : ""}>${esc(mode === "update" ? t("pluginUpdate") : t("installPreviewConfirm"))}</button>`
            }
          </div>
        </div>
      </div>`;
    document.body.appendChild(overlay);
    const confirmBtn = overlay.querySelector<HTMLButtonElement>("#install-preview-confirm");
    const ack = overlay.querySelector<HTMLInputElement>("#install-ack");
    ack?.addEventListener("change", () => {
      if (confirmBtn) confirmBtn.disabled = !ack.checked;
      // When unchecked, put 'why the button won't click' right next to it; it collapses once checked
      const hint = overlay.querySelector<HTMLElement>("#install-ack-hint");
      if (hint) hint.hidden = ack.checked;
    });
    const close = (v: boolean) => {
      overlay.remove();
      resolve(v);
    };
    overlay.querySelector("#install-preview-cancel")?.addEventListener("click", () => close(false));
    confirmBtn?.addEventListener("click", () => close(true));
    overlay.addEventListener("click", (ev) => {
      if (ev.target === overlay) close(false);
    });
  });
}
