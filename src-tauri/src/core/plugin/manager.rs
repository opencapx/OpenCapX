//! PluginManager struct, global switches, InstallOptions, Watchdog, and the store/manifest/compat/settings helpers.
//! Mechanical move from core/plugin.rs.

use super::*;

pub struct PluginManager {
    pub(crate) procs: Mutex<HashMap<String, Arc<PluginProcess>>>,
    pub(crate) watchdogs: Mutex<HashMap<String, Arc<Mutex<Watchdog>>>>,
    /// Cumulative retry count across restarts. Reset to zero in start().
    pub(crate) retry_counts: Mutex<HashMap<String, u32>>,
    /// manifest path → most recent mtime (for auto-reload polling)
    pub(crate) last_mtimes: Mutex<HashMap<String, std::time::SystemTime>>,
    /// Set of plugin ids with auto_reload enabled (per-manager, avoiding shared_store global coupling)
    pub(crate) auto_reload_set: Mutex<std::collections::HashSet<String>>,
}

pub(crate) const WATCHDOG_MAX_RETRIES: u32 = 3;

pub(crate) const WATCHDOG_TICK_MS: u64 = 500;

/// F6 mutual exclusion: install / update / uninstall are globally serialized (in-process lock). Installs are infrequent,
/// so the coarse-grained lock cost is negligible; it prevents concurrent installs from cross-writing the same staging dir (.tmp-<id>-<pid>).
pub(crate) static INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// Extracts the publisher keyId from the manifest signature block (v1/v2 same shape); unsigned → None.
pub fn manifest_signature_key_id(m: &Manifest) -> Option<String> {
    m.signature
        .as_ref()?
        .get("keyId")?
        .as_str()
        .map(str::to_string)
}

/// F7 — the "allow unsigned packages" master switch (settings_kv, default ON; OFF → unsigned/unknown-key is hard-rejected).
pub fn allow_unsigned() -> bool {
    crate::core::shared_store()
        .and_then(|s| s.lock().ok().and_then(|g| g.get_setting("allow_unsigned")))
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// F7 — writes the master switch (settings page).
pub fn set_allow_unsigned(on: bool) -> Result<(), String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("storage unavailable".into());
    };
    let Ok(mut s) = store.lock() else {
        return Err("store lock poisoned".into());
    };
    s.set_setting("allow_unsigned", if on { "1" } else { "0" });
    Ok(())
}

/// S1 — environment isolation switch (default on; `plugin_env_isolation=false` falls back to inheriting the host env).
pub fn env_isolation_enabled() -> bool {
    crate::core::shared_store()
        .and_then(|s| {
            s.lock()
                .ok()
                .and_then(|g| g.get_setting("plugin_env_isolation"))
        })
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// S1 — incremental plugin environment allowlist (comma-separated; empty = minimal allowlist only).
pub fn env_allowlist() -> Vec<String> {
    crate::core::shared_store()
        .and_then(|s| {
            s.lock()
                .ok()
                .and_then(|g| g.get_setting("plugin_env_allowlist"))
        })
        .map(|v| {
            v.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// S5b — sandbox enforcement switch (default false: H1 soaks first; trusted declaring plugins follow the switch, untrusted declaring plugins are forced).
pub fn sandbox_enforcement_enabled() -> bool {
    crate::core::shared_store()
        .and_then(|s| {
            s.lock()
                .ok()
                .and_then(|g| g.get_setting("sandbox_enforcement"))
        })
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// S5b — writes the sandbox enforcement switch (settings page).
pub fn set_sandbox_enforcement(on: bool) -> Result<(), String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("storage unavailable".into());
    };
    let Ok(mut s) = store.lock() else {
        return Err("store lock poisoned".into());
    };
    s.set_setting("sandbox_enforcement", if on { "1" } else { "0" });
    Ok(())
}

/// F7 — install confirmation options. All false by default = fail-closed (soft-warning tier rejects the install).
#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    /// The user has confirmed "unverified source (unsigned / unknown key), risk accepted".
    pub confirm_unsigned: bool,
    /// The user has confirmed "publisher key changed = new publisher takes over".
    pub confirm_key_change: bool,
}

pub(crate) struct Watchdog {
    pub(crate) enabled: bool,
    /// Phase 40 — per-plugin watchdog config (snapshotted at register time; the loop no longer touches the store).
    pub(crate) cfg: crate::core::health::PluginHealthConfig,
}

impl Watchdog {
    pub(crate) fn new(cfg: crate::core::health::PluginHealthConfig) -> Self {
        Self {
            enabled: cfg.enabled,
            cfg,
        }
    }
}

impl PluginManager {
    pub fn shared() -> Arc<Self> {
        static MGR: OnceLock<Arc<PluginManager>> = OnceLock::new();
        MGR.get_or_init(|| {
            Arc::new(PluginManager {
                procs: Mutex::new(HashMap::new()),
                watchdogs: Mutex::new(HashMap::new()),
                retry_counts: Mutex::new(HashMap::new()),
                last_mtimes: Mutex::new(HashMap::new()),
                auto_reload_set: Mutex::new(std::collections::HashSet::new()),
            })
        })
        .clone()
    }

    pub(crate) fn store() -> Option<SharedStore> {
        crate::core::shared_store()
    }

    /// Sets the plugin status column + emits `plugin.state_changed`. Phase 37 routes `probe_failed`
    /// through this same path too, so the EventBus uses one channel (SSE-consistent).
    pub fn set_status(id: &str, status: &str) {
        if let Some(store) = Self::store() {
            if let Ok(mut s) = store.lock() {
                let _ = s.with_conn(|c| {
                    c.execute(
                        "UPDATE plugins SET status = ?2 WHERE id = ?1",
                        params![id, status],
                    )
                    .unwrap_or(0)
                });
            }
        }
        crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
            "plugin.state_changed",
            "core",
            json!({ "pluginId": id, "status": status }),
        ));
    }

    /// F4 — semantic-version compatibility gate: core >= minCoreVersion. Default = no floor. Lenient parsing
    /// (same implementation as the marketplace, avoiding two comparison paths).
    pub fn check_core_compat(m: &Manifest) -> Result<(), String> {
        let Some(min) = &m.min_core_version else {
            return Ok(());
        };
        let core = env!("CARGO_PKG_VERSION");
        match (
            crate::core::marketplace::parse_version_lenient(core),
            crate::core::marketplace::parse_version_lenient(min),
        ) {
            (Some(c), Some(req)) => {
                if c >= req {
                    Ok(())
                } else {
                    Err(format!(
                        "plugin requires core >= {} (current {})",
                        min, core
                    ))
                }
            }
            // unparseable → let through (strict rejection already happens in validate; this is defensive)
            _ => Ok(()),
        }
    }

    /// §4.4 step 4 — confirmation set = permissions[] ∪ all inline-mapping permissions (review M1),
    /// each item annotated with whether it is declaration-derived (→ once-only) and its default tier.
    pub(crate) fn install_ask_plan(m: &Manifest) -> Vec<crate::core::permission::InstallAsk> {
        let mut plan: Vec<crate::core::permission::InstallAsk> = Vec::new();
        let mut push = |permission: String, declared: bool, default: Option<&str>| {
            if plan.iter().any(|a| a.permission == permission) {
                return;
            }
            plan.push(crate::core::permission::InstallAsk {
                permission,
                declared,
                declared_default: default.unwrap_or("ask").to_string(),
            });
        };
        for p in &m.permissions {
            // built-in names as before; declared permission names get the `declared` marker from the inline mapping
            let declared = !crate::core::permission::known(p);
            let default = m
                .capabilities
                .iter()
                .find(|c| c.mapping().map(|(perm, _)| perm) == Some(p.as_str()))
                .and_then(|c| c.mapping().map(|(_, d)| d))
                .map(|s| s.to_string());
            push(p.clone(), declared, default.as_deref());
        }
        for c in &m.capabilities {
            if let Some((perm, default)) = c.mapping() {
                push(perm.to_string(), true, Some(default));
            }
        }
        plan
    }

    /// F6 — update confirmation plan: keep only items that are "added / declaration or default changed".
    /// Completely unchanged → empty plan = silent update (reuses the existing consent, no popup).
    fn update_ask_plan(old: &Manifest, new: &Manifest) -> Vec<crate::core::permission::InstallAsk> {
        let old_plan = Self::install_ask_plan(old);
        Self::install_ask_plan(new)
            .into_iter()
            .filter(|ask| {
                !old_plan.iter().any(|o| {
                    o.permission == ask.permission
                        && o.declared == ask.declared
                        && o.declared_default == ask.declared_default
                })
            })
            .collect()
    }

    /// F6 — update matrix entry: returns (confirmation plan, whether the publisher key changed).
    /// Fresh install → full plan; key changed/unknown → full re-confirmation + marker (new-publisher path);
    /// same key → diff plan only.
    pub(crate) fn update_plan_for(
        old: Option<&Manifest>,
        new: &Manifest,
    ) -> (Vec<crate::core::permission::InstallAsk>, bool) {
        let Some(old) = old else {
            return (Self::install_ask_plan(new), false);
        };
        let key_changed = manifest_signature_key_id(old) != manifest_signature_key_id(new);
        if key_changed {
            (Self::install_ask_plan(new), true)
        } else {
            (Self::update_ask_plan(old, new), false)
        }
    }

    /// F7 — publisher keyId of the installed plugin (used by the update preview / key-change prompt).
    pub fn installed_publisher_key(id: &str) -> Option<String> {
        Self::row_manifest(id)
            .ok()
            .and_then(|(_, m)| manifest_signature_key_id(&m))
    }

    /// M7/F8 — generic settings-page data source: declared schema + current values (secrets only report "set", values are not returned).
    pub fn settings_view(id: &str) -> Result<SettingsViewDto, String> {
        let (_, m) = Self::row_manifest(id)?;
        let mut values = serde_json::Map::new();
        let mut secrets_set = Vec::new();
        for s in &m.settings {
            if s.stype == "button" || s.stype == "list" {
                continue;
            }
            if s.stype == "secret" {
                if crate::core::config::get_secret(id, &s.key).is_some() {
                    secrets_set.push(s.key.clone());
                }
                continue;
            }
            let default = s.default.clone().unwrap_or(serde_json::Value::Null);
            values.insert(
                s.key.clone(),
                crate::core::config::get(id, &s.key, &default),
            );
        }
        Ok(SettingsViewDto {
            settings: m.settings.clone(),
            values,
            secrets_set,
        })
    }

    /// M7/F8 — write a single setting: the key must be declared; since P1 the declared `validate[]` is enforced (failure →
    /// `invalid: <message>` / `invalid: <rule-type>`); secrets go to the keychain, everything else to config.
    /// A plugin's reverse `config.set` does not go through here, so it is not subject to validation.
    pub fn set_setting_value(id: &str, key: &str, value: &serde_json::Value) -> Result<(), String> {
        let (_, m) = Self::row_manifest(id)?;
        let decl = m
            .settings
            .iter()
            .find(|s| s.key == key)
            .ok_or_else(|| format!("setting {} is not declared by {}", key, id))?;
        match decl.stype.as_str() {
            "button" => Err(format!("setting {} is an action; invoke it instead", key)),
            "list" => Err(format!("list setting {} is managed by its plugin", key)),
            "secret" => {
                let text = value
                    .as_str()
                    .ok_or_else(|| format!("secret setting {} must be a string", key))?;
                enforce_validate_rules(decl, value)?;
                crate::core::config::set_secret(id, key, text)
            }
            _ => {
                enforce_validate_rules(decl, value)?;
                crate::core::config::set(id, key, value)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        }
    }

    /// M7/F8 — button control: requests plugin method `settings.<key>` (lazy start).
    pub fn invoke_setting_action(id: &str, key: &str) -> Result<serde_json::Value, String> {
        let (_, m) = Self::row_manifest(id)?;
        let decl = m
            .settings
            .iter()
            .find(|s| s.key == key)
            .ok_or_else(|| format!("setting {} is not declared by {}", key, id))?;
        if decl.stype != "button" {
            return Err(format!("setting {} is not a button action", key));
        }
        let proc = Self::shared().ensure_running(id)?;
        proc.call(
            &format!("settings.{}", key),
            json!({}),
            Duration::from_secs(10),
        )
    }

    /// P3 — list control: forwards CRUD ops to plugin method `settings.<key>` (lazy start, 10s timeout,
    /// same timeout surface as button). The plugin is the sole holder of list data and returns the new array; the host does not persist it.
    /// op ∈ list(read) | add(value) | delete(index) | move(index→to).
    pub fn invoke_setting_list_op(
        id: &str,
        key: &str,
        op: &str,
        index: Option<usize>,
        to: Option<usize>,
        value: Option<String>,
    ) -> Result<serde_json::Value, String> {
        let (_, m) = Self::row_manifest(id)?;
        let decl = m
            .settings
            .iter()
            .find(|s| s.key == key)
            .ok_or_else(|| format!("setting {} is not declared by {}", key, id))?;
        if decl.stype != "list" {
            return Err(format!("setting {} is not a list", key));
        }
        let mut params = json!({ "op": op });
        if let Some(i) = index {
            params["index"] = json!(i);
        }
        if let Some(i) = to {
            params["to"] = json!(i);
        }
        if let Some(v) = value {
            params["value"] = json!(v);
        }
        let proc = Self::shared().ensure_running(id)?;
        proc.call(
            &format!("settings.{}", key),
            params,
            Duration::from_secs(10),
        )
    }

    /// F6 — revises the frozen revocation marker (`revoked_key IS NOT NULL` = disabled by default).
    pub(crate) fn revoked_key_of(id: &str) -> Option<String> {
        Self::store()
            .and_then(|store| {
                store.lock().ok().and_then(|s| {
                    s.with_conn_ref(|c| {
                        c.query_row("SELECT revoked_key FROM plugins WHERE id = ?1", [id], |r| {
                            r.get::<_, Option<String>>(0)
                        })
                        .ok()
                    })
                })
            })
            .flatten()
            .flatten()
    }

    /// M7/F9 — version of the most recent successful initialization handshake (NULL = no successful start record yet).
    pub(crate) fn last_version_of(id: &str) -> Option<String> {
        Self::store()
            .and_then(|store| {
                store.lock().ok().and_then(|s| {
                    s.with_conn_ref(|c| {
                        c.query_row(
                            "SELECT last_version FROM plugins WHERE id = ?1",
                            [id],
                            |r| r.get::<_, Option<String>>(0),
                        )
                        .ok()
                    })
                })
            })
            .flatten()
            .flatten()
    }

    /// M7/F9 — records the version of this successful start (for injecting previousVersion on the next start).
    pub(crate) fn set_last_version(id: &str, version: &str) {
        if let Some(store) = Self::store() {
            if let Ok(mut s) = store.lock() {
                let _ = s.with_conn(|c| {
                    c.execute(
                        "UPDATE plugins SET last_version = ?2 WHERE id = ?1",
                        params![id, version],
                    )
                    .unwrap_or(0)
                });
            }
        }
    }

    pub(crate) fn row_manifest(id: &str) -> Result<(PathBuf, Manifest), String> {
        let Some(store) = Self::store() else {
            return Err("storage unavailable".into());
        };
        let row = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    c.query_row(
                        "SELECT path, manifest FROM plugins WHERE id = ?1",
                        [id],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                    )
                    .ok()
                })
            })
            .flatten();
        let Some((path, manifest)) = row else {
            return Err(format!("plugin not installed: {}", id));
        };
        let m: Manifest = serde_json::from_str(&manifest)
            .map_err(|e| format!("stored manifest broken: {}", e))?;
        Ok((PathBuf::from(path), m))
    }

    /// Detail page: reads README.md in the plugin root. Exists, ≤ limit, UTF-8 → Some(raw);
    /// missing/oversized/non-UTF-8 → None (the frontend shows "not provided"); plugin not installed → Err.
    /// Reads a fixed filename only; does not accept a path from the frontend.
    pub fn readme(id: &str) -> Result<Option<String>, String> {
        const MAX_README_BYTES: u64 = 256 * 1024;
        let (dir, _) = Self::row_manifest(id)?;
        // repo convention is README.md; hand-written plugins often use readme.md, so try each once
        for name in ["README.md", "readme.md"] {
            let p = dir.join(name);
            let Ok(meta) = std::fs::metadata(&p) else {
                continue;
            };
            if !meta.is_file() || meta.len() > MAX_README_BYTES {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&p) {
                if let Ok(text) = String::from_utf8(bytes) {
                    return Ok(Some(text));
                }
            }
        }
        Ok(None)
    }

    /// (id, version) snapshot, used for dependency decisions.
    pub(crate) fn installed_versions() -> Vec<(String, String)> {
        Self::store()
            .and_then(|store| {
                store.lock().ok().and_then(|s| {
                    s.with_conn_ref(|c| {
                        let mut st = c.prepare("SELECT id, version FROM plugins").ok()?;
                        let it = st
                            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                            .ok()?;
                        Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
                    })
                })
            })
            .flatten()
            .unwrap_or_default()
    }
}
