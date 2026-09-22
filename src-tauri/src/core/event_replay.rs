//! Phase 34 — Event stream recording and replay.
//!
//! Design: `spawn_subscribers` spawns one extra thread at startup; every OpencapxEvent the EventBus receives
//! is appended as one NDJSON line to `~/.opencapx/replay/<session>.ndjson` (env `OPENCAPX_REPLAY_DIR`
//! overrides). session id = the unix-second timestamp when spawn_subscribers started.
//!
//! - `record(e)` appends one line; write failures are a silent eprintln (same pattern as plugin_trace; recording is metadata and must not block the main path)
//! - `list_sessions()` scans the directory, reading each file's first + last line for started/ended
//! - `read_session(sid, limit)` reads in reverse + truncates
//! - `replay_to_bus(sid, filter_kind, bus)` reads NDJSON in order and, after filtering, re-publishes via `bus.publish`
//!   (SSE emit + SQLite rewrite; the rewrite is side-effect-free because log_event uses a uuid id primary key)
//! - `export(sid, dst_path)` copies the whole NDJSON to a target path (for frontend download via file URL)

use super::event::{OpencapxEvent, EventBus};
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct ReplaySession {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "startedAt")]
    pub started_at: u64,
    #[serde(rename = "endedAt")]
    pub ended_at: Option<u64>,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(rename = "lineCount")]
    pub line_count: u64,
}

/// Returns `~/.opencapx/replay/`, overridable by the `OPENCAPX_REPLAY_DIR` env.
fn replay_dir() -> PathBuf {
    if let Ok(p) = std::env::var("OPENCAPX_REPLAY_DIR") {
        return PathBuf::from(p);
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".opencapx").join("replay");
    }
    std::env::temp_dir().join("opencapx-replay")
}

/// Current session id (static): the second timestamp at process start.
fn current_session_id() -> &'static str {
    static SID: OnceLock<String> = OnceLock::new();
    SID.get_or_init(|| {
        let secs = now_secs();
        // Add 4 hex digits of randomness to avoid same-second restart collisions (matching plugin_trace::start_inner)
        let suffix: u32 = std::process::id();
        format!("{}-{:04x}", secs, suffix & 0xFFFF)
    })
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// NDJSON file path for the current session (creates the parent directory).
fn current_file() -> PathBuf {
    let path = replay_dir().join(format!("{}.ndjson", current_session_id()));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    path
}

/// Record one event. Thread-safe (append mode).
pub fn record(e: &OpencapxEvent) {
    let path = current_file();
    let line = match serde_json::to_string(e) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("[event_replay] serialize: {}", err);
            return;
        }
    };
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(err) => {
            eprintln!("[event_replay] open {}: {}", path.display(), err);
            return;
        }
    };
    if let Err(err) = writeln!(file, "{}", line) {
        eprintln!("[event_replay] write: {}", err);
    }
}

/// List all recorded sessions (by started_at descending).
pub fn list_sessions() -> Vec<ReplaySession> {
    let dir = replay_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<ReplaySession> = rd
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("ndjson") {
                return None;
            }
            let session_id = path.file_stem()?.to_string_lossy().to_string();
            let meta = entry.metadata().ok()?;
            let size_bytes = meta.len();
            // First line = started_at (the ts field is already set when we record), last line = last ts
            let file = std::fs::File::open(&path).ok()?;
            let reader = BufReader::new(file);
            let mut started_at: Option<u64> = None;
            let mut ended_at: Option<u64> = None;
            let mut line_count: u64 = 0;
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(e) = serde_json::from_str::<OpencapxEvent>(&line) {
                    if started_at.is_none() {
                        started_at = Some(e.timestamp);
                    }
                    ended_at = Some(e.timestamp);
                    line_count += 1;
                }
            }
            Some(ReplaySession {
                session_id,
                started_at: started_at.unwrap_or(0),
                ended_at,
                size_bytes,
                line_count,
            })
        })
        .collect();
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    out
}

/// Read a session's events, in reverse + truncated to limit.
/// Returned events are in chronological order (reversed once more after reading) — the UI expects chronological.
pub fn read_session(session_id: &str, limit: usize) -> Vec<OpencapxEvent> {
    let path = replay_dir().join(format!("{}.ndjson", session_id));
    let Ok(file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut events: Vec<OpencapxEvent> = reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<OpencapxEvent>(&line).ok())
        .collect();
    events.reverse(); // Read in reverse
    events.truncate(limit.min(5000));
    events.reverse(); // Restore chronological order
    events
}

/// Re-publish a session's events to the EventBus by filter (then SSE emit + store persistence).
/// Returns the number of events successfully re-published.
pub fn replay_to_bus(session_id: &str, filter_kind: Option<&str>, bus: &EventBus) -> usize {
    let path = replay_dir().join(format!("{}.ndjson", session_id));
    let Ok(file) = std::fs::File::open(&path) else {
        return 0;
    };
    let reader = BufReader::new(file);
    let mut count = 0usize;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(e) = serde_json::from_str::<OpencapxEvent>(&line) else { continue };
        if let Some(f) = filter_kind {
            if !f.is_empty() && !e.kind.starts_with(f) {
                continue;
            }
        }
        bus.publish(&e);
        count += 1;
    }
    count
}

/// Copy the whole NDJSON to the target path, returning bytes written (0 on failure).
pub fn export(session_id: &str, dst: &std::path::Path) -> u64 {
    let src = replay_dir().join(format!("{}.ndjson", session_id));
    let Ok(bytes) = std::fs::copy(&src, dst) else {
        return 0;
    };
    bytes
}

/// For tests: clear the entire replay directory.
pub fn clear_for_test() {
    let dir = replay_dir();
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test parallel safety: the `OPENCAPX_REPLAY_DIR` env is process-level and tests stomp on each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn ev(kind: &str, ts: u64) -> OpencapxEvent {
        let mut e = OpencapxEvent::new(kind, "core", serde_json::json!({"x": 1}));
        e.timestamp = ts;
        e
    }

    #[test]
    fn record_writes_ndjson_and_lists_sessions() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Isolated directory
        let dir = std::env::temp_dir().join(format!("opencapx-replay-{}", std::process::id()));
        std::env::set_var("OPENCAPX_REPLAY_DIR", &dir);
        clear_for_test();
        // Simulate spawn_subscribers' two parallel recording threads
        record(&ev("plugin.lifecycle.starting", 100));
        record(&ev("plugin.lifecycle.running", 101));
        record(&ev("capability.completed", 200));
        // Listing
        let sessions = list_sessions();
        assert_eq!(sessions.len(), 1, "1 session file");
        let s = &sessions[0];
        assert_eq!(s.line_count, 3);
        assert_eq!(s.started_at, 100);
        assert_eq!(s.ended_at, Some(200));
        assert!(s.size_bytes > 0);
        // read_session reverse read + truncate + restore chronological order
        let events = read_session(&s.session_id, 100);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, "plugin.lifecycle.starting");
        assert_eq!(events[2].kind, "capability.completed");
        // Smaller limit: limit=2 takes the most recent 2 (order preserved)
        let events2 = read_session(&s.session_id, 2);
        assert_eq!(events2.len(), 2, "limit=2 must return the 2 most recent");
        assert_eq!(events2[0].kind, "plugin.lifecycle.running");
        assert_eq!(events2[1].kind, "capability.completed");
        // Clean up env + directory
        std::env::remove_var("OPENCAPX_REPLAY_DIR");
        clear_for_test();
    }

    #[test]
    fn replay_to_bus_filters_by_kind() {
        use std::sync::{Arc, Mutex};
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-replay-flt-{}", std::process::id()));
        std::env::set_var("OPENCAPX_REPLAY_DIR", &dir);
        clear_for_test();
        record(&ev("plugin.lifecycle.starting", 100));
        record(&ev("capability.completed", 200));
        record(&ev("capability.failed", 300));
        // Get the current session id via list_sessions
        let sid = list_sessions()[0].session_id.clone();
        // Subscribe to the EventBus (a fresh instance avoids the shared() singleton being polluted by other parallel tests)
        let bus = EventBus::new();
        let rx = bus.subscribe();
        let got = Arc::new(Mutex::new(Vec::<String>::new()));
        let got2 = got.clone();
        let collector = std::thread::spawn(move || {
            for e in rx {
                got2.lock().unwrap().push(e.kind);
            }
        });
        // No filter: should receive 3
        let total = replay_to_bus(&sid, None, &bus);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(total, 3);
        let got_total = got.lock().unwrap().len();
        assert_eq!(got_total, 3);
        // Filter capability.* : should receive only 2
        let cap_only = replay_to_bus(&sid, Some("capability."), &bus);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(cap_only, 2);
        let final_count = got.lock().unwrap().len();
        // Should have added 2 (the previous 3 + these 2)
        assert_eq!(final_count, 5);
        drop(collector);
        std::env::remove_var("OPENCAPX_REPLAY_DIR");
        clear_for_test();
    }
}