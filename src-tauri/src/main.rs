#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod admin;
mod cli_install;
mod core;
mod detector;
mod hooks;
mod http;
mod mcp;
mod notify;
mod queue;

use clap::Parser;
use core::agent::{session_to_dto, SessionDto, SessionSink};
use core::storage::{self, SharedStore};
use std::path::PathBuf;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Crash log: the global panic hook writes records to `crash.log` (retention caps the size),
/// then hands back to the default hook (print + RUST_BACKTRACE expansion). The hook only appends,
/// and never touches EventBus — in a panic-in-panic scenario any complex dependency is unreliable.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>").to_string();
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = info
            .payload_as_str()
            .unwrap_or("<non-string panic payload>");
        let record = format!(
            "[{}] thread '{}' panicked at {}: {}\n",
            now_secs(),
            thread_name,
            location,
            payload
        );
        core::retention::append_crash(&core::retention::crash_log_path(), &record);
        default_hook(info);
    }));
}

#[tauri::command]
fn get_sessions(store: tauri::State<SharedStore>) -> Vec<SessionDto> {
    let now = now_secs();
    match store.lock() {
        Ok(s) => {
            let mut rows = s.active(now);
            core::agent::sort_sessions(&mut rows);
            rows.iter().map(session_to_dto).collect()
        }
        Err(_) => Vec::new(),
    }
}

/// Project metadata for the bubble group header (git branch / short path). Batched and deduped;
/// disk reads and caching live in core::project; this is just the entry point.
#[tauri::command]
fn project_meta(cwds: Vec<String>) -> Vec<core::project::ProjectMeta> {
    let now = now_secs();
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for cwd in cwds {
        if cwd.is_empty() || seen.contains(&cwd) {
            continue;
        }
        seen.push(cwd.clone());
        out.push(core::project::meta(&cwd, now));
    }
    out
}

#[tauri::command]
fn dismiss_session(store: tauri::State<SharedStore>, id: String) {
    if let Ok(mut s) = store.lock() {
        s.dismiss(&id);
    }
}

#[tauri::command]
fn clear_sessions(store: tauri::State<SharedStore>) {
    if let Ok(mut s) = store.lock() {
        s.clear();
    }
}

#[tauri::command]
fn list_pet_packs() -> Vec<core::petpack::PetPackInfo> {
    core::petpack::list()
}

/// Import a pet pack: directory, single image, or 3D model (.glb/.gltf).
#[tauri::command]
fn import_pet_pack(path: String) -> Result<String, String> {
    let p = std::path::PathBuf::from(&path);
    if p.is_dir() {
        return core::petpack::install_from_dir(&p);
    }
    if !p.is_file() {
        return Err(format!("{path} does not exist"));
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".glb") || lower.ends_with(".gltf") {
        core::petpack::install_from_model(&p, None)
    } else {
        core::petpack::install_from_image(&p, None)
    }
}

/// Read the 3D model's raw bytes. Uses `ipc::Response` to pass raw bytes, not JSON —
/// the model is binary, and serializing it into a numeric array would inflate it several times over.
#[tauri::command]
fn read_pet_model(id: String) -> Result<tauri::ipc::Response, String> {
    core::petpack::model_bytes(&id).map(tauri::ipc::Response::new)
}

/// Read auxiliary assets referenced by `.gltf` (.bin / textures) so the frontend can assemble a self-contained model.
#[tauri::command]
fn read_pet_asset(id: String, rel: String) -> Result<tauri::ipc::Response, String> {
    core::petpack::model_asset(&id, &rel).map(tauri::ipc::Response::new)
}

/// Download an image from a direct URL and install it as a pet pack (download on the Rust side: bypasses CORS + can validate content-type).
#[tauri::command]
fn import_pet_pack_from_url(url: String, name: Option<String>) -> Result<String, String> {
    core::petpack::install_from_url(&url, name.as_deref())
}

#[tauri::command]
fn delete_pet_pack(id: String) -> Result<(), String> {
    core::petpack::delete(&id)
}

/// Read the sprite sheet as a data URL for WebView rendering (avoids extra asset-protocol scope config).
#[tauri::command]
fn read_pet_sheet(id: String) -> Result<String, String> {
    core::petpack::sheet_data_url(&id)
}

#[tauri::command]
fn answer_ask(id: String, answer: String) -> bool {
    core::rpc::resolve_ask(&id, &answer)
}

#[tauri::command]
fn answer_permission(id: String, answer: String) -> bool {
    core::permission::resolve_ask(&id, &answer)
}

#[tauri::command]
fn answer_install(id: String, answer: String) -> bool {
    core::permission::resolve_ask(&id, &answer)
}

#[tauri::command]
fn list_plugins() -> Vec<core::plugin::PluginStatusDto> {
    core::plugin::PluginManager::shared().list()
}

/// **dev/test only; the UI exposes only .ocplugin and the marketplace**: install from an unpacked directory,
/// for test fixtures and local development.
#[tauri::command]
async fn install_plugin(dir: String) -> Result<String, String> {
    // same as install_ocplugin: the install phase blocks on permission answers, so it cannot occupy the main thread
    tauri::async_runtime::spawn_blocking(move || {
        core::plugin::PluginManager::shared().install_from_dir(&std::path::PathBuf::from(dir))
    })
    .await
    .map_err(|e| format!("install task failed: {}", e))?
}

/// Install .ocplugin. **Must be async**: install asks the user for each permission during the commit phase
/// (`opencapx-install-ask`, each blocking up to 60s), and Tauri sync commands run on the main thread
/// — while frozen the window cannot even draw the ask prompt, so the user just sees "clicked install, page stuck".
/// Async commands run on the thread pool, the main thread keeps drawing the UI, and the ask becomes visible.
#[tauri::command]
async fn install_ocplugin(
    path: String,
    confirm_unsigned: Option<bool>,
    confirm_key_change: Option<bool>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        core::plugin::PluginManager::shared().install_ocplugin_ex(
            &std::path::PathBuf::from(path),
            install_opts(confirm_unsigned, confirm_key_change),
        )
    })
    .await
    .map_err(|e| format!("install task failed: {}", e))?
}

/// F7 — assemble install confirmation options (command args → core options).
fn install_opts(
    confirm_unsigned: Option<bool>,
    confirm_key_change: Option<bool>,
) -> core::plugin::InstallOptions {
    core::plugin::InstallOptions {
        confirm_unsigned: confirm_unsigned.unwrap_or(false),
        confirm_key_change: confirm_key_change.unwrap_or(false),
    }
}

/// Wow 5 — for the settings.ts dialog preview. Returns manifest fields + permissions
/// (with the high_risk flag), without actually unpacking/installing.
#[tauri::command]
fn preview_ocplugin(path: String) -> Result<core::plugin::PluginPreviewDto, String> {
    core::plugin::PluginManager::preview_ocplugin(&std::path::PathBuf::from(path))
}

#[tauri::command]
fn list_automation_rules() -> Vec<serde_json::Value> {
    core::automation::load_rules()
}

#[tauri::command]
fn add_automation_rule(
    when: serde_json::Value,
    then: serde_json::Value,
) -> Result<serde_json::Value, String> {
    core::automation::add_rule(&when, &then)
}

#[tauri::command]
fn remove_automation_rule(id: String) -> Result<bool, String> {
    core::automation::remove_rule(&id)
}

#[tauri::command]
fn set_automation_rule_enabled(id: String, enabled: bool) -> Result<serde_json::Value, String> {
    core::automation::set_rule_enabled(&id, enabled)
}

#[tauri::command]
fn list_rules() -> Vec<core::rules::RuleSummary> {
    core::rules::list_summaries()
}

#[tauri::command]
fn set_rule_enabled(id: String, enabled: bool) -> Result<(), String> {
    core::rules::set_enabled_in_global(&id, enabled)
}

#[tauri::command]
fn add_rule(rule: serde_json::Value) -> Result<(), String> {
    core::rules::add_rule_to_global(rule)
}

#[tauri::command]
fn list_trusted_projects() -> Vec<String> {
    core::rules::trusted_projects()
}

#[tauri::command]
fn trust_project(path: String) -> Result<(), String> {
    core::rules::trust(std::path::Path::new(&path))
}

#[tauri::command]
fn untrust_project(path: String) -> Result<(), String> {
    core::rules::untrust(std::path::Path::new(&path))
}

#[tauri::command]
fn list_marketplace(refresh: bool) -> Vec<core::marketplace::PluginMarketEntry> {
    if refresh && core::marketplace::refresh().is_err() {
        // on fetch failure fall back to the local index so the UI is not blank
    }
    core::marketplace::load_index().entries
}

#[derive(serde::Serialize)]
struct AuditEntry {
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
struct AuditSearchFilter {
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
struct LogEntry {
    id: String,
    kind: String,
    plugin_id: String,
    level: String,
    source: String,
    message: String,
    timestamp: u64,
}

#[tauri::command]
fn list_audit(limit: usize) -> Vec<AuditEntry> {
    let Some(store) = core::shared_store() else {
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
fn search_audit_events(filter: AuditSearchFilter) -> Vec<AuditEntry> {
    let Some(store) = core::shared_store() else {
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
struct TimelineEntryDto {
    id: String,
    timestamp: u64,
    kind: String,
    /// agent | capability | permission | security | plugin | pet | system
    category: &'static str,
    /// extracted from payload.agentId; capability events carry agent since v1.1
    agent: String,
    source: String,
    payload: serde_json::Value,
}

/// Kinds included in Timeline: action events with audit value. Noise is excluded:
/// capability.started (completed/failed is enough), plugin.log, plugin.metrics.*,
/// plugin.lifecycle.starting/running (high frequency), pet.state (aggregated heartbeat).
fn timeline_kind_allowed(kind: &str) -> bool {
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

fn timeline_category(kind: &str) -> &'static str {
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
fn timeline_events(limit: Option<usize>, agent: Option<String>) -> Vec<TimelineEntryDto> {
    let Some(store) = core::shared_store() else {
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
struct NotificationDto {
    id: String,
    timestamp: u64,
    /// payload.agentId (empty = non-Agent context)
    agent: String,
    title: String,
    body: String,
    /// info | warn | error
    severity: String,
}

#[tauri::command]
fn list_notifications(limit: Option<usize>, agent: Option<String>) -> Vec<NotificationDto> {
    let Some(store) = core::shared_store() else {
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

/// Update channel definition: per-plugin row ∪ `"stable"` (consistent with check_updates / the settings dropdown).
fn plugin_update_channel(id: &str) -> String {
    core::shared_store()
        .as_ref()
        .and_then(|s| s.lock().ok().map(|g| g.list_plugin_channels()))
        .and_then(|chs| chs.into_iter().find(|(pid, _)| pid == id).map(|(_, c)| c))
        .unwrap_or_else(|| "stable".to_string())
}

fn resolve_market_target(
    id: &str,
    ceiling: &str,
) -> Result<core::marketplace::PluginMarketVersion, String> {
    // M3: if registry has it → registry is authoritative (version selection includes publisher/revocation filtering);
    // not listed or registry unavailable → marketplace (legacy channel).
    if let Some(idx) = core::registry::load_offline() {
        if idx.entries.iter().any(|e| &e.id == id) {
            return core::registry::select_version(&idx, id, env!("CARGO_PKG_VERSION"), ceiling)
                .map(core::registry::to_market_version)
                .ok_or_else(|| format!("no core-compatible version available for {}", id));
        }
    }
    let entry = core::marketplace::find(id).ok_or_else(|| format!("not in marketplace: {}", id))?;
    core::marketplace::resolve_target_with_ceiling(&entry, env!("CARGO_PKG_VERSION"), ceiling)
}

#[tauri::command]
fn install_marketplace(
    id: String,
    confirm_unsigned: Option<bool>,
    confirm_key_change: Option<bool>,
) -> Result<String, String> {
    // explicit install: no channel subscription gate (channels only govern update supply)
    let target = resolve_market_target(&id, "dev")?;
    let path = core::marketplace::download_target(&id, &target)?;
    core::plugin::PluginManager::shared()
        .install_ocplugin_ex(&path, install_opts(confirm_unsigned, confirm_key_change))
}

#[tauri::command]
fn check_plugin_updates() -> Vec<core::marketplace::PluginUpdateInfo> {
    let installed: Vec<(String, String)> = core::plugin::PluginManager::shared()
        .list()
        .into_iter()
        .map(|p| (p.id, p.version))
        .collect();
    // Phase 38 — fetch all (plugin_id, channel) so marketplace can filter: dev users see beta+stable
    // both are offered; stable users see only stable. Empty → all plugins default to stable.
    let channels = core::shared_store()
        .as_ref()
        .and_then(|s| s.lock().ok().map(|g| g.list_plugin_channels()))
        .unwrap_or_default();
    // M3: registry (verified index) is the **authoritative source** for listed plugins: update supply uses registry version selection
    // (including publisher registration/revocation/channel/core-compat filtering); unlisted plugins continue via marketplace.
    let mut out: Vec<core::marketplace::PluginUpdateInfo> = Vec::new();
    let mut registry_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(idx) = core::registry::load_offline() {
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
                core::registry::select_version(&idx, id, env!("CARGO_PKG_VERSION"), ceiling)
            {
                if core::marketplace::version_is_newer(&v.version, current) {
                    out.push(core::marketplace::PluginUpdateInfo {
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
    for u in core::marketplace::check_updates(&installed, &channels) {
        if !registry_ids.contains(&u.id) {
            out.push(u);
        }
    }
    out
}

/// M3 — registry v2 status: source (cache/fetch/seed/none), size, and generatedAt.
#[tauri::command]
fn registry_status() -> core::registry::RegistryStatusDto {
    core::registry::status()
}

/// M3 — force-refresh registry (ignores TTL; anti-replay and signature verification still apply); rescan revocations after refresh.
#[tauri::command]
fn registry_refresh() -> Result<core::registry::RegistryStatusDto, String> {
    let loaded = core::registry::refresh()?;
    core::revocation::sweep_with(&loaded.index);
    Ok(core::registry::status_of(&loaded))
}

/// F7 — update preview (unified rendering): after download + verification, returns a full preview (with diff / key change info),
/// `archivePath` lets the user install directly after confirming (no second download).
#[tauri::command]
fn preview_update(id: String) -> Result<serde_json::Value, String> {
    let ceiling = plugin_update_channel(&id);
    let target = resolve_market_target(&id, &ceiling)?;
    let current = core::plugin::PluginManager::shared()
        .list()
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| p.version)
        .unwrap_or_default();
    let path = core::marketplace::download_target(&id, &target)?;
    let preview = core::plugin::PluginManager::preview_ocplugin(&path)?;
    let old_key = core::plugin::PluginManager::installed_publisher_key(&id);
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
fn get_allow_unsigned() -> bool {
    core::plugin::allow_unsigned()
}

/// F7 — write the "allow unsigned packages" master switch (settings page).
#[tauri::command]
fn set_allow_unsigned(on: bool) -> Result<(), String> {
    core::plugin::set_allow_unsigned(on)
}

/// S5b — sandbox enforcement switch (read).
#[tauri::command]
fn get_sandbox_enforcement() -> bool {
    core::plugin::sandbox_enforcement_enabled()
}

/// S5b — sandbox enforcement switch (write).
#[tauri::command]
fn set_sandbox_enforcement(on: bool) -> Result<(), String> {
    core::plugin::set_sandbox_enforcement(on)
}

/// F6 — explicit reopen: clears the revocation default-disable (audited), allowing another start.
#[tauri::command]
fn reopen_plugin(id: String) -> Result<(), String> {
    core::revocation::reopen(&id)
}

/// Wow 8 — ratings: both marketplace list items and installed plugins let users give 1-5 stars + a text comment.
/// Data is stored in the sqlite plugin_ratings table (id, plugin_id, score, comment, ts).
/// Rate even without installing (rate an uninstalled marketplace item first; it carries over when installed).
#[tauri::command]
fn rate_plugin(
    id: String,
    score: i64,
    comment: Option<String>,
) -> Result<core::storage::PluginRating, String> {
    let Some(store) = core::shared_store() else {
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
fn list_plugin_ratings(id: String, limit: usize) -> Vec<core::storage::PluginRating> {
    let Some(store) = core::shared_store() else {
        return Vec::new();
    };
    store
        .lock()
        .ok()
        .map(|s| s.list_ratings(&id, limit.max(1).min(500)))
        .unwrap_or_default()
}

#[tauri::command]
fn plugin_rating_summary(id: String) -> core::storage::PluginRatingSummary {
    let Some(store) = core::shared_store() else {
        return core::storage::PluginRatingSummary {
            plugin_id: id,
            count: 0,
            avg: 0.0,
        };
    };
    store
        .lock()
        .map(|s| s.rating_summary(&id))
        .unwrap_or_else(|_| core::storage::PluginRatingSummary {
            plugin_id: id,
            count: 0,
            avg: 0.0,
        })
}

/// Phase 30 — recent N call stats per (capability, plugin_id) pair (count + avg + p50 + p95).
/// Aggregation takes the latest N in SQLite via the ROW_NUMBER window function, then sorts in memory to compute percentiles.
#[tauri::command]
fn list_capability_stats(samples: usize) -> Vec<core::storage::CapabilityStat> {
    let Some(store) = core::shared_store() else {
        return Vec::new();
    };
    store
        .lock()
        .map(|s| s.capability_stats_summary(samples))
        .unwrap_or_default()
}

/// Phase 36 — read SLA monitoring config (returns default if never saved).
#[tauri::command]
fn get_sla_config() -> core::sla::SlaConfig {
    match core::shared_store() {
        Some(s) => core::sla::load_config(&s),
        None => core::sla::SlaConfig::default(),
    }
}

/// Phase 36 — write SLA monitoring config (validated then stored in SQLite; the background thread picks up the new value on its next poll).
#[tauri::command]
fn set_sla_config(cfg: core::sla::SlaConfig) -> Result<(), String> {
    let Some(store) = core::shared_store() else {
        return Err("store not initialized".into());
    };
    core::sla::save_config(&store, &cfg)
}

/// Phase 36 — list recent `capability.sla.violated` events (for the UI to show alert history).
#[tauri::command]
fn list_sla_violations(limit: usize) -> Vec<core::sla::SlaViolation> {
    let Some(store) = core::shared_store() else {
        return Vec::new();
    };
    core::sla::list_violations(&store, limit.max(1).min(1000))
}

/// Phase 37 — manually run a probe once (retry for a plugin that already `probe_failed`).
/// Returns the full report; if it passes and the plugin is not running, starts it automatically.
#[tauri::command]
fn run_plugin_probe(id: String) -> Result<core::probe::ProbeReport, String> {
    let mgr = core::plugin::PluginManager::shared();
    // take the declared capability list from the manifest
    let caps = mgr
        .list()
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| p.capabilities)
        .unwrap_or_default();
    let report = core::probe::run_and_publish(&id, &caps);
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
        core::plugin::PluginManager::set_status(&id, "probe_failed");
    }
    Ok(report)
}

/// Phase 37 — get the most recent probe report (JSON-serialized). Returns None if never run.
#[tauri::command]
fn get_probe_report(id: String) -> Option<core::probe::ProbeReport> {
    let store = core::shared_store()?;
    let s = store.lock().ok()?;
    let (_status, _at, json) = s.get_probe_report(&id)?;
    serde_json::from_str(&json).ok()
}

/// Phase 38 — set a plugin's update channel (validated then written to SQLite + emit plugin.channel_changed).
#[tauri::command]
fn set_plugin_channel(id: String, channel: String) -> Result<(), String> {
    let store = core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let normalized = core::marketplace::normalize_channel(&channel);
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
fn get_default_channel() -> String {
    let store = core::shared_store();
    if let Some(s) = store.as_ref() {
        if let Ok(g) = s.lock() {
            if let Some(c) = g.get_setting("defaultChannel") {
                return core::marketplace::normalize_channel(&c);
            }
        }
    }
    "stable".into()
}

#[tauri::command]
fn set_default_channel(channel: String) -> Result<(), String> {
    let store = core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let normalized = core::marketplace::normalize_channel(&channel);
    {
        let mut s = store.lock().map_err(|e| format!("store lock: {}", e))?;
        s.set_setting("defaultChannel", &normalized);
    }
    Ok(())
}

/// Phase 41 — create a workspace backup (snapshots all relevant tables + plugin configs to ~/.opencapx/backups/).
#[tauri::command]
fn create_backup() -> Result<core::backup::BackupMeta, String> {
    let store = core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let g = store.lock().map_err(|e| format!("store lock: {}", e))?;
    match &*g {
        storage::StoreEnum::Db(x) => core::backup::create_backup(x, &core::config::config_dir()),
        storage::StoreEnum::Mem(_) => Err("backups require persistent storage".into()),
    }
}

/// Phase 41 — list existing backup files (by createdAt descending).
#[tauri::command]
fn list_backups() -> Vec<core::backup::BackupMeta> {
    core::backup::list_backups()
}

/// Phase 41 — delete a backup file.
#[tauri::command]
fn delete_backup(filename: String) -> Result<(), String> {
    core::backup::delete_backup(&filename)
}

/// Phase 41 — restore all workspace state from a backup file.
#[tauri::command]
fn restore_backup(filename: String) -> Result<core::backup::RestoreReport, String> {
    let store = core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let mut g = store.lock().map_err(|e| format!("store lock: {}", e))?;
    let report = match &mut *g {
        storage::StoreEnum::Db(x) => {
            core::backup::restore_backup(x, &core::config::config_dir(), &filename)
        }
        storage::StoreEnum::Mem(_) => Err("backups require persistent storage".into()),
    }?;
    // restore may rewrite hotkeys.json (Task 6) → re-register everything at the OS layer.
    let _ = core::hotkey::unregister_all_at_os();
    for dto in core::hotkey::store().list() {
        if dto.enabled {
            if let Err(e) = core::hotkey::register_at_os(&dto.combo) {
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
fn get_plugin_health_config(id: String) -> core::health::PluginHealthConfig {
    match core::shared_store() {
        Some(s) => {
            let g = s.lock().ok();
            g.map(|st| st.get_health_config(&id)).unwrap_or_default()
        }
        None => core::health::PluginHealthConfig::default(),
    }
}

/// Phase 40 — write a plugin's health config (validate then upsert).
/// Writing triggers a "restart watchdog": the current watchdog is removed from PluginManager,
/// and register_watchdog spawns a new one with the new cfg.
#[tauri::command]
fn set_plugin_health_config(
    id: String,
    cfg: core::health::PluginHealthConfig,
) -> Result<(), String> {
    core::health::validate(&cfg)?;
    let store = core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let mut s = store.lock().map_err(|e| format!("store lock: {}", e))?;
    s.upsert_health_config(&id, &cfg);
    drop(s);
    core::plugin::PluginManager::shared().apply_health_config(&id, &cfg);
    Ok(())
}

/// Full hotkey list (with enabled + OS registration state).
#[tauri::command]
fn list_hotkeys() -> Vec<core::hotkey::HotkeyDto> {
    core::hotkey::store().list()
}

/// Write a binding (same combo overwrites). An OS registration failure does not reject the save; it is returned via HotkeyResult.
#[tauri::command]
fn set_hotkey(
    combo: String,
    action: core::hotkey::HotkeyAction,
) -> Result<core::hotkey::HotkeyResult, String> {
    core::hotkey::store().set(&combo, &action)
}

/// Enable/disable a binding (disable unregisters first).
#[tauri::command]
fn set_hotkey_enabled(combo: String, enabled: bool) -> Result<core::hotkey::HotkeyResult, String> {
    core::hotkey::store().set_enabled(&combo, enabled)
}

/// Delete by combo (delete the row on disk first, then unregister at the OS layer).
#[tauri::command]
fn delete_hotkey(combo: String) -> bool {
    core::hotkey::store().delete(&combo)
}

/// Command palette candidates (builtin + each running plugin capability).
#[tauri::command]
fn list_palette_entries() -> Vec<core::hotkey::PaletteEntry> {
    core::hotkey::palette_entries()
}

/// Phase 39 — execute the corresponding action when an OS shortcut fires (called by Builder.with_handler).
fn dispatch_hotkey_action(app: &tauri::AppHandle, action: core::hotkey::HotkeyAction) {
    use core::hotkey::{BuiltinAction, HotkeyAction};
    use tauri::{Emitter, Manager};
    match action {
        HotkeyAction::Builtin { action } => match action {
            BuiltinAction::TogglePet => {
                if let Some(w) = app.get_webview_window("pet") {
                    if w.is_visible().unwrap_or(true) {
                        let _ = w.hide();
                        set_pet_visible(app, false);
                    } else {
                        let _ = w.show();
                        set_pet_visible(app, true);
                    }
                }
            }
            BuiltinAction::OpenSettings => {
                show_settings(app);
            }
            BuiltinAction::OpenPalette => {
                // the command palette currently reuses the settings window — after show+focus a frontend event triggers the palette overlay.
                show_settings(app);
                let _ = app.emit_to("settings", "hotkey::open_palette", serde_json::json!({}));
            }
            BuiltinAction::Quit => app.exit(0),
        },
        HotkeyAction::Plugin {
            plugin_id,
            capability,
        } => {
            // via PluginManager RPC: ensure_running obtains a ProcessHandle, then call(method, {}, 5s).
            let mgr = core::plugin::PluginManager::shared();
            let plugin_id_clone = plugin_id.clone();
            let capability_clone = capability.clone();
            std::thread::spawn(move || match mgr.ensure_running(&plugin_id_clone) {
                Ok(proc) => {
                    let params = serde_json::json!({});
                    let timeout = std::time::Duration::from_secs(5);
                    let _ = proc.call(&capability_clone, params, timeout);
                }
                Err(e) => {
                    eprintln!(
                        "hotkey: failed to ensure_running {}: {}",
                        plugin_id_clone, e
                    );
                }
            });
        }
    }
}

/// Phase 32 — full plugin capability dependency graph (nodes = installed plugins, edges = shared capabilities).
#[tauri::command]
fn list_plugin_dependency_graph() -> core::plugin::CapabilityGraphDto {
    core::plugin::PluginManager::shared().capability_dependency_graph()
}

/// Phase 33 — permission risk heatmap (cells + global denied top-N).
#[tauri::command]
fn list_permission_heatmap() -> core::permission::PermissionHeatmapDto {
    core::permission::heatmap()
}

/// Phase 42 — topological layered start plan (layer[i] is the set of plugins started in parallel at step i) + reversed stop_order.
#[tauri::command]
fn get_plugin_lifecycle_plan() -> core::lifecycle_order::LifecyclePlanDto {
    core::lifecycle_order::compute_lifecycle_plan(&core::plugin::PluginManager::shared())
}

/// Phase 42 — start all installed plugins in plan order. Returns (started_count, errors).
#[derive(serde::Serialize)]
struct StartAllResult {
    started: usize,
    errors: Vec<String>,
}

#[tauri::command]
fn start_all_plugins() -> StartAllResult {
    let mgr = core::plugin::PluginManager::shared();
    let plan = core::lifecycle_order::compute_lifecycle_plan(&mgr);
    let (started, errors) = core::lifecycle_order::start_all_in_order(&mgr, &plan);
    StartAllResult { started, errors }
}

/// Phase 42 — stop all plugins in reverse plan order. Returns the stopped count.
#[tauri::command]
fn stop_all_plugins() -> usize {
    let mgr = core::plugin::PluginManager::shared();
    let plan = core::lifecycle_order::compute_lifecycle_plan(&mgr);
    core::lifecycle_order::stop_all_in_order(&mgr, &plan)
}

/// --safe-mode read-only state (for the settings page banner; leaving safe mode requires an app restart).
#[tauri::command]
fn get_safe_mode_state() -> bool {
    core::safe_mode::is_active()
}

/// Phase 44 — Kill switch global disable toggle.
/// Enabling the kill switch: force stop all running plugins immediately + emit the `plugin.kill_switch.enabled` event.
#[tauri::command]
fn enable_kill_switch(reason: Option<String>) -> core::kill_switch::KillSwitchStateDto {
    let r = reason.unwrap_or_default();
    let mgr = core::plugin::PluginManager::shared();
    let plugins = mgr.list();
    core::kill_switch::enable(&r, "operator");
    for p in &plugins {
        mgr.stop(&p.id);
    }
    core::kill_switch::state()
}

/// Disabling the kill switch: clears state only, does not auto-restart any plugin (the user can manually start what they need).
#[tauri::command]
fn disable_kill_switch() -> core::kill_switch::KillSwitchStateDto {
    core::kill_switch::disable();
    core::kill_switch::state()
}

#[tauri::command]
fn get_kill_switch_state() -> core::kill_switch::KillSwitchStateDto {
    core::kill_switch::state()
}

/// Phase 45 — fetch the latest metrics snapshot for all currently running plugins.
#[tauri::command]
fn get_plugin_metrics() -> Vec<core::plugin_metrics::PluginMetricsSnapshot> {
    core::plugin_metrics::latest_snapshots()
}

/// Phase 45 — fetch the last N historical samples for a plugin (for charts).
#[derive(serde::Deserialize)]
struct MetricsHistoryArgs {
    #[serde(default)]
    plugin_id: String,
    #[serde(default)]
    since_ts: i64,
    #[serde(default = "default_metrics_limit")]
    limit: usize,
}
fn default_metrics_limit() -> usize {
    600
}

#[tauri::command]
fn get_plugin_metrics_history(
    args: MetricsHistoryArgs,
) -> Vec<core::plugin_metrics::PluginMetricsSnapshot> {
    core::plugin_metrics::history(&args.plugin_id, args.since_ts, args.limit)
}

/// Phase 45 — get the monitoring config.
#[tauri::command]
fn get_metrics_config() -> core::plugin_metrics::MetricsConfig {
    core::plugin_metrics::load_config()
}

/// Phase 45 — save the monitoring config.
#[tauri::command]
fn set_metrics_config(cfg: core::plugin_metrics::MetricsConfig) -> Result<(), String> {
    core::plugin_metrics::save_config(&cfg)
}

/// Phase 46 — all profiles (with plugin count).
#[tauri::command]
fn list_workspace_profiles() -> Vec<core::profile::ProfileInfo> {
    core::profile::list_profiles()
}

/// Phase 46 — create a profile.
#[tauri::command]
fn create_workspace_profile(name: String) -> core::profile::ProfileInfo {
    core::profile::create_profile(&name).unwrap_or_else(|e| core::profile::ProfileInfo {
        name,
        is_active: false,
        plugin_count: 0,
        created_at: 0,
    })
}

/// Phase 46 — switch profile (stop all plugins + swap store + emit event).
#[tauri::command]
fn switch_workspace_profile(name: String) -> Result<(), String> {
    core::profile::switch_profile(&name)
}

/// Phase 46 — delete a profile (rejects active / default / only profile).
#[tauri::command]
fn delete_workspace_profile(name: String) -> Result<(), String> {
    core::profile::delete_profile(&name)
}

/// Phase 47 — read alerting config (returns default if never saved, enabled=false).
#[tauri::command]
fn get_alerting_config() -> core::alerting::WebhookConfig {
    core::alerting::load_config()
}

/// Phase 47 — write alerting config (validated then stored in SQLite; the dispatcher picks it up next time).
#[tauri::command]
fn set_alerting_config(cfg: core::alerting::WebhookConfig) -> Result<(), String> {
    core::alerting::save_config(&cfg)
}

/// Phase 47 — immediately send one test payload to the webhook, returns the HTTP status code or Err(reason).
/// bypasses dedup so the user sees the result immediately.
#[tauri::command]
fn test_alerting_webhook() -> Result<u16, String> {
    core::alerting::test_send()
}

/// Phase 48 — list failed deliveries (state=None lists all; "pending"/"exhausted"/"resolved" filter).
#[tauri::command]
fn list_alerting_failed(
    state: Option<String>,
    limit: usize,
) -> Vec<core::storage::FailedDeliveryRow> {
    core::alerting::list_failed_deliveries(state.as_deref(), limit)
}

/// Phase 48 — manually retry one dead letter (resets attempts=0, state=pending, next_retry_ts=now).
#[tauri::command]
fn retry_alerting_failed(id: String) -> Result<(), String> {
    if core::alerting::manual_retry_failed_delivery(&id) {
        Ok(())
    } else {
        Err(format!("not found: {}", id))
    }
}

/// Phase 48 — delete one dead letter.
#[tauri::command]
fn delete_alerting_failed(id: String) -> bool {
    core::alerting::delete_failed_delivery(&id)
}

/// Phase 48 — clear exhausted + resolved rows (keeps pending).
#[tauri::command]
fn clear_alerting_resolved() -> usize {
    core::alerting::clear_resolved_failed_deliveries()
}

/// Phase 48 — read retry config (default).
#[tauri::command]
fn get_alerting_retry_config() -> core::alerting::RetryConfig {
    core::alerting::load_retry_config()
}

/// Phase 48 — write retry config (validated then stored in SQLite).
#[tauri::command]
fn set_alerting_retry_config(cfg: core::alerting::RetryConfig) -> Result<(), String> {
    core::alerting::save_retry_config(&cfg)
}

/// Phase 49 — list all alerting endpoints (including disabled), by created_at ascending.
#[tauri::command]
fn list_alerting_endpoints() -> Vec<core::storage::AlertingEndpointRow> {
    core::alerting::list_endpoints()
}

/// Phase 49 — create/update endpoint (empty id = create, non-empty = overwrite). validate_url + unique name.
#[tauri::command]
fn save_alerting_endpoint(
    ep: core::alerting::WebhookEndpoint,
) -> Result<core::storage::AlertingEndpointRow, String> {
    core::alerting::save_endpoint(ep)
}

/// Phase 49 — delete endpoint, cascading clears its dead letters. Returns (deleted, cleared).
#[tauri::command]
fn delete_alerting_endpoint(id: String) -> Result<(bool, usize), String> {
    core::alerting::delete_endpoint(&id)
}

/// Phase 49 — immediately send one test payload to a single endpoint (signed), returns status or Err.
#[tauri::command]
fn test_alerting_endpoint(id: String) -> Result<u16, String> {
    core::alerting::test_endpoint(&id)
}

/// Phase 58 — dry-run preview: builds an envelope from the frontend-supplied sample JSON and runs render + lint,
/// producing a TemplatePreviewResult (body + content-type + diagnostics). On render failure
/// body="" but diagnostics still contains render_failed (line / column).
#[tauri::command]
fn preview_alerting_template(
    template: String,
    sample: serde_json::Value,
) -> core::alerting::TemplatePreviewResult {
    core::alerting::preview_alerting_template(&template, &sample)
}

/// Phase 59 — list all template presets (5 builtin + user-defined).
#[tauri::command]
fn list_alerting_template_presets() -> Vec<core::alerting::TemplatePreset> {
    core::alerting::list_template_presets()
}

/// Phase 59 — get a single preset by kind (builtin:<slug> or user:<uuid>).
#[tauri::command]
fn get_alerting_template_preset(kind: String) -> Option<core::alerting::TemplatePreset> {
    core::alerting::get_template_preset(&kind)
}

/// Phase 59 — save / update a user-defined preset. Auto-generates a UUID when id is empty.
#[tauri::command]
fn save_alerting_template_preset(
    preset: core::alerting::TemplatePreset,
) -> Result<core::alerting::TemplatePreset, String> {
    core::alerting::save_user_template_preset(&preset)
}

/// Phase 59 — delete only user-defined presets (builtin untouched). Returns true = actually deleted.
#[tauri::command]
fn delete_alerting_template_preset(id: String) -> bool {
    core::alerting::delete_user_template_preset(&id)
}

/// Phase 59 — serialize the given preset list to a YAML / JSON string (serde_yaml also accepts JSON input).
#[tauri::command]
fn export_alerting_presets(presets: Vec<core::alerting::TemplatePreset>) -> Result<String, String> {
    core::alerting::export_presets_to_yaml(&presets)
}

/// Phase 59 — bulk import presets from a YAML / JSON string (forces builtin=false). Returns the import count.
#[tauri::command]
fn import_alerting_presets(yaml: String) -> Result<usize, String> {
    core::alerting::import_presets_from_yaml(&yaml)
}

/// Phase 59 — write a text file (for export to disk). Phase 59 adds no fs plugin, uses std::fs directly.
#[tauri::command]
fn write_text_file(path: String, content: String) -> Result<(), String> {
    core::alerting::write_text_file(&path, &content)
}

/// Phase 59 — read a text file (for import reading).
#[tauri::command]
fn read_text_file(path: String) -> Result<String, String> {
    core::alerting::read_text_file(&path)
}

/// Phase 60 — copy a builtin preset into a new user preset (assign a new UUID + user kind, version=1).
/// name is passed by the caller; changelog is auto-filled with "Forked from builtin 'X' (vN)".
#[tauri::command]
fn fork_alerting_template_preset(
    kind: String,
    name: String,
) -> Result<core::alerting::TemplatePreset, String> {
    core::alerting::fork_builtin_preset(&kind, &name)
}

/// Phase 62 — export the entire alerting state (endpoints + routes + presets + silences + acks) as YAML.
/// Phase 64 — passphrase optional: None → plaintext signed YAML; Some → AES-256-GCM encrypted envelope.
#[tauri::command]
fn export_alerting_bundle(passphrase: Option<String>) -> Result<String, String> {
    core::alerting::export_alerting_bundle(passphrase.as_deref())
}

/// Phase 62 — import an alerting state bundle (YAML or envelope). Upserts section by section, preset builtin forced = false.
/// Returns per-section counts + total; rejects when version exceeds the current schema.
/// Phase 64 — passphrase optional: if the envelope needs one but none was passed → Err("passphrase required").
#[tauri::command]
fn import_alerting_bundle(
    content: String,
    passphrase: Option<String>,
) -> Result<core::alerting::BundleImportSummary, String> {
    core::alerting::import_alerting_bundle(&content, passphrase.as_deref())
}

/// Phase 50 — list all silences (the UI filters active / expired itself).
#[tauri::command]
fn list_alerting_silences() -> Vec<core::storage::SilenceRuleRow> {
    core::alerting::list_silences()
}

/// Phase 50 — upsert silence (empty id = create).
#[tauri::command]
fn save_alerting_silence(
    silence: core::alerting::SilenceRule,
) -> Result<core::storage::SilenceRuleRow, String> {
    core::alerting::save_silence(silence)
}

/// Phase 50 — delete silence; returns true when deleted successfully.
#[tauri::command]
fn delete_alerting_silence(id: String) -> bool {
    core::alerting::delete_silence(&id)
}

/// Phase 50 — list all acks (including expired, the UI filters).
#[tauri::command]
fn list_alerting_acks() -> Vec<core::storage::AckRuleRow> {
    core::alerting::list_acks()
}

/// Phase 50 — add an ack suppression for `kind_pattern`, expiring after window_secs seconds.
#[tauri::command]
fn ack_alerting_kind(
    kind_pattern: String,
    window_secs: u64,
) -> Result<core::storage::AckRuleRow, String> {
    core::alerting::ack_kind(kind_pattern, window_secs)
}

/// Phase 50 — delete the specified ack (for the UI cancel button).
#[tauri::command]
fn delete_alerting_ack(id: String) -> bool {
    core::alerting::delete_ack(&id)
}

// ─── Phase 66: Alert Recipient multi-channel fanout (webhook / log:stderr / log:file / email:smtp) ─

// ─── Phase 67: Alert Recipient persistent CRUD ─────────────────────────────

/// Phase 67 — all recipients (by created_at ASC). For the frontend Recipients tab.
#[tauri::command]
fn list_alerting_recipients() -> Vec<core::alerting::RecipientDef> {
    core::alerting::list_recipients()
}

/// Phase 67 — upsert a recipient. `rec.id` empty = create; `rec.created_at == 0` = fill in now.
/// `kind` must be in the whitelist, `name` non-empty and ≤ 64 chars; name UNIQUE conflict → Err.
#[tauri::command]
fn save_alerting_recipient(
    rec: core::alerting::RecipientDef,
) -> Result<core::alerting::RecipientDef, String> {
    core::alerting::save_recipient(rec)
}

/// Phase 67 — delete a recipient + cascade-clean dangling refs to this id in routes.recipients.
/// Returns (deleted, routes_cleared).
#[tauri::command]
fn delete_alerting_recipient(id: String) -> Result<(bool, usize), String> {
    core::alerting::delete_recipient(&id)
}

/// Phase 67 — send a synthetic alert to this recipient to verify the sink works.
#[tauri::command]
fn test_alerting_recipient_by_id(id: String) -> Result<String, String> {
    core::alerting::test_recipient(&id)
}

/// Phase 68 — detect cycles in the alerting rule graph (route + correlation).
#[tauri::command]
fn detect_alerting_cycles() -> Vec<core::alerting::CycleReport> {
    core::alerting::detect_route_cycles()
}

/// Phase 68 — read the most recent N seen events (new→old), limit capped at 256.
#[tauri::command]
fn recent_alerting_events(limit: usize) -> Vec<core::alerting::RouteSeenEvent> {
    core::alerting::recent_events_snapshot(limit)
}

/// Phase 69 — return the 4-layer severity chain for a source.
#[tauri::command]
fn severity_inheritance_chain(source: String) -> Vec<core::alerting::SeverityLink> {
    core::alerting::severity_inheritance_chain(&source)
}

/// Phase 69 — delete a user-origin severity hint and list affected routes / correlations / aggregations.
#[tauri::command]
fn delete_alerting_severity_hint_cascade(source: String) -> core::alerting::CascadeReport {
    core::alerting::cascade_delete_severity_hint(&source)
}

/// Phase 70 — for a source, list which severity the 4 main paths (route / correlation / aggregation / escalation) actually use + which link of the chain it comes from.
#[tauri::command]
fn severity_propagation_trace(source: String) -> core::alerting::PropagationTrace {
    core::alerting::propagation_trace(&source)
}

/// Phase 74 — dry-run preview endpoint severity override (pure function + reads list_alerting_endpoints, no dispatch side effects)
/// endpoint_id = None → all enabled endpoints; Some(id) → only that endpoint
#[tauri::command]
fn preview_alerting_endpoint_severity(
    source: String,
    endpoint_id: Option<String>,
) -> Result<core::alerting::EndpointSeverityPreview, String> {
    core::alerting::preview_alerting_endpoint_severity(&source, endpoint_id.as_deref())
}

/// Phase 75 — Alerting full-chain dry-run simulator: given a source + optional payload, runs side-effect-free through
/// the five links route / correlation / aggregation / escalation + endpoint fanout, returning the decision at each stage.
#[tauri::command]
fn simulate_alerting_dispatch(
    source: String,
    payload: Option<String>,
) -> Result<core::alerting::AlertingDispatchSimulation, String> {
    core::alerting::simulate_alerting_dispatch(&source, payload.as_deref())
}

/// Phase 50 — clear expired acks (can run at startup or manually).
#[tauri::command]
fn clear_expired_alerting_acks() -> usize {
    core::alerting::clear_expired_acks()
}

/// Phase 51 — list all DSL route rules (including disabled, by priority ASC).
#[tauri::command]
fn list_alerting_routes() -> Vec<core::storage::RouteRuleRow> {
    core::alerting::list_routes()
}

/// Phase 51 — upsert a DSL route (empty id = create).
#[tauri::command]
fn save_alerting_route(
    rule: core::alerting::RouteRule,
) -> Result<core::storage::RouteRuleRow, String> {
    core::alerting::save_route(rule)
}

/// Phase 51 — delete a DSL route.
#[tauri::command]
fn delete_alerting_route(id: String) -> bool {
    core::alerting::delete_route(&id)
}

/// Phase 51 — YAML import (atomic replace).
#[tauri::command]
fn import_alerting_routes_yaml(yaml: String) -> Result<usize, String> {
    core::alerting::import_routes_yaml(&yaml)
}

/// Phase 51 — YAML export.
#[tauri::command]
fn export_alerting_routes_yaml() -> Result<String, String> {
    core::alerting::export_routes_yaml()
}

/// Phase 51 — Dry-run: use `(source, payload_json)` to see which route matches.
#[tauri::command]
fn dry_run_alerting_route(
    source: String,
    payload_json: String,
) -> Result<Option<core::storage::RouteRuleRow>, String> {
    core::alerting::dry_run_route(&source, &payload_json)
}

// ─── Phase 53: severity hints ────────────────────────────────────────────────

/// Phase 53 — list all severity hints (user + manifest).
#[tauri::command]
fn list_alerting_severity_hints() -> Vec<core::alerting::SeverityHintDto> {
    core::alerting::list_severity_hints()
}

/// Phase 53 — User saves a hint (source non-empty + severity valid).
#[tauri::command]
fn save_alerting_severity_hint(
    source: String,
    severity: String,
) -> Result<core::alerting::SeverityHintDto, String> {
    core::alerting::save_user_severity_hint(&source, &severity)
}

/// Phase 53 — delete a user-origin hint.
#[tauri::command]
fn delete_alerting_severity_hint(source: String) -> bool {
    core::alerting::delete_user_severity_hint(&source)
}

/// Phase 53 — clear all user-origin hints.
#[tauri::command]
fn clear_alerting_severity_hints() -> usize {
    core::alerting::clear_user_severity_hints()
}

// ─── Phase 54: alerting aggregation / frequency-threshold rules ──────────────

/// Phase 54 — list all aggregation rules (for the settings UI).
#[tauri::command]
fn list_alerting_aggregations() -> Vec<core::alerting::AggregationRuleDto> {
    core::alerting::list_aggregations()
}

/// Phase 54 — create / update an aggregation rule (auto-fills agg-<uuid> when id is empty).
#[tauri::command]
fn save_alerting_aggregation(
    rule: core::alerting::AggregationRule,
) -> Result<core::alerting::AggregationRuleDto, String> {
    core::alerting::save_aggregation(rule)
}

/// Phase 54 — delete an aggregation rule.
#[tauri::command]
fn delete_alerting_aggregation(id: String) -> bool {
    core::alerting::delete_aggregation(&id)
}

/// Phase 54 — clear all aggregation rules + reset in-memory buckets (for the settings UI + tests).
#[tauri::command]
fn clear_alerting_aggregations() -> usize {
    let n = core::alerting::clear_aggregations();
    core::alerting::_reset_aggregations_for_tests();
    n
}

// ─── Phase 55: alerting correlation suppression (A → B, B suppressed within window_secs) ────────

/// Phase 55 — list all correlation suppression rules (for the settings UI).
#[tauri::command]
fn list_alerting_correlations() -> Vec<core::alerting::CorrelationRuleDto> {
    core::alerting::list_correlations()
}

/// Phase 55 — create / update a correlation rule (auto-fills cor-<uuid> when id is empty).
#[tauri::command]
fn save_alerting_correlation(
    rule: core::alerting::CorrelationRule,
) -> Result<core::alerting::CorrelationRuleDto, String> {
    core::alerting::save_correlation(rule)
}

/// Phase 55 — delete a correlation rule.
#[tauri::command]
fn delete_alerting_correlation(id: String) -> bool {
    core::alerting::delete_correlation(&id)
}

/// Phase 55 — clear all correlation rules + reset last_a memory (for the settings UI + tests).
#[tauri::command]
fn clear_alerting_correlations() -> usize {
    let n = core::alerting::clear_correlations();
    core::alerting::_reset_correlations_for_tests();
    n
}

// ─── Phase 56: alerting upgrade / escalation chain ───────────────────────────

/// Phase 56 — list all escalation rules (for the settings UI).
#[tauri::command]
fn list_alerting_escalations() -> Vec<core::alerting::EscalationRuleDto> {
    core::alerting::list_escalations()
}

/// Phase 56 — create / update an escalation rule (auto-fills esc-<uuid> when id is empty).
#[tauri::command]
fn save_alerting_escalation(
    rule: core::alerting::EscalationRule,
) -> Result<core::alerting::EscalationRuleDto, String> {
    core::alerting::save_escalation(rule)
}

/// Phase 56 — delete an escalation rule.
#[tauri::command]
fn delete_alerting_escalation(id: String) -> bool {
    core::alerting::delete_escalation(&id)
}

/// Phase 56 — clear all escalation rules + reset last_by_source memory (for the settings UI + tests).
#[tauri::command]
fn clear_alerting_escalations() -> usize {
    let n = core::alerting::clear_escalations();
    core::alerting::_reset_escalations_for_tests();
    n
}

/// Phase 43 — Plugin log filter (query substring + level + plugin_id + since_ts + limit).
#[derive(serde::Deserialize)]
struct LogFilterArgs {
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
fn search_logs(filter: LogFilterArgs) -> Vec<LogEntry> {
    let Some(store) = core::shared_store() else {
        return Vec::new();
    };
    let lf = core::log_search::LogFilter {
        query: filter.query,
        level: filter.level,
        plugin_id: filter.plugin_id,
        since_ts: filter.since_ts,
        limit: if filter.limit == 0 { 500 } else { filter.limit },
    };
    core::log_search::search_logs(&store, lf)
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
fn list_replay_sessions() -> Vec<core::event_replay::ReplaySession> {
    core::event_replay::list_sessions()
}

/// Phase 34 — read a session's events (in order, truncated by limit).
#[tauri::command]
fn get_replay_events(session_id: String, limit: usize) -> Vec<core::event::OpencapxEvent> {
    core::event_replay::read_session(&session_id, limit)
}

/// Phase 34 — replay a session's events into the EventBus (filter optional, empty = no filter).
#[tauri::command]
fn replay_session_to_stream(session_id: String, filter_kind: String) -> usize {
    let bus = core::event::EventBus::shared();
    let filter = if filter_kind.is_empty() {
        None
    } else {
        Some(filter_kind.as_str())
    };
    core::event_replay::replay_to_bus(&session_id, filter, &bus)
}

#[derive(serde::Serialize)]
struct LifecycleEntry {
    id: String,
    kind: String,
    timestamp: u64,
    reason: String,
}

#[tauri::command]
fn list_plugin_lifecycle(id: String, limit: usize) -> Vec<LifecycleEntry> {
    let Some(store) = core::shared_store() else {
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
fn list_plugin_traces(id: String) -> Vec<core::plugin_trace::TraceSummary> {
    core::plugin_trace::list_sessions(&id)
}

#[tauri::command]
fn get_plugin_trace(
    id: String,
    session_id: String,
    limit: usize,
) -> Vec<core::plugin_trace::TraceLine> {
    core::plugin_trace::read_session(&id, &session_id, limit.max(50).min(2000))
}

/// Raw lines of a single request chain (descending = newest first).
#[tauri::command]
fn get_rpc_trace(
    agent_id: String,
    trace_id: String,
    limit: usize,
) -> Vec<core::req_trace::RpcTraceLine> {
    core::req_trace::read_trace(&agent_id, &trace_id, limit.max(50).min(2000))
}

/// Raw lines of a single hook session (descending = newest first).
#[tauri::command]
fn get_hook_trace(
    agent_id: String,
    session_id: String,
    limit: usize,
) -> Vec<core::req_trace::RpcTraceLine> {
    core::req_trace::read_hook_trace(&agent_id, &session_id, limit.max(50).min(2000))
}

/// All /rpc request chains (all agents, newest first), with project so the viewer can group by project.
#[tauri::command]
fn list_all_traces() -> Vec<core::req_trace::TraceEntry> {
    core::req_trace::list_all_traces()
}

/// All hook sessions (all agents, newest first), shown alongside request chains and grouped by the same project.
#[tauri::command]
fn list_all_hook_sessions() -> Vec<core::req_trace::TraceEntry> {
    core::req_trace::list_all_hook_sessions()
}

/// Export all /rpc request chain raw files for a project to a user-selected directory and write an index.json manifest.
/// Error codes are in ExportReport (no_chains / dir_unwritable / io: <detail>).
#[tauri::command]
fn export_project_chains(
    dir: String,
    project: String,
) -> Result<core::req_trace::ExportReport, String> {
    core::req_trace::export_chains_to(&dir, &project)
}

#[tauri::command]
fn toggle_plugin(id: String) -> Result<bool, String> {
    core::plugin::PluginManager::shared().toggle(&id)
}

#[tauri::command]
fn uninstall_plugin(id: String) -> Result<(), String> {
    core::plugin::PluginManager::shared().uninstall(&id)
}

#[tauri::command]
fn preview_uninstall_plugin(id: String) -> Result<core::plugin::UninstallPreviewDto, String> {
    core::plugin::PluginManager::shared().uninstall_preview(&id)
}

#[tauri::command]
fn set_plugin_auto_reload(id: String, on: bool) -> Result<(), String> {
    core::plugin::PluginManager::shared().set_auto_reload(&id, on)
}

#[derive(serde::Serialize)]
struct PluginConfigSnapshot {
    /// At minimum merges installed plugin ids; those without a config file are listed too (empty object).
    plugins: Vec<core::PluginConfigEntry>,
}

#[tauri::command]
fn list_plugin_config() -> PluginConfigSnapshot {
    // union installed plugins with existing config files so the UI gets the full view in one go.
    let installed: Vec<String> = core::plugin::PluginManager::shared()
        .list()
        .into_iter()
        .map(|p| p.id)
        .collect();
    let mut by_id: std::collections::HashMap<String, serde_json::Value> =
        core::config::snapshot().into_iter().collect();
    let mut out: Vec<core::PluginConfigEntry> = Vec::new();
    for id in installed {
        let cfg = by_id.remove(&id).unwrap_or(serde_json::json!({}));
        out.push(core::PluginConfigEntry { id, config: cfg });
    }
    // list uninstalled plugins that still have config (so a reinstall does not lose it)
    for (id, cfg) in by_id {
        out.push(core::PluginConfigEntry { id, config: cfg });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    PluginConfigSnapshot { plugins: out }
}

#[tauri::command]
fn get_plugin_config(id: String) -> serde_json::Value {
    serde_json::Value::Object(core::config::all(&id))
}

#[tauri::command]
fn set_plugin_config(id: String, value: serde_json::Value) -> Result<(), String> {
    core::config::replace(&id, &value).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_plugin_config(id: String) -> Result<(), String> {
    core::config::reset(&id).map_err(|e| e.to_string())
}

/// M7/F8 — generic settings page: declared schema + current values (secret only reports "set", value is not returned).
#[tauri::command]
fn list_plugin_settings(id: String) -> Result<core::plugin::SettingsViewDto, String> {
    core::plugin::PluginManager::settings_view(&id)
}

/// Plugin detail page: read the plugin directory README.md (None = not provided).
#[tauri::command]
fn read_plugin_readme(id: String) -> Result<Option<String>, String> {
    core::plugin::PluginManager::readme(&id)
}

/// Corrupt-DB quarantine notice (None = none). Read once when the settings page starts.
#[tauri::command]
fn db_recovery_notice() -> Option<serde_json::Value> {
    core::profile::db_recovery_notice()
}

/// User clicks "Got it" → delete the notice file (the quarantined corrupt DB file is kept for later data recovery).
#[tauri::command]
fn dismiss_db_recovery_notice() -> Result<(), String> {
    core::profile::dismiss_db_recovery_notice()
}

/// P3 — list control: forwards CRUD ops to the plugin method `settings.<key>` (items belong to the plugin).
#[tauri::command]
fn invoke_plugin_setting_list(
    id: String,
    key: String,
    op: String,
    index: Option<usize>,
    to: Option<usize>,
    value: Option<String>,
) -> Result<serde_json::Value, String> {
    core::plugin::PluginManager::invoke_setting_list_op(&id, &key, &op, index, to, value)
}

/// M7/F8 — write a single declarative setting (secret goes to keychain; key must be within the declaration).
#[tauri::command]
fn set_plugin_setting(id: String, key: String, value: serde_json::Value) -> Result<(), String> {
    core::plugin::PluginManager::set_setting_value(&id, &key, &value)
}

/// M7/F8 — button control: calls the plugin method `settings.<key>`.
#[tauri::command]
fn invoke_plugin_setting_action(id: String, key: String) -> Result<serde_json::Value, String> {
    core::plugin::PluginManager::invoke_setting_action(&id, &key)
}

#[tauri::command]
fn list_permissions() -> Vec<core::permission::PluginPermissionsDto> {
    let plugins = core::plugin::PluginManager::shared().list();
    match core::shared_store() {
        Some(store) => core::permission::view(&store, &plugins),
        None => Vec::new(),
    }
}

#[tauri::command]
fn set_permission(plugin_id: String, permission: String, decision: String) -> Result<bool, String> {
    let Some(store) = core::shared_store() else {
        return Err("storage unavailable".into());
    };
    // docs/permissions.md: high-risk permissions can only be granted once at a time, no persistent granted
    if core::permission::HIGH_RISK.contains(&permission.as_str()) && decision == "granted" {
        return Err(
            "high-risk permission cannot be granted permanently; allow once at runtime".into(),
        );
    }
    // docs/permission-domains.md §4.3 enforcement point 3: declared-derived permissions are once-only —
    // set_decision rejects granted, so give a readable reason here first (otherwise only a generic error is shown)
    if decision == "granted" && core::permission::is_declared(&store, &permission) {
        return Err(
            "permission comes from a third-party domain declaration; it is once-only and cannot be granted permanently"
                .into(),
        );
    }
    if core::permission::set_decision(&store, &plugin_id, &permission, &decision) {
        Ok(true)
    } else {
        Err("failed to save decision (unknown permission/decision or sqlite unavailable)".into())
    }
}

// ─── Agent identity settings page commands (docs/permissions.md "Revoke and restore" "Agents view") ───

/// Agents view list: identity + status + last seen. The token never leaves Core.
#[tauri::command]
fn agents_list() -> Vec<core::identity::AgentDto> {
    match core::shared_store() {
        Some(store) => core::identity::list(&store),
        None => Vec::new(),
    }
}

/// Revoke: token invalidated immediately (40102), auto re-registration rejected; only reauthorize can lift it.
#[tauri::command]
fn agent_revoke(agent_id: String) -> Result<bool, String> {
    let Some(store) = core::shared_store() else {
        return Err("storage unavailable".into());
    };
    if core::identity::revoke(&store, &agent_id) {
        Ok(true)
    } else {
        Err("agent not found or already revoked".into())
    }
}

/// Reauthorize: issue a new token (old token void). Returns the token for the settings page to show once,
/// the user updates the CLI token file manually; nothing persists in frontend state.
#[tauri::command]
fn agent_reauthorize(agent_id: String) -> Result<String, String> {
    let Some(store) = core::shared_store() else {
        return Err("storage unavailable".into());
    };
    core::identity::reauthorize(&store, &agent_id).ok_or_else(|| "agent not found".into())
}

/// Agent-layer decision (agent_permissions table). High-risk permissions likewise disallow persistent granted.
#[tauri::command]
fn agent_set_permission(
    agent_id: String,
    permission: String,
    decision: String,
) -> Result<bool, String> {
    let Some(store) = core::shared_store() else {
        return Err("storage unavailable".into());
    };
    if core::permission::HIGH_RISK.contains(&permission.as_str()) && decision == "granted" {
        return Err(
            "high-risk permission cannot be granted permanently; allow once at runtime".into(),
        );
    }
    if decision == "granted" && core::permission::is_declared(&store, &permission) {
        return Err(
            "permission comes from a third-party domain declaration; it is once-only and cannot be granted permanently"
                .into(),
        );
    }
    if core::identity::set_agent_decision(&store, &agent_id, &permission, &decision) {
        Ok(true)
    } else {
        Err("failed to save decision (unknown permission/decision or sqlite unavailable)".into())
    }
}

/// Full permission vocabulary for a single agent (decision = agent override > default table).
#[tauri::command]
fn agent_permissions(agent_id: String) -> Vec<core::permission::PermissionEntryDto> {
    match core::shared_store() {
        Some(store) => core::permission::agent_view(&store, &agent_id),
        None => Vec::new(),
    }
}

/// Global policy view (settings page "System capabilities" tab; see docs/permissions.md "Global policy (hard gate)").
#[tauri::command]
fn core_permission_list() -> Vec<core::permission::CorePermPolicyDto> {
    match core::shared_store() {
        Some(store) => core::permission::core_policy_list(&store),
        None => Vec::new(),
    }
}

/// Write global policy. Globally granted is rejected for high-risk permissions (same enforcement as agent_set_permission).
#[tauri::command]
fn core_permission_set(permission: String, decision: String) -> Result<bool, String> {
    let Some(store) = core::shared_store() else {
        return Err("storage unavailable".into());
    };
    core::permission::set_global_override(&store, &permission, &decision).map(|_| true)
}

/// Clear the global policy override, restoring the built-in default.
#[tauri::command]
fn core_permission_reset(permission: String) -> Result<bool, String> {
    let Some(store) = core::shared_store() else {
        return Err("storage unavailable".into());
    };
    core::permission::clear_global_override(&store, &permission).map(|_| true)
}

fn settings_path() -> std::path::PathBuf {
    if let Some(home) = crate::core::home_dir() {
        return home.join(".opencapx").join("settings.json");
    }
    std::env::temp_dir().join("opencapx-settings.json")
}

fn default_settings() -> serde_json::Value {
    serde_json::json!({
        "theme": "dark",
        "opacity": 0.9,
        "fontSize": 13,
        "mode": "carousel",
        "soundDone": true,
        "soundWaiting": true,
        "bubbleEnabled": true,
        "bubbleDuration": 5,
        "petVisible": true,
        "onboarded": false,
        "breakEnabled": false,
        "breakMinutes": 60,
        "locale": "en",
        // SessionStart additionalContext injection (docs/mcp.md): capability digest into
        // supported agents' context at session start. Opt-out, read by http::session_start_reply.
        "sessionContextInject": true
    })
}

#[tauri::command]
fn get_settings() -> serde_json::Value {
    read_settings_file()
}

fn read_settings_file() -> serde_json::Value {
    let path = settings_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            return v;
        }
    }
    default_settings()
}

/// Mutable state of the tray menu.
///
/// The menu is **dynamically rebuilt** (the session list changes with agent activity), so the check item handle is always
/// a new object — you cannot `manage` a fixed handle, or syncing the check state would write to a discarded menu.
/// `signature` debounces content: no rebuild when nothing changed, avoiding per-second tray churn.
struct TrayState {
    pet_check: std::sync::Mutex<Option<tauri::menu::CheckMenuItem<tauri::Wry>>>,
    bubble_check: std::sync::Mutex<Option<tauri::menu::CheckMenuItem<tauri::Wry>>>,
    signature: std::sync::Mutex<String>,
}

fn sync_tray_pet_check(app: &tauri::AppHandle, visible: bool) {
    use tauri::Manager;
    if let Some(state) = app.try_state::<TrayState>() {
        if let Ok(g) = state.pet_check.lock() {
            if let Some(item) = g.as_ref() {
                let _ = item.set_checked(visible);
            }
        }
    }
}

/// Keeps the tray "Show Bubble" check in sync when bubbleEnabled changes from the settings page
/// (same pattern as the pet check: settings writes do not fire tray-refreshing events).
fn sync_tray_bubble_check(app: &tauri::AppHandle, enabled: bool) {
    use tauri::Manager;
    if let Some(state) = app.try_state::<TrayState>() {
        if let Ok(g) = state.bubble_check.lock() {
            if let Some(item) = g.as_ref() {
                let _ = item.set_checked(enabled);
            }
        }
    }
}

/// Menubar icon: composites a status dot onto the base icon (waiting=orange, working=green, otherwise keeps the original).
///
/// This is the only "status visualization" a native tray can do — you can see someone is waiting without opening the menu.
/// Cached by badge to avoid recompositing pixels on every refresh.
fn tray_icon(badge: core::tray::TrayBadge) -> Option<tauri::image::Image<'static>> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<std::collections::HashMap<u8, Vec<u8>>>> = OnceLock::new();
    let base = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png")).ok()?;
    if badge == core::tray::TrayBadge::None {
        return Some(tauri::image::Image::new_owned(
            base.rgba().to_vec(),
            base.width(),
            base.height(),
        ));
    }
    let key = if badge == core::tray::TrayBadge::Waiting {
        2u8
    } else {
        1u8
    };
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let rgba = {
        let mut map = cache.lock().ok()?;
        map.entry(key)
            .or_insert_with(|| {
                core::tray::compose_icon(base.rgba(), base.width(), base.height(), badge)
            })
            .clone()
    };
    Some(tauri::image::Image::new_owned(
        rgba,
        base.width(),
        base.height(),
    ))
}

/// Update the menubar icon and tooltip. Both are information you can see without opening the menu.
fn refresh_tray_status(
    app: &tauri::AppHandle,
    strs: &core::i18n::Strings,
    sessions: &[core::agent::Session],
) {
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let count =
        |state: core::agent::AgentState| sessions.iter().filter(|s| s.state == state).count();
    let (working, waiting) = (
        count(core::agent::AgentState::Working),
        count(core::agent::AgentState::Waiting),
    );
    let badge = core::tray::badge_for(working, waiting);
    if let Some(img) = tray_icon(badge) {
        // a colored badge cannot be a template (macOS would render it as a monochrome block);
        // set icon+template atomically to avoid flicker from setting icon before template.
        #[cfg(target_os = "macos")]
        let set = tray.set_icon_with_as_template(Some(img), badge == core::tray::TrayBadge::None);
        #[cfg(not(target_os = "macos"))]
        let set = tray.set_icon(Some(img));
        if let Err(e) = set {
            eprintln!("[tray] set_icon failed: {e}");
        }
    }
    let tip = core::tray::tooltip_text(strs, sessions);
    let _ = tray.set_tooltip(Some(tip.as_str()));
}

/// Rebuild the tray menu: summary at top + one row per active session + action items. Skip if content is unchanged.
fn refresh_tray_menu(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::Manager;
    let Some(tray) = app.tray_by_id("main") else {
        return Ok(());
    };
    let Some(store) = core::shared_store() else {
        return Ok(());
    };
    let mut sessions = store
        .lock()
        .ok()
        .map(|s| s.active(core::agent::now_secs()))
        .unwrap_or_default();
    core::agent::sort_sessions(&mut sessions);

    let settings = read_settings_file();
    let pet_visible = settings
        .get("petVisible")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let bubble_enabled = settings
        .get("bubbleEnabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    // native menu text follows the user's language (same setting as the frontend locale)
    let locale = settings
        .get("locale")
        .and_then(|v| v.as_str())
        .unwrap_or("en");
    let strs = core::i18n::strings(core::i18n::from_locale(locale));
    let count =
        |state: core::agent::AgentState| sessions.iter().filter(|s| s.state == state).count();
    let working = count(core::agent::AgentState::Working);
    let waiting = count(core::agent::AgentState::Waiting);
    let done = count(core::agent::AgentState::Done);
    let clearable = sessions.iter().any(|s| {
        matches!(
            s.state,
            core::agent::AgentState::Done | core::agent::AgentState::Idle
        )
    });
    // sessions grouped by project (one submenu per project); branch looked up in core::project (30s cache).
    // group order = appearance order of the first session in the group — sessions are already sorted by sort_sessions,
    // so "projects with someone waiting" naturally come first, the same rule as the bubble group headers.
    let now_s = core::agent::now_secs();
    let branch_of = |cwd: &str| core::project::meta(cwd, now_s).branch;
    let (sections, ungrouped) = core::tray::sections(strs, &sessions, &branch_of);
    let summary = core::tray::summary_text(strs, working, waiting, done, sessions.len());
    // structure goes into the signature: headers contain branches, so switching branch must rebuild the menu
    let structure: Vec<String> = sections
        .iter()
        .map(|s| format!("[{}] {}", s.header, s.rows.join(" | ")))
        .chain(ungrouped.iter().cloned())
        .collect();
    // the CLI item exists only while the command is missing: no symlink at /usr/local/bin, nothing
    // foreign occupying it, and nothing named opencapx resolving on PATH. Installing from the menu
    // (or from Settings) flips this, and the next rebuild drops the item.
    let cli = cli_install::status();
    let install_cli_item =
        cli.supported && !cli.installed && !cli.foreign && !cli_install::on_path();
    // locale goes into the signature: switching language must rebuild the menu
    let signature = format!(
        "{}|{}|{}|{}|{}|{}|{}",
        locale,
        pet_visible,
        bubble_enabled,
        clearable,
        summary,
        install_cli_item,
        structure.join("\n")
    );
    if let Some(state) = app.try_state::<TrayState>() {
        if let Ok(g) = state.signature.lock() {
            if *g == signature {
                return Ok(());
            }
        }
    }

    // top level: empty state / summary / ungrouped sessions (those without a cwd, not forced into submenus)
    let mut top: Vec<tauri::menu::MenuItem<tauri::Wry>> = Vec::new();
    if sections.is_empty() && ungrouped.is_empty() {
        // the empty state says one thing only, not "0 Working · 0 Waiting"
        top.push(tauri::menu::MenuItem::with_id(
            app,
            "session-empty",
            strs.no_active_agents,
            false,
            None::<&str>,
        )?);
    } else {
        top.push(tauri::menu::MenuItem::with_id(
            app,
            "session-summary",
            summary.as_str(),
            false,
            None::<&str>,
        )?);
        for (i, label) in ungrouped.iter().enumerate() {
            top.push(tauri::menu::MenuItem::with_id(
                app,
                format!("session-u{i}"),
                label.as_str(),
                false,
                None::<&str>,
            )?);
        }
    }

    // one submenu per project. The parent must be enabled=true: on macOS a disabled parent cannot be opened
    // into its submenu. Tauri's Submenu has no action slot, so clicking it only expands and cannot misfire anything.
    let mut subs: Vec<tauri::menu::Submenu<tauri::Wry>> = Vec::new();
    for (i, section) in sections.iter().enumerate() {
        let rows: Vec<tauri::menu::MenuItem<tauri::Wry>> = section
            .rows
            .iter()
            .enumerate()
            .map(|(j, label)| {
                tauri::menu::MenuItem::with_id(
                    app,
                    format!("session-{i}-{j}"),
                    label.as_str(),
                    false,
                    None::<&str>,
                )
            })
            .collect::<tauri::Result<Vec<_>>>()?;
        let row_refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = rows
            .iter()
            .map(|r| r as &dyn tauri::menu::IsMenuItem<tauri::Wry>)
            .collect();
        subs.push(tauri::menu::Submenu::with_id_and_items(
            app,
            format!("project-{i}"),
            section.header.as_str(),
            true,
            &row_refs,
        )?);
    }
    let sep = tauri::menu::PredefinedMenuItem::separator(app)?;
    let clear = tauri::menu::MenuItem::with_id(
        app,
        "clear-finished",
        strs.clear_finished,
        clearable,
        None::<&str>,
    )?;
    let install_cli = install_cli_item
        .then(|| {
            tauri::menu::MenuItem::with_id(app, "install-cli", strs.install_cli, true, None::<&str>)
        })
        .transpose()?;
    let toggle = tauri::menu::CheckMenuItem::with_id(
        app,
        "toggle-pet",
        strs.show_pet,
        true,
        pet_visible,
        None::<&str>,
    )?;
    let bubble_toggle = tauri::menu::CheckMenuItem::with_id(
        app,
        "toggle-bubble",
        strs.show_bubble,
        true,
        bubble_enabled,
        None::<&str>,
    )?;
    let open_settings_item = tauri::menu::MenuItem::with_id(
        app,
        "open-settings",
        strs.open_settings,
        true,
        None::<&str>,
    )?;
    let quit = tauri::menu::MenuItem::with_id(app, "quit", strs.quit, true, None::<&str>)?;

    // order: summary/ungrouped → project submenus → separator → action items
    let mut refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = top
        .iter()
        .map(|i| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>)
        .collect();
    for sub in &subs {
        refs.push(sub);
    }
    refs.push(&sep);
    refs.push(&clear);
    refs.push(&toggle);
    refs.push(&bubble_toggle);
    if let Some(item) = &install_cli {
        refs.push(item);
    }
    refs.push(&open_settings_item);
    refs.push(&quit);
    let menu = tauri::menu::Menu::with_items(app, &refs)?;
    tray.set_menu(Some(menu))?;

    if let Some(state) = app.try_state::<TrayState>() {
        if let Ok(mut g) = state.pet_check.lock() {
            *g = Some(toggle.clone());
        }
        if let Ok(mut g) = state.bubble_check.lock() {
            *g = Some(bubble_toggle.clone());
        }
        if let Ok(mut g) = state.signature.lock() {
            *g = signature;
        }
    }
    // icon badge + tooltip share the menu's source and are updated together here (the part visible without opening).
    refresh_tray_status(app, strs, &sessions);
    // rebuild only when content actually changed, so this log line is naturally throttled.
    eprintln!(
        "[tray] lang={locale} menu={:?} badge={:?} tip={}",
        summary,
        core::tray::badge_for(working, waiting),
        core::tray::tooltip_text(strs, &sessions)
    );
    Ok(())
}

/// Clear finished/idle sessions (tray "Clear finished").
fn clear_finished_sessions() {
    let Some(store) = core::shared_store() else {
        return;
    };
    let Ok(mut s) = store.lock() else {
        return;
    };
    let ids: Vec<String> = s
        .active(core::agent::now_secs())
        .iter()
        .filter(|x| {
            matches!(
                x.state,
                core::agent::AgentState::Done | core::agent::AgentState::Idle
            )
        })
        .map(|x| x.id.clone())
        .collect();
    for id in ids {
        s.dismiss(&id);
    }
}

fn set_pet_visible(app: &tauri::AppHandle, v: bool) {
    let mut s = read_settings_file();
    if let Some(o) = s.as_object_mut() {
        o.insert("petVisible".to_string(), serde_json::json!(v));
    }
    let _ = write_settings_file(&s);
    sync_tray_pet_check(app, v);
}

/// Tray "Show Bubble": flips bubbleEnabled in the settings file. The overlay polls settings
/// every second, so the bubble follows within one tick — no window command involved (unlike the
/// pet, whose window is shown/hidden directly; the bubble lives inside the same window).
fn set_bubble_visible(app: &tauri::AppHandle, v: bool) {
    let mut s = read_settings_file();
    if let Some(o) = s.as_object_mut() {
        o.insert("bubbleEnabled".to_string(), serde_json::json!(v));
    }
    let _ = write_settings_file(&s);
    sync_tray_bubble_check(app, v);
}

fn write_settings_file(value: &serde_json::Value) -> bool {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(value) {
        Ok(text) => std::fs::write(&path, text).is_ok(),
        Err(_) => false,
    }
}

#[tauri::command]
fn set_settings(app: tauri::AppHandle, value: serde_json::Value) -> bool {
    let ok = write_settings_file(&value);
    if let Some(v) = value.get("petVisible").and_then(|x| x.as_bool()) {
        sync_tray_pet_check(&app, v);
    }
    if let Some(v) = value.get("bubbleEnabled").and_then(|x| x.as_bool()) {
        sync_tray_bubble_check(&app, v);
    }
    ok
}

/// For the settings page "Copy chain": writes text to the system clipboard (UI side, bypassing the Agent permission gate).
#[tauri::command]
fn ui_clipboard_write(text: String) -> Result<(), String> {
    core::clipboard::write_text(&text)
}

#[tauri::command]
fn get_agents() -> Vec<hooks::AgentInfo> {
    hooks::catalog()
}

#[tauri::command]
fn toggle_agent(kind: String) -> Result<bool, String> {
    hooks::toggle(&kind)
}

/// Pet window mouse passthrough strategy: once a window ignores cursor events it receives no mousemove at all,
/// and the frontend cannot switch itself back (it would stay click-through, unclickable and undraggable). So poll the global cursor
/// position here, convert to in-window logical coordinates, and send to the frontend, which decides passthrough by actual layout (pet canvas / bubbles).
fn spawn_pet_hover_tracker(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        use tauri::{Emitter, Manager};
        let mut tick: u64 = 0;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(60));
            let Some(w) = app.get_webview_window("pet") else {
                continue;
            };
            if !w.is_visible().unwrap_or(false) {
                continue;
            }
            // re-assert "visible on all Spaces" every ~2s: once the window lands on another Space,
            // the current desktop cannot see the pet, and win.show() cannot bring it back.
            tick = tick.wrapping_add(1);
            if tick % 30 == 0 {
                let _ = w.set_visible_on_all_workspaces(true);
            }
            let (Ok(cursor), Ok(pos)) = (app.cursor_position(), w.outer_position()) else {
                continue;
            };
            let scale = w.scale_factor().unwrap_or(1.0);
            let _ = app.emit_to(
                "pet",
                "pet-cursor",
                serde_json::json!({
                    "x": (cursor.x - pos.x as f64) / scale,
                    "y": (cursor.y - pos.y as f64) / scale,
                }),
            );
        }
    });
}

/// Show the settings window. On macOS, unhide and activate the app first, otherwise when launched from a terminal the window may
/// stay on another Space and be invisible to the user.
fn show_settings(app: &tauri::AppHandle) {
    use tauri::Manager;
    #[cfg(target_os = "macos")]
    let _ = app.show();
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[tauri::command]
fn open_settings(app: tauri::AppHandle) {
    show_settings(&app);
}

#[tauri::command]
fn rotate_alerting_bundle_secret() -> Result<(), String> {
    core::alerting::rotate_bundle_secret()
}

#[derive(serde::Serialize)]
struct Stats {
    total: usize,
    today: usize,
    #[serde(rename = "byAgent")]
    by_agent: std::collections::HashMap<String, usize>,
}

fn day_start_secs(now: u64) -> u64 {
    (now / 86400) * 86400
}

fn compute_stats(sessions: &[core::agent::Session], now: u64) -> Stats {
    let start = day_start_secs(now);
    let mut by_agent = std::collections::HashMap::new();
    let mut today = 0;
    for s in sessions {
        *by_agent.entry(s.agent.clone()).or_insert(0) += 1;
        if s.updated_at >= start {
            today += 1;
        }
    }
    Stats {
        total: sessions.len(),
        today,
        by_agent,
    }
}

#[tauri::command]
fn ui_ping(count: usize, last_state: String) {
    eprintln!("[ui-ping] frontend sessions={} last={}", count, last_state);
}

fn cli_flag(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

/// /event uplink: Sent passes through (returning the response body — SessionStart carries
/// the additionalContext digest); Unreachable (app not running) silently queues locally,
/// drained in-process on next app start; Rejected (token invalid/revoked) queues and also
/// warns once about the recovery path (every hook event goes through here; only warns the first time per process).
fn deliver_event(
    payload: &str,
    creds: Option<&core::identity::Credentials>,
    kind: &str,
) -> (http::Deliver, Option<String>) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let (mut deliver, mut body) = http::post_event_with_body(payload, creds);
    // token invalid (40101, not revoked) → void the local token, re-register via TOFU, and deliver again.
    // so rotation/invalidation no longer requires manually deleting the token file; revoked (40102) deliberately skips this — revocation is the
    // user's deliberate decision and can only be lifted by reauthorizing on the settings page.
    if matches!(deliver, http::Deliver::RejectedBadToken) && core::identity::reset_credentials(kind)
    {
        let fresh = core::identity::ensure_registered(kind, "hook");
        let (d, b) = http::post_event_with_body(payload, fresh.as_ref());
        deliver = d;
        body = b;
    }
    match deliver {
        http::Deliver::Sent => {}
        http::Deliver::RejectedBadToken | http::Deliver::Rejected => {
            let _ = WARNED.set(());
            eprintln!(
                "OpenCapX: token rejected (agent revoked, or rotation self-heal failed). \
                 Events are queued locally. To recover: OpenCapX Settings -> Agents -> \
                 Reauthorize, then update the token file in ~/.opencapx/agent-tokens/ and rerun."
            );
            let _ = queue::enqueue(&http::queue_dir(), payload);
        }
        http::Deliver::Unreachable => {
            let _ = queue::enqueue(&http::queue_dir(), payload);
        }
    }
    (deliver, body)
}

/// `opencapx connect <claude|codex|opencode>`: installs hooks + writes `opencapx mcp`
/// into that agent's MCP config (idempotent; no credentials in the config, mcp handles TOFU itself at startup).
/// Settings → General: the global `opencapx` command state.
#[tauri::command]
fn cli_command_status() -> cli_install::Status {
    cli_install::status()
}

/// Settings → General: install the global command (macOS: authorization dialog when needed).
/// Rebuilds the tray menu so its install item appears/disappears with the state.
#[tauri::command]
fn cli_command_install(app: tauri::AppHandle) -> Result<String, String> {
    let out = cli_install::install(true);
    let _ = refresh_tray_menu(&app);
    out
}

/// Settings → General: remove the global command.
#[tauri::command]
fn cli_command_uninstall(app: tauri::AppHandle) -> Result<String, String> {
    let out = cli_install::uninstall(true);
    let _ = refresh_tray_menu(&app);
    out
}

/// `opencapx install-cli` — symlink the stable shim into PATH so `opencapx` resolves from any
/// terminal. The Settings row calls `cli_install::install(true)` directly.
fn run_install_cli(elevate: bool) -> ! {
    match cli_install::install(elevate) {
        Ok(msg) => {
            eprintln!("install-cli: {msg}");
            if !cli_install::on_path() {
                eprintln!("install-cli: note: /usr/local/bin is not on this shell's PATH — add it to use the command by name");
            }
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("install-cli: {e}");
            std::process::exit(1);
        }
    }
}

/// `opencapx uninstall-cli` — remove the symlink (only if it is ours).
fn run_uninstall_cli(elevate: bool) -> ! {
    match cli_install::uninstall(elevate) {
        Ok(msg) => {
            eprintln!("uninstall-cli: {msg}");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("uninstall-cli: {e}");
            std::process::exit(1);
        }
    }
}

fn run_connect(kind: &str) -> ! {
    let catalog = hooks::catalog();
    if !catalog.iter().any(|a| a.kind == kind) {
        eprintln!(
            "connect: unknown agent: {} (options: {})",
            kind,
            catalog
                .iter()
                .map(|a| a.kind.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        std::process::exit(2);
    }
    match hooks::ensure_installed(kind) {
        Ok(()) => eprintln!("connect: hooks in place ({})", hooks::display_name(kind)),
        Err(e) => {
            eprintln!("connect: failed to write hooks: {}", e);
            std::process::exit(1);
        }
    }
    if hooks::supports_mcp(kind) {
        match hooks::ensure_mcp(kind) {
            Ok((path, written)) => {
                if written {
                    eprintln!("connect: MCP server written to {}", path);
                } else {
                    eprintln!("connect: MCP server already in place (unchanged)");
                }
            }
            Err(e) => {
                eprintln!("connect: failed to write MCP config: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        eprintln!("connect: {} is hooks-only — no MCP target yet (session state + command-rule rewrites only)", hooks::display_name(kind));
    }
    // Repair pass: entries written by an earlier build (or a dev checkout that has since
    // moved / been cleaned) still bake a dead binary path — repoint them at the stable shim.
    // Also backfills the codex identity env into MCP blocks written before it existed.
    let fixed = hooks::refresh_installations();
    if fixed > 0 {
        let noun = if fixed == 1 { "entry" } else { "entries" };
        eprintln!(
            "connect: repaired {} config {} (stable CLI path / codex identity env)",
            fixed, noun
        );
    }
    if hooks::supports_mcp(kind) {
        eprintln!(
            "connect: done. Restart {} and have it call opencapx.list_capabilities to self-test;",
            hooks::display_name(kind)
        );
        eprintln!("connect: auth is auto-registered at opencapx mcp startup; the config file contains no credentials.");
    } else {
        eprintln!("connect: done. Restart {} — the hooks report session state and apply command-rule rewrites by rule.", hooks::display_name(kind));
    }
    if !cli_install::on_path() {
        eprintln!("connect: tip: `opencapx` is not on PATH — run `opencapx install-cli` (or install it from Settings → General) to use the command by name");
    }
    std::process::exit(0);
}

/// Extract the working directory from the raw payload (the agent's cwd, used to locate project-level rules).
fn payload_cwd(payload: &str) -> Option<std::path::PathBuf> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("cwd")?
        .as_str()
        .map(std::path::PathBuf::from)
}

/// When the payload carries a host marker, trust it as the source.
///
/// opencode's native hook payload carries `hook_source: "opencode-plugin"`; but some third-party
/// forwarders hardcode `--agent claude`, mis-recording opencode traffic as claude (both telemetry and audit get
/// polluted). The payload has more say about "who am I" than the caller, so it overrides --agent here.
/// Only an explicit opencode declaration is honored; claude/codex and other normal paths are unaffected.
fn payload_host(payload: &str) -> Option<&'static str> {
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    let src = v.get("hook_source")?.as_str()?;
    if src.eq_ignore_ascii_case("opencode-plugin") {
        Some("opencode")
    } else {
        None
    }
}

/// Whether this hook should write back to the host (pure function, no IO).
///
/// Returns `Some` only when the agent supports PreToolUse's `updatedInput`, the event is PreToolUse, and the command matches
/// a rule; otherwise always `None` — stay a dumb pipe (zero stdout). The accompanying
/// `permissionDecision: "allow"` is decision D1: rewriting means allowing.
fn hook_response(payload: &str, agent: &str, set: &core::rules::RuleSet) -> Option<String> {
    hook_decision(payload, agent, set).map(|d| d.response)
}

struct HookDecision {
    rule_id: String,
    response: String,
}

/// Whether this host "actually applies command rewrites", and whether it writes back via stdout.
///
/// - `claude` / `codex` / `gemini`: the host honors stdout's `updatedInput` / `tool_input` → write back + record audit.
/// - `droid` / `copilot`: the Claude shape (`hookSpecificOutput.updatedInput`); copilot must be configured with the
///   PascalCase `PreToolUse` event for the VS Code-compatible payload that honors `updatedInput`.
/// - `omp`: the host itself ignores stdout, but the extension `connect` installs reads it and returns the
///   rewrite as `{ input }`, applied before the host's approval gate → write back + record audit.
/// - `cursor`: the top-level snake_case envelope (`permission` + `updated_input`), see `pre_tool_response`.
/// - `opencode`: the host does **not** honor stdout (see the opencode_plugin notes), but OpenCapX's own
///   opencode plugin does the rewrite via `rewrite` + in-place args editing → counts as applied, must be audited, but no writeback.
/// - everything else: neither rewrite nor audit (`None`).
fn rewrite_host(agent: &str) -> Option<bool> {
    match agent {
        "claude" | "codex" => Some(true),
        // gemini's hook **config** format shares its origin with Claude (`gemini hooks migrate` only migrates config),
        // but the **response** fields differ: it reads hookSpecificOutput.tool_input, see pre_tool_response.
        "gemini" => Some(true),
        "omp" => Some(true),
        "droid" | "copilot" => Some(true),
        "cursor" => Some(true),
        "opencode" => Some(false),
        _ => None,
    }
}

/// Each host's "pre-execution" event name (case-insensitive): Claude family uses PreToolUse, gemini uses BeforeTool.
fn is_pre_tool_event(name: &str) -> bool {
    name.eq_ignore_ascii_case("PreToolUse") || name.eq_ignore_ascii_case("BeforeTool")
}

/// Cursor's permission hooks answer with JSON on every invocation: with no rewrite and no audit-only
/// hit, print `{}` (valid JSON, no fields → no decision) instead of staying silent. Other hosts keep
/// the historical zero-stdout behavior.
fn cursor_noop_stdout(agent: &str, payload: &str) -> Option<&'static str> {
    if !agent.eq_ignore_ascii_case("cursor") {
        return None;
    }
    let event = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let name = event.get("hook_event_name")?.as_str()?;
    if is_pre_tool_event(name) {
        Some("{}")
    } else {
        None
    }
}

/// Build the "pre-execution rewrite" response body per host — the field names differ, so they cannot share one.
///
/// - `claude` / `codex` / `droid` / `copilot`:`hookSpecificOutput.updatedInput` + `permissionDecision`。
///   (copilot's rewritten commands still pass its own confirmation dialog — upstream github/copilot-cli#2643.)
/// - `gemini`: reads `hookSpecificOutput.tool_input` (**snake_case**, does not recognize `updatedInput`),
///   `hookEventName` is `BeforeTool`. See gemini-cli `docs/hooks/reference.md`.
/// - `omp`: the Claude shape; the installed OMP extension reads `updatedInput.command`
///   (it ignores `permissionDecision` — OMP's own approval gate decides).
/// - `cursor`: the top-level snake_case envelope (`continue` + `permission` + `updated_input`) — Cursor's
///   `preToolUse` does not read `hookSpecificOutput`.
fn pre_tool_response(agent: &str, rule_id: &str, tool_input: &serde_json::Value) -> String {
    if agent.eq_ignore_ascii_case("cursor") {
        serde_json::json!({
            "continue": true,
            "permission": "allow",
            "updated_input": tool_input,
        })
        .to_string()
    } else if agent.eq_ignore_ascii_case("gemini") {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "BeforeTool",
                "tool_input": tool_input,
            }
        })
        .to_string()
    } else {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": format!("OpenCapX rule {rule_id}"),
                "updatedInput": tool_input,
            }
        })
        .to_string()
    }
}

fn hook_decision(payload: &str, agent: &str, set: &core::rules::RuleSet) -> Option<HookDecision> {
    let emit_stdout = rewrite_host(agent)?;
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    let event = v.get("hook_event_name")?.as_str()?;
    if !is_pre_tool_event(event) {
        return None;
    }
    let mut tool_input = v.get("tool_input")?.clone();
    let cmd = tool_input.get("command")?.as_str()?;
    let core::rules::RewriteOutcome::Rewritten { rule_id, command } = combined_rewrite(cmd, set)
    else {
        return None;
    };
    // Audit-only hits (command unchanged): record the shape for the Timeline but write
    // nothing to stdout — emitting `permissionDecision: allow` here would auto-approve
    // exactly the shapes the guard itself could not make safe, bypassing the host's own
    // permission prompt. Trusted-passthrough is the one exception: the user explicitly
    // vouched for that host, so the allow is the point.
    if command == cmd && rule_id != "danger/trusted-passthrough" {
        return Some(HookDecision {
            rule_id,
            response: String::new(),
        });
    }
    if !emit_stdout {
        // no rewrite response, but still report rule_id for audit (opencode does the rewrite via its own plugin).
        return Some(HookDecision {
            rule_id,
            response: String::new(),
        });
    }
    tool_input["command"] = serde_json::Value::String(command);
    let response = pre_tool_response(agent, &rule_id, &tool_input);
    Some(HookDecision { rule_id, response })
}

fn run_hook(args: &[String]) -> ! {
    use std::io::Read;
    // Self-heal the stable CLI: when invoked from a path other than the shim (dev binary,
    // freshly installed app), refresh ~/.opencapx/bin/opencapx so configs keep working.
    // Typically one stat; a no-op when this process IS the shim.
    let _ = hooks::ensure_shim();
    let agent = cli_flag(args, "--agent").unwrap_or_else(|| "auto".into());
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    if stdin.trim().is_empty() {
        if let Some(event) = cli_flag(args, "--event") {
            let text = match event.as_str() {
                "working" => "running",
                "done" => "done",
                "registered" => "session start",
                other => other,
            };
            let session = cli_flag(args, "--session").unwrap_or_default();
            let project = cli_flag(args, "--project").unwrap_or_default();
            stdin = format!(
                "{{\"agent\":{},\"text\":{},\"session_id\":{},\"project\":{}}}",
                serde_json::to_string(&agent).unwrap_or_default(),
                serde_json::to_string(&text).unwrap_or_default(),
                serde_json::to_string(&session).unwrap_or_default(),
                serde_json::to_string(&project).unwrap_or_default(),
            );
        }
    }
    let (parsed_agent, _) = http::parse_hook_payload(&stdin);
    // when the payload carries its own source (opencode native marker) it overrides --agent: see payload_host.
    let host = payload_host(&stdin);
    let agent = match host {
        Some(h) => h.to_string(),
        None if agent.is_empty() || agent == "auto" => parsed_agent,
        None => agent,
    };
    // Agent identity (docs/permissions.md "Registration (TOFU)"): --agent first, otherwise host-sniffed env.
    // Core not running → None; /event failures are queued locally by deliver_event + warned by reason.
    let kind = match host {
        Some(h) => h.to_string(),
        None => match cli_flag(args, "--agent") {
            Some(k) if !k.is_empty() && k != "auto" => k,
            _ => core::identity::detect_kind(),
        },
    };
    let creds = core::identity::ensure_registered(&kind, "hook");
    // send the raw payload (injecting only agent / send time / matched rule id): parsing and enrichment
    // (transcript/model) all happen on the Core side; the CLI is a dumb pipe.
    // see core::agent::stamp_payload_annotated.
    let cwd = payload_cwd(&stdin).or_else(|| std::env::current_dir().ok());
    let set = core::rules::load(cwd.as_deref());
    let decision = hook_decision(&stdin, &agent, &set);
    let payload = core::agent::stamp_payload_annotated(
        &stdin,
        &agent,
        core::agent::now_secs(),
        decision.as_ref().map(|d| d.rule_id.as_str()),
    );
    let (deliver, resp_body) = deliver_event(&payload, creds.as_ref(), &kind);
    // Two stdout paths, at most one fires:
    // 1. PreToolUse rewrite (empty response = matched but this host does not write back via stdout: audit only, no output);
    // 2. SessionStart injection — when Core attached a capability digest and this host merges
    //    stdout additionalContext, surface it so the agent knows OpenCapX's abilities without
    //    calling list_capabilities first. Everything else stays zero-stdout (dumb pipe).
    let mut out = decision
        .filter(|d| !d.response.is_empty())
        .map(|d| d.response);
    if out.is_none() {
        out = cursor_noop_stdout(&agent, &stdin).map(str::to_string);
    }
    if out.is_none()
        && matches!(deliver, http::Deliver::Sent)
        && http::session_context_host(&agent)
        && is_session_start(&stdin)
    {
        if let Some(ctx) = resp_body
            .as_deref()
            .and_then(|b| serde_json::from_str::<serde_json::Value>(b).ok())
            .and_then(|v| {
                v.get("additionalContext")
                    .and_then(|c| c.as_str())
                    .map(String::from)
            })
        {
            out = Some(session_start_response(&agent, &ctx));
        }
    }
    if let Some(o) = out {
        println!("{}", o);
    }
    std::process::exit(0);
}

/// Claude-payload shape: the hook_event_name field says which host event fired.
/// Case-insensitive — cursor sends "sessionStart" (not injected today, but the check stays cheap).
fn is_session_start(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| {
            v.get("hook_event_name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .map(|n| n.eq_ignore_ascii_case("SessionStart"))
        .unwrap_or(false)
}

/// SessionStart stdout writeback. v1: the Claude-nested family (claude/codex/droid/grok — see
/// http::session_context_host) all speak the Claude shape; diverge per-host here the day one
/// differs, mirroring pre_tool_response.
fn session_start_response(_agent: &str, ctx: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": ctx,
        }
    })
    .to_string()
}

/// Rewrite pipeline: user rules first (they win), then the built-in danger guard — the only
/// path that can rewrite compound pipelines, since the rule engine refuses them by design.
/// Both the hook path and `opencapx rewrite` (used by the opencode plugin) go through here.
fn combined_rewrite(cmd: &str, set: &core::rules::RuleSet) -> core::rules::RewriteOutcome {
    use core::rules::{rewrite_command, RewriteOutcome, Stage};
    match rewrite_command(cmd, Stage::ToolPre, set) {
        r @ RewriteOutcome::Rewritten { .. } => r,
        RewriteOutcome::Unchanged => {
            // Stance: `OPEN_CAPX_DANGER_GUARD` (explicit per-invocation) over the durable
            // `opencapx guard mode` field in guard.json, default installer (installers need
            // the network and $HOME; strict is the network-denied fence).
            let settings = core::sandbox::resolve_guard_settings();
            // The guard's rewrite must call the stable shim (same rule as hook/MCP entries).
            let bin = hooks::ensure_shim()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "opencapx".into());
            if !settings.enabled {
                // Disabled (env var or guard.json): the idiom must not become invisible —
                // audit it as `danger/guard-disabled` with the command unchanged. The env
                // var in particular is the same injection surface that made project rules
                // trust-gated, so its effect stays on the record.
                return match core::sandbox::guard(cmd, &bin, settings.profile, settings.env) {
                    Some(_) => RewriteOutcome::Rewritten {
                        rule_id: "danger/guard-disabled".to_string(),
                        command: cmd.to_string(),
                    },
                    None => RewriteOutcome::Unchanged,
                };
            }
            match core::sandbox::guard(cmd, &bin, settings.profile, settings.env) {
                Some(hit) => RewriteOutcome::Rewritten {
                    rule_id: hit.rule_id.to_string(),
                    command: hit.command,
                },
                None => RewriteOutcome::Unchanged,
            }
        }
    }
}

/// `opencapx rewrite <command...>`: map a single command to its rule-rewritten form.
/// Match → print to stdout + `exit 0`; no match → `exit 1` (no output).
/// **Pure computation, does not execute the command** — execution is the caller's (agent's) responsibility.
fn run_rewrite(cmd: &str) -> ! {
    let set = core::rules::load(std::env::current_dir().ok().as_deref());
    match combined_rewrite(cmd, &set) {
        core::rules::RewriteOutcome::Rewritten { command, .. } => {
            println!("{command}");
            std::process::exit(0);
        }
        core::rules::RewriteOutcome::Unchanged => std::process::exit(1),
    }
}

fn run_wrap(args: &[String]) -> ! {
    let dash = args.iter().position(|a| a == "--");
    let cmd: Vec<String> = match dash {
        Some(i) => args[i + 1..].to_vec(),
        None => args.to_vec(),
    };
    if cmd.is_empty() {
        eprintln!("OpenCapX run: missing command after --");
        std::process::exit(2);
    }
    let start = serde_json::json!({"agent": "run", "text": format!("running {}", cmd.join(" "))});
    let run_kind = core::identity::detect_kind();
    let creds = core::identity::ensure_registered(&run_kind, "hook");
    deliver_event(
        &core::agent::stamp_payload(&start.to_string(), "run", core::agent::now_secs()),
        creds.as_ref(),
        &run_kind,
    );
    let mut child = match std::process::Command::new(&cmd[0]).args(&cmd[1..]).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("OpenCapX run: spawn failed: {}", e);
            std::process::exit(127);
        }
    };
    let status = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
    let done = serde_json::json!({"agent": "run", "text": "done"}).to_string();
    deliver_event(
        &core::agent::stamp_payload(&done, "run", core::agent::now_secs()),
        creds.as_ref(),
        &run_kind,
    );
    std::process::exit(status);
}

// ===== M2 signing toolchain CLI (keygen/pack/verify) =====
// WHY: authors need a self-contained signing/self-check toolchain, and the Rust CLI and Python SDK must agree on the same
// byte digest. pack/verify stdout emits only one line of machine-readable JSON (for SDK/CI parsing),
// human explanations and errors always go to stderr to avoid polluting the pipe; keygen is interactive and appends one
// suggested trusted-keys entry after the JSON line (same stdout, easy for authors to copy).

fn hex_encode_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Parse a 32-byte seed: 64 hex characters, or `@path` pointing to a file containing hex (after trim).
fn read_seed_arg(arg: &str) -> Result<[u8; 32], String> {
    let text = if let Some(path) = arg.strip_prefix('@') {
        std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read key file {}: {}", path, e))?
    } else {
        arg.to_string()
    };
    let text = text.trim();
    if text.len() != 64 {
        return Err(format!(
            "seed must be 64 hex characters, got {}",
            text.len()
        ));
    }
    let mut seed = [0u8; 32];
    for i in 0..32 {
        seed[i] = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("seed hex @{}: {}", i * 2, e))?;
    }
    Ok(seed)
}

fn run_keygen(out: &str) -> ! {
    let out_path = PathBuf::from(out);
    // WHY: the seed is the identity root; silently overwriting it would permanently lose the mapping between the old private key and published signed packages;
    // better to error and make the author explicitly rename/delete than to overwrite destructively.
    if out_path.exists() {
        eprintln!("keygen: {} already exists, refusing to overwrite (use a different --out or delete it first)", out);
        std::process::exit(1);
    }
    let mut seed = [0u8; 32];
    if let Err(e) = getrandom::getrandom(&mut seed) {
        eprintln!("keygen: failed to obtain randomness: {}", e);
        std::process::exit(1);
    }
    let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
    let pk_hex = hex_encode_lower(sk.verifying_key().as_bytes());
    if let Err(e) = std::fs::write(&out_path, format!("{}\n", hex_encode_lower(&seed))) {
        eprintln!("keygen: failed to write {}: {}", out, e);
        std::process::exit(1);
    }
    println!("{}", serde_json::json!({"publicKey": pk_hex, "out": out}));
    // keyId is chosen by the publisher (the CLI cannot know it); only suggest the entry shape for copying.
    println!(
        "suggested trusted-keys entry: {{\"<keyId>\":{{\"alg\":\"ed25519\",\"publicKey\":\"{}\"}}}}",
        pk_hex
    );
    std::process::exit(0);
}

fn run_pack(dir: &std::path::Path, key_arg: &str, key_id: &str, out: Option<&str>) -> ! {
    let manifest_path = dir.join("opencapx-plugin.json");
    let manifest_text = match std::fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("pack: failed to read {}: {}", manifest_path.display(), e);
            std::process::exit(1);
        }
    };
    let manifest: serde_json::Value = match serde_json::from_str(&manifest_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pack: manifest is invalid JSON: {}", e);
            std::process::exit(1);
        }
    };
    // WHY: Python (json) and serde_json (ryu) serialize floats/out-of-range integers differently, which would make
    // cross-language digests silently diverge; this is the pack boundary, nothing is on disk yet, so refuse and exit.
    if let Err(e) = core::signing::ensure_signable_numbers(&manifest) {
        eprintln!("pack: {}", e);
        std::process::exit(1);
    }
    let Some(id) = manifest
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
    else {
        eprintln!("pack: manifest is missing id");
        std::process::exit(1);
    };
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    let out = out
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}-{}.ocplugin", id, version));

    let seed = match read_seed_arg(&key_arg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pack: {}", e);
            std::process::exit(1);
        }
    };
    match core::pack::pack_dir(dir, &seed, &key_id, std::path::Path::new(&out)) {
        Ok(digest) => {
            println!(
                "{}",
                serde_json::json!({"out": out, "digest": digest, "keyId": key_id})
            );
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("pack: {}", e);
            std::process::exit(1);
        }
    }
}

fn run_verify(file: &str, trusted_keys: Option<&str>) -> ! {
    if let Some(tk) = trusted_keys {
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", tk);
    }
    let path = std::path::Path::new(file);
    // WHY: plugin_sig::verify conservatively returns Unsigned for unopenable files (install will block again),
    // but the CLI must distinguish "IO/format error (exit 1)" from "valid but unsigned (exit 2)", so self-check first.
    if !path.is_file() {
        eprintln!(
            "verify: file does not exist or is not a regular file: {}",
            file
        );
        std::process::exit(1);
    }
    match std::fs::File::open(path)
        .ok()
        .and_then(|f| zip::ZipArchive::new(f).ok())
    {
        Some(_) => {}
        None => {
            eprintln!(
                "verify: not a valid .ocplugin (zip) or cannot open: {}",
                file
            );
            std::process::exit(1);
        }
    }

    let outcome = core::plugin_sig::verify(path);
    let key_id = match &outcome {
        core::plugin_sig::VerifyOutcome::Trusted { key_id }
        | core::plugin_sig::VerifyOutcome::UnknownKey { key_id }
        | core::plugin_sig::VerifyOutcome::BadSignature { key_id } => Some(key_id.clone()),
        _ => None,
    };
    let mut obj = serde_json::Map::new();
    obj.insert(
        "status".into(),
        serde_json::Value::String(outcome.label().to_string()),
    );
    if let Some(kid) = &key_id {
        obj.insert("keyId".into(), serde_json::Value::String(kid.clone()));
    }
    println!("{}", serde_json::Value::Object(obj));
    eprintln!("verify: {} -> {}", file, outcome.label());

    use core::plugin_sig::VerifyOutcome as O;
    let code = match outcome {
        O::Trusted { .. } => 0,
        O::Unsigned | O::UnknownKey { .. } => 2,
        O::HashMismatch { .. } | O::BadSignature { .. } | O::MalformedSignature => 1,
    };
    std::process::exit(code);
}

/// M3 — sign the registry index with the official signer: inject/overwrite `indexSignature`.
/// Default output = index.json in the same directory as the input (hosting convention).
fn run_sign_index(input: &str, key_arg: &str, key_id: &str, out: Option<&str>) -> ! {
    let seed = match read_seed_arg(&key_arg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sign-index: {}", e);
            std::process::exit(1);
        }
    };
    let raw = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("sign-index: failed to read {}: {}", input, e);
            std::process::exit(1);
        }
    };
    let signed = match core::registry::sign_index(&raw, &seed, &key_id) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sign-index: {}", e);
            std::process::exit(1);
        }
    };
    let out = out.map(str::to_string).unwrap_or_else(|| {
        std::path::Path::new(input)
            .with_file_name("index.json")
            .display()
            .to_string()
    });
    if let Err(e) = std::fs::write(&out, &signed) {
        eprintln!("sign-index: failed to write {}: {}", out, e);
        std::process::exit(1);
    }
    println!("{}", serde_json::json!({"out": out, "keyId": key_id}));
    std::process::exit(0);
}

/// M3 — verify the registry index: official public keys = source constants ∪ `OPENCAPX_REGISTRY_OFFICIAL_KEYS`.
/// M6 — F10 automated gate (registry CI / local pre-run): full verification + JSON report + exit code.
fn run_verify_package(file: &str, keys: Option<&str>, index: Option<&str>) -> ! {
    let keys_path = keys
        .map(std::path::PathBuf::from)
        .unwrap_or_else(core::plugin_sig::trusted_keys_path);
    let keys = core::plugin_sig::load_trusted_keys_from(&keys_path);
    if keys.is_empty() {
        eprintln!(
            "verify-package: warning: {} has no valid trust entries (only the registry index is available)",
            keys_path.display()
        );
    }
    let index = match index {
        Some(p) => {
            let raw = match std::fs::read(&p) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("verify-package: failed to read index {}: {}", p, e);
                    std::process::exit(1);
                }
            };
            match core::registry::verify_index(&raw) {
                Ok(idx) => Some(idx),
                Err(e) => {
                    eprintln!("verify-package: index signature verification failed: {}", e);
                    println!(
                        "{}",
                        serde_json::json!({"ok": false, "checks": [{"id": "index", "ok": false, "detail": e}]})
                    );
                    std::process::exit(1);
                }
            }
        }
        None => None,
    };
    let report = core::verify::verify_package(
        std::path::Path::new(file),
        &keys,
        index.as_ref(),
        &core::verify::GateLimits::default(),
    );
    for c in &report.checks {
        eprintln!("{} {}: {}", if c.ok { "✓" } else { "✗" }, c.id, c.detail);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );
    std::process::exit(if report.ok { 0 } else { 1 });
}

fn run_verify_index(input: &str) -> ! {
    let raw = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("verify-index: failed to read {}: {}", input, e);
            std::process::exit(1);
        }
    };
    match core::registry::verify_index(&raw) {
        Ok(idx) => {
            println!(
                "{}",
                serde_json::json!({
                    "status": "valid",
                    "schemaVersion": idx.schema_version,
                    "generatedAt": idx.generated_at,
                    "publishers": idx.publishers.len(),
                    "entries": idx.entries.len(),
                    "revokedKeys": idx.revoked_keys.len(),
                })
            );
            eprintln!("verify-index: {} -> valid", input);
            std::process::exit(0);
        }
        Err(e) => {
            println!("{}", serde_json::json!({"status": "invalid"}));
            eprintln!("verify-index: {} -> {}", input, e);
            std::process::exit(1);
        }
    }
}

/// Human-facing CLI. Host-spawned entry points (`hook` / `mcp` / `run`) are dispatched before this
/// parse and keep lenient argument handling — see `main`.
#[derive(clap::Parser)]
#[command(
    name = "opencapx",
    version,
    about = "OpenCapX — the desktop body for AI agents (GUI + CLI)"
)]
struct Cli {
    /// Start with third-party plugins disabled (see Settings → General → Safe mode)
    #[arg(long)]
    safe_mode: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

/// One variant per user-facing subcommand. Commands whose detailed parsing still lives in their
/// `run_cli` take the remaining tokens verbatim, and `disable_help_flag` keeps their own `--help`
/// behavior unchanged until they are migrated.
#[derive(clap::Subcommand)]
enum Cmd {
    /// Write the agent's hook + MCP config (an unknown name lists the hosts)
    Connect { agent: String },
    /// Run a command behind the OS guard (seatbelt / bwrap)
    #[command(disable_help_flag = true)]
    Sandbox {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print the rewritten form of a command (does not execute it)
    Rewrite {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Command rules: list / explain / trust / untrust
    #[command(disable_help_flag = true)]
    Rules {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Danger-guard installer domains: trust / untrust / list / mode / env
    #[command(disable_help_flag = true)]
    Guard {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Automation rules: list / add / remove
    #[command(disable_help_flag = true)]
    Automation {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Install the `opencapx` command into PATH
    InstallCli {
        /// macOS: use the system authorization dialog when /usr/local/bin is not writable
        #[arg(long)]
        elevate: bool,
    },
    /// Remove the `opencapx` command from PATH
    UninstallCli {
        /// macOS: use the system authorization dialog when /usr/local/bin is not writable
        #[arg(long)]
        elevate: bool,
    },
    /// Generate a plugin signing key
    Keygen {
        /// Output path for the seed file
        #[arg(long, default_value = "opencapx-signing.key.hex")]
        out: String,
    },
    /// Package and sign a plugin directory
    Pack {
        /// Plugin directory to package
        dir: PathBuf,
        /// Signing seed: 64 hex chars, or @path to a file containing hex
        #[arg(long, value_name = "SEED|@FILE")]
        key: String,
        /// Publisher key id (goes into the signature)
        #[arg(long)]
        key_id: String,
        /// Output path (default: <id>-<version>.ocplugin)
        #[arg(long)]
        out: Option<String>,
    },
    /// Verify a signed plugin file
    Verify {
        /// The signed plugin file (.ocplugin)
        file: String,
        /// trusted-keys.json to verify against (default: the installed one)
        #[arg(long)]
        trusted_keys: Option<String>,
    },
    /// Verify a packed `.ocplugin` against trusted keys / the registry index
    VerifyPackage {
        /// The packed .ocplugin to verify
        file: String,
        /// trusted-keys.json (default: the installed one)
        #[arg(long)]
        keys: Option<String>,
        /// Registry index to consult
        #[arg(long)]
        index: Option<String>,
    },
    /// Sign a plugin registry index
    SignIndex {
        /// Unsigned index JSON
        input: String,
        /// Signing seed: 64 hex chars, or @path to a file containing hex
        #[arg(long, value_name = "SEED|@FILE")]
        key: String,
        /// Publisher key id (goes into the signature)
        #[arg(long)]
        key_id: String,
        /// Output path (default: index.json next to the input)
        #[arg(long)]
        out: Option<String>,
    },
    /// Verify a plugin registry index
    VerifyIndex {
        /// The index.json to verify
        input: String,
    },
}

fn main() {
    install_panic_hook();
    let raw: Vec<String> = std::env::args().collect();
    // host-spawned entry points keep lenient parsing (checked before clap): their configs are
    // written by other versions, and a strict parse error here would break the agent integration.
    if raw.len() > 1 {
        match raw[1].as_str() {
            "hook" => run_hook(&raw[1..]),
            "run" => run_wrap(&raw[1..]),
            "mcp" => mcp::run(),
            _ => {}
        }
    }
    // LaunchServices can hand the app legacy `-psn_0_…` arguments when it is opened from Finder;
    // clap would reject them and the GUI would never start.
    let argv: Vec<String> = raw
        .into_iter()
        .filter(|a| !a.starts_with("-psn_"))
        .collect();
    let cli = Cli::parse_from(argv);
    if cli.safe_mode {
        core::safe_mode::set_active(true);
        eprintln!("[core] safe mode active: third-party plugins will not start");
    }
    match cli.command {
        Some(Cmd::Connect { agent }) => run_connect(&agent),
        Some(Cmd::Sandbox { args }) => std::process::exit(core::sandbox::run_cli(&args)),
        Some(Cmd::Rewrite { command }) => run_rewrite(&command.join(" ")),
        Some(Cmd::Rules { args }) => std::process::exit(core::rules::run_cli(&args)),
        Some(Cmd::Guard { args }) => std::process::exit(core::sandbox::run_guard_cli(&args)),
        Some(Cmd::Automation { args }) => std::process::exit(core::automation::run_cli(&args)),
        Some(Cmd::InstallCli { elevate }) => run_install_cli(elevate),
        Some(Cmd::UninstallCli { elevate }) => run_uninstall_cli(elevate),
        Some(Cmd::Keygen { out }) => run_keygen(&out),
        Some(Cmd::Pack {
            dir,
            key,
            key_id,
            out,
        }) => run_pack(&dir, &key, &key_id, out.as_deref()),
        Some(Cmd::Verify { file, trusted_keys }) => run_verify(&file, trusted_keys.as_deref()),
        Some(Cmd::VerifyPackage { file, keys, index }) => {
            run_verify_package(&file, keys.as_deref(), index.as_deref())
        }
        Some(Cmd::SignIndex {
            input,
            key,
            key_id,
            out,
        }) => run_sign_index(&input, &key, &key_id, out.as_deref()),
        Some(Cmd::VerifyIndex { input }) => run_verify_index(&input),
        None => {}
    }

    // Phase 46 — profile startup flow: migrate the old flat DB → read the active profile → open the corresponding store.
    let _ = core::profile::ensure_default_profile_migrated();
    let active = core::profile::active_profile_name();
    // corrupt-DB quarantine: a DB that cannot open/fails validation is renamed for evidence, a fresh empty DB takes over, and a notice is left for the settings page —
    // never silently run in memory (that would make the app look fine while persisting nothing).
    let (store_kind, db_quarantine) = core::profile::open_profile_store(&active);
    if let Some(q) = &db_quarantine {
        eprintln!(
            "[profile] db quarantined for profile {} ({}): {} → {}",
            active, q.reason, q.from, q.to
        );
        core::profile::write_db_recovery_notice(q);
    }
    let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(store_kind));
    let store_for_setup = store.clone();
    let bus = core::event::EventBus::shared();
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    if event.state != ShortcutState::Pressed {
                        return;
                    }
                    let combo = shortcut.clone().into_string();
                    // file is the source of truth; disabled bindings are already unregistered at the OS layer, with an extra enabled guard here as a fallback.
                    // read the file once per keypress — a low-frequency operation, accept this cost (see the task header comment).
                    let actions: Vec<core::hotkey::HotkeyAction> = core::hotkey::store()
                        .list()
                        .into_iter()
                        .filter(|b| {
                            b.enabled
                                && core::hotkey::normalize_combo(&b.combo)
                                    == core::hotkey::normalize_combo(&combo)
                        })
                        .map(|b| b.action)
                        .collect();
                    for action in actions {
                        dispatch_hotkey_action(app, action);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(store)
        .setup(move |app| {
            use tauri::Manager;
            let handle = app.handle().clone();
            // Stable-CLI maintenance: refresh the ~/.opencapx/bin shim and repoint agent
            // configs still referencing a dead binary path (dev checkout moved, cargo clean,
            // app reinstalled elsewhere). Pure file surgery, idempotent, no-ops when current.
            let fixed = hooks::refresh_installations();
            if fixed > 0 {
                let noun = if fixed == 1 { "entry" } else { "entries" };
                eprintln!(
                    "[core] repaired {} config {} (stable CLI shim / codex identity env)",
                    fixed, noun
                );
            }
            app.manage(TrayState {
                pet_check: std::sync::Mutex::new(None),
                bubble_check: std::sync::Mutex::new(None),
                signature: std::sync::Mutex::new(String::new()),
            });
            if let Some(tray) = app.tray_by_id("main") {
                tray.on_menu_event(|app_handle, event| match event.id.as_ref() {
                    "clear-finished" => {
                        clear_finished_sessions();
                        let _ = refresh_tray_menu(app_handle);
                    }
                    "toggle-pet" => {
                        if let Some(w) = app_handle.get_webview_window("pet") {
                            let next = !w.is_visible().unwrap_or(true);
                            if next {
                                let _ = w.show();
                            } else {
                                let _ = w.hide();
                            }
                            set_pet_visible(app_handle, next);
                        }
                    }
                    "toggle-bubble" => {
                        let next = !read_settings_file()
                            .get("bubbleEnabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        set_bubble_visible(app_handle, next);
                    }
                    "install-cli" => {
                        // the authorization dialog blocks — run it off the main thread, then rebuild
                        // the menu so a successful install drops the item on the next signature check.
                        let handle = app_handle.clone();
                        std::thread::spawn(move || {
                            match cli_install::install(true) {
                                Ok(msg) => eprintln!("[tray] {msg}"),
                                Err(e) => eprintln!("[tray] install-cli failed: {e}"),
                            }
                            let _ = refresh_tray_menu(&handle);
                        });
                    }
                    "open-settings" => show_settings(app_handle),
                    "quit" => app_handle.exit(0),
                    _ => {}
                });
            }
            for payload in queue::drain(&http::queue_dir()).unwrap_or_default() {
                core::event::ingest(
                    Some(&handle),
                    &store_for_setup,
                    &bus,
                    &payload,
                    "unknown",
                    None,
                );
            }
            core::event::spawn_subscribers(handle.clone(), store_for_setup.clone(), bus.clone());
            core::subscriber::spawn_fanout(bus.clone(), core::plugin::PluginManager::shared());
            core::pet::spawn_state_mapper(bus.clone());
            core::plugin::spawn_auto_reload_poller();
            core::revocation::spawn_watch();
            core::plugin_metrics::spawn_monitor();
            core::alerting::spawn_dispatcher();
            core::alerting::spawn_retry_loop();
            core::alerting::spawn_escalation_loop();
            core::sla::spawn_sla_monitor(store_for_setup.clone(), bus.clone());
            core::retention::spawn_retention_worker();
            core::set_shared_store(store_for_setup.clone());
            core::set_app_handle(handle.clone());
            // the tray menu must be built after shared_store is ready: menu content reads active sessions directly.
            let _ = refresh_tray_menu(&handle);
            {
                // session change → rebuild the tray menu (content-signature debounced; no rebuild when unchanged).
                let tray_app = handle.clone();
                let tray_bus = bus.clone();
                std::thread::spawn(move || {
                    let mut rx = tray_bus.subscribe();
                    while let Ok(e) = rx.recv() {
                        if !(e.kind.starts_with("agent.")
                            || e.kind.starts_with("pet.")
                            || e.kind.starts_with("permission."))
                        {
                            continue;
                        }
                        let _ = refresh_tray_menu(&tray_app);
                    }
                });
            }
            core::hotkey::set_shortcut_app(handle.clone());
            // one-time migration: SQLite old rows → hotkeys.json (skipped if the file exists; table untouched).
            {
                let rows = match store_for_setup.lock() {
                    Ok(g) => g.list_hotkeys(),
                    Err(_) => Vec::new(),
                };
                let n = core::hotkey::migrate_from_sqlite(&rows);
                if n > 0 {
                    eprintln!("hotkey: migrated {} bindings from sqlite to json", n);
                }
            }
            // register all enabled bindings; failures only log, and the badge is surfaced by list_hotkeys.
            for dto in core::hotkey::store().list() {
                if dto.enabled {
                    if let Err(e) = core::hotkey::register_at_os(&dto.combo) {
                        eprintln!("hotkey: register {} at startup failed: {}", dto.combo, e);
                    }
                }
            }
            // restore installed plugins: start in dependency-topology order, bringing up only those running at last exit.
            // skipped entirely under safe mode (a manual start is also rejected by start_inner's gatekeeper),
            // emits an event so the Activity Timeline explains why plugins did not start.
            if core::safe_mode::is_active() {
                bus.publish(&core::event::OpencapxEvent::new(
                    "core.safe_mode",
                    "core",
                    serde_json::json!({ "active": true, "note": "plugin restore skipped" }),
                ));
                eprintln!("[plugin] restore skipped: safe mode active");
            } else {
                let mgr = core::plugin::PluginManager::shared();
                let plan = core::lifecycle_order::compute_lifecycle_plan(&mgr);
                let running: std::collections::BTreeSet<String> = mgr
                    .list()
                    .into_iter()
                    .filter(|p| p.status == "running")
                    .map(|p| p.id)
                    .collect();
                let (started, errors) =
                    core::lifecycle_order::start_subset_in_order(&mgr, &plan, &running);
                if started > 0 || !errors.is_empty() {
                    eprintln!(
                        "[plugin] restore: {} started, {} error(s)",
                        started,
                        errors.len()
                    );
                }
            }
            // when launched from a terminal (dev/hot-reload), macOS may assign the window outside the current Space,
            // and the user sees no window on the current desktop. The pet is pinned to all Spaces.
            if let Some(w) = app.get_webview_window("pet") {
                let _ = w.set_visible_on_all_workspaces(true);
                let _ = w.show();
            }
            spawn_pet_hover_tracker(handle.clone());
            // state-machine retention: periodically archive expired sessions into session_archive and clear them from the active table.
            {
                let sweep_store = store_for_setup.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                    let now = core::agent::now_secs();
                    if let Ok(mut s) = sweep_store.lock() {
                        let n = s.sweep(now);
                        if n > 0 {
                            eprintln!("[sessions] archived {} expired session(s)", n);
                        }
                        // archives are kept 90 days, prune expired ones along the way
                        let dropped = s.prune_session_archive(now);
                        if dropped > 0 {
                            eprintln!("[sessions] pruned {} archived session(s)", dropped);
                        }
                    }
                });
            }
            std::thread::spawn(move || http::serve(handle, store_for_setup, bus));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_sessions,
            project_meta,
            dismiss_session,
            clear_sessions,
            list_pet_packs,
            import_pet_pack,
            import_pet_pack_from_url,
            delete_pet_pack,
            read_pet_sheet,
            read_pet_model,
            read_pet_asset,
            get_settings,
            set_settings,
            ui_clipboard_write,
            get_agents,
            toggle_agent,
            open_settings,
            ui_ping,
            answer_ask,
            answer_permission,
            answer_install,
            list_plugins,
            install_plugin,
            install_ocplugin,
            preview_ocplugin,
            install_marketplace,
            list_marketplace,
            check_plugin_updates,
            registry_status,
            registry_refresh,
            preview_update,
            reopen_plugin,
            get_allow_unsigned,
            set_allow_unsigned,
            get_sandbox_enforcement,
            set_sandbox_enforcement,
            rate_plugin,
            list_plugin_ratings,
            plugin_rating_summary,
            list_capability_stats,
            list_audit,
            search_audit_events,
            timeline_events,
            list_notifications,
            list_automation_rules,
            add_automation_rule,
            remove_automation_rule,
            set_automation_rule_enabled,
            list_rules,
            set_rule_enabled,
            add_rule,
            list_trusted_projects,
            trust_project,
            untrust_project,
            toggle_plugin,
            uninstall_plugin,
            preview_uninstall_plugin,
            list_permissions,
            set_permission,
            agents_list,
            agent_revoke,
            agent_reauthorize,
            agent_set_permission,
            agent_permissions,
            core_permission_list,
            core_permission_set,
            core_permission_reset,
            set_plugin_auto_reload,
            list_plugin_lifecycle,
            list_plugin_traces,
            list_plugin_dependency_graph,
            list_permission_heatmap,
            get_plugin_lifecycle_plan,
            start_all_plugins,
            stop_all_plugins,
            enable_kill_switch,
            disable_kill_switch,
            get_kill_switch_state,
            get_safe_mode_state,
            cli_command_status,
            cli_command_install,
            cli_command_uninstall,
            get_plugin_metrics,
            get_plugin_metrics_history,
            get_metrics_config,
            set_metrics_config,
            list_workspace_profiles,
            create_workspace_profile,
            switch_workspace_profile,
            delete_workspace_profile,
            search_logs,
            list_replay_sessions,
            get_replay_events,
            replay_session_to_stream,
            get_sla_config,
            set_sla_config,
            list_sla_violations,
            run_plugin_probe,
            get_probe_report,
            set_plugin_channel,
            get_default_channel,
            set_default_channel,
            get_plugin_health_config,
            set_plugin_health_config,
            create_backup,
            list_backups,
            restore_backup,
            delete_backup,
            get_alerting_config,
            set_alerting_config,
            test_alerting_webhook,
            list_alerting_failed,
            retry_alerting_failed,
            delete_alerting_failed,
            clear_alerting_resolved,
            get_alerting_retry_config,
            set_alerting_retry_config,
            list_alerting_endpoints,
            save_alerting_endpoint,
            delete_alerting_endpoint,
            test_alerting_endpoint,
            preview_alerting_template,
            list_alerting_template_presets,
            get_alerting_template_preset,
            save_alerting_template_preset,
            delete_alerting_template_preset,
            export_alerting_presets,
            import_alerting_presets,
            write_text_file,
            read_text_file,
            fork_alerting_template_preset,
            export_alerting_bundle,
            import_alerting_bundle,
            rotate_alerting_bundle_secret,
            list_alerting_silences,
            save_alerting_silence,
            delete_alerting_silence,
            list_alerting_acks,
            ack_alerting_kind,
            delete_alerting_ack,
            clear_expired_alerting_acks,
            list_alerting_recipients,
            save_alerting_recipient,
            delete_alerting_recipient,
            test_alerting_recipient_by_id,
            detect_alerting_cycles,
            recent_alerting_events,
            severity_inheritance_chain,
            delete_alerting_severity_hint_cascade,
            severity_propagation_trace,
            preview_alerting_endpoint_severity,
            simulate_alerting_dispatch,
            list_alerting_routes,
            save_alerting_route,
            delete_alerting_route,
            import_alerting_routes_yaml,
            export_alerting_routes_yaml,
            dry_run_alerting_route,
            list_alerting_severity_hints,
            save_alerting_severity_hint,
            delete_alerting_severity_hint,
            clear_alerting_severity_hints,
            list_alerting_aggregations,
            save_alerting_aggregation,
            delete_alerting_aggregation,
            clear_alerting_aggregations,
            list_alerting_correlations,
            save_alerting_correlation,
            delete_alerting_correlation,
            clear_alerting_correlations,
            list_alerting_escalations,
            save_alerting_escalation,
            delete_alerting_escalation,
            clear_alerting_escalations,
            list_hotkeys,
            set_hotkey,
            set_hotkey_enabled,
            delete_hotkey,
            list_palette_entries,
            get_plugin_trace,
            get_rpc_trace,
            get_hook_trace,
            list_all_traces,
            list_all_hook_sessions,
            export_project_chains,
            list_plugin_config,
            get_plugin_config,
            set_plugin_config,
            delete_plugin_config,
            list_plugin_settings,
            read_plugin_readme,
            invoke_plugin_setting_list,
            db_recovery_notice,
            dismiss_db_recovery_notice,
            set_plugin_setting,
            invoke_plugin_setting_action
        ])
        .run(tauri::generate_context!())
        .expect("OpenCapX failed to run");
}

/// Serializes tests that flip the process-global guard env/file (danger-guard stance).
/// Both the sandbox guard-config tests and hook_decision_danger_guard_path mutate
/// OPEN_CAPX_DANGER_GUARD / OPEN_CAPX_GUARD_FILE; without this lock they race.
#[cfg(test)]
pub(crate) static GUARD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_dto_roundtrip() {
        let dto = core::agent::process_body(
            r#"{"agent":"codex","text":"running tool","project":"/x/demo"}"#,
            "unknown",
        );
        let s = core::agent::dto_to_session(&dto);
        let back = core::agent::session_to_dto(&s);
        assert_eq!(back.id, dto.id);
        assert_eq!(back.agent, "codex");
        assert_eq!(back.state, dto.state);
        assert_eq!(back.project, "demo");
    }

    #[test]
    fn commands_default_settings_shape() {
        let v = default_settings();
        assert_eq!(v["mode"], "carousel");
        assert_eq!(v["locale"], "en");
    }

    /// 2026-09-18 hotkey redesign — HotkeyResult serde shape (frontend contract).
    #[test]
    fn hotkey_result_shape() {
        let r = core::hotkey::HotkeyResult {
            binding: core::hotkey::HotkeyDto {
                combo: "CmdOrCtrl+Shift+P".into(),
                action: core::hotkey::HotkeyAction::Builtin {
                    action: core::hotkey::BuiltinAction::OpenPalette,
                },
                enabled: true,
                registered_at_os: false,
            },
            registered: false,
            error: Some("Already registered".into()),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["binding"]["combo"], "CmdOrCtrl+Shift+P");
        assert_eq!(v["binding"]["enabled"], true);
        assert_eq!(v["registered"], false);
        assert_eq!(v["error"], "Already registered");
        let ok = core::hotkey::HotkeyResult { error: None, ..r };
        assert!(serde_json::to_value(&ok).unwrap().get("error").is_none());
    }

    /// i2 §13 — list_notifications: notification.posted events → DTO + agent filter.
    /// Uses the global shared_store and holds TEST_STORE_LOCK to serialize (same as the plugin test pattern).
    #[test]
    fn notifications_list_maps_events_and_filters_by_agent() {
        let dir = std::env::temp_dir().join(format!("opencapx-notif-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store: core::storage::SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            core::storage::StoreEnum::Db(core::storage::Storage::open(&dir.join("t.db")).unwrap()),
        ));
        let _g = core::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        core::set_shared_store(store.clone());
        {
            let mut s = store.lock().unwrap();
            s.log_event(&core::event::OpencapxEvent::new(
                "notification.posted",
                "mcp",
                serde_json::json!({ "agentId": "claude-main", "title": "Build", "body": "done", "severity": "info" }),
            ));
            s.log_event(&core::event::OpencapxEvent::new(
                "notification.posted",
                "mcp",
                serde_json::json!({ "agentId": "server-agent", "title": "CPU", "body": ">90%", "severity": "warn" }),
            ));
            // noise: other kinds must not be pulled in by the notification prefix query
            s.log_event(&core::event::OpencapxEvent::new(
                "plugin.log",
                "core",
                serde_json::json!({ "pluginId": "x" }),
            ));
        }
        let all = list_notifications(None, None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].agent, "server-agent"); // descending, newest first
        assert_eq!(all[0].severity, "warn");
        assert_eq!(all[1].title, "Build");
        let filtered = list_notifications(None, Some("claude".into()));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].agent, "claude-main");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timeline_whitelist_and_category() {
        // included: action events with audit value
        for k in [
            "agent.started",
            "capability.completed",
            "permission.denied",
            "auth.rejected",
            "plugin.installed",
            "pet.say",
            "workspace.switched",
            "notification.posted",
            "capability.subscribed",
            "capability.unsubscribed",
            "automation.rule_fired",
        ] {
            assert!(
                timeline_kind_allowed(k),
                "{} should be included in Timeline",
                k
            );
        }
        // excluded noise: heartbeat / high-frequency / log kinds
        for k in [
            "capability.started",
            "capability.event",
            "plugin.log",
            "plugin.metrics.cpu",
            "plugin.lifecycle.starting",
            "plugin.lifecycle.running",
            "pet.state",
            "nope.nope",
        ] {
            assert!(!timeline_kind_allowed(k), "{} should not enter Timeline", k);
        }
        // prefix → category
        assert_eq!(timeline_category("agent.started"), "agent");
        assert_eq!(timeline_category("capability.failed"), "capability");
        assert_eq!(timeline_category("permission.requested"), "permission");
        assert_eq!(timeline_category("auth.rejected"), "security");
        assert_eq!(timeline_category("plugin.installed"), "plugin");
        assert_eq!(timeline_category("pet.say"), "pet");
        assert_eq!(timeline_category("notification.posted"), "notification");
        assert_eq!(timeline_category("automation.rule_fired"), "system");
        assert_eq!(timeline_category("whatever.else"), "system");
    }

    #[test]
    fn commands_stats_counts() {
        let sessions = vec![
            core::agent::Session {
                id: "a".into(),
                agent: "claude".into(),
                project: "p".into(),
                cwd: String::new(),
                message: "".into(),
                state: core::agent::AgentState::Working,
                updated_at: 86400 * 10 + 5,
                started_at: 86400 * 10,
                model: String::new(),
                speech: String::new(),
                choices: None,
                answered: None,
            },
            core::agent::Session {
                id: "b".into(),
                agent: "codex".into(),
                project: "q".into(),
                cwd: String::new(),
                message: "".into(),
                state: core::agent::AgentState::Done,
                updated_at: 86400 * 10 + 6,
                started_at: 86400 * 10,
                model: String::new(),
                speech: String::new(),
                choices: None,
                answered: None,
            },
            core::agent::Session {
                id: "c".into(),
                agent: "claude".into(),
                project: "p".into(),
                cwd: String::new(),
                message: "".into(),
                state: core::agent::AgentState::Done,
                updated_at: 86400 * 9 + 6,
                started_at: 86400 * 10,
                model: String::new(),
                speech: String::new(),
                choices: None,
                answered: None,
            },
        ];
        let st = compute_stats(&sessions, 86400 * 10 + 100);
        assert_eq!(st.total, 3);
        assert_eq!(st.today, 2);
        assert_eq!(st.by_agent["claude"], 2);
    }

    /// panic hook end-to-end: install the hook → trigger a catchable panic → crash.log should have a record.
    /// the hook is process-global; save/restore prevents polluting other tests. env needs mutex for the same reason as
    /// retention's traces_env_lock; the same lock serializes here.
    #[test]
    fn panic_hook_writes_crash_log() {
        let _g = core::plugin_trace::traces_env_lock();
        let dir = std::env::temp_dir().join(format!("ocx-crash-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("crash.log");
        std::env::set_var("OPENCAPX_CRASH_LOG", &log);

        let prev = std::panic::take_hook();
        install_panic_hook();
        let _ = std::panic::catch_unwind(|| panic!("hook-probe-boom"));
        std::panic::set_hook(prev);

        let text = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(text.contains("panicked"), "crash record missing: {text:?}");
        assert!(
            text.contains("hook-probe-boom"),
            "payload missing: {text:?}"
        );
        std::env::remove_var("OPENCAPX_CRASH_LOG");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn rw_set() -> core::rules::RuleSet {
        core::rules::ruleset_from_json(
            r#"{"version":1,"rules":[{"id":"sandbox-curl",
                "when":{"stage":"tool_pre","command":{"prefix":"curl "}},
                "then":{"action":"rewrite","prepend":"sandbox"}}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn hook_rewrites_pretooluse_command() {
        let input = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash",
            "tool_input":{"command":"curl https://x"}}"#;
        let out = hook_response(input, "claude", &rw_set()).expect("should rewrite");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(
            v["hookSpecificOutput"]["updatedInput"]["command"],
            "sandbox curl https://x"
        );
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    #[test]
    fn hook_preserves_other_tool_input_fields() {
        let input = r#"{"hook_event_name":"PreToolUse",
            "tool_input":{"command":"curl x","timeout":1000}}"#;
        let out = hook_response(input, "claude", &rw_set()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["updatedInput"]["timeout"], 1000);
    }

    #[test]
    fn hook_emits_nothing_on_non_pretooluse() {
        assert!(hook_response(r#"{"hook_event_name":"Stop"}"#, "claude", &rw_set()).is_none());
    }

    #[test]
    fn hook_emits_nothing_for_unsupported_agent() {
        // cursor is now supported too (see rewrite_host); use an agent genuinely absent from the rewrite host table.
        let input = r#"{"hook_event_name":"PreToolUse","tool_input":{"command":"curl x"}}"#;
        assert!(hook_response(input, "windsurf", &rw_set()).is_none());
    }

    #[test]
    fn hook_emits_nothing_when_no_rule_matches() {
        let input = r#"{"hook_event_name":"PreToolUse","tool_input":{"command":"ls /tmp"}}"#;
        assert!(hook_response(input, "claude", &rw_set()).is_none());
    }

    #[test]
    fn hook_emits_nothing_for_non_bash_tool() {
        let input = r#"{"hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{"file_path":"/x"}}"#;
        assert!(hook_response(input, "claude", &rw_set()).is_none());
    }

    #[test]
    fn payload_cwd_reads_cwd_field() {
        assert_eq!(
            payload_cwd(r#"{"cwd":"/tmp/proj"}"#).as_deref(),
            Some(std::path::Path::new("/tmp/proj"))
        );
        assert!(payload_cwd(r#"{}"#).is_none());
    }

    #[test]
    fn payload_host_detects_opencode_source() {
        assert_eq!(
            payload_host(r#"{"hook_source":"opencode-plugin"}"#),
            Some("opencode")
        );
        assert_eq!(
            payload_host(r#"{"hook_source":"OpenCode-Plugin"}"#),
            Some("opencode"),
            "case-insensitive"
        );
        assert_eq!(payload_host(r#"{"hook_source":"claude-code"}"#), None);
        assert_eq!(payload_host(r#"{}"#), None);
        assert_eq!(payload_host("not json"), None);
    }

    #[test]
    fn rewrite_host_distinguishes_emit_from_apply() {
        assert_eq!(rewrite_host("claude"), Some(true));
        assert_eq!(rewrite_host("codex"), Some(true));
        assert_eq!(
            rewrite_host("opencode"),
            Some(false),
            "opencode rewrites but does not write back"
        );
        assert_eq!(
            rewrite_host("gemini"),
            Some(true),
            "gemini honors tool_input writeback"
        );
        assert_eq!(
            rewrite_host("omp"),
            Some(true),
            "omp's extension consumes the stdout rewrite and applies it as {{ input }}"
        );
        assert_eq!(
            rewrite_host("droid"),
            Some(true),
            "droid honors hookSpecificOutput.updatedInput"
        );
        assert_eq!(
            rewrite_host("copilot"),
            Some(true),
            "copilot CLI honors updatedInput on the PascalCase PreToolUse event"
        );
        assert_eq!(
            rewrite_host("cursor"),
            Some(true),
            "cursor honors the top-level updated_input envelope"
        );
        assert_eq!(
            rewrite_host("windsurf"),
            None,
            "unsupported host neither writes nor audits"
        );
    }

    #[test]
    fn is_pre_tool_event_accepts_claude_and_gemini_names() {
        assert!(is_pre_tool_event("PreToolUse"));
        assert!(is_pre_tool_event("pretooluse"));
        assert!(
            is_pre_tool_event("BeforeTool"),
            "gemini's pre-execution event name"
        );
        assert!(!is_pre_tool_event("AfterTool"));
        assert!(!is_pre_tool_event("Stop"));
    }

    #[test]
    fn gemini_response_uses_tool_input_not_updated_input() {
        let ti = serde_json::json!({ "command": "/tmp/w curl x" });
        let v: serde_json::Value =
            serde_json::from_str(&pre_tool_response("gemini", "r1", &ti)).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "BeforeTool");
        assert_eq!(
            v["hookSpecificOutput"]["tool_input"]["command"],
            "/tmp/w curl x"
        );
        assert!(
            v["hookSpecificOutput"].get("updatedInput").is_none(),
            "gemini does not recognize updatedInput"
        );
    }

    #[test]
    fn claude_response_uses_updated_input() {
        let ti = serde_json::json!({ "command": "/tmp/w curl x" });
        let v: serde_json::Value =
            serde_json::from_str(&pre_tool_response("claude", "r1", &ti)).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["updatedInput"]["command"],
            "/tmp/w curl x"
        );
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "allow");
        assert!(v["hookSpecificOutput"].get("tool_input").is_none());
    }

    #[test]
    fn gemini_before_tool_hit_emits_gemini_response() {
        let input = r#"{"hook_event_name":"BeforeTool","tool_input":{"command":"curl https://x"}}"#;
        let d = hook_decision(input, "gemini", &rw_set()).expect("gemini should produce a rewrite");
        assert_eq!(d.rule_id, "sandbox-curl");
        assert!(
            d.response.contains("tool_input"),
            "must use gemini's field name"
        );
    }

    #[test]
    fn omp_hit_emits_claude_shape_writeback() {
        let input = r#"{"hook_event_name":"PreToolUse","tool_input":{"command":"curl https://x"}}"#;
        let d =
            hook_decision(input, "omp", &rw_set()).expect("a match must report rule_id for audit");
        assert_eq!(d.rule_id, "sandbox-curl");
        let v: serde_json::Value = serde_json::from_str(&d.response).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["updatedInput"]["command"],
            "sandbox curl https://x"
        );
    }

    #[test]
    fn cursor_response_uses_top_level_envelope() {
        let ti = serde_json::json!({ "command": "/tmp/w curl x" });
        let v: serde_json::Value =
            serde_json::from_str(&pre_tool_response("cursor", "r1", &ti)).unwrap();
        assert_eq!(v["permission"], "allow");
        assert_eq!(v["continue"], true);
        assert_eq!(v["updated_input"]["command"], "/tmp/w curl x");
        assert!(
            v.get("hookSpecificOutput").is_none(),
            "cursor does not read hookSpecificOutput"
        );
    }

    #[test]
    fn cursor_hit_emits_top_level_writeback() {
        let input = r#"{"hook_event_name":"preToolUse","tool_input":{"command":"curl https://x"}}"#;
        let d = hook_decision(input, "cursor", &rw_set())
            .expect("a match must report rule_id for audit");
        assert_eq!(d.rule_id, "sandbox-curl");
        let v: serde_json::Value = serde_json::from_str(&d.response).unwrap();
        assert_eq!(v["updated_input"]["command"], "sandbox curl https://x");
        assert_eq!(v["permission"], "allow");
    }

    #[test]
    fn cursor_pretool_noop_answers_valid_json() {
        let p = r#"{"hook_event_name":"preToolUse","tool_input":{"command":"echo hi"}}"#;
        assert_eq!(cursor_noop_stdout("cursor", p), Some("{}"));
        assert_eq!(
            cursor_noop_stdout("cursor", r#"{"hook_event_name":"sessionStart"}"#),
            None
        );
        assert_eq!(
            cursor_noop_stdout("claude", p),
            None,
            "other hosts stay zero-stdout"
        );
    }

    #[test]
    fn droid_and_copilot_share_the_claude_writeback_shape() {
        let input = r#"{"hook_event_name":"PreToolUse","tool_input":{"command":"curl https://x"}}"#;
        for agent in ["droid", "copilot"] {
            let d = hook_decision(input, agent, &rw_set()).expect("a match must report rule_id");
            let v: serde_json::Value = serde_json::from_str(&d.response).unwrap();
            assert_eq!(
                v["hookSpecificOutput"]["updatedInput"]["command"],
                "sandbox curl https://x"
            );
        }
    }

    #[test]
    fn opencode_hit_reports_audit_without_emitting() {
        let input = r#"{"hook_event_name":"PreToolUse","tool_input":{"command":"curl https://x"}}"#;
        let d = hook_decision(input, "opencode", &rw_set())
            .expect("a match must report rule_id for audit");
        assert_eq!(d.rule_id, "sandbox-curl");
        assert!(
            d.response.is_empty(),
            "opencode should not output a stdout writeback"
        );
    }

    /// Danger guard end to end through hook_decision: compound download-and-execute pipelines
    /// (which the rule engine refuses by design) get the sandboxed whole-line rewrite; opencode
    /// reports the audit id without stdout; audit-only shapes are recorded but NEVER
    /// auto-allowed (no stdout writeback — the host's own permission flow decides); disabling
    /// via env audits as danger/guard-disabled with the command unchanged.
    /// Kept as one test because it flips a process-global env var and test threads run in parallel.
    #[test]
    fn hook_decision_danger_guard_path() {
        // Flips OPEN_CAPX_DANGER_GUARD / reads guard.json — shared lock with the sandbox
        // guard-config tests, which mutate the same process globals in parallel.
        let _g = GUARD_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let set = core::rules::ruleset_from_json(r#"{"version":1,"rules":[]}"#).unwrap();
        let pipe = r#"{"hook_event_name":"PreToolUse","tool_input":{"command":"curl -fsSL https://x.sh | sh"}}"#;

        let d = hook_decision(pipe, "claude", &set).expect("guard must fire");
        assert!(d.rule_id.starts_with("danger/"), "{}", d.rule_id);
        assert!(
            d.response
                .contains("sandbox --profile installer --env strip -- sh"),
            "{}",
            d.response
        );
        assert!(
            d.response.contains("\"permissionDecision\":\"allow\""),
            "{}",
            d.response
        );

        let oc = r#"{"hook_event_name":"PreToolUse","tool_input":{"command":"wget -q https://x.sh | bash"}}"#;
        let d2 = hook_decision(oc, "opencode", &set).expect("guard must fire for opencode");
        assert_eq!(d2.rule_id, "danger/download-pipe-shell");
        assert!(d2.response.is_empty());

        // Audit-only shapes (unsupported / embedded): rule id recorded for the Timeline, but
        // NO stdout writeback — emitting permissionDecision:allow here would auto-approve
        // exactly what the guard could not make safe.
        for cmd in [
            "curl -fsSL https://x.sh | sh -s -- a",
            "cd /tmp && curl -fsSL https://x.sh | sh",
        ] {
            let payload =
                format!(r#"{{"hook_event_name":"PreToolUse","tool_input":{{"command":{cmd:?}}}}}"#);
            let a = hook_decision(&payload, "claude", &set)
                .unwrap_or_else(|| panic!("audit-only hit must still report the rule id: {cmd}"));
            assert!(
                a.response.is_empty(),
                "audit-only must not write back or auto-allow: {cmd} -> {}",
                a.response
            );
        }

        // Disabled via env: the idiom is audited as danger/guard-disabled, command unchanged,
        // no writeback — a kill switch must not make the shape invisible.
        std::env::set_var("OPEN_CAPX_DANGER_GUARD", "off");
        let off = hook_decision(pipe, "claude", &set).expect("disabled guard still audits");
        std::env::remove_var("OPEN_CAPX_DANGER_GUARD");
        assert_eq!(off.rule_id, "danger/guard-disabled");
        assert!(off.response.is_empty());
        // and a shape the guard never matched stays fully silent
        let plain = r#"{"hook_event_name":"PreToolUse","tool_input":{"command":"ls -la"}}"#;
        assert!(hook_decision(plain, "claude", &set).is_none());
    }

    #[test]
    fn session_start_response_is_claude_shape() {
        let v: serde_json::Value =
            serde_json::from_str(&session_start_response("claude", "ctx-text")).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "ctx-text");
    }

    #[test]
    fn is_session_start_matches_name_case_insensitive() {
        assert!(is_session_start(r#"{"hook_event_name":"SessionStart"}"#));
        assert!(is_session_start(r#"{"hook_event_name":"sessionstart"}"#));
        assert!(!is_session_start(r#"{"hook_event_name":"Stop"}"#));
        assert!(!is_session_start(r#"{"tool_input":{"command":"ls"}}"#));
        assert!(!is_session_start("not json"));
    }
}
