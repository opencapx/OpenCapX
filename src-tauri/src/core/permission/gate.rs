//! the runtime gates: gate/gate_with for plugins and gate_agent/gate_agent_with for agent identities.
//! Mechanical move from core/permission.rs.

use super::*;

/// Execution gate: Granted passes; Ask goes to a prompt (see the runtime check flow in docs/permissions.md).
/// caller goes into the audit event: capability (mcp execute) or plugin.reverse (core.requestPermission).
/// reason is an optional note from plugin.reverse (e.g. "upload image to vision API"); it goes into the audit + prompt text.
pub fn gate(
    store: &SharedStore,
    plugin_id: &str,
    permission: &str,
    caller: &str,
    reason: Option<&str>,
) -> Decision {
    gate_with(
        default_asker(),
        store,
        plugin_id,
        permission,
        caller,
        reason,
    )
}

/// F12 — injectable version of gate: identical behavior to [`gate`], only the "ask" channel is replaceable.
pub fn gate_with(
    asker: &dyn Asker,
    store: &SharedStore,
    plugin_id: &str,
    permission: &str,
    caller: &str,
    reason: Option<&str>,
) -> Decision {
    let d = check(store, plugin_id, permission);
    if d != Decision::Ask {
        return d;
    }
    // session tier: already granted in this process → pass directly (no prompt; audit only lands on the grant)
    if session_has(&format!("plugin:{}", plugin_id), permission) {
        return Decision::Granted;
    }
    if !asker.is_available() {
        // UI absent (test process/frontend not started): fast deny, no pending request left behind
        let mut extra = json!({ "reason": "no-ui", "caller": caller });
        if let Some(r) = reason {
            if let Some(o) = extra.as_object_mut() {
                o.insert(
                    "requestReason".to_string(),
                    serde_json::Value::String(r.to_string()),
                );
            }
        }
        audit("denied", plugin_id, permission, extra);
        return Decision::Denied;
    }
    let id = format!("perm-{}", nanos());
    let mut requested_extra = json!({ "scope": null, "caller": caller });
    if let Some(r) = reason {
        if let (Some(o), Some(e)) = (
            requested_extra.as_object_mut(),
            json!({ "requestReason": r }).as_object(),
        ) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    audit("requested", plugin_id, permission, requested_extra);
    // §4.3 enforcement points 1/2: declared-derived permissions are always once-only (Always-laundering H2)
    let can_always = can_always(store, permission);
    let mut payload = json!({
        "id": id,
        "pluginId": plugin_id,
        "permission": permission,
        "canAlways": can_always,
    });
    if let Some(r) = reason {
        if let Some(o) = payload.as_object_mut() {
            o.insert(
                "reason".to_string(),
                serde_json::Value::String(r.to_string()),
            );
        }
    }
    match asker.ask(&AskRequest {
        id: id.clone(),
        event: "opencapx-permission-ask",
        payload,
        timeout: ASK_TIMEOUT,
    }) {
        AskOutcome::Answered(a) => match a.as_str() {
            "once" => {
                audit(
                    "granted",
                    plugin_id,
                    permission,
                    json!({ "decision": "once" }),
                );
                Decision::Granted
            }
            "always" => {
                if can_always {
                    set_decision(store, plugin_id, permission, "granted");
                    audit(
                        "granted",
                        plugin_id,
                        permission,
                        json!({ "decision": "always" }),
                    );
                } else {
                    // high-risk is not persisted; grant for this call only
                    audit(
                        "granted",
                        plugin_id,
                        permission,
                        json!({ "decision": "once" }),
                    );
                }
                Decision::Granted
            }
            "session" => {
                // v1.5 third tier: in-process memory grant, cleared on restart. Declared-derived (once-only) does not accept it,
                // downgraded to once; high-risk allows it (pressure valve, not persisted).
                if crate::core::declaration::is_declared_permission(store, permission) {
                    audit(
                        "granted",
                        plugin_id,
                        permission,
                        json!({ "decision": "once", "downgradedFrom": "session" }),
                    );
                } else {
                    session_grant(&format!("plugin:{}", plugin_id), permission);
                    audit(
                        "granted",
                        plugin_id,
                        permission,
                        json!({ "decision": "session" }),
                    );
                }
                Decision::Granted
            }
            _ => {
                audit(
                    "denied",
                    plugin_id,
                    permission,
                    json!({ "reason": "user", "caller": caller }),
                );
                Decision::Denied
            }
        },
        AskOutcome::Timeout => {
            // timeout: notify the frontend to dismiss the bubble
            if let Some(app) = crate::core::app_handle() {
                let _ = app.emit(
                    "opencapx-permission-ask-done",
                    json!({ "id": id, "answer": null }),
                );
            }
            audit(
                "denied",
                plugin_id,
                permission,
                json!({ "reason": "timeout", "caller": caller }),
            );
            Decision::Denied
        }
        AskOutcome::NoUi => {
            audit(
                "denied",
                plugin_id,
                permission,
                json!({ "reason": "no-ui", "caller": caller }),
            );
            Decision::Denied
        }
    }
}

/// Agent-layer audit (docs/permissions.md "Audit": permission.* carries an agentId subject).
fn audit_agent(kind: &str, agent_id: &str, permission: &str, extra: serde_json::Value) {
    let mut payload = json!({ "agentId": agent_id, "permission": permission });
    if let (Some(o), Some(e)) = (payload.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
        &format!("permission.{}", kind),
        "core",
        payload,
    ));
}

/// Agent-layer gate (the Agent half of "two-layer judgment" in docs/permissions.md).
/// Isomorphic to gate(), with AgentIdentity as the subject; the prompt event name is separate (opencapx-agent-permission-ask),
/// and frontend answers likewise go through resolve_ask. Core native tools (say/notify/set_state/ask) pass only this layer.
/// v1 note: the two layers are serial gates (this function → plugin-layer gate), so the first ask may show two prompts in sequence;
/// merging into a single prompt is v2 UX; see the deviation note under "two-layer judgment" in docs/permissions.md.
pub fn gate_agent(store: &SharedStore, agent_id: &str, permission: &str, caller: &str) -> Decision {
    gate_agent_with(default_asker(), store, agent_id, permission, caller)
}

/// F12 — injectable version of gate_agent (same behavior as [`gate_agent`]).
pub fn gate_agent_with(
    asker: &dyn Asker,
    store: &SharedStore,
    agent_id: &str,
    permission: &str,
    caller: &str,
) -> Decision {
    let d = crate::core::identity::check_agent(store, agent_id, permission);
    if d != Decision::Ask {
        return d;
    }
    // session tier (same semantics as the plugin layer; subject agent:<id>)
    if session_has(&format!("agent:{}", agent_id), permission) {
        return Decision::Granted;
    }
    if !asker.is_available() {
        // UI absent (test process/frontend not started): fast deny, no pending request left behind. Same behavior as gate().
        audit_agent(
            "denied",
            agent_id,
            permission,
            json!({ "reason": "no-ui", "caller": caller }),
        );
        return Decision::Denied;
    }
    let id = format!("agentperm-{}", nanos());
    audit_agent(
        "requested",
        agent_id,
        permission,
        json!({ "caller": caller }),
    );
    // §4.3 enforcement points 1/2: declared-derived permissions are always once-only (Always-laundering H2)
    let can_always = can_always(store, permission);
    let payload = json!({
        "id": id,
        "agentId": agent_id,
        "displayName": crate::core::identity::display_name_for_agent(store, agent_id),
        "permission": permission,
        "canAlways": can_always,
    });
    match asker.ask(&AskRequest {
        id: id.clone(),
        event: "opencapx-agent-permission-ask",
        payload,
        timeout: ASK_TIMEOUT,
    }) {
        AskOutcome::Answered(a) => match a.as_str() {
            "once" => {
                audit_agent(
                    "granted",
                    agent_id,
                    permission,
                    json!({ "decision": "once", "caller": caller }),
                );
                Decision::Granted
            }
            "session" => {
                // v1.5 third tier (same semantics as the plugin layer): declared-derived downgrades to once, otherwise grant in-process memory
                if crate::core::declaration::is_declared_permission(store, permission) {
                    audit_agent(
                        "granted",
                        agent_id,
                        permission,
                        json!({ "decision": "once", "caller": caller, "downgradedFrom": "session" }),
                    );
                } else {
                    session_grant(&format!("agent:{}", agent_id), permission);
                    audit_agent(
                        "granted",
                        agent_id,
                        permission,
                        json!({ "decision": "session", "caller": caller }),
                    );
                }
                Decision::Granted
            }
            "always" => {
                if can_always {
                    crate::core::identity::set_agent_decision(
                        store, agent_id, permission, "granted",
                    );
                    audit_agent(
                        "granted",
                        agent_id,
                        permission,
                        json!({ "decision": "always", "caller": caller }),
                    );
                } else {
                    // high-risk is not persisted; grant for this call only
                    audit_agent(
                        "granted",
                        agent_id,
                        permission,
                        json!({ "decision": "once", "caller": caller }),
                    );
                }
                Decision::Granted
            }
            _ => {
                audit_agent(
                    "denied",
                    agent_id,
                    permission,
                    json!({ "reason": "user", "caller": caller }),
                );
                Decision::Denied
            }
        },
        AskOutcome::Timeout => {
            if let Some(app) = crate::core::app_handle() {
                let _ = app.emit(
                    "opencapx-agent-permission-ask-done",
                    json!({ "id": id, "answer": null }),
                );
            }
            audit_agent(
                "denied",
                agent_id,
                permission,
                json!({ "reason": "timeout", "caller": caller }),
            );
            Decision::Denied
        }
        AskOutcome::NoUi => {
            audit_agent(
                "denied",
                agent_id,
                permission,
                json!({ "reason": "no-ui", "caller": caller }),
            );
            Decision::Denied
        }
    }
}
