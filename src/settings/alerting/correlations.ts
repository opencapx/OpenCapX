import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc, escAttr } from "../shared";

// ─── Phase 55: alerting correlation suppression (B is suppressed within window_secs after A) ────────

interface CorrelationRuleDto {
  id: string;
  name: string;
  kindPatternA: string;
  kindPatternB: string;
  windowSecs: number;
  enabled: boolean;
  createdAt: number;
}

function corrMsg(text: string, ok: boolean): void {
  const msg = document.getElementById("corr-msg");
  if (msg) msg.textContent = `${ok ? "✓" : "✗"} ${text}`;
}

export async function refreshAlertingCorrelations(): Promise<void> {
  const list = document.getElementById("alerting-correlations-list");
  if (!list) return;
  let rules: CorrelationRuleDto[] = [];
  try {
    rules = await invoke<CorrelationRuleDto[]>("list_alerting_correlations");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">✗ ${String(err)}</div>`;
    return;
  }
  if (rules.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingCorrelationsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = rules
    .map((r) => {
      const enabledBadge = `<span class="corr-enabled-badge ${r.enabled ? "on" : "off"}">${r.enabled ? esc(t("alertingCorrelationEnabledOn")) : esc(t("alertingCorrelationEnabledOff"))}</span>`;
      const toggle = `<button class="btn ghost corr-toggle" data-id="${escAttr(r.id)}" type="button">${r.enabled ? esc(t("alertingCorrelationDisable")) : esc(t("alertingCorrelationEnable"))}</button>`;
      return `<div class="corr-card ${r.enabled ? "corr-card-on" : "corr-card-off"}" data-id="${escAttr(r.id)}">
        <span class="corr-name">${esc(r.name)}</span>
        <code class="corr-pattern-a">${esc(r.kindPatternA)}</code>
        <span class="corr-arrow">→</span>
        <code class="corr-pattern-b">${esc(r.kindPatternB)}</code>
        <span class="corr-window">≤ ${r.windowSecs}s</span>
        ${enabledBadge}
        ${toggle}
        <button class="btn ghost corr-delete" data-id="${escAttr(r.id)}" type="button">${esc(t("alertingCorrelationDelete"))}</button>
      </div>`;
    })
    .join("");
  list.querySelectorAll<HTMLButtonElement>(".corr-delete").forEach((btn) => {
    btn.addEventListener("click", () => void onDeleteCorrelation(btn.dataset.id || ""));
  });
  list.querySelectorAll<HTMLButtonElement>(".corr-toggle").forEach((btn) => {
    btn.addEventListener("click", () => void onToggleCorrelation(btn.dataset.id || ""));
  });
}

export async function onAddCorrelation(): Promise<void> {
  const name = (document.getElementById("corr-name") as HTMLInputElement)?.value.trim() || "";
  const pa = (document.getElementById("corr-pattern-a") as HTMLInputElement)?.value.trim() || "";
  const pb = (document.getElementById("corr-pattern-b") as HTMLInputElement)?.value.trim() || "";
  const windowSecs = parseInt((document.getElementById("corr-window") as HTMLInputElement)?.value || "0", 10);
  if (!name) {
    corrMsg(t("alertingCorrelationNameRequired"), false);
    return;
  }
  if (windowSecs <= 0) {
    corrMsg(t("alertingCorrelationInvalid"), false);
    return;
  }
  try {
    const rule: CorrelationRuleDto = {
      id: "",
      name,
      kindPatternA: pa,
      kindPatternB: pb,
      windowSecs,
      enabled: true,
      createdAt: 0,
    };
    const saved = await invoke<CorrelationRuleDto>("save_alerting_correlation", { rule });
    corrMsg(`${t("alertingSaved")}: ${saved.name}`, true);
    const n = document.getElementById("corr-name") as HTMLInputElement | null;
    if (n) n.value = "";
    void refreshAlertingCorrelations();
  } catch (err) {
    corrMsg(String(err), false);
  }
}

async function onDeleteCorrelation(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingCorrelationDeleteConfirm"))) return;
  try {
    const ok = await invoke<boolean>("delete_alerting_correlation", { id });
    corrMsg(ok ? t("alertingSaved") : t("alertingCorrelationDeleteFailed"), ok);
    void refreshAlertingCorrelations();
  } catch (err) {
    corrMsg(String(err), false);
  }
}

async function onToggleCorrelation(id: string): Promise<void> {
  if (!id) return;
  try {
    const rules = await invoke<CorrelationRuleDto[]>("list_alerting_correlations");
    const r = rules.find((x) => x.id === id);
    if (!r) {
      corrMsg(t("alertingCorrelationNotFound"), false);
      return;
    }
    const updated: CorrelationRuleDto = { ...r, enabled: !r.enabled };
    await invoke<CorrelationRuleDto>("save_alerting_correlation", { rule: updated });
    corrMsg(`${t("alertingSaved")}: ${updated.name} (${updated.enabled ? "on" : "off"})`, true);
    void refreshAlertingCorrelations();
  } catch (err) {
    corrMsg(String(err), false);
  }
}

export async function clearAllCorrelations(): Promise<void> {
  if (!window.confirm(t("alertingCorrelationClearConfirm"))) return;
  try {
    const n = await invoke<number>("clear_alerting_correlations");
    corrMsg(`${t("alertingSaved")} (${n})`, true);
    void refreshAlertingCorrelations();
  } catch (err) {
    corrMsg(String(err), false);
  }
}
