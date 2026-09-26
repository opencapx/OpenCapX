//! aggregation rules.
//! Mechanical move from core/storage/alerting_tables.rs.

use super::*;

impl Storage {
    fn aggregation_row_from_stmt(r: &rusqlite::Row) -> rusqlite::Result<AggregationRuleRow> {
        Ok(AggregationRuleRow {
            id: r.get(0)?,
            name: r.get(1)?,
            kind_pattern: r.get(2)?,
            window_secs: r.get::<_, i64>(3)? as u64,
            threshold_count: r.get::<_, i64>(4)? as u32,
            action: r.get(5)?,
            target_severity: r.get(6)?,
            enabled: r.get::<_, i64>(7)? != 0,
            created_at: r.get::<_, i64>(8)? as u64,
        })
    }

    /// All frequency-threshold rules (added in Phase 54). Phase 51 routes sort by priority similarly; here it is by created_at ASC.
    pub fn list_alerting_aggregations(&self) -> Vec<AggregationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern, window_secs, threshold_count, action,
                    target_severity, enabled, created_at
             FROM alerting_aggregations ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::aggregation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Enabled rules only (added in Phase 54).
    pub fn list_enabled_alerting_aggregations(&self) -> Vec<AggregationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern, window_secs, threshold_count, action,
                    target_severity, enabled, created_at
             FROM alerting_aggregations WHERE enabled = 1 ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::aggregation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Upsert an aggregation rule (added in Phase 54). id overwrites; created_at keeps the old value (to avoid UI time jumps).
    pub fn upsert_alerting_aggregation(&mut self, row: &AggregationRuleRow) {
        let _ = self.conn.execute(
            "INSERT INTO alerting_aggregations
             (id, name, kind_pattern, window_secs, threshold_count, action,
              target_severity, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET name=?2, kind_pattern=?3, window_secs=?4,
                threshold_count=?5, action=?6, target_severity=?7, enabled=?8",
            rusqlite::params![
                row.id,
                row.name,
                row.kind_pattern,
                row.window_secs as i64,
                row.threshold_count as i64,
                row.action,
                row.target_severity,
                row.enabled as i64,
                row.created_at as i64,
            ],
        );
    }

    pub fn delete_alerting_aggregation(&mut self, id: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_aggregations WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0)
            > 0
    }

    pub fn clear_alerting_aggregations(&mut self) -> usize {
        self.conn
            .execute("DELETE FROM alerting_aggregations", [])
            .unwrap_or(0)
    }
}
