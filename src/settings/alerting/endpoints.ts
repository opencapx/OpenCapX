import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc, escAttr } from "../shared";
import { WebhookHeader } from "./config";
import { TemplatePreviewResult, onApplyPreset, onDeleteUserPreset, onExportPresets, onForkBuiltin, onImportPresets, onSavePreset, refreshTemplatePresets, userTemplatePresets } from "./presets";
import { refreshAlertingFailed } from "./retry";
import { ALERTING_SOURCES, DEFAULT_TEMPLATE_SAMPLE } from "./shared";

// ─── Phase 49: multi-endpoint fanout ────────────────────────────────────────────

export interface WebhookEndpointDto {
  id: string;
  name: string;
  url: string;
  enabled: boolean;
  headers: WebhookHeader[];
  secret: string;
  sourceFilter: string[];
  createdAt: number;
  schemaVersion: number;
  template?: string;        // Phase 57: optional per-endpoint template
  templateSample?: string;  // Phase 58: optional sample JSON for live preview (persisted)
  severityOverrides: Array<{ source: string; severity: string }>; // Phase 72: per-source severity override
}

// Phase 74 — endpoint override dry-run preview DTO
interface EndpointSeverityOverrideHitDto {
  pattern: string;
  severity: string;
  index: number;
}

export interface EndpointPreviewRowDto {
  endpointId: string;
  endpointName: string;
  overrideHit: EndpointSeverityOverrideHitDto | null;
  propagationSeverity: string;
  finalEnvelopeSeverity: string;
}

interface EndpointSeverityPreviewDto {
  source: string;
  endpoints: EndpointPreviewRowDto[];
}

export let alertingEndpoints: WebhookEndpointDto[] = [];

export async function refreshAlertingEndpoints(): Promise<void> {
  await refreshTemplatePresets(); // Phase 59: refresh the user preset cache first, so the UI can render the Custom group
  const list = document.getElementById("alerting-endpoints-list");
  if (!list) return;
  try {
    alertingEndpoints = await invoke<WebhookEndpointDto[]>("list_alerting_endpoints");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">${esc(String(err))}</div>`;
    return;
  }
  renderAlertingEndpoints();
}

function renderAlertingEndpoints(): void {
  const list = document.getElementById("alerting-endpoints-list");
  if (!list) return;
  if (alertingEndpoints.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingEndpointsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = alertingEndpoints.map((ep, i) => renderEndpointCard(ep, i)).join("");
  for (const ep of alertingEndpoints) {
    bindEndpointCard(ep);
  }
}

function renderOverrideRow(epId: string, oi: number, srcName: string, sev: string): string {
  const opts = ["info", "warn", "error", "critical"]
    .map((v) => `<option value="${v}"${v === sev ? " selected" : ""}>${v}</option>`)
    .join("");
  return `<div class="alerting-override-row" data-ep-id="${escAttr(epId)}" data-oi="${oi}">
     <input type="text" class="logs-input alerting-ep-override-source" data-ep-id="${escAttr(epId)}" data-oi="${oi}" placeholder="${esc(t("alertingEndpointSeverityOverrideSource"))}" value="${escAttr(srcName)}" style="min-width:200px" />
     <select class="logs-input alerting-ep-override-severity" data-ep-id="${escAttr(epId)}" data-oi="${oi}">${opts}</select>
     <button class="btn ghost alerting-ep-override-delete" data-ep-id="${escAttr(epId)}" data-oi="${oi}" type="button" title="${esc(t("alertingEndpointSeverityOverrideDelete"))}">×</button>
   </div>`;
}

function renderEndpointCard(ep: WebhookEndpointDto, i: number): string {
  const sourceBoxes = ALERTING_SOURCES.map((s) => {
    const checked = ep.sourceFilter.includes(s.id) ? " checked" : "";
    return `<label><input type="checkbox" class="alerting-ep-src" data-ep-id="${escAttr(ep.id)}" data-src="${escAttr(s.id)}"${checked}/> ${esc(t(s.i18n))}</label>`;
  }).join("");
  const headersHtml = ep.headers.map((h, hi) =>
    `<div class="alerting-header-row">
       <input type="text" class="logs-input alerting-ep-hkey" data-ep-id="${escAttr(ep.id)}" data-hi="${hi}" placeholder="${esc(t("alertingHeaderKey"))}" value="${escAttr(h.key)}" />
       <input type="text" class="logs-input alerting-ep-hval" data-ep-id="${escAttr(ep.id)}" data-hi="${hi}" placeholder="${esc(t("alertingHeaderValue"))}" value="${escAttr(h.value)}" />
       <button class="btn ghost alerting-ep-hdel" data-ep-id="${escAttr(ep.id)}" data-hi="${hi}" type="button">×</button>
     </div>`
  ).join("");
  return `
    <div class="alerting-endpoint-card ${ep.enabled ? "alerting-ep-on" : "alerting-ep-off"}" data-ep-id="${escAttr(ep.id)}" data-i="${i}">
      <div class="alerting-row">
        <label class="alerting-toggle"><input type="checkbox" class="alerting-ep-enabled" data-ep-id="${escAttr(ep.id)}"${ep.enabled ? " checked" : ""}/> ${esc(t("alertingEndpointEnabled"))}</label>
      </div>
      <div class="alerting-row">
        <label class="alerting-label">${esc(t("alertingEndpointName"))}</label>
        <input type="text" class="logs-input alerting-ep-name" data-ep-id="${escAttr(ep.id)}" value="${escAttr(ep.name)}" />
      </div>
      <div class="alerting-row">
        <label class="alerting-label">${esc(t("alertingUrl"))}</label>
        <input type="text" class="logs-input alerting-ep-url" data-ep-id="${escAttr(ep.id)}" placeholder="https://hooks.example.com/..." value="${escAttr(ep.url)}" />
      </div>
      <div class="alerting-row vertical">
        <label class="alerting-label">${esc(t("alertingHeaders"))}</label>
        <div class="alerting-ep-headers" data-ep-id="${escAttr(ep.id)}">${headersHtml || `<div class="setting-hint">${esc(t("alertingHeadersHint"))}</div>`}</div>
        <button class="btn ghost alerting-ep-add-header" data-ep-id="${escAttr(ep.id)}" type="button">${esc(t("alertingAddHeader"))}</button>
      </div>
      <div class="alerting-row vertical">
        <label class="alerting-label">${esc(t("alertingEndpointSecret"))}</label>
        <input type="password" class="logs-input alerting-ep-secret" data-ep-id="${escAttr(ep.id)}" placeholder="${esc(t("alertingEndpointSecretHint"))}" value="${escAttr(ep.secret)}" />
      </div>
      <div class="alerting-row vertical">
        <label class="alerting-label">${esc(t("alertingEndpointSourceFilter"))}</label>
        <div class="alerting-sources">${sourceBoxes}</div>
      </div>
      <div class="alerting-row vertical">
        <details class="alerting-overrides-section">
          <summary class="alerting-label">${esc(t("alertingEndpointSeverityOverrides"))}</summary>
          <span class="setting-hint">${esc(t("alertingEndpointSeverityOverridesHint"))}</span>
          <div class="alerting-overrides-list" data-ep-id="${escAttr(ep.id)}">
            ${(ep.severityOverrides ?? []).map((o, oi) => renderOverrideRow(ep.id, oi, o.source, o.severity)).join("")}
          </div>
          <button class="btn ghost alerting-ep-override-add" data-ep-id="${escAttr(ep.id)}" type="button">${esc(t("alertingEndpointSeverityOverrideAdd"))}</button>
        </details>
      </div>
      <div class="alerting-row">
        <label class="alerting-label">${esc(t("alertingSchemaVersion"))}</label>
        <select class="logs-input alerting-ep-schema-version" data-ep-id="${escAttr(ep.id)}">
          <option value="0"${(ep.schemaVersion ?? 0) === 0 ? " selected" : ""}>${esc(t("alertingSchemaLegacy"))}</option>
          <option value="1"${(ep.schemaVersion ?? 0) === 1 ? " selected" : ""}>${esc(t("alertingSchemaCanonical"))}</option>
        </select>
      </div>
      <div class="alerting-row vertical">
        <label class="alerting-label">${esc(t("alertingEndpointTemplatePreset"))}</label>
        <div class="alerting-template-preset-bar">
          <select class="logs-input alerting-ep-preset-select" data-ep-id="${escAttr(ep.id)}">
            <option value="">${esc(t("alertingEndpointTemplatePresetNone"))}</option>
            <optgroup label="${esc(t("alertingEndpointTemplatePresetBuiltins"))}">
              <option value="builtin:slack">${esc(t("alertingEndpointTemplatePresetSlack"))}</option>
              <option value="builtin:discord">${esc(t("alertingEndpointTemplatePresetDiscord"))}</option>
              <option value="builtin:msteams">${esc(t("alertingEndpointTemplatePresetMsTeams"))}</option>
              <option value="builtin:generic_json">${esc(t("alertingEndpointTemplatePresetGenericJson"))}</option>
              <option value="builtin:plain_text">${esc(t("alertingEndpointTemplatePresetPlainText"))}</option>
            </optgroup>
            ${userTemplatePresets.length > 0 ? `
              <optgroup label="${esc(t("alertingEndpointTemplatePresetCustom"))}">
                ${userTemplatePresets.map((p) => {
                  const label = !p.builtin && p.version > 1
                    ? `${esc(p.name)} <span class="preset-version-suffix">v${p.version}</span>`
                    : esc(p.name);
                  return `<option value="${escAttr(p.kind)}">${label}</option>`;
                }).join("")}
              </optgroup>` : ""}
          </select>
          <button class="btn ghost preset-icon-btn alerting-ep-preset-save" data-ep-id="${escAttr(ep.id)}" title="${esc(t("alertingEndpointTemplatePresetSaveTitle"))}" type="button">💾</button>
          <button class="btn ghost preset-icon-btn alerting-ep-preset-fork" data-ep-id="${escAttr(ep.id)}" title="${esc(t("alertingEndpointTemplatePresetForkTitle"))}" type="button">🍴</button>
          <button class="btn ghost preset-icon-btn alerting-ep-preset-delete" data-ep-id="${escAttr(ep.id)}" title="${esc(t("alertingEndpointTemplatePresetDeleteTitle"))}" type="button">🗑</button>
          <button class="btn ghost preset-icon-btn alerting-ep-preset-export" data-ep-id="${escAttr(ep.id)}" title="${esc(t("alertingEndpointTemplatePresetExportTitle"))}" type="button">⬇</button>
          <button class="btn ghost preset-icon-btn alerting-ep-preset-import" data-ep-id="${escAttr(ep.id)}" title="${esc(t("alertingEndpointTemplatePresetImportTitle"))}" type="button">⬆</button>
        </div>
        <span class="setting-hint">${esc(t("alertingEndpointTemplatePresetBarHint"))}</span>
        <label class="alerting-label">
          <input type="checkbox" class="alerting-ep-template-toggle" data-ep-id="${escAttr(ep.id)}"${ep.template ? " checked" : ""}/>
          ${esc(t("alertingEndpointTemplate"))}
        </label>
        <span class="setting-hint">${esc(t("alertingEndpointTemplateHint"))}</span>
        <textarea class="logs-input alerting-ep-template" data-ep-id="${escAttr(ep.id)}" rows="6" placeholder="${esc(t("alertingEndpointTemplatePlaceholder"))}"${ep.template ? "" : " disabled"}>${esc(ep.template ?? "")}</textarea>
        <span class="setting-hint">${esc(t("alertingEndpointTemplateContentType"))}</span>
        <label class="alerting-label alerting-template-sample-label">${esc(t("alertingEndpointTemplateSample"))}</label>
        <span class="setting-hint">${esc(t("alertingEndpointTemplateSampleHint"))}</span>
        <textarea class="logs-input alerting-ep-sample" data-ep-id="${escAttr(ep.id)}" rows="6" placeholder="${esc(t("alertingEndpointTemplateSamplePlaceholder"))}"${ep.template ? "" : " disabled"}>${esc(ep.templateSample ?? DEFAULT_TEMPLATE_SAMPLE)}</textarea>
        <div class="endpoint-template-preview-row">
          <span class="endpoint-content-type-badge ct-text" data-ep-id="${escAttr(ep.id)}">${esc(t("alertingEndpointTemplatePreviewLabel"))}</span>
          <pre class="endpoint-template-preview" data-ep-id="${escAttr(ep.id)}"></pre>
        </div>
        <div class="endpoint-template-diag" data-ep-id="${escAttr(ep.id)}"></div>
      </div>
      <div class="alerting-actions">
        <button class="btn ghost alerting-ep-test" data-ep-id="${escAttr(ep.id)}" type="button">${esc(t("alertingTest"))}</button>
        <button class="btn ghost alerting-ep-preview" data-ep-id="${escAttr(ep.id)}" type="button" title="${esc(t("alertingSeverityPreviewButton"))}">🔮</button>
        <button class="btn alerting-ep-save" data-ep-id="${escAttr(ep.id)}" type="button">${esc(t("alertingSave"))}</button>
        <button class="btn ghost alerting-ep-delete" data-ep-id="${escAttr(ep.id)}" type="button">${esc(t("alertingEndpointDelete"))}</button>
        <span class="setting-hint alerting-ep-msg" data-ep-id="${escAttr(ep.id)}"></span>
      </div>
    </div>`;
}

function bindEndpointCard(ep: WebhookEndpointDto): void {
  const root = document.querySelector(`.alerting-endpoint-card[data-ep-id="${CSS.escape(ep.id)}"]`);
  if (!root) return;
  // Add header
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-add-header").forEach((b) => {
    b.addEventListener("click", () => {
      const ep2 = alertingEndpoints.find((x) => x.id === ep.id);
      if (!ep2) return;
      ep2.headers.push({ key: "", value: "" });
      refreshAlertingEndpoints();
    });
  });
  // Delete header
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-hdel").forEach((b) => {
    b.addEventListener("click", () => {
      const hi = Number(b.dataset.hi);
      const ep2 = alertingEndpoints.find((x) => x.id === ep.id);
      if (!ep2) return;
      ep2.headers.splice(hi, 1);
      refreshAlertingEndpoints();
    });
  });
  // Phase 72: add severity override row -> push an info default straight into the local DTO, refresh renders
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-override-add").forEach((b) => {
    b.addEventListener("click", () => {
      const ep2 = alertingEndpoints.find((x) => x.id === ep.id);
      if (!ep2) return;
      if (!ep2.severityOverrides) ep2.severityOverrides = [];
      ep2.severityOverrides.push({ source: "", severity: "info" });
      refreshAlertingEndpoints();
    });
  });
  // Phase 72: delete severity override row
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-override-delete").forEach((b) => {
    b.addEventListener("click", () => {
      const oi = Number(b.dataset.oi);
      const ep2 = alertingEndpoints.find((x) => x.id === ep.id);
      if (!ep2 || !ep2.severityOverrides) return;
      ep2.severityOverrides.splice(oi, 1);
      refreshAlertingEndpoints();
    });
  });
  // save
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-save").forEach((b) => {
    b.addEventListener("click", () => void onSaveEndpoint(ep.id));
  });
  // delete
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-delete").forEach((b) => {
    b.addEventListener("click", () => void onDeleteEndpoint(ep.id));
  });
  // test
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-test").forEach((b) => {
    b.addEventListener("click", () => void onTestEndpoint(ep.id));
  });
  // Phase 74: single-endpoint preview (auto-presets endpointId into the section)
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preview").forEach((b) => {
    b.addEventListener("click", () => {
      const sel = document.querySelector<HTMLSelectElement>("#alerting-preview-endpoint");
      if (sel) sel.value = ep.id;
      const srcEl = document.querySelector<HTMLInputElement>("#alerting-preview-source");
      if (srcEl) srcEl.focus();
    });
  });
  // Phase 59: preset picker — selecting a builtin / user preset fills in template + sample
  root.querySelectorAll<HTMLSelectElement>(".alerting-ep-preset-select").forEach((sel) => {
    sel.addEventListener("change", () => {
      void onApplyPreset(ep.id, sel.value);
      // Restore the placeholder option after use, so selecting the same item next time still fires change
      sel.value = "";
    });
  });
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preset-save").forEach((b) => {
    b.addEventListener("click", () => void onSavePreset(ep.id));
  });
  // Phase 60: fork builtin -> prompt listing 5 builtins for the user to choose + a new name
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preset-fork").forEach((b) => {
    b.addEventListener("click", () => void onForkBuiltin());
  });
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preset-delete").forEach((b) => {
    b.addEventListener("click", () => void onDeleteUserPreset(ep.id));
  });
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preset-export").forEach((b) => {
    b.addEventListener("click", () => void onExportPresets());
  });
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preset-import").forEach((b) => {
    b.addEventListener("click", () => void onImportPresets());
  });
  // template toggle: enable/disable textareas + trigger the first preview
  root.querySelectorAll<HTMLInputElement>(".alerting-ep-template-toggle").forEach((t) => {
    t.addEventListener("change", () => {
      root.querySelectorAll<HTMLTextAreaElement>(".alerting-ep-template, .alerting-ep-sample").forEach((ta) => {
        ta.disabled = !t.checked;
      });
      void refreshTemplatePreview(ep.id, root);
    });
  });
  // live preview debounce
  const tplEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-template");
  const sampleEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-sample");
  if (tplEl && sampleEl) {
    let timer = 0;
    const debounced = () => {
      if (timer) window.clearTimeout(timer);
      timer = window.setTimeout(() => void refreshTemplatePreview(ep.id, root), 250);
    };
    tplEl.addEventListener("input", debounced);
    sampleEl.addEventListener("input", debounced);
    // Run once initially (so the user immediately sees the effect of the default sample)
    void refreshTemplatePreview(ep.id, root);
  }
}

export async function refreshTemplatePreview(epId: string, root: Element): Promise<void> {
  const tplEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-template");
  const sampleEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-sample");
  const previewEl = root.querySelector<HTMLPreElement>(".endpoint-template-preview");
  const diagEl = root.querySelector<HTMLDivElement>(".endpoint-template-diag");
  const badgeEl = root.querySelector<HTMLSpanElement>(".endpoint-content-type-badge");
  if (!tplEl || !sampleEl || !previewEl || !diagEl) return;
  if (tplEl.disabled) {
    previewEl.textContent = t("alertingEndpointTemplatePreviewDisabled");
    diagEl.innerHTML = "";
    if (badgeEl) badgeEl.className = "endpoint-content-type-badge ct-text";
    return;
  }
  const tpl = tplEl.value;
  let sample: unknown = {};
  try {
    sample = sampleEl.value.trim() ? JSON.parse(sampleEl.value) : {};
    sampleEl.classList.remove("invalid");
  } catch (e) {
    sampleEl.classList.add("invalid");
    previewEl.textContent = `✗ ${t("alertingEndpointTemplateSampleInvalid")}: ${String(e)}`;
    diagEl.innerHTML = "";
    return;
  }
  try {
    const result = await invoke<TemplatePreviewResult>("preview_alerting_template", {
      template: tpl,
      sample,
    });
    previewEl.textContent = result.body || t("alertingEndpointTemplatePreviewEmpty");
    if (badgeEl) {
      badgeEl.textContent = result.contentType;
      badgeEl.className = `endpoint-content-type-badge ${result.contentType.startsWith("application/json") ? "ct-json" : "ct-text"}`;
    }
    if (result.diagnostics.length === 0) {
      diagEl.innerHTML = `<span class="setting-hint">✓ ${esc(t("alertingEndpointTemplateLintClean"))}</span>`;
    } else {
      diagEl.innerHTML = result.diagnostics.map((d) => `
        <div class="endpoint-template-diag-item diag-${escAttr(d.severity)}">
          <span class="endpoint-template-diag-loc">[line ${d.line}, col ${d.column}]</span>
          <span class="endpoint-template-diag-code">${esc(d.code)}</span>
          <span class="endpoint-template-diag-msg">${esc(d.message)}</span>
        </div>`).join("");
    }
  } catch (err) {
    previewEl.textContent = `✗ ${String(err)}`;
  }
}

async function onSaveEndpoint(id: string): Promise<void> {
  const root = document.querySelector(`.alerting-endpoint-card[data-ep-id="${CSS.escape(id)}"]`);
  const msg = document.querySelector(`.alerting-ep-msg[data-ep-id="${CSS.escape(id)}"]`);
  const ep = alertingEndpoints.find((x) => x.id === id);
  if (!root || !ep) return;
  // Sync current form values
  const nameEl = root.querySelector<HTMLInputElement>(".alerting-ep-name");
  const urlEl = root.querySelector<HTMLInputElement>(".alerting-ep-url");
  const secretEl = root.querySelector<HTMLInputElement>(".alerting-ep-secret");
  const enabledEl = root.querySelector<HTMLInputElement>(".alerting-ep-enabled");
  const srcEls = root.querySelectorAll<HTMLInputElement>(".alerting-ep-src");
  const hkeyEls = root.querySelectorAll<HTMLInputElement>(".alerting-ep-hkey");
  const hvalEls = root.querySelectorAll<HTMLInputElement>(".alerting-ep-hval");
  const svEl = root.querySelector<HTMLSelectElement>(".alerting-ep-schema-version");
  const tplToggleEl = root.querySelector<HTMLInputElement>(".alerting-ep-template-toggle");
  const tplEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-template");
  const sampleEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-sample");
  // Collect headers (in DOM order)
  const headers: WebhookHeader[] = [];
  hkeyEls.forEach((k, hi) => {
    const v = hvalEls[hi]?.value ?? "";
    headers.push({ key: k.value, value: v });
  });
  const sourceFilter: string[] = [];
  srcEls.forEach((el) => { if (el.checked) sourceFilter.push(el.dataset.src ?? ""); });
  // Phase 72: collect severity overrides
  const oSrcEls = root.querySelectorAll<HTMLInputElement>(".alerting-ep-override-source");
  const oSevEls = root.querySelectorAll<HTMLSelectElement>(".alerting-ep-override-severity");
  const severityOverrides: Array<{ source: string; severity: string }> = [];
  oSrcEls.forEach((srcEl, oi) => {
    const srcVal = (srcEl.value ?? "").trim();
    if (!srcVal) return; // skip empty source
    const sevVal = oSevEls[oi]?.value ?? "info";
    severityOverrides.push({ source: srcVal, severity: sevVal });
  });
  const payload: WebhookEndpointDto = {
    id: ep.id,
    name: nameEl?.value ?? "",
    url: urlEl?.value ?? "",
    enabled: enabledEl?.checked ?? false,
    headers,
    secret: secretEl?.value ?? "",
    sourceFilter,
    createdAt: ep.createdAt,
    schemaVersion: Number(svEl?.value ?? 0),
    template: (tplToggleEl?.checked && tplEl?.value.trim()) ? tplEl.value : undefined,
    templateSample: (tplToggleEl?.checked && sampleEl?.value.trim()) ? sampleEl.value : undefined,
    severityOverrides,
  };
  try {
    const saved = await invoke<WebhookEndpointDto>("save_alerting_endpoint", { ep: payload });
    // Update the local cache (the backend generates a new id)
    const idx = alertingEndpoints.findIndex((x) => x.id === ep.id);
    if (idx >= 0) alertingEndpoints[idx] = saved;
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
    refreshAlertingEndpoints();
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function onDeleteEndpoint(id: string): Promise<void> {
  if (!window.confirm(t("alertingEndpointDeleteConfirm"))) return;
  const msg = document.querySelector(`.alerting-ep-msg[data-ep-id="${CSS.escape(id)}"]`);
  try {
    const cleared = await invoke<[boolean, number]>("delete_alerting_endpoint", { id });
    alertingEndpoints = alertingEndpoints.filter((x) => x.id !== id);
    refreshAlertingEndpoints();
    // Deleting an endpoint may cascade-clear dead letters; also refresh the failed list
    void refreshAlertingFailed();
    if (msg) msg.textContent = `${t("alertingClearExhaustedDone")} (${cleared[1]})`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function onTestEndpoint(id: string): Promise<void> {
  const msg = document.querySelector(`.alerting-ep-msg[data-ep-id="${CSS.escape(id)}"]`);
  try {
    const status = await invoke<number>("test_alerting_endpoint", { id });
    if (msg) msg.textContent = `✓ ${t("alertingTestSuccess")} (${status})`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${t("alertingTestFailed")}: ${String(err)}`;
  }
}

export async function onAddEndpoint(): Promise<void> {
  // Let the backend generate an id first; once the frontend has it, enter edit mode immediately
  const name = window.prompt(t("alertingEndpointName")) ?? "";
  if (!name.trim()) return;
  const payload: WebhookEndpointDto = {
    id: "",
    name: name.trim(),
    url: "",
    enabled: true,
    headers: [],
    secret: "",
    sourceFilter: [],
    createdAt: 0,
    schemaVersion: 0,
    template: undefined,
    templateSample: undefined,
    severityOverrides: [],
  };
  try {
    const saved = await invoke<WebhookEndpointDto>("save_alerting_endpoint", { ep: payload });
    alertingEndpoints.push(saved);
    refreshAlertingEndpoints();
  } catch (err) {
    alert(`${t("alertingSaveFailed")}: ${String(err)}`);
  }
}

// Phase 74 — endpoint override dry-run preview
export function populateAlertingPreviewEndpointSelect(): void {
  const sel = document.querySelector<HTMLSelectElement>("#alerting-preview-endpoint");
  if (!sel) return;
  const current = sel.value;
  const enabled = alertingEndpoints.filter((e) => e.enabled);
  sel.innerHTML =
    `<option value="">${esc(t("alertingSeverityPreviewAllEndpoints"))}</option>` +
    enabled
      .map(
        (e) =>
          `<option value="${escAttr(e.id)}"${e.id === current ? " selected" : ""}>${esc(e.name)}</option>`,
      )
      .join("");
}

export async function onPreviewPredict(): Promise<void> {
  const srcEl = document.querySelector<HTMLInputElement>("#alerting-preview-source");
  const epSel = document.querySelector<HTMLSelectElement>("#alerting-preview-endpoint");
  const result = document.querySelector<HTMLDivElement>("#alerting-preview-result");
  const msg = document.querySelector<HTMLSpanElement>("#alerting-preview-msg");
  if (!srcEl || !result) return;
  const source = srcEl.value.trim();
  if (!source) {
    if (msg) msg.textContent = `✗ ${t("alertingSeverityPreviewSourceRequired")}`;
    return;
  }
  const endpointId = epSel?.value || null;
  try {
    if (msg) msg.textContent = t("alertingSeverityPreviewRunning");
    const preview = await invoke<EndpointSeverityPreviewDto>(
      "preview_alerting_endpoint_severity",
      { source, endpointId },
    );
    if (msg) msg.textContent = `✓ ${t("alertingSeverityPreviewResult")} (${preview.endpoints.length})`;
    if (preview.endpoints.length === 0) {
      result.innerHTML = `<div class="setting-hint">${esc(t("alertingSeverityPreviewNoEndpoints"))}</div>`;
      return;
    }
    result.innerHTML = preview.endpoints.map((row) => renderPreviewRow(row)).join("");
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

export function renderPreviewRow(row: EndpointPreviewRowDto): string {
  const hitHtml = row.overrideHit
    ? `<code>${esc(row.overrideHit.pattern)}</code> → <span class="severity-badge sev-${escAttr(row.overrideHit.severity)}">${esc(row.overrideHit.severity)}</span> <span class="setting-hint">(${t("alertingSeverityPreviewIndex")} ${row.overrideHit.index})</span>`
    : `<em>${esc(t("alertingSeverityPreviewNoOverride"))}</em>`;
  return `<div class="alerting-preview-row">
    <div class="alerting-preview-ep"><strong>${esc(row.endpointName)}</strong></div>
    <div class="alerting-preview-prop">${esc(t("alertingSeverityPreviewPropagation"))}: <span class="severity-badge sev-${escAttr(row.propagationSeverity)}">${esc(row.propagationSeverity)}</span></div>
    <div class="alerting-preview-override">${esc(t("alertingSeverityPreviewOverride"))}: ${hitHtml}</div>
    <div class="alerting-preview-final">${esc(t("alertingSeverityPreviewFinal"))}: <span class="severity-badge sev-${escAttr(row.finalEnvelopeSeverity)}">${esc(row.finalEnvelopeSeverity)}</span></div>
  </div>`;
}
