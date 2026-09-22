//! Global hotkeys + command palette (originally Phase 39; redesigned 2026-09-18).
//!
//! Data model:
//! - Bindings live in `~/.opencapx/hotkeys.json` (env `OPENCAPX_HOTKEYS_FILE` overrides),
//!   and `HotkeyFileStore` handles read-modify-write (serialized by an in-process Mutex).
//! - `action` is nested JSON, distinguishing the two kinds `Builtin` and `Plugin`.
//! - Default bindings are code constants; when the file lacks the `bindings` key, the constants are returned; the first mutating operation
//!   materializes the defaults into the file (materialize-on-first-write), avoiding "adding one custom key =
//!   all default keys lost".
//!
//! Constraints:
//! - A normalized combo is unique; writing the same combo overwrites (guaranteed at the code layer, no DB PK).
//! - An OS-level registration failure does not reject the save — the failure is returned with `HotkeyResult` and the UI shows a badge.
//! - Corrupt file: reads fall back to defaults/empty; before the next write it is backed up as `hotkeys.json.bak`.
//! - The old SQLite `hotkey_binding` table is migrated in one shot (`migrate_from_sqlite`); the table itself is left untouched.

use super::storage;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HotkeyAction {
    Builtin {
        action: BuiltinAction,
    },
    Plugin {
        plugin_id: String,
        capability: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltinAction {
    TogglePet,
    OpenSettings,
    OpenPalette,
    Quit,
}

/// Display shape for the front end. `registered_at_os` is the OS registration state at query time.
#[derive(Debug, Clone, Serialize)]
pub struct HotkeyDto {
    pub combo: String,
    pub action: HotkeyAction,
    pub enabled: bool,
    pub registered_at_os: bool,
}

/// Write-operation result: the binding has been persisted; `registered=false` + `error` means OS-level registration failed
/// (the save is not rolled back — the UI shows a badge, and it registers again on startup/retry).
#[derive(Debug, Clone, Serialize)]
pub struct HotkeyResult {
    pub binding: HotkeyDto,
    pub registered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A command-palette entry: flattens a builtin action + every (plugin, capability) for display.
#[derive(Debug, Clone, Serialize)]
pub struct PaletteEntry {
    pub id: String,
    pub title: String,
    pub kind: String, // "builtin" / "plugin"
    pub plugin_id: Option<String>,
    pub capability: Option<String>,
}

/// 2026-09-18 — one hotkey binding (JSON file shape).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct HotkeyBinding {
    /// Normalized uppercase key string, e.g. `CmdOrCtrl+Shift+K` / `F5`.
    pub combo: String,
    pub action: HotkeyAction,
    /// Disabled = not registered at the OS layer and skipped by the dispatch fallback. Defaults to true.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// File top level.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct HotkeyFile {
    #[serde(default)]
    bindings: Vec<HotkeyBinding>,
}

/// Three default bindings (chosen by the user in the 2026-09-18 review; quit is not preset — risk of accidental triggering).
pub fn default_bindings() -> Vec<HotkeyBinding> {
    vec![
        HotkeyBinding {
            combo: "CmdOrCtrl+Shift+P".into(),
            action: HotkeyAction::Builtin { action: BuiltinAction::OpenPalette },
            enabled: true,
        },
        HotkeyBinding {
            combo: "CmdOrCtrl+Shift+,".into(),
            action: HotkeyAction::Builtin { action: BuiltinAction::OpenSettings },
            enabled: true,
        },
        HotkeyBinding {
            combo: "CmdOrCtrl+Shift+H".into(),
            action: HotkeyAction::Builtin { action: BuiltinAction::TogglePet },
            enabled: true,
        },
    ]
}

fn hotkeys_file_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("OPENCAPX_HOTKEYS_FILE") {
        return p.into();
    }
    match dirs::home_dir() {
        Some(h) => h.join(".opencapx").join("hotkeys.json"),
        None => std::env::temp_dir().join("opencapx-hotkeys.json"),
    }
}

/// The read-modify-write latch for hotkeys.json (in-process).
static FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// JSON file store. Tests `open` a tmp path directly, bypassing the global singleton.
pub struct HotkeyFileStore {
    path: std::path::PathBuf,
}

impl HotkeyFileStore {
    pub fn open(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// File parse result; absent / corrupt → None.
    fn parse(&self) -> Option<Vec<HotkeyBinding>> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let file: HotkeyFile = serde_json::from_str(&text).ok()?;
        Some(file.bindings)
    }

    /// User-visible list: file missing/corrupt → default bindings.
    pub fn read(&self) -> Vec<HotkeyBinding> {
        self.parse().unwrap_or_else(default_bindings)
    }

    /// Raw file content (for a backup snapshot): file missing/corrupt → empty.
    pub fn read_raw(&self) -> Vec<HotkeyBinding> {
        self.parse().unwrap_or_default()
    }

    /// Overwrite the file. An existing corrupt file is first backed up as `<path>.bak`, then the new content is written.
    pub fn write(&self, bindings: &[HotkeyBinding]) -> Result<(), String> {
        let _guard = FILE_LOCK.lock().map_err(|e| e.to_string())?;
        if self.path.exists() && self.parse().is_none() {
            let bak = self.path.with_extension("json.bak");
            let _ = std::fs::copy(&self.path, &bak);
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = HotkeyFile { bindings: bindings.to_vec() };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, text).map_err(|e| e.to_string())
    }

    /// Normalize + validate + overwrite the same combo + persist + attempt registration.
    /// Reads go through `read()` (a missing file → defaults as the base): the first mutating operation
    /// materializes the defaults into the file, avoiding the loss of all default keys when the user adds their first custom key.
    pub fn set(&self, combo_raw: &str, action: &HotkeyAction) -> Result<HotkeyResult, String> {
        let combo = normalize_combo(combo_raw);
        validate_combo(&combo)?;
        let mut bindings = self.read();
        let binding = HotkeyBinding { combo: combo.clone(), action: action.clone(), enabled: true };
        bindings.retain(|b| b.combo != combo);
        bindings.push(binding.clone());
        self.write(&bindings)?;
        Ok(self.register_result(binding))
    }

    /// Enable/disable. Disabling unregisters first, then changes the file; enabling changes the file, then registers.
    pub fn set_enabled(&self, combo_raw: &str, enabled: bool) -> Result<HotkeyResult, String> {
        let combo = normalize_combo(combo_raw);
        let mut bindings = self.read();
        let Some(b) = bindings.iter_mut().find(|b| b.combo == combo) else {
            return Err(format!("binding not found: {}", combo));
        };
        b.enabled = enabled;
        let binding = b.clone();
        self.write(&bindings)?;
        if !enabled {
            let _ = unregister_at_os(&combo);
        }
        Ok(self.register_result(binding))
    }

    /// Delete a row. Unregister only after a successful persist; returns false when the combo does not exist.
    /// Deleting a default while the file is missing → materializes the remaining defaults (that one no longer appears).
    pub fn delete(&self, combo_raw: &str) -> bool {
        let combo = normalize_combo(combo_raw);
        let mut bindings = self.read();
        let before = bindings.len();
        bindings.retain(|b| b.combo != combo);
        if bindings.len() == before {
            return false;
        }
        if self.write(&bindings).is_err() {
            return false;
        }
        let _ = unregister_at_os(&combo);
        true
    }

    /// Full list, with OS registration state attached.
    pub fn list(&self) -> Vec<HotkeyDto> {
        self.read()
            .into_iter()
            .map(|b| HotkeyDto {
                registered_at_os: is_registered_at_os(&b.combo),
                combo: b.combo,
                action: b.action,
                enabled: b.enabled,
            })
            .collect()
    }

    /// A binding already persisted → registration result. No registration is attempted when enabled=false.
    fn register_result(&self, b: HotkeyBinding) -> HotkeyResult {
        let (registered, error) = if b.enabled {
            match register_at_os(&b.combo) {
                Ok(r) => (r, None),
                Err(e) => (false, Some(e)),
            }
        } else {
            (false, None)
        };
        HotkeyResult {
            binding: HotkeyDto {
                combo: b.combo,
                action: b.action,
                enabled: b.enabled,
                registered_at_os: registered,
            },
            registered,
            error,
        }
    }
}

/// Global singleton (shared by all paths after main.rs setup).
pub fn store() -> &'static HotkeyFileStore {
    STORE.get_or_init(|| HotkeyFileStore::open(hotkeys_file_path()))
}

static STORE: OnceLock<HotkeyFileStore> = OnceLock::new();

/// One-shot migration: file already exists / rows empty / all payloads fail to parse → 0 (no file written).
/// On success, write the SQLite rows as a whole file and return the count; label / timestamps are dropped.
/// Note that migration does not materialize defaults — the old rows are the whole truth.
/// Non-destructive: the SQLite table itself is left untouched (a lesson from the 2026-09-18 database-restore incident).
pub fn migrate_from_sqlite(rows: &[storage::HotkeyRow]) -> usize {
    migrate_from_sqlite_at(store(), rows)
}

fn migrate_from_sqlite_at(s: &HotkeyFileStore, rows: &[storage::HotkeyRow]) -> usize {
    if s.path().exists() || rows.is_empty() {
        return 0;
    }
    let bindings: Vec<HotkeyBinding> = rows
        .iter()
        .filter_map(|r| {
            let action = parse_action(&r.payload)?;
            Some(HotkeyBinding { combo: normalize_combo(&r.combo), action, enabled: true })
        })
        .collect();
    if bindings.is_empty() || s.write(&bindings).is_err() {
        return 0;
    }
    bindings.len()
}

static SHORTCUT_APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// The AppHandle is injected by main.rs at setup so register/unregister_at_os go through
/// the Tauri global-shortcut plugin API.
pub fn set_shortcut_app(a: tauri::AppHandle) {
    let _ = SHORTCUT_APP.set(a);
}

fn shortcut_app() -> Option<&'static tauri::AppHandle> {
    SHORTCUT_APP.get()
}

/// Collapse a combo string into its normalized uppercase form:
/// - strip excess whitespace
/// - `Ctrl` → `CmdOrCtrl` (`super`/lowercase `ctrl` are synonyms)
/// - fix the order lexicographically, so `Shift+A` and `A+Shift` are not treated as different
pub fn normalize_combo(raw: &str) -> String {
    let mut mods = std::collections::BTreeSet::new();
    let mut keys = Vec::new();
    for part in raw.split('+') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "cmdorctrl" => {
                mods.insert("CmdOrCtrl");
            }
            "cmd" | "command" => {
                mods.insert("CmdOrCtrl");
            }
            "alt" | "option" => {
                mods.insert("Alt");
            }
            "shift" => {
                mods.insert("Shift");
            }
            "super" | "meta" | "win" | "windows" => {
                mods.insert("Super");
            }
            _ => keys.push(p.to_ascii_uppercase()),
        }
    }
    keys.sort();
    let mut out: Vec<&str> = mods.into_iter().collect();
    out.extend(keys.iter().map(|s| s.as_str()));
    out.join("+")
}

fn is_function_key(key: &str) -> bool {
    let Some(num) = key.strip_prefix('F') else { return false };
    matches!(num.parse::<u32>(), Ok(n) if (1..=12).contains(&n))
}

/// Combo validation: it must either contain a modifier (has `+`) or be a bare F1–F12; an empty string is rejected.
pub fn validate_combo(combo: &str) -> Result<(), String> {
    if combo.is_empty() {
        return Err("empty combo".into());
    }
    if combo.contains('+') {
        return Ok(());
    }
    if is_function_key(combo) {
        return Ok(());
    }
    Err(format!(
        "combo must include at least one modifier or be a bare F1-F12 key (got `{}`)",
        combo
    ))
}

/// List all palette candidates (builtin + every capability of every running plugin).
pub fn palette_entries() -> Vec<PaletteEntry> {
    let mut out = Vec::new();
    out.push(PaletteEntry {
        id: "builtin:toggle-pet".into(),
        title: "Toggle pet visibility".into(),
        kind: "builtin".into(),
        plugin_id: None,
        capability: None,
    });
    out.push(PaletteEntry {
        id: "builtin:open-settings".into(),
        title: "Open settings".into(),
        kind: "builtin".into(),
        plugin_id: None,
        capability: None,
    });
    out.push(PaletteEntry {
        id: "builtin:open-palette".into(),
        title: "Open command palette".into(),
        kind: "builtin".into(),
        plugin_id: None,
        capability: None,
    });
    out.push(PaletteEntry {
        id: "builtin:quit".into(),
        title: "Quit OpenCapX".into(),
        kind: "builtin".into(),
        plugin_id: None,
        capability: None,
    });
    for p in super::plugin::PluginManager::shared().list() {
        for cap in &p.capabilities {
            out.push(PaletteEntry {
                id: format!("plugin:{}:{}", p.id, cap),
                title: format!("{} → {}", p.name, cap),
                kind: "plugin".into(),
                plugin_id: Some(p.id.clone()),
                capability: Some(cap.clone()),
            });
        }
    }
    out
}

/// Parse the payload back into an action (used for dispatch).
pub fn parse_action(payload: &str) -> Option<HotkeyAction> {
    serde_json::from_str(payload).ok()
}

/// Register/re-register a hotkey at the OS layer. Returns whether it succeeded.
/// Register an empty handler via `tauri_plugin_global_shortcut::GlobalShortcutExt::on_shortcut`;
/// the real dispatch logic goes through the global `Builder::with_handler` set by main.rs.
/// Register/re-register a hotkey at the OS layer.
/// - `Ok(true)`  registered (including the short-circuit when it was already registered)
/// - `Ok(false)` not attempted — the AppHandle has not been injected yet (early startup / unit-test environment)
/// - `Err(msg)`  OS rejected (combo taken by another app, etc.); msg is used directly for the UI badge
pub fn register_at_os(combo: &str) -> Result<bool, String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let Some(app) = shortcut_app().cloned() else {
        return Ok(false);
    };
    let gs = app.global_shortcut();
    if gs.is_registered(combo) {
        return Ok(true);
    }
    let combo_owned = combo.to_string();
    let combo_captured = combo_owned.clone();
    gs.on_shortcut(combo_owned.as_str(), move |_app2, _shortcut, _ev| {
        // Real dispatch goes through the global handler; leaving this empty avoids the OS reporting "not registered".
        let _ = &combo_captured;
    })
    .map(|_| true)
    .map_err(|e| e.to_string())
}

/// Query the combo's current OS registration state (always false when the app is not injected).
pub fn is_registered_at_os(combo: &str) -> bool {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let Some(app) = shortcut_app().cloned() else {
        return false;
    };
    app.global_shortcut().is_registered(combo)
}

pub fn unregister_at_os(combo: &str) -> bool {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let Some(app) = shortcut_app().cloned() else {
        return false;
    };
    app.global_shortcut().unregister(combo).is_ok()
}

pub fn unregister_all_at_os() -> bool {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let Some(app) = shortcut_app().cloned() else {
        return false;
    };
    app.global_shortcut().unregister_all().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_combo_canonical() {
        assert_eq!(normalize_combo("Ctrl+Shift+K"), "CmdOrCtrl+Shift+K");
        assert_eq!(normalize_combo("ctrl+k"), "CmdOrCtrl+K");
        assert_eq!(normalize_combo("Super+Alt+M"), "Alt+Super+M");
        assert_eq!(normalize_combo("  alt  +  F12 "), "Alt+F12");
        assert_eq!(normalize_combo("a"), "A");
    }

    #[test]
    fn normalize_combo_dedupes_modifiers() {
        // Ctrl + Command both map to CmdOrCtrl in Tauri, so duplicates should be deduped
        assert_eq!(normalize_combo("Ctrl+Command+K"), "CmdOrCtrl+K");
    }

    #[test]
    #[test]
    fn action_roundtrip_json() {
        let a = HotkeyAction::Plugin {
            plugin_id: "com.x".into(),
            capability: "image.analyze".into(),
        };
        let s = serde_json::to_string(&a).unwrap();
        let b: HotkeyAction = serde_json::from_str(&s).unwrap();
        assert_eq!(a, b);
        let a2 = HotkeyAction::Builtin { action: BuiltinAction::OpenPalette };
        let s2 = serde_json::to_string(&a2).unwrap();
        let b2: HotkeyAction = serde_json::from_str(&s2).unwrap();
        assert_eq!(a2, b2);
    }

    // ---- 2026-09-18 JSON storage design: tests ----

    static TEST_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn tmp_store() -> HotkeyFileStore {
        let n = TEST_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("opencapx-hktest-{}-{}.json", pid, n));
        let _ = std::fs::remove_file(&path);
        HotkeyFileStore::open(path)
    }

    #[test]
    fn store_missing_returns_defaults() {
        let s = tmp_store();
        assert_eq!(s.read(), default_bindings());
        assert!(s.read_raw().is_empty());
    }

    #[test]
    fn store_roundtrip() {
        let s = tmp_store();
        let bindings = vec![
            HotkeyBinding {
                combo: "CmdOrCtrl+Shift+P".into(),
                action: HotkeyAction::Builtin { action: BuiltinAction::OpenPalette },
                enabled: true,
            },
            HotkeyBinding {
                combo: "F5".into(),
                action: HotkeyAction::Builtin { action: BuiltinAction::Quit },
                enabled: false,
            },
        ];
        s.write(&bindings).unwrap();
        // A new instance reads the same file — not relying on in-memory state
        let s2 = HotkeyFileStore::open(s.path().to_path_buf());
        assert_eq!(s2.read(), bindings);
    }

    #[test]
    fn store_corrupt_returns_defaults() {
        let s = tmp_store();
        std::fs::write(s.path(), "{ not json").unwrap();
        assert_eq!(s.read(), default_bindings());
        assert!(s.read_raw().is_empty());
    }

    #[test]
    fn store_corrupt_backed_up_on_next_write() {
        let s = tmp_store();
        std::fs::write(s.path(), "{ not json").unwrap();
        let one = vec![HotkeyBinding {
            combo: "CmdOrCtrl+K".into(),
            action: HotkeyAction::Builtin { action: BuiltinAction::TogglePet },
            enabled: true,
        }];
        s.write(&one).unwrap();
        let bak = s.path().with_extension("json.bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "{ not json");
        assert_eq!(s.read(), one);
    }

    #[test]
    fn enabled_defaults_true_on_deserialize() {
        let v: HotkeyBinding = serde_json::from_str(
            r#"{"combo":"F6","action":{"kind":"builtin","action":"quit"}}"#,
        )
        .unwrap();
        assert!(v.enabled);
    }

    #[test]
    fn validate_allows_bare_function_keys() {
        assert!(validate_combo("F5").is_ok());
        assert!(validate_combo("F12").is_ok());
        assert!(validate_combo("CmdOrCtrl+F12").is_ok());
        assert!(validate_combo("Alt+F1").is_ok());
    }

    #[test]
    fn validate_rejects_bare_ordinary_keys() {
        assert!(validate_combo("A").is_err());
        assert!(validate_combo("Space").is_err());
        assert!(validate_combo("").is_err());
        assert!(validate_combo("F13").is_err());
        assert!(validate_combo("F0").is_err());
    }

    #[test]
    fn normalize_is_idempotent_on_canonical_forms() {
        // A normalized string must stay unchanged on re-normalization — the picker/restore-defaults feeds already-normalized strings back in.
        // Regression: without the "cmdorctrl" branch, "CmdOrCtrl+Shift+P" is split into Shift+CMDORCTRL+P.
        for c in [
            "CmdOrCtrl+Shift+P",
            "CmdOrCtrl+Shift+,",
            "CmdOrCtrl+Shift+H",
            "Alt+P",
            "F5",
            "Shift+1",
        ] {
            assert_eq!(normalize_combo(c), c);
        }
    }

    fn builtin(combo: &str, action: BuiltinAction) -> HotkeyBinding {
        HotkeyBinding { combo: combo.into(), action: HotkeyAction::Builtin { action }, enabled: true }
    }

    /// materialize-on-first-write: the first set on a brand-new file writes all 3 defaults into the file.
    #[test]
    fn crud_first_write_materializes_defaults() {
        let s = tmp_store();
        s.set("CmdOrCtrl+K", &HotkeyAction::Builtin { action: BuiltinAction::TogglePet }).unwrap();
        let got = s.read();
        assert_eq!(got.len(), 4, "3 defaults + 1 newly created");
        assert!(got.iter().any(|b| b.combo == "CmdOrCtrl+K"));
    }

    #[test]
    fn crud_set_normalizes_and_upserts() {
        let s = tmp_store();
        // Deliberately use lowercase ctrl — it should normalize to CmdOrCtrl
        let r = s.set("ctrl+k", &HotkeyAction::Builtin { action: BuiltinAction::TogglePet }).unwrap();
        assert_eq!(r.binding.combo, "CmdOrCtrl+K");
        assert!(!r.binding.registered_at_os, "registration is not attempted without an app handle");
        assert!(r.error.is_none());
        // The same combo overwrites the action without creating a second entry (still the only K on top of the materialized defaults)
        s.set("CmdOrCtrl+K", &HotkeyAction::Builtin { action: BuiltinAction::Quit }).unwrap();
        let all = s.read();
        let ks: Vec<&HotkeyBinding> = all.iter().filter(|b| b.combo == "CmdOrCtrl+K").collect();
        assert_eq!(ks.len(), 1);
        assert_eq!(ks[0].action, HotkeyAction::Builtin { action: BuiltinAction::Quit });
    }

    #[test]
    fn crud_set_rejects_invalid_combo() {
        let s = tmp_store();
        assert!(s.set("A", &HotkeyAction::Builtin { action: BuiltinAction::Quit }).is_err());
        assert!(!s.path().exists(), "a validation failure does not persist");
    }

    #[test]
    fn crud_set_enabled_toggles_and_errors_on_missing() {
        let s = tmp_store();
        s.set("F5", &HotkeyAction::Builtin { action: BuiltinAction::Quit }).unwrap();
        let r = s.set_enabled("F5", false).unwrap();
        assert!(!r.binding.enabled);
        let f5 = s.read().into_iter().find(|b| b.combo == "F5").unwrap();
        assert!(!f5.enabled);
        assert!(s.set_enabled("CmdOrCtrl+X", true).is_err());
    }

    #[test]
    fn crud_delete_roundtrip() {
        let s = tmp_store();
        s.set("F5", &HotkeyAction::Builtin { action: BuiltinAction::Quit }).unwrap();
        assert!(s.delete("f5"), "delete matches on the normalized combo");
        assert!(s.read().iter().all(|b| b.combo != "F5"), "F5 is gone while the other defaults remain");
        assert!(!s.delete("F5"), "a duplicate delete returns false");
    }

    #[test]
    fn crud_lists_with_registration_state() {
        let s = tmp_store();
        s.write(&[builtin("F5", BuiltinAction::Quit)]).unwrap();
        let dtos = s.list();
        assert_eq!(dtos.len(), 1);
        assert_eq!(dtos[0].combo, "F5");
        assert!(dtos[0].enabled);
        assert!(!dtos[0].registered_at_os, "not registered without an app handle");
    }

    fn sqlite_row(combo: &str, payload: &str) -> super::super::storage::HotkeyRow {
        super::super::storage::HotkeyRow {
            combo: combo.into(),
            label: "ignored".into(),
            payload: payload.into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn migrate_copies_rows_into_empty_file() {
        let s = tmp_store();
        let rows = vec![
            sqlite_row("CmdOrCtrl+Shift+K", r#"{"kind":"builtin","action":"quit"}"#),
            sqlite_row(
                "Alt+P",
                r#"{"kind":"plugin","plugin_id":"com.x","capability":"image.analyze"}"#,
            ),
        ];
        let n = migrate_from_sqlite_at(&s, &rows);
        assert_eq!(n, 2);
        // label / timestamps are dropped, enabled is filled in as true, and combo is normalized; migration is a whole-file overwrite
        // that does not materialize defaults (migration semantics = the old rows are authoritative).
        let got = s.read();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|b| b.enabled));
        assert!(got.iter().any(|b| b.combo == "Alt+P"));
    }

    #[test]
    fn migrate_skips_when_file_exists_or_rows_empty() {
        let s = tmp_store();
        let rows = vec![sqlite_row("F5", r#"{"kind":"builtin","action":"quit"}"#)];
        // File absent + rows empty → 0, no file written
        assert_eq!(migrate_from_sqlite_at(&s, &[]), 0);
        assert!(!s.path().exists());
        // Migrate once, then feed new rows → 0, an existing file is left untouched
        assert_eq!(migrate_from_sqlite_at(&s, &rows), 1);
        assert_eq!(migrate_from_sqlite_at(&s, &rows), 0);
        assert_eq!(s.read().len(), 1);
    }
}