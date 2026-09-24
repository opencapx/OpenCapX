//! the recent-events ring buffer and the route cycle linter.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Phase 68 — Maximum entries in the in-memory ring buffer (beyond this, pop_front the old ones).
pub const RECENT_EVENTS_CAP: usize = 256;

/// Phase 68 — event snapshot for one fanout (written to the ring for seen_in_last queries + timeline display).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteSeenEvent {
    pub id: String,
    pub source: String,
    /// short summary of the payload (truncated to 64 chars).
    pub payload_summary: String,
    pub ts_secs: u64,
    pub routes_fired: Vec<String>,
    pub correlations_hit: Vec<String>,
}

#[derive(Debug, Default)]
struct RecentEventsBuffer {
    pub(crate) events: std::collections::VecDeque<RouteSeenEvent>,
}

static RECENT_EVENTS: std::sync::OnceLock<std::sync::Mutex<RecentEventsBuffer>> =
    std::sync::OnceLock::new();

fn recent_events_buffer() -> &'static std::sync::Mutex<RecentEventsBuffer> {
    RECENT_EVENTS.get_or_init(|| std::sync::Mutex::new(RecentEventsBuffer::default()))
}

/// Phase 68 — push a seen event into the ring; pop_front when capacity is exceeded.
pub fn record_seen_event(
    source: &str,
    payload_summary: &str,
    ts_secs: u64,
    routes_fired: Vec<String>,
    correlations_hit: Vec<String>,
) {
    let mut buf = match recent_events_buffer().lock() {
        Ok(b) => b,
        Err(_) => return,
    };
    if buf.events.len() >= RECENT_EVENTS_CAP {
        buf.events.pop_front();
    }
    buf.events.push_back(RouteSeenEvent {
        id: gen_event_id(),
        source: source.to_string(),
        payload_summary: payload_summary.chars().take(64).collect(),
        ts_secs,
        routes_fired,
        correlations_hit,
    });
}

/// Phase 68 — query the ring: whether an event matching `pattern` occurred within the past `window_secs`.
/// `window_secs == 0` is always false (spec validation already forbids 0, but this is a fallback).
pub fn recently_seen(pattern: &str, window_secs: u64, now_secs: u64) -> bool {
    if window_secs == 0 || pattern.is_empty() {
        return false;
    }
    let buf = match recent_events_buffer().lock() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let cutoff = now_secs.saturating_sub(window_secs);
    buf.events
        .iter()
        .rev()
        .any(|e| e.ts_secs >= cutoff && kind_matches(pattern, &e.source))
}

/// Phase 68 — Read the most recent N events (new→old).
pub fn recent_events_snapshot(limit: usize) -> Vec<RouteSeenEvent> {
    let limit = limit.min(RECENT_EVENTS_CAP);
    let buf = match recent_events_buffer().lock() {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    buf.events.iter().rev().take(limit).cloned().collect()
}

/// Phase 68 — Clear the ring buffer (for tests).
pub fn _reset_recent_events_for_tests() {
    if let Ok(mut buf) = recent_events_buffer().lock() {
        buf.events.clear();
    }
}

/// Phase 68 — Cycle type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleKind {
    SelfLoop,
    RouteToRoute,
    RouteToCorrelation,
}

/// Phase 68 — Cycle report (`cycle` is the node-name sequence, with identical first and last to close it).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleReport {
    pub cycle: Vec<String>,
    pub kind: CycleKind,
}

/// Phase 68 — Detect cycles in the alerting rule graph (route + correlation).
/// Nodes: `<route.name>` and `<correlation.name>`.
/// Edges:
///   - R1 → R2 when R1.seen_in_last.pattern globs onto R2.kind_pattern
///   - R1 → C1 when R1.kind_pattern globs onto C1.kind_pattern_a
///   - C1 → R1 when C1.kind_pattern_b globs onto R1.kind_pattern
pub fn detect_route_cycles() -> Vec<CycleReport> {
    use std::collections::{HashMap, HashSet};

    let routes: Vec<RouteRule> = list_routes()
        .into_iter()
        .filter(|r| r.enabled)
        .map(|r| route_row_to_dto(&r))
        .collect();
    let corrs = load_enabled_correlation_rules();
    let r_names: Vec<String> = routes.iter().map(|r| r.name.clone()).collect();
    let c_names: Vec<String> = corrs.iter().map(|c| c.name.clone()).collect();
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for r1 in &routes {
        if let Some(spec) = &r1.seen_in_last {
            for r2 in &routes {
                // do not dedupe self-edges (keep them → so self-loop detection can find them)
                if kind_matches(&spec.pattern, &r2.kind_pattern) {
                    adj.entry(r1.name.clone())
                        .or_default()
                        .push(r2.name.clone());
                }
            }
        }
        for c in &corrs {
            if kind_matches(&r1.kind_pattern, &c.kind_pattern_a) {
                adj.entry(r1.name.clone()).or_default().push(c.name.clone());
            }
        }
    }
    for c in &corrs {
        for r in &routes {
            if kind_matches(&c.kind_pattern_b, &r.kind_pattern) {
                adj.entry(c.name.clone()).or_default().push(r.name.clone());
            }
        }
    }
    // include isolated nodes
    let mut nodes: HashSet<String> = HashSet::new();
    for n in &r_names {
        nodes.insert(n.clone());
    }
    for n in &c_names {
        nodes.insert(n.clone());
    }
    for (k, vs) in &adj {
        nodes.insert(k.clone());
        for v in vs {
            nodes.insert(v.clone());
        }
    }

    let mut color: HashMap<String, u8> = nodes.iter().map(|n| (n.clone(), 0u8)).collect();
    let mut stack: Vec<String> = Vec::new();
    let mut reports: Vec<CycleReport> = Vec::new();

    // list self-loops separately
    let mut self_seen: HashSet<String> = HashSet::new();
    for (from, tos) in &adj {
        for to in tos {
            if from == to && self_seen.insert(from.clone()) {
                reports.push(CycleReport {
                    cycle: vec![from.clone(), from.clone()],
                    kind: CycleKind::SelfLoop,
                });
            }
        }
    }

    fn dfs(
        node: &str,
        adj: &HashMap<String, Vec<String>>,
        color: &mut HashMap<String, u8>,
        stack: &mut Vec<String>,
        reports: &mut Vec<CycleReport>,
    ) {
        color.insert(node.to_string(), 1); // GRAY
        stack.push(node.to_string());
        if let Some(neighbors) = adj.get(node) {
            for n in neighbors {
                let c = *color.get(n).unwrap_or(&0);
                if c == 1 {
                    // cycle found: from n's position in the stack to the end + close with n
                    if let Some(pos) = stack.iter().position(|x| x == n) {
                        let mut cycle = stack[pos..].to_vec();
                        cycle.push(n.to_string());
                        reports.push(CycleReport {
                            cycle,
                            kind: CycleKind::RouteToRoute,
                        });
                    }
                } else if c == 0 {
                    dfs(n, adj, color, stack, reports);
                }
            }
        }
        color.insert(node.to_string(), 2); // BLACK
        stack.pop();
    }

    // sort before dfs to guarantee deterministic report order
    let mut sorted_nodes: Vec<String> = nodes.into_iter().collect();
    sorted_nodes.sort();
    for n in &sorted_nodes {
        if *color.get(n).unwrap_or(&0) == 0 {
            dfs(n, &adj, &mut color, &mut stack, &mut reports);
        }
    }
    // dedupe: dedupe the cycle sequences + stable ordering
    reports.sort_by(|a, b| a.cycle.cmp(&b.cycle));
    reports.dedup_by(|a, b| a.cycle == b.cycle);
    reports
}
