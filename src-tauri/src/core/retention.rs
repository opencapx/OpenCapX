//! S2 — trace/log retention policy: disk usage has an upper bound.
//!
//! - traces(`traces_root/<plugin>/<session>.ndjson`): per plugin keep the **most recent 20** sessions
//!   **and** no older than **14 days**; when the total exceeds **50MB**, delete from oldest.
//! - logs(`logs_root/<id>.log`): over **10MB** rotate `.log → .log.1 → .log.2`, keeping 2 backups.
//! - Trigger: once at startup + every 24h (`spawn_retention_worker`); delete failures are a silent eprintln,
//!   never blocking the main path (matching the event_replay style).
//! - Tests override the root paths with `OPENCAPX_TRACES_DIR` / `OPENCAPX_LOGS_DIR`.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

pub const TRACE_MAX_SESSIONS: usize = 20;
pub const TRACE_MAX_AGE_DAYS: u64 = 14;
pub const TRACE_MAX_BYTES: u64 = 50 * 1024 * 1024;
pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const LOG_BACKUPS: usize = 2;
/// P1 — screenshot cache retention: most recent 20 ∩ within 7 days (vision writes ~6MB each time; grow-only would fill the disk).
pub const SCREEN_KEEP: usize = 20;
pub const SCREEN_MAX_AGE_DAYS: u64 = 7;

/// Plugin log root. Defaults to `~/.opencapx/logs/plugins`; tests can override with `OPENCAPX_LOGS_DIR`;
/// process.rs stderr landing and rotation share this single resolution, avoiding path drift between the two.
pub fn logs_root() -> PathBuf {
    if let Ok(dir) = std::env::var("OPENCAPX_LOGS_DIR") {
        return PathBuf::from(dir);
    }
    crate::core::home_dir()
        .map(|h| h.join(".opencapx").join("logs").join("plugins"))
        .unwrap_or_else(|| std::env::temp_dir().join("opencapx-plugin-logs"))
}

/// crash.log cap: over the limit, rotate `.1` first then append (keep 1 backup; tighter than plugin logs,
/// a single panic record should stand out, not drown in history).
pub const CRASH_LOG_MAX_BYTES: u64 = 1024 * 1024;

/// Core panic record file. Defaults to `~/.opencapx/logs/crash.log` (same root level as logs_root,
/// not under plugins/ — it records the host, not plugins); tests can set `OPENCAPX_CRASH_LOG`
/// to point directly at a file path.
pub fn crash_log_path() -> PathBuf {
    if let Ok(p) = std::env::var("OPENCAPX_CRASH_LOG") {
        return PathBuf::from(p);
    }
    crate::core::home_dir()
        .map(|h| h.join(".opencapx").join("logs").join("crash.log"))
        .unwrap_or_else(|| std::env::temp_dir().join("opencapx-crash.log"))
}

/// Append one crash record (already formatted, with newline). Write failures are silent — panicking
/// again inside a panic hook is pointless. Over the cap, `crash.log → crash.log.1` replaces the old backup.
pub fn append_crash(path: &std::path::Path, record: &str) {
    use std::io::Write;
    let Ok(meta) = std::fs::metadata(path) else {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(path));
        let _ = std::fs::write(path, record.as_bytes());
        return;
    };
    if meta.len() >= CRASH_LOG_MAX_BYTES {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = f.write_all(record.as_bytes());
}

/// Run once at startup + every 24h (reuses the background-thread pattern, same as the SLA monitor).
pub fn spawn_retention_worker() {
    std::thread::spawn(|| loop {
        let (traces, logs) = prune_all();
        // O1 — the events table is bounded in the same pass: the desktop stays resident without restart, clearing only on open is not enough.
        let events = super::shared_store()
            .map(|s| super::storage::prune_events_shared(&s))
            .unwrap_or(0);
        // P1 — the screenshot cache is bounded in the same pass (this machine once measured 216MB, all large screenshots).
        let screens = super::vision::cache_dir()
            .map(|d| prune_screen_captures_at(&d))
            .unwrap_or(0);
        if traces > 0 || logs > 0 || events > 0 || screens > 0 {
            eprintln!(
                "[retention] pruned traces={} rotated_logs={} events={} screens={}",
                traces, logs, events, screens
            );
        }
        std::thread::sleep(Duration::from_secs(24 * 3600));
    });
}

/// Full prune: returns (trace files deleted, log files rotated).
pub fn prune_all() -> (usize, usize) {
    (prune_traces(), rotate_logs())
}

fn remove_quiet(path: &std::path::Path) -> bool {
    // O5 — evict the path from trace handle caches before deleting: avoids cached handles writing to the inode of a deleted file.
    // Both trace trees (plugin / rpc) must be cleared — each has its own handles table,
    // and remove is a no-op for paths it does not own, so evicting either one is safe.
    super::plugin_trace::evict_path(path);
    super::req_trace::evict_path(path);
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("[retention] remove {} failed: {}", path.display(), e);
            false
        }
    }
}

/// traces retention: most recent 20 and within 14 days; over 50MB total, delete from oldest.
/// The rpc/ request-chain and hooks/ session trace subtrees each run the same policy (first-level subdirectory = agent / kind).
pub fn prune_traces() -> usize {
    let root = super::plugin_trace::traces_root();
    prune_traces_at(&root)
        + prune_traces_at(&root.join("rpc"))
        + prune_traces_at(&root.join("hooks"))
}

/// The body is implemented against an injected root: tests pass a temp directory directly, avoiding process-level env races with other tests.
fn prune_traces_at(root: &std::path::Path) -> usize {
    let Ok(plugins) = std::fs::read_dir(root) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in plugins.filter_map(|e| e.ok()) {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // (path, mtime, size), sorted by mtime newest→oldest
        let mut files: Vec<(PathBuf, SystemTime, u64)> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map(|x| x == "ndjson").unwrap_or(false))
                    .filter_map(|e| {
                        let md = e.metadata().ok()?;
                        let mtime = md.modified().ok()?;
                        Some((e.path(), mtime, md.len()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if files.is_empty() {
            continue;
        }
        files.sort_by_key(|(_, mtime, _)| std::cmp::Reverse(*mtime));
        let age_limit = Duration::from_secs(TRACE_MAX_AGE_DAYS * 86_400);
        // First filter by "most recent 20 and within 14 days"; drop the rest
        let mut kept: Vec<(PathBuf, u64)> = Vec::new();
        for (i, (path, mtime, size)) in files.into_iter().enumerate() {
            let recent = i < TRACE_MAX_SESSIONS;
            let fresh = now
                .duration_since(mtime)
                .map(|d| d < age_limit)
                .unwrap_or(true);
            if recent && fresh {
                kept.push((path, size));
            } else if remove_quiet(&path) {
                removed += 1;
            }
        }
        // Then delete from oldest by total size (50MB) (kept is already sorted newest→oldest)
        let mut total: u64 = kept.iter().map(|(_, s)| *s).sum();
        while total > TRACE_MAX_BYTES && !kept.is_empty() {
            let (path, size) = kept.pop().expect("non-empty");
            if remove_quiet(&path) {
                removed += 1;
            }
            total = total.saturating_sub(size);
        }
    }
    removed
}

/// P1 — screenshot cache retention: most recent 20 ∩ 7 days; delete the rest (only matching `screen-*.png`, leaving other cache files alone).
fn prune_screen_captures_at(dir: &std::path::Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut files: Vec<(PathBuf, SystemTime)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with("screen-") && n.ends_with(".png")
        })
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((e.path(), mtime))
        })
        .collect();
    if files.is_empty() {
        return 0;
    }
    files.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));
    let now = SystemTime::now();
    let age_limit = Duration::from_secs(SCREEN_MAX_AGE_DAYS * 86_400);
    let mut removed = 0;
    for (i, (path, mtime)) in files.into_iter().enumerate() {
        let recent = i < SCREEN_KEEP;
        let fresh = now
            .duration_since(mtime)
            .map(|d| d < age_limit)
            .unwrap_or(true);
        if !(recent && fresh) && remove_quiet(&path) {
            removed += 1;
        }
    }
    removed
}

/// Log rotation: `.log` (>10MB) → `.log.1` → `.log.2` (keep 2 backups, discard the oldest).
pub fn rotate_logs() -> usize {
    rotate_logs_at(&logs_root())
}

fn rotate_logs_at(root: &std::path::Path) -> usize {
    let Ok(rd) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut rotated = 0;
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().map(|x| x != "log").unwrap_or(true) {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        if md.len() <= LOG_MAX_BYTES {
            continue;
        }
        let base = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(format!("{}.{}", base, LOG_BACKUPS));
        for i in (1..LOG_BACKUPS).rev() {
            let _ = std::fs::rename(format!("{}.{}", base, i), format!("{}.{}", base, i + 1));
        }
        if std::fs::rename(&path, format!("{}.1", base)).is_ok() {
            rotated += 1;
        }
    }
    rotated
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn fixture_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opencapx-retention-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Create a file: marker bytes at the head, length set_len to len (sparse), optionally backdating mtime.
    fn touch(dir: &std::path::Path, name: &str, marker: &str, len: u64, age: Option<Duration>) {
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(marker.as_bytes()).unwrap();
        f.set_len(len.max(marker.len() as u64)).unwrap();
        if let Some(age) = age {
            f.set_modified(SystemTime::now() - age).unwrap();
        }
    }

    /// S2 — traces three-tier bounds: count (most recent 20) / age (14 days) / total size (over 50MB delete from oldest).
    #[test]
    fn prune_traces_enforces_count_age_and_size() {
        let root = fixture_root("traces");
        let _ = std::fs::read_dir(&root);

        // ① Count: 25 fresh files → keep only the most recent 20 (delete the oldest 5)
        let a = root.join("com.a");
        std::fs::create_dir_all(&a).unwrap();
        for i in 0..25u64 {
            touch(
                &a,
                &format!("s{:02}.ndjson", i),
                "x",
                10,
                Some(Duration::from_secs((25 - i) * 60)),
            );
        }
        // ② Age: 1 file from 30 days ago + 2 fresh → delete the old one
        let b = root.join("com.b");
        std::fs::create_dir_all(&b).unwrap();
        touch(
            &b,
            "old.ndjson",
            "o",
            10,
            Some(Duration::from_secs(30 * 86_400)),
        );
        touch(&b, "new1.ndjson", "n", 10, None);
        touch(&b, "new2.ndjson", "n", 10, None);
        // ③ Total size: 2 fresh 30MB files (sparse) → delete the oldest 1
        let c = root.join("com.c");
        std::fs::create_dir_all(&c).unwrap();
        touch(
            &c,
            "big-old.ndjson",
            "b",
            30 * 1024 * 1024,
            Some(Duration::from_secs(3600)),
        );
        touch(&c, "big-new.ndjson", "b", 30 * 1024 * 1024, None);

        let removed = prune_traces_at(&root);
        assert_eq!(removed, 5 + 1 + 1, "count 5 + age 1 + size 1");

        let count = |p: &std::path::Path| std::fs::read_dir(p).unwrap().count();
        assert_eq!(count(&a), 20);
        assert_eq!(count(&b), 2);
        assert_eq!(count(&c), 1);
        assert!(
            c.join("big-new.ndjson").exists(),
            "size pruning deletes from oldest"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// P1 — screenshot cache: most recent 20 ∩ 7 days; delete old/excess, leave non-screenshot files alone.
    #[test]
    fn prune_screen_captures_bounds_cache() {
        let root = fixture_root("screen");
        for i in 0..25u64 {
            touch(
                &root,
                &format!("screen-{}.png", i),
                "p",
                10,
                Some(Duration::from_secs((25 - i) * 60)),
            );
        }
        touch(
            &root,
            "screen-old.png",
            "p",
            10,
            Some(Duration::from_secs(30 * 86_400)),
        );
        touch(&root, "other.bin", "p", 10, None);
        let removed = prune_screen_captures_at(&root);
        assert_eq!(removed, 5 + 1, "count 5 + age 1");
        assert!(
            root.join("other.bin").exists(),
            "non-screenshot files are untouched"
        );
        let remain = std::fs::read_dir(&root).unwrap().count();
        assert_eq!(remain, SCREEN_KEEP + 1, "keep 20 screenshots + other.bin");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// S2 — log rotation: small files stay put; over the cap, chain-shift, keeping `.log.1/.log.2`.
    #[test]
    fn rotate_logs_caps_and_shifts_backups() {
        let root = fixture_root("logs");

        // Small file: no rotation
        touch(&root, "small.log", "S", 1024, None);
        assert_eq!(rotate_logs_at(&root), 0);
        assert!(root.join("small.log").exists());

        // Chain shift: ONE/TWO old backups + new large file NEW
        touch(&root, "com.y.log.1", "ONE", 16, None);
        touch(&root, "com.y.log.2", "TWO", 16, None);
        touch(&root, "com.y.log", "NEW", LOG_MAX_BYTES + 1, None);

        assert_eq!(rotate_logs_at(&root), 1);
        let head = |name: &str| std::fs::read(root.join(name)).unwrap()[..3].to_vec();
        assert_eq!(head("com.y.log.2"), b"ONE", "old .1 moved to .2");
        assert_eq!(head("com.y.log.1"), b"NEW", ".log moved to .1");
        assert!(
            !root.join("com.y.log").exists(),
            ".log is gone after rotation (rebuilt on next append)"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The rpc trace subtree must also be covered by prune_traces() (each agent directory uses the same 20/14d/50MB policy).
    /// Tests the public entry point: the change is just "prune_traces scans one more subtree".
    /// Asserts only the total count, not the survivors — 25 files written in the same second share an mtime, so intra-group order is undefined.
    #[test]
    fn prune_traces_covers_rpc_subtree() {
        let _g = super::super::plugin_trace::traces_env_lock();
        let base = std::env::temp_dir().join(format!("ocx-ret-rpc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        let agent_dir = base.join("rpc").join("ag_x_01");
        std::fs::create_dir_all(&agent_dir).unwrap();
        // 25 trace files → over the per-directory limit of 20, the oldest 5 are deleted
        for i in 0..25u64 {
            let p = agent_dir.join(format!("rpc-{i:020}.ndjson"));
            std::fs::write(
                &p,
                "{\"ev\":\"start\",\"spanId\":\"s0\",\"name\":\"rpc\",\"ts\":1,\"attrs\":{}}\n",
            )
            .unwrap();
        }

        let removed = prune_traces();

        assert_eq!(
            removed, 5,
            "oldest files over the limit under rpc/ are deleted"
        );
        let left = std::fs::read_dir(&agent_dir).unwrap().count();
        assert_eq!(left, 20);

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn crash_append_creates_and_grows() {
        let base = fixture_root("crash1");
        let p = base.join("crash.log");
        append_crash(&p, "[t0] first\n");
        append_crash(&p, "[t1] second\n");
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("[t0] first"));
        assert!(text.contains("[t1] second"));
    }

    #[test]
    fn crash_append_rotates_when_over_cap() {
        let base = fixture_root("crash2");
        let p = base.join("crash.log");
        // Pre-seed an oversized old file → the next append rotates first
        let big = "x".repeat(CRASH_LOG_MAX_BYTES as usize + 1);
        std::fs::write(&p, &big).unwrap();
        append_crash(&p, "[t2] fresh\n");
        let backup = base.join("crash.log.1");
        assert!(backup.is_file(), "oversized log must rotate to .1");
        assert_eq!(std::fs::metadata(&backup).unwrap().len(), big.len() as u64);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "[t2] fresh\n",
            "rotated file starts fresh with the new record"
        );
    }
}
