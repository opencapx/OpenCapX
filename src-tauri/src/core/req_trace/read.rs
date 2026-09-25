//! reading single traces back: summaries, RPC and hook trace readers.
//! Mechanical move from core/req_trace.rs.

use super::*;
use serde::{Deserialize, Serialize};

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

pub(crate) fn line_ts(line: &RpcTraceLine) -> u64 {
    match line {
        RpcTraceLine::Start { ts, .. }
        | RpcTraceLine::Event { ts, .. }
        | RpcTraceLine::End { ts, .. } => *ts,
    }
}

/// Take the owning project from a line's attrs; missing field/non-string → "" (old trace unknown).
pub(crate) fn project_from_line(line: &RpcTraceLine) -> String {
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
    let dir = root.join(crate::core::plugin_trace::sanitize(agent_id));
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("ndjson") {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        let mut started_at = 0u64;
        let mut ended_at = None;
        let mut count = 0u64;
        let mut last: Option<RpcTraceLine> = None;
        if let Ok(text) = std::fs::read_to_string(&p) {
            for l in text.lines().filter(|l| !l.is_empty()) {
                let Ok(line) = serde_json::from_str::<RpcTraceLine>(l) else {
                    continue;
                };
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
        .join(crate::core::plugin_trace::sanitize(agent_id))
        .join(format!(
            "{}.ndjson",
            crate::core::plugin_trace::sanitize(file_stem)
        ));
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
    crate::core::plugin_trace::traces_root().join("hooks")
}

fn hook_path(agent_id: &str, session_id: &str) -> PathBuf {
    hook_traces_root()
        .join(crate::core::plugin_trace::sanitize(agent_id))
        .join(format!(
            "{}.ndjson",
            crate::core::plugin_trace::sanitize(session_id)
        ))
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
