//! alerting routes (DSL when/then).
//! Mechanical move from core/storage/alerting_tables.rs.

use super::*;

impl Storage {
    /// All DSL route rules (by priority ASC, then created_at ASC). Added in Phase 51.
    pub fn list_alerting_routes(&self) -> Vec<RouteRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, priority, enabled, kind_pattern, target_endpoint_ids_json,
                    payload_path, payload_match, tags_json, recipients_json, seen_in_last_json, created_at
             FROM alerting_routes ORDER BY priority ASC, created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            let ids_json: String = r.get(5)?;
            let tags_json: Option<String> = r.get(8)?;
            let recipients_json: Option<String> = r.get(9)?;
            let seen_in_last_json: Option<String> = r.get(10)?;
            let target_endpoint_ids: Vec<String> =
                serde_json::from_str(&ids_json).unwrap_or_default();
            let tags: Vec<String> = tags_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let recipients: Vec<String> = recipients_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            Ok(RouteRuleRow {
                id: r.get(0)?,
                name: r.get(1)?,
                priority: r.get(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                kind_pattern: r.get(4)?,
                payload_path: r.get(6)?,
                payload_match: r.get(7)?,
                target_endpoint_ids,
                recipients,
                tags,
                seen_in_last_json,
                created_at: r.get(11)?,
            })
        })
        .ok()
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// Upsert a route (id overwrites; created_at keeps the old value to avoid UI jumps). Added in Phase 51.
    pub fn upsert_alerting_route(&mut self, row: &RouteRuleRow) {
        let ids_json =
            serde_json::to_string(&row.target_endpoint_ids).unwrap_or_else(|_| "[]".into());
        let tags_json = serde_json::to_string(&row.tags).unwrap_or_else(|_| "[]".into());
        let recipients_json =
            serde_json::to_string(&row.recipients).unwrap_or_else(|_| "[]".into());
        let _ = self.conn.execute(
            "INSERT INTO alerting_routes
             (id, name, priority, enabled, kind_pattern, payload_path, payload_match,
              target_endpoint_ids_json, tags_json, recipients_json, seen_in_last_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name,
                priority=excluded.priority,
                enabled=excluded.enabled,
                kind_pattern=excluded.kind_pattern,
                payload_path=excluded.payload_path,
                payload_match=excluded.payload_match,
                target_endpoint_ids_json=excluded.target_endpoint_ids_json,
                tags_json=excluded.tags_json,
                recipients_json=excluded.recipients_json,
                seen_in_last_json=excluded.seen_in_last_json",
            rusqlite::params![
                row.id,
                row.name,
                row.priority,
                row.enabled as i64,
                row.kind_pattern,
                row.payload_path,
                row.payload_match,
                ids_json,
                tags_json,
                recipients_json,
                row.seen_in_last_json,
                row.created_at as i64,
            ],
        );
    }

    /// Delete a route. Added in Phase 51.
    pub fn delete_alerting_route(&mut self, id: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_routes WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0)
            > 0
    }

    /// Clear all routes (for atomic import). Added in Phase 51.
    pub fn clear_alerting_routes(&mut self) -> usize {
        self.conn
            .execute("DELETE FROM alerting_routes", [])
            .unwrap_or(0)
    }
}
