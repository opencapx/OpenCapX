import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc, escAttr } from "../shared";

// ─── Phase 56: alerting escalation chain ─────────────────────────────────

export interface EscalationRuleDto {
  id: string;
  name: string;
  kindPattern: string;
  escalateAfterSecs: number;
  targetSeverity: string;
  targetEndpointIds?: string[];
  enabled: boolean;
  createdAt: number;
}

function escMsg(text: string, ok: boolean): void {
  const msg = document.getElementById("esc-msg");
  if (msg) msg.textContent = `${ok ? "✓" : "✗"} ${text}`;
}

export async function refreshAlertingEscalations(): Promise<void> {
  const list = document.getElementById("alerting-escalations-list");
  if (!list) return;
  let rules: EscalationRuleDto[] = [];
  try {
    rules = await invoke<EscalationRuleDto[]>("list_alerting_escalations");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">✗ ${String(err)}</div>`;
    return;
  }
  if (rules.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingEscalationsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = rules
    .map((r) => {
      const enabledBadge = `<span class="esc-enabled-badge ${r.enabled ? "on" : "off"}">${r.enabled ? esc(t("alertingEscalationEnabledOn")) : esc(t("alertingEscalationEnabledOff"))}</span>`;
      const toggle = `<button class="btn ghost esc-toggle" data-id="${escAttr(r.id)}" type="button">${r.enabled ? esc(t("alertingEscalationDisable")) : esc(t("alertingEscalationEnable"))}</button>`;
      const endpointsLabel = r.targetEndpointIds && r.targetEndpointIds.length > 0
        ? ` → ${esc(r.targetEndpointIds.join(", "))}`
        : ` → ${esc(t("alertingEscalationAllEndpoints"))}`;
      return `<div class="esc-card ${r.enabled ? "esc-card-on" : "esc-card-off"}" data-id="${escAttr(r.id)}">
        <span class="esc-name">${esc(r.name)}</span>
        <code class="esc-pattern">${esc(r.kindPattern)}</code>
        <span class="esc-window">≥ ${r.escalateAfterSecs}s</span>
        <span class="esc-target-severity">${esc(r.targetSeverity)}</span>
        <span class="esc-endpoints">${endpointsLabel}</span>
        ${enabledBadge}
        ${toggle}
        <button class="btn ghost esc-delete" data-id="${escAttr(r.id)}" type="button">${esc(t("alertingEscalationDelete"))}</button>
      </div>`;
    })
    .join("");
  list.querySelectorAll<HTMLButtonElement>(".esc-delete").forEach((btn) => {
    btn.addEventListener("click", () => void onDeleteEscalation(btn.dataset.id || ""));
  });
  list.querySelectorAll<HTMLButtonElement>(".esc-toggle").forEach((btn) => {
    btn.addEventListener("click", () => void onToggleEscalation(btn.dataset.id || ""));
  });
}

export async function onAddEscalation(): Promise<void> {
  const name = (document.getElementById("esc-name") as HTMLInputElement)?.value.trim() || "";
  const pattern = (document.getElementById("esc-pattern") as HTMLInputElement)?.value.trim() || "";
  const afterSecs = parseInt((document.getElementById("esc-after") as HTMLInputElement)?.value || "0", 10);
  const severity = (document.getElementById("esc-target-severity") as HTMLSelectElement)?.value || "critical";
  const endpointsRaw = (document.getElementById("esc-endpoints") as HTMLInputElement)?.value.trim() || "";
  if (!name) {
    escMsg(t("alertingEscalationNameRequired"), false);
    return;
  }
  if (afterSecs <= 0) {
    escMsg(t("alertingEscalationInvalid"), false);
    return;
  }
  const endpointIds = endpointsRaw
    ? endpointsRaw.split(",").map((s) => s.trim()).filter((s) => s.length > 0)
    : undefined;
  try {
    const rule: EscalationRuleDto = {
      id: "",
      name,
      kindPattern: pattern,
      escalateAfterSecs: afterSecs,
      targetSeverity: severity,
      targetEndpointIds: endpointIds,
      enabled: true,
      createdAt: 0,
    };
    const saved = await invoke<EscalationRuleDto>("save_alerting_escalation", { rule });
    escMsg(`${t("alertingSaved")}: ${saved.name}`, true);
    const n = document.getElementById("esc-name") as HTMLInputElement | null;
    if (n) n.value = "";
    void refreshAlertingEscalations();
  } catch (err) {
    escMsg(String(err), false);
  }
}

async function onDeleteEscalation(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingEscalationDeleteConfirm"))) return;
  try {
    const ok = await invoke<boolean>("delete_alerting_escalation", { id });
    escMsg(ok ? t("alertingSaved") : t("alertingEscalationDeleteFailed"), ok);
    void refreshAlertingEscalations();
  } catch (err) {
    escMsg(String(err), false);
  }
}

async function onToggleEscalation(id: string): Promise<void> {
  if (!id) return;
  try {
    const rules = await invoke<EscalationRuleDto[]>("list_alerting_escalations");
    const r = rules.find((x) => x.id === id);
    if (!r) {
      escMsg(t("alertingEscalationNotFound"), false);
      return;
    }
    const updated: EscalationRuleDto = { ...r, enabled: !r.enabled };
    await invoke<EscalationRuleDto>("save_alerting_escalation", { rule: updated });
    escMsg(`${t("alertingSaved")}: ${updated.name} (${updated.enabled ? "on" : "off"})`, true);
    void refreshAlertingEscalations();
  } catch (err) {
    escMsg(String(err), false);
  }
}

export async function clearAllEscalations(): Promise<void> {
  if (!window.confirm(t("alertingEscalationClearConfirm"))) return;
  try {
    const n = await invoke<number>("clear_alerting_escalations");
    escMsg(`${t("alertingSaved")} (${n})`, true);
    void refreshAlertingEscalations();
  } catch (err) {
    escMsg(String(err), false);
  }
}
