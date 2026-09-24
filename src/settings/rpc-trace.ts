import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { t } from "../i18n";
import {
  esc,
  formatBytes,
  formatLifecycleTime,
  group,
  listError,
  listSkeleton,
} from "./shared";

/// One row of list_all_traces / list_all_hook_sessions (one line per trace file; pending requests have no endedAt).
/// An empty project string = this trace was written before the project field existed (legacy data); grouped under 'Untagged project'.
interface TraceEntry {
  agentId: string;
  project: string;
  traceId: string;
  startedAt: number;
  endedAt?: number;
  sizeBytes: number;
  lineCount: number;
  /// 'ok' | 'error' | '' ('' = request pending / hook session has no root end).
  status: string;
  /// Last activity time (ms); hook sessions have no root end, so the row header uses it instead of a duration.
  lastTs: number;
}

/** Single /rpc trace row: the `ev` tag distinguishes the three states; camelCase fields are guaranteed by Rust serde rename. */
type RpcTraceLine =
  | { ev: "start"; spanId: string; parentId?: string; name: string; ts: number; attrs: unknown }
  | { ev: "event"; spanId: string; name: string; ts: number; attrs: unknown }
  | { ev: "end"; spanId: string; ts: number; status: "ok" | "error"; error?: string; attrs?: unknown };

interface TraceSummary {
  sessionId: string;
  startedAt: number;
  endedAt?: number;
  sizeBytes: number;
  lineCount: number;
}

interface TraceLine {
  ts: number;
  dir: "in" | "out";
  payload: Record<string, unknown>;
}

export async function showTraceDialog(pluginId: string, pluginName: string): Promise<void> {
  const overlay = document.createElement("div");
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog trace-dialog" role="dialog" aria-label="${esc(t("traceTitle"))}">
      <h2>${esc(t("traceTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)}</div>
      </div>
      <div class="trace-loading">${esc(t("traceLoading"))}</div>
    </div>`;
  document.body.appendChild(overlay);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) overlay.remove();
  });
  let sessions: TraceSummary[] = [];
  try {
    sessions = await invoke<TraceSummary[]>("list_plugin_traces", { id: pluginId });
  } catch (err) {
    overlay.innerHTML = `
      <div class="install-dialog trace-dialog" role="dialog" aria-label="${esc(t("traceTitle"))}">
        <h2>${esc(t("traceTitle"))}</h2>
        <p class="muted">${esc(t("traceLoadFailed"))}: ${esc(String((err as Error).message ?? err))}</p>
        <div class="install-actions">
          <button class="btn ghost" id="trace-close" type="button">${esc(t("installPreviewCancel"))}</button>
        </div>
      </div>`;
    overlay.querySelector("#trace-close")?.addEventListener("click", () => overlay.remove());
    return;
  }
  if (sessions.length === 0) {
    overlay.innerHTML = `
      <div class="install-dialog trace-dialog" role="dialog" aria-label="${esc(t("traceTitle"))}">
        <h2>${esc(t("traceTitle"))}</h2>
        <div class="install-head">
          <div class="install-name">${esc(pluginName)}</div>
          <div class="install-id">${esc(pluginId)}</div>
        </div>
        <p class="muted">${esc(t("traceEmpty"))}</p>
        <div class="install-actions">
          <button class="btn ghost" id="trace-close" type="button">${esc(t("installPreviewCancel"))}</button>
        </div>
      </div>`;
    overlay.querySelector("#trace-close")?.addEventListener("click", () => overlay.remove());
    return;
  }
  // Show the session list; clicking switches to details
  const sessionList = sessions
    .map(
      (s) =>
        `<li class="trace-session-row" data-session="${esc(s.sessionId)}"><span class="trace-session-id">${esc(s.sessionId)}</span><span class="trace-session-meta">${s.lineCount} ${esc(t("traceLines"))} · ${formatBytes(s.sizeBytes)}</span></li>`,
    )
    .join("");
  overlay.innerHTML = `
    <div class="install-dialog trace-dialog" role="dialog" aria-label="${esc(t("traceTitle"))}">
      <h2>${esc(t("traceTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)} · ${sessions.length} ${esc(t("traceSessions"))}</div>
      </div>
      <ul class="trace-session-list">${sessionList}</ul>
      <div id="trace-detail"></div>
      <div class="install-actions">
        <button class="btn ghost" id="trace-close" type="button">${esc(t("installPreviewCancel"))}</button>
      </div>
    </div>`;
  overlay.querySelector("#trace-close")?.addEventListener("click", () => overlay.remove());
  const detail = overlay.querySelector("#trace-detail") as HTMLElement | null;
  overlay.querySelectorAll<HTMLElement>(".trace-session-row").forEach((row) => {
    row.addEventListener("click", async () => {
      const sid = row.dataset.session ?? "";
      if (!detail) return;
      detail.innerHTML = `<p class="muted">${esc(t("traceLoading"))}</p>`;
      let lines: TraceLine[] = [];
      try {
        lines = await invoke<TraceLine[]>("get_plugin_trace", { id: pluginId, sessionId: sid, limit: 200 });
      } catch (err) {
        detail.innerHTML = `<p class="muted">${esc(t("traceLoadFailed"))}: ${esc(String((err as Error).message ?? err))}</p>`;
        return;
      }
      const body = lines.length
        ? `<ul class="trace-lines">${lines
            .map(
              (l) =>
                `<li><span class="trace-dir trace-dir-${esc(l.dir)}">${l.dir === "in" ? "←" : "→"}</span><span class="trace-ts">${esc(formatLifecycleTime(l.ts))}</span><code class="trace-payload">${esc(JSON.stringify(l.payload))}</code></li>`,
            )
            .join("")}</ul>`
        : `<p class="muted">${esc(t("traceEmpty"))}</p>`;
      detail.innerHTML = `<div class="trace-detail-head">${esc(sid)} · ${lines.length} ${esc(t("traceLines"))}</div>${body}`;
    });
  });
}

/** Span tree row rendering: group the tree by parentId, inline the event rows, indent by depth. */
function renderRpcTraceTree(lines: RpcTraceLine[]): string {
  // get_rpc_trace returns in reverse order -> reverse back to ascending time order to build the tree
  const ordered = [...lines].reverse();
  const starts = new Map<string, Extract<RpcTraceLine, { ev: "start" }>>();
  const ends = new Map<string, Extract<RpcTraceLine, { ev: "end" }>>();
  const events = new Map<string, Extract<RpcTraceLine, { ev: "event" }>[]>();
  for (const l of ordered) {
    if (l.ev === "start") starts.set(l.spanId, l);
    else if (l.ev === "end") ends.set(l.spanId, l);
    else {
      const arr = events.get(l.spanId) ?? [];
      arr.push(l);
      events.set(l.spanId, arr);
    }
  }
  const children = new Map<string, string[]>();
  for (const [id, s] of starts) {
    const key = s.parentId ?? "";
    const arr = children.get(key) ?? [];
    arr.push(id);
    children.set(key, arr);
  }
  const rows: string[] = [];
  // Don't render empty payloads: {} / [] / empty strings (including whitespace-only) are noise in the tree (measured on capability.things.list with {}),
  // rendering them only stretches the row and wastes attention; everything else still outputs full JSON.
  const fmtAttrs = (a: unknown): string => {
    const s = JSON.stringify(a);
    if (s === undefined || s === "{}" || s === "[]" || (typeof a === "string" && a.trim() === "")) return "";
    return ` <code class="trace-payload">${esc(s)}</code>`;
  };
  const walk = (id: string, depth: number): void => {
    const s = starts.get(id);
    if (!s) return;
    const e = ends.get(id);
    const dur = e ? `${e.ts - s.ts}ms` : "…";
    const err = e?.error ? ` <span class="rpc-span-err">${esc(e.error)}</span>` : "";
    rows.push(
      `<li class="rpc-span rpc-span-${e ? e.status : "unset"}" style="margin-left:${depth * 16}px">` +
        `<span class="rpc-span-name">${esc(s.name)}</span><span class="rpc-span-dur">${esc(dur)}</span>${err}` +
        `${fmtAttrs(s.attrs)}</li>`,
    );
    for (const ev of events.get(id) ?? []) {
      rows.push(
        `<li class="rpc-event" style="margin-left:${(depth + 1) * 16}px">· ${esc(ev.name)}${fmtAttrs(ev.attrs)}</li>`,
      );
    }
    for (const c of children.get(id) ?? []) walk(c, depth + 1);
  };
  for (const rootId of children.get("") ?? []) walk(rootId, 0);
  return rows.join("");
}

/// Project grouping: rpc / hook entries with the same project are grouped together; lastTs = the group's most recent activity time (ms).
interface RpcProjectGroup {
  key: string;
  traces: TraceEntry[];
  hooks: TraceEntry[];
  lastTs: number;
}

/// Return value of export_project_chains (Rust serde camelCase). The frontend only reads counts/directory/failure count,
/// it does not parse the chains content — file copying and index.json are all done by the backend.
interface ExportReport {
  dir: string;
  project: string;
  exported: number;
  overwritten: number;
  manifestPath: string;
  chains: { traceId: string; file: string; agentId: string; startedAt: number; endedAt: number | null; lineCount: number; sizeBytes: number; status: string }[];
  failures: { traceId: string; reason: string }[];
}

/// The two full lists from the last successful fetch: exporting all projects reuses the same grouping from groupRpcProjects
/// (including the '' untagged project), without rescanning the DOM or creating another grouping logic.
let rpcLastTraces: TraceEntry[] = [];
let rpcLastHooks: TraceEntry[] = [];

/// Concurrency guard for export-all-projects: any re-trigger while running (including while the directory picker is open) is silently ignored.
let rpcExportAllProjectsBusy = false;

/// Request chains tab: grouped by project, all expanded inline; the two full lists arrive at once and project bodies land with the render (zero requests to expand),
/// only each row's details (trace tree / hook events) are lazy-loaded on first expand.
/// allowEnter: true only on the 'tab-switch render' (plays the enter animation); refresh/retry don't pass it,
/// and .rpc-no-enter explicitly turns it off — .fresh-tab lingers in #tab-body across refreshes, and without turning it off it replays on every refresh.
async function refreshRpcTraces(allowEnter = false): Promise<void> {
  const box = document.getElementById("rpc-trace-list");
  const msg = document.getElementById("rpc-trace-msg");
  if (!box) return;
  box.classList.toggle("rpc-no-enter", !allowEnter);
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  // Remember expanded projects and restore them after a refresh (same details[open] approach as refreshIdAgents)
  const openProjects = new Set(
    Array.from(box.querySelectorAll<HTMLDetailsElement>("details[data-rpc-project][open]")).map(
      (d) => d.dataset.rpcProject ?? "",
    ),
  );
  let traces: TraceEntry[] = [];
  let hooks: TraceEntry[] = [];
  let failed = false;
  try {
    [traces, hooks] = await Promise.all([invoke<TraceEntry[]>("list_all_traces"), invoke<TraceEntry[]>("list_all_hook_sessions")]);
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  rpcLastTraces = traces;
  rpcLastHooks = hooks;
  if (failed) {
    if (msg) msg.textContent = "";
    box.innerHTML = listError("listLoadFailed", "rpc-trace-retry");
    document.getElementById("rpc-trace-retry")?.addEventListener("click", () => void refreshRpcTraces());
    return;
  }
  if (traces.length === 0 && hooks.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("rpcTraceEmpty"))}</div>`;
    if (msg) msg.textContent = "";
    return;
  }
  const groups = groupRpcProjects(traces, hooks);
  box.innerHTML = `<div class="settings-list">${groups.map((g) => rpcProjectGroup(g, openProjects.has(g.key))).join("")}</div>`;
  wireRpcRows(box);
  wireRpcExportAll(box);
  if (msg) {
    const failedCount = traces.filter((e) => e.status === "error").length;
    msg.textContent = `${traces.length + hooks.length} ${t("listItems")}${failedCount > 0 ? ` · ${failedCount} ${t("rpcTraceFailed")}` : ""}`;
  }
}

/// The two full lists -> grouped by project; most recent activity first, the untagged project ('') always last.
function groupRpcProjects(traces: TraceEntry[], hooks: TraceEntry[]): RpcProjectGroup[] {
  const byKey = new Map<string, RpcProjectGroup>();
  const put = (e: TraceEntry, kind: "rpc" | "hook"): void => {
    let g = byKey.get(e.project);
    if (!g) {
      g = { key: e.project, traces: [], hooks: [], lastTs: 0 };
      byKey.set(e.project, g);
    }
    (kind === "rpc" ? g.traces : g.hooks).push(e);
    g.lastTs = Math.max(g.lastTs, e.endedAt ?? e.startedAt);
  };
  for (const e of traces) put(e, "rpc");
  for (const e of hooks) put(e, "hook");
  return [...byKey.values()].sort((a, b) => {
    if (a.key === "") return 1;
    if (b.key === "") return -1;
    return b.lastTs - a.lastTs;
  });
}

/// Project group row: title + two counts + last activity time (ms -> s then formatted); both body sections render with the list in one pass.
/// When the group has rpc entries with errors, append the failure count to the subtitle — so failures are visible even while collapsed.
function rpcProjectGroup(g: RpcProjectGroup, open: boolean): string {
  const rpcBody = g.traces.length
    ? `<div class="settings-list">${g.traces.map(rpcTraceRow).join("")}</div>`
    : `<p class="list-empty">${esc(t("rpcTraceEmpty"))}</p>`;
  const hookBody = g.hooks.length
    ? `<div class="settings-list">${g.hooks.map(rpcHookRow).join("")}</div>`
    : `<p class="list-empty">${esc(t("traceEmpty"))}</p>`;
  const title = g.key === "" ? t("rpcTraceNoProject") : g.key;
  const failed = g.traces.filter((e) => e.status === "error").length;
  const failSuffix = failed > 0 ? ` · ${failed} ${esc(t("rpcTraceFailed"))}` : "";
  return `<details class="rec" data-rpc-project="${esc(g.key)}"${open ? " open" : ""}><summary class="rec-head"><span class="rec-title">${esc(title)}</span><span class="rec-sub">${g.traces.length} ${esc(t("rpcTraceSectionRpc"))} · ${g.hooks.length} ${esc(t("rpcHookSessions"))}${failSuffix}</span><span class="rec-time seconds">${esc(formatLifecycleTime(Math.floor(g.lastTs / 1000)))}</span><button class="btn ghost rpc-export-all" type="button" data-rpc-export-all="${esc(g.key)}">${esc(t("rpcExportAll"))}</button></summary><div class="rec-detail"><p class="settings-group-title">${esc(t("rpcTraceSectionRpc"))}</p>${rpcBody}<p class="settings-group-title">${esc(t("rpcHookSessions"))}</p>${hookBody}</div></details>`;
}

/// Lazy-load wiring for a row's first expand; re-opening is guarded by the row's own dataset.loaded, so it doesn't refetch.
function wireRpcRows(box: HTMLElement): void {
  box.querySelectorAll<HTMLDetailsElement>("details[data-rpc-trace]").forEach((d) => {
    d.addEventListener("toggle", () => {
      if (d.open) void openRpcTraceDetail(d);
    });
  });
  box.querySelectorAll<HTMLDetailsElement>("details[data-hook-trace]").forEach((d) => {
    d.addEventListener("toggle", () => {
      if (d.open) void openHookTraceDetail(d);
    });
  });
}

/// Wiring for the 'Export all' in the project card header: the button is inside <summary>, so the default behavior must be intercepted,
/// otherwise clicking the button also toggles the card. The project is read from data-rpc-export-all, not matched by text.
function wireRpcExportAll(box: HTMLElement): void {
  box.querySelectorAll<HTMLButtonElement>("button[data-rpc-export-all]").forEach((btn) => {
    btn.addEventListener("click", (ev) => {
      ev.preventDefault(); // intercept <summary>'s default toggle
      ev.stopPropagation(); // intercept bubbling to the card handler
      void runRpcExportAll(btn);
    });
  });
}

/// Export execution: cancel = silent; while running disable and swap the text, restore in finally (so errors don't leave it stuck disabled);
/// result goes into the tab's existing #rpc-trace-msg (hint style on success, the existing .cfg-err red text on failure).
async function runRpcExportAll(btn: HTMLButtonElement): Promise<void> {
  let picked: string | null = null;
  try {
    picked = await open({ directory: true, multiple: false });
  } catch {
    /* Dialog unavailable: treat as cancel, silently */
  }
  if (!picked) return;
  const msgAtPick = document.getElementById("rpc-trace-msg");
  const label = btn.textContent ?? "";
  const project = btn.dataset.rpcExportAll ?? "";
  btn.disabled = true;
  btn.textContent = t("rpcExportAllBusy");
  try {
    const r = await invoke<ExportReport>("export_project_chains", { dir: picked, project });
    const msg = rpcMsgNode(msgAtPick);
    if (msg) {
      msg.classList.remove("cfg-err");
      const overwritten = r.overwritten > 0 ? ` · ${t("rpcExportAllOverwritten").replace("{count}", String(r.overwritten))}` : "";
      const failures = r.failures.length > 0 ? ` · ${t("rpcExportAllFailures").replace("{count}", String(r.failures.length))}` : "";
      msg.textContent = t("rpcExportAllDone").replace("{count}", String(r.exported)).replace("{dir}", r.dir) + overwritten + failures;
    }
  } catch (err) {
    const msg = rpcMsgNode(msgAtPick);
    if (msg) {
      const code = typeof err === "string" ? err : String((err as Error).message ?? err);
      msg.textContent = rpcExportErrorText(code);
      msg.classList.add("cfg-err");
    }
  } finally {
    btn.disabled = false;
    btn.textContent = label;
  }
}

/// The message node may be replaced by a tab re-render: prefer the current DOM one, fall back to the captured one (same double-safety as the button).
function rpcMsgNode(captured: HTMLElement | null): HTMLElement | null {
  return (document.getElementById("rpc-trace-msg") ?? captured) as HTMLElement | null;
}

/// Failure code -> text: three known codes are localized; unknown strings pass through as-is (a generic message would swallow the real reason).
function rpcExportErrorText(code: string): string {
  if (code === "no_chains") return t("rpcExportNoChains");
  if (code === "dir_unwritable") return t("rpcExportDirUnwritable");
  if (code.startsWith("io: ")) return `${t("rpcExportIoFailed")}: ${code.slice(4)}`;
  return code;
}

/// Export all projects: pick a directory once, then export project by project in order (a single project's failure doesn't stop the rest).
/// The project list comes from the same grouping (groupRpcProjects, including the '' untagged project), no DOM scanning and no separate logic;
/// concurrency guard + restore the current DOM button by id in finally — re-renders don't leave a stuck disabled state.
async function runRpcExportAllProjects(btn: HTMLButtonElement): Promise<void> {
  if (rpcExportAllProjectsBusy) return;
  rpcExportAllProjectsBusy = true;
  try {
    let picked: string | null = null;
    try {
      picked = await open({ directory: true, multiple: false });
    } catch {
      /* Dialog unavailable: treat as cancel, silently */
    }
    if (!picked) return;
    const msgAtPick = document.getElementById("rpc-trace-msg");
    btn.disabled = true;
    btn.textContent = t("rpcExportAllBusy");
    let exported = 0;
    let overwritten = 0;
    let failures = 0;
    let okProjects = 0;
    let lastError = "";
    const projects = groupRpcProjects(rpcLastTraces, rpcLastHooks).map((g) => g.key);
    for (const project of projects) {
      try {
        const r = await invoke<ExportReport>("export_project_chains", { dir: picked, project });
        exported += r.exported;
        overwritten += r.overwritten;
        failures += r.failures.length;
        okProjects += 1;
      } catch (err) {
        const code = typeof err === "string" ? err : String((err as Error).message ?? err);
        // no_chains = this project has no exportable chains: count as skipped, not failed
        if (code !== "no_chains") lastError = code;
      }
    }
    const msg = rpcMsgNode(msgAtPick);
    if (msg) {
      if (okProjects === 0 && lastError) {
        msg.textContent = rpcExportErrorText(lastError);
        msg.classList.add("cfg-err");
      } else if (okProjects === 0) {
        // Every project was skipped (or there are no projects): report the no-chains text, not an error
        msg.textContent = t("rpcExportNoChains");
        msg.classList.remove("cfg-err");
      } else {
        const overwrittenText = overwritten > 0 ? ` · ${t("rpcExportAllOverwritten").replace("{count}", String(overwritten))}` : "";
        const failuresText = failures > 0 ? ` · ${t("rpcExportAllFailures").replace("{count}", String(failures))}` : "";
        msg.textContent =
          t("rpcExportAllProjectsDone")
            .replace("{projects}", String(okProjects))
            .replace("{count}", String(exported))
            .replace("{dir}", picked) + overwrittenText + failuresText;
        msg.classList.remove("cfg-err");
      }
    }
  } finally {
    rpcExportAllProjectsBusy = false;
    // Re-rendering replaces the button node: restore both the current DOM button and the one that was clicked, for double safety
    const live = document.getElementById("rpc-export-all-projects") as HTMLButtonElement | null;
    for (const b of new Set([live, btn])) {
      if (!b) continue;
      b.disabled = false;
      b.textContent = t("rpcExportAllProjects");
    }
  }
}

/// Status chip: reuses the plugin tab's .plugin-status palette (-running green / -error red / base neutral),
/// mapping the three states ok/error/pending, without inventing new colors.
function rpcStatusChip(status: string): string {
  const cls = status === "ok" ? " plugin-status-running" : status === "error" ? " plugin-status-error" : "";
  const label = status === "ok" ? t("rpcTraceOk") : status === "error" ? t("rpcTraceFailed") : t("rpcTraceRunning");
  return `<span class="plugin-status${cls}">${esc(label)}</span>`;
}

/// /rpc trace row: summary expanded inline; meta carries the agent (rows are no longer grouped by agent, so without it the origin is unknown).
function rpcTraceRow(e: TraceEntry): string {
  // startedAt/endedAt are milliseconds (list_traces takes the row's ts, produced by now_ms), so the unit is ms
  // — consistent with the tree's `dur` of `Nms`; pending (no endedAt) shows ... (not the string undefined).
  const dur = e.endedAt ? `${e.endedAt - e.startedAt}ms` : "…";
  return `<details class="rec" data-rpc-trace="${esc(e.traceId)}" data-rpc-agent="${esc(e.agentId)}"><summary class="rec-head rec-mono"><span class="rec-title">${esc(e.traceId)}</span>${rpcStatusChip(e.status)}<span class="rec-sub">${esc(e.agentId)} · ${e.lineCount} ${esc(t("traceLines"))} · ${esc(formatBytes(e.sizeBytes))} · ${esc(dur)}</span></summary><div class="rec-detail" data-rpc-trace-body></div></details>`;
}

/// hook session row: same as above; hook files have only event rows and no root end -> endedAt is always absent,
/// so the row header shows the last activity time instead (lastTs is ms, formatLifecycleTime takes seconds, so it must be /1000).
function rpcHookRow(e: TraceEntry): string {
  const last = formatLifecycleTime(Math.floor((e.lastTs ?? e.endedAt ?? e.startedAt) / 1000));
  return `<details class="rec" data-hook-trace="${esc(e.traceId)}" data-rpc-agent="${esc(e.agentId)}"><summary class="rec-head rec-mono"><span class="rec-title">${esc(e.traceId)}</span><span class="rec-sub">${esc(e.agentId)} · ${e.lineCount} ${esc(t("traceLines"))} · ${esc(formatBytes(e.sizeBytes))} · ${esc(last)}</span></summary><div class="rec-detail" data-hook-trace-body></div></details>`;
}

/// Whole chain -> pasteable plain text: one line per record, two spaces per indent level, event prefix `· `,
/// attrs as compact JSON; spans without an end render their duration as .... Used by the 'Copy chain' button (pure function, easy to unit test).
function rpcTraceToText(lines: RpcTraceLine[]): string {
  // Same tree building as renderRpcTraceTree: get_rpc_trace returns in reverse order -> first reverse back to ascending time order
  const ordered = [...lines].reverse();
  const starts = new Map<string, Extract<RpcTraceLine, { ev: "start" }>>();
  const ends = new Map<string, Extract<RpcTraceLine, { ev: "end" }>>();
  const events = new Map<string, Extract<RpcTraceLine, { ev: "event" }>[]>();
  for (const l of ordered) {
    if (l.ev === "start") starts.set(l.spanId, l);
    else if (l.ev === "end") ends.set(l.spanId, l);
    else {
      const arr = events.get(l.spanId) ?? [];
      arr.push(l);
      events.set(l.spanId, arr);
    }
  }
  const children = new Map<string, string[]>();
  for (const [id, s] of starts) {
    const key = s.parentId ?? "";
    const arr = children.get(key) ?? [];
    arr.push(id);
    children.set(key, arr);
  }
  const out: string[] = [];
  const walk = (id: string, depth: number): void => {
    const s = starts.get(id);
    if (!s) return;
    const e = ends.get(id);
    const parts = [s.name, e ? `${e.ts - s.ts}ms` : "…"];
    if (e) {
      parts.push(e.status);
      if (e.error) parts.push(e.error);
    }
    parts.push(JSON.stringify(s.attrs));
    out.push(`${"  ".repeat(depth)}${parts.join("  ")}`);
    for (const ev of events.get(id) ?? []) {
      out.push(`${"  ".repeat(depth + 1)}· ${ev.name}  ${JSON.stringify(ev.attrs)}`);
    }
    for (const c of children.get(id) ?? []) walk(c, depth + 1);
  };
  for (const rootId of children.get("") ?? []) walk(rootId, 0);
  return out.join("\n");
}

/// hook session -> plain text: one line per event (name + timestamp + compact JSON), in the same order as the flat detail view.
function hookTraceToText(lines: RpcTraceLine[]): string {
  return lines
    .filter((l): l is Extract<RpcTraceLine, { ev: "event" }> => l.ev === "event")
    // The row's ts is ms and formatLifecycleTime takes seconds — the same /1000 as in the detail render
    .map((l) => `· ${l.name}  ${formatLifecycleTime(Math.floor(l.ts / 1000))}  ${JSON.stringify(l.attrs)}`)
    .join("\n");
}

/// Action row at the bottom of the detail: copy (human-readable text) / export (raw NDJSON for tools).
/// Buttons use .btn.ghost; the keyboard focus ring is provided by .op-content button.btn:focus-visible.
function rpcDetailActions(): string {
  return `<div class="rpc-actions"><button class="btn ghost" type="button" data-rpc-copy>${esc(t("rpcTraceCopy"))}</button><button class="btn ghost" type="button" data-rpc-export>${esc(t("rpcTraceExport"))}</button></div>`;
}

/// Export raw NDJSON: one record per line, in the same order as the on-disk file (the frontend's rows are in ascending time order = file order).
/// Uses Blob + a.click(), consistent with replay / audit export — no clipboard API dependency.
/// Failures outside onDone (Blob/download unavailable) show a failure message instead of staying silent.
function wireRpcExportButton(scope: HTMLElement, fileName: string, buildContent: () => string): void {
  const btn = scope.querySelector<HTMLButtonElement>("[data-rpc-export]");
  if (!btn) return;
  const label = btn.textContent ?? "";
  let restore = 0;
  btn.addEventListener("click", () => {
    let ok = true;
    try {
      const blob = new Blob([buildContent()], { type: "application/x-ndjson" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = fileName;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    } catch {
      ok = false;
    }
    btn.textContent = ok ? t("rpcTraceExported") : t("rpcTraceExportFailed");
    window.clearTimeout(restore);
    restore = window.setTimeout(
      () => {
        btn.textContent = label;
      },
      ok ? 1500 : 2000,
    );
  });
}

/// trace rows -> NDJSON text (one record per line). input is already in file order (the caller reverses back to ascending first),
/// so stringify line by line: JSON.parse/stringify preserves key order, making it byte-identical to the on-disk content.
function linesToNdjson(lines: RpcTraceLine[]): string {
  return lines.map((l) => JSON.stringify(l)).join("\n") + "\n";
}

/// Copy feedback: prefer the app's own clipboard command (navigator.clipboard is often denied in the WebView),
/// and when both levels fail, explicitly show a failure message for 2s then restore — no more silently pretending success. The label is captured during wiring,
/// so repeated clicks don't lock 'Copied/Copy failed' in as the original text.
function wireRpcCopyButton(scope: HTMLElement, buildText: () => string): void {
  const btn = scope.querySelector<HTMLButtonElement>("[data-rpc-copy]");
  if (!btn) return;
  const label = btn.textContent ?? "";
  let restore = 0;
  btn.addEventListener("click", async () => {
    const text = buildText();
    let copied = false;
    try {
      await invoke("ui_clipboard_write", { text });
      copied = true;
    } catch {
      try {
        await navigator.clipboard.writeText(text);
        copied = true;
      } catch {
        copied = false;
      }
    }
    btn.textContent = copied ? t("rpcTraceCopied") : t("rpcTraceCopyFailed");
    window.clearTimeout(restore);
    restore = window.setTimeout(
      () => {
        btn.textContent = label;
      },
      copied ? 1500 : 2000,
    );
  });
}

/// /rpc detail: the span tree is fetched only on first expand (limit 500); on failure the flag is cleared so re-expanding retries.
async function openRpcTraceDetail(d: HTMLDetailsElement): Promise<void> {
  const target = d.querySelector<HTMLElement>("[data-rpc-trace-body]");
  if (!target || target.dataset.loaded === "1") return;
  target.dataset.loaded = "1"; // guards against concurrent double-fetch; cleared on failure
  target.innerHTML = `<p class="list-empty">${esc(t("traceLoading"))}</p>`;
  let lines: RpcTraceLine[] = [];
  try {
    lines = await invoke<RpcTraceLine[]>("get_rpc_trace", { agentId: d.dataset.rpcAgent ?? "", traceId: d.dataset.rpcTrace ?? "", limit: 500 });
  } catch (err) {
    target.dataset.loaded = "";
    target.innerHTML = `<p class="rpc-err">${esc(t("traceLoadFailed"))}: ${esc(String((err as Error).message ?? err))}</p>`;
    return;
  }
  // The API returns newest-first -> reverse back to file order (on disk it's ascending time); export uses this, the tree reverses internally.
  const ordered = [...lines].reverse();
  target.innerHTML = lines.length
    ? `<ul class="trace-lines rpc-tree">${renderRpcTraceTree(lines)}</ul>${rpcDetailActions()}`
    : `<p class="list-empty">${esc(t("traceEmpty"))}</p>`;
  if (lines.length) {
    wireRpcCopyButton(target, () => rpcTraceToText(lines));
    wireRpcExportButton(target, `${d.dataset.rpcTrace ?? "trace"}.ndjson`, () => linesToNdjson(ordered));
  }
}

/// hook session detail: fetch flat events on first expand (limit 300); render only rows where ev === 'event'.
async function openHookTraceDetail(d: HTMLDetailsElement): Promise<void> {
  const target = d.querySelector<HTMLElement>("[data-hook-trace-body]");
  if (!target || target.dataset.loaded === "1") return;
  target.dataset.loaded = "1";
  target.innerHTML = `<p class="list-empty">${esc(t("traceLoading"))}</p>`;
  let lines: RpcTraceLine[] = [];
  try {
    lines = await invoke<RpcTraceLine[]>("get_hook_trace", { agentId: d.dataset.rpcAgent ?? "", sessionId: d.dataset.hookTrace ?? "", limit: 300 });
  } catch (err) {
    target.dataset.loaded = "";
    target.innerHTML = `<p class="rpc-err">${esc(t("traceLoadFailed"))}: ${esc(String((err as Error).message ?? err))}</p>`;
    return;
  }
  // get_hook_trace returns newest-first (read_trace_at reverse order) -> reverse back to ascending time:
  // session logs should be read forward in time, and this stays consistent with the rpc tree (also reversed back to ascending).
  const ordered = [...lines].reverse();
  const rows = ordered
    .map((l) =>
      l.ev === "event"
        // The row's ts is ms (now_ms) and formatLifecycleTime takes seconds — it must be /1000,
        // otherwise feeding ms directly renders as 1970 (measured).
        ? `<li class="rpc-event">· ${esc(l.name)} <span class="trace-ts">${esc(formatLifecycleTime(Math.floor(l.ts / 1000)))}</span> <code class="trace-payload">${esc(JSON.stringify(l.attrs))}</code></li>`
        : "",
    )
    .join("");
  target.innerHTML = rows
    ? `<ul class="trace-lines rpc-tree">${rows}</ul>${rpcDetailActions()}`
    : `<p class="list-empty">${esc(t("traceEmpty"))}</p>`;
  if (rows) {
    wireRpcCopyButton(target, () => hookTraceToText(ordered));
    wireRpcExportButton(target, `${d.dataset.hookTrace ?? "session"}.ndjson`, () => linesToNdjson(ordered));
  }
}

export function renderRpcTrace(body: HTMLElement): void {
  // Request chains: grouped by project, all expanded inline (project bodies come with the full list in one shot), no dialogs.
  body.innerHTML = group("rpcTraceTitle",
    `<div class="setting-row vertical"><span class="setting-hint">${esc(t("rpcTraceTitle"))} · ${esc(t("rpcHookSessions"))}</span><div><button class="btn ghost" id="rpc-trace-refresh" type="button">${esc(t("auditRefresh"))}</button><button class="btn ghost" id="rpc-export-all-projects" type="button">${esc(t("rpcExportAllProjects"))}</button><span class="setting-hint" id="rpc-trace-msg"></span></div><div id="rpc-trace-list" class="rpc-rows"></div></div>`);
  document.getElementById("rpc-trace-refresh")?.addEventListener("click", () => void refreshRpcTraces());
  // Look up the button by id at click time (a tab switch rebuilds this row); don't capture stale references
  document.getElementById("rpc-export-all-projects")?.addEventListener("click", () => {
    const btn = document.getElementById("rpc-export-all-projects") as HTMLButtonElement | null;
    if (btn) void runRpcExportAllProjects(btn);
  });
  // Only here is the enter animation allowed; the refresh button and failure retry use the default (allowEnter=false) and automatically get .rpc-no-enter
  void refreshRpcTraces(true);
}
