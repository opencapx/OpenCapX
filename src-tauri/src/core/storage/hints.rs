//! alerting severity hints (manifest + user).
//! Mechanical move from core/storage/alerting_tables.rs.

use super::*;

impl Storage {
    /// All hints (by source ASC). Added in Phase 53.
    pub fn list_alerting_severity_hints(&self) -> Vec<SeverityHintRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT source, severity, origin, plugin_id, updated_at
             FROM alerting_severity_hints ORDER BY source ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok(SeverityHintRow {
                source: r.get(0)?,
                severity: r.get(1)?,
                origin: r.get(2)?,
                plugin_id: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })
        .ok()
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// Upsert a hint. Added in Phase 53.
    pub fn upsert_alerting_severity_hint(
        &mut self,
        source: &str,
        severity: &str,
        origin: &str,
        plugin_id: Option<&str>,
        updated_at: i64,
    ) -> Result<(), String> {
        let ok = self.conn.execute(
            "INSERT INTO alerting_severity_hints (source, severity, origin, plugin_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source, origin) DO UPDATE SET severity=?2, plugin_id=?4, updated_at=?5",
            rusqlite::params![source, severity, origin, plugin_id, updated_at],
        );
        match ok {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("upsert severity hint failed: {e}")),
        }
    }

    pub fn delete_alerting_severity_hint_by_source_and_origin(
        &mut self,
        source: &str,
        origin: &str,
    ) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_severity_hints WHERE source = ?1 AND origin = ?2",
                rusqlite::params![source, origin],
            )
            .unwrap_or(0)
            > 0
    }

    /// Batch-delete hints matching (origin, plugin_id), returning the delete count. Added in Phase 53.
    pub fn delete_alerting_severity_hints_by_origin_plugin(
        &mut self,
        origin: &str,
        plugin_id: &str,
    ) -> usize {
        self.conn
            .execute(
                "DELETE FROM alerting_severity_hints WHERE origin = ?1 AND plugin_id = ?2",
                rusqlite::params![origin, plugin_id],
            )
            .unwrap_or(0)
    }

    /// Clear all hints with origin user. Added in Phase 53, returns the delete count.
    pub fn clear_user_alerting_severity_hints(&mut self) -> usize {
        self.conn
            .execute(
                "DELETE FROM alerting_severity_hints WHERE origin = 'user'",
                [],
            )
            .unwrap_or(0)
    }

    /// Look up the hint matching (source, origin) (single row). Added in Phase 53.
    pub fn get_alerting_severity_hint(
        &self,
        source: &str,
        origin: &str,
    ) -> Option<SeverityHintRow> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source, severity, origin, plugin_id, updated_at
             FROM alerting_severity_hints WHERE source = ?1 AND origin = ?2 LIMIT 1",
            )
            .ok()?;
        stmt.query_row(rusqlite::params![source, origin], |r| {
            Ok(SeverityHintRow {
                source: r.get(0)?,
                severity: r.get(1)?,
                origin: r.get(2)?,
                plugin_id: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })
        .ok()
    }
}
