//! the install-time ask flow: defaults, confirm_install (interactive and noninteractive), decision commits.
//! Mechanical move from core/permission.rs.

use super::*;

/// One item of install confirmation (§4.3 integration points #3 / #7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallAsk {
    pub permission: String,
    /// true = derived from a plugin-domain declaration → **once-only** (no Always; not grantable on the settings page)
    pub declared: bool,
    /// The default written for a declared item in the manifest (ask | denied); ignored for built-in items
    pub declared_default: String,
}

/// Default for declared-derived (§4.3 integration point #2): static table first, then frozen declaration table, then denied.
/// **Never implicit granted**.
pub fn default_decision_for(store: &SharedStore, permission: &str) -> Decision {
    if known(permission) {
        return default_decision(permission);
    }
    crate::core::declaration::declared_default(store, permission).unwrap_or(Decision::Denied)
}

/// Whether a permission name is enforceable: built-in vocabulary ∪ frozen declaration table (shared by §4.3 integration points #3 / #4 / #5).
pub fn known_or_declared(store: &SharedStore, permission: &str) -> bool {
    known(permission) || crate::core::declaration::is_declared_permission(store, permission)
}

/// Whether the permission name is **declared-derived** (the criterion for the four once-only enforcement points, §4.3).
pub fn is_declared(store: &SharedStore, permission: &str) -> bool {
    crate::core::declaration::is_declared_permission(store, permission)
}

/// once-only criterion (§4.3 enforcement points 1/2): neither **high-risk** nor **declared-derived** permissions may offer Always.
/// Shared by `gate()` and `gate_agent()`; if either is true then `can_always = false`.
pub fn can_always(store: &SharedStore, permission: &str) -> bool {
    !HIGH_RISK.contains(&permission) && !is_declared(store, permission)
}

/// Permission confirmation at install time: the implementation behind the install bubble in docs/permissions.md.
/// - Shows a one-time dialog with per-item Always Allow / Permit Once / Deny (high-risk only Permit Once/Deny)
/// - §4.4 consent-before-commit (review C2): **collects decisions only, zero DB writes** —
///   returns a `(permission, decision)` list; the caller swaps the directory then persists in a single transaction;
///   deny / timeout → Err, and the caller must not produce any effective change (the installed version is unaffected)
/// - UI absent (test process): collect per the default table (review 4); persistence is likewise left to the commit phase
pub fn confirm_install(
    plugin_id: &str,
    plan: &[InstallAsk],
) -> Result<Vec<(String, String)>, String> {
    confirm_install_with(default_asker(), plugin_id, plan)
}

/// F12 — injectable version of confirm_install: identical behavior (UI absent collects per the default table;
/// deny / timeout / invalid answer → Err, and the caller must not produce any effective change).
pub fn confirm_install_with(
    asker: &dyn Asker,
    plugin_id: &str,
    plan: &[InstallAsk],
) -> Result<Vec<(String, String)>, String> {
    if !asker.is_available() {
        return confirm_install_noninteractive(plugin_id, plan);
    }
    let mut decisions: Vec<(String, String)> = Vec::with_capacity(plan.len());
    for ask in plan {
        let perm = &ask.permission;
        if !ask.declared && !known(perm) {
            audit(
                "denied",
                plugin_id,
                perm,
                json!({ "reason": "unknown-permission", "caller": "install" }),
            );
            return Err(format!(
                "unknown permission {} (plugin declares unknown capability?)",
                perm
            ));
        }
        // review 3 + 7: declared-derived permissions **must not be Always** (the install dialog must block it too,
        // otherwise H2's once-only only blocks the runtime prompt)
        let can_always = !HIGH_RISK.contains(&perm.as_str()) && !ask.declared;
        let id = format!("install-{}-{}", plugin_id, nanos());
        audit(
            "install_requested",
            plugin_id,
            perm,
            json!({ "scope": null, "caller": "install", "canAlways": can_always, "declared": ask.declared }),
        );
        let payload = json!({
            "id": id,
            "pluginId": plugin_id,
            "permission": perm,
            "canAlways": can_always,
            "declared": ask.declared,
        });
        let decision = match asker.ask(&AskRequest {
            id: id.clone(),
            event: "opencapx-install-ask",
            payload,
            timeout: ASK_TIMEOUT,
        }) {
            AskOutcome::Answered(a) => match a.as_str() {
                "always" if can_always => {
                    audit(
                        "granted",
                        plugin_id,
                        perm,
                        json!({ "decision": "always", "caller": "install" }),
                    );
                    "granted"
                }
                // §4.3 enforcement point 4: answering Always when Always is not allowed (high-risk / declared-derived) →
                // downgrade to once rather than rejecting the whole batch (an out-of-sync old frontend will not break the install)
                "always" => {
                    audit(
                        "granted",
                        plugin_id,
                        perm,
                        json!({ "decision": "once", "caller": "install", "downgradedFrom": "always" }),
                    );
                    "ask"
                }
                "once" => {
                    audit(
                        "granted",
                        plugin_id,
                        perm,
                        json!({ "decision": "once", "caller": "install" }),
                    );
                    "ask"
                }
                "deny" => {
                    audit(
                        "denied",
                        plugin_id,
                        perm,
                        json!({ "reason": "user", "caller": "install" }),
                    );
                    // the user denying one item at install → the whole plugin install fails (aligned with the docs install flow: item-by-item confirmation).
                    // at this point the directory has not been swapped / nothing persisted, so the installed version is intact (consent-before-commit).
                    return Err(format!("install denied: {} {}", plugin_id, perm));
                }
                _ => {
                    audit(
                        "denied",
                        plugin_id,
                        perm,
                        json!({ "reason": "invalid-answer", "caller": "install" }),
                    );
                    return Err(format!("install aborted: {} {}", plugin_id, perm));
                }
            },
            AskOutcome::Timeout => {
                if let Some(app) = crate::core::app_handle() {
                    let _ = app.emit(
                        "opencapx-install-ask-done",
                        json!({ "id": id, "answer": null }),
                    );
                }
                audit(
                    "denied",
                    plugin_id,
                    perm,
                    json!({ "reason": "timeout", "caller": "install" }),
                );
                return Err(format!("install timed out: {} {}", plugin_id, perm));
            }
            AskOutcome::NoUi => {
                audit(
                    "denied",
                    plugin_id,
                    perm,
                    json!({ "reason": "no-ui", "caller": "install" }),
                );
                return Err(format!("install aborted (no ui): {} {}", plugin_id, perm));
            }
        };
        decisions.push((perm.clone(), decision.to_string()));
    }
    Ok(decisions)
}

/// Non-interactive confirmation (no UI: tests / CI). §4.4 review 4: collect per "declared default / static-table default",
/// **does not write to the DB** — persistence is likewise left to the commit phase. audit is marked `install-no-ui` to distinguish it.
pub(crate) fn confirm_install_noninteractive(
    plugin_id: &str,
    plan: &[InstallAsk],
) -> Result<Vec<(String, String)>, String> {
    let mut decisions: Vec<(String, String)> = Vec::with_capacity(plan.len());
    for ask in plan {
        let perm = &ask.permission;
        if !ask.declared && !known(perm) {
            audit(
                "denied",
                plugin_id,
                perm,
                json!({ "reason": "unknown-permission", "caller": "install-no-ui" }),
            );
            return Err(format!("unknown permission {}", perm));
        }
        let d = if ask.declared {
            parse_decision(&ask.declared_default)
        } else {
            default_decision(perm)
        };
        decisions.push((perm.clone(), decision_str(d).to_string()));
        audit(
            if d == Decision::Denied {
                "denied"
            } else {
                "granted"
            },
            plugin_id,
            perm,
            json!({
                "decision": if d == Decision::Ask { "ask-default" } else { "default" },
                "caller": "install-no-ui",
                "declared": ask.declared
            }),
        );
    }
    Ok(decisions)
}

/// Commit phase (§4.4 step 5): writes the confirmed decisions into `plugin_permissions` in a **single transaction**.
/// Called only after the user confirms everything; any failure rolls back the whole batch (no half-applied decisions).
/// Empty list = no-op.
pub fn commit_install_decisions(
    store: &SharedStore,
    plugin_id: &str,
    decisions: &[(String, String)],
) -> Result<(), String> {
    if decisions.is_empty() {
        return Ok(());
    }
    let now = crate::core::agent::now_secs();
    let Ok(mut s) = store.lock() else {
        return Err("storage unavailable".into());
    };
    match s.try_with_conn(|c| {
        let tx = c
            .unchecked_transaction()
            .map_err(|e| format!("begin failed: {}", e))?;
        upsert_install_decisions_in_tx(&tx, plugin_id, decisions, now as i64)?;
        tx.commit().map_err(|e| format!("commit failed: {}", e))?;
        Ok(())
    }) {
        Some(r) => r,
        None => Err("sqlite unavailable".into()),
    }
}

/// Upserts a batch of install decisions within a transaction (`plugin_permissions`). The install commit phase and
/// `commit_install_decisions` share this single SQL source.
pub fn upsert_install_decisions_in_tx(
    tx: &rusqlite::Transaction,
    plugin_id: &str,
    decisions: &[(String, String)],
    now: i64,
) -> Result<(), String> {
    for (perm, decision) in decisions {
        tx.execute(
            "INSERT INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at)
             VALUES (?1, ?2, NULL, ?3, ?4)
             ON CONFLICT(plugin_id, permission) DO UPDATE SET decision = ?3, updated_at = ?4",
            rusqlite::params![plugin_id, perm, decision, now],
        )
        .map_err(|e| format!("write {} failed: {}", perm, e))?;
    }
    Ok(())
}
