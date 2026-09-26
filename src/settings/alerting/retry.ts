import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc, escAttr } from "../shared";
import { alertingEndpoints } from "./endpoints";

// ─── Phase 48: Retry queue + failed deliveries panel ─────────────────────────

interface RetryConfigDto {
  maxAttempts: number;
  initialBackoffSecs: number;
  maxBackoffSecs: number;
  retentionDays: number;
}

interface FailedDeliveryDto {
  id: string;
  source: string;
  url: string;
  payload: string;
  firstAttemptTs: number;
  lastAttemptTs: number;
  attempts: number;
  maxAttempts: number;
  lastError: string;
  nextRetryTs: number;
  state: string;
  endpointId?: string;
}

let retryConfig: RetryConfigDto | null = null;

let failedState: string = "";

export async function refreshAlertingRetryConfig(): Promise<void> {
  const form = document.getElementById("alerting-retry-form");
  if (!form) return;
  try {
    retryConfig = await invoke<RetryConfigDto>("get_alerting_retry_config");
  } catch (err) {
    form.innerHTML = `<div class="setting-hint">${esc(String(err))}</div>`;
    return;
  }
  const c = retryConfig;
  form.innerHTML = `
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingRetryMaxAttempts"))}</label>
      <input type="number" class="logs-input alerting-interval" id="retry-max-attempts" min="1" max="100" value="${c.maxAttempts}" />
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingRetryInitialBackoff"))}</label>
      <input type="number" class="logs-input alerting-interval" id="retry-initial" min="1" max="86400" value="${c.initialBackoffSecs}" />
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingRetryMaxBackoff"))}</label>
      <input type="number" class="logs-input alerting-interval" id="retry-max" min="1" max="604800" value="${c.maxBackoffSecs}" />
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingRetryRetention"))}</label>
      <input type="number" class="logs-input alerting-interval" id="retry-retention" min="1" max="365" value="${c.retentionDays}" />
    </div>`;
}

export async function saveAlertingRetryConfig(): Promise<void> {
  const msg = document.getElementById("alerting-retry-msg");
  const maxA = Number((document.getElementById("retry-max-attempts") as HTMLInputElement | null)?.value ?? 5);
  const init = Number((document.getElementById("retry-initial") as HTMLInputElement | null)?.value ?? 30);
  const maxB = Number((document.getElementById("retry-max") as HTMLInputElement | null)?.value ?? 600);
  const ret = Number((document.getElementById("retry-retention") as HTMLInputElement | null)?.value ?? 7);
  try {
    await invoke("set_alerting_retry_config", {
      cfg: {
        maxAttempts: Math.max(1, Math.min(100, maxA)),
        initialBackoffSecs: Math.max(1, Math.min(86400, init)),
        maxBackoffSecs: Math.max(1, Math.min(604800, maxB)),
        retentionDays: Math.max(1, Math.min(365, ret)),
      },
    });
    if (msg) msg.textContent = t("alertingSaved");
  } catch (err) {
    if (msg) msg.textContent = `${t("alertingSaveFailed")}: ${String(err)}`;
  }
}

export async function refreshAlertingFailed(): Promise<void> {
  const sel = document.getElementById("alerting-failed-state") as HTMLSelectElement | null;
  failedState = sel?.value ?? "";
  const list = document.getElementById("alerting-failed-list");
  if (!list) return;
  try {
    const rows = await invoke<FailedDeliveryDto[]>("list_alerting_failed", {
      state: failedState || null,
      limit: 200,
    });
    renderFailedDeliveries(rows);
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">${esc(String(err))}</div>`;
  }
}

function renderFailedDeliveries(rows: FailedDeliveryDto[]): void {
  const list = document.getElementById("alerting-failed-list");
  if (!list) return;
  if (rows.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingFailedEmpty"))}</div>`;
    return;
  }
  list.innerHTML = `<div class="alerting-failed-table">${rows.map(renderFailedRow).join("")}</div>`;
  list.querySelectorAll<HTMLButtonElement>(".alerting-failed-retry").forEach((btn) => {
    btn.addEventListener("click", () => void onRetryFailed(btn.dataset.id ?? ""));
  });
  list.querySelectorAll<HTMLButtonElement>(".alerting-failed-del").forEach((btn) => {
    btn.addEventListener("click", () => void onDeleteFailed(btn.dataset.id ?? ""));
  });
}

function renderFailedRow(r: FailedDeliveryDto): string {
  const nextRetry = r.nextRetryTs > 0 ? new Date(r.nextRetryTs * 1000).toLocaleString() : "—";
  const stateClass = r.state === "exhausted" ? "alerting-failed-exhausted" : r.state === "resolved" ? "alerting-failed-resolved" : "alerting-failed-pending";
  const stateLabel = r.state === "exhausted" ? t("alertingFailedStateExhausted") : r.state === "resolved" ? t("alertingFailedStateResolved") : t("alertingFailedStatePending");
  // Phase 49 — translate endpointId into an endpoint name (when found in the local cache)
  const epChip = r.endpointId
    ? `<span class="alerting-failed-ep" title="${escAttr(r.endpointId)}">${esc(alertingEndpoints.find((e) => e.id === r.endpointId)?.name ?? r.endpointId)}</span>`
    : "";
  return `<div class="alerting-failed-row ${stateClass}">
    <div class="alerting-failed-meta"><code>${esc(r.id)}</code> ${epChip} <span class="alerting-failed-source">${esc(r.source)}</span> <span class="alerting-failed-state-badge">${esc(stateLabel)}</span></div>
    <div class="alerting-failed-detail">${esc(t("alertingFailedAttempts"))}: ${r.attempts}/${r.maxAttempts} · ${esc(t("alertingFailedError"))}: ${esc(r.lastError)}</div>
    <div class="alerting-failed-next">${esc(t("alertingFailedNextRetry"))}: ${esc(nextRetry)}</div>
    <div class="alerting-failed-actions">
      <button class="btn ghost alerting-failed-retry" data-id="${escAttr(r.id)}" type="button">${esc(t("alertingFailedRetry"))}</button>
      <button class="btn ghost alerting-failed-del" data-id="${escAttr(r.id)}" type="button">${esc(t("alertingFailedDelete"))}</button>
    </div>
  </div>`;
}

async function onRetryFailed(id: string): Promise<void> {
  if (!id) return;
  const msg = document.getElementById("alerting-failed-msg");
  try {
    await invoke("retry_alerting_failed", { id });
    if (msg) msg.textContent = `${t("alertingFailedRetryDone")}: ${id}`;
    void refreshAlertingFailed();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function onDeleteFailed(id: string): Promise<void> {
  if (!id) return;
  const msg = document.getElementById("alerting-failed-msg");
  try {
    await invoke("delete_alerting_failed", { id });
    if (msg) msg.textContent = `${t("alertingFailedDeleteDone")}: ${id}`;
    void refreshAlertingFailed();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

export async function clearAlertingResolved(): Promise<void> {
  const msg = document.getElementById("alerting-failed-msg");
  if (!window.confirm(t("alertingClearExhaustedConfirm"))) return;
  try {
    const n = await invoke<number>("clear_alerting_resolved");
    if (msg) msg.textContent = `${t("alertingClearExhaustedDone")}: ${n}`;
    void refreshAlertingFailed();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}
