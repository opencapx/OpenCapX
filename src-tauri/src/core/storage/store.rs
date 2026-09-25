//! StoreEnum: the memory/Db dual backend and its forwarding impls.
//! Mechanical move from core/storage.rs.

use super::*;

/// The storage the app actually uses: SQLite preferred, falls back to memory if it cannot open.
pub enum StoreEnum {
    Db(Storage),
    Mem(crate::core::agent::SessionStore),
}

impl SessionSink for StoreEnum {
    fn upsert(&mut self, s: Session) {
        match self {
            StoreEnum::Db(x) => x.upsert(s),
            StoreEnum::Mem(x) => x.upsert(s),
        }
    }

    fn get(&self, id: &str) -> Option<Session> {
        match self {
            StoreEnum::Db(x) => x.get(id),
            StoreEnum::Mem(x) => x.get(id),
        }
    }

    fn active(&self, now: u64) -> Vec<Session> {
        match self {
            StoreEnum::Db(x) => x.active(now),
            StoreEnum::Mem(x) => x.active(now),
        }
    }

    fn all(&self) -> Vec<Session> {
        match self {
            StoreEnum::Db(x) => x.all(),
            StoreEnum::Mem(x) => x.all(),
        }
    }

    fn dismiss(&mut self, id: &str) {
        match self {
            StoreEnum::Db(x) => x.dismiss(id),
            StoreEnum::Mem(x) => x.dismiss(id),
        }
    }

    fn clear(&mut self) {
        match self {
            StoreEnum::Db(x) => x.clear(),
            StoreEnum::Mem(x) => x.clear(),
        }
    }

    fn sweep(&mut self, now: u64) -> usize {
        match self {
            StoreEnum::Db(x) => x.sweep(now),
            StoreEnum::Mem(x) => x.sweep(now),
        }
    }
}

impl StoreEnum {
    /// Session history (descending). The Mem variant has no archive, returns empty.
    pub fn list_session_archive(&self, limit: usize) -> Vec<crate::core::agent::ArchivedSession> {
        match self {
            StoreEnum::Db(s) => s.list_session_archive(limit),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Clean up archives past the retention period (the Mem variant is a no-op).
    pub fn prune_session_archive(&self, now: u64) -> usize {
        match self {
            StoreEnum::Db(s) => s.prune_session_archive(now),
            StoreEnum::Mem(_) => 0,
        }
    }

    pub fn log_event(&mut self, e: &OpencapxEvent) {
        if let StoreEnum::Db(x) = self {
            x.insert_event(e);
        }
    }

    /// Recent events (the Mem variant returns empty). Used by the audit timeline / SSE cold-start replay.
    pub fn list_events(&self, kind_prefix: &str, limit: usize) -> Vec<OpencapxEvent> {
        match self {
            StoreEnum::Db(s) => s.list_events(kind_prefix, limit),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Phase 35 — Audit search/filter (kind prefix + query + time range). The Mem variant returns empty.
    pub fn list_events_filtered(
        &self,
        kind_prefix: Option<&str>,
        query: Option<&str>,
        since: Option<u64>,
        until: Option<u64>,
        limit: usize,
    ) -> Vec<OpencapxEvent> {
        match self {
            StoreEnum::Db(s) => s.list_events_filtered(kind_prefix, query, since, until, limit),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Read-only SQL escape hatch (the Mem variant returns None). Used by plugin/permission/capability tables.
    pub fn with_conn_ref<R>(&self, f: impl FnOnce(&rusqlite::Connection) -> R) -> Option<R> {
        match self {
            StoreEnum::Db(s) => Some(f(&s.conn)),
            StoreEnum::Mem(_) => None,
        }
    }

    /// Writable SQL escape hatch, returns the number of affected rows.
    pub fn with_conn(&mut self, f: impl FnOnce(&rusqlite::Connection) -> usize) -> Option<usize> {
        match self {
            StoreEnum::Db(s) => Some(f(&s.conn)),
            StoreEnum::Mem(_) => None,
        }
    }

    /// Writable SQL escape hatch (**fallible version**): used by paths needing transactions + error propagation.
    /// The Mem variant returns None (the caller treats it as "storage unavailable").
    pub fn try_with_conn<R>(
        &mut self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<R, String>,
    ) -> Option<Result<R, String>> {
        match self {
            StoreEnum::Db(s) => Some(f(&s.conn)),
            StoreEnum::Mem(_) => None,
        }
    }

    #[allow(dead_code)]
    pub fn is_db(&self) -> bool {
        matches!(self, StoreEnum::Db(_))
    }
}

impl StoreEnum {
    /// Add a rating (the Mem variant returns success directly but does not count as persisted).
    pub fn add_rating(
        &mut self,
        plugin_id: &str,
        score: i64,
        comment: Option<&str>,
        ts: u64,
    ) -> Result<PluginRating, String> {
        match self {
            StoreEnum::Db(x) => x.add_rating(plugin_id, score, comment, ts),
            StoreEnum::Mem(_) => Ok(PluginRating {
                id: format!("rat-{}-{}", ts, plugin_id),
                plugin_id: plugin_id.to_string(),
                score,
                comment: comment.map(|s| s.to_string()),
                ts,
            }),
        }
    }

    pub fn list_ratings(&self, plugin_id: &str, limit: usize) -> Vec<PluginRating> {
        match self {
            StoreEnum::Db(x) => x.list_ratings(plugin_id, limit),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn rating_summary(&self, plugin_id: &str) -> PluginRatingSummary {
        match self {
            StoreEnum::Db(x) => x.rating_summary(plugin_id),
            StoreEnum::Mem(_) => PluginRatingSummary {
                plugin_id: plugin_id.to_string(),
                count: 0,
                avg: 0.0,
            },
        }
    }

    /// Phase 30 Dashboard: aggregate call stats per (capability, plugin_id) pair.
    pub fn capability_stats_summary(&self, samples_per_pair: usize) -> Vec<CapabilityStat> {
        match self {
            StoreEnum::Db(x) => x.capability_stats_summary(samples_per_pair),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Record a call (the Mem variant swallows it; only Db persists).
    /// Phase 31: result + error_kind distinguish success/failure classification.
    pub fn record_capability_call(
        &mut self,
        cap: &str,
        plugin: &str,
        elapsed_ms: i64,
        ts: u64,
        result: &str,
        error_kind: Option<&str>,
    ) {
        if let StoreEnum::Db(x) = self {
            x.record_capability_call(cap, plugin, elapsed_ms, ts, result, error_kind);
        }
    }

    /// Phase 37 — Probe report written to disk. The Mem variant is a silent no-op (tests do not need persistence either).
    pub fn set_probe_status(
        &mut self,
        plugin_id: &str,
        status: &str,
        report: Option<&str>,
        at: u64,
    ) {
        if let StoreEnum::Db(x) = self {
            x.set_probe_status(plugin_id, status, report, at);
        }
    }

    /// Phase 37 — read the probe report. The Mem variant returns None.
    pub fn get_probe_report(&self, plugin_id: &str) -> Option<(String, u64, String)> {
        match self {
            StoreEnum::Db(x) => x.get_probe_report(plugin_id),
            StoreEnum::Mem(_) => None,
        }
    }

    /// Phase 38 — set the plugin channel. The Mem variant is a silent no-op.
    pub fn set_plugin_channel(&mut self, plugin_id: &str, channel: &str) {
        if let StoreEnum::Db(x) = self {
            x.set_plugin_channel(plugin_id, channel);
        }
    }

    /// Phase 38 — batch-read all (plugin_id, channel). The Mem variant returns empty.
    pub fn list_plugin_channels(&self) -> Vec<(String, String)> {
        match self {
            StoreEnum::Db(x) => x.list_plugin_channels(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Phase 38 — global key/value. The Mem variant returns None.
    pub fn get_setting(&self, key: &str) -> Option<String> {
        match self {
            StoreEnum::Db(x) => x.get_setting(key),
            StoreEnum::Mem(_) => None,
        }
    }

    /// Phase 38 — global key/value. The Mem variant is a silent no-op.
    pub fn set_setting(&mut self, key: &str, value: &str) {
        if let StoreEnum::Db(x) = self {
            x.set_setting(key, value);
        }
    }

    /// Phase 39 — full hotkey list (the Mem variant returns empty).
    pub fn list_hotkeys(&self) -> Vec<HotkeyRow> {
        match self {
            StoreEnum::Db(x) => x.list_hotkeys(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Phase 40 — read health config (the Mem variant returns the default).
    pub fn get_health_config(&self, plugin_id: &str) -> crate::core::health::PluginHealthConfig {
        match self {
            StoreEnum::Db(x) => x.get_health_config(plugin_id),
            StoreEnum::Mem(_) => crate::core::health::PluginHealthConfig::default(),
        }
    }

    /// Phase 40 — upsert health config (the Mem variant is a no-op).
    pub fn upsert_health_config(
        &mut self,
        plugin_id: &str,
        cfg: &crate::core::health::PluginHealthConfig,
    ) {
        if let StoreEnum::Db(x) = self {
            x.upsert_health_config(plugin_id, cfg);
        }
    }

    /// Phase 40 — all health configs (the Mem variant returns empty).
    pub fn list_health_configs(&self) -> Vec<(String, crate::core::health::PluginHealthConfig)> {
        match self {
            StoreEnum::Db(x) => x.list_health_configs(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    // ─── Phase 53: alerting_severity_hints forwarding ────────────────────────────────

    pub fn list_alerting_severity_hints(&self) -> Vec<SeverityHintRow> {
        match self {
            StoreEnum::Db(x) => x.list_alerting_severity_hints(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn upsert_alerting_severity_hint(
        &mut self,
        source: &str,
        severity: &str,
        origin: &str,
        plugin_id: Option<&str>,
        updated_at: i64,
    ) -> Result<(), String> {
        match self {
            StoreEnum::Db(x) => {
                x.upsert_alerting_severity_hint(source, severity, origin, plugin_id, updated_at)
            }
            StoreEnum::Mem(_) => Ok(()),
        }
    }

    pub fn get_alerting_severity_hint(
        &self,
        source: &str,
        origin: &str,
    ) -> Option<SeverityHintRow> {
        match self {
            StoreEnum::Db(x) => x.get_alerting_severity_hint(source, origin),
            StoreEnum::Mem(_) => None,
        }
    }

    pub fn delete_alerting_severity_hint_by_source_and_origin(
        &mut self,
        source: &str,
        origin: &str,
    ) -> bool {
        match self {
            StoreEnum::Db(x) => {
                x.delete_alerting_severity_hint_by_source_and_origin(source, origin)
            }
            StoreEnum::Mem(_) => false,
        }
    }

    pub fn delete_alerting_severity_hints_by_origin_plugin(
        &mut self,
        origin: &str,
        plugin_id: &str,
    ) -> usize {
        match self {
            StoreEnum::Db(x) => {
                x.delete_alerting_severity_hints_by_origin_plugin(origin, plugin_id)
            }
            StoreEnum::Mem(_) => 0,
        }
    }

    pub fn clear_user_alerting_severity_hints(&mut self) -> usize {
        match self {
            StoreEnum::Db(x) => x.clear_user_alerting_severity_hints(),
            StoreEnum::Mem(_) => 0,
        }
    }

    // ─── Phase 54: alerting_aggregations forwarding ──────────────────────────────────

    pub fn list_alerting_aggregations(&self) -> Vec<AggregationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_alerting_aggregations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn list_enabled_alerting_aggregations(&self) -> Vec<AggregationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_enabled_alerting_aggregations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn upsert_alerting_aggregation(&mut self, row: &AggregationRuleRow) {
        match self {
            StoreEnum::Db(x) => x.upsert_alerting_aggregation(row),
            StoreEnum::Mem(_) => {}
        }
    }

    pub fn delete_alerting_aggregation(&mut self, id: &str) -> bool {
        match self {
            StoreEnum::Db(x) => x.delete_alerting_aggregation(id),
            StoreEnum::Mem(_) => false,
        }
    }

    pub fn clear_alerting_aggregations(&mut self) -> usize {
        match self {
            StoreEnum::Db(x) => x.clear_alerting_aggregations(),
            StoreEnum::Mem(_) => 0,
        }
    }

    // ─── Phase 55: alerting_correlations forwarding ──────────────────────────────────

    pub fn list_alerting_correlations(&self) -> Vec<CorrelationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_alerting_correlations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn list_enabled_alerting_correlations(&self) -> Vec<CorrelationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_enabled_alerting_correlations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn upsert_alerting_correlation(&mut self, row: &CorrelationRuleRow) {
        match self {
            StoreEnum::Db(x) => x.upsert_alerting_correlation(row),
            StoreEnum::Mem(_) => {}
        }
    }

    pub fn delete_alerting_correlation(&mut self, id: &str) -> bool {
        match self {
            StoreEnum::Db(x) => x.delete_alerting_correlation(id),
            StoreEnum::Mem(_) => false,
        }
    }

    pub fn clear_alerting_correlations(&mut self) -> usize {
        match self {
            StoreEnum::Db(x) => x.clear_alerting_correlations(),
            StoreEnum::Mem(_) => 0,
        }
    }

    // ─── Phase 56: alerting_escalations forwarding ───────────────────────────────────

    pub fn list_alerting_escalations(&self) -> Vec<EscalationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_alerting_escalations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn list_enabled_alerting_escalations(&self) -> Vec<EscalationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_enabled_alerting_escalations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn upsert_alerting_escalation(&mut self, row: &EscalationRuleRow) {
        match self {
            StoreEnum::Db(x) => x.upsert_alerting_escalation(row),
            StoreEnum::Mem(_) => {}
        }
    }

    pub fn delete_alerting_escalation(&mut self, id: &str) -> bool {
        match self {
            StoreEnum::Db(x) => x.delete_alerting_escalation(id),
            StoreEnum::Mem(_) => false,
        }
    }

    pub fn clear_alerting_escalations(&mut self) -> usize {
        match self {
            StoreEnum::Db(x) => x.clear_alerting_escalations(),
            StoreEnum::Mem(_) => 0,
        }
    }
}

pub type SharedStore = Arc<Mutex<StoreEnum>>;

/// O1 — shared entry point for event-table retention cleanup: besides the once-at-startup pass (see `Storage::open`), the retention worker
/// calls it every 24h, keeping events bounded even when the long-running desktop process is never restarted. Returns the deleted row count.
pub(crate) fn prune_events_shared(store: &SharedStore) -> usize {
    let cutoff = crate::core::agent::now_secs().saturating_sub(EVENT_RETENTION_DAYS * 86400);
    store
        .lock()
        .ok()
        .and_then(|mut s| {
            s.with_conn(|c| {
                c.execute("DELETE FROM events WHERE timestamp < ?1", [cutoff])
                    .unwrap_or(0)
            })
        })
        .unwrap_or(0)
}
