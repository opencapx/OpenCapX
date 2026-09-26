import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc, escAttr } from "../shared";
import { WebhookEndpointDto } from "./endpoints";

// ─── Phase 51: DSL routing rules (when/then) ────────────────────────────────────────
export interface RouteRuleDto {
  id: string;
  name: string;
  priority: number;
  enabled: boolean;
  kindPattern: string;
  payloadPath: string | null;
  payloadMatch: string | null;
  targetEndpointIds: string[];
  recipients: string[];
  tags: string[];
  // Phase 68 — temporal correlation condition (fan out only if an event matching the pattern occurred within the last N seconds)
  seenInLast: { pattern: string; windowSecs: number } | null;
  createdAt: number;
}

// Phase 68 — correlation analysis
export interface RouteSeenEventDto {
  id: string;
  source: string;
  payloadSummary: string;
  tsSecs: number;
  routesFired: string[];
  correlationsHit: string[];
}

export interface CycleReportDto {
  cycle: string[];
  // backend serde rename_all = "snake_case" → "self_loop" / "route_to_route" / "route_to_correlation"
  kind: "self_loop" | "route_to_route" | "route_to_correlation";
}

// ─── Phase 51: DSL routes helpers ──────────────────────────────────────────────

export async function refreshAlertingRoutes(): Promise<void> {
  const list = document.getElementById("alerting-routes-list");
  const msg = document.getElementById("alerting-route-msg");
  if (!list) return;
  try {
    const items = await invoke<RouteRuleDto[]>("list_alerting_routes");
    if (items.length === 0) {
      list.innerHTML = `<span class="setting-hint">${esc(t("alertingRoutesEmpty"))}</span>`;
      return;
    }
    // Build endpoint name lookup for richer target rendering
    const eps = await invoke<WebhookEndpointDto[]>("list_alerting_endpoints");
    const nameById = new Map<string, string>(eps.map((e) => [e.id, e.name] as [string, string]));
    list.innerHTML = items
      .map((r) => {
        const status = r.enabled
          ? `<span class="alerting-badge alerting-badge-on">${esc(t("alertingRouteEnabled"))}</span>`
          : `<span class="alerting-badge">${esc(t("alertingRouteDisabled"))}</span>`;
        const targetNames = r.targetEndpointIds
          .map((id) => nameById.get(id) || id)
          .join(", ");
        const recipientsLine = (r.recipients ?? []).length > 0
          ? `<div class="route-card-row"><span class="setting-hint">${esc(t("alertingRouteRecipients"))}:</span> <code>${esc(r.recipients.join(", "))}</code></div>`
          : "";
        const whenClause = r.payloadPath && r.payloadMatch
          ? `${esc(r.kindPattern)}<br/><span class="setting-hint">→ ${esc(r.payloadPath)} <code>${esc(r.payloadMatch)}</code></span>`
          : `${esc(r.kindPattern)}`;
        const tags = r.tags.length > 0
          ? `<div class="route-tags">${r.tags.map((tg) => `<span class="route-tag">${esc(tg)}</span>`).join("")}</div>`
          : "";
        return `<div class="route-card${r.enabled ? " route-card-active" : ""}">
          <div class="route-card-head">
            <b>${esc(r.name)}</b>
            <span class="route-priority">#${r.priority}</span>
            ${status}
          </div>
          <div class="route-card-row"><span class="setting-hint">${esc(t("alertingRouteWhen"))}:</span> ${whenClause}</div>
          <div class="route-card-row"><span class="setting-hint">${esc(t("alertingRouteThen"))}:</span> <code>${esc(targetNames)}</code></div>
          ${recipientsLine}
          ${tags}
          <div class="route-card-foot"><button class="btn ghost" data-route-del="${escAttr(r.id)}" type="button">${esc(t("alertingSilenceDelete"))}</button><button class="btn ghost" data-route-toggle="${escAttr(r.id)}" type="button">${r.enabled ? esc(t("alertingRouteDisable")) : esc(t("alertingRouteEnable"))}</button></div>
        </div>`;
      })
      .join("");
    list.querySelectorAll<HTMLButtonElement>("[data-route-del]").forEach((btn) => {
      btn.addEventListener("click", () => void deleteRoute(btn.dataset.routeDel || ""));
    });
    list.querySelectorAll<HTMLButtonElement>("[data-route-toggle]").forEach((btn) => {
      btn.addEventListener("click", () => void toggleRoute(btn.dataset.routeToggle || ""));
    });
    if (msg) msg.textContent = "";
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function deleteRoute(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingRouteDeleteConfirm"))) return;
  const msg = document.getElementById("alerting-route-msg");
  try {
    await invoke<boolean>("delete_alerting_route", { id });
    if (msg) msg.textContent = t("alertingRouteDeleted");
    void refreshAlertingRoutes();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function toggleRoute(id: string): Promise<void> {
  if (!id) return;
  const msg = document.getElementById("alerting-route-msg");
  try {
    const items = await invoke<RouteRuleDto[]>("list_alerting_routes");
    const r = items.find((x) => x.id === id);
    if (!r) return;
    await invoke("save_alerting_route", {
      rule: {
        id: r.id,
        name: r.name,
        priority: r.priority,
        enabled: !r.enabled,
        kindPattern: r.kindPattern,
        payloadPath: r.payloadPath,
        payloadMatch: r.payloadMatch,
        targetEndpointIds: r.targetEndpointIds,
        recipients: r.recipients ?? [],
        tags: r.tags,
      },
    });
    void refreshAlertingRoutes();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

export async function onAddRoute(): Promise<void> {
  const msg = document.getElementById("alerting-route-msg");
  const name = window.prompt(t("alertingRouteNamePrompt"), "critical -> oncall");
  if (!name) return;
  const pattern = window.prompt(t("alertingRoutePatternPrompt"), "*") || "*";
  const priorityStr = window.prompt(t("alertingRoutePriorityPrompt"), "100") || "100";
  const priority = parseInt(priorityStr, 10);
  if (isNaN(priority)) {
    if (msg) msg.textContent = t("alertingRoutePriorityInvalid");
    return;
  }
  // pick endpoint ids
  let eps: WebhookEndpointDto[] = [];
  try {
    eps = await invoke<WebhookEndpointDto[]>("list_alerting_endpoints");
  } catch {}
  if (eps.length === 0) {
    if (msg) msg.textContent = t("alertingRouteNeedEndpoint");
    return;
  }
  const idsStr = window.prompt(
    t("alertingRouteTargetPrompt"),
    eps.map((e) => `${e.id}(${e.name})`).join(", "),
  );
  if (!idsStr) return;
  const targetIds: string[] = [];
  for (let token of idsStr.split(",")) {
    token = token.trim();
    if (!token) continue;
    // Accepts id or id(name) form
    const idPart = token.split("(")[0].trim();
    if (idPart) targetIds.push(idPart);
  }
  if (targetIds.length === 0) {
    if (msg) msg.textContent = t("alertingRouteTargetRequired");
    return;
  }
  // Phase 68 — temporal correlation condition (optional)
  let seenInLast: { pattern: string; windowSecs: number } | null = null;
  const seenRaw = window.prompt(t("alertingRouteSeenInLastPrompt"), "");
  if (seenRaw?.trim()) {
    const parts = seenRaw.split("|").map((s) => s.trim());
    const seenPat = parts[0] ?? "";
    const seenWin = parseInt(parts[1] ?? "60", 10);
    if (seenPat) {
      seenInLast = { pattern: seenPat, windowSecs: Number.isFinite(seenWin) && seenWin > 0 ? seenWin : 60 };
    }
  }
  try {
    await invoke("save_alerting_route", {
      rule: {
        id: "",
        name,
        priority,
        enabled: true,
        kindPattern: pattern,
        payloadPath: null,
        payloadMatch: null,
        targetEndpointIds: targetIds,
        recipients: [],
        tags: [],
        seenInLast,
      },
    });
    if (msg) msg.textContent = t("alertingRouteCreated");
    void refreshAlertingRoutes();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

export async function onImportRoutesYaml(): Promise<void> {
  const msg = document.getElementById("alerting-route-msg");
  const yaml = window.prompt(t("alertingRouteImportPrompt"), "version: 1\nrules: []\n");
  if (!yaml) return;
  try {
    const n = await invoke<number>("import_alerting_routes_yaml", { yaml });
    if (msg) msg.textContent = `${t("alertingRouteImported")}: ${n}`;
    void refreshAlertingRoutes();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

export async function onExportRoutesYaml(): Promise<void> {
  const msg = document.getElementById("alerting-route-msg");
  try {
    const yaml = await invoke<string>("export_alerting_routes_yaml");
    // Write the YAML to the clipboard so the user can copy it easily
    try {
      await navigator.clipboard.writeText(yaml);
      if (msg) msg.textContent = t("alertingRouteExportedClipboard");
    } catch {
      window.prompt(t("alertingRouteExportPrompt"), yaml);
      if (msg) msg.textContent = t("alertingRouteExported");
    }
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

export async function onDryRunRoute(): Promise<void> {
  const msg = document.getElementById("alerting-route-msg");
  const source = window.prompt(t("alertingRouteDryRunSourcePrompt"), "plugin.metrics.exceeded") || "";
  if (!source) return;
  const payloadJson = window.prompt(t("alertingRouteDryRunPayloadPrompt"), '{"cpu_percent": 95}') || "";
  try {
    const hit = await invoke<RouteRuleDto | null>("dry_run_alerting_route", {
      source,
      payloadJson,
    });
    if (hit) {
      if (msg) msg.textContent = `${t("alertingRouteDryRunHit")}: ${hit.name} (#${hit.priority})`;
    } else {
      if (msg) msg.textContent = t("alertingRouteDryRunNoHit");
    }
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}
