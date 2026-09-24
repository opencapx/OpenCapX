//! marketplace install, registry, security, ratings, SLA, probe, channels, backups, health commands.
//! Mechanical move from main.rs.

use super::*;

/// Update channel definition: per-plugin row ∪ `"stable"` (consistent with check_updates / the settings dropdown).
pub(crate) fn plugin_update_channel(id: &str) -> String {
    crate::core::shared_store()
        .as_ref()
        .and_then(|s| s.lock().ok().map(|g| g.list_plugin_channels()))
        .and_then(|chs| chs.into_iter().find(|(pid, _)| pid == id).map(|(_, c)| c))
        .unwrap_or_else(|| "stable".to_string())
}

pub(crate) fn resolve_market_target(
    id: &str,
    ceiling: &str,
) -> Result<crate::core::marketplace::PluginMarketVersion, String> {
    // M3: if registry has it → registry is authoritative (version selection includes publisher/revocation filtering);
    // not listed or registry unavailable → marketplace (legacy channel).
    if let Some(idx) = crate::core::registry::load_offline() {
        if idx.entries.iter().any(|e| &e.id == id) {
            return crate::core::registry::select_version(
                &idx,
                id,
                env!("CARGO_PKG_VERSION"),
                ceiling,
            )
            .map(crate::core::registry::to_market_version)
            .ok_or_else(|| format!("no core-compatible version available for {}", id));
        }
    }
    let entry =
        crate::core::marketplace::find(id).ok_or_else(|| format!("not in marketplace: {}", id))?;
    crate::core::marketplace::resolve_target_with_ceiling(
        &entry,
        env!("CARGO_PKG_VERSION"),
        ceiling,
    )
}

#[tauri::command]
pub(crate) fn install_marketplace(
    id: String,
    confirm_unsigned: Option<bool>,
    confirm_key_change: Option<bool>,
) -> Result<String, String> {
    // explicit install: no channel subscription gate (channels only govern update supply)
    let target = resolve_market_target(&id, "dev")?;
    let path = crate::core::marketplace::download_target(&id, &target)?;
    crate::core::plugin::PluginManager::shared()
        .install_ocplugin_ex(&path, install_opts(confirm_unsigned, confirm_key_change))
}

#[tauri::command]
pub(crate) fn check_plugin_updates() -> Vec<crate::core::marketplace::PluginUpdateInfo> {
    let installed: Vec<(String, String)> = crate::core::plugin::PluginManager::shared()
        .list()
        .into_iter()
        .map(|p| (p.id, p.version))
        .collect();
    // Phase 38 — fetch all (plugin_id, channel) so marketplace can filter: dev users see beta+stable
    // both are offered; stable users see only stable. Empty → all plugins default to stable.
    let channels = crate::core::shared_store()
        .as_ref()
        .and_then(|s| s.lock().ok().map(|g| g.list_plugin_channels()))
        .unwrap_or_default();
    // M3: registry (verified index) is the **authoritative source** for listed plugins: update supply uses registry version selection
    // (including publisher registration/revocation/channel/core-compat filtering); unlisted plugins continue via marketplace.
    let mut out: Vec<crate::core::marketplace::PluginUpdateInfo> = Vec::new();
    let mut registry_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(idx) = crate::core::registry::load_offline() {
        for (id, current) in &installed {
            if !idx.entries.iter().any(|e| &e.id == id) {
                continue;
            }
            registry_ids.insert(id.clone());
            let ceiling = channels
                .iter()
                .find(|(pid, _)| pid == id)
                .map(|(_, c)| c.as_str())
                .unwrap_or("stable");
            if let Some(v) =
                crate::core::registry::select_version(&idx, id, env!("CARGO_PKG_VERSION"), ceiling)
            {
                if crate::core::marketplace::version_is_newer(&v.version, current) {
                    out.push(crate::core::marketplace::PluginUpdateInfo {
                        id: id.clone(),
                        current_version: current.clone(),
                        latest_version: v.version.clone(),
                        download_url: v.download_url.clone(),
                        sha256: v.sha256.clone(),
                        channel: v.channel.clone().unwrap_or_else(|| "stable".into()),
                    });
                }
            }
        }
    }
    for u in crate::core::marketplace::check_updates(&installed, &channels) {
        if !registry_ids.contains(&u.id) {
            out.push(u);
        }
    }
    out
}

/// M3 — registry v2 status: source (cache/fetch/seed/none), size, and generatedAt.
#[tauri::command]
pub(crate) fn registry_status() -> crate::core::registry::RegistryStatusDto {
    crate::core::registry::status()
}

/// M3 — force-refresh registry (ignores TTL; anti-replay and signature verification still apply); rescan revocations after refresh.
#[tauri::command]
pub(crate) fn registry_refresh() -> Result<crate::core::registry::RegistryStatusDto, String> {
    let loaded = crate::core::registry::refresh()?;
    crate::core::revocation::sweep_with(&loaded.index);
    Ok(crate::core::registry::status_of(&loaded))
}

/// F7 — update preview (unified rendering): after download + verification, returns a full preview (with diff / key change info),
/// `archivePath` lets the user install directly after confirming (no second download).
#[tauri::command]
pub(crate) fn preview_update(id: String) -> Result<serde_json::Value, String> {
    let ceiling = plugin_update_channel(&id);
    let target = resolve_market_target(&id, &ceiling)?;
    let current = crate::core::plugin::PluginManager::shared()
        .list()
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| p.version)
        .unwrap_or_default();
    let path = crate::core::marketplace::download_target(&id, &target)?;
    let preview = crate::core::plugin::PluginManager::preview_ocplugin(&path)?;
    let old_key = crate::core::plugin::PluginManager::installed_publisher_key(&id);
    let new_key = preview.signature.key_id.clone();
    let publisher_change = if old_key != new_key {
        serde_json::json!({ "from": old_key, "to": new_key })
    } else {
        serde_json::Value::Null
    };
    Ok(serde_json::json!({
        "preview": preview,
        "archivePath": path.display().to_string(),
        "currentVersion": current,
        "publisherChange": publisher_change,
    }))
}

/// F7 — read the "allow unsigned packages" master switch (default ON).
#[tauri::command]
pub(crate) fn get_allow_unsigned() -> bool {
    crate::core::plugin::allow_unsigned()
}

/// F7 — write the "allow unsigned packages" master switch (settings page).
#[tauri::command]
pub(crate) fn set_allow_unsigned(on: bool) -> Result<(), String> {
    crate::core::plugin::set_allow_unsigned(on)
}

/// S5b — sandbox enforcement switch (read).
#[tauri::command]
pub(crate) fn get_sandbox_enforcement() -> bool {
    crate::core::plugin::sandbox_enforcement_enabled()
}

/// S5b — sandbox enforcement switch (write).
#[tauri::command]
pub(crate) fn set_sandbox_enforcement(on: bool) -> Result<(), String> {
    crate::core::plugin::set_sandbox_enforcement(on)
}

/// F6 — explicit reopen: clears the revocation default-disable (audited), allowing another start.
#[tauri::command]
pub(crate) fn reopen_plugin(id: String) -> Result<(), String> {
    crate::core::revocation::reopen(&id)
}

/// Wow 8 — ratings: both marketplace list items and installed plugins let users give 1-5 stars + a text comment.
/// Data is stored in the sqlite plugin_ratings table (id, plugin_id, score, comment, ts).
/// Rate even without installing (rate an uninstalled marketplace item first; it carries over when installed).
#[tauri::command]
pub(crate) fn rate_plugin(
    id: String,
    score: i64,
    comment: Option<String>,
) -> Result<crate::core::storage::PluginRating, String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("storage not available".into());
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut s = store.lock().map_err(|e| format!("store lock: {}", e))?;
    s.add_rating(&id, score, comment.as_deref(), ts)
}

#[tauri::command]
pub(crate) fn list_plugin_ratings(
    id: String,
    limit: usize,
) -> Vec<crate::core::storage::PluginRating> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    store
        .lock()
        .ok()
        .map(|s| s.list_ratings(&id, limit.max(1).min(500)))
        .unwrap_or_default()
}

#[tauri::command]
pub(crate) fn plugin_rating_summary(id: String) -> crate::core::storage::PluginRatingSummary {
    let Some(store) = crate::core::shared_store() else {
        return crate::core::storage::PluginRatingSummary {
            plugin_id: id,
            count: 0,
            avg: 0.0,
        };
    };
    store
        .lock()
        .map(|s| s.rating_summary(&id))
        .unwrap_or_else(|_| crate::core::storage::PluginRatingSummary {
            plugin_id: id,
            count: 0,
            avg: 0.0,
        })
}

/// Phase 30 — recent N call stats per (capability, plugin_id) pair (count + avg + p50 + p95).
/// Aggregation takes the latest N in SQLite via the ROW_NUMBER window function, then sorts in memory to compute percentiles.
#[tauri::command]
pub(crate) fn list_capability_stats(samples: usize) -> Vec<crate::core::storage::CapabilityStat> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    store
        .lock()
        .map(|s| s.capability_stats_summary(samples))
        .unwrap_or_default()
}

/// Phase 36 — read SLA monitoring config (returns default if never saved).
#[tauri::command]
pub(crate) fn get_sla_config() -> crate::core::sla::SlaConfig {
    match crate::core::shared_store() {
        Some(s) => crate::core::sla::load_config(&s),
        None => crate::core::sla::SlaConfig::default(),
    }
}

/// Phase 36 — write SLA monitoring config (validated then stored in SQLite; the background thread picks up the new value on its next poll).
#[tauri::command]
pub(crate) fn set_sla_config(cfg: crate::core::sla::SlaConfig) -> Result<(), String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("store not initialized".into());
    };
    crate::core::sla::save_config(&store, &cfg)
}

/// Phase 36 — list recent `capability.sla.violated` events (for the UI to show alert history).
#[tauri::command]
pub(crate) fn list_sla_violations(limit: usize) -> Vec<crate::core::sla::SlaViolation> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    crate::core::sla::list_violations(&store, limit.max(1).min(1000))
}

/// Phase 37 — manually run a probe once (retry for a plugin that already `probe_failed`).
/// Returns the full report; if it passes and the plugin is not running, starts it automatically.
#[tauri::command]
pub(crate) fn run_plugin_probe(id: String) -> Result<crate::core::probe::ProbeReport, String> {
    let mgr = crate::core::plugin::PluginManager::shared();
    // take the declared capability list from the manifest
    let caps = mgr
        .list()
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| p.capabilities)
        .unwrap_or_default();
    let report = crate::core::probe::run_and_publish(&id, &caps);
    // passed + not currently running → start it along the way so the plugin enters the normal lifecycle
    if report.status == "passed" {
        let running = mgr
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.status == "running" || p.status == "starting")
            .unwrap_or(false);
        if !running {
            if let Err(e) = mgr.start(&id) {
                eprintln!("[probe] auto-start after passed probe failed: {}", e);
            }
        }
    } else if report.status == "failed" {
        crate::core::plugin::PluginManager::set_status(&id, "probe_failed");
    }
    Ok(report)
}

/// Phase 37 — get the most recent probe report (JSON-serialized). Returns None if never run.
#[tauri::command]
pub(crate) fn get_probe_report(id: String) -> Option<crate::core::probe::ProbeReport> {
    let store = crate::core::shared_store()?;
    let s = store.lock().ok()?;
    let (_status, _at, json) = s.get_probe_report(&id)?;
    serde_json::from_str(&json).ok()
}

/// Phase 38 — set a plugin's update channel (validated then written to SQLite + emit plugin.channel_changed).
#[tauri::command]
pub(crate) fn set_plugin_channel(id: String, channel: String) -> Result<(), String> {
    let store = crate::core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let normalized = crate::core::marketplace::normalize_channel(&channel);
    {
        let mut s = store.lock().map_err(|e| format!("store lock: {}", e))?;
        s.set_plugin_channel(&id, &normalized);
    }
    crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
        "plugin.channel_changed",
        "core",
        serde_json::json!({ "pluginId": id, "channel": normalized }),
    ));
    Ok(())
}

/// Phase 38 — global default update channel (used by plugins without their own setting).
#[tauri::command]
pub(crate) fn get_default_channel() -> String {
    let store = crate::core::shared_store();
    if let Some(s) = store.as_ref() {
        if let Ok(g) = s.lock() {
            if let Some(c) = g.get_setting("defaultChannel") {
                return crate::core::marketplace::normalize_channel(&c);
            }
        }
    }
    "stable".into()
}

#[tauri::command]
pub(crate) fn set_default_channel(channel: String) -> Result<(), String> {
    let store = crate::core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let normalized = crate::core::marketplace::normalize_channel(&channel);
    {
        let mut s = store.lock().map_err(|e| format!("store lock: {}", e))?;
        s.set_setting("defaultChannel", &normalized);
    }
    Ok(())
}

/// Phase 41 — create a workspace backup (snapshots all relevant tables + plugin configs to ~/.opencapx/backups/).
#[tauri::command]
pub(crate) fn create_backup() -> Result<crate::core::backup::BackupMeta, String> {
    let store = crate::core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let g = store.lock().map_err(|e| format!("store lock: {}", e))?;
    match &*g {
        storage::StoreEnum::Db(x) => {
            crate::core::backup::create_backup(x, &crate::core::config::config_dir())
        }
        storage::StoreEnum::Mem(_) => Err("backups require persistent storage".into()),
    }
}

/// Phase 41 — list existing backup files (by createdAt descending).
#[tauri::command]
pub(crate) fn list_backups() -> Vec<crate::core::backup::BackupMeta> {
    crate::core::backup::list_backups()
}

/// Phase 41 — delete a backup file.
#[tauri::command]
pub(crate) fn delete_backup(filename: String) -> Result<(), String> {
    crate::core::backup::delete_backup(&filename)
}

/// Phase 41 — restore all workspace state from a backup file.
#[tauri::command]
pub(crate) fn restore_backup(
    filename: String,
) -> Result<crate::core::backup::RestoreReport, String> {
    let store = crate::core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let mut g = store.lock().map_err(|e| format!("store lock: {}", e))?;
    let report = match &mut *g {
        storage::StoreEnum::Db(x) => {
            crate::core::backup::restore_backup(x, &crate::core::config::config_dir(), &filename)
        }
        storage::StoreEnum::Mem(_) => Err("backups require persistent storage".into()),
    }?;
    // restore may rewrite hotkeys.json (Task 6) → re-register everything at the OS layer.
    let _ = crate::core::hotkey::unregister_all_at_os();
    for dto in crate::core::hotkey::store().list() {
        if dto.enabled {
            if let Err(e) = crate::core::hotkey::register_at_os(&dto.combo) {
                eprintln!(
                    "hotkey: re-register {} after restore failed: {}",
                    dto.combo, e
                );
            }
        }
    }
    Ok(report)
}

/// Phase 40 — read a plugin's health config (returns default if never saved).
#[tauri::command]
pub(crate) fn get_plugin_health_config(id: String) -> crate::core::health::PluginHealthConfig {
    match crate::core::shared_store() {
        Some(s) => {
            let g = s.lock().ok();
            g.map(|st| st.get_health_config(&id)).unwrap_or_default()
        }
        None => crate::core::health::PluginHealthConfig::default(),
    }
}

/// Phase 40 — write a plugin's health config (validate then upsert).
/// Writing triggers a "restart watchdog": the current watchdog is removed from PluginManager,
/// and register_watchdog spawns a new one with the new cfg.
#[tauri::command]
pub(crate) fn set_plugin_health_config(
    id: String,
    cfg: crate::core::health::PluginHealthConfig,
) -> Result<(), String> {
    crate::core::health::validate(&cfg)?;
    let store = crate::core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let mut s = store.lock().map_err(|e| format!("store lock: {}", e))?;
    s.upsert_health_config(&id, &cfg);
    drop(s);
    crate::core::plugin::PluginManager::shared().apply_health_config(&id, &cfg);
    Ok(())
}

/// Full hotkey list (with enabled + OS registration state).
#[tauri::command]
pub(crate) fn list_hotkeys() -> Vec<crate::core::hotkey::HotkeyDto> {
    crate::core::hotkey::store().list()
}

/// Write a binding (same combo overwrites). An OS registration failure does not reject the save; it is returned via HotkeyResult.
#[tauri::command]
pub(crate) fn set_hotkey(
    combo: String,
    action: crate::core::hotkey::HotkeyAction,
) -> Result<crate::core::hotkey::HotkeyResult, String> {
    crate::core::hotkey::store().set(&combo, &action)
}

/// Enable/disable a binding (disable unregisters first).
#[tauri::command]
pub(crate) fn set_hotkey_enabled(
    combo: String,
    enabled: bool,
) -> Result<crate::core::hotkey::HotkeyResult, String> {
    crate::core::hotkey::store().set_enabled(&combo, enabled)
}

/// Delete by combo (delete the row on disk first, then unregister at the OS layer).
#[tauri::command]
pub(crate) fn delete_hotkey(combo: String) -> bool {
    crate::core::hotkey::store().delete(&combo)
}

/// Command palette candidates (builtin + each running plugin capability).
#[tauri::command]
pub(crate) fn list_palette_entries() -> Vec<crate::core::hotkey::PaletteEntry> {
    crate::core::hotkey::palette_entries()
}
