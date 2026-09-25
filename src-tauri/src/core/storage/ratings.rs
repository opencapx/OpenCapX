//! plugin ratings and capability-stats accessors.
//! Mechanical move from core/storage.rs.

use super::*;

impl Storage {
    /// Add a rating; returns the generated row. An out-of-range score returns Err (so the caller reports 4xx).
    pub fn add_rating(
        &mut self,
        plugin_id: &str,
        score: i64,
        comment: Option<&str>,
        ts: u64,
    ) -> Result<PluginRating, String> {
        if !(1..=5).contains(&score) {
            return Err(format!("score out of range: {}", score));
        }
        let id = format!("rat-{}-{}", ts, plugin_id);
        let comment_owned = comment.map(|s| s.to_string());
        let res = self.conn.execute(
            "INSERT INTO plugin_ratings (id, plugin_id, score, comment, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, plugin_id, score, &comment_owned, ts as i64],
        );
        match res {
            Ok(_) => Ok(PluginRating {
                id,
                plugin_id: plugin_id.to_string(),
                score,
                comment: comment_owned,
                ts,
            }),
            Err(e) => Err(format!("insert rating: {}", e)),
        }
    }

    pub fn list_ratings(&self, plugin_id: &str, limit: usize) -> Vec<PluginRating> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, plugin_id, score, comment, ts FROM plugin_ratings
             WHERE plugin_id = ?1 ORDER BY ts DESC LIMIT ?2",
        ) else {
            return Vec::new();
        };
        stmt.query_map(rusqlite::params![plugin_id, limit as i64], |r| {
            Ok(PluginRating {
                id: r.get(0)?,
                plugin_id: r.get(1)?,
                score: r.get(2)?,
                comment: r.get(3)?,
                ts: r.get::<_, i64>(4)? as u64,
            })
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    pub fn rating_summary(&self, plugin_id: &str) -> PluginRatingSummary {
        let mut count: i64 = 0;
        let mut avg = 0.0_f64;
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT COUNT(*), COALESCE(AVG(score), 0) FROM plugin_ratings WHERE plugin_id = ?1",
        ) {
            if let Ok(rows) = stmt.query_map(rusqlite::params![plugin_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
            }) {
                if let Some(Ok((c, a))) = rows.into_iter().next() {
                    count = c;
                    avg = (a * 100.0).round() / 100.0;
                }
            }
        }
        PluginRatingSummary {
            plugin_id: plugin_id.to_string(),
            count,
            avg,
        }
    }

    /// Phase 37 — Probe self-check: write a report row (overwrite).
    /// status is expected to be "pending" / "passed" / "failed"; report is the full JSON-serialized ProbeReport.
    pub fn set_probe_status(
        &mut self,
        plugin_id: &str,
        status: &str,
        report: Option<&str>,
        at: u64,
    ) {
        let _ = self.conn.execute(
            "UPDATE plugins SET probe_status = ?2, probe_at = ?3, probe_report = ?4 WHERE id = ?1",
            rusqlite::params![plugin_id, status, at as i64, report],
        );
    }

    /// Phase 37 — read the most recent probe report. Returns (status, at, report_json).
    pub fn get_probe_report(&self, plugin_id: &str) -> Option<(String, u64, String)> {
        let mut stmt = self
            .conn
            .prepare("SELECT probe_status, probe_at, probe_report FROM plugins WHERE id = ?1")
            .ok()?;
        stmt.query_row(rusqlite::params![plugin_id], |r| {
            let status: Option<String> = r.get(0)?;
            let at: Option<i64> = r.get(1)?;
            let report: Option<String> = r.get(2)?;
            Ok((
                status.unwrap_or_default(),
                at.unwrap_or(0) as u64,
                report.unwrap_or_default(),
            ))
        })
        .ok()
        .filter(|(s, _, _)| !s.is_empty())
    }

    /// Phase 38 — write a plugin's channel (`channel` is already one of the stable stable/beta/dev, normalized by the caller).
    pub fn set_plugin_channel(&mut self, plugin_id: &str, channel: &str) {
        let _ = self.conn.execute(
            "INSERT INTO plugin_channel (plugin_id, channel, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(plugin_id) DO UPDATE SET channel=?2, updated_at=?3",
            rusqlite::params![plugin_id, channel, crate::core::agent::now_secs() as i64],
        );
    }

    /// Phase 38 — fetch all (plugin_id, channel) at once. Used for batch filtering in check_plugin_updates.
    pub fn list_plugin_channels(&self) -> Vec<(String, String)> {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT plugin_id, channel FROM plugin_channel")
        else {
            return Vec::new();
        };
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .ok()
            .map(|i| i.filter_map(|x| x.ok()).collect())
            .unwrap_or_default()
    }

    /// Phase 40 — read a plugin's health config (all columns allow NULL; missing values use the default).
    pub fn get_health_config(&self, plugin_id: &str) -> crate::core::health::PluginHealthConfig {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT heartbeat_sec, ping_timeout_ms, max_retries, backoff_initial_ms, enabled
             FROM plugin_health_config WHERE plugin_id = ?1",
        ) else {
            return crate::core::health::PluginHealthConfig::default();
        };
        stmt.query_row(rusqlite::params![plugin_id], |r| {
            Ok(crate::core::health::config_from_row(
                r.get::<_, Option<i64>>(0).ok().flatten(),
                r.get::<_, Option<i64>>(1).ok().flatten(),
                r.get::<_, Option<i64>>(2).ok().flatten(),
                r.get::<_, Option<i64>>(3).ok().flatten(),
                r.get::<_, Option<i64>>(4).ok().flatten(),
            ))
        })
        .unwrap_or_default()
    }

    /// Phase 40 — write a plugin's health config (upsert). The caller validates with validate first.
    pub fn upsert_health_config(
        &mut self,
        plugin_id: &str,
        cfg: &crate::core::health::PluginHealthConfig,
    ) {
        let now = crate::core::agent::now_secs() as i64;
        let _ = self.conn.execute(
            "INSERT INTO plugin_health_config
                (plugin_id, heartbeat_sec, ping_timeout_ms, max_retries, backoff_initial_ms, enabled, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(plugin_id) DO UPDATE SET
                heartbeat_sec=?2, ping_timeout_ms=?3, max_retries=?4,
                backoff_initial_ms=?5, enabled=?6, updated_at=?7",
            rusqlite::params![
                plugin_id,
                cfg.heartbeat_sec as i64,
                cfg.ping_timeout_ms as i64,
                cfg.max_retries as i64,
                cfg.backoff_initial_ms as i64,
                if cfg.enabled { 1 } else { 0 },
                now,
            ],
        );
    }

    /// Phase 40 — fetch all plugin_health_config rows at once. Used by the list command for batch returns.
    pub fn list_health_configs(&self) -> Vec<(String, crate::core::health::PluginHealthConfig)> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT plugin_id, heartbeat_sec, ping_timeout_ms, max_retries, backoff_initial_ms, enabled
             FROM plugin_health_config",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            let pid: String = r.get(0)?;
            let cfg = crate::core::health::config_from_row(
                r.get::<_, Option<i64>>(1).ok().flatten(),
                r.get::<_, Option<i64>>(2).ok().flatten(),
                r.get::<_, Option<i64>>(3).ok().flatten(),
                r.get::<_, Option<i64>>(4).ok().flatten(),
                r.get::<_, Option<i64>>(5).ok().flatten(),
            );
            Ok((pid, cfg))
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// Phase 38 — global key/value config (used for defaultChannel). A nonexistent key returns None.
    pub fn get_setting(&self, key: &str) -> Option<String> {
        let mut stmt = self
            .conn
            .prepare("SELECT v FROM settings_kv WHERE k = ?1")
            .ok()?;
        stmt.query_row(rusqlite::params![key], |r| r.get::<_, String>(0))
            .ok()
    }

    /// Phase 38 — write global key/value.
    pub fn set_setting(&mut self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO settings_kv (k, v) VALUES (?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v=?2",
            rusqlite::params![key, value],
        );
    }

    /// Phase 45 — write a metrics sample. Returns the rowid.
    pub fn insert_metrics_sample(
        &mut self,
        plugin_id: &str,
        ts: i64,
        cpu_pct: f64,
        rss_bytes: i64,
        threads: i64,
        fds: i64,
    ) {
        let _ = self.conn.execute(
            "INSERT INTO plugin_metrics_samples (plugin_id, ts, cpu_pct, rss_bytes, threads, fds)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![plugin_id, ts, cpu_pct, rss_bytes, threads, fds],
        );
    }

    /// Phase 45 — fetch a plugin's historical samples after from_ts (or all if from_ts=0), by ts ascending.
    pub fn list_metrics_samples(
        &self,
        plugin_id: &str,
        from_ts: i64,
        limit: usize,
    ) -> Vec<(i64, f64, i64, i64, i64)> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT ts, cpu_pct, rss_bytes, threads, fds FROM plugin_metrics_samples
             WHERE plugin_id = ?1 AND ts >= ?2 ORDER BY ts DESC LIMIT ?3",
        ) else {
            return Vec::new();
        };
        let mut rows: Vec<(i64, f64, i64, i64, i64)> = stmt
            .query_map(rusqlite::params![plugin_id, from_ts, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .ok()
            .map(|i| i.filter_map(|x| x.ok()).collect())
            .unwrap_or_default();
        rows.reverse();
        rows
    }

    /// Phase 45 — keep only the latest 1500 rows per plugin (discard older ones beyond that). Batch-deletes the oldest.
    pub fn prune_metrics_samples(&mut self, keep_per_plugin: usize) {
        let _ = self.conn.execute_batch(&format!(
            "DELETE FROM plugin_metrics_samples WHERE id IN (
                SELECT id FROM plugin_metrics_samples m1
                WHERE (SELECT COUNT(*) FROM plugin_metrics_samples m2
                       WHERE m2.plugin_id = m1.plugin_id AND m2.id >= m1.id) > {keep}
             )",
            keep = keep_per_plugin as i64,
        ));
    }
}
