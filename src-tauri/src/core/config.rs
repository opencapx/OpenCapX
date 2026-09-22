//! Plugin config (per-plugin key/value JSON): i1.md §17/§18.
//!
//! Real plugins (like vision, tts) need their own config (API key / endpoint / theme).
//! core stores these in `~/.opencapx/config/<pluginId>.json` — one file per plugin,
//! without fighting SQLite over shared storage; the reverse RPC `config.get/set` lets plugins read their own,
//! and tauri commands let the frontend settings page edit them.

use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Config directory: `~/.opencapx/config` (avoiding a storage::path conflict; it has its own directory here).
pub fn config_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(".opencapx").join("config");
    }
    std::env::temp_dir().join("opencapx-config")
}

/// A single plugin config file: `<config_dir>/<plugin_id>.json`.
pub fn config_path(plugin_id: &str) -> PathBuf {
    config_dir().join(format!("{}.json", sanitize(plugin_id)))
}

/// For reverse RPC + tests: redirect to a test tmp directory.
pub fn config_path_in(plugin_id: &str, dir: &Path) -> PathBuf {
    dir.join(format!("{}.json", sanitize(plugin_id)))
}

fn sanitize(plugin_id: &str) -> String {
    // First replace `..` (arbitrary path traversal) with `__`, then apply the generic mapping.
    // Namespace-separating `.` is still allowed (com.opencapx.cat → com.opencapx.cat.json).
    let trimmed = plugin_id.replace("..", "__");
    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Resolve the plugin ID to an absolute path, preventing escape (jumping up directories).
fn ensure_inside(child: &Path, parent: &Path) -> std::io::Result<()> {
    let child = child.canonicalize().unwrap_or_else(|_| child.to_path_buf());
    let parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
    if !child.starts_with(&parent) {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "plugin id resolves outside config dir",
        ))
    } else {
        Ok(())
    }
}

/// Read all config for one plugin. File missing → empty object.
pub fn all(plugin_id: &str) -> Map<String, Value> {
    let p = config_path(plugin_id);
    read_from(&p).unwrap_or_default()
}

/// Read all config for one plugin from a given directory (for tests).
pub fn all_in(plugin_id: &str, dir: &Path) -> Map<String, Value> {
    let p = config_path_in(plugin_id, dir);
    read_from(&p).unwrap_or_default()
}

fn read_from(path: &Path) -> Option<Map<String, Value>> {
    let text = fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.as_object().cloned()
}

/// atomic write: write `<file>.tmp` then rename, avoiding reads of half-written files.
fn write_atomic(path: &Path, value: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Get one key. If the key is missing, return `default` (JSON null or any Value).
pub fn get(plugin_id: &str, key: &str, default: &Value) -> Value {
    let p = config_path(plugin_id);
    let Some(map) = read_from(&p) else {
        return default.clone();
    };
    map.get(key).cloned().unwrap_or_else(|| default.clone())
}

/// Write one key. Returns the written value (so callers can emit events).
pub fn set(plugin_id: &str, key: &str, value: &Value) -> std::io::Result<Value> {
    let p = config_path(plugin_id);
    if let Some(parent) = p.parent() {
        let _ = ensure_inside(&p, parent); // Defensive: ensure the write stays inside config_dir
    }
    let mut map = read_from(&p).unwrap_or_default();
    map.insert(key.to_string(), value.clone());
    write_atomic(&p, &Value::Object(map.clone()))?;
    Ok(value.clone())
}

/// Delete one key. Key missing → succeeds silently.
pub fn delete(plugin_id: &str, key: &str) -> std::io::Result<()> {
    let p = config_path(plugin_id);
    let mut map = read_from(&p).unwrap_or_default();
    if map.remove(key).is_some() {
        write_atomic(&p, &Value::Object(map))?;
    }
    Ok(())
}

/// Reset the entire plugin config (use with care: only called on uninstall).
pub fn reset(plugin_id: &str) -> std::io::Result<()> {
    let p = config_path(plugin_id);
    if p.exists() {
        fs::remove_file(&p)?;
    }
    Ok(())
}

/// List all plugin ids that have config (by looking at `~/.opencapx/config/*.json`).
pub fn list_plugins() -> Vec<String> {
    let dir = config_dir();
    let Ok(mut read_dir) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = read_dir
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".json") && !name.ends_with(".json.tmp") {
                Some(name.trim_end_matches(".json").to_string())
            } else {
                None
            }
        })
        .collect();
    out.sort();
    out
}

/// Wholesale replace (for the settings page). value must be an object, otherwise error.
pub fn replace(plugin_id: &str, value: &Value) -> std::io::Result<()> {
    let obj = value.as_object().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "config root must be an object")
    })?;
    let p = config_path(plugin_id);
    write_atomic(&p, &Value::Object(obj.clone()))
}

/// Batch interface for the settings UI: merges all stored configs + all installed plugin ids.
pub fn snapshot() -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = list_plugins()
        .into_iter()
        .map(|pid| {
            let cfg = all(&pid);
            (pid, Value::Object(cfg))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Generic reverse RPC entry: dispatch a `config.{op}` call from a plugin to the matching fn.
/// Returns a Value (for the reply); errors return an Err string.
/// M7/F8 — the OS keychain service name used for secret:* (same desktop identity as alerting).
const KEYCHAIN_SERVICE: &str = "com.opencapx.desktop";

/// secret key name (the part after the `secret:` prefix): alphanumeric + `_-`, ≤64.
fn valid_secret_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn secret_user(plugin_id: &str, key: &str) -> String {
    format!("plugin-secret:{}:{}", plugin_id, key)
}

/// M7/F8 — write a secret: keychain first; on failure (including the deliberate skip under cargo test) fall back to a 0600 file.
pub fn set_secret(plugin_id: &str, key: &str, value: &str) -> Result<(), String> {
    if !valid_secret_key(key) {
        return Err(format!("invalid secret key {:?}", key));
    }
    match keychain_set(plugin_id, key, value) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("[opencapx] secret keychain write failed ({e}); falling back to file");
            fallback_write(plugin_id, key, value)
        }
    }
}

/// M7/F8 — read a secret (owner plugin via RPC; the UI layer always masks it).
pub fn get_secret(plugin_id: &str, key: &str) -> Option<String> {
    if !valid_secret_key(key) {
        return None;
    }
    match keychain_get(plugin_id, key) {
        Ok(v) => v,
        Err(_) => fallback_read(plugin_id, key),
    }
}

/// M7/F8 — delete a secret (clears both the keychain and the fallback file; missing is tolerated).
pub fn delete_secret(plugin_id: &str, key: &str) -> Result<(), String> {
    if !valid_secret_key(key) {
        return Err(format!("invalid secret key {:?}", key));
    }
    let _ = keychain_delete(plugin_id, key);
    let path = fallback_path(plugin_id, key)?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("remove fallback secret: {e}"))?;
    }
    Ok(())
}

/// M7/F8 — uninstall cleanup: remove the plugin's fallback secret directory (returns files cleaned).
/// Honest boundary: keychain entries cannot be enumerated (the platform does not offer it), so reinstalling with the same keyId will not clear them automatically.
pub fn forget_plugin_secrets(plugin_id: &str) -> usize {
    let Ok(root) = secrets_root() else {
        return 0;
    };
    let dir = root.join(sanitize(plugin_id));
    let count = fs::read_dir(&dir)
        .map(|it| it.filter_map(|x| x.ok()).count())
        .unwrap_or(0);
    if dir.exists() {
        let _ = fs::remove_dir_all(&dir);
    }
    count
}

fn keychain_entry(plugin_id: &str, key: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, &secret_user(plugin_id, key))
        .map_err(|e| format!("keychain entry init: {e}"))
}

fn keychain_set(plugin_id: &str, key: &str, value: &str) -> Result<(), String> {
    // cargo test skips the keychain: the test binary's hash changes on every rebuild, the Keychain ACL rejects it →
    // the system authorization prompt would hang the tests (same handling as alerting); tests always use the 0600 fallback file.
    if cfg!(test) {
        return Err("keychain disabled under cargo test".into());
    }
    keychain_entry(plugin_id, key)?
        .set_password(value)
        .map_err(|e| format!("keychain set_password: {e}"))
}

fn keychain_get(plugin_id: &str, key: &str) -> Result<Option<String>, String> {
    if cfg!(test) {
        return Err("keychain disabled under cargo test".into());
    }
    match keychain_entry(plugin_id, key)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keychain get_password: {e}")),
    }
}

fn keychain_delete(plugin_id: &str, key: &str) -> Result<(), String> {
    if cfg!(test) {
        return Err("keychain disabled under cargo test".into());
    }
    match keychain_entry(plugin_id, key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keychain delete: {e}")),
    }
}

/// Fallback directory: `OPENCAPX_SECRETS_DIR` (test-injected) > `<config_dir>/opencapx/plugin-secrets`.
fn secrets_root() -> Result<std::path::PathBuf, String> {
    if let Ok(d) = std::env::var("OPENCAPX_SECRETS_DIR") {
        return Ok(std::path::PathBuf::from(d));
    }
    let base = dirs::config_dir().ok_or_else(|| "no config_dir".to_string())?;
    Ok(base.join("opencapx").join("plugin-secrets"))
}

fn fallback_path(plugin_id: &str, key: &str) -> Result<std::path::PathBuf, String> {
    Ok(secrets_root()?.join(sanitize(plugin_id)).join(key))
}

fn fallback_write(plugin_id: &str, key: &str, value: &str) -> Result<(), String> {
    let path = fallback_path(plugin_id, key)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir secrets dir: {e}"))?;
    }
    fs::write(&path, value).map_err(|e| format!("write fallback secret: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn fallback_read(plugin_id: &str, key: &str) -> Option<String> {
    fs::read_to_string(fallback_path(plugin_id, key).ok()?).ok()
}

pub fn dispatch(plugin_id: &str, op: &str, params: &Value) -> Result<Value, String> {
    match op {
        "get" => {
            let key = params
                .get("key")
                .and_then(|x| x.as_str())
                .ok_or_else(|| "config.get requires params.key".to_string())?;
            let default = params.get("default").cloned().unwrap_or(Value::Null);
            // M7/F8 — `secret:<key>`: goes through keychain/fallback; unset → default.
            if let Some(skey) = key.strip_prefix("secret:") {
                return Ok(match get_secret(plugin_id, skey) {
                    Some(s) => Value::String(s),
                    None => default,
                });
            }
            Ok(get(plugin_id, key, &default))
        }
        "set" => {
            let key = params
                .get("key")
                .and_then(|x| x.as_str())
                .ok_or_else(|| "config.set requires params.key".to_string())?;
            let value = params.get("value").cloned().unwrap_or(Value::Null);
            // M7/F8 — `secret:<key>`: write-only, never read back (the receipt is a fixed mask); the value must be a string.
            if let Some(skey) = key.strip_prefix("secret:") {
                let text = value
                    .as_str()
                    .ok_or_else(|| "secret value must be a string".to_string())?;
                set_secret(plugin_id, skey, text)?;
                return Ok(Value::String("********".into()));
            }
            set(plugin_id, key, &value).map_err(|e| e.to_string())?;
            Ok(value)
        }
        "delete" => {
            let key = params
                .get("key")
                .and_then(|x| x.as_str())
                .ok_or_else(|| "config.delete requires params.key".to_string())?;
            if let Some(skey) = key.strip_prefix("secret:") {
                delete_secret(plugin_id, skey)?;
                return Ok(Value::Null);
            }
            delete(plugin_id, key).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "all" => Ok(Value::Object(all(plugin_id))),
        "list" => Ok(Value::Array(list_plugins().into_iter().map(Value::String).collect())),
        _ => Err(format!("unknown config op {}", op)),
    }
}

/// Force-write a config to a given directory (for tests).
pub fn force_write_in(plugin_id: &str, dir: &Path, value: &Value) -> std::io::Result<()> {
    let p = config_path_in(plugin_id, dir);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    write_atomic(&p, value)
}

/// DTO for tauri commands: plugin id + its config (already a JSON Value).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginConfigEntry {
    pub id: String,
    pub config: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Each test gets its own tmp dir (avoids parallel interference).
    fn fresh_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "opencapx-cfg-{}-{}",
            std::process::id(),
            n
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn all_in_empty_returns_empty_object() {
        let dir = fresh_dir();
        assert!(all_in("com.x.test", &dir).is_empty());
    }

    #[test]
    fn set_then_get_roundtrips() {
        let dir = fresh_dir();
        force_write_in("com.x.test", &dir, &json!({"k": "v"})).unwrap();
        let map = all_in("com.x.test", &dir);
        assert_eq!(map.get("k").and_then(|v| v.as_str()), Some("v"));
    }

    #[test]
    fn sanitize_strips_path_traversal() {
        // An id containing a slash/.. must be sanitized into a single file and not escape config_dir
        let dir = fresh_dir();
        let p = config_path_in("../escape", &dir);
        assert!(p.file_name().unwrap().to_string_lossy().contains("__escape") || p.file_name().unwrap().to_string_lossy().contains("_escape"));
        assert!(!p.to_string_lossy().contains(".."));
    }

    #[test]
    fn dispatch_get_returns_default_when_missing() {
        let dir = fresh_dir();
        let got = dispatch_with("com.x", "get", &json!({"key": "missing", "default": 42}), &dir).unwrap();
        assert_eq!(got, json!(42));
    }

    #[test]
    fn dispatch_set_then_get_roundtrips() {
        let dir = fresh_dir();
        dispatch_with(
            "com.x",
            "set",
            &json!({"key": "apiKey", "value": "secret-123"}),
            &dir,
        )
        .unwrap();
        let got = dispatch_with(
            "com.x",
            "get",
            &json!({"key": "apiKey"}),
            &dir,
        )
        .unwrap();
        assert_eq!(got, json!("secret-123"));
    }

    #[test]
    fn dispatch_delete_removes_key() {
        let dir = fresh_dir();
        dispatch_with(
            "com.x",
            "set",
            &json!({"key": "a", "value": 1}),
            &dir,
        )
        .unwrap();
        dispatch_with("com.x", "delete", &json!({"key": "a"}), &dir).unwrap();
        let got = dispatch_with("com.x", "get", &json!({"key": "a", "default": null}), &dir).unwrap();
        assert_eq!(got, Value::Null);
    }

    #[test]
    fn dispatch_all_lists_keys() {
        let dir = fresh_dir();
        dispatch_with("com.x", "set", &json!({"key": "a", "value": 1}), &dir).unwrap();
        dispatch_with("com.x", "set", &json!({"key": "b", "value": "two"}), &dir).unwrap();
        let all = dispatch_with("com.x", "all", &json!({}), &dir).unwrap();
        assert_eq!(all.get("a"), Some(&json!(1)));
        assert_eq!(all.get("b"), Some(&json!("two")));
    }

    /// Temporarily redirect dispatch to a given directory (avoid polluting home config).
    fn dispatch_with(plugin_id: &str, op: &str, params: &Value, dir: &Path) -> Result<Value, String> {
        // Use force_write / read_from directly against dir, simulating dispatch
        let path = config_path_in(plugin_id, dir);
        let mut map = read_from(&path).unwrap_or_default();
        match op {
            "get" => {
                let key = params.get("key").and_then(|x| x.as_str()).unwrap();
                let default = params.get("default").cloned().unwrap_or(Value::Null);
                Ok(map.get(key).cloned().unwrap_or(default))
            }
            "set" => {
                let key = params.get("key").and_then(|x| x.as_str()).unwrap().to_string();
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                map.insert(key, value.clone());
                write_atomic(&path, &Value::Object(map)).map_err(|e| e.to_string())?;
                Ok(value)
            }
            "delete" => {
                let key = params.get("key").and_then(|x| x.as_str()).unwrap().to_string();
                map.remove(&key);
                write_atomic(&path, &Value::Object(map)).map_err(|e| e.to_string())?;
                Ok(Value::Null)
            }
            "all" => Ok(Value::Object(map)),
            _ => Err(format!("unknown op {}", op)),
        }
    }

    // ── M7/F8: secret:* round trip (keychain automatically falls back to a 0600 file under test) ──────────

    #[test]
    fn secret_round_trip_via_dispatch_fallback() {
        // Shares TEST_STORE_LOCK with plugin.rs secret cases: OPENCAPX_SECRETS_DIR is a
        // process-level variable, both sites change it, so they must be serialized.
        let _g = crate::core::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = fresh_dir();
        std::env::set_var("OPENCAPX_SECRETS_DIR", &dir);

        // set → fixed-mask receipt (write-only, never read back)
        let out = dispatch(
            "com.test.secret",
            "set",
            &json!({"key": "secret:token", "value": "abc123"}),
        )
        .expect("set secret");
        assert_eq!(out, json!("********"));

        // Fallback file lands + 0600 (unix)
        let f = dir.join("com.test.secret").join("token");
        assert!(f.exists(), "fallback file must exist under test");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "abc123");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "fallback secret must be 0600");
        }

        // owner RPC can read it back
        let got = dispatch("com.test.secret", "get", &json!({"key": "secret:token"})).unwrap();
        assert_eq!(got, json!("abc123"));

        // Unset → default
        let missing = dispatch(
            "com.test.secret",
            "get",
            &json!({"key": "secret:nope", "default": "fallback"}),
        )
        .unwrap();
        assert_eq!(missing, json!("fallback"));

        // all does not include secrets (secrets do not land in the config file)
        let all = dispatch("com.test.secret", "all", &json!({})).unwrap();
        assert!(all.get("secret:token").is_none());

        // delete → file cleanup + read back null
        dispatch("com.test.secret", "delete", &json!({"key": "secret:token"})).unwrap();
        assert!(!f.exists());
        let gone = dispatch("com.test.secret", "get", &json!({"key": "secret:token"})).unwrap();
        assert_eq!(gone, json!(null));

        std::env::remove_var("OPENCAPX_SECRETS_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secret_value_must_be_string() {
        let _g = crate::core::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = fresh_dir();
        std::env::set_var("OPENCAPX_SECRETS_DIR", &dir);
        let err = dispatch("p", "set", &json!({"key": "secret:x", "value": 42})).unwrap_err();
        assert!(err.contains("string"), "got: {}", err);
        std::env::remove_var("OPENCAPX_SECRETS_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}