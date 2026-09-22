//! Phase 45 — Plugin runtime metrics (CPU / memory / threads / FD).
//!
//! Data sources:
//! - macOS: `ps -o %cpu=,%mem=,rss=,pid` gets rss + %cpu, then a /proc-style sysctl fallback;
//!   thread count + fd count are parsed with `lsof -p` (simple cross-platform: lsof is used on Linux too).
//! - Linux: `/proc/<pid>/stat` + `/proc/<pid>/status` + `/proc/<pid>/fd` directory count.
//!
//! Design notes:
//! - Process-level CPU% needs a two-sample delta (utime+stime jiffies / time interval), so this module keeps a
//!   per-pid cache of the last `(jiffies, instant)`; the second call gets `delta_j / delta_t` to compute CPU%.
//! - When a single process does not exist, `collect_snapshot` returns None and the monitor thread clears it from the cache.
//! - Thresholds + poll period are persisted in settings_kv (key `plugin_metrics_config`).
//! - Each poll's snapshot is also written to the `plugin_metrics_samples` ring table (at most 1500 rows per plugin).
//! - Threshold exceeded → emit a `plugin.metrics.exceeded` EventBus event (per (plugin, kind) 5s dedup),
//!   SSE pushes it live to tint the frontend toast/card.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;

/// A single plugin's one-shot sample snapshot. None fields = that data source is unreadable (process exited / unsupported platform).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMetricsSnapshot {
    pub plugin_id: String,
    pub pid: u32,
    pub cpu_pct: Option<f64>,
    pub rss_bytes: Option<i64>,
    pub threads: Option<i64>,
    pub fds: Option<i64>,
    pub ts: i64,
}

/// Global monitoring config (persisted in settings_kv).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsConfig {
    /// Poll period (seconds). 0 = monitoring disabled.
    pub poll_secs: u32,
    /// CPU% threshold (0..=100). 0 = CPU rule off.
    pub cpu_pct_max: u32,
    /// RSS threshold (bytes). 0 = memory rule off.
    pub rss_bytes_max: u64,
    /// Thread count threshold. 0 = thread rule off.
    pub thread_count_max: u32,
    /// Samples kept per plugin (ring). Default 1500 ≈ 1h @ 2s sampling.
    pub keep_samples: u32,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            poll_secs: 2,
            cpu_pct_max: 95,
            rss_bytes_max: 512 * 1024 * 1024,
            thread_count_max: 100,
            keep_samples: 1500,
        }
    }
}

pub fn validate(cfg: &MetricsConfig) -> Result<(), String> {
    if cfg.poll_secs > 3600 {
        return Err(format!("poll_secs out of range (0..=3600): {}", cfg.poll_secs));
    }
    if cfg.cpu_pct_max > 100 {
        return Err(format!("cpu_pct_max out of range (0..=100): {}", cfg.cpu_pct_max));
    }
    if cfg.rss_bytes_max > 64 * 1024 * 1024 * 1024 {
        return Err(format!("rss_bytes_max too large (max 64 GiB): {}", cfg.rss_bytes_max));
    }
    if cfg.thread_count_max > 100_000 {
        return Err(format!("thread_count_max out of range: {}", cfg.thread_count_max));
    }
    if cfg.keep_samples == 0 || cfg.keep_samples > 50_000 {
        return Err(format!("keep_samples out of range: {}", cfg.keep_samples));
    }
    Ok(())
}

/// Previous sample per pid, used for the CPU% delta calculation.
#[derive(Debug, Clone)]
struct PrevSample {
    jiffies: u64,
    at: Instant,
}

#[derive(Default)]
struct Inner {
    prev: HashMap<u32, PrevSample>,
    latest: HashMap<String, PluginMetricsSnapshot>,
    /// per (plugin_id, kind) last alert instant → dedup.
    last_alert: HashMap<(String, String), Instant>,
}

static SHARED: OnceLock<Arc<Mutex<Inner>>> = OnceLock::new();

fn shared() -> Arc<Mutex<Inner>> {
    SHARED.get_or_init(|| Arc::new(Mutex::new(Inner::default()))).clone()
}

/// Read settings_kv for the metrics config. kv missing → defaults.
pub fn load_config() -> MetricsConfig {
    let Some(store) = super::shared_store() else {
        return MetricsConfig::default();
    };
    let Some(s) = store.lock().ok() else {
        return MetricsConfig::default();
    };
    match s.get_setting("plugin_metrics_config") {
        Some(json) => serde_json::from_str(&json).unwrap_or_default(),
        None => MetricsConfig::default(),
    }
}

/// Write the config to settings_kv (called by the monitor at startup and by the UI settings page).
pub fn save_config(cfg: &MetricsConfig) -> Result<(), String> {
    validate(cfg)?;
    let Some(store) = super::shared_store() else {
        return Err("store not ready".to_string());
    };
    let mut s = store.lock().map_err(|_| "store poisoned".to_string())?;
    let s_json = serde_json::to_string(cfg).map_err(|e| e.to_string())?;
    s.set_setting("plugin_metrics_config", &s_json);
    Ok(())
}

/// Get the latest snapshot for all plugins (for the tauri command).
pub fn latest_snapshots() -> Vec<PluginMetricsSnapshot> {
    let arc = shared();
    let s = arc.lock().expect("metrics mutex poisoned");
    let mut v: Vec<PluginMetricsSnapshot> = s.latest.values().cloned().collect();
    v.sort_by(|a, b| a.plugin_id.cmp(&b.plugin_id));
    v
}

/// Phase 46 — clear the latest cache on profile switch (otherwise a new profile sees stale data for old plugin ids).
pub fn clear_latest() {
    let arc = shared();
    let result = arc.lock();
    if let Ok(mut s) = result {
        s.latest.clear();
    }
}

/// Fetch a plugin's history after from_ts (for the chart). Limit defaults to 600 (5 minutes @ 2s).
pub fn history(plugin_id: &str, from_ts: i64, limit: usize) -> Vec<PluginMetricsSnapshot> {
    let Some(store) = super::shared_store() else {
        return Vec::new();
    };
    let Ok(g) = store.lock() else { return Vec::new() };
    use super::storage::StoreEnum;
    let rows = match &*g {
        StoreEnum::Db(s) => s.list_metrics_samples(plugin_id, from_ts, limit),
        StoreEnum::Mem(_) => Vec::new(),
    };
    rows.into_iter()
        .map(|(ts, cpu, rss, threads, fds)| PluginMetricsSnapshot {
            plugin_id: plugin_id.to_string(),
            pid: 0, // History is not bound to a pid (the process may have restarted)
            cpu_pct: Some(cpu),
            rss_bytes: Some(rss),
            threads: Some(threads),
            fds: Some(fds),
            ts,
        })
        .collect()
}

/// Called by the monitor: write the already-computed snapshot to the latest cache + SQLite + check thresholds + emit events.
pub fn record_snapshot(snap: &PluginMetricsSnapshot, cfg: &MetricsConfig) {
    let arc = shared();
    let mut s = match arc.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    s.latest.insert(snap.plugin_id.clone(), snap.clone());

    // SQLite persistence (failure does not affect real-time data)
    if let Some(store) = super::shared_store() {
        if let Ok(mut g) = store.lock() {
            use super::storage::StoreEnum;
            if let (Some(cpu), Some(rss)) = (snap.cpu_pct, snap.rss_bytes) {
                match &mut *g {
                    StoreEnum::Db(s) => {
                        s.insert_metrics_sample(
                            &snap.plugin_id,
                            snap.ts,
                            cpu,
                            rss,
                            snap.threads.unwrap_or(0),
                            snap.fds.unwrap_or(0),
                        );
                        s.prune_metrics_samples(cfg.keep_samples as usize);
                    }
                    StoreEnum::Mem(_) => {}
                }
            }
        }
    }

    // Threshold alerts (dedup 5 seconds)
    check_alerts(snap, cfg, &mut s);
}

const ALERT_DEDUP_SECS: u64 = 5;

fn check_alerts(snap: &PluginMetricsSnapshot, cfg: &MetricsConfig, s: &mut Inner) {
    let now = Instant::now();
    let mut checks: Vec<(&str, f64, f64)> = Vec::new(); // (kind, value, threshold)
    if cfg.cpu_pct_max > 0 {
        if let Some(cpu) = snap.cpu_pct {
            if cpu > cfg.cpu_pct_max as f64 {
                checks.push(("cpu", cpu, cfg.cpu_pct_max as f64));
            }
        }
    }
    if cfg.rss_bytes_max > 0 {
        if let Some(rss) = snap.rss_bytes {
            if rss > cfg.rss_bytes_max as i64 {
                checks.push(("memory", rss as f64, cfg.rss_bytes_max as f64));
            }
        }
    }
    if cfg.thread_count_max > 0 {
        if let Some(t) = snap.threads {
            if t > cfg.thread_count_max as i64 {
                checks.push(("threads", t as f64, cfg.thread_count_max as f64));
            }
        }
    }
    for (kind, value, threshold) in checks {
        let key = (snap.plugin_id.clone(), kind.to_string());
        let should_emit = match s.last_alert.get(&key) {
            Some(prev) => now.duration_since(*prev).as_secs() >= ALERT_DEDUP_SECS,
            None => true,
        };
        if should_emit {
            s.last_alert.insert(key, now);
            super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                "plugin.metrics.exceeded",
                "core",
                serde_json::json!({
                    "pluginId": snap.plugin_id,
                    "kind": kind,
                    "value": value,
                    "threshold": threshold,
                }),
            ));
        }
    }
}

/// Called by the monitor: clean up the per-pid prev cache for exited processes.
pub fn forget_dead_pids(current_pids: &[u32]) {
    let arc = shared();
    let mut s = match arc.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let live: std::collections::HashSet<u32> = current_pids.iter().copied().collect();
    s.prev.retain(|pid, _| live.contains(pid));
}

/// Called by the monitor: sample one pid's metrics (None means the process is dead).
/// O4 — stats are batch-collected by the caller (avoiding 3 subprocesses per plugin); here we only do delta/assembly.
pub fn collect_snapshot(
    plugin_id: &str,
    pid: u32,
    (jiffies, threads, fds, rss_bytes): (u64, i64, i64, i64),
) -> Option<PluginMetricsSnapshot> {
    let arc = shared();
    let mut s = arc.lock().ok()?;
    let now = Instant::now();
    let cpu_pct = match s.prev.get(&pid) {
        Some(prev) if jiffies >= prev.jiffies && now > prev.at => {
            let delta_j = jiffies - prev.jiffies;
            let delta_t = now.duration_since(prev.at).as_secs_f64();
            if delta_t > 0.0 {
                Some((delta_j as f64 / delta_t) * 100.0)
            } else {
                None
            }
        }
        _ => Some(0.0), // The first frame has no delta, so report 0 rather than None and wait for the next poll.
    };
    s.prev.insert(
        pid,
        PrevSample {
            jiffies,
            at: now,
        },
    );
    let ts = super::agent::now_secs() as i64;
    Some(PluginMetricsSnapshot {
        plugin_id: plugin_id.to_string(),
        pid,
        cpu_pct,
        rss_bytes: Some(rss_bytes),
        threads: Some(threads),
        fds: Some(fds),
        ts,
    })
}

/// Returns (cpu_jiffies, threads, fds, rss_bytes).
/// Process missing or stat parse failure → None; the monitor treats it as dead.
#[cfg(target_os = "linux")]
fn read_proc_stats(pid: u32) -> Option<(u64, i64, i64, i64)> {
    // /proc/<pid>/stat: utime (14) + stime (15) in jiffies (CLK_TCK is usually 100)
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    let rparen = stat.rfind(')')?;
    let after_paren = &stat[rparen + 2..];
    let mut fields = after_paren.split_whitespace();
    // field 0 is just past ')', so utime is the 12th token after rparen (index 11)
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.nth(0)?.parse().ok()?;
    let jiffies = utime.saturating_add(stime);

    // /proc/<pid>/status:Threads:    <n>
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    let threads = status
        .lines()
        .find_map(|l| l.strip_prefix("Threads:"))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    // /proc/<pid>/statm: RSS in pages
    let statm = std::fs::read_to_string(format!("/proc/{}/statm", pid)).ok()?;
    let pages: i64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let rss_bytes = pages.saturating_mul(4096);

    let fds = std::fs::read_dir(format!("/proc/{}/fd", pid))
        .map(|d| d.count() as i64)
        .unwrap_or(0);

    Some((jiffies, threads, fds, rss_bytes))
}

/// O4 — macOS batch sampling: one `ps` (pid/cpu/rss/time) + one `lsof` on demand (fd),
/// replacing the old "3 subprocesses per plugin"; lsof is expensive → throttle to 30s, reusing the fd cache on other rounds.
/// Thread count: macOS ps has no `thcount`/`tid` keyword (the old implementation silently got 0, and an invalid keyword makes the whole
/// ps call fail) → this layer honestly reports 0; reliable source = roadmap (sysctl task_info).
#[cfg(target_os = "macos")]
fn read_proc_stats_batch(
    pids: &[u32],
    poll_fds: bool,
    fd_cache: &mut std::collections::HashMap<u32, i64>,
) -> std::collections::HashMap<u32, (u64, i64, i64, i64)> {
    use std::collections::HashMap;
    let mut out: HashMap<u32, (u64, i64, i64, i64)> = HashMap::new();
    if pids.is_empty() {
        return out;
    }
    let list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    if let Ok(ps_out) = Command::new("ps")
        .args(["-o", "pid=,pcpu=,rss=,time=", "-p", &list])
        .output()
    {
        for line in String::from_utf8_lossy(&ps_out.stdout).lines() {
            let mut parts = line.split_whitespace();
            let (Some(pid_s), Some(_cpu), Some(rss), Some(time_s)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Ok(pid) = pid_s.parse::<u32>() else {
                continue;
            };
            let Ok(rss_kb) = rss.parse::<i64>() else {
                continue;
            };
            let jiffies = parse_time_str(time_s).unwrap_or(0);
            out.insert(pid, (jiffies, 0, 0, rss_kb.saturating_mul(1024)));
        }
    }
    if poll_fds {
        fd_cache.clear();
        if let Ok(lsof_out) = Command::new("lsof").args(["-p", &list]).output() {
            for line in String::from_utf8_lossy(&lsof_out.stdout).lines().skip(1) {
                if let Some(pid_s) = line.split_whitespace().nth(1) {
                    if let Ok(pid) = pid_s.parse::<u32>() {
                        *fd_cache.entry(pid).or_insert(0) += 1;
                    }
                }
            }
        }
    }
    for (pid, entry) in out.iter_mut() {
        entry.2 = *fd_cache.get(pid).unwrap_or(&0);
    }
    out
}

#[cfg(target_os = "macos")]
fn parse_time_str(s: &str) -> Option<u64> {
    // "MM:SS" or "HH:MM:SS" → seconds
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        2 => {
            let m: u64 = parts[0].parse().ok()?;
            let sec: u64 = parts[1].parse().ok()?;
            Some(m * 60 + sec)
        }
        3 => {
            let h: u64 = parts[0].parse().ok()?;
            let m: u64 = parts[1].parse().ok()?;
            let sec: u64 = parts[2].parse().ok()?;
            Some(h * 3600 + m * 60 + sec)
        }
        _ => None,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_proc_stats(_pid: u32) -> Option<(u64, i64, i64, i64)> {
    None
}

/// Background polling thread (singleton, idempotent). Scans all running plugins every `cfg.poll_secs`.
pub fn spawn_monitor() {
    use std::sync::OnceLock;
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| monitor_loop());
}

fn monitor_loop() {
    #[cfg(target_os = "macos")]
    let mut cycle: u64 = 0;
    #[cfg(target_os = "macos")]
    let mut fd_cache: std::collections::HashMap<u32, i64> = std::collections::HashMap::new();
    loop {
        let cfg = load_config();
        let sleep_secs = if cfg.poll_secs == 0 { 2 } else { cfg.poll_secs };
        std::thread::sleep(std::time::Duration::from_secs(sleep_secs as u64));
        if cfg.poll_secs == 0 {
            continue;
        }
        let mgr = super::plugin::PluginManager::shared();
        let pairs = mgr.list_running_with_pid();
        let pids: Vec<u32> = pairs.iter().map(|(_, p)| *p).collect();
        // O4 — macOS batch: one ps + lsof throttled to once per 30s (poll_secs≈2s × 15).
        #[cfg(target_os = "macos")]
        let batch = {
            cycle += 1;
            let poll_fds = cycle % 15 == 1;
            read_proc_stats_batch(&pids, poll_fds, &mut fd_cache)
        };
        let mut snapshots: Vec<PluginMetricsSnapshot> = Vec::new();
        for (plugin_id, pid) in &pairs {
            #[cfg(target_os = "macos")]
            let stats = batch.get(pid).copied();
            #[cfg(not(target_os = "macos"))]
            let stats = read_proc_stats(*pid);
            let Some(stats) = stats else { continue };
            if let Some(snap) = collect_snapshot(plugin_id, *pid, stats) {
                record_snapshot(&snap, &cfg);
                snapshots.push(snap);
            }
        }
        forget_dead_pids(&pids);
        if !snapshots.is_empty() {
            // Push a real-time copy to SSE via EventBus.
            super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                "plugin.metrics.sampled",
                "core",
                serde_json::json!({
                    "snapshots": snapshots.iter().map(|s| serde_json::json!({
                        "pluginId": s.plugin_id,
                        "pid": s.pid,
                        "cpuPct": s.cpu_pct,
                        "rssBytes": s.rss_bytes,
                        "threads": s.threads,
                        "fds": s.fds,
                        "ts": s.ts,
                    })).collect::<Vec<_>>()
                }),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_defaults() {
        assert!(validate(&MetricsConfig::default()).is_ok());
    }

    #[test]
    fn validate_rejects_out_of_range() {
        let mut cfg = MetricsConfig::default();
        cfg.poll_secs = 4000;
        assert!(validate(&cfg).is_err());
        cfg.poll_secs = 0; // 0 is valid (monitoring off)
        cfg.cpu_pct_max = 101;
        assert!(validate(&cfg).is_err());
        cfg.cpu_pct_max = 95;
        cfg.rss_bytes_max = 100 * 1024 * 1024 * 1024;
        assert!(validate(&cfg).is_err());
        cfg.rss_bytes_max = 0;
        cfg.thread_count_max = 200_000;
        assert!(validate(&cfg).is_err());
        cfg.thread_count_max = 100;
        cfg.keep_samples = 0;
        assert!(validate(&cfg).is_err());
        cfg.keep_samples = 60_000;
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn roundtrip_json() {
        let cfg = MetricsConfig {
            poll_secs: 5,
            cpu_pct_max: 80,
            rss_bytes_max: 256 * 1024 * 1024,
            thread_count_max: 200,
            keep_samples: 3000,
        };
        let s_json = serde_json::to_string(&cfg).unwrap();
        let back: MetricsConfig = serde_json::from_str(&s_json).unwrap();
        assert_eq!(cfg.poll_secs, back.poll_secs);
        assert_eq!(cfg.cpu_pct_max, back.cpu_pct_max);
        assert_eq!(cfg.rss_bytes_max, back.rss_bytes_max);
    }

    #[test]
    fn latest_snapshots_empty_initially() {
        let _ = latest_snapshots(); // must not panic
    }

    #[test]
    fn history_empty_for_unknown_plugin() {
        let h = history("nonexistent-plugin", 0, 100);
        assert!(h.is_empty());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn parse_time_str_handles_both_shapes() {
        assert_eq!(parse_time_str("00:01:23"), Some(83));
        assert_eq!(parse_time_str("01:23:45"), Some(3600 + 23 * 60 + 45));
        assert_eq!(parse_time_str("0:30"), Some(30));
        assert!(parse_time_str("garbage").is_none());
    }

    #[test]
    fn cpu_pct_first_frame_is_zero_not_none() {
        // The first frame has no delta → cpu reports 0 (takes effect on the next frame), it should not be None.
        // After O4, stats are collected by the caller (a missing process is detected at the collection layer); here we pass a set of numbers for assembly.
        let snap = collect_snapshot("test", 999_999, (100, 1, 5, 1024));
        let snap = snap.expect("assembly does not depend on process liveness");
        assert_eq!(snap.cpu_pct, Some(0.0));
        assert_eq!(snap.threads, Some(1));
        assert_eq!(snap.fds, Some(5));
    }

    /// O4 — batch collection can read its own process; throttled rounds reuse the fd cache.
    #[test]
    #[cfg(target_os = "macos")]
    fn proc_stats_batch_reads_self() {
        let me = std::process::id();
        let mut cache = std::collections::HashMap::new();
        let batch = read_proc_stats_batch(&[me], true, &mut cache);
        let (_, _threads, fds, rss) = *batch.get(&me).expect("self in batch");
        assert!(fds >= 3, "open fds: {}", fds);
        assert!(rss > 0, "rss: {}", rss);
        let batch2 = read_proc_stats_batch(&[me], false, &mut cache);
        assert_eq!(batch2.get(&me).map(|s| s.2), Some(fds), "throttled round reuses the cache");
    }
}