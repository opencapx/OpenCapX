//! the append-only trace writer: file handles, eviction, line writes.
//! Mechanical move from core/req_trace.rs.

use super::*;

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Trace root directory: `<traces_root>/rpc/`. Separated from plugin trace by one level:
/// retention's prune_traces_at only scans the direct ndjson inside the first-level subdirectories of traces_root,
/// so the rpc subtree is not mistakenly cleaned; prune_traces() explicitly runs again over rpc/ (Task 3).
pub fn rpc_traces_root() -> PathBuf {
    crate::core::plugin_trace::traces_root().join("rpc")
}

pub(crate) fn trace_path(agent_id: &str, trace_id: &str) -> PathBuf {
    rpc_traces_root()
        .join(crate::core::plugin_trace::sanitize(agent_id))
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
pub(crate) fn write_line(path: &PathBuf, line: &RpcTraceLine) {
    let Ok(s) = serde_json::to_string(line) else {
        return;
    };
    let mut map = match handles().lock() {
        Ok(m) => m,
        Err(_) => return,
    };
    if !map.contains_key(path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
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
