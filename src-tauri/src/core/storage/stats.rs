//! hotkey rows and capability-call stats (non-alerting tables that shared this block).
//! Mechanical move from core/storage/alerting_tables.rs.

use super::*;

impl Storage {
    /// Phase 39 — all hotkey bindings. combo is the case-normalized string (e.g. "Cmd+Shift+K").
    pub fn list_hotkeys(&self) -> Vec<HotkeyRow> {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT combo, label, payload, created_at, updated_at FROM hotkey_binding ORDER BY combo")
        else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok(HotkeyRow {
                combo: r.get(0)?,
                label: r.get(1)?,
                payload: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// Record a capability call + latency. Used by the Phase 30 dashboard.
    /// Phase 31: adds result + error_kind, failure classification.
    pub fn record_capability_call(
        &mut self,
        cap: &str,
        plugin: &str,
        elapsed_ms: i64,
        ts: u64,
        result: &str,
        error_kind: Option<&str>,
    ) {
        let _ = self.conn.execute(
            "INSERT INTO capability_stats (capability, plugin_id, elapsed_ms, ts, result, error_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![cap, plugin, elapsed_ms, ts as i64, result, error_kind],
        );
    }

    /// Aggregate the latest N calls per (capability, plugin_id) pair: count + avg + p50 + p95 + last_used + fail_count + errors.
    /// Computes percentiles in memory (N≤500 to prevent blowup), not via SQLite math functions. p50/p95 use only result='ok' samples; failed times are excluded from the latency distribution.
    pub fn capability_stats_summary(&self, samples_per_pair: usize) -> Vec<CapabilityStat> {
        let n = samples_per_pair.max(10).min(500);
        // take the latest n per pair: use a subquery to find the n newest ts for each (cap, plugin), then aggregate.
        // SQLite has no LATERAL, so use the window function ROW_NUMBER() OVER (PARTITION BY ... ORDER BY ts DESC).
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT capability, plugin_id, elapsed_ms, ts, result, error_kind FROM (
                SELECT capability, plugin_id, elapsed_ms, ts, result, error_kind,
                       ROW_NUMBER() OVER (PARTITION BY capability, plugin_id ORDER BY ts DESC) AS rn
                FROM capability_stats
             ) WHERE rn <= ?1 ORDER BY capability ASC, ts DESC",
        ) else {
            return Vec::new();
        };
        // (cap, plugin, elapsed, ts, result, error_kind)
        let rows: Vec<(String, String, i64, i64, String, Option<String>)> = stmt
            .query_map([n as i64], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .ok()
            .map(|i| i.filter_map(|x| x.ok()).collect())
            .unwrap_or_default();
        // group by (cap, plugin)
        use std::collections::BTreeMap;
        let mut groups: BTreeMap<(String, String), Vec<(i64, i64, String, Option<String>)>> =
            BTreeMap::new();
        let mut last_used: std::collections::HashMap<(String, String), u64> = Default::default();
        for (cap, plugin, elapsed, ts, result, error_kind) in rows {
            groups
                .entry((cap.clone(), plugin.clone()))
                .or_default()
                .push((elapsed, ts, result, error_kind));
            let key = (cap, plugin);
            let entry = last_used.entry(key).or_insert(0);
            if (ts as u64) > *entry {
                *entry = ts as u64;
            }
        }
        let mut out: Vec<CapabilityStat> = groups
            .into_iter()
            .map(|((capability, plugin_id), mut samples)| {
                let last_used_at = last_used
                    .remove(&(capability.clone(), plugin_id.clone()))
                    .unwrap_or(0);
                let count = samples.len() as i64;
                // compute latency percentiles from ok only (a failed timeout=60000 would pollute p95)
                let mut ok_elapsed: Vec<i64> = samples
                    .iter()
                    .filter(|s| s.2 == "ok")
                    .map(|s| s.0)
                    .collect();
                ok_elapsed.sort_unstable();
                let (avg_ms, p50_ms, p95_ms) = if ok_elapsed.is_empty() {
                    (0.0, 0, 0)
                } else {
                    let sum: i64 = ok_elapsed.iter().sum();
                    let avg = sum as f64 / ok_elapsed.len() as f64;
                    let p50 = ok_elapsed[ok_elapsed.len() / 2];
                    let p95_idx = ((ok_elapsed.len() as f64 * 0.95).ceil() as usize)
                        .saturating_sub(1)
                        .min(ok_elapsed.len() - 1);
                    let p95 = ok_elapsed[p95_idx];
                    ((avg * 100.0).round() / 100.0, p50, p95)
                };
                // failure bucketing
                let mut errors: BTreeMap<String, i64> = BTreeMap::new();
                let mut fail_count: i64 = 0;
                for (_, _, result, error_kind) in &samples {
                    if result != "ok" {
                        fail_count += 1;
                        let kind = error_kind.clone().unwrap_or_else(|| "unknown".to_string());
                        *errors.entry(kind).or_insert(0) += 1;
                    }
                }
                CapabilityStat {
                    capability,
                    plugin_id,
                    count,
                    avg_ms,
                    p50_ms,
                    p95_ms,
                    last_used_at,
                    fail_count,
                    errors,
                }
            })
            .collect();
        // show to the UI by count descending (hot paths first)
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.capability.cmp(&b.capability)));
        out
    }
}
