import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc, escAttr } from "../shared";
import { refreshAlertingAggregations } from "./aggregations";
import { refreshAlertingCorrelations } from "./correlations";
import { refreshAlertingRoutes } from "./routes";

// ─── Phase 53: Alerting severity hints ─────────────────────────────────────────

interface SeverityHintDto {
  source: string;
  severity: string;
  origin: string;
  pluginId?: string;
  updatedAt: number;
  effectiveSeverity: string;   // Phase 69 — effective severity actually applied
}

interface SeverityLinkDto {
  policy: "manifest" | "plugin_default" | "user_override" | "disabled";
  severity: string | null;
  source: string;
  pluginId?: string;
  hit: boolean;
}

interface CascadeReportDto {
  hintDeleted: boolean;
  affectedRoutes: string[];
  affectedCorrelations: string[];
  affectedAggregations: string[];
}

interface PropagationTraceDto {
  source: string;
  routeSeverity: string;
  routeOrigin: "manifest" | "plugin_default" | "user_override" | "disabled";
  correlationSeverity: string;
  correlationOrigin: "manifest" | "plugin_default" | "user_override" | "disabled";
  aggregationSeverity: string;
  aggregationOrigin: "manifest" | "plugin_default" | "user_override" | "disabled";
  escalationSeverity: string;
  escalationOrigin: "manifest" | "plugin_default" | "user_override" | "disabled";
}

export async function refreshAlertingSeverityHints(): Promise<void> {
  const list = document.getElementById("alerting-severity-hints-list");
  if (!list) return;
  let hints: SeverityHintDto[] = [];
  try {
    hints = await invoke<SeverityHintDto[]>("list_alerting_severity_hints");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">${esc(String(err))}</div>`;
    return;
  }
  if (hints.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingSeverityHintsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = hints.map((h) => {
    const opts = ["info", "warn", "error", "critical"].map((s) =>
      `<option value="${s}"${s === h.severity ? " selected" : ""}>${s}</option>`
    ).join("");
    const originLabel = h.origin === "manifest"
      ? t("alertingSeverityHintOriginManifest")
      : t("alertingSeverityHintOriginUser");
    const owner = h.pluginId ? ` <span class="setting-hint">@${esc(h.pluginId)}</span>` : "";
    const effective = h.effectiveSeverity !== h.severity
      ? ` <span class="severity-hint-effective">${esc(t("alertingSeverityEffective"))} <strong>${esc(h.effectiveSeverity)}</strong></span>`
      : "";
    return `
      <div class="severity-hint-card severity-hint-card-${escAttr(h.severity)}" data-source="${escAttr(h.source)}">
        <code class="severity-hint-source">${esc(h.source)}</code>
        <select class="logs-input severity-hint-severity" data-source="${escAttr(h.source)}">${opts}</select>
        <span class="severity-origin-badge severity-origin-badge-${escAttr(h.origin)}">${esc(originLabel)}</span>
        ${owner}
        ${effective}
        <button class="btn ghost severity-hint-preview" data-source="${escAttr(h.source)}" type="button" title="${esc(t("alertingSeverityPreviewChain"))}">↻</button>
        <button class="btn ghost severity-hint-propagation" data-source="${escAttr(h.source)}" type="button" title="${esc(t("alertingSeverityPreviewPropagation"))}">↗</button>
        <button class="btn ghost severity-hint-cascade" data-source="${escAttr(h.source)}" type="button" title="${esc(t("alertingSeverityCascadeDelete"))}">⌫</button>
        <button class="btn ghost severity-hint-delete" data-source="${escAttr(h.source)}" type="button">×</button>
      </div>`;
  }).join("");
  list.querySelectorAll<HTMLSelectElement>(".severity-hint-severity").forEach((el) => {
    el.addEventListener("change", () => {
      const source = el.dataset.source ?? "";
      void onUpdateSeverityHint(source, el.value);
    });
  });
  list.querySelectorAll<HTMLButtonElement>(".severity-hint-delete").forEach((btn) => {
    btn.addEventListener("click", () => {
      const source = btn.dataset.source ?? "";
      void onDeleteSeverityHint(source);
    });
  });
  list.querySelectorAll<HTMLButtonElement>(".severity-hint-preview").forEach((btn) => {
    btn.addEventListener("click", () => {
      const source = btn.dataset.source ?? "";
      void onPreviewSeverityChain(source);
    });
  });
  list.querySelectorAll<HTMLButtonElement>(".severity-hint-propagation").forEach((btn) => {
    btn.addEventListener("click", () => {
      const source = btn.dataset.source ?? "";
      void onPreviewPropagation(source);
    });
  });
  list.querySelectorAll<HTMLButtonElement>(".severity-hint-cascade").forEach((btn) => {
    btn.addEventListener("click", () => {
      const source = btn.dataset.source ?? "";
      void onCascadeDeleteSeverityHint(source);
    });
  });
}

export async function onAddSeverityHint(): Promise<void> {
  const msg = document.getElementById("severity-hints-msg");
  const srcEl = document.getElementById("severity-hint-source") as HTMLInputElement | null;
  const sevEl = document.getElementById("severity-hint-severity") as HTMLSelectElement | null;
  const source = (srcEl?.value ?? "").trim();
  const severity = sevEl?.value ?? "warn";
  if (!source) {
    if (msg) msg.textContent = t("alertingSeverityHintAddInvalid");
    return;
  }
  try {
    await invoke("save_alerting_severity_hint", { source, severity });
    if (srcEl) srcEl.value = "";
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
    void refreshAlertingSeverityHints();
    void refreshAlertingAggregations();
    void refreshAlertingCorrelations();
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function onUpdateSeverityHint(source: string, severity: string): Promise<void> {
  const msg = document.getElementById("severity-hints-msg");
  try {
    await invoke("save_alerting_severity_hint", { source, severity });
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function onDeleteSeverityHint(source: string): Promise<void> {
  const msg = document.getElementById("severity-hints-msg");
  if (!window.confirm(t("alertingSeverityHintsClearConfirm") + ` (${source})`)) return;
  try {
    const removed = await invoke<boolean>("delete_alerting_severity_hint", { source });
    if (msg) msg.textContent = removed ? `✓ ${t("alertingSaved")}` : `(manifest-origin; use uninstall plugin)`;
    void refreshAlertingSeverityHints();
    void refreshAlertingAggregations();
    void refreshAlertingCorrelations();
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

export async function clearAllSeverityHints(): Promise<void> {
  const msg = document.getElementById("severity-hints-msg");
  if (!window.confirm(t("alertingSeverityHintsClearConfirm"))) return;
  try {
    const n = await invoke<number>("clear_alerting_severity_hints");
    if (msg) msg.textContent = `${t("alertingSaved")} (${n})`;
    void refreshAlertingSeverityHints();
    void refreshAlertingAggregations();
    void refreshAlertingCorrelations();
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

// Phase 69 — severity policy chain preview + cascade delete
function policyLabel(policy: SeverityLinkDto["policy"]): string {
  switch (policy) {
    case "manifest":       return t("alertingSeverityPolicyManifest");
    case "plugin_default": return t("alertingSeverityPolicyPluginDefault");
    case "user_override":  return t("alertingSeverityPolicyUserOverride");
    case "disabled":       return t("alertingSeverityPolicyDisabled");
    default:               return policy;
  }
}

async function onPreviewSeverityChain(source: string): Promise<void> {
  const result = document.getElementById("alerting-severity-chain-result");
  const msg = document.getElementById("severity-hints-msg");
  if (!result) return;
  try {
    const links = await invoke<SeverityLinkDto[]>("severity_inheritance_chain", { source });
    const rows = links.map((l) => {
      const mark = l.hit ? "●" : "○";
      const sevText = l.severity ?? "—";
      const plugin = l.pluginId ? ` <span class="setting-hint">@${esc(l.pluginId)}</span>` : "";
      const cls = l.hit ? "severity-link-row severity-link-hit" : "severity-link-row";
      return `<div class="${cls}"><code>${mark}</code> <strong>${esc(policyLabel(l.policy))}</strong> → <code>${esc(sevText)}</code>${plugin}</div>`;
    }).join("");
    result.innerHTML = `<div class="setting-hint">${esc(source)}</div>${rows}`;
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function onCascadeDeleteSeverityHint(source: string): Promise<void> {
  const result = document.getElementById("alerting-severity-cascade-result");
  const msg = document.getElementById("severity-hints-msg");
  if (!window.confirm(t("alertingSeverityCascadeConfirm") + ` (${source})`)) return;
  try {
    const report = await invoke<CascadeReportDto>("delete_alerting_severity_hint_cascade", { source });
    const parts: string[] = [];
    if (report.hintDeleted) parts.push(`✓ ${t("alertingSeverityCascadeDone")}`);
    parts.push(`${t("alertingSeverityAffectedRoutes")}: <strong>${report.affectedRoutes.length}</strong>`);
    parts.push(`${t("alertingSeverityAffectedCorrelations")}: <strong>${report.affectedCorrelations.length}</strong>`);
    parts.push(`${t("alertingSeverityAffectedAggregations")}: <strong>${report.affectedAggregations.length}</strong>`);
    if (result) result.innerHTML = `<div class="setting-hint">${esc(source)} → ${parts.join(" · ")}</div>`;
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
    void refreshAlertingSeverityHints();
    void refreshAlertingRoutes();
    void refreshAlertingCorrelations();
    void refreshAlertingAggregations();
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

// Phase 70 — severity cross-chain propagation preview
async function onPreviewPropagation(source: string): Promise<void> {
  const result = document.getElementById("alerting-severity-propagation-result");
  const msg = document.getElementById("severity-hints-msg");
  if (!result) return;
  try {
    const tr = await invoke<PropagationTraceDto>("severity_propagation_trace", { source });
    const rows: [string, string, "manifest" | "plugin_default" | "user_override" | "disabled"][] = [
      [t("alertingSeverityPropagationRoute"),       tr.routeSeverity,       tr.routeOrigin],
      [t("alertingSeverityPropagationCorrelation"), tr.correlationSeverity, tr.correlationOrigin],
      [t("alertingSeverityPropagationAggregation"), tr.aggregationSeverity, tr.aggregationOrigin],
      [t("alertingSeverityPropagationEscalation"),  tr.escalationSeverity,  tr.escalationOrigin],
    ];
    result.innerHTML = `<div class="setting-hint">${esc(source)}</div>` + rows.map(([label, sev, origin]) =>
      `<div class="severity-propagation-row">
         <span class="severity-propagation-label">${esc(label)}</span>
         <code class="severity-propagation-severity">${esc(sev)}</code>
         <span class="severity-origin-badge severity-origin-badge-${escAttr(origin)}">${esc(policyLabel(origin))}</span>
       </div>`
    ).join("");
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}
