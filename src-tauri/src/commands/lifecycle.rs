//! plugin lifecycle, kill switch, safe mode, metrics, profiles commands.
//! Mechanical move from main.rs.

/// Phase 32 — full plugin capability dependency graph (nodes = installed plugins, edges = shared capabilities).
#[tauri::command]
pub(crate) fn list_plugin_dependency_graph() -> crate::core::plugin::CapabilityGraphDto {
    crate::core::plugin::PluginManager::shared().capability_dependency_graph()
}

/// Phase 33 — permission risk heatmap (cells + global denied top-N).
#[tauri::command]
pub(crate) fn list_permission_heatmap() -> crate::core::permission::PermissionHeatmapDto {
    crate::core::permission::heatmap()
}

/// Phase 42 — topological layered start plan (layer[i] is the set of plugins started in parallel at step i) + reversed stop_order.
#[tauri::command]
pub(crate) fn get_plugin_lifecycle_plan() -> crate::core::lifecycle_order::LifecyclePlanDto {
    crate::core::lifecycle_order::compute_lifecycle_plan(
        &crate::core::plugin::PluginManager::shared(),
    )
}

/// Phase 42 — start all installed plugins in plan order. Returns (started_count, errors).
#[derive(serde::Serialize)]
pub(crate) struct StartAllResult {
    started: usize,
    errors: Vec<String>,
}

#[tauri::command]
pub(crate) fn start_all_plugins() -> StartAllResult {
    let mgr = crate::core::plugin::PluginManager::shared();
    let plan = crate::core::lifecycle_order::compute_lifecycle_plan(&mgr);
    let (started, errors) = crate::core::lifecycle_order::start_all_in_order(&mgr, &plan);
    StartAllResult { started, errors }
}

/// Phase 42 — stop all plugins in reverse plan order. Returns the stopped count.
#[tauri::command]
pub(crate) fn stop_all_plugins() -> usize {
    let mgr = crate::core::plugin::PluginManager::shared();
    let plan = crate::core::lifecycle_order::compute_lifecycle_plan(&mgr);
    crate::core::lifecycle_order::stop_all_in_order(&mgr, &plan)
}

/// --safe-mode read-only state (for the settings page banner; leaving safe mode requires an app restart).
#[tauri::command]
pub(crate) fn get_safe_mode_state() -> bool {
    crate::core::safe_mode::is_active()
}

/// Phase 44 — Kill switch global disable toggle.
/// Enabling the kill switch: force stop all running plugins immediately + emit the `plugin.kill_switch.enabled` event.
#[tauri::command]
pub(crate) fn enable_kill_switch(
    reason: Option<String>,
) -> crate::core::kill_switch::KillSwitchStateDto {
    let r = reason.unwrap_or_default();
    let mgr = crate::core::plugin::PluginManager::shared();
    let plugins = mgr.list();
    crate::core::kill_switch::enable(&r, "operator");
    for p in &plugins {
        mgr.stop(&p.id);
    }
    crate::core::kill_switch::state()
}

/// Disabling the kill switch: clears state only, does not auto-restart any plugin (the user can manually start what they need).
#[tauri::command]
pub(crate) fn disable_kill_switch() -> crate::core::kill_switch::KillSwitchStateDto {
    crate::core::kill_switch::disable();
    crate::core::kill_switch::state()
}

#[tauri::command]
pub(crate) fn get_kill_switch_state() -> crate::core::kill_switch::KillSwitchStateDto {
    crate::core::kill_switch::state()
}

/// Phase 45 — fetch the latest metrics snapshot for all currently running plugins.
#[tauri::command]
pub(crate) fn get_plugin_metrics() -> Vec<crate::core::plugin_metrics::PluginMetricsSnapshot> {
    crate::core::plugin_metrics::latest_snapshots()
}

/// Phase 45 — fetch the last N historical samples for a plugin (for charts).
#[derive(serde::Deserialize)]
pub(crate) struct MetricsHistoryArgs {
    #[serde(default)]
    plugin_id: String,
    #[serde(default)]
    since_ts: i64,
    #[serde(default = "default_metrics_limit")]
    limit: usize,
}

pub(crate) fn default_metrics_limit() -> usize {
    600
}

#[tauri::command]
pub(crate) fn get_plugin_metrics_history(
    args: MetricsHistoryArgs,
) -> Vec<crate::core::plugin_metrics::PluginMetricsSnapshot> {
    crate::core::plugin_metrics::history(&args.plugin_id, args.since_ts, args.limit)
}

/// Phase 45 — get the monitoring config.
#[tauri::command]
pub(crate) fn get_metrics_config() -> crate::core::plugin_metrics::MetricsConfig {
    crate::core::plugin_metrics::load_config()
}

/// Phase 45 — save the monitoring config.
#[tauri::command]
pub(crate) fn set_metrics_config(
    cfg: crate::core::plugin_metrics::MetricsConfig,
) -> Result<(), String> {
    crate::core::plugin_metrics::save_config(&cfg)
}

/// Phase 46 — all profiles (with plugin count).
#[tauri::command]
pub(crate) fn list_workspace_profiles() -> Vec<crate::core::profile::ProfileInfo> {
    crate::core::profile::list_profiles()
}

/// Phase 46 — create a profile.
#[tauri::command]
pub(crate) fn create_workspace_profile(name: String) -> crate::core::profile::ProfileInfo {
    crate::core::profile::create_profile(&name).unwrap_or_else(|e| {
        crate::core::profile::ProfileInfo {
            name,
            is_active: false,
            plugin_count: 0,
            created_at: 0,
        }
    })
}

/// Phase 46 — switch profile (stop all plugins + swap store + emit event).
#[tauri::command]
pub(crate) fn switch_workspace_profile(name: String) -> Result<(), String> {
    crate::core::profile::switch_profile(&name)
}

/// Phase 46 — delete a profile (rejects active / default / only profile).
#[tauri::command]
pub(crate) fn delete_workspace_profile(name: String) -> Result<(), String> {
    crate::core::profile::delete_profile(&name)
}

/// Phase 47 — read alerting config (returns default if never saved, enabled=false).
#[tauri::command]
pub(crate) fn get_alerting_config() -> crate::core::alerting::WebhookConfig {
    crate::core::alerting::load_config()
}

/// Phase 47 — write alerting config (validated then stored in SQLite; the dispatcher picks it up next time).
#[tauri::command]
pub(crate) fn set_alerting_config(cfg: crate::core::alerting::WebhookConfig) -> Result<(), String> {
    crate::core::alerting::save_config(&cfg)
}

/// Phase 47 — immediately send one test payload to the webhook, returns the HTTP status code or Err(reason).
/// bypasses dedup so the user sees the result immediately.
#[tauri::command]
pub(crate) fn test_alerting_webhook() -> Result<u16, String> {
    crate::core::alerting::test_send()
}

/// Phase 48 — list failed deliveries (state=None lists all; "pending"/"exhausted"/"resolved" filter).
#[tauri::command]
pub(crate) fn list_alerting_failed(
    state: Option<String>,
    limit: usize,
) -> Vec<crate::core::storage::FailedDeliveryRow> {
    crate::core::alerting::list_failed_deliveries(state.as_deref(), limit)
}

/// Phase 48 — manually retry one dead letter (resets attempts=0, state=pending, next_retry_ts=now).
#[tauri::command]
pub(crate) fn retry_alerting_failed(id: String) -> Result<(), String> {
    if crate::core::alerting::manual_retry_failed_delivery(&id) {
        Ok(())
    } else {
        Err(format!("not found: {}", id))
    }
}

/// Phase 48 — delete one dead letter.
#[tauri::command]
pub(crate) fn delete_alerting_failed(id: String) -> bool {
    crate::core::alerting::delete_failed_delivery(&id)
}

/// Phase 48 — clear exhausted + resolved rows (keeps pending).
#[tauri::command]
pub(crate) fn clear_alerting_resolved() -> usize {
    crate::core::alerting::clear_resolved_failed_deliveries()
}

/// Phase 48 — read retry config (default).
#[tauri::command]
pub(crate) fn get_alerting_retry_config() -> crate::core::alerting::RetryConfig {
    crate::core::alerting::load_retry_config()
}

/// Phase 48 — write retry config (validated then stored in SQLite).
#[tauri::command]
pub(crate) fn set_alerting_retry_config(
    cfg: crate::core::alerting::RetryConfig,
) -> Result<(), String> {
    crate::core::alerting::save_retry_config(&cfg)
}

/// Phase 49 — list all alerting endpoints (including disabled), by created_at ascending.
#[tauri::command]
pub(crate) fn list_alerting_endpoints() -> Vec<crate::core::storage::AlertingEndpointRow> {
    crate::core::alerting::list_endpoints()
}

/// Phase 49 — create/update endpoint (empty id = create, non-empty = overwrite). validate_url + unique name.
#[tauri::command]
pub(crate) fn save_alerting_endpoint(
    ep: crate::core::alerting::WebhookEndpoint,
) -> Result<crate::core::storage::AlertingEndpointRow, String> {
    crate::core::alerting::save_endpoint(ep)
}

/// Phase 49 — delete endpoint, cascading clears its dead letters. Returns (deleted, cleared).
#[tauri::command]
pub(crate) fn delete_alerting_endpoint(id: String) -> Result<(bool, usize), String> {
    crate::core::alerting::delete_endpoint(&id)
}

/// Phase 49 — immediately send one test payload to a single endpoint (signed), returns status or Err.
#[tauri::command]
pub(crate) fn test_alerting_endpoint(id: String) -> Result<u16, String> {
    crate::core::alerting::test_endpoint(&id)
}

/// Phase 58 — dry-run preview: builds an envelope from the frontend-supplied sample JSON and runs render + lint,
/// producing a TemplatePreviewResult (body + content-type + diagnostics). On render failure
/// body="" but diagnostics still contains render_failed (line / column).
#[tauri::command]
pub(crate) fn preview_alerting_template(
    template: String,
    sample: serde_json::Value,
) -> crate::core::alerting::TemplatePreviewResult {
    crate::core::alerting::preview_alerting_template(&template, &sample)
}

/// Phase 59 — list all template presets (5 builtin + user-defined).
#[tauri::command]
pub(crate) fn list_alerting_template_presets() -> Vec<crate::core::alerting::TemplatePreset> {
    crate::core::alerting::list_template_presets()
}

/// Phase 59 — get a single preset by kind (builtin:<slug> or user:<uuid>).
#[tauri::command]
pub(crate) fn get_alerting_template_preset(
    kind: String,
) -> Option<crate::core::alerting::TemplatePreset> {
    crate::core::alerting::get_template_preset(&kind)
}

/// Phase 59 — save / update a user-defined preset. Auto-generates a UUID when id is empty.
#[tauri::command]
pub(crate) fn save_alerting_template_preset(
    preset: crate::core::alerting::TemplatePreset,
) -> Result<crate::core::alerting::TemplatePreset, String> {
    crate::core::alerting::save_user_template_preset(&preset)
}

/// Phase 59 — delete only user-defined presets (builtin untouched). Returns true = actually deleted.
#[tauri::command]
pub(crate) fn delete_alerting_template_preset(id: String) -> bool {
    crate::core::alerting::delete_user_template_preset(&id)
}

/// Phase 59 — serialize the given preset list to a YAML / JSON string (serde_yaml also accepts JSON input).
#[tauri::command]
pub(crate) fn export_alerting_presets(
    presets: Vec<crate::core::alerting::TemplatePreset>,
) -> Result<String, String> {
    crate::core::alerting::export_presets_to_yaml(&presets)
}

/// Phase 59 — bulk import presets from a YAML / JSON string (forces builtin=false). Returns the import count.
#[tauri::command]
pub(crate) fn import_alerting_presets(yaml: String) -> Result<usize, String> {
    crate::core::alerting::import_presets_from_yaml(&yaml)
}
