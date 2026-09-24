import { invoke } from "@tauri-apps/api/core";
import { t } from "../i18n";
import { esc, escAttr, listError, listSkeleton } from "./shared";

export function renderRules(body: HTMLElement): void {
  // Command rules: route an agent's shell command to a specified executor (see docs/rules.md).
  // The add form is inline in the tab (no dialog); project-level rules are read-only, their source stays in the repo file.
  // Three parts: 1) add rule (form) 2) existing rules (list) 3) trusted projects (trust management).
  body.innerHTML = `<div class="rules-page">
    <p class="rules-intro">${esc(t("rulesHint"))}</p>
    <section class="rules-sect">
      <p class="rules-sect-title">${esc(t("rulesAdd"))}</p>
      <div class="rules-form">
        <label class="rules-field"><span class="rules-label">${esc(t("rulesMatchLabel"))}</span><select id="rule-match-kind" aria-label="${escAttr(t("rulesMatchLabel"))}"><option value="prefix">${esc(t("rulesMatchPrefix"))}</option><option value="binary">${esc(t("rulesMatchBinary"))}</option><option value="regex">${esc(t("rulesMatchRegex"))}</option></select></label>
        <label class="rules-field"><span class="rules-label">${esc(t("rulesMatchValuePlaceholder"))}</span><input type="text" id="rule-match-value" aria-label="${escAttr(t("rulesMatchValuePlaceholder"))}" /></label>
        <label class="rules-field"><span class="rules-label">${esc(t("rulesXformLabel"))}</span><select id="rule-xform-kind" aria-label="${escAttr(t("rulesXformLabel"))}"><option value="prepend">${esc(t("rulesXformPrepend"))}</option><option value="replace_binary">${esc(t("rulesXformReplace"))}</option><option value="env">${esc(t("rulesXformEnv"))}</option></select></label>
        <div class="rules-xform">
          <label class="rules-field" id="rule-f-prepend"><span class="rules-label">${esc(t("rulesPrependPlaceholder"))}</span><input type="text" id="rule-prepend" aria-label="${escAttr(t("rulesPrependPlaceholder"))}" /></label>
          <label class="rules-field" id="rule-f-from" hidden><span class="rules-label">${esc(t("rulesFromPlaceholder"))}</span><input type="text" id="rule-from" aria-label="${escAttr(t("rulesFromPlaceholder"))}" /></label>
          <label class="rules-field" id="rule-f-to" hidden><span class="rules-label">${esc(t("rulesToPlaceholder"))}</span><input type="text" id="rule-to" aria-label="${escAttr(t("rulesToPlaceholder"))}" /></label>
          <label class="rules-field" id="rule-f-envk" hidden><span class="rules-label">${esc(t("rulesEnvKeyPlaceholder"))}</span><input type="text" id="rule-env-k" aria-label="${escAttr(t("rulesEnvKeyPlaceholder"))}" /></label>
          <label class="rules-field" id="rule-f-envv" hidden><span class="rules-label">${esc(t("rulesEnvValuePlaceholder"))}</span><input type="text" id="rule-env-v" aria-label="${escAttr(t("rulesEnvValuePlaceholder"))}" /></label>
        </div>
        <label class="rules-field rules-field-wide"><span class="rules-label">${esc(t("rulesIdOptional"))}</span><input type="text" id="rule-id" aria-label="${escAttr(t("rulesIdOptional"))}" /></label>
      </div>
      <div class="rules-actions"><button class="btn primary" id="rule-save" type="button">${esc(t("rulesAdd"))}</button><span class="rules-msg" id="rule-msg" role="status"></span></div>
    </section>
    <section class="rules-sect">
      <div class="rules-sect-head"><p class="rules-sect-title">${esc(t("rulesListTitle"))}</p><span class="rules-count" id="rules-msg"></span></div>
      <div id="rules-list" class="rules-list"></div>
    </section>
    <section class="rules-sect">
      <p class="rules-sect-title">${esc(t("rulesTrustTitle"))}</p>
      <p class="rules-sect-hint">${esc(t("rulesTrustHint"))}</p>
      <div class="rules-trust-form"><input class="rules-input" id="rules-trust-path" placeholder="${escAttr(t("rulesTrustPlaceholder"))}" aria-label="${escAttr(t("rulesTrustPlaceholder"))}" type="text"/><button class="btn ghost" id="rules-trust-add" type="button">${esc(t("rulesTrustAdd"))}</button></div>
      <span class="rules-msg" id="rules-trust-msg" role="status"></span>
      <div id="rules-trust-list" class="rules-trust-list"></div>
    </section>
  </div>`;
  document.getElementById("rule-save")?.addEventListener("click", () => void submitRuleForm());
  document.getElementById("rule-xform-kind")?.addEventListener("change", () => syncRuleXformFields());
  document.getElementById("rules-trust-add")?.addEventListener("click", () => void addTrustedProject());
  syncRuleXformFields();
  void refreshRules();
}

interface RuleSummary {
  id: string;
  enabled: boolean;
  stage: string;
  matcher: string;
  action: string;
  source: string;
}

/// Inline message funnel: textContent + error-state class in one pass; empty text also clears the error state,
/// so a later refresh/success receipt doesn't inherit the previous round's red text.
function setRuleMsg(msg: HTMLElement | null, text: string, isErr = false): void {
  if (!msg) return;
  msg.textContent = text;
  msg.classList.toggle("is-err", isErr && text !== "");
}

/// Command rule row: the source layer is key information — global can be toggled here (the toggle is .toggle-switch),
/// project-level is read-only (changing it requires editing files in the repo); read-only rows also show status text + a read-only hint (not relying on dot color alone).
/// Three inline segments: id/source badge -> match -> rewrite; the control column always has exactly one active control.
function ruleSummaryRow(r: RuleSummary): string {
  const editable = r.source === "global";
  const stateLabel = r.enabled ? t("automationEnabled") : t("automationDisabled");
  const control = editable
    ? `<button class="toggle-switch${r.enabled ? " active" : ""}" data-rule-toggle="${escAttr(r.id)}" type="button" role="switch" aria-checked="${r.enabled}" aria-label="${escAttr(r.id)}" title="${escAttr(stateLabel)}"><span class="toggle-slider"></span></button>`
    : `<span class="rules-state">${esc(stateLabel)} · ${esc(t("rulesReadOnly"))}</span>`;
  const sourceCls = r.source === "global" ? " is-global" : "";
  return `<div class="rules-row">
    <span class="rules-dot ${r.enabled ? "on" : "off"}" aria-hidden="true"></span>
    <div class="rules-main">
      <div class="rules-head"><span class="rules-id">${esc(r.id)}</span><span class="rules-source${sourceCls}" title="${escAttr(r.source)}">${esc(r.source)}</span></div>
      <div class="rules-detail">${r.matcher ? `<span class="rules-match">${esc(r.matcher)}</span><span class="rules-arrow" aria-hidden="true">→</span>` : ""}<span class="rules-action">${esc(r.action)}</span></div>
    </div>
    <div class="rules-side">${control}</div>
  </div>`;
}

async function refreshRules(): Promise<void> {
  const box = document.getElementById("rules-list");
  const msg = document.getElementById("rules-msg");
  if (!box) return;
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  let rules: RuleSummary[] = [];
  let failed = false;
  try {
    rules = await invoke<RuleSummary[]>("list_rules");
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  if (failed) {
    setRuleMsg(msg, "");
    box.innerHTML = listError("listLoadFailed", "rules-retry");
    document.getElementById("rules-retry")?.addEventListener("click", () => void refreshRules());
    return;
  }
  if (rules.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("rulesEmpty"))}</div>`;
    setRuleMsg(msg, "");
  } else {
    box.innerHTML = rules.map(ruleSummaryRow).join("");
    box.querySelectorAll("button[data-rule-toggle]").forEach((b) => {
      b.addEventListener("click", async () => {
        const id = (b as HTMLElement).dataset.ruleToggle ?? "";
        const rule = rules.find((r) => r.id === id);
        await invoke("set_rule_enabled", { id, enabled: !(rule?.enabled ?? true) });
        await refreshRules();
      });
    });
    setRuleMsg(msg, `${rules.length} ${t("listItems")}`);
  }
  await refreshTrustedProjects();
}

async function refreshTrustedProjects(): Promise<void> {
  const box = document.getElementById("rules-trust-list");
  if (!box) return;
  let paths: string[] = [];
  try {
    paths = await invoke<string[]>("list_trusted_projects");
  } catch {
    paths = [];
  }
  if (paths.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("rulesTrustEmpty"))}</div>`;
    return;
  }
  box.innerHTML = paths
    .map(
      (p) =>
        `<div class="rules-trust-row"><span class="rules-trust-path" title="${escAttr(p)}">${esc(p)}</span><button class="btn ghost danger" data-untrust="${escAttr(p)}" type="button">${esc(t("rulesUntrust"))}</button></div>`,
    )
    .join("");
  box.querySelectorAll("button[data-untrust]").forEach((b) => {
    b.addEventListener("click", async () => {
      await invoke("untrust_project", { path: (b as HTMLElement).dataset.untrust ?? "" });
      await refreshRules();
    });
  });
}

async function addTrustedProject(): Promise<void> {
  const input = document.getElementById("rules-trust-path") as HTMLInputElement | null;
  const msg = document.getElementById("rules-trust-msg");
  const path = input?.value.trim() ?? "";
  if (!path) {
    setRuleMsg(msg, t("rulesTrustNeedPath"), true);
    return;
  }
  try {
    await invoke("trust_project", { path });
    if (input) input.value = "";
    setRuleMsg(msg, t("rulesTrustAdded"));
  } catch (e) {
    setRuleMsg(msg, String(e), true);
  }
  await refreshRules();
}

/// Submit the inline form: the frontend only assembles fields; validation and writing are left to Rust's add_rule
/// (which re-runs RulesFile validation; invalid fields such as shell are rejected here).
async function submitRuleForm(): Promise<void> {
  const msg = document.getElementById("rule-msg");
  const fail = (text: string): void => setRuleMsg(msg, text, true);
  const built = buildRuleFromForm(fail);
  if (!built) return;
  try {
    await invoke("add_rule", { rule: built });
  } catch (e) {
    fail(`${t("rulesAddFailed")} ${String(e)}`);
    return;
  }
  setRuleMsg(msg, t("rulesAdded"));
  clearRuleForm();
  await refreshRules();
}

function clearRuleForm(): void {
  for (const id of [
    "rule-id",
    "rule-match-value",
    "rule-prepend",
    "rule-from",
    "rule-to",
    "rule-env-k",
    "rule-env-v",
  ]) {
    const el = document.getElementById(id) as HTMLInputElement | null;
    if (el) el.value = "";
  }
}

/// Toggle input visibility by the chosen transform (toggling only the input leaves an empty label, so toggle the wrapper too).
function syncRuleXformFields(): void {
  const kind = (document.getElementById("rule-xform-kind") as HTMLSelectElement | null)?.value ?? "prepend";
  const show = (id: string, on: boolean): void => {
    const el = document.getElementById(id);
    if (el) (el as HTMLElement).hidden = !on;
  };
  show("rule-f-prepend", kind === "prepend");
  show("rule-f-from", kind === "replace_binary");
  show("rule-f-to", kind === "replace_binary");
  show("rule-f-envk", kind === "env");
  show("rule-f-envv", kind === "env");
}

/// Assemble the rule JSON; when fields are incomplete, report an error via fail and return null.
function buildRuleFromForm(fail: (text: string) => void): Record<string, unknown> | null {
  const val = (id: string): string =>
    (document.getElementById(id) as HTMLInputElement | null)?.value.trim() ?? "";
  const matchKind = (document.getElementById("rule-match-kind") as HTMLSelectElement | null)?.value ?? "prefix";
  const matchValue = val("rule-match-value");
  if (!matchValue) {
    fail(t("rulesNeedMatch"));
    return null;
  }
  const xformKind = (document.getElementById("rule-xform-kind") as HTMLSelectElement | null)?.value ?? "prepend";
  const then: Record<string, unknown> = { action: "rewrite" };
  if (xformKind === "prepend") {
    const wrapper = val("rule-prepend");
    if (!wrapper) {
      fail(t("rulesNeedTransform"));
      return null;
    }
    then.prepend = wrapper;
  } else if (xformKind === "replace_binary") {
    const from = val("rule-from");
    const to = val("rule-to");
    if (!from || !to) {
      fail(t("rulesNeedTransform"));
      return null;
    }
    then.replace_binary = { from, to };
  } else {
    const key = val("rule-env-k");
    const value = val("rule-env-v");
    if (!key || !value) {
      fail(t("rulesNeedTransform"));
      return null;
    }
    then.env = { [key]: value };
  }
  const rule: Record<string, unknown> = {
    enabled: true,
    when: { stage: "tool_pre", command: { [matchKind]: matchValue } },
    then,
  };
  const id = val("rule-id");
  if (id) rule.id = id;
  return rule;
}
