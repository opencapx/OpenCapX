//! F6/D5 — revocation channel: a registry `revokedKeys` hit on an installed plugin → block install/update,
//! disable by default + a `plugin.revoked` event; the user can explicitly reopen (recorded in the audit).
//! Check points: client startup, every 24h, manual registry refresh, and before every install/update.

use super::plugin::PluginManager;
use super::registry;
use rusqlite::params;

/// Whether the target publisher key has been revoked by the registry (when the offline copy is readable; unreadable → false).
pub fn key_is_revoked(key_id: &str) -> bool {
    registry::load_offline()
        .map(|idx| idx.revoked_keys.iter().any(|r| r.key_id == key_id))
        .unwrap_or(false)
}

/// A plugin hit by one sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokedHit {
    pub plugin_id: String,
    pub key_id: String,
}

/// Run one revocation scan over the index: hit plugins → stop the process + record `revoked_key` + event.
/// Plugins already disabled for the same key / explicitly reopened (ack) by the user are not processed again.
pub fn sweep_with(index: &registry::RegistryIndex) -> Vec<RevokedHit> {
    let mut hits = Vec::new();
    let Some(store) = super::shared_store() else {
        return hits;
    };
    // Snapshot: (id, manifest_json, revoked_key, revocation_ack). Do not call stop while holding the read lock.
    let rows: Vec<(String, String, Option<String>, Option<String>)> = store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                let mut st = c
                    .prepare("SELECT id, manifest, revoked_key, revocation_ack FROM plugins")
                    .ok()?;
                let it = st
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<String>>(3)?,
                        ))
                    })
                    .ok()?;
                Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
            })
        })
        .flatten()
        .unwrap_or_default();

    let now = super::plugin_trace::now_secs();
    let mut marked: Vec<(String, String)> = Vec::new();
    for (id, manifest_json, revoked_key, ack) in rows {
        let Ok(m) = serde_json::from_str::<super::plugin::Manifest>(&manifest_json) else {
            continue;
        };
        let Some(key_id) = super::plugin::manifest_signature_key_id(&m) else {
            continue; // Unsigned: not part of the revocation system
        };
        if !index.revoked_keys.iter().any(|r| r.key_id == key_id) {
            continue;
        }
        if revoked_key.as_deref() == Some(key_id.as_str()) {
            continue; // Already disabled (idempotent)
        }
        if ack.as_deref() == Some(key_id.as_str()) {
            continue; // The user explicitly reopened this key; do not disable again
        }
        marked.push((id, key_id));
    }

    for (id, key_id) in marked {
        PluginManager::shared().stop(&id);
        if let Some(store) = super::shared_store() {
            if let Ok(mut s) = store.lock() {
                let _ = s.with_conn(|c| {
                    c.execute(
                        "UPDATE plugins SET revoked_key = ?2, revoked_at = ?3 WHERE id = ?1",
                        params![id, key_id, now as i64],
                    )
                    .unwrap_or(0)
                });
            }
        }
        super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
            "plugin.revoked",
            "core",
            serde_json::json!({ "pluginId": id, "keyId": key_id, "reason": "publisher-key-revoked" }),
        ));
        hits.push(RevokedHit {
            plugin_id: id,
            key_id,
        });
    }
    hits
}

/// Run one sweep over the offline-available index; an unreadable index → empty (no false hits).
pub fn sweep() -> Vec<RevokedHit> {
    match registry::load_offline() {
        Some(index) => sweep_with(&index),
        None => Vec::new(),
    }
}

/// Explicit reopen: clear the disable flag, record `revocation_ack` (= the key the user has acknowledged), and leave an audit event.
pub fn reopen(plugin_id: &str) -> Result<(), String> {
    let Some(store) = super::shared_store() else {
        return Err("storage unavailable".into());
    };
    // Outer None = not installed/storage unavailable; inner None = the revoked column is NULL (not disabled).
    let state: Option<Option<String>> = store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT revoked_key FROM plugins WHERE id = ?1",
                    [plugin_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
            })
        })
        .flatten();
    let key = match state {
        None => return Err(format!("plugin not installed: {}", plugin_id)),
        Some(None) => return Err(format!("plugin is not revoked: {}", plugin_id)),
        Some(Some(key)) => key,
    };
    if let Ok(mut s) = store.lock() {
        s.with_conn(|c| {
            c.execute(
                "UPDATE plugins SET revoked_key = NULL, revoked_at = NULL, revocation_ack = ?2 WHERE id = ?1",
                params![plugin_id, key],
            )
            .unwrap_or(0)
        });
    }
    super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
        "plugin.revoked.reopened",
        "core",
        serde_json::json!({ "pluginId": plugin_id, "keyId": key }),
    ));
    Ok(())
}

/// Start the watch: sweep once at startup + recheck every 24h (offline index; skip silently when unreadable).
pub fn spawn_watch() {
    std::thread::spawn(|| loop {
        let _ = sweep();
        std::thread::sleep(std::time::Duration::from_secs(24 * 60 * 60));
    });
}
