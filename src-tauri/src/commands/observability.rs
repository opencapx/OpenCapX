//! audit, timeline, notifications, logs, replay, traces commands.
//! Mechanical move from main.rs.

#[derive(serde::Serialize)]
pub(crate) struct AuditEntry {
    id: String,
    kind: String,
    plugin_id: String,
    permission: String,
    decision: String,
    reason: String,
    timestamp: u64,
}

/// Phase 35 — Audit search filter. All 5 frontend fields are optional; empty string / 0 / None all mean "no filtering".
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditSearchFilter {
    #[serde(default)]
    kind_prefix: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    since_ts: Option<u64>,
    #[serde(default)]
    until_ts: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(serde::Serialize)]
pub(crate) struct LogEntry {
    id: String,
    kind: String,
    plugin_id: String,
    level: String,
    source: String,
    message: String,
    timestamp: u64,
}

#[tauri::command]
pub(crate) fn list_audit(limit: usize) -> Vec<AuditEntry> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let events = store
        .lock()
        .ok()
        .map(|s| s.list_events("permission.", limit))
        .unwrap_or_default();
    events
        .into_iter()
        .map(|e| {
            let plugin_id = e
                .payload
                .get("pluginId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let permission = e
                .payload
                .get("permission")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let decision = e
                .payload
                .get("decision")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let reason = e
                .payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            AuditEntry {
                id: e.id,
                kind: e.kind,
                plugin_id,
                permission,
                decision,
                reason,
                timestamp: e.timestamp,
            }
        })
        .collect()
}

/// Phase 35 — Audit search/filter. Empty string / 0 / None all mean "no filtering", equivalent to list_audit's default behavior.
#[tauri::command]
pub(crate) fn search_audit_events(filter: AuditSearchFilter) -> Vec<AuditEntry> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    // clamp limit to [1, 5000] to guard against the frontend passing 0 / huge values
    let limit = filter.limit.unwrap_or(200).clamp(1, 5000);
    let events = store
        .lock()
        .ok()
        .map(|s| {
            s.list_events_filtered(
                filter.kind_prefix.as_deref(),
                filter.query.as_deref(),
                filter.since_ts,
                filter.until_ts,
                limit,
            )
        })
        .unwrap_or_default();
    events
        .into_iter()
        .map(|e| {
            let plugin_id = e
                .payload
                .get("pluginId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let permission = e
                .payload
                .get("permission")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let decision = e
                .payload
                .get("decision")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let reason = e
                .payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            AuditEntry {
                id: e.id,
                kind: e.kind,
                plugin_id,
                permission,
                decision,
                reason,
                timestamp: e.timestamp,
            }
        })
        .collect()
}

/// i2 §14 Activity Timeline prototype entry. The backend only filters and categorizes,
/// the frontend composes the text per kind with i18n templates (presentation does not enter Rust).
#[derive(serde::Serialize)]
pub(crate) struct TimelineEntryDto {
    id: String,
    timestamp: u64,
    kind: String,
    /// agent | capability | permission | security | plugin | pet | system
    category: &'static str,
    /// extracted from payload.agentId; capability events carry agent since v1.1
    pub(crate) agent: String,
    source: String,
    payload: serde_json::Value,
}

/// Kinds included in Timeline: action events with audit value. Noise is excluded:
/// capability.started (completed/failed is enough), plugin.log, plugin.metrics.*,
/// plugin.lifecycle.starting/running (high frequency), pet.state (aggregated heartbeat).
pub(crate) fn timeline_kind_allowed(kind: &str) -> bool {
    matches!(
        kind,
        "agent.started"
            | "agent.registered"
            | "capability.completed"
            | "capability.failed"
            // B10 subscription lifecycle (high-frequency capability.event is excluded: file watches would flood the timeline)
            | "capability.subscribed"
            | "capability.unsubscribed"
            | "permission.granted"
            | "permission.denied"
            | "permission.requested"
            | "auth.rejected"
            | "plugin.installed"
            | "plugin.uninstalled"
            | "plugin.signature.verified"
            | "plugin.start.rejected"
            | "plugin.kill_switch.enabled"
            | "plugin.lifecycle.crashed"
            // M4 — update and revocation semantics: key change, revocation hit, explicit reopen all have audit value
            | "plugin.update.key_changed"
            | "plugin.revoked"
            | "plugin.revoked.reopened"
            // F7 — leave a trace for unsigned installs
            | "plugin.installed.unsigned"
            | "pet.say"
            | "pet.state_changed"
            | "workspace.switched"
            | "notification.posted"
            // Automation (§15): firing is audited; rule_failed is rare (hand-written bad rules), included too
            | "automation.rule_fired"
            | "automation.rule_failed"
    )
}

pub(crate) fn timeline_category(kind: &str) -> &'static str {
    match kind {
        k if k.starts_with("agent.") => "agent",
        k if k.starts_with("capability.") => "capability",
        k if k.starts_with("permission.") => "permission",
        k if k.starts_with("auth.") => "security",
        k if k.starts_with("plugin.") => "plugin",
        k if k.starts_with("pet.") => "pet",
        k if k.starts_with("notification.") => "notification",
        k if k.starts_with("automation.") => "system",
        _ => "system",
    }
}

/// Timeline data source: all events filtered by whitelist, optionally filtered by agent.
/// limit is the count **after** filtering; the underlying layer fetches more (limit*4, capped at 2000) then filters.
#[tauri::command]
pub(crate) fn timeline_events(
    limit: Option<usize>,
    agent: Option<String>,
) -> Vec<TimelineEntryDto> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let limit = limit.unwrap_or(200).clamp(1, 500);
    let fetch = (limit * 4).min(2000);
    let events = store
        .lock()
        .ok()
        .map(|s| s.list_events("", fetch))
        .unwrap_or_default();
    events
        .into_iter()
        .filter(|e| timeline_kind_allowed(&e.kind))
        .filter(|e| match &agent {
            Some(a) if !a.is_empty() => e
                .payload
                .get("agentId")
                .and_then(|v| v.as_str())
                .map(|id| id.contains(a.as_str()))
                .unwrap_or(false),
            _ => true,
        })
        .take(limit)
        .map(|e| TimelineEntryDto {
            agent: e
                .payload
                .get("agentId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            category: timeline_category(&e.kind),
            id: e.id,
            timestamp: e.timestamp,
            kind: e.kind,
            source: e.source,
            payload: e.payload,
        })
        .collect()
}

/// i2 §13 Notification Center entry: the OS toast sent by opencapx.notify is also
/// recorded as a `notification.posted` event, read back here to aggregate for the settings page.
#[derive(serde::Serialize)]
pub(crate) struct NotificationDto {
    id: String,
    timestamp: u64,
    /// payload.agentId (empty = non-Agent context)
    pub(crate) agent: String,
    pub(crate) title: String,
    body: String,
    /// info | warn | error
    pub(crate) severity: String,
}

#[tauri::command]
pub(crate) fn list_notifications(
    limit: Option<usize>,
    agent: Option<String>,
) -> Vec<NotificationDto> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let limit = limit.unwrap_or(200).clamp(1, 500);
    store
        .lock()
        .ok()
        .map(|s| s.list_events("notification.posted", limit))
        .unwrap_or_default()
        .into_iter()
        .filter(|e| match &agent {
            Some(a) if !a.is_empty() => e
                .payload
                .get("agentId")
                .and_then(|v| v.as_str())
                .map(|id| id.contains(a.as_str()))
                .unwrap_or(false),
            _ => true,
        })
        .map(|e| {
            let s = |k: &str| {
                e.payload
                    .get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            NotificationDto {
                id: e.id,
                timestamp: e.timestamp,
                agent: s("agentId"),
                title: s("title"),
                body: s("body"),
                severity: s("severity"),
            }
        })
        .collect()
}

/// Phase 43 — Plugin log filter (query substring + level + plugin_id + since_ts + limit).
#[derive(serde::Deserialize)]
pub(crate) struct LogFilterArgs {
    #[serde(default)]
    query: String,
    #[serde(default)]
    level: String,
    #[serde(default)]
    plugin_id: String,
    #[serde(default)]
    since_ts: Option<u64>,
    #[serde(default)]
    limit: usize,
}

#[tauri::command]
pub(crate) fn search_logs(filter: LogFilterArgs) -> Vec<LogEntry> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let lf = crate::core::log_search::LogFilter {
        query: filter.query,
        level: filter.level,
        plugin_id: filter.plugin_id,
        since_ts: filter.since_ts,
        limit: if filter.limit == 0 { 500 } else { filter.limit },
    };
    crate::core::log_search::search_logs(&store, lf)
        .into_iter()
        .map(|d| LogEntry {
            id: d.id,
            kind: d.kind,
            plugin_id: d.plugin_id,
            level: d.level,
            source: d.source,
            message: d.message,
            timestamp: d.timestamp,
        })
        .collect()
}

/// Phase 34 — Event stream recording: list all sessions (NDJSON files).
#[tauri::command]
pub(crate) fn list_replay_sessions() -> Vec<crate::core::event_replay::ReplaySession> {
    crate::core::event_replay::list_sessions()
}

/// Phase 34 — read a session's events (in order, truncated by limit).
#[tauri::command]
pub(crate) fn get_replay_events(
    session_id: String,
    limit: usize,
) -> Vec<crate::core::event::OpencapxEvent> {
    crate::core::event_replay::read_session(&session_id, limit)
}

/// Phase 34 — replay a session's events into the EventBus (filter optional, empty = no filter).
#[tauri::command]
pub(crate) fn replay_session_to_stream(session_id: String, filter_kind: String) -> usize {
    let bus = crate::core::event::EventBus::shared();
    let filter = if filter_kind.is_empty() {
        None
    } else {
        Some(filter_kind.as_str())
    };
    crate::core::event_replay::replay_to_bus(&session_id, filter, &bus)
}

#[derive(serde::Serialize)]
pub(crate) struct LifecycleEntry {
    id: String,
    kind: String,
    timestamp: u64,
    reason: String,
}

#[tauri::command]
pub(crate) fn list_plugin_lifecycle(id: String, limit: usize) -> Vec<LifecycleEntry> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    // plugin.* event family + filter by pluginId + time descending + limit
    // fetch all related events with the prefix "plugin.", then filter by pluginId in memory.
    let events = store
        .lock()
        .ok()
        .map(|s| s.list_events("plugin.", limit.max(50).min(500)))
        .unwrap_or_default();
    let mut out: Vec<LifecycleEntry> = events
        .into_iter()
        .filter(|e| {
            e.payload.get("pluginId").and_then(|v| v.as_str()) == Some(id.as_str())
                || e.source == id // plugin.log's source looks like plugin:<id>
        })
        .map(|e| LifecycleEntry {
            reason: e
                .payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            id: e.id,
            kind: e.kind,
            timestamp: e.timestamp,
        })
        .collect();
    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    out.truncate(limit);
    out
}

#[tauri::command]
pub(crate) fn list_plugin_traces(id: String) -> Vec<crate::core::plugin_trace::TraceSummary> {
    crate::core::plugin_trace::list_sessions(&id)
}

#[tauri::command]
pub(crate) fn get_plugin_trace(
    id: String,
    session_id: String,
    limit: usize,
) -> Vec<crate::core::plugin_trace::TraceLine> {
    crate::core::plugin_trace::read_session(&id, &session_id, limit.max(50).min(2000))
}

/// Raw lines of a single request chain (descending = newest first).
#[tauri::command]
pub(crate) fn get_rpc_trace(
    agent_id: String,
    trace_id: String,
    limit: usize,
) -> Vec<crate::core::req_trace::RpcTraceLine> {
    crate::core::req_trace::read_trace(&agent_id, &trace_id, limit.max(50).min(2000))
}

/// Raw lines of a single hook session (descending = newest first).
#[tauri::command]
pub(crate) fn get_hook_trace(
    agent_id: String,
    session_id: String,
    limit: usize,
) -> Vec<crate::core::req_trace::RpcTraceLine> {
    crate::core::req_trace::read_hook_trace(&agent_id, &session_id, limit.max(50).min(2000))
}

/// All /rpc request chains (all agents, newest first), with project so the viewer can group by project.
#[tauri::command]
pub(crate) fn list_all_traces() -> Vec<crate::core::req_trace::TraceEntry> {
    crate::core::req_trace::list_all_traces()
}

/// All hook sessions (all agents, newest first), shown alongside request chains and grouped by the same project.
#[tauri::command]
pub(crate) fn list_all_hook_sessions() -> Vec<crate::core::req_trace::TraceEntry> {
    crate::core::req_trace::list_all_hook_sessions()
}

/// Export all /rpc request chain raw files for a project to a user-selected directory and write an index.json manifest.
/// Error codes are in ExportReport (no_chains / dir_unwritable / io: <detail>).
#[tauri::command]
pub(crate) fn export_project_chains(
    dir: String,
    project: String,
) -> Result<crate::core::req_trace::ExportReport, String> {
    crate::core::req_trace::export_chains_to(&dir, &project)
}
