//! per-plugin decisions (check/set), the global policy hard gate, and the core-capability policy list.
//! Mechanical move from core/permission.rs.

use super::*;

/// Look up the plugin-level decision: DB override > default table.
pub fn check(store: &SharedStore, plugin_id: &str, permission: &str) -> Decision {
    let found: Option<String> = store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT decision FROM plugin_permissions WHERE plugin_id = ?1 AND permission = ?2",
                    params![plugin_id, permission],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
        })
        .flatten();
    found
        .as_deref()
        .map(parse_decision)
        .unwrap_or_else(|| default_decision_for(store, permission))
}

/// Explicit grant/revoke (settings page, install confirmation, runtime Always writeback).
/// §4.3 enforcement point 3: declared-derived permissions **refuse to be written as granted** — the settings page can only set ask / denied,
/// closeable but not permanently openable (the other half of Always-laundering H2).
pub fn set_decision(
    store: &SharedStore,
    plugin_id: &str,
    permission: &str,
    decision: &str,
) -> bool {
    if !known_or_declared(store, permission) || !["granted", "denied", "ask"].contains(&decision) {
        return false;
    }
    if decision == "granted" && is_declared(store, permission) {
        audit(
            "denied",
            plugin_id,
            permission,
            json!({ "reason": "declared-permission-once-only", "caller": "set-decision" }),
        );
        return false;
    }
    let now = crate::core::agent::now_secs();
    store
        .lock()
        .ok()
        .and_then(|mut s| {
            s.with_conn(|c| {
                c.execute(
                    "INSERT INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at)
                     VALUES (?1, ?2, NULL, ?3, ?4)
                     ON CONFLICT(plugin_id, permission) DO UPDATE SET decision = ?3, updated_at = ?4",
                    params![plugin_id, permission, decision, now],
                )
                .unwrap_or(0)
            })
        })
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// Look up the global policy override. None = no override (continue with per-agent / built-in default).
/// The Mem variant / lock acquisition failure is likewise treated as no override.
pub fn global_override(store: &SharedStore, permission: &str) -> Option<Decision> {
    // the lock guard is released at the end of the `let` statement; do not re-acquire the lock while holding it (same-thread reentrant deadlock)
    let found: Option<String> = store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT decision FROM permission_policy WHERE permission = ?1",
                    params![permission],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
        })
        .flatten();
    found.as_deref().map(parse_decision)
}

/// Global policy's UI string form ("" = no override, otherwise granted/ask/denied).
pub fn global_override_str(store: &SharedStore, permission: &str) -> &'static str {
    match global_override(store, permission) {
        Some(d) => decision_str(d),
        None => "",
    }
}

/// Write global policy. Only permissions within the static vocabulary; high-risk permissions reject global granted (high-risk allows only once-at-a-time grants).
/// On success emits a `permission.policy.changed` audit event.
pub fn set_global_override(
    store: &SharedStore,
    permission: &str,
    decision: &str,
) -> Result<(), String> {
    if !known(permission) {
        return Err(format!("unknown permission: {}", permission));
    }
    if !["granted", "denied", "ask"].contains(&decision) {
        return Err(format!("invalid decision: {}", decision));
    }
    if decision == "granted" && HIGH_RISK.contains(&permission) {
        return Err(
            "high-risk permission cannot be globally granted; allow once at runtime".into(),
        );
    }
    let now = crate::core::agent::now_secs() as i64;
    let res = store.lock().ok().and_then(|mut s| {
        s.try_with_conn(|c| {
            c.execute(
                "INSERT INTO permission_policy (permission, decision, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(permission) DO UPDATE SET decision = ?2, updated_at = ?3",
                params![permission, decision, now],
            )
            .map(|_| ())
            .map_err(|e| format!("write policy failed: {}", e))
        })
    });
    match res {
        Some(Ok(())) => {
            audit_policy(permission, decision);
            Ok(())
        }
        Some(Err(e)) => Err(e),
        None => Err("storage unavailable".into()),
    }
}

/// Clear the global policy override (restores the built-in default). Also emits an audit event, with decision recorded as "none".
pub fn clear_global_override(store: &SharedStore, permission: &str) -> Result<(), String> {
    let res = store.lock().ok().and_then(|mut s| {
        s.try_with_conn(|c| {
            c.execute(
                "DELETE FROM permission_policy WHERE permission = ?1",
                params![permission],
            )
            .map(|_| ())
            .map_err(|e| format!("clear policy failed: {}", e))
        })
    });
    match res {
        Some(Ok(())) => {
            audit_policy(permission, "none");
            Ok(())
        }
        Some(Err(e)) => Err(e),
        None => Err("storage unavailable".into()),
    }
}

/// Reverse lookup: which core capabilities use a given permission (settings page "System capabilities" behavior labels).
pub fn core_capabilities_for(permission: &str) -> Vec<&'static str> {
    crate::core::capability::CAPABILITY_IDS
        .iter()
        .copied()
        .filter(|cap| capability_permission(cap) == Some(permission))
        .collect()
}

/// Global policy view entry (settings page "System capabilities" tab data source).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorePermPolicyDto {
    pub permission: String,
    /// UI text for the core capabilities this permission covers (e.g. image.read → image.analyze · image.ocr)
    pub capabilities: Vec<String>,
    /// Built-in default tier (static table)
    pub builtin_default: String,
    /// Global override; None = follow the default (camelCase would produce overrideDecision,
    /// but the frontend contract wants "override", hence the explicit rename)
    #[serde(rename = "override")]
    pub override_decision: Option<String>,
    /// Effective decision at the global layer: denied when global is denied, otherwise equal to the built-in default
    pub effective: String,
    pub high_risk: bool,
}

/// Settings page "System capabilities" data: one row per permission **with a capability mapping**.
/// Permissions without a mapping (pet.animation / storage.local / network.request / microphone /
/// camera / process.execute / plugin.install) are not listed — they are not subject to a capability gate.
pub fn core_policy_list(store: &SharedStore) -> Vec<CorePermPolicyDto> {
    let mut out: Vec<CorePermPolicyDto> = PERMISSIONS
        .iter()
        .filter_map(|(perm, default)| {
            let capabilities = core_capabilities_for(perm);
            if capabilities.is_empty() {
                return None;
            }
            let ov = global_override(store, perm);
            Some(CorePermPolicyDto {
                permission: perm.to_string(),
                capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
                builtin_default: default.to_string(),
                override_decision: ov.map(|d| decision_str(d).to_string()),
                effective: match ov {
                    Some(Decision::Denied) => "denied".to_string(),
                    _ => default.to_string(),
                },
                high_risk: HIGH_RISK.contains(perm),
            })
        })
        .collect();
    out.sort_by(|a, b| a.permission.cmp(&b.permission));
    out
}
