//! template preset library: built-ins, user presets, YAML import/export, schema migration.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Phase 59 — One template preset (user-defined + 5 built-in constants).
/// `kind` distinguishes builtin / user: builtin uses `"builtin:<slug>"`, user uses `"user:<uuid>"`.
/// Phase 60 — adds `version: u32` (auto-bumped on each save of the same id; 1 the first time) + `changelog: String` (optional notes).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplatePreset {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub kind: String,
    pub template: String,
    #[serde(default)]
    pub sample: String,
    #[serde(default)]
    pub builtin: bool,
    #[serde(default, rename = "createdAt")]
    pub created_at: u64,
    #[serde(default = "default_template_preset_version")]
    pub version: u32,
    #[serde(default)]
    pub changelog: String,
}

fn default_template_preset_version() -> u32 {
    1
}

/// Phase 59 — YAML / JSON document schema. The `version` field is used for future schema migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetYamlDoc {
    #[serde(default = "default_preset_doc_version")]
    pub version: u32,
    pub presets: Vec<TemplatePreset>,
}

fn default_preset_doc_version() -> u32 {
    1
}

/// Phase 61 — Current YAML/JSON document schema version. Bump when new fields are added to `TemplatePreset`.
pub const CURRENT_DOC_VERSION: u32 = 1;

/// Phase 61 — A single migration rule: `from_version → to_version` runs the `migrate` function.
/// `name` is used for logs / error diagnostics (`migrate v0→v1 (v0_to_v1) failed: ...`).
pub struct PresetMigration {
    pub from: u32,
    pub to: u32,
    pub name: &'static str,
    pub migrate: fn(PresetYamlDoc) -> Result<PresetYamlDoc, String>,
}

/// Phase 61 — v0 (yaml has no `version` field at all, or an explicit `version: 0`) → v1 migration.
/// Mainly: fill doc.version = 1; default each preset's version field to 1 (a field added in Phase 60+).
fn migrate_v0_to_v1(mut doc: PresetYamlDoc) -> Result<PresetYamlDoc, String> {
    doc.version = 1;
    for p in doc.presets.iter_mut() {
        if p.version == 0 {
            p.version = 1;
        }
        if p.changelog.is_empty() {
            // the v0 schema has no changelog field; serde default gives "", leaving it as-is.
        }
        if p.kind.is_empty() {
            // when a preset in a v0 document has no kind field (rare), default to the user namespace
            if p.id.is_empty() {
                p.id = gen_event_id();
            }
            p.kind = format!("user:{}", p.id);
        }
    }
    Ok(doc)
}

/// Phase 61 — Global migration table. Sorted by `from` ascending; `migrate_preset_yaml` finds the next step and runs it in turn.
/// Currently only `0 → 1`. When Phase 62+ adds fields, bump CURRENT_DOC_VERSION and append an entry.
pub const MIGRATORS: &[PresetMigration] = &[PresetMigration {
    from: 0,
    to: 1,
    name: "v0_to_v1",
    migrate: migrate_v0_to_v1,
}];

/// Phase 61 — Parse YAML / JSON and upgrade along the MIGRATORS chain to CURRENT_DOC_VERSION.
/// Returns (migrated_doc, applied_migration_names) — the former for upsert, the latter for the frontend import notice.
pub fn migrate_preset_yaml(yaml: &str) -> Result<(PresetYamlDoc, Vec<&'static str>), String> {
    let mut doc: PresetYamlDoc =
        serde_yaml::from_str(yaml).map_err(|e| format!("yaml parse: {}", e))?;
    let mut applied: Vec<&'static str> = Vec::new();
    while doc.version < CURRENT_DOC_VERSION {
        let next = MIGRATORS
            .iter()
            .find(|m| m.from == doc.version)
            .ok_or_else(|| {
                format!(
                    "no migrator available for doc version {} (current = {})",
                    doc.version, CURRENT_DOC_VERSION
                )
            })?;
        let from = doc.version;
        doc = (next.migrate)(doc)?;
        // defensive: if the migrator did not update version correctly, fix it manually here
        if doc.version != next.to {
            eprintln!(
                "[alerting::migrate_preset_yaml] WARNING: migrator '{}' returned doc.version={} (expected {})",
                next.name, doc.version, next.to
            );
            doc.version = next.to;
        }
        eprintln!(
            "[alerting::migrate_preset_yaml] applied {} ({} → {})",
            next.name, from, doc.version
        );
        applied.push(next.name);
        // guard against infinite loops: even a buggy migrator must not run forever
        if applied.len() > 16 {
            return Err(
                "migration chain too long (>16 steps); aborting to prevent infinite loop".into(),
            );
        }
    }
    if doc.version > CURRENT_DOC_VERSION {
        return Err(format!(
            "doc version {} is newer than current {}; refusing to import future schema",
            doc.version, CURRENT_DOC_VERSION
        ));
    }
    Ok((doc, applied))
}

/// Phase 59 — 5 built-in presets. Covers the most common webhook receivers.
/// Cached with `OnceLock` (avoids const-fn limits); all callers get them through the `builtin_presets()` function.
pub fn builtin_presets() -> &'static [TemplatePreset] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<TemplatePreset>> = OnceLock::new();
    CACHE.get_or_init(|| {
        vec![
            TemplatePreset {
                id: "builtin-slack".into(),
                name: "Slack incoming webhook".into(),
                description: "Slack-compatible JSON: {\"text\":\"<msg>\"}. Mentions @oncall if tags.oncall set.".into(),
                kind: "builtin:slack".into(),
                template: r#"{
  "text": "[{{severity}}] {{source}}: {{payload.message}}{{#if payload.tags.oncall}} @{{payload.tags.oncall}}{{/if}}",
  "username": "OpenCapX",
  "icon_emoji": ":warning:"
}"#.into(),
                sample: r#"{"message":"p99 latency exceeded 500ms","tags":{"oncall":"alice"}}"#.into(),
                builtin: true,
                created_at: 0,
                version: 1,
                changelog: String::new(),
            },
            TemplatePreset {
                id: "builtin-discord".into(),
                name: "Discord webhook".into(),
                description: "Discord-compatible JSON: {\"content\":\"<msg>\"}.".into(),
                kind: "builtin:discord".into(),
                template: r#"{
  "content": "[{{severity}}] {{source}}: {{payload.message}}",
  "username": "OpenCapX"
}"#.into(),
                sample: r#"{"message":"CPU 95% on host web-1"}"#.into(),
                builtin: true,
                created_at: 0,
                version: 1,
                changelog: String::new(),
            },
            TemplatePreset {
                id: "builtin-msteams".into(),
                name: "Microsoft Teams webhook".into(),
                description: "Teams MessageCard with themeColor switching by severity.".into(),
                kind: "builtin:msteams".into(),
                template: r#"{
  "@type": "MessageCard",
  "@context": "https://schema.org/extensions",
  "themeColor": "{{#if payload.severity_eq_critical}}FF0000{{else}}{{#if payload.severity_eq_error}}FFA500{{else}}{{#if payload.severity_eq_warn}}FFCC00{{else}}00CC00{{/if}}{{/if}}{{/if}}",
  "title": "[{{severity}}] {{source}}",
  "text": "{{payload.message}}"
}"#.into(),
                sample: r#"{"message":"DB connection pool exhausted"}"#.into(),
                builtin: true,
                created_at: 0,
                version: 1,
                changelog: String::new(),
            },
            TemplatePreset {
                id: "builtin-generic-json".into(),
                name: "Generic JSON envelope".into(),
                description: "Pass the canonical envelope's payload through verbatim.".into(),
                kind: "builtin:generic_json".into(),
                template: "{{payload}}".into(),
                sample: r#"{"any":"json","nested":{"k":"v"}}"#.into(),
                builtin: true,
                created_at: 0,
                version: 1,
                changelog: String::new(),
            },
            TemplatePreset {
                id: "builtin-plain-text".into(),
                name: "Plain text".into(),
                description: "Single-line plain text format. Content-Type: text/plain.".into(),
                kind: "builtin:plain_text".into(),
                template: "[{{severity}}] {{source}} at {{timestamp}}: {{payload.message}}".into(),
                sample: r#"{"message":"hello world"}"#.into(),
                builtin: true,
                created_at: 0,
                version: 1,
                changelog: String::new(),
            },
        ]
    })
}

/// Phase 59 — builtin + user presets. If storage is uninitialized, only builtins are returned.
pub fn list_template_presets() -> Vec<TemplatePreset> {
    let mut out: Vec<TemplatePreset> = builtin_presets().to_vec();
    let Some(store) = crate::core::shared_store() else {
        return out;
    };
    let Ok(s) = store.lock() else { return out };
    let StoreEnum::Db(db) = &*s else { return out };
    if let Ok(user_rows) = db.list_user_template_presets() {
        for r in user_rows {
            out.push(TemplatePreset {
                id: r.id,
                name: r.name,
                description: r.description.unwrap_or_default(),
                kind: r.kind,
                template: r.template,
                sample: r.sample.unwrap_or_default(),
                builtin: r.builtin,
                created_at: r.created_at,
                version: r.version,
                changelog: r.changelog.unwrap_or_default(),
            });
        }
    }
    out
}

/// Phase 59 — Fetch a single preset by kind (returns both builtin and user).
pub fn get_template_preset(kind: &str) -> Option<TemplatePreset> {
    if let Some(b) = builtin_presets().iter().find(|p| p.kind == kind).cloned() {
        return Some(b);
    }
    let Some(store) = crate::core::shared_store() else {
        return None;
    };
    let Ok(s) = store.lock() else { return None };
    let StoreEnum::Db(db) = &*s else { return None };
    db.list_user_template_presets()
        .ok()?
        .into_iter()
        .find(|r| r.kind == kind)
        .map(|r| TemplatePreset {
            id: r.id,
            name: r.name,
            description: r.description.unwrap_or_default(),
            kind: r.kind,
            template: r.template,
            sample: r.sample.unwrap_or_default(),
            builtin: r.builtin,
            created_at: r.created_at,
            version: r.version,
            changelog: r.changelog.unwrap_or_default(),
        })
}

/// Phase 59 — Save a custom preset. Validates name / template / sample size; rejects builtin=true.
/// Phase 60 — Repeated saves of the same id auto-bump version (using `max(old.version + 1, requested_version)`).
pub fn save_user_template_preset(p: &TemplatePreset) -> Result<TemplatePreset, String> {
    if p.name.trim().is_empty() {
        return Err("name required".into());
    }
    if p.name.len() > 64 {
        return Err("name too long (max 64)".into());
    }
    if p.template.len() > 16 * 1024 {
        return Err("template too large (max 16KB)".into());
    }
    if p.sample.len() > 16 * 1024 {
        return Err("sample too large (max 16KB)".into());
    }
    if p.changelog.len() > 512 {
        return Err("changelog too long (max 512 chars)".into());
    }
    if p.builtin {
        return Err("cannot save builtin preset as user".into());
    }
    let mut out = p.clone();
    if out.id.is_empty() {
        out.id = gen_event_id();
    }
    out.kind = if out.kind.is_empty() || out.kind.starts_with("builtin:") {
        format!("user:{}", out.id)
    } else {
        out.kind.clone()
    };
    let now = crate::core::agent::now_secs();
    out.created_at = if out.created_at == 0 {
        now
    } else {
        out.created_at
    };

    // Phase 60: the same id already exists → read the old version + created_at, auto-bump version (and keep created_at).
    let store = crate::core::shared_store().ok_or_else(|| "store unavailable".to_string())?;
    let s = store.lock().map_err(|_| "store poisoned".to_string())?;
    let StoreEnum::Db(db) = &*s else {
        return Err("sqlite store required".into());
    };
    let existing = db
        .list_user_template_presets()
        .ok()
        .and_then(|rows| rows.into_iter().find(|r| r.id == out.id));
    if let Some(old) = existing {
        out.version = old.version.saturating_add(1).max(out.version.max(1));
        if out.created_at == now || out.created_at == 0 {
            out.created_at = old.created_at;
        }
    } else {
        out.version = out.version.max(1);
        out.created_at = if out.created_at == 0 {
            now
        } else {
            out.created_at
        };
    }
    drop(s); // release the lock before writing (mutable borrow below)
    let mut s2 = store.lock().map_err(|_| "store poisoned".to_string())?;
    let StoreEnum::Db(db) = &mut *s2 else {
        return Err("sqlite store required".into());
    };
    let row = crate::core::storage::TemplatePresetRow {
        id: out.id.clone(),
        name: out.name.clone(),
        description: if out.description.is_empty() {
            None
        } else {
            Some(out.description.clone())
        },
        kind: out.kind.clone(),
        template: out.template.clone(),
        sample: if out.sample.is_empty() {
            None
        } else {
            Some(out.sample.clone())
        },
        builtin: false,
        version: out.version,
        changelog: if out.changelog.is_empty() {
            None
        } else {
            Some(out.changelog.clone())
        },
        created_at: out.created_at,
    };
    db.upsert_template_preset(&row)
        .map_err(|e| format!("save preset: {}", e))?;
    Ok(out)
}

/// Phase 60 — Copy a builtin preset into a new user preset (assigns a new id + user:<uuid> kind,
/// name passed by the caller, version=1, builtin=false). Lets the user freely edit the forked copy.
pub fn fork_builtin_preset(kind: &str, name: &str) -> Result<TemplatePreset, String> {
    let name_trimmed = name.trim();
    if name_trimmed.is_empty() {
        return Err("name required".into());
    }
    if name_trimmed.len() > 64 {
        return Err("name too long (max 64)".into());
    }
    let src = builtin_presets()
        .iter()
        .find(|p| p.kind == kind)
        .cloned()
        .ok_or_else(|| format!("unknown builtin kind: {}", kind))?;
    let new_id = gen_event_id();
    let out = TemplatePreset {
        id: new_id.clone(),
        name: name_trimmed.to_string(),
        description: src.description.clone(),
        kind: format!("user:{}", new_id),
        template: src.template.clone(),
        sample: src.sample.clone(),
        builtin: false,
        created_at: crate::core::agent::now_secs(),
        version: 1,
        changelog: format!("Forked from builtin '{}' (v{})", src.name, src.version),
    };
    let row = crate::core::storage::TemplatePresetRow {
        id: out.id.clone(),
        name: out.name.clone(),
        description: if out.description.is_empty() {
            None
        } else {
            Some(out.description.clone())
        },
        kind: out.kind.clone(),
        template: out.template.clone(),
        sample: if out.sample.is_empty() {
            None
        } else {
            Some(out.sample.clone())
        },
        builtin: false,
        version: out.version,
        changelog: if out.changelog.is_empty() {
            None
        } else {
            Some(out.changelog.clone())
        },
        created_at: out.created_at,
    };
    let store = crate::core::shared_store().ok_or_else(|| "store unavailable".to_string())?;
    let mut s = store.lock().map_err(|_| "store poisoned".to_string())?;
    let StoreEnum::Db(db) = &mut *s else {
        return Err("sqlite store required".into());
    };
    db.upsert_template_preset(&row)
        .map_err(|e| format!("fork preset: {}", e))?;
    Ok(out)
}

/// Phase 59 — Delete only user presets (builtins untouched). Returns true when something was actually deleted.
pub fn delete_user_template_preset(id: &str) -> bool {
    let Some(store) = crate::core::shared_store() else {
        return false;
    };
    let Ok(mut s) = store.lock() else {
        return false;
    };
    let StoreEnum::Db(db) = &mut *s else {
        return false;
    };
    db.delete_template_preset(id)
}

/// Phase 59 — Serialize the preset list to YAML (serde_yaml also accepts JSON input).
pub fn export_presets_to_yaml(presets: &[TemplatePreset]) -> Result<String, String> {
    let doc = PresetYamlDoc {
        version: 1,
        presets: presets.to_vec(),
    };
    serde_yaml::to_string(&doc).map_err(|e| format!("yaml serialize: {}", e))
}

/// Phase 59 — Import presets from YAML / JSON. Every preset is forced to builtin=false; a missing id gets an automatic UUID.
/// Phase 61 — First call `migrate_preset_yaml` to run the migration chain (legacy docs are auto-upgraded to CURRENT_DOC_VERSION).
/// Returns count of presets imported.
pub fn import_presets_from_yaml(yaml: &str) -> Result<usize, String> {
    let (doc, _applied) = migrate_preset_yaml(yaml)?;
    let store = crate::core::shared_store().ok_or_else(|| "store unavailable".to_string())?;
    let mut s = store.lock().map_err(|_| "store poisoned".to_string())?;
    let StoreEnum::Db(db) = &mut *s else {
        return Err("sqlite store required".into());
    };
    let mut count = 0;
    for mut p in doc.presets {
        p.builtin = false;
        if p.id.is_empty() {
            p.id = gen_event_id();
        }
        if p.name.trim().is_empty() {
            continue;
        }
        let row = crate::core::storage::TemplatePresetRow {
            id: p.id.clone(),
            name: p.name.clone(),
            description: if p.description.is_empty() {
                None
            } else {
                Some(p.description.clone())
            },
            kind: if p.kind.is_empty() {
                format!("user:{}", p.id)
            } else {
                p.kind.clone()
            },
            template: p.template.clone(),
            sample: if p.sample.is_empty() {
                None
            } else {
                Some(p.sample.clone())
            },
            builtin: false,
            version: p.version.max(1),
            changelog: if p.changelog.is_empty() {
                None
            } else {
                Some(p.changelog.clone())
            },
            created_at: p.created_at,
        };
        let _ = db.upsert_template_preset(&row);
        count += 1;
    }
    Ok(count)
}

/// Phase 59 — Write a text file (used to persist an export). Phase 59 does not pull in the fs plugin; it uses std::fs directly.
pub fn write_text_file(path: &str, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|e| format!("write file: {}", e))
}

pub fn read_text_file(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("read file: {}", e))
}
