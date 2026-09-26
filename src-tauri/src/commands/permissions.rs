//! permission and agent-identity commands.
//! Mechanical move from main.rs.

#[tauri::command]
pub(crate) fn list_permissions() -> Vec<crate::core::permission::PluginPermissionsDto> {
    let plugins = crate::core::plugin::PluginManager::shared().list();
    match crate::core::shared_store() {
        Some(store) => crate::core::permission::view(&store, &plugins),
        None => Vec::new(),
    }
}

#[tauri::command]
pub(crate) fn set_permission(
    plugin_id: String,
    permission: String,
    decision: String,
) -> Result<bool, String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("storage unavailable".into());
    };
    // docs/permissions.md: high-risk permissions can only be granted once at a time, no persistent granted
    if crate::core::permission::HIGH_RISK.contains(&permission.as_str()) && decision == "granted" {
        return Err(
            "high-risk permission cannot be granted permanently; allow once at runtime".into(),
        );
    }
    // docs/permission-domains.md §4.3 enforcement point 3: declared-derived permissions are once-only —
    // set_decision rejects granted, so give a readable reason here first (otherwise only a generic error is shown)
    if decision == "granted" && crate::core::permission::is_declared(&store, &permission) {
        return Err(
            "permission comes from a third-party domain declaration; it is once-only and cannot be granted permanently"
                .into(),
        );
    }
    if crate::core::permission::set_decision(&store, &plugin_id, &permission, &decision) {
        Ok(true)
    } else {
        Err("failed to save decision (unknown permission/decision or sqlite unavailable)".into())
    }
}

/// Agents view list: identity + status + last seen. The token never leaves Core.
#[tauri::command]
pub(crate) fn agents_list() -> Vec<crate::core::identity::AgentDto> {
    match crate::core::shared_store() {
        Some(store) => crate::core::identity::list(&store),
        None => Vec::new(),
    }
}

/// Revoke: token invalidated immediately (40102), auto re-registration rejected; only reauthorize can lift it.
#[tauri::command]
pub(crate) fn agent_revoke(agent_id: String) -> Result<bool, String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("storage unavailable".into());
    };
    if crate::core::identity::revoke(&store, &agent_id) {
        Ok(true)
    } else {
        Err("agent not found or already revoked".into())
    }
}

/// Reauthorize: issue a new token (old token void). Returns the token for the settings page to show once,
/// the user updates the CLI token file manually; nothing persists in frontend state.
#[tauri::command]
pub(crate) fn agent_reauthorize(agent_id: String) -> Result<String, String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("storage unavailable".into());
    };
    crate::core::identity::reauthorize(&store, &agent_id).ok_or_else(|| "agent not found".into())
}

/// Agent-layer decision (agent_permissions table). High-risk permissions likewise disallow persistent granted.
#[tauri::command]
pub(crate) fn agent_set_permission(
    agent_id: String,
    permission: String,
    decision: String,
) -> Result<bool, String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("storage unavailable".into());
    };
    if crate::core::permission::HIGH_RISK.contains(&permission.as_str()) && decision == "granted" {
        return Err(
            "high-risk permission cannot be granted permanently; allow once at runtime".into(),
        );
    }
    if decision == "granted" && crate::core::permission::is_declared(&store, &permission) {
        return Err(
            "permission comes from a third-party domain declaration; it is once-only and cannot be granted permanently"
                .into(),
        );
    }
    if crate::core::identity::set_agent_decision(&store, &agent_id, &permission, &decision) {
        Ok(true)
    } else {
        Err("failed to save decision (unknown permission/decision or sqlite unavailable)".into())
    }
}

/// Full permission vocabulary for a single agent (decision = agent override > default table).
#[tauri::command]
pub(crate) fn agent_permissions(
    agent_id: String,
) -> Vec<crate::core::permission::PermissionEntryDto> {
    match crate::core::shared_store() {
        Some(store) => crate::core::permission::agent_view(&store, &agent_id),
        None => Vec::new(),
    }
}

/// Global policy view (settings page "System capabilities" tab; see docs/permissions.md "Global policy (hard gate)").
#[tauri::command]
pub(crate) fn core_permission_list() -> Vec<crate::core::permission::CorePermPolicyDto> {
    match crate::core::shared_store() {
        Some(store) => crate::core::permission::core_policy_list(&store),
        None => Vec::new(),
    }
}

/// Write global policy. Globally granted is rejected for high-risk permissions (same enforcement as agent_set_permission).
#[tauri::command]
pub(crate) fn core_permission_set(permission: String, decision: String) -> Result<bool, String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("storage unavailable".into());
    };
    crate::core::permission::set_global_override(&store, &permission, &decision).map(|_| true)
}

/// Clear the global policy override, restoring the built-in default.
#[tauri::command]
pub(crate) fn core_permission_reset(permission: String) -> Result<bool, String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("storage unavailable".into());
    };
    crate::core::permission::clear_global_override(&store, &permission).map(|_| true)
}
