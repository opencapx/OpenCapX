//! Phase 36 — Capability SLA monitoring / alert thresholds.
//!
//! A background thread scans `capability_stats` for the last `window_secs` every `poll_secs`,
//! computing for each (capability, plugin_id) pair:
//! - p95 latency of `ok` samples (percentile from an in-memory sort, following the Phase 30 pattern)
//! - failure rate of `result != 'ok'`
//!
//! Either exceeding its threshold → emit a `capability.sla.violated` event (p.kind = "latency" / "failure").
//! The same (plugin, capability, kind) is emitted only once per `window_secs`; dedup uses an in-memory HashMap.
//!
//! Config is stored in the SQLite `sla_config(sk PRIMARY KEY, value TEXT)` table as JSON;
//! `set_sla_config` validates before writing, preventing illegal values from blowing up the background thread.

use super::event::{EventBus, OpencapxEvent};
use super::storage::SharedStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// SLA config. `#[serde(rename_all = "camelCase")]` lets the frontend / JS receive it directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlaConfig {
    /// p95 latency threshold for ok capability calls (milliseconds); 0 = disable this rule.
    #[serde(default = "default_p95")]
    pub p95_threshold_ms: u64,
    /// Failure-rate threshold (0..=100); alerting above it; > 100 or NaN is treated as disabled.
    #[serde(default = "default_fail_rate")]
    pub fail_rate_threshold_pct: f64,
    /// Monitoring window (seconds); only samples within the last window_secs are considered.
    #[serde(default = "default_window")]
    pub window_secs: u64,
    /// Monitor thread poll interval (seconds).
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
}

fn default_p95() -> u64 {
    2000
}
fn default_fail_rate() -> f64 {
    25.0
}
fn default_window() -> u64 {
    300
}
fn default_poll() -> u64 {
    30
}

impl Default for SlaConfig {
    fn default() -> Self {
        Self {
            p95_threshold_ms: default_p95(),
            fail_rate_threshold_pct: default_fail_rate(),
            window_secs: default_window(),
            poll_secs: default_poll(),
        }
    }
}

/// One alert. kind: "latency" / "failure". The "observed" unit matches the threshold
/// (latency=ms, failure=percentage 0..=100).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlaViolation {
    pub plugin_id: String,
    pub capability: String,
    pub kind: String,
    pub threshold: f64,
    pub observed: f64,
    pub since_ts: u64,
    pub samples: u64,
}

/// The SQLite key under which config is stored; there is only one row for now.
const CONFIG_KEY: &str = "default";

fn ensure_config_table(store: &SharedStore) {
    if let Ok(mut g) = store.lock() {
        g.with_conn(|c| {
            c.execute(
                "CREATE TABLE IF NOT EXISTS sla_config (
                    sk TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
                [],
            )
            .unwrap_or(0)
        });
    }
}

pub fn load_config(store: &SharedStore) -> SlaConfig {
    ensure_config_table(store);
    let raw: Option<String> = store
        .lock()
        .ok()
        .and_then(|mut s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT value FROM sla_config WHERE sk = ?1",
                    [CONFIG_KEY],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
        })
        .flatten();
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(store: &SharedStore, cfg: &SlaConfig) -> Result<(), String> {
    if cfg.window_secs == 0 || cfg.window_secs > 86400 {
        return Err(format!(
            "window_secs out of range (1..=86400): {}",
            cfg.window_secs
        ));
    }
    if cfg.poll_secs == 0 || cfg.poll_secs > 3600 {
        return Err(format!(
            "poll_secs out of range (1..=3600): {}",
            cfg.poll_secs
        ));
    }
    if !(0.0..=100.0).contains(&cfg.fail_rate_threshold_pct) {
        return Err(format!(
            "fail_rate_threshold_pct out of range (0..=100): {}",
            cfg.fail_rate_threshold_pct
        ));
    }
    ensure_config_table(store);
    let body = serde_json::to_string(cfg).map_err(|e| e.to_string())?;
    store.lock().ok().map(|mut s| {
        s.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO sla_config (sk, value) VALUES (?1, ?2)",
                rusqlite::params![CONFIG_KEY, body],
            )
            .unwrap_or(0)
        })
    });
    Ok(())
}

/// Stats for one (capability, plugin_id) pair.
#[derive(Debug, Clone)]
struct PairStat {
    samples: u64,
    fail_count: u64,
    ok_latencies: Vec<i64>,
    since_ts: u64,
}

/// Pure function: compute per-pair metrics from capability_stats within the last window_secs, returning threshold-crossing alerts.
/// Tests can seed samples and assert directly.
pub fn detect_violations(store: &SharedStore, cfg: &SlaConfig, now: u64) -> Vec<SlaViolation> {
    if cfg.window_secs == 0 {
        return Vec::new();
    }
    let since = now.saturating_sub(cfg.window_secs) as i64;
    // Take all samples in the window, group by (cap, plugin) in memory, and compute p95 / fail_rate
    let rows: Vec<(String, String, i64, i64, String)> = store
        .lock()
        .ok()
        .map(|s| {
            s.with_conn_ref(|c| {
                let mut stmt = match c.prepare(
                    "SELECT capability, plugin_id, elapsed_ms, ts, result FROM capability_stats
                     WHERE ts >= ?1 ORDER BY ts ASC",
                ) {
                    Ok(s) => s,
                    Err(_) => return Vec::new(),
                };
                let it = match stmt.query_map([since], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                }) {
                    Ok(it) => it,
                    Err(_) => return Vec::new(),
                };
                it.filter_map(|x| x.ok()).collect::<Vec<_>>()
            })
            .unwrap_or_default()
        })
        .unwrap_or_default();

    let mut groups: HashMap<(String, String), PairStat> = HashMap::new();
    for (cap, plugin, elapsed, ts, result) in rows {
        let entry = groups.entry((plugin, cap)).or_insert(PairStat {
            samples: 0,
            fail_count: 0,
            ok_latencies: Vec::new(),
            since_ts: now,
        });
        entry.samples += 1;
        if result != "ok" {
            entry.fail_count += 1;
        } else {
            entry.ok_latencies.push(elapsed);
        }
        if (ts as u64) < entry.since_ts {
            entry.since_ts = ts as u64;
        }
    }

    let mut out: Vec<SlaViolation> = Vec::new();
    for ((plugin_id, capability), stat) in groups {
        // Skip when samples are too few (<10); alerts are easily noisy
        if stat.samples < 10 {
            continue;
        }
        // p95 (ok only)
        if cfg.p95_threshold_ms > 0 && !stat.ok_latencies.is_empty() {
            let mut lats = stat.ok_latencies.clone();
            lats.sort_unstable();
            let idx = (lats.len() as f64 * 0.95).ceil() as usize;
            let p95 = lats
                .get(idx.saturating_sub(1).min(lats.len() - 1))
                .copied()
                .unwrap_or(0);
            if (p95 as u64) > cfg.p95_threshold_ms {
                out.push(SlaViolation {
                    plugin_id: plugin_id.clone(),
                    capability: capability.clone(),
                    kind: "latency".into(),
                    threshold: cfg.p95_threshold_ms as f64,
                    observed: p95 as f64,
                    since_ts: stat.since_ts,
                    samples: stat.samples,
                });
            }
        }
        // fail rate
        if cfg.fail_rate_threshold_pct > 0.0 {
            let rate = (stat.fail_count as f64) * 100.0 / (stat.samples as f64);
            if rate > cfg.fail_rate_threshold_pct {
                out.push(SlaViolation {
                    plugin_id: plugin_id.clone(),
                    capability: capability.clone(),
                    kind: "failure".into(),
                    threshold: cfg.fail_rate_threshold_pct,
                    observed: rate,
                    since_ts: stat.since_ts,
                    samples: stat.samples,
                });
            }
        }
    }
    out
}

/// List recent `capability.sla.violated` events (for the frontend history panel).
pub fn list_violations(store: &SharedStore, limit: usize) -> Vec<SlaViolation> {
    store
        .lock()
        .ok()
        .map(|s| {
            s.with_conn_ref(|c| {
                let mut stmt = match c.prepare(
                    "SELECT id, source, timestamp, payload FROM events
                     WHERE type = 'capability.sla.violated'
                     ORDER BY timestamp DESC LIMIT ?1",
                ) {
                    Ok(s) => s,
                    Err(_) => return Vec::new(),
                };
                let it = match stmt.query_map([limit as i64], |r| {
                    let payload: String = r.get(3)?;
                    let p: serde_json::Value =
                        serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        p,
                    ))
                }) {
                    Ok(it) => it,
                    Err(_) => return Vec::new(),
                };
                it.filter_map(|x| x.ok())
                    .map(|(_id, _source, ts, payload)| {
                        let plugin_id = payload
                            .get("pluginId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let capability = payload
                            .get("capability")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let kind = payload
                            .get("kind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let threshold = payload
                            .get("threshold")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let observed = payload
                            .get("observed")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let samples = payload.get("samples").and_then(|v| v.as_u64()).unwrap_or(0);
                        SlaViolation {
                            plugin_id,
                            capability,
                            kind,
                            threshold,
                            observed,
                            since_ts: ts as u64,
                            samples,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Start the background monitor thread; dedup is in memory (process-level, sufficient for the refresh policy).
pub fn spawn_sla_monitor(store: SharedStore, bus: Arc<EventBus>) {
    std::thread::spawn(move || {
        let mut last_seen: HashMap<(String, String, String), u64> = HashMap::new();
        let cfg = load_config(&store);
        let mut next_poll_at =
            std::time::Instant::now() + std::time::Duration::from_secs(cfg.poll_secs);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if std::time::Instant::now() < next_poll_at {
                continue;
            }
            let cfg = load_config(&store);
            next_poll_at =
                std::time::Instant::now() + std::time::Duration::from_secs(cfg.poll_secs.max(2));
            let now = super::event_replay::now_secs();
            for v in detect_violations(&store, &cfg, now) {
                let key = (v.plugin_id.clone(), v.capability.clone(), v.kind.clone());
                // dedup: the same (plugin, capability, kind) fires only once per window_secs
                if let Some(prev) = last_seen.get(&key) {
                    if now.saturating_sub(*prev) < cfg.window_secs {
                        continue;
                    }
                }
                last_seen.insert(key, now);
                let payload = serde_json::json!({
                    "pluginId": v.plugin_id,
                    "capability": v.capability,
                    "kind": v.kind,
                    "threshold": v.threshold,
                    "observed": v.observed,
                    "samples": v.samples,
                });
                bus.publish(&OpencapxEvent::new(
                    "capability.sla.violated",
                    "sla-monitor",
                    payload,
                ));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::storage::{Storage, StoreEnum};
    use std::sync::Mutex;

    fn tmp_store(tag: &str) -> SharedStore {
        let dir = std::env::temp_dir().join(format!("opencapx-sla-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Storage::open(&dir.join("t.db")).unwrap();
        Arc::new(Mutex::new(StoreEnum::Db(s)))
    }

    fn cap_sample(
        store: &SharedStore,
        cap: &str,
        plugin: &str,
        elapsed: i64,
        ts: i64,
        result: &str,
    ) {
        if let Ok(mut s) = store.lock() {
            s.record_capability_call(cap, plugin, elapsed, ts as u64, result, None);
        }
    }

    /// Both p95-threshold and fail_rate-threshold triggers are detected, with no duplicate per pair.
    #[test]
    fn detect_violations_finds_p95_and_fail_rate() {
        let store = tmp_store("detect");
        let now = 1_000_000u64;
        // Slow pair (plug-a, image.analyze): 10 ok, 9 at 100ms + 1 at 5000ms → p95 ≈ 5000 > 1000 threshold
        for i in 0..10 {
            let elapsed = if i == 9 { 5000 } else { 100 };
            cap_sample(
                &store,
                "image.analyze",
                "plug-a",
                elapsed,
                (now - 60 + i) as i64,
                "ok",
            );
        }
        // High-failure pair (plug-b, shell.exec): 6 ok + 5 timeout → 5/11 ≈ 45% > 25%
        for i in 0..6 {
            cap_sample(
                &store,
                "shell.exec",
                "plug-b",
                200,
                (now - 50 + i) as i64,
                "ok",
            );
        }
        for i in 0..5 {
            cap_sample(
                &store,
                "shell.exec",
                "plug-b",
                60000,
                (now - 40 + i) as i64,
                "timeout",
            );
        }
        // Healthy pair (plug-c, camera): 8 ok all at 50ms → should not trigger
        for i in 0..8 {
            cap_sample(&store, "camera", "plug-c", 50, (now - 30 + i) as i64, "ok");
        }

        let cfg = SlaConfig {
            p95_threshold_ms: 1000,
            fail_rate_threshold_pct: 25.0,
            window_secs: 300,
            poll_secs: 30,
        };
        let vios = detect_violations(&store, &cfg, now);
        let kinds: Vec<(&str, &str, &str)> = vios
            .iter()
            .map(|v| (v.plugin_id.as_str(), v.capability.as_str(), v.kind.as_str()))
            .collect();
        assert!(
            kinds.contains(&("plug-a", "image.analyze", "latency")),
            "plug-a/image.analyze triggered latency, actual: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&("plug-b", "shell.exec", "failure")),
            "plug-b/shell.exec triggered failure (45% > 25%), actual: {:?}",
            kinds
        );
        assert!(
            !kinds.iter().any(|(p, _, k)| *p == "plug-c"),
            "plug-c is healthy and should not trigger; actual: {:?}",
            kinds
        );
    }

    /// Fewer than 10 samples are skipped, avoiding noise.
    #[test]
    fn detect_violations_skips_low_sample_pairs() {
        let store = tmp_store("low");
        let now = 1_000_000u64;
        // Only 5 samples, all fail
        for i in 0..5 {
            cap_sample(
                &store,
                "shell.exec",
                "plug-x",
                60000,
                (now - 30 + i) as i64,
                "err",
            );
        }
        let cfg = SlaConfig {
            p95_threshold_ms: 100,
            fail_rate_threshold_pct: 10.0,
            window_secs: 300,
            poll_secs: 30,
        };
        let vios = detect_violations(&store, &cfg, now);
        assert!(
            vios.is_empty(),
            "5 samples should not trigger (noise); actual {} entries",
            vios.len()
        );
    }

    /// Samples outside the window must be ignored.
    #[test]
    fn detect_violations_ignores_samples_outside_window() {
        let store = tmp_store("window");
        let now = 1_000_000u64;
        // All from an hour ago, far outside the 60s window
        for i in 0..10 {
            cap_sample(
                &store,
                "shell.exec",
                "plug-old",
                60000,
                (now - 3600 + i) as i64,
                "err",
            );
        }
        let cfg = SlaConfig {
            p95_threshold_ms: 0,
            fail_rate_threshold_pct: 25.0,
            window_secs: 60,
            poll_secs: 30,
        };
        let vios = detect_violations(&store, &cfg, now);
        assert!(
            vios.is_empty(),
            "samples outside the window must be ignored; actual {} entries",
            vios.len()
        );
    }

    /// Config save / load round-trip + validation.
    #[test]
    fn config_save_load_and_validates() {
        let store = tmp_store("cfg");
        let cfg = SlaConfig {
            p95_threshold_ms: 1234,
            fail_rate_threshold_pct: 17.5,
            window_secs: 600,
            poll_secs: 60,
        };
        save_config(&store, &cfg).unwrap();
        let got = load_config(&store);
        assert_eq!(got.p95_threshold_ms, 1234);
        assert!((got.fail_rate_threshold_pct - 17.5).abs() < 1e-9);
        assert_eq!(got.window_secs, 600);
        assert_eq!(got.poll_secs, 60);

        // Validation: negative / out of range
        assert!(save_config(
            &store,
            &SlaConfig {
                window_secs: 0,
                ..cfg.clone()
            }
        )
        .is_err());
        assert!(save_config(
            &store,
            &SlaConfig {
                poll_secs: 99999,
                ..cfg.clone()
            }
        )
        .is_err());
        assert!(save_config(
            &store,
            &SlaConfig {
                fail_rate_threshold_pct: 150.0,
                ..cfg.clone()
            }
        )
        .is_err());

        // Guard against workspace pollution
        let _ = std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("opencapx-sla-{}-cfg", std::process::id())),
        );
    }
}
