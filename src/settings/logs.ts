import { invoke } from "@tauri-apps/api/core";
import { connectEventStream, onEvent, type OpencapxEvent } from "../events";
import { t } from "../i18n";
import { detailBlock, detailKv, esc, group } from "./shared";

// ---- Logs tab (plugin.log live stream + history) ----------------------------

export function renderLogs(body: HTMLElement): void {
  body.innerHTML = group("tabLogs",
    `<div class="setting-row vertical"><span class="setting-hint">${esc(t("logsHint"))}</span><div><span class="audit-live" id="logs-live">${esc(t("logsOffline"))}</span></div><div id="logs-list"></div></div>
    <div class="setting-row vertical"><div class="logs-filter-row"><input class="logs-input" id="logs-q" placeholder="${esc(t("logsQueryPlaceholder"))}" type="text"/><select class="logs-select" id="logs-level"><option value="">${esc(t("logsLevelAll"))}</option><option value="info">${esc(t("logLevelInfo"))}</option><option value="warn">${esc(t("logLevelWarn"))}</option><option value="error">${esc(t("logLevelError"))}</option><option value="debug">${esc(t("logLevelDebug"))}</option></select><input class="logs-input" id="logs-plugin" placeholder="${esc(t("logsPluginPlaceholder"))}" type="text"/><label class="logs-tail-label"><input type="checkbox" id="logs-tail"/><span>${esc(t("logsTail"))}</span></label><button class="btn ghost" id="logs-apply" type="button">${esc(t("logsApply"))}</button><button class="btn ghost" id="logs-clear" type="button">${esc(t("logsClear"))}</button></div><span class="setting-hint" id="logs-msg"></span></div>`);
  document.getElementById("logs-apply")?.addEventListener("click", () => void refreshLogs(true));
  document.getElementById("logs-clear")?.addEventListener("click", () => clearLogFilters());
  document.getElementById("logs-tail")?.addEventListener("change", () => void onTailToggle());
  void refreshLogs(true);
  startLogsStream();
}

interface LogEntry {
  id: string;
  kind: string;
  plugin_id: string;
  level: string;
  source: string;
  message: string;
  timestamp: number;
}

let logsStreamOnEvent: (() => void) | null = null;

function readLogFilters(): { query: string; level: string; pluginId: string; sinceTs: number | null } {
  const qEl = document.getElementById("logs-q") as HTMLInputElement | null;
  const lvEl = document.getElementById("logs-level") as HTMLSelectElement | null;
  const piEl = document.getElementById("logs-plugin") as HTMLInputElement | null;
  const query = qEl?.value.trim() ?? "";
  const level = lvEl?.value ?? "";
  const pluginId = piEl?.value.trim() ?? "";
  return { query, level, pluginId, sinceTs: null };
}

function clearLogFilters(): void {
  const qEl = document.getElementById("logs-q") as HTMLInputElement | null;
  const lvEl = document.getElementById("logs-level") as HTMLSelectElement | null;
  const piEl = document.getElementById("logs-plugin") as HTMLInputElement | null;
  if (qEl) qEl.value = "";
  if (lvEl) lvEl.value = "";
  if (piEl) piEl.value = "";
  void refreshLogs(true);
}

function onTailToggle(): void {
  logTailActive = (document.getElementById("logs-tail") as HTMLInputElement | null)?.checked ?? false;
  logTailLastTs = 0;
  if (logTailActive) {
    startLogTailPoll();
  } else {
    stopLogTailPoll();
    void refreshLogs(true);
  }
}

let logTailActive = false;
let logTailLastTs = 0;
let logTailTimer: ReturnType<typeof setInterval> | null = null;

function startLogTailPoll(): void {
  stopLogTailPoll();
  // Immediately pull a round of new events
  void pollLogTail();
  logTailTimer = setInterval(() => void pollLogTail(), 2000);
}

function stopLogTailPoll(): void {
  if (logTailTimer) {
    clearInterval(logTailTimer);
    logTailTimer = null;
  }
}

async function pollLogTail(): Promise<void> {
  if (!logTailActive) return;
  const filters = readLogFilters();
  try {
    const newOnes = await invoke<LogEntry[]>("search_logs", {
      filter: {
        query: filters.query,
        level: filters.level,
        pluginId: filters.pluginId,
        sinceTs: logTailLastTs,
        limit: 100,
      },
    });
    if (newOnes.length > 0) {
      logTailLastTs = Math.max(...newOnes.map((e) => e.timestamp));
      prependLogEntries(newOnes);
    }
  } catch (err) {
    console.warn("log tail poll failed:", err);
  }
}

function prependLogEntries(entries: LogEntry[]): void {
  const box = document.getElementById("logs-list");
  if (!box) return;
  const empty = box.querySelector(".setting-hint");
  if (empty) box.innerHTML = "";
  // Ascending order: old ones inserted first, new ones after
  const html = entries.map((e) => renderLogRow(e)).join("");
  box.insertAdjacentHTML("afterbegin", html);
}

async function refreshLogs(reset: boolean): Promise<void> {
  const box = document.getElementById("logs-list");
  const msg = document.getElementById("logs-msg");
  if (!box) return;
  const filters = readLogFilters();
  try {
    const entries = await invoke<LogEntry[]>("search_logs", {
      filter: {
        query: filters.query,
        level: filters.level,
        pluginId: filters.pluginId,
        sinceTs: null,
        limit: 200,
      },
    });
    if (reset) logTailLastTs = 0;
    if (entries.length > 0) {
      logTailLastTs = Math.max(logTailLastTs, ...entries.map((e) => e.timestamp));
    }
    if (entries.length === 0) {
      box.innerHTML = `<div class="setting-hint">${esc(t("logsEmpty"))}</div>`;
      if (msg) msg.textContent = "";
      return;
    }
    box.innerHTML = entries.map((e) => renderLogRow(e)).join("");
    if (msg)
      msg.textContent = `${entries.length} ${esc(t("logsEntriesShown"))}`;
  } catch (err) {
    box.innerHTML = `<div class="setting-hint">✗ ${esc(String(err))}</div>`;
  }
}

function renderLogRow(e: OpencapxEvent | LogEntry): string {
  const payload = (e as OpencapxEvent).payload as Record<string, unknown> | undefined;
  const pluginId = String(payload?.["pluginId"] ?? (e as LogEntry).plugin_id ?? "—");
  const id = String((e as OpencapxEvent).id ?? (e as LogEntry).id ?? "");
  const level = String(payload?.["level"] ?? (e as LogEntry).level ?? "info").toLowerCase();
  const source = String(payload?.["source"] ?? (e as LogEntry).source ?? "");
  const message = String(payload?.["message"] ?? (e as LogEntry).message ?? "");
  const timestamp = (e as OpencapxEvent).timestamp || (e as LogEntry).timestamp;
  const time = new Date(timestamp * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const fullTs = new Date(timestamp * 1000).toISOString().replace("T", " ").slice(0, 19);
  const lvlLabel = level === "warn" || level === "warning"
    ? t("logLevelWarn")
    : level === "error" || level === "err"
    ? t("logLevelError")
    : level === "debug"
    ? t("logLevelDebug")
    : t("logLevelInfo");
  const lvlClass = level === "warn" || level === "warning"
    ? "warn"
    : level === "error" || level === "err"
    ? "error"
    : level === "debug"
    ? "debug"
    : "info";
  const srcLabel = source === "stderr" ? t("logSourceStderr") : source === "reverse" ? t("logSourceReverse") : source;
  const detail =
    detailBlock(t("recMessage"), message) +
    detailKv(t("auditDetailSource"), srcLabel) +
    detailKv(t("auditDetailId"), id) +
    detailKv(t("auditDetailTime"), fullTs);
  return `<details class="rec"><summary class="rec-head log-head"><span class="rec-time seconds">${esc(time)}</span><span class="rec-badge log-lvl-${lvlClass}">${esc(lvlLabel)}</span><span class="rec-title">${esc(pluginId)}</span><span class="rec-sub">${esc(message)}</span></summary><div class="rec-detail">${detail}</div></details>`;
}

function startLogsStream(): void {
  stopLogsStream();
  const live = document.getElementById("logs-live");
  connectEventStream();
  const setLive = (text: string, cls: string) => {
    if (!live) return;
    live.textContent = text;
    live.className = `audit-live ${cls}`;
  };
  setLive(t("logsConnecting"), "audit-live-pending");
  logsStreamOnEvent = onEvent("plugin.log", (ev) => {
    const box = document.getElementById("logs-list");
    if (!box) return;
    if (!logEntryMatchesFilters(ev as unknown as LogEntry)) return;
    const empty = box.querySelector(".setting-hint");
    if (empty && empty.textContent === t("logsEmpty")) box.innerHTML = "";
    box.insertAdjacentHTML("afterbegin", renderLogRow(ev));
    setLive(t("logsLive"), "audit-live-on");
  });
}

function logEntryMatchesFilters(e: LogEntry): boolean {
  const filters = readLogFilters();
  if (filters.level && (e.level || "").toLowerCase() !== filters.level.toLowerCase()) return false;
  if (filters.pluginId && e.plugin_id !== filters.pluginId) return false;
  if (filters.query) {
    const q = filters.query.toLowerCase();
    const hay =
      ((e.message || "") + " " + (e.plugin_id || "") + " " + (e.source || "")).toLowerCase();
    if (!hay.includes(q)) return false;
  }
  return true;
}

export function stopLogsStream(): void {
  logsStreamOnEvent?.();
  logsStreamOnEvent = null;
}
