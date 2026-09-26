//! alerting endpoints (multi-endpoint fanout).
//! Mechanical move from core/storage/alerting_tables.rs.

use super::*;

impl Storage {
    fn parse_endpoint_row(
        &self,
        id: String,
        name: String,
        url: String,
        enabled: i64,
        headers_json: String,
        secret: String,
        source_filter_json: String,
        created_at: i64,
        schema_version: i64,
        template: Option<String>,
        template_sample: Option<String>,
        severity_overrides: Option<String>,
    ) -> AlertingEndpointRow {
        let headers: Vec<(String, String)> =
            serde_json::from_str(&headers_json).unwrap_or_default();
        let source_filter: Vec<String> =
            serde_json::from_str(&source_filter_json).unwrap_or_default();
        AlertingEndpointRow {
            id,
            name,
            url,
            enabled: enabled != 0,
            headers,
            secret,
            source_filter,
            created_at: created_at as u64,
            schema_version: schema_version.max(0) as u32,
            template,
            template_sample,
            severity_overrides,
        }
    }

    /// List all endpoints, by created_at ascending.
    pub fn list_alerting_endpoints(&self) -> Vec<AlertingEndpointRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, url, enabled, headers_json, secret, source_filter_json, created_at, schema_version, template, template_sample, severity_overrides_json
               FROM alerting_endpoints ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .ok()
        .map(|i| {
            i.filter_map(|x| x.ok())
                .map(
                    |(id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so)| {
                        self.parse_endpoint_row(
                            id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so,
                        )
                    },
                )
                .collect()
        })
        .unwrap_or_default()
    }

    /// List all enabled endpoints — used during dispatcher fanout.
    pub fn list_enabled_alerting_endpoints(&self) -> Vec<AlertingEndpointRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, url, enabled, headers_json, secret, source_filter_json, created_at, schema_version, template, template_sample, severity_overrides_json
               FROM alerting_endpoints WHERE enabled = 1 ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .ok()
        .map(|i| {
            i.filter_map(|x| x.ok())
                .map(
                    |(id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so)| {
                        self.parse_endpoint_row(
                            id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so,
                        )
                    },
                )
                .collect()
        })
        .unwrap_or_default()
    }

    /// Get a single endpoint by id.
    pub fn get_alerting_endpoint(&self, id: &str) -> Option<AlertingEndpointRow> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, url, enabled, headers_json, secret, source_filter_json, created_at, schema_version, template, template_sample, severity_overrides_json
                   FROM alerting_endpoints WHERE id = ?1",
            )
            .ok()?;
        stmt.query_row(rusqlite::params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .ok()
        .map(
            |(id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so)| {
                self.parse_endpoint_row(id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so)
            },
        )
    }

    /// Upsert: update when there is an id, otherwise use the passed-in id (generated by the caller).
    pub fn upsert_alerting_endpoint(&mut self, row: &AlertingEndpointRow) {
        let headers_json = serde_json::to_string(&row.headers).unwrap_or_else(|_| "[]".into());
        let source_filter_json =
            serde_json::to_string(&row.source_filter).unwrap_or_else(|_| "[]".into());
        let _ = self.conn.execute(
            "INSERT INTO alerting_endpoints
                (id, name, url, enabled, headers_json, secret, source_filter_json, created_at, schema_version, template, template_sample, severity_overrides_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name,
                url=excluded.url,
                enabled=excluded.enabled,
                headers_json=excluded.headers_json,
                secret=excluded.secret,
                source_filter_json=excluded.source_filter_json,
                schema_version=excluded.schema_version,
                template=excluded.template,
                template_sample=excluded.template_sample,
                severity_overrides_json=excluded.severity_overrides_json",
            rusqlite::params![
                row.id,
                row.name,
                row.url,
                if row.enabled { 1i64 } else { 0i64 },
                headers_json,
                row.secret,
                source_filter_json,
                row.created_at as i64,
                row.schema_version as i64,
                row.template.as_deref(),
                row.template_sample.as_deref(),
                row.severity_overrides.as_deref(),
            ],
        );
    }

    /// Delete an endpoint, cascading cleanup of its dead letters. Returns (endpoint_deleted, deliveries_deleted).
    pub fn delete_alerting_endpoint(&mut self, id: &str) -> (bool, usize) {
        let n = self
            .conn
            .execute(
                "DELETE FROM alerting_endpoints WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0);
        let cleared = self.delete_failed_deliveries_for_endpoint(id);
        (n > 0, cleared)
    }
}
