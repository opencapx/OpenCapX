//! correlation rules.
//! Mechanical move from core/storage/alerting_tables.rs.

use super::*;

impl Storage {
    fn correlation_row_from_stmt(r: &rusqlite::Row) -> rusqlite::Result<CorrelationRuleRow> {
        Ok(CorrelationRuleRow {
            id: r.get(0)?,
            name: r.get(1)?,
            kind_pattern_a: r.get(2)?,
            kind_pattern_b: r.get(3)?,
            window_secs: r.get::<_, i64>(4)? as u64,
            enabled: r.get::<_, i64>(5)? != 0,
            created_at: r.get::<_, i64>(6)? as u64,
        })
    }

    pub fn list_alerting_correlations(&self) -> Vec<CorrelationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern_a, kind_pattern_b, window_secs,
                    enabled, created_at
             FROM alerting_correlations ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::correlation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn list_enabled_alerting_correlations(&self) -> Vec<CorrelationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern_a, kind_pattern_b, window_secs,
                    enabled, created_at
             FROM alerting_correlations WHERE enabled = 1 ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::correlation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn upsert_alerting_correlation(&mut self, row: &CorrelationRuleRow) {
        let _ = self.conn.execute(
            "INSERT INTO alerting_correlations
             (id, name, kind_pattern_a, kind_pattern_b, window_secs, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET name=?2, kind_pattern_a=?3, kind_pattern_b=?4,
                window_secs=?5, enabled=?6",
            rusqlite::params![
                row.id,
                row.name,
                row.kind_pattern_a,
                row.kind_pattern_b,
                row.window_secs as i64,
                row.enabled as i64,
                row.created_at as i64,
            ],
        );
    }

    pub fn delete_alerting_correlation(&mut self, id: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_correlations WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0)
            > 0
    }

    pub fn clear_alerting_correlations(&mut self) -> usize {
        self.conn
            .execute("DELETE FROM alerting_correlations", [])
            .unwrap_or(0)
    }
}
