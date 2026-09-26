//! alerting recipients.
//! Mechanical move from core/storage/alerting_tables.rs.

use super::*;

impl Storage {
    /// All recipients (by created_at ASC). Added in Phase 67.
    pub fn list_alerting_recipients(&self) -> Vec<RecipientRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind, config_json, enabled, created_at
             FROM alerting_recipients ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            let cfg: String = r.get(3)?;
            let config: serde_json::Value =
                serde_json::from_str(&cfg).unwrap_or_else(|_| serde_json::json!({}));
            Ok(RecipientRow {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                config,
                enabled: r.get::<_, i64>(4)? != 0,
                created_at: r.get::<_, i64>(5)? as u64,
            })
        })
        .ok()
        .map(|it| it.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// Upsert recipient. A `name` UNIQUE conflict → returns Err (handled by the caller's upper layer).
    /// Added in Phase 67.
    pub fn upsert_alerting_recipient(&mut self, row: &RecipientRow) -> Result<(), String> {
        let config_json = serde_json::to_string(&row.config).unwrap_or_else(|_| "{}".into());
        self.conn
            .execute(
                "INSERT INTO alerting_recipients
                    (id, name, kind, config_json, enabled, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                    name=excluded.name,
                    kind=excluded.kind,
                    config_json=excluded.config_json,
                    enabled=excluded.enabled",
                rusqlite::params![
                    row.id,
                    row.name,
                    row.kind,
                    config_json,
                    if row.enabled { 1i64 } else { 0i64 },
                    row.created_at as i64,
                ],
            )
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("UNIQUE") {
                    format!("recipient name '{}' already exists", row.name)
                } else {
                    format!("upsert recipient: {msg}")
                }
            })?;
        Ok(())
    }

    /// Delete a recipient + cascade-clean dangling refs to this id in routes.recipients.
    /// Returns (deleted, routes_cleared). Added in Phase 67.
    pub fn delete_alerting_recipient(&mut self, id: &str) -> (bool, usize) {
        let n = self
            .conn
            .execute(
                "DELETE FROM alerting_recipients WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0);
        let mut cleared = 0usize;
        if n > 0 {
            // collect routes referencing this id (`webhook:{id}` or `recipient:{id}`)
            let rows = self.list_alerting_routes();
            for row in rows {
                let before = row.recipients.len();
                let mut new_row = row.clone();
                let filtered: Vec<String> = new_row
                    .recipients
                    .drain(..)
                    .filter(|c| {
                        !(c.starts_with(&format!("webhook:{id}"))
                            || c.starts_with(&format!("recipient:{id}")))
                    })
                    .collect();
                if filtered.len() < before {
                    new_row.recipients = filtered;
                    self.upsert_alerting_route(&new_row);
                    cleared += 1;
                }
            }
        }
        (n > 0, cleared)
    }
}
