import { invoke } from "@tauri-apps/api/core";
import { connectEventStream, onEvent, type OpencapxEvent } from "../events";
import { t } from "../i18n";
import { dayLabel, detailKv, detailPayload, esc, escAttr, formatBytes, group, listError, listSkeleton } from "./shared";

let auditStreamOnEvent: (() => void) | null = null;
// i2 §14 — the Audit tab shows the Activity Timeline by default; the old permission list moves into a second view.
let auditView: "timeline" | "perms" = "timeline";

export function renderAudit(body: HTMLElement): void {
  body.innerHTML = group("tabAudit",
    `<div class="setting-row vertical"><div class="audit-view-toggle"><button class="btn ghost" id="audit-view-timeline" type="button" aria-pressed="${auditView === "timeline"}">${esc(t("auditViewTimeline"))}</button><button class="btn ghost" id="audit-view-perms" type="button" aria-pressed="${auditView === "perms"}">${esc(t("auditViewPerms"))}</button></div></div>
    <div class="setting-row vertical" id="timeline-row"${auditView === "perms" ? " hidden" : ""}><div class="filter-bar"><label class="filter-field filter-field-narrow"><span class="filter-label">${esc(t("timelineAgentPlaceholder"))}</span><input type="text" id="timeline-agent" placeholder="${esc(t("timelineAgentPlaceholder"))}" aria-label="${esc(t("timelineAgentPlaceholder"))}" /></label><div class="filter-actions"><button class="btn ghost" id="timeline-refresh" type="button">${esc(t("auditRefresh"))}</button><span class="setting-hint" id="timeline-msg"></span></div></div><div id="timeline-list"></div></div>
    <div class="setting-row vertical" id="audit-row"${auditView === "timeline" ? " hidden" : ""}><span class="setting-hint">${esc(t("auditHint"))}</span><div class="filter-bar"><label class="filter-field"><span class="filter-label">${esc(t("auditSearchLabel"))}</span><input type="text" id="audit-q" placeholder="${esc(t("auditSearchPlaceholder"))}" aria-label="${esc(t("auditSearchLabel"))}" /></label><div class="filter-actions"><button class="btn ghost" id="audit-apply" type="button">${esc(t("auditApply"))}</button><button class="btn ghost" id="audit-refresh" type="button">${esc(t("auditRefresh"))}</button></div></div><details class="filter-adv" id="audit-adv"><summary class="filter-adv-summary">${esc(t("auditAdvanced"))}<span class="filter-adv-mark" id="audit-adv-mark"></span></summary><div class="filter-grid"><label class="filter-field"><span class="filter-label">${esc(t("auditPrefixLabel"))}</span><input type="text" id="audit-prefix" placeholder="${esc(t("auditPrefixPlaceholder"))}" aria-label="${esc(t("auditPrefixLabel"))}" /></label><label class="filter-field"><span class="filter-label">${esc(t("auditSince"))}</span><input type="datetime-local" id="audit-since" aria-label="${esc(t("auditSince"))}" /></label><label class="filter-field"><span class="filter-label">${esc(t("auditUntil"))}</span><input type="datetime-local" id="audit-until" aria-label="${esc(t("auditUntil"))}" /></label></div><div class="filter-actions"><button class="btn ghost" id="audit-clear" type="button">${esc(t("auditClear"))}</button><button class="btn ghost" id="audit-export" type="button">${esc(t("auditExportCsv"))}</button></div></details><div class="filter-status"><span class="setting-hint" id="audit-msg"></span><span class="audit-live" id="audit-live">${esc(t("auditOffline"))}</span></div><div id="audit-list"></div></div>
    <div class="setting-row vertical"><span class="setting-hint">${esc(t("replayHint"))}</span><div><button class="btn ghost" id="replay-refresh" type="button">${esc(t("replayRefresh"))}</button><span class="setting-hint" id="replay-msg"></span></div><div id="replay-list"></div></div>`);
  document.getElementById("audit-view-timeline")?.addEventListener("click", () => setAuditView("timeline"));
  document.getElementById("audit-view-perms")?.addEventListener("click", () => setAuditView("perms"));
  document.getElementById("timeline-refresh")?.addEventListener("click", () => void refreshTimeline());
  document.getElementById("timeline-agent")?.addEventListener("keydown", (ev) => {
    if ((ev as KeyboardEvent).key === "Enter") void refreshTimeline();
  });
  document.getElementById("audit-refresh")?.addEventListener("click", () => void refreshAudit());
  document.getElementById("audit-apply")?.addEventListener("click", () => void refreshAudit());
  document.getElementById("audit-q")?.addEventListener("keydown", (ev) => {
    if ((ev as KeyboardEvent).key === "Enter") void refreshAudit();
  });
  document.getElementById("audit-clear")?.addEventListener("click", () => {
    (document.getElementById("audit-q") as HTMLInputElement | null)!.value = "";
    (document.getElementById("audit-prefix") as HTMLInputElement | null)!.value = "";
    (document.getElementById("audit-since") as HTMLInputElement | null)!.value = "";
    (document.getElementById("audit-until") as HTMLInputElement | null)!.value = "";
    paintAuditAdv();
    void refreshAudit();
  });
  document.getElementById("audit-export")?.addEventListener("click", () => void exportAuditCsv());
  // Even when advanced filters are collapsed, it must be visible that 'a filter is active': any prefix/time value -> the badge shows a count.
  // The search box is always visible so it doesn't count; values still come from readAuditFilter(), without changing refreshAudit logic.
  const paintAuditAdv = (): void => {
    const mark = document.getElementById("audit-adv-mark");
    if (!mark) return;
    const f = readAuditFilter();
    const n = (f.kindPrefix ? 1 : 0) + (f.sinceTs ? 1 : 0) + (f.untilTs ? 1 : 0);
    mark.textContent = n > 0 ? `• ${n}` : "";
  };
  ["audit-prefix", "audit-since", "audit-until"].forEach((id) =>
    document.getElementById(id)?.addEventListener("input", paintAuditAdv),
  );
  paintAuditAdv();
  document.getElementById("replay-refresh")?.addEventListener("click", () => void refreshReplay());
  if (auditView === "timeline") void refreshTimeline();
  else void refreshAudit();
  void refreshReplay();
  startAuditStream();
}

interface AuditEntry {
  id: string;
  kind: string;
  plugin_id: string;
  permission: string;
  decision: string;
  reason: string;
  timestamp: number;
}

/// i2 §14 Activity Timeline entry (the backend timeline_events are already allowlist-filtered).
/// Text assembly happens in this layer; presentation does not go into Rust.
interface TimelineEntry {
  id: string;
  timestamp: number;
  kind: string;
  category: string;
  agent: string;
  source: string;
  payload: Record<string, unknown>;
}

const TL_ICONS: Record<string, string> = {
  agent: "🤖",
  capability: "⚡",
  permission: "🔐",
  security: "🛡️",
  plugin: "🧩",
  pet: "🐱",
  system: "⚙️",
  notification: "🔔",
};

/// {k} placeholder substitution; values come from payload fields (toString), defaulting to an empty string.
function tlFmt(tpl: string, p: Record<string, unknown>): string {
  return tpl.replace(/\{(\w+)\}/g, (_, k: string) => {
    const v = p[k];
    return v === undefined || v === null ? "" : String(v);
  });
}

/// kind -> localized text. Kinds not covered fall back to the generic template.
function tlText(e: TimelineEntry): string {
  const p = e.payload;
  const str = (k: string): string => (typeof p[k] === "string" ? (p[k] as string) : "");
  switch (e.kind) {
    case "agent.started": return t("tlAgentStarted");
    case "agent.registered": return t("tlAgentRegistered");
    case "capability.completed":
      return tlFmt(t("tlCapCompleted"), { capability: str("capability"), plugin: str("pluginId"), ms: p.elapsedMs ?? "" });
    case "capability.failed": {
      // payload: attemptedProviders[{pluginId, error}] — take the first failed provider
      const att = Array.isArray(p.attemptedProviders) ? (p.attemptedProviders as { pluginId?: string; error?: string }[]) : [];
      const first = att[0] ?? {};
      return tlFmt(t("tlCapFailed"), { capability: str("capability"), plugin: first.pluginId ?? "", reason: first.error ?? "" });
    }
    case "capability.subscribed":
      return tlFmt(t("tlSubscribed"), { capability: str("capability") });
    case "capability.unsubscribed":
      return tlFmt(t("tlUnsubscribed"), { capability: str("capability"), reason: str("reason") });
    case "permission.granted":
      return tlFmt(t("tlPermGranted"), { permission: str("permission") });
    case "permission.denied":
      return tlFmt(t("tlPermDenied"), { permission: str("permission") });
    case "permission.requested":
      return tlFmt(t("tlPermRequested"), { permission: str("permission") });
    case "auth.rejected":
      return tlFmt(t("tlAuthRejected"), { reason: str("reason") });
    case "plugin.installed":
      return tlFmt(t("tlPluginInstalled"), { plugin: str("pluginId"), version: str("version") });
    case "plugin.uninstalled":
      return tlFmt(t("tlPluginUninstalled"), { plugin: str("pluginId") });
    case "plugin.signature.verified":
      return tlFmt(t("tlPluginSignature"), { plugin: str("id"), key: str("keyId") });
    case "plugin.start.rejected":
      return tlFmt(t("tlPluginStartRejected"), { plugin: str("pluginId"), reason: str("reason") });
    case "plugin.kill_switch.enabled":
      return tlFmt(t("tlPluginKilled"), { reason: str("reason") });
    case "plugin.lifecycle.crashed":
      return tlFmt(t("tlPluginCrashed"), { plugin: str("pluginId"), reason: str("reason") });
    case "pet.say":
      return tlFmt(t("tlPetSay"), { text: str("text") });
    case "pet.state_changed":
      return tlFmt(t("tlPetState"), { state: str("state") });
    case "workspace.switched":
      return tlFmt(t("tlWorkspace"), { profile: str("profile") });
    case "notification.posted":
      return tlFmt(t("tlNotification"), { title: str("title"), body: str("body") });
    default:
      return tlFmt(t("tlGeneric"), { kind: e.kind });
  }
}

/// Timeline entry: collapsed it shows a one-line summary; expand to see category/source/raw payload.
function renderTimelineRow(e: TimelineEntry): string {
  const time = new Date(e.timestamp * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const who = e.agent || (typeof e.payload.pluginId === "string" ? (e.payload.pluginId as string) : "") || e.source;
  const icon = TL_ICONS[e.category] ?? TL_ICONS.system;
  const fullTs = new Date(e.timestamp * 1000).toISOString().replace("T", " ").slice(0, 19);
  const detail =
    detailKv(t("auditDetailCategory"), e.category) +
    detailKv(t("auditDetailKind"), e.kind) +
    detailKv(t("auditDetailAgent"), e.agent) +
    detailKv(t("auditDetailSource"), e.source) +
    detailKv(t("auditDetailTime"), fullTs) +
    detailPayload(t("auditDetailPayload"), e.payload);
  return `<details class="rec"><summary class="rec-head"><span class="rec-time">${esc(time)}</span><span class="rec-icon">${icon}</span><span class="rec-text"><b>${esc(who)}</b> ${esc(tlText(e))}</span></summary><div class="rec-detail">${detail}</div></details>`;
}

/// View toggle: timeline (default) / perms (legacy permission list). Only touches DOM visibility + button state.
function setAuditView(v: "timeline" | "perms"): void {
  auditView = v;
  document.getElementById("timeline-row")?.toggleAttribute("hidden", v !== "timeline");
  document.getElementById("audit-row")?.toggleAttribute("hidden", v !== "perms");
  const bt = document.getElementById("audit-view-timeline");
  const bp = document.getElementById("audit-view-perms");
  bt?.setAttribute("aria-pressed", String(v === "timeline"));
  bp?.setAttribute("aria-pressed", String(v === "perms"));
  if (v === "timeline") void refreshTimeline();
  else void refreshAudit();
}

async function refreshTimeline(): Promise<void> {
  const box = document.getElementById("timeline-list");
  const msg = document.getElementById("timeline-msg");
  if (!box) return;
  // On first load (container still empty) lay out the skeleton + aria-busy; refresh/retry doesn't touch the DOM and doesn't flash the skeleton
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  const agent = (document.getElementById("timeline-agent") as HTMLInputElement | null)?.value.trim() ?? "";
  let entries: TimelineEntry[] = [];
  let failed = false;
  try {
    entries = await invoke<TimelineEntry[]>("timeline_events", { limit: 300, agent: agent || null });
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  if (failed) {
    if (msg) msg.textContent = "";
    box.innerHTML = listError("listLoadFailed", "timeline-retry");
    document.getElementById("timeline-retry")?.addEventListener("click", () => void refreshTimeline());
    return;
  }
  if (entries.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("timelineEmpty"))}</div>`;
    if (msg) msg.textContent = "";
    return;
  }
  // Group by day (the backend returns newest-first; within a group keep that order, across groups show nearest to farthest)
  const groups: { day: string; items: TimelineEntry[] }[] = [];
  for (const e of entries) {
    const day = dayLabel(e.timestamp);
    const last = groups[groups.length - 1];
    if (last && last.day === day) last.items.push(e);
    else groups.push({ day, items: [e] });
  }
  box.innerHTML = groups
    .map((g) => `<div class="tl-day">${esc(g.day)}</div>` + g.items.map(renderTimelineRow).join(""))
    .join("");
  if (msg) msg.textContent = `${entries.length} ${t("listItems")}`;
}

// Phase 34 — Event stream record/replay UI.
interface ReplaySession {
  sessionId: string;
  startedAt: number;
  endedAt?: number;
  sizeBytes: number;
  lineCount: number;
}

async function refreshReplay(): Promise<void> {
  const box = document.getElementById("replay-list");
  const msg = document.getElementById("replay-msg");
  if (!box) return;
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(2);
  }
  let sessions: ReplaySession[] = [];
  let failed = false;
  try {
    sessions = await invoke<ReplaySession[]>("list_replay_sessions");
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  if (failed) {
    if (msg) msg.textContent = "";
    box.innerHTML = listError("listLoadFailed", "replay-retry");
    document.getElementById("replay-retry")?.addEventListener("click", () => void refreshReplay());
    return;
  }
  if (sessions.length === 0) {
    box.innerHTML = `<p class="list-empty">${esc(t("replayEmpty"))}</p>`;
    if (msg) msg.textContent = "";
    return;
  }
  const rows = sessions
    .map((s) => {
      const startDate = new Date(s.startedAt * 1000).toLocaleString();
      const dur = s.endedAt ? `${s.endedAt - s.startedAt}s` : "ongoing";
      return `<tr><td><code>${esc(s.sessionId)}</code></td><td>${esc(startDate)}</td><td>${esc(dur)}</td><td>${s.lineCount}</td><td>${esc(formatBytes(s.sizeBytes))}</td><td><input type="text" class="replay-filter" placeholder="${esc(t("replayFilter"))}" aria-label="${esc(t("replayFilter"))}: ${esc(s.sessionId)}" data-sid="${escAttr(s.sessionId)}" /><button class="btn ghost replay-go" data-sid="${escAttr(s.sessionId)}">▶</button><button class="btn ghost replay-export" data-sid="${escAttr(s.sessionId)}">⬇</button></td></tr>`;
    })
    .join("");
  box.innerHTML = `<table class="replay-table"><thead><tr><th>${esc(t("replaySession"))}</th><th>${esc(t("replayStarted"))}</th><th>${esc(t("replayDuration"))}</th><th>${esc(t("replayEvents"))}</th><th>${esc(t("replaySize"))}</th><th>${esc(t("replayActions"))}</th></tr></thead><tbody>${rows}</tbody></table>`;
  if (msg) msg.textContent = `${sessions.length} ${esc(t("replaySessions"))}`;
  box.querySelectorAll<HTMLButtonElement>(".replay-go").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const sid = btn.dataset.sid!;
      const filter = (box.querySelector(`input.replay-filter[data-sid="${CSS.escape(sid)}"]`) as HTMLInputElement)?.value.trim() ?? "";
      const n = await invoke<number>("replay_session_to_stream", { sessionId: sid, filterKind: filter });
      if (msg) msg.textContent = `${t("replayReplayed")} ${n}`;
    });
  });
  box.querySelectorAll<HTMLButtonElement>(".replay-export").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const sid = btn.dataset.sid!;
      const events = await invoke<{ kind: string; timestamp: number; source: string; payload: unknown }[]>("get_replay_events", { sessionId: sid, limit: 50000 });
      const lines = events.map((e) => JSON.stringify({ id: "", type: e.kind, source: e.source, timestamp: e.timestamp, payload: e.payload })).join("\n");
      const blob = new Blob([lines + "\n"], { type: "application/x-ndjson" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `opencapx-replay-${sid}.ndjson`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
      if (msg) msg.textContent = t("replayExported");
    });
  });
}

// Phase 35 — read the current filter conditions from the Search/filter UI; an empty value means 'no filter' and returns null.
function readAuditFilter(): {
  kindPrefix: string | null;
  query: string | null;
  sinceTs: number | null;
  untilTs: number | null;
} {
  const q = (document.getElementById("audit-q") as HTMLInputElement | null)?.value.trim() ?? "";
  const prefix = (document.getElementById("audit-prefix") as HTMLInputElement | null)?.value.trim() ?? "";
  const sinceStr = (document.getElementById("audit-since") as HTMLInputElement | null)?.value ?? "";
  const untilStr = (document.getElementById("audit-until") as HTMLInputElement | null)?.value ?? "";
  // Parse datetime-local into unix seconds (Date.parse returns ms)
  const since = sinceStr ? Math.floor(new Date(sinceStr).getTime() / 1000) : 0;
  const until = untilStr ? Math.floor(new Date(untilStr).getTime() / 1000) : 0;
  return {
    kindPrefix: prefix || null,
    query: q || null,
    sinceTs: since > 0 ? since : null,
    untilTs: until > 0 ? until : null,
  };
}

// Whether any filter condition is active
function auditFilterActive(): boolean {
  const f = readAuditFilter();
  return !!(f.kindPrefix || f.query || f.sinceTs || f.untilTs);
}

async function refreshAudit(): Promise<void> {
  const box = document.getElementById("audit-list");
  const msg = document.getElementById("audit-msg");
  if (!box) return;
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  const filter = readAuditFilter();
  let entries: AuditEntry[] = [];
  let failed = false;
  try {
    if (auditFilterActive()) {
      entries = await invoke<AuditEntry[]>("search_audit_events", {
        filter: {
          kindPrefix: filter.kindPrefix,
          query: filter.query,
          sinceTs: filter.sinceTs,
          untilTs: filter.untilTs,
          limit: 500,
        },
      });
    } else {
      entries = await invoke<AuditEntry[]>("list_audit", { limit: 200 });
    }
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  if (failed) {
    if (msg) msg.textContent = "";
    box.innerHTML = listError("listLoadFailed", "audit-list-retry");
    document.getElementById("audit-list-retry")?.addEventListener("click", () => void refreshAudit());
    return;
  }
  if (entries.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("auditEmpty"))}</div>`;
    if (msg) msg.textContent = "";
    return;
  }
  box.innerHTML = entries.map((e) => renderAuditRow(auditRowFromEntry(e))).join("");
  if (msg) msg.textContent = `${entries.length} ${t("listItems")}`;
}

// Phase 35 — CSV export of the current filtered results (same Blob download pattern as the Phase 34 NDJSON).
// Fields: id, kind, pluginId, permission, decision, reason, timestamp, timestamp_iso
// Escaping: values containing commas/double quotes/newlines -> wrap in double quotes + escape inner double quotes as two.
function csvEscape(v: string): string {
  if (v === "") return "";
  if (/[",\n\r]/.test(v)) return `"${v.replace(/"/g, '""')}"`;
  return v;
}

async function exportAuditCsv(): Promise<void> {
  const msg = document.getElementById("audit-msg");
  try {
    let entries: AuditEntry[];
    const filter = readAuditFilter();
    if (auditFilterActive()) {
      entries = await invoke<AuditEntry[]>("search_audit_events", {
        filter: {
          kindPrefix: filter.kindPrefix,
          query: filter.query,
          sinceTs: filter.sinceTs,
          untilTs: filter.untilTs,
          limit: 5000,
        },
      });
    } else {
      entries = await invoke<AuditEntry[]>("list_audit", { limit: 5000 });
    }
    if (entries.length === 0) {
      if (msg) msg.textContent = t("auditEmpty");
      return;
    }
    const header = ["id", "kind", "pluginId", "permission", "decision", "reason", "timestamp", "timestamp_iso"];
    const rows = entries.map((e) => {
      const ts = new Date(e.timestamp * 1000).toISOString().replace("T", " ").slice(0, 19);
      return [
        e.id,
        e.kind,
        e.plugin_id,
        e.permission,
        e.decision,
        e.reason,
        String(e.timestamp),
        ts,
      ].map(csvEscape).join(",");
    });
    const csv = "﻿" + header.join(",") + "\n" + rows.join("\n") + "\n"; // BOM: make Excel recognize UTF-8
    const blob = new Blob([csv], { type: "text/csv;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    a.download = `opencapx-audit-${stamp}.csv`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
    if (msg) msg.textContent = `${entries.length} ${t("auditExported")}`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

interface AuditRowData {
  id: string;
  kind: string;
  pluginId: string;
  permission: string;
  decision: string;
  reason: string;
  timestamp: number;
}

function auditRowFromEvent(e: OpencapxEvent): AuditRowData {
  const p = (e.payload ?? {}) as Record<string, unknown>;
  return {
    id: e.id,
    kind: e.kind,
    pluginId: String(p["pluginId"] ?? "—"),
    permission: String(p["permission"] ?? ""),
    decision: String(p["decision"] ?? e.kind),
    reason: String(p["reason"] ?? ""),
    timestamp: e.timestamp,
  };
}

function auditRowFromEntry(e: AuditEntry): AuditRowData {
  return {
    id: e.id,
    kind: e.kind,
    pluginId: e.plugin_id || "—",
    permission: e.permission,
    decision: e.decision || e.kind,
    reason: e.reason,
    timestamp: e.timestamp,
  };
}

/// Permission decision record: collapsed it shows only 'time · decision · permission · plugin'; expand for the rest.
function renderAuditRow(d: AuditRowData): string {
  const time = new Date(d.timestamp * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const fullTs = new Date(d.timestamp * 1000).toISOString().replace("T", " ").slice(0, 19);
  const badgeClass = d.decision === "granted" ? " ok" : d.decision === "denied" ? " err" : "";
  const detail =
    detailKv(t("permReason"), d.reason) +
    (d.kind && d.kind !== d.decision ? detailKv(t("auditDetailKind"), d.kind) : "") +
    detailKv(t("auditDetailTime"), fullTs) +
    detailKv(t("auditDetailId"), d.id);
  return `<details class="rec"><summary class="rec-head rec-mono"><span class="rec-time seconds">${esc(time)}</span><span class="rec-badge${badgeClass}">${esc(d.decision)}</span><span class="rec-title">${esc(d.permission)}</span><span class="rec-sub">${esc(d.pluginId)}</span></summary><div class="rec-detail">${detail}</div></details>`;
}

function startAuditStream(): void {
  stopAuditStream();
  const live = document.getElementById("audit-live");
  connectEventStream();
  const setLive = (text: string, cls: string) => {
    if (!live) return;
    live.textContent = text;
    live.className = `audit-live ${cls}`;
  };
  setLive(t("auditConnecting"), "audit-live-pending");
  auditStreamOnEvent = onEvent("*", (ev) => {
    if (!ev.kind.startsWith("permission.")) return;
    const box = document.getElementById("audit-list");
    if (!box) return;
    const empty = box.querySelector(".setting-hint");
    if (empty && empty.textContent === t("auditEmpty")) box.innerHTML = "";
    box.insertAdjacentHTML("afterbegin", renderAuditRow(auditRowFromEvent(ev)));
    setLive(t("auditLive"), "audit-live-on");
  });
  // Simple readiness probe: one fetch to check the /events headers (HEAD won't work — only GET is supported).
  // Failure/reconnect is handled by events.ts itself; this only reflects status.
  setLive(t("auditConnecting"), "audit-live-pending");
}

export function stopAuditStream(): void {
  auditStreamOnEvent?.();
  auditStreamOnEvent = null;
}
