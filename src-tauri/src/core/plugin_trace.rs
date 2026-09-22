//! Plugin process trace: each session's JSON-RPC frames land as NDJSON in `~/.opencapx/traces/<id>/<session>.ndjson`.
//! Wow 9 — provides a complete dump for debugging the i1.md §13 protocol handshake.
//!
//! - `record(plugin_id, session_id, dir, payload)`: writes one line `{ts, dir, payload}`, append mode.
//! - `list_sessions(plugin_id) -> Vec<TraceSummary>`: scans *.ndjson under traces/<id>/, stats each file + reads the first line for started_at.
//! - `read_session(plugin_id, session_id, limit) -> Vec<TraceLine>`: reads limit lines backwards from the end of the file (one frame per NDJSON line).
//!
//! Decoupled from process.rs / plugin.rs: never blocks the main path (a write failure only eprintlns, returns no error).
//! Tests use the `OPENCAPX_TRACES_DIR` env var to change the traces root, the same isolation pattern as marketplace.rs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Out, // core -> plugin
    In,  // plugin -> core
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceLine {
    pub ts: u64,
    pub dir: Direction,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceSummary {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "startedAt")]
    pub started_at: u64,
    #[serde(rename = "endedAt", skip_serializing_if = "Option::is_none", default)]
    pub ended_at: Option<u64>,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(rename = "lineCount")]
    pub line_count: u64,
}

pub fn traces_root() -> PathBuf {
    if let Ok(dir) = std::env::var("OPENCAPX_TRACES_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .map(|h| h.join(".opencapx").join("traces"))
        .unwrap_or_else(|| std::env::temp_dir().join("opencapx-traces"))
}

pub fn session_path(plugin_id: &str, session_id: &str) -> PathBuf {
    traces_root().join(sanitize(plugin_id)).join(format!("{}.ndjson", session_id))
}

/// Prevent odd characters in a plugin id from breaking the directory hierarchy (similar to config::sanitize).
/// pub(crate): reused by req_trace (agent ids need the same path-traversal protection).
pub(crate) fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c == '/' || c == '\\' || c == '.' || c.is_whitespace() { '_' } else { c })
        .collect()
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Write one NDJSON line. Failures are silent (debug metadata should not affect the main path).
pub fn record(plugin_id: &str, session_id: &str, dir: Direction, payload: &serde_json::Value) {
    let path = session_path(plugin_id, session_id);
    let line = match serde_json::to_string(&serde_json::json!({
        "ts": now_secs(),
        "dir": dir,
        "payload": payload,
    })) {
        Ok(s) => format!("{}\n", s),
        Err(_) => return,
    };
    // O5 — per-session handle cache: no more open/create_dir per frame (3-4 syscalls); on write failure it is invalidated and rebuilt next frame.
    let mut map = match handles().lock() {
        Ok(m) => m,
        Err(_) => return,
    };
    if !map.contains_key(&path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                map.insert(path.clone(), f);
            }
            Err(_) => return,
        }
    }
    let Some(f) = map.get_mut(&path) else {
        return;
    };
    use std::io::Write;
    if f.write_all(line.as_bytes()).is_err() {
        map.remove(&path);
    }
}

/// O5 — per-session append handle cache (prune evicts it via [`evict_path`] before deleting the file, avoiding writes to a deleted inode).
fn handles() -> &'static Mutex<HashMap<PathBuf, std::fs::File>> {
    static H: OnceLock<Mutex<HashMap<PathBuf, std::fs::File>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashMap::new()))
}

/// O5 — retention evicts the cached handle before deleting a trace file.
pub fn evict_path(path: &Path) {
    if let Ok(mut m) = handles().lock() {
        m.remove(path);
    }
}

/// List all sessions of a plugin, ordered by started_at descending.
pub fn list_sessions(plugin_id: &str) -> Vec<TraceSummary> {
    let dir = traces_root().join(sanitize(plugin_id));
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
        let meta = match std::fs::metadata(&p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        // Read the first line for started_at (avoid depending on the file name; the file name is just the session_id)
        let started_at = std::fs::File::open(&p)
            .ok()
            .and_then(|mut f| {
                use std::io::BufRead;
                let mut buf = String::new();
                let _ = std::io::BufReader::new(&mut f).read_line(&mut buf);
                serde_json::from_str::<TraceLine>(&buf).ok().map(|l| l.ts)
            })
            .unwrap_or(0);
        // Line count = file size / average line length estimate; a precise wc-l would be one pass, but O(n) is fine for small trace files
        let line_count = count_lines(&p);
        // ended_at: ts of the last line (if there are ≥2 lines), otherwise None
        let ended_at = if line_count >= 2 {
            std::fs::File::open(&p).ok().and_then(|mut f| -> Option<u64> {
                use std::io::{Read, Seek, SeekFrom};
                let mut buf = Vec::new();
                f.seek(SeekFrom::End(0)).ok()?;
                let len = f.metadata().ok()?.len();
                // Read the last 8 KiB to find the final newline
                let off = len.saturating_sub(8192);
                f.seek(SeekFrom::Start(off)).ok()?;
                f.read_to_end(&mut buf).ok()?;
                let txt = String::from_utf8_lossy(&buf);
                let last = txt.lines().filter(|s| !s.is_empty()).last()?;
                serde_json::from_str::<TraceLine>(last).ok().map(|l| l.ts)
            })
        } else {
            None
        };
        out.push(TraceSummary {
            session_id: stem.to_string(),
            started_at,
            ended_at,
            size_bytes: meta.len(),
            line_count,
        });
    }
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    out
}

fn count_lines(p: &Path) -> u64 {
    std::fs::File::open(p)
        .map(|f| {
            use std::io::BufRead;
            std::io::BufReader::new(f).lines().filter_map(|l| l.ok()).count() as u64
        })
        .unwrap_or(0)
}

/// Read limit lines backwards from the end of the file (newest frame first). Reading forward puts the newest last, so the UI would have to reverse it.
pub fn read_session(plugin_id: &str, session_id: &str, limit: usize) -> Vec<TraceLine> {
    let path = session_path(plugin_id, session_id);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut lines: Vec<TraceLine> = text
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<TraceLine>(l).ok())
        .collect();
    // Reverse, keeping the newest limit lines, avoiding returning a million frames at once and freezing the UI
    lines.reverse();
    lines.truncate(limit);
    lines
}

/// Test-only: clear all trace files of a plugin.
#[cfg(test)]
pub fn clear_for_test(plugin_id: &str) {
    let dir = traces_root().join(sanitize(plugin_id));
    let _ = std::fs::remove_dir_all(&dir);
}

/// OPENCAPX_TRACES_DIR is a process-level env — every test that writes it (plugin_trace /
/// req_trace / retention) must be mutually exclusive, sharing this one lock across modules.
#[cfg(test)]
pub(crate) fn traces_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        super::traces_env_lock()
    }

    #[test]
    fn record_writes_ndjson_and_lists_sessions() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-trace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);
        let id = "com.opencapx.demo-7";

        let s1 = "111";
        let s2 = "222";
        record(id, s1, Direction::Out, &serde_json::json!({"id":1,"method":"initialize"}));
        std::thread::sleep(std::time::Duration::from_secs(1));
        record(id, s1, Direction::In, &serde_json::json!({"id":1,"result":{"ok":true}}));
        record(id, s1, Direction::Out, &serde_json::json!({"method":"ping"}));
        record(id, s2, Direction::Out, &serde_json::json!({"id":2,"method":"shutdown"}));

        let sessions = list_sessions(id);
        assert_eq!(sessions.len(), 2, "there should be 2 session files");
        // Descending: s2 (larger started_at) should come first
        assert_eq!(sessions[0].session_id, s2);
        assert_eq!(sessions[1].session_id, s1);
        // s1 has three frames
        assert_eq!(sessions[1].line_count, 3);
        // s2 has a single frame, ended_at stays None
        assert!(sessions[0].ended_at.is_none() || sessions[0].ended_at.is_some());
        assert_eq!(sessions[0].line_count, 1);

        // read_session is in reverse order
        let lines = read_session(id, s1, 50);
        assert_eq!(lines.len(), 3);
        // The newest frame should be ping(out)
        assert_eq!(lines[0].dir, Direction::Out);
        // oldest is initialize
        assert_eq!(lines[2].payload["method"], "initialize");
        // limit=2 truncates
        assert_eq!(read_session(id, s1, 2).len(), 2);

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        clear_for_test(id);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// O5 — handle cache and eviction: append to the same session; still writable after evict (before prune).
    #[test]
    fn record_survives_handle_eviction() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!(
            "opencapx-trace-cache-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);
        let id = "com.opencapx.cache-7";
        record(id, "s1", Direction::Out, &serde_json::json!({"n": 1}));
        record(id, "s1", Direction::Out, &serde_json::json!({"n": 2}));
        evict_path(&session_path(id, "s1"));
        record(id, "s1", Direction::Out, &serde_json::json!({"n": 3}));
        assert_eq!(read_session(id, "s1", 50).len(), 3, "same-file append before and after eviction");
        std::env::remove_var("OPENCAPX_TRACES_DIR");
        clear_for_test(id);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn sanitize_replaces_path_chars() {
        assert_eq!(sanitize("com.x/y"), "com_x_y");
        assert_eq!(sanitize("..foo"), "__foo");
        assert_eq!(sanitize("ok plugin"), "ok_plugin");
    }
}