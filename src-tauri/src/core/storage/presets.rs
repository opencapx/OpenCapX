//! user template presets.
//! Mechanical move from core/storage/alerting_tables.rs.

use super::*;

impl Storage {
    /// List user-defined presets (builtins always live in the `core::alerting::BUILTIN_PRESETS` constant).
    pub fn list_user_template_presets(&self) -> Result<Vec<TemplatePresetRow>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, kind, template, sample, builtin, version, changelog, created_at
               FROM template_presets WHERE builtin = 0 ORDER BY created_at ASC",
        ).map_err(|e| format!("prepare list_user_template_presets: {}", e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, i64>(9)?,
                ))
            })
            .map_err(|e| format!("query list_user_template_presets: {}", e))?;
        let mut out = Vec::new();
        for row in rows {
            let (
                id,
                name,
                description,
                kind,
                template,
                sample,
                builtin,
                version,
                changelog,
                created_at,
            ) = row.map_err(|e| format!("row: {}", e))?;
            out.push(TemplatePresetRow {
                id,
                name,
                description,
                kind,
                template,
                sample,
                builtin: builtin != 0,
                version: version.max(1) as u32,
                changelog,
                created_at: created_at.max(0) as u64,
            });
        }
        Ok(out)
    }

    /// Upsert preset; `builtin=true` is also allowed (so builtin rows can still be persisted when exported/imported).
    /// Phase 60 — adds `version` + `changelog` columns; upsert writes both (allows resetting version = 1 on import).
    pub fn upsert_template_preset(&mut self, p: &TemplatePresetRow) -> Result<(), String> {
        let _ = self.conn.execute(
            "INSERT INTO template_presets
                (id, name, description, kind, template, sample, builtin, version, changelog, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name,
                description=excluded.description,
                kind=excluded.kind,
                template=excluded.template,
                sample=excluded.sample,
                builtin=excluded.builtin,
                version=excluded.version,
                changelog=excluded.changelog",
            rusqlite::params![
                p.id,
                p.name,
                p.description.as_deref(),
                p.kind,
                p.template,
                p.sample.as_deref(),
                if p.builtin { 1i64 } else { 0i64 },
                p.version as i64,
                p.changelog.as_deref(),
                p.created_at as i64,
            ],
        ).map_err(|e| format!("upsert template_preset: {}", e))?;
        Ok(())
    }

    /// Delete only builtin=0 rows (avoid accidental deletion of built-ins). Returns true when actually deleted.
    pub fn delete_template_preset(&mut self, id: &str) -> bool {
        let n = self
            .conn
            .execute(
                "DELETE FROM template_presets WHERE id = ?1 AND builtin = 0",
                rusqlite::params![id],
            )
            .unwrap_or(0);
        n > 0
    }
}
