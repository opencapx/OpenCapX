import { invoke } from "@tauri-apps/api/core";
import { t, type I18nKey } from "../../i18n";
import { esc, escAttr } from "../shared";
import { refreshAlertingRoutes } from "./routes";
import { setAlertingMsg } from "./shared";

// ─── Phase 67: Alert Recipient persistent CRUD ──────────────────────────────
interface RecipientDto {
  id: string;
  name: string;
  kind: string;
  config: Record<string, unknown>;
  enabled: boolean;
  createdAt: number;
}

// ─── Phase 66: Alert Recipient (multi-channel fanout: webhook / log:stderr / log:file / email:smtp) ─
interface RecipientKindPreset {
  kind: string;
  labelKey: I18nKey;
  specTemplate: string;
  hintKey: I18nKey;
}

const RECIPIENT_KIND_PRESETS: RecipientKindPreset[] = [
  { kind: "log:stderr", labelKey: "alertingRecipientKindLogStderr", specTemplate: "log:stderr", hintKey: "alertingRecipientKindLogStderrHint" },
  { kind: "log:file", labelKey: "alertingRecipientKindLogFile", specTemplate: "log:file:/tmp/opencapx-alerts.log", hintKey: "alertingRecipientKindLogFileHint" },
  { kind: "email:smtp", labelKey: "alertingRecipientKindEmailSmtp", specTemplate: "email:smtp:smtp.example.com:587:alerts@example.com:oncall@example.com", hintKey: "alertingRecipientKindEmailSmtpHint" },
  { kind: "webhook", labelKey: "alertingRecipientKindWebhookRef", specTemplate: "webhook:{endpoint_id}", hintKey: "alertingRecipientWebhookRefHint" },
];

export async function onAddRecipient(): Promise<void> {
  // Phase 67 — go through the persistence flow: pick kind -> open a form collecting fields (name + each kind's config) -> save_alerting_recipient.
  // No longer make the user hand-write the spec string (that was the Phase 66 stopgap).
  const menu = RECIPIENT_KIND_PRESETS
    .map((p, i) => `${i + 1}. ${t(p.labelKey)}`)
    .join("\n");
  const choiceRaw = window.prompt(`${t("alertingRecipientAdd")}\n${menu}`);
  if (!choiceRaw?.trim()) return;
  const num = Number(choiceRaw.trim());
  let presetIdx: number;
  if (Number.isInteger(num) && num >= 1 && num <= RECIPIENT_KIND_PRESETS.length) {
    presetIdx = num - 1;
  } else {
    const matched = RECIPIENT_KIND_PRESETS.findIndex((p) => p.kind === choiceRaw.trim());
    if (matched < 0) {
      setAlertingMsg(`✗ ${t("alertingRecipientUnknownKind")}: ${choiceRaw}`);
      return;
    }
    presetIdx = matched;
  }
  const preset = RECIPIENT_KIND_PRESETS[presetIdx];
  const name = window.prompt(t("alertingRecipientNamePrompt"), `my-${preset.kind.replace(":", "-")}`);
  if (!name?.trim()) return;
  // Collect config fields by kind
  let config: Record<string, unknown> = {};
  try {
    if (preset.kind === "log:file") {
      const path = window.prompt(t("alertingRecipientPathPrompt"), "/tmp/opencapx-alerts.log");
      if (!path?.trim()) return;
      config = { path: path.trim() };
    } else if (preset.kind === "email:smtp") {
      const relay = window.prompt(t("alertingRecipientSmtpRelayPrompt"), "smtp.example.com");
      if (!relay?.trim()) return;
      const portStr = window.prompt(t("alertingRecipientSmtpPortPrompt"), "587");
      const port = Number(portStr || "587");
      const from = window.prompt(t("alertingRecipientSmtpFromPrompt"), "alerts@example.com");
      if (!from?.trim()) return;
      const to = window.prompt(t("alertingRecipientSmtpToPrompt"), "oncall@example.com");
      if (!to?.trim()) return;
      config = { relay: relay.trim(), port, from: from.trim(), to: to.trim() };
    } else if (preset.kind === "webhook") {
      let eps: Array<{ id: string; name: string }> = [];
      try {
        eps = await invoke<Array<{ id: string; name: string }>>("list_alerting_endpoints");
      } catch {}
      if (eps.length === 0) {
        setAlertingMsg(`✗ ${t("alertingRouteNeedEndpoint")}`);
        return;
      }
      const epIds = window.prompt(
        t("alertingRecipientWebhookEndpointPrompt"),
        eps.map((e) => `${e.id}(${e.name})`).join(", "),
      );
      if (!epIds?.trim()) return;
      const idPart = epIds.trim().split("(")[0].trim();
      if (!idPart) return;
      config = { endpoint_id: idPart };
    }
    const rec = await invoke<RecipientDto>("save_alerting_recipient", {
      rec: {
        id: "",
        name: name.trim(),
        kind: preset.kind,
        config,
        enabled: true,
        createdAt: 0,
      },
    });
    // Test once immediately
    await invoke<string>("test_alerting_recipient_by_id", { id: rec.id });
    setAlertingMsg(`✓ ${t("alertingRecipientAdded")}: ${rec.name}`);
    await refreshRecipients();
    await refreshAlertingRoutes();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

export async function refreshRecipients(): Promise<void> {
  const list = document.getElementById("alerting-recipients-list");
  if (!list) return;
  let items: RecipientDto[] = [];
  try {
    items = await invoke<RecipientDto[]>("list_alerting_recipients");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">✗ ${String(err)}</div>`;
    return;
  }
  if (items.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingRecipientsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = items
    .map((r) => {
      const cfgStr = JSON.stringify(r.config);
      const status = r.enabled
        ? `<span class="alerting-badge alerting-badge-on">${esc(t("alertingRecipientCardEnabled"))}</span>`
        : `<span class="alerting-badge">${esc(t("alertingRecipientCardDisabled"))}</span>`;
      return `<div class="recipient-card${r.enabled ? " recipient-card-active" : ""}" data-id="${escAttr(r.id)}">
        <div class="recipient-card-head">
          <b>${esc(r.name)}</b>
          <code class="recipient-kind">${esc(r.kind)}</code>
          ${status}
        </div>
        <div class="recipient-card-config"><span class="setting-hint">config:</span> <code>${esc(cfgStr)}</code></div>
        <div class="recipient-card-foot">
          <button class="btn ghost rec-test" data-id="${escAttr(r.id)}" type="button">${esc(t("alertingRecipientTest"))}</button>
          <button class="btn ghost rec-toggle" data-id="${escAttr(r.id)}" data-enabled="${r.enabled ? "1" : "0"}" type="button">${r.enabled ? esc(t("alertingRecipientCardDisable")) : esc(t("alertingRecipientCardEnable"))}</button>
          <button class="btn ghost rec-delete" data-id="${escAttr(r.id)}" type="button">${esc(t("alertingRecipientCardDelete"))}</button>
        </div>
      </div>`;
    })
    .join("");
  list.querySelectorAll<HTMLButtonElement>(".rec-test").forEach((b) => {
    b.addEventListener("click", () => void onTestRecipient(b.dataset.id || ""));
  });
  list.querySelectorAll<HTMLButtonElement>(".rec-toggle").forEach((b) => {
    b.addEventListener("click", () => void onToggleRecipient(b.dataset.id || "", b.dataset.enabled === "1"));
  });
  list.querySelectorAll<HTMLButtonElement>(".rec-delete").forEach((b) => {
    b.addEventListener("click", () => void onDeleteRecipient(b.dataset.id || ""));
  });
}

async function onTestRecipient(id: string): Promise<void> {
  if (!id) return;
  try {
    const result = await invoke<string>("test_alerting_recipient_by_id", { id });
    setAlertingMsg(`✓ ${t("alertingRecipientTested")}\n${result}`);
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

async function onToggleRecipient(id: string, currentEnabled: boolean): Promise<void> {
  if (!id) return;
  try {
    const items = await invoke<RecipientDto[]>("list_alerting_recipients");
    const r = items.find((x) => x.id === id);
    if (!r) return;
    await invoke<RecipientDto>("save_alerting_recipient", {
      rec: { ...r, enabled: !currentEnabled },
    });
    await refreshRecipients();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

async function onDeleteRecipient(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingRecipientCardDeleteConfirm"))) return;
  try {
    const [deleted, routesCleared] = await invoke<[boolean, number]>(
      "delete_alerting_recipient",
      { id },
    );
    if (deleted) {
      const suffix = routesCleared > 0
        ? ` (${routesCleared} route ref cleared)`
        : "";
      setAlertingMsg(`✓ ${t("alertingRecipientDeleted")}${suffix}`);
    }
    await refreshRecipients();
    await refreshAlertingRoutes();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}
