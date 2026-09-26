//! escalation rules.
//! Mechanical move from core/storage/alerting_tables.rs.

use super::*;

impl Storage {
    fn escalation_row_from_stmt(r: &rusqlite::Row) -> rusqlite::Result<EscalationRuleRow> {
        let ids_json: String = r.get(5)?;
        let target_endpoint_ids: Option<Vec<String>> = if ids_json.is_empty() || ids_json == "[]" {
            None
        } else {
            serde_json::from_str(&ids_json).ok()
        };
        Ok(EscalationRuleRow {
            id: r.get(0)?,
            name: r.get(1)?,
            kind_pattern: r.get(2)?,
            escalate_after_secs: r.get::<_, i64>(3)? as u64,
            target_severity: r.get(4)?,
            target_endpoint_ids,
            enabled: r.get::<_, i64>(6)? != 0,
            created_at: r.get::<_, i64>(7)? as u64,
        })
    }

    pub fn list_alerting_escalations(&self) -> Vec<EscalationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern, escalate_after_secs, target_severity,
                    target_endpoint_ids, enabled, created_at
             FROM alerting_escalations ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::escalation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn list_enabled_alerting_escalations(&self) -> Vec<EscalationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern, escalate_after_secs, target_severity,
                    target_endpoint_ids, enabled, created_at
             FROM alerting_escalations WHERE enabled = 1 ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::escalation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn upsert_alerting_escalation(&mut self, row: &EscalationRuleRow) {
        let ids_json =
            serde_json::to_string(row.target_endpoint_ids.as_ref().unwrap_or(&Vec::new()))
                .unwrap_or_else(|_| "[]".into());
        let _ = self.conn.execute(
            "INSERT INTO alerting_escalations
             (id, name, kind_pattern, escalate_after_secs, target_severity,
              target_endpoint_ids, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET name=?2, kind_pattern=?3, escalate_after_secs=?4,
                target_severity=?5, target_endpoint_ids=?6, enabled=?7",
            rusqlite::params![
                row.id,
                row.name,
                row.kind_pattern,
                row.escalate_after_secs as i64,
                row.target_severity,
                ids_json,
                row.enabled as i64,
                row.created_at as i64,
            ],
        );
    }

    pub fn delete_alerting_escalation(&mut self, id: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_escalations WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0)
            > 0
    }

    pub fn clear_alerting_escalations(&mut self) -> usize {
        self.conn
            .execute("DELETE FROM alerting_escalations", [])
            .unwrap_or(0)
    }
}
