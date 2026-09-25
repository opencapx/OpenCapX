//! cross-agent listing: all traces/hook sessions, per-agent summaries, dir stats.
//! Mechanical move from core/req_trace.rs.

use super::*;
use serde::{Deserialize, Serialize};

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
pub(crate) fn list_all_in(root: &Path, is_rpc: bool) -> Vec<(u64, TraceEntry)> {
    let Ok(agents) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for agent in agents.flatten() {
        let dir = agent.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(agent_id) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("ndjson") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(meta) = std::fs::metadata(&p) else {
                continue;
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let mut started_at = 0u64;
            let mut ended_at = None;
            let mut count = 0u64;
            let mut project = String::new();
            let mut project_seen = false;
            let mut last: Option<RpcTraceLine> = None;
            let mut status = String::new();
            for l in text.lines().filter(|l| !l.is_empty()) {
                let Ok(line) = serde_json::from_str::<RpcTraceLine>(l) else {
                    continue;
                };
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
                if let RpcTraceLine::End {
                    span_id, status: s, ..
                } = &line
                {
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
        .map(
            |(agent_id, (rpc_count, hook_count, last_ts))| TraceAgentSummary {
                agent_id,
                rpc_count,
                hook_count,
                last_ts,
            },
        )
        .collect();
    // Most recently active first; ties broken by id to avoid order jitter
    out.sort_by(|a, b| {
        b.last_ts
            .cmp(&a.last_ts)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    });
    out
}
