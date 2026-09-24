import { invoke } from "@tauri-apps/api/core";
import { t } from "../i18n";
import { esc, group, listError, listSkeleton } from "./shared";

// ─── i2 §15 Automation ───────────────────────────────────────────────────────

export function renderAutomation(body: HTMLElement): void {
  // i2 §15 Automation: Event -> Rule -> Action; rules land in ~/.opencapx/automation.json (see docs/automation.md)
  // The form goes into a dialog: the tab keeps only 'Add rule' + count/errors + the rule list, so the list owns the main surface.
  body.innerHTML = group("tabAutomation",
    `<div class="setting-row vertical"><span class="setting-hint">${esc(t("automationHint"))}</span><div class="rule-toolbar"><button class="btn" id="auto-add" type="button">${esc(t("automationAdd"))}</button><span class="setting-hint" id="automation-msg"></span></div><div id="automation-list"></div></div>`);
  document.getElementById("auto-add")?.addEventListener("click", () => openAutomationDialog());
  void refreshAutomation();
}

interface AutomationRule {
  id: string;
  enabled?: boolean;
  when?: { event?: string; match?: Record<string, unknown> };
  then?: { action?: string; title?: string; body?: string; text?: string };
}

/// notify uses title+body, say uses text; toggle input visibility by action.
/// In the dialog the input is wrapped in .filter-field: toggling only the input's hidden leaves an empty label, so toggle the wrapper too.
function syncAutomationActionFields(): void {
  const action = (document.getElementById("auto-action") as HTMLSelectElement | null)?.value ?? "notify";
  const notifyOnly = ["auto-title", "auto-body"];
  for (const id of notifyOnly) toggleAutoField(id, action === "notify");
  toggleAutoField("auto-text", action === "say");
}

/// Collapse/expand one dialog field: toggle the hidden state of both the input itself (by id, preserving sync's original semantics) and its label wrapper.
function toggleAutoField(id: string, visible: boolean): void {
  const el = document.getElementById(id);
  el?.toggleAttribute("hidden", !visible);
  el?.closest(".filter-field")?.toggleAttribute("hidden", !visible);
}

/// Rule row: multi-line layout — event / condition / action each on its own line; match is rendered as k = v pairs instead of raw JSON.
/// Enabled state uses two channels: dot color (.dot.done/.idle) + button text (enable/disable), not color alone.
function automationRuleRow(r: AutomationRule): string {
  const on = r.enabled !== false;
  const match = r.when?.match ? Object.entries(r.when.match).map(([k, v]) => `${k} = ${JSON.stringify(v)}`).join(" · ") : "";
  const actionName = r.then?.action === "say" ? t("automationActionSay") : t("automationActionNotify");
  const detail = r.then?.action === "say" ? (r.then.text ?? "") : [r.then?.title, r.then?.body].filter(Boolean).join(" · ");
  return `<div class="sess rule-row"><span class="dot ${on ? "done" : "idle"}"></span><div class="rule-text"><div class="rule-event">${esc(r.when?.event ?? "—")}</div>${match ? `<div class="rule-cond">${esc(match)}</div>` : ""}<div class="rule-action">${esc(actionName)}${detail ? ` · ${esc(detail)}` : ""}</div></div><div class="rule-btns"><button class="btn ghost" data-auto-toggle="${esc(r.id)}" type="button">${esc(on ? t("automationEnabled") : t("automationDisabled"))}</button><button class="btn ghost danger" data-auto-del="${esc(r.id)}" type="button">${esc(t("automationRemove"))}</button></div></div>`;
}

async function refreshAutomation(): Promise<void> {
  const box = document.getElementById("automation-list");
  const msg = document.getElementById("automation-msg");
  if (!box) return;
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  let rules: AutomationRule[] = [];
  let failed = false;
  try {
    rules = await invoke<AutomationRule[]>("list_automation_rules");
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  if (failed) {
    if (msg) msg.textContent = "";
    box.innerHTML = listError("listLoadFailed", "automation-retry");
    document.getElementById("automation-retry")?.addEventListener("click", () => void refreshAutomation());
    return;
  }
  if (rules.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("automationEmpty"))}</div>`;
    if (msg) msg.textContent = "";
    return;
  }
  box.innerHTML = rules.map(automationRuleRow).join("");
  box.querySelectorAll("button[data-auto-toggle]").forEach((b) => {
    b.addEventListener("click", async () => {
      const id = (b as HTMLElement).dataset.autoToggle ?? "";
      const rule = rules.find((r) => r.id === id);
      await invoke("set_automation_rule_enabled", { id, enabled: !(rule?.enabled !== false) });
      await refreshAutomation();
    });
  });
  box.querySelectorAll("button[data-auto-del]").forEach((b) => {
    b.addEventListener("click", async () => {
      await invoke("remove_automation_rule", { id: (b as HTMLElement).dataset.autoDel ?? "" });
      await refreshAutomation();
    });
  });
  if (msg) msg.textContent = `${rules.length} ${t("listItems")}`;
}

/// Add-rule dialog: reuses the .install-dialog look and the existing overlay construction (click backdrop to close, Escape to close).
/// Validation/write errors go only to #auto-dlg-msg inside the dialog, not to the tab's #automation-msg; on success clear fields, close the dialog, refresh the list.
/// Focus: enters at #auto-event, returns to #auto-add.
function openAutomationDialog(): void {
  const overlay = document.createElement("div");
  overlay.id = "auto-dialog-overlay";
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog auto-dialog" role="dialog" aria-label="${esc(t("automationAdd"))}">
      <h2>${esc(t("automationAdd"))}</h2>
      <div class="auto-form">
        <label class="filter-field"><span class="filter-label">${esc(t("automationEventLabel"))}</span><input type="text" id="auto-event" placeholder="${esc(t("automationEventPlaceholder"))}" aria-label="${esc(t("automationEventLabel"))}" /></label>
        <label class="filter-field"><span class="filter-label">${esc(t("automationMatchLabel"))}</span><input type="text" id="auto-match" placeholder="${esc(t("automationMatchPlaceholder"))}" aria-label="${esc(t("automationMatchLabel"))}" /></label>
        <label class="filter-field"><span class="filter-label">${esc(t("automationActionLabel"))}</span><select id="auto-action" aria-label="${esc(t("automationActionLabel"))}"><option value="notify">${esc(t("automationActionNotify"))}</option><option value="say">${esc(t("automationActionSay"))}</option></select></label>
        <label class="filter-field"><span class="filter-label">${esc(t("automationTitlePlaceholder"))}</span><input type="text" id="auto-title" aria-label="${esc(t("automationTitlePlaceholder"))}" /></label>
        <label class="filter-field"><span class="filter-label">${esc(t("automationBodyPlaceholder"))}</span><input type="text" id="auto-body" aria-label="${esc(t("automationBodyPlaceholder"))}" /></label>
        <label class="filter-field"><span class="filter-label">${esc(t("automationSayPlaceholder"))}</span><input type="text" id="auto-text" aria-label="${esc(t("automationSayPlaceholder"))}" hidden /></label>
      </div>
      <span class="auto-msg" id="auto-dlg-msg"></span>
      <div class="install-actions">
        <button class="btn ghost" id="auto-cancel" type="button">${esc(t("installPreviewCancel"))}</button>
        <button class="btn primary" id="auto-save" type="button">${esc(t("automationAdd"))}</button>
      </div>
    </div>`;
  document.body.appendChild(overlay);
  const close = (): void => {
    document.removeEventListener("keydown", onKey, true);
    overlay.remove();
    document.getElementById("auto-add")?.focus();
  };
  function onKey(ev: KeyboardEvent): void {
    if (ev.key === "Escape") close();
  }
  document.addEventListener("keydown", onKey, true);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) close();
  });
  overlay.querySelector("#auto-cancel")?.addEventListener("click", close);
  overlay.querySelector("#auto-save")?.addEventListener("click", async () => {
    const ok = await onAddAutomationRule((text) => {
      const m = overlay.querySelector("#auto-dlg-msg");
      if (m) m.textContent = text;
    });
    if (ok) close();
  });
  document.getElementById("auto-action")?.addEventListener("change", () => syncAutomationActionFields());
  syncAutomationActionFields();
  document.getElementById("auto-event")?.focus();
}

/// Validate -> write (payload is still { when, then }) -> clear fields. Returns whether it succeeded.
/// errorTo: where error text lands (passed by the dialog); defaults to the tab's #automation-msg (matching old behavior).
async function onAddAutomationRule(errorTo?: (text: string) => void): Promise<boolean> {
  const msg = document.getElementById("automation-msg");
  const fail = (text: string): boolean => {
    if (errorTo) errorTo(text);
    else if (msg) msg.textContent = text;
    return false;
  };
  const val = (id: string) => (document.getElementById(id) as HTMLInputElement | null)?.value.trim() ?? "";
  const event = val("auto-event");
  if (!event) return fail(t("automationNeedEvent"));
  const matchRaw = val("auto-match");
  let when: Record<string, unknown> = { event };
  if (matchRaw) {
    try {
      when = { event, match: JSON.parse(matchRaw) };
    } catch {
      return fail(t("automationBadJson"));
    }
  }
  const action = (document.getElementById("auto-action") as HTMLSelectElement | null)?.value ?? "notify";
  const then = action === "say" ? { action, text: val("auto-text") } : { action, title: val("auto-title"), body: val("auto-body") };
  try {
    await invoke("add_automation_rule", { when, then });
    for (const id of ["auto-event", "auto-match", "auto-title", "auto-body", "auto-text"]) {
      const el = document.getElementById(id) as HTMLInputElement | null;
      if (el) el.value = "";
    }
    if (msg) msg.textContent = t("automationAdded");
    await refreshAutomation();
    return true;
  } catch (e) {
    return fail(`✗ ${String(e)}`);
  }
}
