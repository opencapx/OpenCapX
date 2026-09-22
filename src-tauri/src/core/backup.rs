//! Phase 41 — one-click Workspace backup / restore.
//!
//! Data model:
//! - Backup directory: `~/.opencapx/backups/`, overridden by env `OPENCAPX_BACKUPS_DIR` (matching the isolation pattern of marketplace / traces /
//!   trusted-keys / plugin-config / replay).
//! - A single backup = one JSON file, top-level fields:
//!   - `version: "1"` — schema version, future-proof (old versions are not refused for now)
//!   - `createdAt: u64` — epoch seconds
//!   - `appVersion: String` — CARGO_PKG_VERSION
//!   - `tables: { [name: String]: Vec<Vec<serde_json::Value>> }` — serialized row by row
//!     (a vector rather than an object because events / sessions have no fixed schema; storing cell by cell is more robust)
//!   - `pluginConfigs: HashMap<String, serde_json::Value>` — the contents of `~/.opencapx/config/<id>.json`
//!
//! Constraints:
//! - A backup is "snapshot" semantics, not a transaction. On restore the current store is locked first, existing data in the relevant tables is cleared, then INSERT,
//!   any stage returning Err → abort; the store may be half-new half-old (re-restore manually).
//! - The backup covers only "user preferences / config / business data", not the whole opencapx.db file (that would tangle with schema upgrades).
//! - The backup also leaves events / sessions / capability_stats history alone (large, loss-tolerant, not workspace state).
//!
//! Key API:
//! - `take_snapshot(store, plugin_config_dir) -> Snapshot` — read all relevant tables + read plugin config files
//! - `apply_snapshot(store, snapshot, plugin_config_dir) -> RestoreReport` — write to DB + write plugin config
//! - `create_backup() -> Result<BackupMeta, String>` — take + write file + return metadata
//! - `list_backups() -> Vec<BackupMeta>` — list the directory (by createdAt DESC)
//! - `delete_backup(filename) -> Result<(), String>` — rm by filename
//! - `restore_backup(filename) -> Result<RestoreReport, String>` — read file + apply

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const BACKUP_SCHEMA_VERSION: &str = "1";

/// Backup top level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupSnapshot {
    pub version: String,
    pub created_at: u64,
    pub app_version: String,
/// Table name → rows (each row is a vector of columns, one-to-one with storage).
    pub tables: HashMap<String, Vec<Vec<serde_json::Value>>>,
/// plugin id → config JSON content (empty / missing ones are not added to the HashMap).
    pub plugin_configs: HashMap<String, serde_json::Value>,
/// Hotkey bindings (the contents of hotkeys.json; old snapshots lack this field → empty, ignored on restore).
    #[serde(default)]
    pub hotkeys: Vec<super::hotkey::HotkeyBinding>,
}

/// Backup metadata (for the UI to list).
#[derive(Debug, Clone, Serialize)]
pub struct BackupMeta {
    pub filename: String,
    pub created_at: u64,
    pub size_bytes: u64,
    pub app_version: String,
/// Row counts per category (lets the UI show things like "N plugins · M hotkeys").
    pub table_counts: HashMap<String, usize>,
    pub plugin_config_count: usize,
}

/// restore report (lets the UI show "restored X records").
#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    pub filename: String,
    pub table_counts: HashMap<String, usize>,
    pub plugin_config_count: usize,
}

fn backups_dir() -> PathBuf {
    if let Ok(p) = std::env::var("OPENCAPX_BACKUPS_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".opencapx").join("backups")
}

/// For backup: pull all rows of one table from the store (column order comes from prepare; only values are used here).
pub fn dump_table(
    store: &super::storage::Storage,
    table: &str,
) -> Result<Vec<Vec<serde_json::Value>>, String> {
    let sql = format!("SELECT * FROM {}", table);
    let Ok(mut stmt) = store.conn.prepare(&sql) else {
        return Ok(Vec::new());
    };
    let ncols = stmt.column_count();
    let rows = stmt
        .query_map([], |r| {
            let mut row = Vec::with_capacity(ncols);
            for i in 0..ncols {
                let v: serde_json::Value = match r.get::<_, rusqlite::types::Value>(i) {
                    Ok(rusqlite::types::Value::Null) => serde_json::Value::Null,
                    Ok(rusqlite::types::Value::Integer(n)) => serde_json::json!(n),
                    Ok(rusqlite::types::Value::Real(f)) => serde_json::json!(f),
                    Ok(rusqlite::types::Value::Text(t)) => serde_json::Value::String(t),
                    Ok(rusqlite::types::Value::Blob(b)) => {
                        serde_json::Value::String(format!("<blob {} bytes>", b.len()))
                    }
                    Err(_) => serde_json::Value::Null,
                };
                row.push(v);
            }
            Ok(row)
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|x| x.ok()).collect())
}

/// Snapshot from the in-memory store — covers only "workspace state" tables.
const BACKUP_TABLES: &[&str] = &[
    "plugins",
    "plugin_permissions",
    "capabilities",
    "plugin_ratings",
    "plugin_channel",
    "plugin_health_config",
    "settings_kv",
];

pub fn take_snapshot(
    store: &super::storage::Storage,
    plugin_config_dir: &Path,
) -> BackupSnapshot {
    let mut tables = HashMap::new();
    for t in BACKUP_TABLES {
        match dump_table(store, t) {
            Ok(rows) => {
                tables.insert((*t).to_string(), rows);
            }
            Err(_) => {
                tables.insert((*t).to_string(), Vec::new());
            }
        }
    }
    let plugin_configs = read_plugin_configs(plugin_config_dir);
    let hotkeys = super::hotkey::store().read_raw();
    BackupSnapshot {
        version: BACKUP_SCHEMA_VERSION.into(),
        created_at: super::agent::now_secs(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        tables,
        plugin_configs,
        hotkeys,
    }
}

fn read_plugin_configs(dir: &Path) -> HashMap<String, serde_json::Value> {
    let mut out = HashMap::new();
    let Ok(read) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(s) = std::fs::read_to_string(&path) {
            let val: serde_json::Value = serde_json::from_str(&s).unwrap_or(serde_json::Value::Null);
            out.insert(id.to_string(), val);
        }
    }
    out
}

/// restore: clear all rows of the target tables + INSERT the new rows.
/// Note: events / sessions / capability_stats are not restored (history is deliberately preserved).
fn apply_table(
    store: &mut super::storage::Storage,
    table: &str,
    rows: &[Vec<serde_json::Value>],
) -> Result<usize, String> {
    if rows.is_empty() {
        // Still DELETE so old data is cleared (this matches "no table in the backup = do not keep it")
        store
            .conn
            .execute(&format!("DELETE FROM {}", table), [])
            .map_err(|e| e.to_string())?;
        return Ok(0);
    }
    store
        .conn
        .execute(&format!("DELETE FROM {}", table), [])
        .map_err(|e| e.to_string())?;
    // Column count
    let ncols = rows.first().map(|r| r.len()).unwrap_or(0);
    if ncols == 0 {
        return Ok(0);
    }
    let placeholders = (1..=ncols).map(|i| format!("?{}", i)).collect::<Vec<_>>().join(",");
    let sql = format!("INSERT INTO {} VALUES ({})", table, placeholders);
    let mut count = 0;
    for row in rows {
        if row.len() != ncols {
            return Err(format!(
                "row has {} cols but table {} expects {}",
                row.len(),
                table,
                ncols
            ));
        }
        let params: Vec<Box<dyn rusqlite::ToSql>> = row
            .iter()
            .map(|v| match v {
                serde_json::Value::Null => Box::new(rusqlite::types::Value::Null) as Box<dyn rusqlite::ToSql>,
                serde_json::Value::Bool(b) => Box::new(if *b { 1i64 } else { 0i64 }) as Box<dyn rusqlite::ToSql>,
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        Box::new(i) as Box<dyn rusqlite::ToSql>
                    } else if let Some(f) = n.as_f64() {
                        Box::new(f) as Box<dyn rusqlite::ToSql>
                    } else {
                        Box::new(rusqlite::types::Value::Null) as Box<dyn rusqlite::ToSql>
                    }
                }
                serde_json::Value::String(s) => Box::new(s.clone()) as Box<dyn rusqlite::ToSql>,
                _ => Box::new(v.to_string()) as Box<dyn rusqlite::ToSql>,
            })
            .collect();
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        match store.conn.execute(&sql, refs.as_slice()) {
            Ok(_) => count += 1,
            Err(e) => return Err(format!("insert into {} failed: {}", table, e)),
        }
    }
    Ok(count)
}

pub fn apply_snapshot(
    store: &mut super::storage::Storage,
    snapshot: &BackupSnapshot,
    plugin_config_dir: &Path,
) -> Result<RestoreReport, String> {
    let mut table_counts = HashMap::new();
    for (name, rows) in &snapshot.tables {
        let n = apply_table(store, name, rows)?;
        table_counts.insert(name.clone(), n);
    }
    // plugin configs — write to disk
    let _ = std::fs::create_dir_all(plugin_config_dir);
    // First clear all .json in the config dir (avoid orphans that "exist locally but not in the backup")
    if let Ok(read) = std::fs::read_dir(plugin_config_dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    for (id, val) in &snapshot.plugin_configs {
        let path = plugin_config_dir.join(format!("{}.json", id));
        let s = serde_json::to_string_pretty(val).map_err(|e| e.to_string())?;
        std::fs::write(&path, s).map_err(|e| e.to_string())?;
    }
    // hotkeys — overwrite the file only if the snapshot carries hotkeys (empty = old snapshot, leave the current state).
    if !snapshot.hotkeys.is_empty() {
        super::hotkey::store().write(&snapshot.hotkeys)?;
    }
    Ok(RestoreReport {
        filename: String::new(),
        table_counts,
        plugin_config_count: snapshot.plugin_configs.len(),
    })
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect()
}

pub fn create_backup(
    store: &super::storage::Storage,
    plugin_config_dir: &Path,
) -> Result<BackupMeta, String> {
    let snap = take_snapshot(store, plugin_config_dir);
    let dir = backups_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let filename = format!(
        "backup-{}-{}.json",
        snap.created_at,
        sanitize_filename(&snap.app_version)
    );
    let path = dir.join(&filename);
    let s = serde_json::to_string_pretty(&snap).map_err(|e| e.to_string())?;
    std::fs::write(&path, s).map_err(|e| e.to_string())?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut table_counts = HashMap::new();
    for (k, v) in &snap.tables {
        table_counts.insert(k.clone(), v.len());
    }
    table_counts.insert("hotkeys".into(), snap.hotkeys.len());
    Ok(BackupMeta {
        filename,
        created_at: snap.created_at,
        size_bytes: size,
        app_version: snap.app_version,
        table_counts,
        plugin_config_count: snap.plugin_configs.len(),
    })
}

pub fn list_backups() -> Vec<BackupMeta> {
    let dir = backups_dir();
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        // Parse the top level for metadata (on failure, fall back to filename + size)
        let mut meta = BackupMeta {
            filename: path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            created_at: 0,
            size_bytes: size,
            app_version: String::new(),
            table_counts: HashMap::new(),
            plugin_config_count: 0,
        };
        if let Ok(s) = std::fs::read_to_string(&path) {
            if let Ok(snap) = serde_json::from_str::<BackupSnapshot>(&s) {
                meta.created_at = snap.created_at;
                meta.app_version = snap.app_version;
                for (k, v) in &snap.tables {
                    meta.table_counts.insert(k.clone(), v.len());
                }
                meta.plugin_config_count = snap.plugin_configs.len();
            }
        }
        out.push(meta);
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out
}

pub fn delete_backup(filename: &str) -> Result<(), String> {
    let path = backups_dir().join(filename);
    if !path.starts_with(backups_dir()) {
        return Err("path traversal rejected".into());
    }
    if !path.exists() {
        return Err(format!("backup not found: {}", filename));
    }
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

pub fn restore_backup(
    store: &mut super::storage::Storage,
    plugin_config_dir: &Path,
    filename: &str,
) -> Result<RestoreReport, String> {
    let path = backups_dir().join(filename);
    if !path.starts_with(backups_dir()) {
        return Err("path traversal rejected".into());
    }
    if !path.exists() {
        return Err(format!("backup not found: {}", filename));
    }
    let s = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let snap: BackupSnapshot = serde_json::from_str(&s).map_err(|e| e.to_string())?;
    let mut report = apply_snapshot(store, &snap, plugin_config_dir)?;
    report.filename = filename.to_string();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "opencapx-backup-{}-{}-{}",
            std::process::id(),
            tag,
            super::super::agent::now_secs()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn snapshot_round_trips_through_apply() {
        let dbdir = tmpdir("db");
        let cfgdir = tmpdir("cfg");
        let mut db = super::super::storage::Storage::open(&dbdir.join("a.db")).unwrap();
        // Seed some data
        db.conn
            .execute(
                "INSERT INTO plugin_channel (plugin_id, channel, updated_at) VALUES (?1, ?2, ?3)",
                rusqlite::params!["plug-x", "beta", 100i64],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO settings_kv (k, v) VALUES (?1, ?2)",
                rusqlite::params!["defaultChannel", "dev"],
            )
            .unwrap();
        // Write a plugin config file
        std::fs::write(
            cfgdir.join("plug-x.json"),
            r#"{"theme": "dark", "limit": 5}"#,
        )
            .unwrap();

        let snap = take_snapshot(&db, &cfgdir);
        assert_eq!(snap.tables.get("plugin_channel").unwrap().len(), 1);
        assert_eq!(snap.tables.get("settings_kv").unwrap().len(), 1);
        assert!(snap.plugin_configs.contains_key("plug-x"));

        // Mutate the original data
        db.conn
            .execute(
                "UPDATE plugin_channel SET channel='stable' WHERE plugin_id='plug-x'",
                [],
            )
            .unwrap();
        std::fs::write(cfgdir.join("plug-x.json"), r#"{"theme":"light"}"#).unwrap();
        std::fs::write(cfgdir.join("plug-orphan.json"), r#"{"x":1}"#).unwrap();

        // restore
        let report = apply_snapshot(&mut db, &snap, &cfgdir).unwrap();
        assert_eq!(report.table_counts.get("plugin_channel").copied().unwrap_or(0), 1);
        assert_eq!(report.plugin_config_count, 1);
        // channel in the DB should be beta
        let got: String = db
            .conn
            .query_row(
                "SELECT channel FROM plugin_channel WHERE plugin_id=?1",
                rusqlite::params!["plug-x"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(got, "beta");
        // The file should be restored to its original content
        let restored = std::fs::read_to_string(cfgdir.join("plug-x.json")).unwrap();
        assert!(restored.contains("dark"));
        // Orphan files should be cleaned up
        assert!(!cfgdir.join("plug-orphan.json").exists());
    }

    #[test]
    fn create_and_list_backup_round_trip() {
        let dbdir = tmpdir("cl-db");
        let cfgdir = tmpdir("cl-cfg");
        let db = super::super::storage::Storage::open(&dbdir.join("b.db")).unwrap();
        // OPENCAPX_BACKUPS_DIR is process-level env, and path_traversal_rejected also set/removes it;
        // run in parallel, it can be removed between create and list → lists the default directory → assertion fails. Hold the lock to serialize
        // (TEST_STORE_LOCK is this suite's agreed mutex for "process-level global state").
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("OPENCAPX_BACKUPS_DIR", &cfgdir);
        // A backup can be created with no data
        let meta = create_backup(&db, &cfgdir).unwrap();
        assert!(meta.size_bytes > 0);
        assert!(meta.filename.starts_with("backup-"));
        let list = list_backups();
        assert!(list.iter().any(|m| m.filename == meta.filename));

        // Delete it
        delete_backup(&meta.filename).unwrap();
        let list2 = list_backups();
        assert!(!list2.iter().any(|m| m.filename == meta.filename));

        std::env::remove_var("OPENCAPX_BACKUPS_DIR");
    }

    #[test]
    fn path_traversal_rejected() {
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("OPENCAPX_BACKUPS_DIR", "/tmp/opencapx-bk-test-only");
        assert!(delete_backup("../etc/passwd").is_err());
        std::env::remove_var("OPENCAPX_BACKUPS_DIR");
    }

    /// 2026-09-18 — an old snapshot (without the hotkeys field) must deserialize, with hotkeys empty.
    /// Note snake_case: BackupSnapshot has no rename_all, so it lands on disk as snake_case.
    #[test]
    fn old_snapshot_without_hotkeys_deserializes() {
        let text = r#"{
            "version": "1",
            "created_at": 1,
            "app_version": "0.1.0",
            "tables": {},
            "plugin_configs": {}
        }"#;
        let snap: BackupSnapshot = serde_json::from_str(text).unwrap();
        assert!(snap.hotkeys.is_empty());
    }

    #[test]
    fn snapshot_hotkeys_roundtrip() {
        let mut snap = BackupSnapshot {
            version: "1".into(),
            created_at: 1,
            app_version: "0.1.0".into(),
            tables: HashMap::new(),
            plugin_configs: HashMap::new(),
            hotkeys: Vec::new(),
        };
        snap.hotkeys.push(super::super::hotkey::HotkeyBinding {
            combo: "CmdOrCtrl+Shift+P".into(),
            action: super::super::hotkey::HotkeyAction::Builtin {
                action: super::super::hotkey::BuiltinAction::OpenPalette,
            },
            enabled: true,
        });
        let text = serde_json::to_string(&snap).unwrap();
        let back: BackupSnapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(back.hotkeys.len(), 1);
        assert_eq!(back.hotkeys[0].combo, "CmdOrCtrl+Shift+P");
    }
}