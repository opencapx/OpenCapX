//! plugin install/update/config/settings-view commands.
//! Mechanical move from main.rs.

use super::*;

/// **dev/test only; the UI exposes only .ocplugin and the marketplace**: install from an unpacked directory,
/// for test fixtures and local development.
#[tauri::command]
pub(crate) async fn install_plugin(dir: String) -> Result<String, String> {
    // same as install_ocplugin: the install phase blocks on permission answers, so it cannot occupy the main thread
    tauri::async_runtime::spawn_blocking(move || {
        crate::core::plugin::PluginManager::shared()
            .install_from_dir(&std::path::PathBuf::from(dir))
    })
    .await
    .map_err(|e| format!("install task failed: {}", e))?
}

/// Install .ocplugin. **Must be async**: install asks the user for each permission during the commit phase
/// (`opencapx-install-ask`, each blocking up to 60s), and Tauri sync commands run on the main thread
/// — while frozen the window cannot even draw the ask prompt, so the user just sees "clicked install, page stuck".
/// Async commands run on the thread pool, the main thread keeps drawing the UI, and the ask becomes visible.
#[tauri::command]
pub(crate) async fn install_ocplugin(
    path: String,
    confirm_unsigned: Option<bool>,
    confirm_key_change: Option<bool>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::core::plugin::PluginManager::shared().install_ocplugin_ex(
            &std::path::PathBuf::from(path),
            install_opts(confirm_unsigned, confirm_key_change),
        )
    })
    .await
    .map_err(|e| format!("install task failed: {}", e))?
}

/// F7 — assemble install confirmation options (command args → core options).
pub(crate) fn install_opts(
    confirm_unsigned: Option<bool>,
    confirm_key_change: Option<bool>,
) -> crate::core::plugin::InstallOptions {
    crate::core::plugin::InstallOptions {
        confirm_unsigned: confirm_unsigned.unwrap_or(false),
        confirm_key_change: confirm_key_change.unwrap_or(false),
    }
}

/// Wow 5 — for the settings.ts dialog preview. Returns manifest fields + permissions
/// (with the high_risk flag), without actually unpacking/installing.
#[tauri::command]
pub(crate) fn preview_ocplugin(
    path: String,
) -> Result<crate::core::plugin::PluginPreviewDto, String> {
    crate::core::plugin::PluginManager::preview_ocplugin(&std::path::PathBuf::from(path))
}

#[tauri::command]
pub(crate) fn list_automation_rules() -> Vec<serde_json::Value> {
    crate::core::automation::load_rules()
}

#[tauri::command]
pub(crate) fn add_automation_rule(
    when: serde_json::Value,
    then: serde_json::Value,
) -> Result<serde_json::Value, String> {
    crate::core::automation::add_rule(&when, &then)
}

#[tauri::command]
pub(crate) fn remove_automation_rule(id: String) -> Result<bool, String> {
    crate::core::automation::remove_rule(&id)
}

#[tauri::command]
pub(crate) fn set_automation_rule_enabled(
    id: String,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    crate::core::automation::set_rule_enabled(&id, enabled)
}

#[tauri::command]
pub(crate) fn list_rules() -> Vec<crate::core::rules::RuleSummary> {
    crate::core::rules::list_summaries()
}

#[tauri::command]
pub(crate) fn set_rule_enabled(id: String, enabled: bool) -> Result<(), String> {
    crate::core::rules::set_enabled_in_global(&id, enabled)
}

#[tauri::command]
pub(crate) fn add_rule(rule: serde_json::Value) -> Result<(), String> {
    crate::core::rules::add_rule_to_global(rule)
}

#[tauri::command]
pub(crate) fn list_trusted_projects() -> Vec<String> {
    crate::core::rules::trusted_projects()
}

#[tauri::command]
pub(crate) fn trust_project(path: String) -> Result<(), String> {
    crate::core::rules::trust(std::path::Path::new(&path))
}

#[tauri::command]
pub(crate) fn untrust_project(path: String) -> Result<(), String> {
    crate::core::rules::untrust(std::path::Path::new(&path))
}

#[tauri::command]
pub(crate) fn list_marketplace(refresh: bool) -> Vec<crate::core::marketplace::PluginMarketEntry> {
    if refresh && crate::core::marketplace::refresh().is_err() {
        // on fetch failure fall back to the local index so the UI is not blank
    }
    crate::core::marketplace::load_index().entries
}

#[tauri::command]
pub(crate) fn toggle_plugin(id: String) -> Result<bool, String> {
    crate::core::plugin::PluginManager::shared().toggle(&id)
}

#[tauri::command]
pub(crate) fn uninstall_plugin(id: String) -> Result<(), String> {
    crate::core::plugin::PluginManager::shared().uninstall(&id)
}

#[tauri::command]
pub(crate) fn preview_uninstall_plugin(
    id: String,
) -> Result<crate::core::plugin::UninstallPreviewDto, String> {
    crate::core::plugin::PluginManager::shared().uninstall_preview(&id)
}

#[tauri::command]
pub(crate) fn set_plugin_auto_reload(id: String, on: bool) -> Result<(), String> {
    crate::core::plugin::PluginManager::shared().set_auto_reload(&id, on)
}

#[derive(serde::Serialize)]
pub(crate) struct PluginConfigSnapshot {
    /// At minimum merges installed plugin ids; those without a config file are listed too (empty object).
    plugins: Vec<crate::core::PluginConfigEntry>,
}

#[tauri::command]
pub(crate) fn list_plugin_config() -> PluginConfigSnapshot {
    // union installed plugins with existing config files so the UI gets the full view in one go.
    let installed: Vec<String> = crate::core::plugin::PluginManager::shared()
        .list()
        .into_iter()
        .map(|p| p.id)
        .collect();
    let mut by_id: std::collections::HashMap<String, serde_json::Value> =
        crate::core::config::snapshot().into_iter().collect();
    let mut out: Vec<crate::core::PluginConfigEntry> = Vec::new();
    for id in installed {
        let cfg = by_id.remove(&id).unwrap_or(serde_json::json!({}));
        out.push(crate::core::PluginConfigEntry { id, config: cfg });
    }
    // list uninstalled plugins that still have config (so a reinstall does not lose it)
    for (id, cfg) in by_id {
        out.push(crate::core::PluginConfigEntry { id, config: cfg });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    PluginConfigSnapshot { plugins: out }
}

#[tauri::command]
pub(crate) fn get_plugin_config(id: String) -> serde_json::Value {
    serde_json::Value::Object(crate::core::config::all(&id))
}

#[tauri::command]
pub(crate) fn set_plugin_config(id: String, value: serde_json::Value) -> Result<(), String> {
    crate::core::config::replace(&id, &value).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn delete_plugin_config(id: String) -> Result<(), String> {
    crate::core::config::reset(&id).map_err(|e| e.to_string())
}

/// M7/F8 — generic settings page: declared schema + current values (secret only reports "set", value is not returned).
#[tauri::command]
pub(crate) fn list_plugin_settings(
    id: String,
) -> Result<crate::core::plugin::SettingsViewDto, String> {
    crate::core::plugin::PluginManager::settings_view(&id)
}

/// Plugin detail page: read the plugin directory README.md (None = not provided).
#[tauri::command]
pub(crate) fn read_plugin_readme(id: String) -> Result<Option<String>, String> {
    crate::core::plugin::PluginManager::readme(&id)
}

/// Corrupt-DB quarantine notice (None = none). Read once when the settings page starts.
#[tauri::command]
pub(crate) fn db_recovery_notice() -> Option<serde_json::Value> {
    crate::core::profile::db_recovery_notice()
}

/// User clicks "Got it" → delete the notice file (the quarantined corrupt DB file is kept for later data recovery).
#[tauri::command]
pub(crate) fn dismiss_db_recovery_notice() -> Result<(), String> {
    crate::core::profile::dismiss_db_recovery_notice()
}

/// P3 — list control: forwards CRUD ops to the plugin method `settings.<key>` (items belong to the plugin).
#[tauri::command]
pub(crate) fn invoke_plugin_setting_list(
    id: String,
    key: String,
    op: String,
    index: Option<usize>,
    to: Option<usize>,
    value: Option<String>,
) -> Result<serde_json::Value, String> {
    crate::core::plugin::PluginManager::invoke_setting_list_op(&id, &key, &op, index, to, value)
}

/// M7/F8 — write a single declarative setting (secret goes to keychain; key must be within the declaration).
#[tauri::command]
pub(crate) fn set_plugin_setting(
    id: String,
    key: String,
    value: serde_json::Value,
) -> Result<(), String> {
    crate::core::plugin::PluginManager::set_setting_value(&id, &key, &value)
}

/// M7/F8 — button control: calls the plugin method `settings.<key>`.
#[tauri::command]
pub(crate) fn invoke_plugin_setting_action(
    id: String,
    key: String,
) -> Result<serde_json::Value, String> {
    crate::core::plugin::PluginManager::invoke_setting_action(&id, &key)
}
