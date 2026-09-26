import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { refreshAlertingEndpoints, refreshTemplatePreview } from "./endpoints";
import { refreshAlertingRoutes } from "./routes";
import { DEFAULT_TEMPLATE_SAMPLE, FRONTEND_BUILTIN_PRESETS, setAlertingMsg } from "./shared";
import { refreshAlertingAcks, refreshAlertingSilences } from "./silences";

// Phase 58: live preview DTOs
interface TemplateDiagnostic {
  severity: "error" | "warning";
  code: string;
  message: string;
  offset: number;
  line: number;
  column: number;
}

export interface TemplatePreviewResult {
  body: string;
  contentType: string;
  diagnostics: TemplateDiagnostic[];
}

// Phase 59: template preset DTO — returned by the backend list_alerting_template_presets / get_alerting_template_preset
interface TemplatePreset {
  id: string;
  name: string;
  description: string;
  kind: string;       // 'builtin:<slug>' or 'user:<uuid>'
  template: string;
  sample: string;
  builtin: boolean;
  createdAt: number;
  version: number;    // Phase 60: built-in fixed at 1; auto-bumped on user save
  changelog: string;  // Phase 60: optional user-provided changelog
}

export let userTemplatePresets: TemplatePreset[] = []; // Phase 59: user custom preset cache

export async function refreshTemplatePresets(): Promise<void> {
  try {
    const all = await invoke<TemplatePreset[]>("list_alerting_template_presets");
    userTemplatePresets = all.filter((p) => !p.builtin);
  } catch (e) {
    console.error("list_alerting_template_presets failed:", e);
    userTemplatePresets = [];
  }
}

// Phase 59: apply a preset (builtin or user) to the current endpoint — fill in template + sample + trigger preview
export async function onApplyPreset(epId: string, kind: string): Promise<void> {
  if (!kind) return;
  const root = document.querySelector(`.alerting-endpoint-card[data-ep-id="${CSS.escape(epId)}"]`);
  if (!root) return;
  let p: TemplatePreset | null = null;
  try {
    p = await invoke<TemplatePreset | null>("get_alerting_template_preset", { kind });
  } catch (e) {
    console.error("get_alerting_template_preset failed:", e);
    return;
  }
  if (!p) return;
  // Check the template toggle (if unchecked) and enable the textareas
  const tplToggleEl = root.querySelector<HTMLInputElement>(".alerting-ep-template-toggle");
  const tplEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-template");
  const sampleEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-sample");
  if (!tplEl || !sampleEl) return;
  if (tplToggleEl && !tplToggleEl.checked) {
    tplToggleEl.checked = true;
    tplToggleEl.dispatchEvent(new Event("change"));
  }
  tplEl.value = p.template;
  sampleEl.value = p.sample;
  void refreshTemplatePreview(epId, root);
}

// Phase 59: save the current endpoint's template + sample as a user preset. Prompt for name + description.
export async function onSavePreset(epId: string): Promise<void> {
  const root = document.querySelector(`.alerting-endpoint-card[data-ep-id="${CSS.escape(epId)}"]`);
  if (!root) return;
  const tplEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-template");
  const sampleEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-sample");
  if (!tplEl) return;
  if (!tplEl.value.trim()) {
    setAlertingMsg(t("alertingEndpointTemplatePresetSaveEmpty"));
    return;
  }
  const name = window.prompt(t("alertingEndpointTemplatePresetSavePrompt"));
  if (!name?.trim()) return;
  const description = window.prompt(t("alertingEndpointTemplatePresetSaveDescPrompt")) ?? "";
  try {
    const saved = await invoke<TemplatePreset>("save_alerting_template_preset", {
      preset: {
        id: "",
        name: name.trim(),
        description: description.trim(),
        kind: "",
        template: tplEl.value,
        sample: sampleEl?.value ?? DEFAULT_TEMPLATE_SAMPLE,
        builtin: false,
        createdAt: 0,
      },
    });
    await refreshTemplatePresets();
    setAlertingMsg(`✓ ${t("alertingEndpointTemplatePresetSaved")}: ${saved.name}`);
    // Re-render the endpoint cards; the preset dropdown will bring in the new option
    await refreshAlertingEndpoints();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 59: delete a user preset — a simple select lists existing user presets; pick one, confirm, then call delete
export async function onDeleteUserPreset(_epId: string): Promise<void> {
  await refreshTemplatePresets();
  if (userTemplatePresets.length === 0) {
    setAlertingMsg(t("alertingEndpointTemplatePresetDeleteNone"));
    return;
  }
  const list = userTemplatePresets.map((p, i) => `${i + 1}. ${p.name}`).join("\n");
  const ans = window.prompt(`${t("alertingEndpointTemplatePresetDeletePrompt")}\n${list}`);
  if (!ans) return;
  const idx = Number(ans.trim()) - 1;
  if (!Number.isFinite(idx) || idx < 0 || idx >= userTemplatePresets.length) {
    setAlertingMsg(t("alertingEndpointTemplatePresetDeleteInvalid"));
    return;
  }
  const target = userTemplatePresets[idx];
  try {
    await invoke<boolean>("delete_alerting_template_preset", { id: target.id });
    await refreshTemplatePresets();
    setAlertingMsg(`✓ ${t("alertingEndpointTemplatePresetDeleted")}: ${target.name}`);
    await refreshAlertingEndpoints();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 60: fork builtin preset — prompt the user to pick a builtin kind + give a new name -> the backend copies it into a user preset
export async function onForkBuiltin(): Promise<void> {
  const list = FRONTEND_BUILTIN_PRESETS
    .map((b, i) => `${i + 1}. ${b.name}`)
    .join("\n");
  const raw = (window.prompt(`${t("alertingEndpointTemplatePresetForkPrompt")}\n${list}`) ?? "").trim();
  if (!raw) return;
  // Accepts: an index '1'-'5' or the kind directly, e.g. 'builtin:slack'
  let resolvedKind = "";
  const num = Number(raw);
  if (Number.isInteger(num) && num >= 1 && num <= FRONTEND_BUILTIN_PRESETS.length) {
    resolvedKind = FRONTEND_BUILTIN_PRESETS[num - 1].kind;
  } else if (raw.startsWith("builtin:")) {
    resolvedKind = raw;
  }
  if (!resolvedKind) {
    setAlertingMsg(t("alertingEndpointTemplatePresetForkInvalid"));
    return;
  }
  const name = window.prompt(t("alertingEndpointTemplatePresetForkNamePrompt"));
  if (!name?.trim()) return;
  try {
    const forked = await invoke<TemplatePreset>("fork_alerting_template_preset", {
      kind: resolvedKind,
      name: name.trim(),
    });
    await refreshTemplatePresets();
    setAlertingMsg(`✓ ${t("alertingEndpointTemplatePresetForked")}: ${forked.name}`);
    await refreshAlertingEndpoints();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 59: export all template presets (builtin + user) to a YAML / JSON file
export async function onExportPresets(): Promise<void> {
  try {
    const all = await invoke<TemplatePreset[]>("list_alerting_template_presets");
    const yaml = await invoke<string>("export_alerting_presets", { presets: all });
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      defaultPath: "opencapx-presets.yaml",
      filters: [
        { name: "YAML/JSON", extensions: ["yaml", "yml", "json"] },
      ],
    });
    if (!path) return;
    await invoke("write_text_file", { path, content: yaml });
    setAlertingMsg(`✓ ${t("alertingEndpointTemplatePresetExported")}: ${path}`);
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 59: import presets from a YAML / JSON file
export async function onImportPresets(): Promise<void> {
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({
      multiple: false,
      filters: [
        { name: "YAML/JSON", extensions: ["yaml", "yml", "json"] },
      ],
    });
    if (!path || Array.isArray(path)) return;
    const content = await invoke<string>("read_text_file", { path });
    const count = await invoke<number>("import_alerting_presets", { yaml: content });
    await refreshTemplatePresets();
    // Phase 61: the backend runs the migration chain automatically; logs go to stderr (visible in the tauri console)
    // Here only the success count is shown; migration details go through console.info for devs
    console.info(`[alerting::import] imported ${count} presets; migration log on stderr`);
    setAlertingMsg(`✓ ${t("alertingEndpointTemplatePresetImported")}: ${count}`);
    await refreshAlertingEndpoints();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 62: export the entire alerting config (endpoints + routes + presets + silence + ack) to one YAML file.
// Phase 64: passphrase optional — prompt; empty = signed plaintext, non-empty = AES-GCM encrypted envelope.
export async function onExportAlertingBundle(): Promise<void> {
  const msgEl = document.getElementById("alerting-bundle-msg");
  if (msgEl) msgEl.textContent = t("alertingBundleExporting");
  try {
    const pp = (window.prompt(t("alertingBundlePassphrasePrompt")) ?? "").trim();
    if (pp === "_cancel_") return; // sentinel: user closed the prompt directly
    const passphrase = pp === "" ? null : pp;
    const out = await invoke<string>("export_alerting_bundle", { passphrase });
    const encrypted = passphrase !== null;
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      defaultPath: encrypted ? "opencapx-alerting-bundle.enc.json" : "opencapx-alerting-bundle.yaml",
      filters: encrypted
        ? [{ name: "Encrypted JSON", extensions: ["json"] }]
        : [{ name: "YAML", extensions: ["yaml", "yml"] }],
    });
    if (!path) {
      if (msgEl) msgEl.textContent = "";
      return;
    }
    await invoke("write_text_file", { path, content: out });
    const mode = encrypted ? t("alertingBundleEncrypted") : t("alertingBundleSigned");
    if (msgEl) msgEl.textContent = `${t("alertingBundleExported")} (${mode})`;
    setAlertingMsg(`✓ ${t("alertingBundleExported")} (${mode}): ${path}`);
  } catch (e) {
    if (msgEl) msgEl.textContent = String(e);
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 62: import the entire alerting config from a YAML file; upsert section by section, forcing preset builtin = false.
// Phase 64: auto-detect plaintext signed vs encrypted envelope; if the file is a JSON envelope, prompt for a passphrase.
export async function onImportAlertingBundle(): Promise<void> {
  const msgEl = document.getElementById("alerting-bundle-msg");
  if (msgEl) msgEl.textContent = t("alertingBundleImporting");
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({
      multiple: false,
      filters: [
        { name: "Bundle (YAML or encrypted JSON)", extensions: ["yaml", "yml", "json"] },
      ],
    });
    if (!path || Array.isArray(path)) {
      if (msgEl) msgEl.textContent = "";
      return;
    }
    const content = await invoke<string>("read_text_file", { path });
    // Auto-detect: after trim, starting with `{` -> encrypted envelope -> passphrase required
    const isEncrypted = content.trimStart().startsWith("{");
    let passphrase: string | null = null;
    if (isEncrypted) {
      const pp = (window.prompt(t("alertingBundleEncryptedRequiresPassphrase")) ?? "").trim();
      if (pp === "") {
        if (msgEl) msgEl.textContent = t("alertingBundleCancelled");
        return;
      }
      passphrase = pp;
    }
    const summary = await invoke<BundleImportSummary>("import_alerting_bundle", {
      content,
      passphrase,
    });
    if (msgEl) msgEl.textContent = `${t("alertingBundleImported")}: ${summary.total}`;
    setAlertingMsg(`✓ ${t("alertingBundleImported")}: ${summary.total}\n  endpoints: ${summary.endpoints}\n  routes: ${summary.routes}\n  presets: ${summary.presets}\n  silences: ${summary.silences}\n  acks: ${summary.acks}`);
    // Refresh all related views
    await refreshTemplatePresets();
    await refreshAlertingEndpoints();
    await refreshAlertingRoutes();
    await refreshAlertingSilences();
    await refreshAlertingAcks();
  } catch (e) {
    if (msgEl) msgEl.textContent = String(e);
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 65: rotate the bundle signing key (generate a new 32 bytes, write to the OS keychain; bundles exported with the old key fail verification)
export async function onRotateBundleSecret(): Promise<void> {
  const msgEl = document.getElementById("alerting-bundle-msg");
  if (!window.confirm(t("alertingBundleRotateConfirm"))) return;
  try {
    await invoke<void>("rotate_alerting_bundle_secret");
    if (msgEl) msgEl.textContent = t("alertingBundleRotated");
    setAlertingMsg(`✓ ${t("alertingBundleRotated")}`);
  } catch (e) {
    if (msgEl) msgEl.textContent = String(e);
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 62: per-section counts returned by bundle import.
interface BundleImportSummary {
  endpoints: number;
  routes: number;
  presets: number;
  silences: number;
  acks: number;
  total: number;
}
