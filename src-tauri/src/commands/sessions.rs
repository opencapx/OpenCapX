//! session commands + Stats aggregation.
//! Mechanical move from main.rs.

use super::*;

#[tauri::command]
pub(crate) fn get_sessions(store: tauri::State<SharedStore>) -> Vec<SessionDto> {
    let now = now_secs();
    match store.lock() {
        Ok(s) => {
            let mut rows = s.active(now);
            crate::core::agent::sort_sessions(&mut rows);
            rows.iter().map(session_to_dto).collect()
        }
        Err(_) => Vec::new(),
    }
}

/// Project metadata for the bubble group header (git branch / short path). Batched and deduped;
/// disk reads and caching live in crate::core::project; this is just the entry point.
#[tauri::command]
pub(crate) fn project_meta(cwds: Vec<String>) -> Vec<crate::core::project::ProjectMeta> {
    let now = now_secs();
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for cwd in cwds {
        if cwd.is_empty() || seen.contains(&cwd) {
            continue;
        }
        seen.push(cwd.clone());
        out.push(crate::core::project::meta(&cwd, now));
    }
    out
}

#[tauri::command]
pub(crate) fn dismiss_session(store: tauri::State<SharedStore>, id: String) {
    if let Ok(mut s) = store.lock() {
        s.dismiss(&id);
    }
}

#[tauri::command]
pub(crate) fn clear_sessions(store: tauri::State<SharedStore>) {
    if let Ok(mut s) = store.lock() {
        s.clear();
    }
}

#[tauri::command]
pub(crate) fn rotate_alerting_bundle_secret() -> Result<(), String> {
    crate::core::alerting::rotate_bundle_secret()
}

#[derive(serde::Serialize)]
pub(crate) struct Stats {
    pub(crate) total: usize,
    pub(crate) today: usize,
    #[serde(rename = "byAgent")]
    pub(crate) by_agent: std::collections::HashMap<String, usize>,
}

pub(crate) fn day_start_secs(now: u64) -> u64 {
    (now / 86400) * 86400
}

pub(crate) fn compute_stats(sessions: &[crate::core::agent::Session], now: u64) -> Stats {
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
