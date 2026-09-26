import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc, escAttr } from "../shared";

// ─── Phase 54: alerting aggregation / frequency threshold rules ──────────────────────────────────

interface AggregationRuleDto {
  id: string;
  name: string;
  kindPattern: string;
  windowSecs: number;
  thresholdCount: number;
  action: string;
  targetSeverity?: string | null;
  enabled: boolean;
  createdAt: number;
}

function aggMsg(text: string, ok: boolean): void {
  const msg = document.getElementById("agg-msg");
  if (msg) msg.textContent = `${ok ? "✓" : "✗"} ${text}`;
}

export function updateAggTargetSeverityVisibility(): void {
  const sel = document.getElementById("agg-action") as HTMLSelectElement | null;
  const tgt = document.getElementById("agg-target-severity") as HTMLSelectElement | null;
  if (!sel || !tgt) return;
  tgt.disabled = sel.value !== "downgrade";
}

export async function refreshAlertingAggregations(): Promise<void> {
  const list = document.getElementById("alerting-aggregations-list");
  if (!list) return;
  updateAggTargetSeverityVisibility();
  let rules: AggregationRuleDto[] = [];
  try {
    rules = await invoke<AggregationRuleDto[]>("list_alerting_aggregations");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">✗ ${String(err)}</div>`;
    return;
  }
  if (rules.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingAggregationsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = rules
    .map((r) => {
      const tgt = r.action === "downgrade" && r.targetSeverity
        ? `<span class="agg-target-severity">→ ${esc(r.targetSeverity)}</span>`
        : "";
      const enabledBadge = `<span class="agg-enabled-badge ${r.enabled ? "on" : "off"}">${r.enabled ? esc(t("alertingAggregationEnabledOn")) : esc(t("alertingAggregationEnabledOff"))}</span>`;
      const toggle = `<button class="btn ghost agg-toggle" data-id="${escAttr(r.id)}" type="button">${r.enabled ? esc(t("alertingAggregationDisable")) : esc(t("alertingAggregationEnable"))}</button>`;
      return `<div class="agg-card ${r.enabled ? "agg-card-on" : "agg-card-off"}" data-id="${escAttr(r.id)}">
        <span class="agg-name">${esc(r.name)}</span>
        <code class="agg-pattern">${esc(r.kindPattern)}</code>
        <span class="agg-count">${r.thresholdCount}× / ${r.windowSecs}s</span>
        <span class="agg-action-badge agg-action-${escAttr(r.action)}">${esc(r.action)}</span>
        ${tgt}
        ${enabledBadge}
        ${toggle}
        <button class="btn ghost agg-delete" data-id="${escAttr(r.id)}" type="button">${esc(t("alertingAggregationDelete"))}</button>
      </div>`;
    })
    .join("");
  list.querySelectorAll<HTMLButtonElement>(".agg-delete").forEach((btn) => {
    btn.addEventListener("click", () => void onDeleteAggregation(btn.dataset.id || ""));
  });
  list.querySelectorAll<HTMLButtonElement>(".agg-toggle").forEach((btn) => {
    btn.addEventListener("click", () => void onToggleAggregation(btn.dataset.id || ""));
  });
}

export async function onAddAggregation(): Promise<void> {
  const name = (document.getElementById("agg-name") as HTMLInputElement)?.value.trim() || "";
  const pattern = (document.getElementById("agg-pattern") as HTMLInputElement)?.value.trim() || "*";
  const windowSecs = parseInt((document.getElementById("agg-window") as HTMLInputElement)?.value || "0", 10);
  const threshold = parseInt((document.getElementById("agg-threshold") as HTMLInputElement)?.value || "0", 10);
  const action = (document.getElementById("agg-action") as HTMLSelectElement)?.value || "suppress";
  const targetSeverity = (document.getElementById("agg-target-severity") as HTMLSelectElement)?.value || null;
  if (!name) {
    aggMsg(t("alertingAggregationNameRequired"), false);
    return;
  }
  if (windowSecs <= 0 || threshold <= 0) {
    aggMsg(t("alertingAggregationInvalid"), false);
    return;
  }
  try {
    const rule: AggregationRuleDto = {
      id: "",
      name,
      kindPattern: pattern,
      windowSecs,
      thresholdCount: threshold,
      action,
      targetSeverity: action === "downgrade" ? targetSeverity : null,
      enabled: true,
      createdAt: 0,
    };
    const saved = await invoke<AggregationRuleDto>("save_alerting_aggregation", { rule });
    aggMsg(`${t("alertingSaved")}: ${saved.name}`, true);
    const n = document.getElementById("agg-name") as HTMLInputElement | null;
    if (n) n.value = "";
    void refreshAlertingAggregations();
  } catch (err) {
    aggMsg(String(err), false);
  }
}

async function onDeleteAggregation(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingAggregationDeleteConfirm"))) return;
  try {
    const ok = await invoke<boolean>("delete_alerting_aggregation", { id });
    aggMsg(ok ? t("alertingSaved") : t("alertingAggregationDeleteFailed"), ok);
    void refreshAlertingAggregations();
  } catch (err) {
    aggMsg(String(err), false);
  }
}

async function onToggleAggregation(id: string): Promise<void> {
  if (!id) return;
  try {
    const rules = await invoke<AggregationRuleDto[]>("list_alerting_aggregations");
    const r = rules.find((x) => x.id === id);
    if (!r) {
      aggMsg(t("alertingAggregationNotFound"), false);
      return;
    }
    const updated: AggregationRuleDto = { ...r, enabled: !r.enabled };
    await invoke<AggregationRuleDto>("save_alerting_aggregation", { rule: updated });
    aggMsg(`${t("alertingSaved")}: ${updated.name} (${updated.enabled ? "on" : "off"})`, true);
    void refreshAlertingAggregations();
  } catch (err) {
    aggMsg(String(err), false);
  }
}

export async function clearAllAggregations(): Promise<void> {
  if (!window.confirm(t("alertingAggregationClearConfirm"))) return;
  try {
    const n = await invoke<number>("clear_alerting_aggregations");
    aggMsg(`${t("alertingSaved")} (${n})`, true);
    void refreshAlertingAggregations();
  } catch (err) {
    aggMsg(String(err), false);
  }
}
