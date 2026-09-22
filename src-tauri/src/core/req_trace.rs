//! /rpc request-chain trace: one span tree per Agent tool call, NDJSON persisted to
//! `~/.opencapx/traces/rpc/<agent_id>/<trace_id>.ndjson`。
//!
//! Lightweight self-built span model (no OTel SDK, 0 dependencies):
//! - two-line protocol (span start/end) + event lines, append-only; in-flight requests are visible (start with no end)
//! - thread-local context: http.rs spawns a separate thread per /rpc, giving a natural scope
//!   (Rust std threads have no ALS, so thread-local carries the scope)
//! - with no trace context (tests/direct internal calls) everything is a no-op
//! - write failures are silent (same discipline as plugin_trace: debugging metadata does not affect the main path)
//!
//! Chain shape:
//! rpc (root, attrs: agent/conn/project + durMs on the end line)
//! ├── event: dispatch {tool, requestId, input preview}
//! ├── event: permission.agent {permission, granted}
//! ├── capability.<name>
//! │   ├── event: gate.denied {pluginId, permission}
//! │   └── plugin.<pluginId> {sessionId ← links to a plugin_trace dump, elapsedMs}
//! ├── event: ask.shown / ask.answered / ask.timeout / ask.cancelled
//! └── end line {status, error}

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpanStatus {
    Ok,
    Error,
}

/// Single-line NDJSON, the `ev` tag distinguishes the three states; camelCase aligns with the frontend interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "lowercase")]
pub enum RpcTraceLine {
    Start {
        #[serde(rename = "spanId")]
        span_id: String,
        #[serde(rename = "parentId", default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        name: String,
        ts: u64,
        attrs: Value,
    },
    Event {
        #[serde(rename = "spanId")]
        span_id: String,
        name: String,
        ts: u64,
        attrs: Value,
    },
    End {
        #[serde(rename = "spanId")]
        span_id: String,
        ts: u64,
        status: SpanStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attrs: Option<Value>,
    },
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Trace root directory: `<traces_root>/rpc/`. Separated from plugin trace by one level:
/// retention's prune_traces_at only scans the direct ndjson inside the first-level subdirectories of traces_root,
/// so the rpc subtree is not mistakenly cleaned; prune_traces() explicitly runs again over rpc/ (Task 3).
pub fn rpc_traces_root() -> PathBuf {
    super::plugin_trace::traces_root().join("rpc")
}

fn trace_path(agent_id: &str, trace_id: &str) -> PathBuf {
    rpc_traces_root()
        .join(super::plugin_trace::sanitize(agent_id))
        .join(format!("{}.ndjson", trace_id))
}

/// Same as O5: each file caches a handle; a write failure invalidates it and it is rebuilt on the next frame.
fn handles() -> &'static Mutex<HashMap<PathBuf, std::fs::File>> {
    static H: OnceLock<Mutex<HashMap<PathBuf, std::fs::File>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashMap::new()))
}

/// retention removes the file from the cached handle before deleting it (prevents appending to a deleted inode).
pub fn evict_path(path: &Path) {
    if let Ok(mut m) = handles().lock() {
        m.remove(path);
    }
}

/// Write one line of NDJSON. Any failure returns silently.
fn write_line(path: &PathBuf, line: &RpcTraceLine) {
    let Ok(s) = serde_json::to_string(line) else { return };
    let mut map = match handles().lock() {
        Ok(m) => m,
        Err(_) => return,
    };
    if !map.contains_key(path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(f) => {
                map.insert(path.clone(), f);
            }
            Err(_) => return,
        }
    }
    let Some(f) = map.get_mut(path) else { return };
    use std::io::Write;
    if f.write_all(format!("{}\n", s).as_bytes()).is_err() {
        map.remove(path);
    }
}

/// thread-local trace context. stack[0] is always the root span ("s0"); child spans are pushed,
/// and popped back to their own position on end (nesting imbalance defense: intermediate unclosed spans are truncated directly).
struct Ctx {
    path: PathBuf,
    stack: Vec<String>,
    next_span: u64,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = RefCell::new(None);
}

/// Enter the trace context of one /rpc request (called at the /rpc thread entry in http.rs).
/// Writes the root span start (name "rpc") and returns the trace_id (also the file name).
/// `project` = the short path of the CLI's project (the project::short_path form); "" = unknown,
/// and the viewer groups it as ungrouped when grouping by project (traces persisted before this change are all "").
pub fn begin(agent_id: &str, conn_id: &str, project: &str) -> String {
    let trace_id = format!("rpc-{}", nanos());
    let path = trace_path(agent_id, &trace_id);
    write_line(
        &path,
        &RpcTraceLine::Start {
            span_id: "s0".into(),
            parent_id: None,
            name: "rpc".into(),
            ts: now_ms(),
            attrs: json!({ "agent": agent_id, "conn": conn_id, "project": project }),
        },
    );
    CTX.with(|c| {
        *c.borrow_mut() = Some(Ctx { path, stack: vec!["s0".into()], next_span: 1 });
    });
    trace_id
}

/// Open a child span (parent = stack top). No trace context → a no-op Span with an empty id.
pub fn span(name: &str, attrs: Value) -> Span {
    let id = CTX.with(|c| {
        let mut b = c.borrow_mut();
        let Some(ctx) = b.as_mut() else { return String::new() };
        let id = format!("s{}", ctx.next_span);
        ctx.next_span += 1;
        let parent = ctx.stack.last().cloned();
        write_line(
            &ctx.path,
            &RpcTraceLine::Start {
                span_id: id.clone(),
                parent_id: parent,
                name: name.to_string(),
                ts: now_ms(),
                attrs,
            },
        );
        ctx.stack.push(id.clone());
        id
    });
    Span { id, _not_send: PhantomData }
}

/// Explicitly managed span: end consumes ownership, mem::take clears the id so the Drop fallback no longer fires.
/// _not_send: CTX is thread-local, so a Span moved across threads would end on the wrong thread and silently
/// drop the line — use !Send to block it at compile time (a bare PhantomData pointer takes no space).
pub struct Span {
    id: String,
    _not_send: PhantomData<*mut ()>,
}

impl Span {
    pub fn end(mut self, ok: bool, error: Option<&str>, attrs: Value) {
        let id = std::mem::take(&mut self.id);
        end_span(&id, ok, error, attrs);
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        // panic / early return fallback: a span not explicitly ended is recorded as error, not left dangling.
        if !self.id.is_empty() {
            end_span(&self.id, false, Some("dropped without end"), json!({}));
        }
    }
}

fn end_span(id: &str, ok: bool, error: Option<&str>, attrs: Value) {
    if id.is_empty() {
        return;
    }
    CTX.with(|c| {
        let mut b = c.borrow_mut();
        let Some(ctx) = b.as_mut() else { return };
        if let Some(pos) = ctx.stack.iter().rposition(|s| s == id) {
            ctx.stack.truncate(pos);
        }
        write_line(
            &ctx.path,
            &RpcTraceLine::End {
                span_id: id.to_string(),
                ts: now_ms(),
                status: if ok { SpanStatus::Ok } else { SpanStatus::Error },
                error: error.map(String::from),
                attrs: if attrs.as_object().map(|o| o.is_empty()).unwrap_or(false) {
                    None
                } else if attrs.is_null() {
                    None
                } else {
                    Some(attrs)
                },
            },
        );
    });
}

/// Add an event line to the top-of-stack span (an instantaneous step with no duration: permission decision, dispatch, ask result).
pub fn event(name: &str, attrs: Value) {
    CTX.with(|c| {
        let b = c.borrow();
        let Some(ctx) = b.as_ref() else { return };
        let Some(top) = ctx.stack.last() else { return };
        write_line(
            &ctx.path,
            &RpcTraceLine::Event {
                span_id: top.clone(),
                name: name.to_string(),
                ts: now_ms(),
                attrs,
            },
        );
    });
}

/// End the trace: all remaining unclosed spans are recorded as error (a pending chain is not left dangling), then write the root end and clear the context.
pub fn finish(ok: bool, error: Option<&str>, attrs: Value) {
    CTX.with(|c| {
        let mut b = c.borrow_mut();
        let Some(mut ctx) = b.take() else { return };
        while ctx.stack.len() > 1 {
            if let Some(id) = ctx.stack.pop() {
                write_line(
                    &ctx.path,
                    &RpcTraceLine::End {
                        span_id: id,
                        ts: now_ms(),
                        status: SpanStatus::Error,
                        error: Some("unfinished at trace end".into()),
                        attrs: None,
                    },
                );
            }
        }
        let root = ctx.stack[0].clone();
        write_line(
            &ctx.path,
            &RpcTraceLine::End {
                span_id: root,
                ts: now_ms(),
                status: if ok { SpanStatus::Ok } else { SpanStatus::Error },
                error: error.map(String::from),
                attrs: if attrs.as_object().map(|o| o.is_empty()).unwrap_or(false) {
                    None
                } else if attrs.is_null() {
                    None
                } else {
                    Some(attrs)
                },
            },
        );
    });
}

/// input preview truncation (byte cap, backing off to a char boundary).
pub fn trunc_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…[truncated]", &s[..cut])
}

/// viewer summary: one line per trace file (shown in the settings agents tab).
#[derive(Debug, Clone, Serialize)]
pub struct RpcTraceSummary {
    #[serde(rename = "traceId")]
    pub trace_id: String,
    #[serde(rename = "startedAt")]
    pub started_at: u64,
    #[serde(rename = "endedAt", skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(rename = "lineCount")]
    pub line_count: u64,
}

fn line_ts(line: &RpcTraceLine) -> u64 {
    match line {
        RpcTraceLine::Start { ts, .. } | RpcTraceLine::Event { ts, .. } | RpcTraceLine::End { ts, .. } => *ts,
    }
}

/// Take the owning project from a line's attrs; missing field/non-string → "" (old trace unknown).
fn project_from_line(line: &RpcTraceLine) -> String {
    let attrs = match line {
        RpcTraceLine::Start { attrs, .. } | RpcTraceLine::Event { attrs, .. } => attrs,
        RpcTraceLine::End { attrs, .. } => attrs.as_ref().unwrap_or(&Value::Null),
    };
    attrs
        .get("project")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// List all traces of an agent, started_at descending (newest first).
/// Same as plugin_trace::list_sessions: scan the directory + read the first line for started_at.
/// ended_at exists only when the last line is the **root span's end** (spanId=="s0") — otherwise the request
/// is pending (e.g. ask waiting on the user), and the viewer needs to distinguish "in progress" from "finished".
pub fn list_traces(agent_id: &str) -> Vec<RpcTraceSummary> {
    list_traces_at(&rpc_traces_root(), agent_id)
}

/// Shared body with hook trace: the hook side has a different root directory, everything else is identical.
fn list_traces_at(root: &Path, agent_id: &str) -> Vec<RpcTraceSummary> {
    let dir = root.join(super::plugin_trace::sanitize(agent_id));
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("ndjson") {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(meta) = std::fs::metadata(&p) else { continue };
        let mut started_at = 0u64;
        let mut ended_at = None;
        let mut count = 0u64;
        let mut last: Option<RpcTraceLine> = None;
        if let Ok(text) = std::fs::read_to_string(&p) {
            for l in text.lines().filter(|l| !l.is_empty()) {
                let Ok(line) = serde_json::from_str::<RpcTraceLine>(l) else { continue };
                if started_at == 0 {
                    started_at = line_ts(&line);
                }
                last = Some(line);
                count += 1;
            }
        }
        if let Some(RpcTraceLine::End { span_id, ts, .. }) = &last {
            if span_id == "s0" {
                ended_at = Some(*ts);
            }
        }
        out.push(RpcTraceSummary {
            trace_id: stem.to_string(),
            started_at,
            ended_at,
            size_bytes: meta.len(),
            line_count: count,
        });
    }
    // started_at has only millisecond precision: multiple requests within the same millisecond tie, and the viewer list order follows
    // read_dir jitter (observed two adjacent traces differing by only 0.43ms). trace_id is `rpc-<nanos>`,
    // which is itself the real creation order — use it as a secondary key, giving a deterministic and correct order.
    out.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.trace_id.cmp(&a.trace_id))
    });
    out
}

/// Read limit lines in reverse (newest first). A pending request writes start/event last, so reverse order sees them first.
pub fn read_trace(agent_id: &str, trace_id: &str, limit: usize) -> Vec<RpcTraceLine> {
    read_trace_at(&rpc_traces_root(), agent_id, trace_id, limit)
}

/// file_stem goes through sanitize, matching the persisted name of hook_path (the hook's session_id is an arbitrary
/// string given by the host). For rpc's `rpc-<nanos>` it is a no-op.
fn read_trace_at(root: &Path, agent_id: &str, file_stem: &str, limit: usize) -> Vec<RpcTraceLine> {
    let path = root
        .join(super::plugin_trace::sanitize(agent_id))
        .join(format!("{}.ndjson", super::plugin_trace::sanitize(file_stem)));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut lines: Vec<RpcTraceLine> = text
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<RpcTraceLine>(l).ok())
        .collect();
    lines.reverse();
    lines.truncate(limit);
    lines
}

/// hook trace root directory: traces/hooks/<agent_id>/<session_id>.ndjson.
/// Parallel to rpc/ — retention runs the same policy on each (first-level subdirectory = agent).
pub fn hook_traces_root() -> PathBuf {
    super::plugin_trace::traces_root().join("hooks")
}

fn hook_path(agent_id: &str, session_id: &str) -> PathBuf {
    hook_traces_root()
        .join(super::plugin_trace::sanitize(agent_id))
        .join(format!("{}.ndjson", super::plugin_trace::sanitize(session_id)))
}

/// hook event persistence: called by event::ingest, independent of the /rpc thread-local context
/// (ingest runs on the /event main loop thread). spanId is always "s0", no parent/child — the viewer flattens it.
/// The hook-side event name directly uses the normalized agent.* (the output of agent_event_type).
pub fn hook_event(agent_id: &str, session_id: &str, name: &str, attrs: Value) {
    let path = hook_path(agent_id, session_id);
    write_line(
        &path,
        &RpcTraceLine::Event {
            span_id: "s0".into(),
            name: name.to_string(),
            ts: now_ms(),
            attrs,
        },
    );
}

pub fn list_hook_traces(agent_id: &str) -> Vec<RpcTraceSummary> {
    list_traces_at(&hook_traces_root(), agent_id)
}

pub fn read_hook_trace(agent_id: &str, session_id: &str, limit: usize) -> Vec<RpcTraceLine> {
    read_trace_at(&hook_traces_root(), agent_id, session_id, limit)
}

/// One viewer line: one request chain or one hook session, with ownership (agent + project).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEntry {
    pub agent_id: String,
    /// The project name in short_path form; "" = unknown (traces persisted before this change).
    pub project: String,
    pub trace_id: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub size_bytes: u64,
    pub line_count: u64,
    /// The root span's end status: "ok" / "error"; "" = no root end yet (request pending / hook session).
    pub status: String,
    /// The timestamp of the last line (the trace's last activity; for a pending request it is the newest line).
    pub last_ts: u64,
}

/// Scan all agent directories under one trace tree (rpc/ or hooks/), one line per ndjson file.
/// Returns (file mtime seconds, TraceEntry): the hook side has no root end, so mtime defines "newest first".
/// is_rpc: true takes project from the first line (rpc root start); false takes it from the first event line (hook has no start).
fn list_all_in(root: &Path, is_rpc: bool) -> Vec<(u64, TraceEntry)> {
    let Ok(agents) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for agent in agents.flatten() {
        let dir = agent.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(agent_id) = dir.file_name().and_then(|s| s.to_str()) else { continue };
        let Ok(files) = std::fs::read_dir(&dir) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("ndjson") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            let Ok(meta) = std::fs::metadata(&p) else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let mut started_at = 0u64;
            let mut ended_at = None;
            let mut count = 0u64;
            let mut project = String::new();
            let mut project_seen = false;
            let mut last: Option<RpcTraceLine> = None;
            let mut status = String::new();
            for l in text.lines().filter(|l| !l.is_empty()) {
                let Ok(line) = serde_json::from_str::<RpcTraceLine>(l) else { continue };
                if started_at == 0 {
                    started_at = line_ts(&line);
                }
                let project_line = if is_rpc {
                    matches!(line, RpcTraceLine::Start { .. })
                } else {
                    matches!(line, RpcTraceLine::Event { .. })
                };
                if !project_seen && project_line {
                    project = project_from_line(&line);
                    project_seen = true;
                }
                if let RpcTraceLine::End { span_id, status: s, .. } = &line {
                    if span_id == "s0" {
                        status = match s {
                            SpanStatus::Ok => "ok".into(),
                            SpanStatus::Error => "error".into(),
                        };
                    }
                }
                last = Some(line);
                count += 1;
            }
            if let Some(RpcTraceLine::End { span_id, ts, .. }) = &last {
                if span_id == "s0" {
                    ended_at = Some(*ts);
                }
            }
            let last_ts = last.as_ref().map(line_ts).unwrap_or(0);
            out.push((
                mtime,
                TraceEntry {
                    agent_id: agent_id.to_string(),
                    project,
                    trace_id: stem.to_string(),
                    started_at,
                    ended_at,
                    size_bytes: meta.len(),
                    line_count: count,
                    status,
                    last_ts,
                },
            ));
        }
    }
    out
}

/// Full enumeration of the rpc tree (all agents), started_at descending, ties broken by trace_id descending.
pub fn list_all_traces() -> Vec<TraceEntry> {
    let mut out: Vec<TraceEntry> = list_all_in(&rpc_traces_root(), true)
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    // Same deterministic ordering as list_traces: when milliseconds tie, the trace_id (`rpc-<nanos>`) is the real creation order.
    out.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.trace_id.cmp(&a.trace_id))
    });
    out
}

/// Full enumeration of the hooks tree, newest first (hook files have no root end, sorted by file mtime).
pub fn list_all_hook_sessions() -> Vec<TraceEntry> {
    let mut pairs = list_all_in(&hook_traces_root(), false);
    // mtime has second granularity so ties occur: add trace_id/agent_id ordering to avoid read_dir jitter.
    pairs.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.trace_id.cmp(&a.1.trace_id))
            .then_with(|| b.1.agent_id.cmp(&a.1.agent_id))
    });
    pairs.into_iter().map(|(_, e)| e).collect()
}

/// Overview of agents that have traces (the viewer's "Request Chains" tab uses it to list all agents).
/// Does not read the agents table: traces should remain visible after revocation/deletion — the directory itself is the source of truth.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceAgentSummary {
    pub agent_id: String,
    pub rpc_count: u64,
    pub hook_count: u64,
    /// The newest file mtime (seconds) across the agent's two trees, used for sorting.
    pub last_ts: u64,
}

/// Under one agent directory: (ndjson file count, newest mtime seconds).
fn dir_stats(dir: &Path) -> (u64, u64) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut n = 0u64;
    let mut last = 0u64;
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("ndjson") {
            continue;
        }
        n += 1;
        if let Ok(md) = e.metadata().and_then(|m| m.modified()) {
            let secs = md
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            last = last.max(secs);
        }
    }
    (n, last)
}

fn collect_agents_at(
    root: &Path,
    into: &mut std::collections::BTreeMap<String, (u64, u64, u64)>,
    is_rpc: bool,
) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(name) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let (count, last) = dir_stats(&dir);
        if count == 0 {
            continue;
        }
        let e = into.entry(name.to_string()).or_insert((0, 0, 0));
        if is_rpc {
            e.0 += count;
        } else {
            e.1 += count;
        }
        e.2 = e.2.max(last);
    }
}

pub fn list_trace_agents() -> Vec<TraceAgentSummary> {
    let mut map: std::collections::BTreeMap<String, (u64, u64, u64)> =
        std::collections::BTreeMap::new();
    collect_agents_at(&rpc_traces_root(), &mut map, true);
    collect_agents_at(&hook_traces_root(), &mut map, false);
    let mut out: Vec<TraceAgentSummary> = map
        .into_iter()
        .map(|(agent_id, (rpc_count, hook_count, last_ts))| TraceAgentSummary {
            agent_id,
            rpc_count,
            hook_count,
            last_ts,
        })
        .collect();
    // Most recently active first; ties broken by id to avoid order jitter
    out.sort_by(|a, b| b.last_ts.cmp(&a.last_ts).then_with(|| a.agent_id.cmp(&b.agent_id)));
    out
}

/// One written request chain in the export manifest (frontend contract, camelCase fields).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedChain {
    pub trace_id: String,
    /// The file name in the target directory (file name only, e.g. `rpc-1789576401787063000.ndjson`).
    pub file: String,
    pub agent_id: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub line_count: u64,
    pub size_bytes: u64,
    pub status: String,
}

/// Record of a single chain copy failure: not silent, returned per item so the frontend can notify.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFailure {
    pub trace_id: String,
    pub reason: String,
}

/// Project export report. Error code convention: `no_chains` / `dir_unwritable` / `io: <detail>`,
/// for frontend localization (see export_chains_to).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    pub dir: String,
    pub project: String,
    /// Number of files successfully written.
    pub exported: u64,
    /// Of these, the number of files that already existed before overwrite (pre-overwrite probe).
    pub overwritten: u64,
    /// `<dir>/index.json`。
    pub manifest_path: String,
    pub chains: Vec<ExportedChain>,
    pub failures: Vec<ExportFailure>,
}

/// Copy all /rpc request chains of a project **raw bytes** to `<dir>/<sanitize(project)>/`
/// (using `_no-project` when the project string is empty), and write the index.json manifest in that subdirectory.
/// Reuses the viewer's enumeration path (list_all_in) + project_from_line ownership decision, guaranteeing the export
/// matches what the user sees; hook traces are out of scope. Copies one by one; a single failure is recorded in failures and it continues.
/// Error codes: no chains → "no_chains"; cannot create directory → "dir_unwritable"; otherwise → "io: <detail>".
pub fn export_chains_to(dir: &str, project: &str) -> Result<ExportReport, String> {
    // Same function as the viewer's full enumeration: rpc tree, grouped by the project of the first line's root start.
    let selected: Vec<(u64, TraceEntry)> = list_all_in(&rpc_traces_root(), true)
        .into_iter()
        .filter(|(_, e)| e.project == project)
        .collect();
    if selected.is_empty() {
        return Err("no_chains".into());
    }
    // The project name goes through sanitize as a whole, yielding a **single-level** directory name (not split on /, no nesting).
    // The project string may be "" (old traces before the project header): if empty after sanitize, use a stable ASCII fallback name.
    let project_dir_name = {
        let s = super::plugin_trace::sanitize(project);
        if s.is_empty() {
            "_no-project".to_string()
        } else {
            s
        }
    };
    // First locate the actual write directory `<dir>/<project-dir-name>`, and only create the directory after validating the chain list
    // (no_chains already early-returns above, so no empty directory is left); if creation fails, writing is impossible.
    let out_dir = Path::new(dir).join(&project_dir_name);
    if std::fs::create_dir_all(&out_dir).is_err() {
        return Err("dir_unwritable".into());
    }

    let mut chains = Vec::with_capacity(selected.len());
    let mut failures = Vec::new();
    let mut overwritten = 0u64;
    for (_, e) in selected {
        // Source path same as read_trace_at: both the agent/trace segments go through sanitize.
        let src = rpc_traces_root()
            .join(super::plugin_trace::sanitize(&e.agent_id))
            .join(format!("{}.ndjson", super::plugin_trace::sanitize(&e.trace_id)));
        let file = format!("{}.ndjson", super::plugin_trace::sanitize(&e.trace_id));
        let dst = out_dir.join(&file);
        // Pre-overwrite probe: existing files count toward overwritten (only counted for successfully written files).
        let pre_existing = dst.exists();
        // Byte-level copy (no JSON round-trip): std::fs::copy.
        match std::fs::copy(&src, &dst) {
            Ok(_) => {
                if pre_existing {
                    overwritten += 1;
                }
                chains.push(ExportedChain {
                    trace_id: e.trace_id,
                    file,
                    agent_id: e.agent_id,
                    started_at: e.started_at,
                    ended_at: e.ended_at,
                    line_count: e.line_count,
                    size_bytes: e.size_bytes,
                    status: e.status,
                });
            }
            Err(err) => failures.push(ExportFailure { trace_id: e.trace_id, reason: err.to_string() }),
        }
    }

    let exported = chains.len() as u64;
    let manifest_path = out_dir.join("index.json");
    let manifest = json!({
        "schema": 1,
        "exportedAt": now_ms(),
        "project": project,
        "chainCount": exported,
        "chains": chains.clone(),
    });
    let manifest_text =
        serde_json::to_string_pretty(&manifest).map_err(|e| format!("io: {}", e))?;
    std::fs::write(&manifest_path, manifest_text).map_err(|e| format!("io: {}", e))?;

    Ok(ExportReport {
        dir: out_dir.to_string_lossy().into_owned(),
        project: project.to_string(),
        exported,
        overwritten,
        manifest_path: manifest_path.to_string_lossy().into_owned(),
        chains,
        failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Process-level env, sharing one lock with the plugin_trace / retention tests (see above).
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        super::super::plugin_trace::traces_env_lock()
    }

    /// Read the whole trace file and parse line by line (same parsing as the debugging view).
    fn read_all(agent: &str, trace: &str) -> Vec<RpcTraceLine> {
        let text = std::fs::read_to_string(trace_path(agent, trace)).unwrap();
        text.lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str::<RpcTraceLine>(l).ok())
            .collect()
    }

    #[test]
    fn full_chain_writes_start_event_end_lines() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        let tid = begin("ag_x_01", "conn-7", "");
        event("dispatch", serde_json::json!({ "tool": "opencapx.say" }));
        let sp = span("capability.opencapx.execute", serde_json::json!({ "capability": "opencapx.execute" }));
        event("gate.denied", serde_json::json!({ "pluginId": "com.demo" }));
        sp.end(true, None, serde_json::json!({ "elapsedMs": 12 }));
        finish(true, None, serde_json::json!({ "durMs": 20 }));

        let lines = read_all("ag_x_01", &tid);
        assert_eq!(lines.len(), 6, "start+event+start+event+end+end is 6 lines in total");
        // Root start: no parent, name=rpc, with agent/conn
        match &lines[0] {
            RpcTraceLine::Start { span_id, parent_id, name, attrs, .. } => {
                assert_eq!(span_id, "s0");
                assert!(parent_id.is_none());
                assert_eq!(name, "rpc");
                assert_eq!(attrs["agent"], "ag_x_01");
                assert_eq!(attrs["conn"], "conn-7");
            }
            other => panic!("line0 should be Start, actual {:?}", other),
        }
        // The dispatch event on the root hangs off s0
        match &lines[1] {
            RpcTraceLine::Event { span_id, name, .. } => {
                assert_eq!(span_id, "s0");
                assert_eq!(name, "dispatch");
            }
            other => panic!("line1 should be Event, actual {:?}", other),
        }
        // Child span parent = s0
        match &lines[2] {
            RpcTraceLine::Start { span_id, parent_id, .. } => {
                assert_eq!(span_id, "s1");
                assert_eq!(parent_id.as_deref(), Some("s0"));
            }
            other => panic!("line2 should be Start, actual {:?}", other),
        }
        // The event on the child span hangs off s1
        match &lines[3] {
            RpcTraceLine::Event { span_id, name, .. } => {
                assert_eq!(span_id, "s1");
                assert_eq!(name, "gate.denied");
            }
            other => panic!("line3 should be Event, actual {:?}", other),
        }
        // child end ok
        match &lines[4] {
            RpcTraceLine::End { span_id, status, .. } => {
                assert_eq!(span_id, "s1");
                assert_eq!(*status, SpanStatus::Ok);
            }
            other => panic!("line4 should be End, actual {:?}", other),
        }
        // root end ok + durMs
        match &lines[5] {
            RpcTraceLine::End { span_id, status, attrs, .. } => {
                assert_eq!(span_id, "s0");
                assert_eq!(*status, SpanStatus::Ok);
                assert_eq!(attrs.as_ref().unwrap()["durMs"], 20);
            }
            other => panic!("line5 should be End, actual {:?}", other),
        }

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn no_context_all_calls_are_noop() {
        // Hold the lock: rpc_traces_root() reads env; without the lock parallel tests could redirect the root and drift assertions
        let _g = lock_env();
        let sp = span("orphan", serde_json::json!({}));
        event("ev", serde_json::json!({}));
        finish(true, None, serde_json::Value::Null);
        sp.end(true, None, serde_json::Value::Null); // empty id, should be skipped internally
        assert!(rpc_traces_root().join("ag_none").read_dir().is_err());
    }

    #[test]
    fn dropped_span_writes_error_end() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        let tid = begin("ag_x_02", "", "");
        {
            let _sp = span("plugin.demo", serde_json::json!({})); // no explicit end
        } // Drop triggers the fallback
        finish(true, None, serde_json::Value::Null);

        let lines = read_all("ag_x_02", &tid);
        // root start + child start + child end (error, dropped) + root end
        match &lines[2] {
            RpcTraceLine::End { span_id, status, error, .. } => {
                assert_eq!(span_id, "s1");
                assert_eq!(*status, SpanStatus::Error);
                assert_eq!(error.as_deref(), Some("dropped without end"));
            }
            other => panic!("line2 should be End, actual {:?}", other),
        }

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn list_and_read_traces_roundtrip() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        let t1 = begin("ag_list", "", "");
        finish(true, None, serde_json::Value::Null);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let t2 = begin("ag_list", "", "");
        finish(false, Some("boom"), serde_json::Value::Null);
        // Pending: only root start, no end — ended_at must be None (so the viewer can distinguish "in progress")
        let t3 = begin("ag_list", "", "");

        let list = list_traces("ag_list");
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].trace_id, t3, "newest first");
        assert!(list[0].ended_at.is_none(), "a pending trace's ended_at should be None");
        assert!(list[1].ended_at.is_some(), "a finished trace should have ended_at");
        assert!(list[1].line_count >= 2);
        assert!(list[2].size_bytes > 0);

        let lines = read_trace("ag_list", &t1, 50);
        assert!(lines.len() >= 2);
        // Reverse order: the last line (root end) first
        assert!(matches!(lines[0], RpcTraceLine::End { .. }));
        // limit takes effect
        assert!(read_trace("ag_list", &t1, 1).len() == 1);

        // Other agents are mutually invisible
        assert!(list_traces("ag_other").is_empty());

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// hook events are persisted directly to a file (no thread-local context), and the session file accumulates per event.
    #[test]
    fn hook_event_appends_without_ctx() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        // Do not call begin — hook_event must work independently of the /rpc context
        hook_event("ag_h", "sess-a1", "agent.started", serde_json::json!({ "message": "prompt" }));
        hook_event("ag_h", "sess-a1", "agent.completed", serde_json::json!({}));
        hook_event("ag_h", "sess-b2", "agent.started", serde_json::json!({}));

        let list = list_hook_traces("ag_h");
        assert_eq!(list.len(), 2, "the two hook sessions each have one file");
        assert!(list[0].line_count >= 1);
        let lines = read_hook_trace("ag_h", "sess-a1", 50);
        assert_eq!(lines.len(), 2);
        // read_*_at reverse order (newest first): [0] = the later-written completed, [1] = the earlier-written started.
        // spanId is always "s0" (a uniform viewer shape, no parent/child)
        assert!(matches!(&lines[0], RpcTraceLine::Event { span_id, name, .. } if span_id == "s0" && name == "agent.completed"));
        assert!(matches!(&lines[1], RpcTraceLine::Event { span_id, name, .. } if span_id == "s0" && name == "agent.started"));

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// tab data source: merge counts from the rpc and hooks trees, newest activity descending; agents with no files are not listed.
    #[test]
    fn list_trace_agents_merges_rpc_and_hooks() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        finish(true, None, serde_json::Value::Null); // clear any leftover context
        begin("ag_a", "", "");
        finish(true, None, serde_json::Value::Null);
        begin("ag_a", "", "");
        finish(true, None, serde_json::Value::Null);
        begin("ag_b", "", "");
        finish(true, None, serde_json::Value::Null);
        hook_event("ag_a", "s1", "agent.started", serde_json::json!({}));
        hook_event("ag_c", "s1", "agent.started", serde_json::json!({}));

        let list = list_trace_agents();
        assert_eq!(list.len(), 3, "the three agents each have a trace");
        let find = |id: &str| list.iter().find(|x| x.agent_id == id).unwrap();
        assert_eq!(find("ag_a").rpc_count, 2);
        assert_eq!(find("ag_a").hook_count, 1);
        assert_eq!(find("ag_b").rpc_count, 1);
        assert_eq!(find("ag_b").hook_count, 0);
        assert_eq!(find("ag_c").rpc_count, 0);
        assert_eq!(find("ag_c").hook_count, 1);
        // Reverse order: last_ts is non-increasing
        assert!(list.windows(2).all(|w| w[0].last_ts >= w[1].last_ts));

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn trunc_str_cuts_on_char_boundary() {
        assert_eq!(trunc_str("abc", 10), "abc");
        let s = "aあbあcあ";
        let out = trunc_str(s, 4); // "aあ" = 4 bytes, the next 'b' is exactly on the boundary
        assert!(out.starts_with("aあ"));
        assert!(out.ends_with("…[truncated]"));
    }

    /// The root span carries the project reported by the CLI; the viewer groups by it.
    #[test]
    fn root_span_carries_project() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        let tid = begin("ag_p", "c9", "proj/x");
        finish(true, None, serde_json::Value::Null);

        let lines = read_all("ag_p", &tid);
        match &lines[0] {
            RpcTraceLine::Start { attrs, .. } => {
                assert_eq!(attrs["project"], "proj/x");
                assert_eq!(attrs["agent"], "ag_p");
                assert_eq!(attrs["conn"], "c9");
            }
            other => panic!("line0 should be Start, actual {:?}", other),
        }

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Full enumeration: each request chain across agents carries agent_id + project; an old trace with no project → "".
    #[test]
    fn list_all_traces_carries_agent_and_project() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        let t1 = begin("ag_pa", "", "proj/a");
        finish(true, None, serde_json::Value::Null);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let t2 = begin("ag_pb", "", ""); // old-style persistence: no project
        finish(true, None, serde_json::Value::Null);

        let all = list_all_traces();
        assert_eq!(all.len(), 2, "the two agents each have one trace");
        assert_eq!(all[0].trace_id, t2, "started_at descending: newest first");
        assert_eq!(all[0].agent_id, "ag_pb");
        assert_eq!(all[0].project, "", "a trace missing the project field is grouped as unknown");
        assert_eq!(all[1].trace_id, t1);
        assert_eq!(all[1].agent_id, "ag_pa");
        assert_eq!(all[1].project, "proj/a");

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// viewer summary status/last_ts: finished → "ok" + last line ts;
    /// begin without finish (request pending) → "" + the root start ts.
    #[test]
    fn list_all_traces_carries_status_and_last_ts() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        let done = begin("ag_st", "", "");
        finish(true, None, serde_json::Value::Null);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let hanging = begin("ag_st", "", ""); // no finish: root end missing

        let all = list_all_traces();
        let find = |tid: &str| all.iter().find(|e| e.trace_id == tid).unwrap();
        let done_entry = find(&done);
        assert_eq!(done_entry.status, "ok");
        assert_eq!(
            done_entry.last_ts,
            line_ts(read_all("ag_st", &done).last().unwrap()),
            "last_ts = the ts of the last line (root end)"
        );
        let hang_entry = find(&hanging);
        assert_eq!(hang_entry.status, "", "no root end while pending");
        assert_eq!(
            hang_entry.last_ts,
            line_ts(read_all("ag_st", &hanging).last().unwrap()),
            "pending last_ts = the root start ts"
        );

        finish(true, None, serde_json::Value::Null); // clear the thread-local context
        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// hooks full enumeration: each hook session file carries its project (written by event.rs into hook attrs).
    #[test]
    fn list_all_hook_sessions_carries_project() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        hook_event("ag_ha", "sess-1", "agent.started", serde_json::json!({ "project": "proj/h" }));
        hook_event("ag_hb", "sess-2", "agent.started", serde_json::json!({}));

        let all = list_all_hook_sessions();
        assert_eq!(all.len(), 2, "the two hook sessions each have one file");
        let find = |sid: &str| all.iter().find(|e| e.trace_id == sid).unwrap();
        assert_eq!(find("sess-1").agent_id, "ag_ha");
        assert_eq!(find("sess-1").project, "proj/h");
        assert_eq!(find("sess-2").agent_id, "ag_hb");
        assert_eq!(find("sess-2").project, "", "a hook session with no project is grouped as unknown");

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Export: raw-byte copy of same-project chains + index.json manifest; a rerun counts overwritten; no chains reports no_chains.
    #[test]
    fn export_chains_copies_raw_bytes_and_writes_manifest() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let out = std::env::temp_dir().join(format!("opencapx-reqexport-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&out);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        // Two chains in the same project; one in another project to verify filtering.
        let t1 = begin("ag_ex", "c1", "proj/exp");
        event("dispatch", serde_json::json!({ "tool": "opencapx.say" }));
        finish(true, None, serde_json::Value::Null);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let t2 = begin("ag_ex", "c2", "proj/exp");
        finish(false, Some("boom"), serde_json::Value::Null);
        let t3 = begin("ag_ex", "c3", "proj/other");
        finish(true, None, serde_json::Value::Null);

        let report = export_chains_to(out.to_str().unwrap(), "proj/exp").unwrap();
        assert_eq!(report.exported, 2, "only the two chains of proj/exp are exported");
        assert_eq!(report.overwritten, 0);
        assert!(report.failures.is_empty());
        assert_eq!(report.project, "proj/exp");
        assert!(report.chains.iter().all(|c| c.trace_id == t1 || c.trace_id == t2));
        assert!(report.chains.iter().any(|c| c.trace_id == t1 && c.status == "ok"));
        assert!(report.chains.iter().any(|c| c.trace_id == t2 && c.status == "error"));

        // Persisted at the single-level subdirectory `<dir>/<sanitize(project)>/`.
        let proj_dir = out.join("proj_exp");
        assert_eq!(report.dir, proj_dir.to_string_lossy().to_string(), "report.dir points at the actually written subdirectory");

        // The directory holds exactly as many ndjson files as same-project chains.
        let ndjson: Vec<_> = std::fs::read_dir(&proj_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("ndjson"))
            .collect();
        assert_eq!(ndjson.len(), 2, "the two same-project chains each have one file, excluding proj/other");

        // Byte-level identity: source file == target file (no JSON round-trip).
        for tid in [&t1, &t2] {
            let src = trace_path("ag_ex", tid);
            let dst = proj_dir.join(format!("{}.ndjson", tid));
            assert_eq!(
                std::fs::read(&src).unwrap(),
                std::fs::read(&dst).unwrap(),
                "{} should be byte-identical",
                tid
            );
        }
        // t3 belongs to another project, not persisted.
        assert!(!proj_dir.join(format!("{}.ndjson", t3)).exists());

        // index.json is parseable, with schema/chainCount/project/chains correct.
        let manifest_path = proj_dir.join("index.json");
        assert_eq!(report.manifest_path, manifest_path.to_string_lossy().to_string());
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["schema"], 1);
        assert_eq!(manifest["chainCount"], 2);
        assert_eq!(manifest["project"], "proj/exp");
        assert_eq!(manifest["chains"].as_array().unwrap().len(), 2);

        // Rerun: both already exist → overwritten == exported.
        let again = export_chains_to(out.to_str().unwrap(), "proj/exp").unwrap();
        assert_eq!(again.exported, 2);
        assert_eq!(again.overwritten, 2, "both already existed on rerun");

        // A project with no chains → no_chains, and it **does not touch the filesystem**: neither the selection directory nor nested subdirectories are created.
        let untouched = std::env::temp_dir().join(format!("opencapx-reqexport-nc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&untouched);
        assert_eq!(
            export_chains_to(untouched.to_str().unwrap(), "proj/none").unwrap_err(),
            "no_chains"
        );
        assert!(!untouched.exists(), "a no_chains early return must not leave any directory behind");

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&out);
        let _ = std::fs::remove_dir_all(&untouched);
    }

    /// project == "" → use the stable ASCII fallback directory name `_no-project` (no localized label).
    #[test]
    fn export_chains_empty_project_falls_back_to_no_project_dir() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let out = std::env::temp_dir().join(format!("opencapx-reqexport-np-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&out);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        let tid = begin("ag_np", "c1", "");
        event("dispatch", serde_json::json!({ "tool": "opencapx.say" }));
        finish(true, None, serde_json::Value::Null);

        let report = export_chains_to(out.to_str().unwrap(), "").unwrap();
        assert_eq!(report.exported, 1);
        assert_eq!(report.project, "");
        let proj_dir = out.join("_no-project");
        assert_eq!(report.dir, proj_dir.to_string_lossy().to_string());
        assert!(proj_dir.join(format!("{}.ndjson", tid)).exists(), "the chain lands under the _no-project level");
        assert_eq!(report.manifest_path, proj_dir.join("index.json").to_string_lossy().to_string());
        assert!(proj_dir.join("index.json").exists());

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// dir points at an existing file (directory creation must fail) → dir_unwritable.
    #[test]
    fn export_chains_reports_dir_unwritable() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
        let bogus = std::env::temp_dir().join(format!("opencapx-reqexport-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_file(&bogus);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        let tid = begin("ag_du", "", "proj/du");
        finish(true, None, serde_json::Value::Null);
        assert!(!tid.is_empty());
        std::fs::write(&bogus, b"x").unwrap();

        assert_eq!(
            export_chains_to(bogus.to_str().unwrap(), "proj/du").unwrap_err(),
            "dir_unwritable"
        );
        // The nested target directory name likewise goes through sanitize, and no directory is created on failure.
        assert!(!bogus.join("proj_du").exists());

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_file(&bogus);
    }
}
