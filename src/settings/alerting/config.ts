import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc, escAttr } from "../shared";

// ─── Phase 47: Alerting webhook configuration ──────────────────────────────────────────

export interface WebhookHeader { key: string; value: string }

interface WebhookConfigDto {
  enabled: boolean;
  url: string;
  minIntervalSecs: number;
  sources: {
    metricsExceeded: boolean;
    slaViolated: boolean;
    killSwitchEngaged: boolean;
    pluginCrashed: boolean;
  };
  customHeaders: WebhookHeader[];
  schemaVersion: number;
}

let alertingConfig: WebhookConfigDto | null = null;

export async function refreshAlerting(): Promise<void> {
  const form = document.getElementById("alerting-form");
  if (!form) return;
  try {
    alertingConfig = await invoke<WebhookConfigDto>("get_alerting_config");
  } catch (err) {
    form.innerHTML = `<div class="setting-hint">${esc(String(err))}</div>`;
    return;
  }
  renderAlertingForm();
}

function renderAlertingForm(): void {
  const form = document.getElementById("alerting-form");
  if (!form || !alertingConfig) return;
  const c = alertingConfig;
  document.getElementById("alerting-hero-dot")?.classList.toggle("ok", c.enabled);
  form.innerHTML = `
    <div class="alerting-master">
      <div class="alerting-master-info">
        <span class="setting-label">${esc(t("alertingEnabled"))}</span>
        <span class="setting-hint" id="alerting-enabled-state">${esc(t(c.enabled ? "alertingAggregationEnabledOn" : "alertingAggregationEnabledOff"))}</span>
      </div>
      <button class="toggle-switch${c.enabled ? " active" : ""}" id="alerting-enabled" type="button" aria-pressed="${c.enabled}" aria-label="${esc(t("alertingEnabled"))}"><span class="toggle-slider"></span></button>
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingUrl"))}</label>
      <input type="text" class="logs-input alerting-url" id="alerting-url" placeholder="https://hooks.slack.com/..." value="${escAttr(c.url)}" />
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingMinInterval"))}</label>
      <input type="number" class="logs-input alerting-interval" id="alerting-interval" min="0" max="3600" value="${c.minIntervalSecs}" />
    </div>
    <div class="alerting-row vertical">
      <label class="alerting-label">${esc(t("alertingSources"))}</label>
      <div class="alerting-sources">
        <label><input type="checkbox" id="alerting-src-metrics"${c.sources.metricsExceeded ? " checked" : ""}/> ${esc(t("alertingSourceMetrics"))}</label>
        <label><input type="checkbox" id="alerting-src-sla"${c.sources.slaViolated ? " checked" : ""}/> ${esc(t("alertingSourceSla"))}</label>
        <label><input type="checkbox" id="alerting-src-ks"${c.sources.killSwitchEngaged ? " checked" : ""}/> ${esc(t("alertingSourceKillSwitch"))}</label>
        <label><input type="checkbox" id="alerting-src-crash"${c.sources.pluginCrashed ? " checked" : ""}/> ${esc(t("alertingSourceCrash"))}</label>
      </div>
    </div>
    <div class="alerting-row vertical">
      <label class="alerting-label">${esc(t("alertingHeaders"))}</label>
      <div id="alerting-headers"></div>
      <button class="btn ghost" id="alerting-add-header" type="button">${esc(t("alertingAddHeader"))}</button>
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingSchemaVersion"))}</label>
      <select class="logs-input alerting-schema-version" id="alerting-schema-version">
        <option value="0"${(c.schemaVersion ?? 0) === 0 ? " selected" : ""}>${esc(t("alertingSchemaLegacy"))}</option>
        <option value="1"${(c.schemaVersion ?? 0) === 1 ? " selected" : ""}>${esc(t("alertingSchemaCanonical"))}</option>
      </select>
      <span class="setting-hint">${esc(t("alertingSchemaVersionHint"))}</span>
    </div>`;
  renderAlertingHeaders(c.customHeaders);
  document.getElementById("alerting-enabled")?.addEventListener("click", () => void onAlertingMasterToggle());
  document.getElementById("alerting-url")?.addEventListener("input", () => clearAlertingUrlInvalid());
  document.getElementById("alerting-add-header")?.addEventListener("click", () => {
    if (!alertingConfig) return;
    alertingConfig.customHeaders.push({ key: "", value: "" });
    renderAlertingHeaders(alertingConfig.customHeaders);
  });
}

function renderAlertingHeaders(headers: WebhookHeader[]): void {
  const box = document.getElementById("alerting-headers");
  if (!box) return;
  if (headers.length === 0) {
    box.innerHTML = `<div class="setting-hint">${esc(t("alertingHeadersHint"))}</div>`;
    return;
  }
  box.innerHTML = headers.map((h, i) =>
    `<div class="alerting-header-row">
       <input type="text" class="logs-input alerting-hkey" data-i="${i}" placeholder="${esc(t("alertingHeaderKey"))}" value="${escAttr(h.key)}" />
       <input type="text" class="logs-input alerting-hval" data-i="${i}" placeholder="${esc(t("alertingHeaderValue"))}" value="${escAttr(h.value)}" />
       <button class="btn ghost alerting-hdel" data-i="${i}" type="button">×</button>
     </div>`
  ).join("");
  box.querySelectorAll<HTMLInputElement>(".alerting-hkey").forEach((el) => {
    el.addEventListener("input", () => {
      const i = Number(el.dataset.i);
      if (alertingConfig) alertingConfig.customHeaders[i].key = el.value;
    });
  });
  box.querySelectorAll<HTMLInputElement>(".alerting-hval").forEach((el) => {
    el.addEventListener("input", () => {
      const i = Number(el.dataset.i);
      if (alertingConfig) alertingConfig.customHeaders[i].value = el.value;
    });
  });
  box.querySelectorAll<HTMLButtonElement>(".alerting-hdel").forEach((btn) => {
    btn.addEventListener("click", () => {
      const i = Number(btn.dataset.i);
      if (!alertingConfig) return;
      alertingConfig.customHeaders.splice(i, 1);
      renderAlertingHeaders(alertingConfig.customHeaders);
    });
  });
}

/// Toggle visuals + status text + hero dot: touch only these three, don't repaint the whole form (preserving focus/caret of other inputs).
function paintAlertingMasterSwitch(): void {
  const on = alertingConfig?.enabled ?? false;
  const btn = document.getElementById("alerting-enabled");
  btn?.classList.toggle("active", on);
  btn?.setAttribute("aria-pressed", String(on));
  const state = document.getElementById("alerting-enabled-state");
  if (state) state.textContent = t(on ? "alertingAggregationEnabledOn" : "alertingAggregationEnabledOff");
  document.getElementById("alerting-hero-dot")?.classList.toggle("ok", on);
}

function clearAlertingUrlInvalid(): void {
  const urlEl = document.getElementById("alerting-url") as HTMLInputElement | null;
  if (!urlEl) return;
  urlEl.classList.remove("alerting-url-invalid");
  urlEl.removeAttribute("aria-invalid");
}

/// One read path shared by save / toggle / test: sync all current form values into alertingConfig,
/// ensuring no entry point drops other fields the user is typing. enabled is read from the toggle's active class.
function syncAlertingFormToConfig(): void {
  if (!alertingConfig) return;
  const enabledEl = document.getElementById("alerting-enabled");
  const urlEl = document.getElementById("alerting-url") as HTMLInputElement | null;
  const intEl = document.getElementById("alerting-interval") as HTMLInputElement | null;
  const mEl = document.getElementById("alerting-src-metrics") as HTMLInputElement | null;
  const sEl = document.getElementById("alerting-src-sla") as HTMLInputElement | null;
  const kEl = document.getElementById("alerting-src-ks") as HTMLInputElement | null;
  const cEl = document.getElementById("alerting-src-crash") as HTMLInputElement | null;
  const svEl = document.getElementById("alerting-schema-version") as HTMLSelectElement | null;
  alertingConfig.enabled = enabledEl?.classList.contains("active") ?? false;
  alertingConfig.url = urlEl?.value ?? "";
  alertingConfig.minIntervalSecs = Math.max(0, Math.min(3600, Number(intEl?.value ?? 5)));
  alertingConfig.sources.metricsExceeded = mEl?.checked ?? false;
  alertingConfig.sources.slaViolated = sEl?.checked ?? false;
  alertingConfig.sources.killSwitchEngaged = kEl?.checked ?? false;
  alertingConfig.sources.pluginCrashed = cEl?.checked ?? false;
  alertingConfig.schemaVersion = Number(svEl?.value ?? 0);
}

/// master toggle: click-to-save immediately, the hero dot syncs at once; revert the visuals on failure.
/// With an empty URL the backend semantics don't change (it still saves); the frontend only does 'hint + highlight + focus'.
async function onAlertingMasterToggle(): Promise<void> {
  if (!alertingConfig) return;
  const msg = document.getElementById("alerting-msg");
  syncAlertingFormToConfig();
  alertingConfig.enabled = !alertingConfig.enabled;
  paintAlertingMasterSwitch();
  try {
    await invoke("set_alerting_config", { cfg: alertingConfig });
    if (alertingConfig.enabled && alertingConfig.url.trim() === "") {
      if (msg) msg.textContent = `${t("alertingUrl")}: ${t("pluginValidateRequired")}`;
      const urlEl = document.getElementById("alerting-url") as HTMLInputElement | null;
      urlEl?.classList.add("alerting-url-invalid");
      urlEl?.setAttribute("aria-invalid", "true");
      urlEl?.focus();
    } else {
      clearAlertingUrlInvalid();
      if (msg) msg.textContent = t("alertingSaved");
    }
  } catch (err) {
    alertingConfig.enabled = !alertingConfig.enabled;
    paintAlertingMasterSwitch();
    if (msg) msg.textContent = `${t("alertingSaveFailed")}: ${String(err)}`;
  }
}

export async function saveAlertingConfig(): Promise<void> {
  const msg = document.getElementById("alerting-msg");
  if (!alertingConfig) return;
  syncAlertingFormToConfig();
  try {
    await invoke("set_alerting_config", { cfg: alertingConfig });
    document.getElementById("alerting-hero-dot")?.classList.toggle("ok", alertingConfig.enabled);
    if (msg) msg.textContent = t("alertingSaved");
  } catch (err) {
    if (msg) msg.textContent = `${t("alertingSaveFailed")}: ${String(err)}`;
  }
}

export async function testAlertingWebhook(): Promise<void> {
  const msg = document.getElementById("alerting-msg");
  if (!alertingConfig) return;
  syncAlertingFormToConfig();
  try {
    await invoke("set_alerting_config", { cfg: alertingConfig });
  } catch (err) {
    if (msg) msg.textContent = `${t("alertingSaveFailed")}: ${String(err)}`;
    return;
  }
  try {
    const status = await invoke<number>("test_alerting_webhook");
    if (msg) msg.textContent = `✓ ${t("alertingTestSuccess")} (${status})`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${t("alertingTestFailed")}: ${String(err)}`;
  }
}
