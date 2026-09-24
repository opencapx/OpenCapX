//! settings file read/write, pet hover tracker, settings window commands.
//! Mechanical move from main.rs.

use super::*;
use crate::tray::{sync_tray_bubble_check, sync_tray_pet_check};

pub(crate) fn settings_path() -> std::path::PathBuf {
    if let Some(home) = crate::core::home_dir() {
        return home.join(".opencapx").join("settings.json");
    }
    std::env::temp_dir().join("opencapx-settings.json")
}

pub(crate) fn default_settings() -> serde_json::Value {
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
pub(crate) fn get_settings() -> serde_json::Value {
    read_settings_file()
}

pub(crate) fn read_settings_file() -> serde_json::Value {
    let path = settings_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            return v;
        }
    }
    default_settings()
}

pub(crate) fn write_settings_file(value: &serde_json::Value) -> bool {
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
pub(crate) fn set_settings(app: tauri::AppHandle, value: serde_json::Value) -> bool {
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
pub(crate) fn ui_clipboard_write(text: String) -> Result<(), String> {
    crate::core::clipboard::write_text(&text)
}

#[tauri::command]
pub(crate) fn get_agents() -> Vec<hooks::AgentInfo> {
    hooks::catalog()
}

#[tauri::command]
pub(crate) fn toggle_agent(kind: String) -> Result<bool, String> {
    hooks::toggle(&kind)
}

/// Pet window mouse passthrough strategy: once a window ignores cursor events it receives no mousemove at all,
/// and the frontend cannot switch itself back (it would stay click-through, unclickable and undraggable). So poll the global cursor
/// position here, convert to in-window logical coordinates, and send to the frontend, which decides passthrough by actual layout (pet canvas / bubbles).
pub(crate) fn spawn_pet_hover_tracker(app: tauri::AppHandle) {
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
pub(crate) fn show_settings(app: &tauri::AppHandle) {
    use tauri::Manager;
    #[cfg(target_os = "macos")]
    let _ = app.show();
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[tauri::command]
pub(crate) fn open_settings(app: tauri::AppHandle) {
    show_settings(&app);
}
