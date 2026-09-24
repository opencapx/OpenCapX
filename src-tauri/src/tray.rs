//! tray state, menu, pet/bubble visibility, hotkey dispatch.
//! Mechanical move from main.rs.

use super::*;
use crate::settings_file::write_settings_file;

/// Phase 39 — execute the corresponding action when an OS shortcut fires (called by Builder.with_handler).
pub(crate) fn dispatch_hotkey_action(
    app: &tauri::AppHandle,
    action: crate::core::hotkey::HotkeyAction,
) {
    use crate::core::hotkey::{BuiltinAction, HotkeyAction};
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
            let mgr = crate::core::plugin::PluginManager::shared();
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

/// Mutable state of the tray menu.
///
/// The menu is **dynamically rebuilt** (the session list changes with agent activity), so the check item handle is always
/// a new object — you cannot `manage` a fixed handle, or syncing the check state would write to a discarded menu.
/// `signature` debounces content: no rebuild when nothing changed, avoiding per-second tray churn.
pub(crate) struct TrayState {
    pub(crate) pet_check: std::sync::Mutex<Option<tauri::menu::CheckMenuItem<tauri::Wry>>>,
    pub(crate) bubble_check: std::sync::Mutex<Option<tauri::menu::CheckMenuItem<tauri::Wry>>>,
    pub(crate) signature: std::sync::Mutex<String>,
}

pub(crate) fn sync_tray_pet_check(app: &tauri::AppHandle, visible: bool) {
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
pub(crate) fn sync_tray_bubble_check(app: &tauri::AppHandle, enabled: bool) {
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
pub(crate) fn tray_icon(
    badge: crate::core::tray::TrayBadge,
) -> Option<tauri::image::Image<'static>> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<std::collections::HashMap<u8, Vec<u8>>>> = OnceLock::new();
    let base = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png")).ok()?;
    if badge == crate::core::tray::TrayBadge::None {
        return Some(tauri::image::Image::new_owned(
            base.rgba().to_vec(),
            base.width(),
            base.height(),
        ));
    }
    let key = if badge == crate::core::tray::TrayBadge::Waiting {
        2u8
    } else {
        1u8
    };
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let rgba = {
        let mut map = cache.lock().ok()?;
        map.entry(key)
            .or_insert_with(|| {
                crate::core::tray::compose_icon(base.rgba(), base.width(), base.height(), badge)
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
pub(crate) fn refresh_tray_status(
    app: &tauri::AppHandle,
    strs: &crate::core::i18n::Strings,
    sessions: &[crate::core::agent::Session],
) {
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let count = |state: crate::core::agent::AgentState| {
        sessions.iter().filter(|s| s.state == state).count()
    };
    let (working, waiting) = (
        count(crate::core::agent::AgentState::Working),
        count(crate::core::agent::AgentState::Waiting),
    );
    let badge = crate::core::tray::badge_for(working, waiting);
    if let Some(img) = tray_icon(badge) {
        // a colored badge cannot be a template (macOS would render it as a monochrome block);
        // set icon+template atomically to avoid flicker from setting icon before template.
        #[cfg(target_os = "macos")]
        let set =
            tray.set_icon_with_as_template(Some(img), badge == crate::core::tray::TrayBadge::None);
        #[cfg(not(target_os = "macos"))]
        let set = tray.set_icon(Some(img));
        if let Err(e) = set {
            eprintln!("[tray] set_icon failed: {e}");
        }
    }
    let tip = crate::core::tray::tooltip_text(strs, sessions);
    let _ = tray.set_tooltip(Some(tip.as_str()));
}

/// Rebuild the tray menu: summary at top + one row per active session + action items. Skip if content is unchanged.
pub(crate) fn refresh_tray_menu(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::Manager;
    let Some(tray) = app.tray_by_id("main") else {
        return Ok(());
    };
    let Some(store) = crate::core::shared_store() else {
        return Ok(());
    };
    let mut sessions = store
        .lock()
        .ok()
        .map(|s| s.active(crate::core::agent::now_secs()))
        .unwrap_or_default();
    crate::core::agent::sort_sessions(&mut sessions);

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
    let strs = crate::core::i18n::strings(crate::core::i18n::from_locale(locale));
    let count = |state: crate::core::agent::AgentState| {
        sessions.iter().filter(|s| s.state == state).count()
    };
    let working = count(crate::core::agent::AgentState::Working);
    let waiting = count(crate::core::agent::AgentState::Waiting);
    let done = count(crate::core::agent::AgentState::Done);
    let clearable = sessions.iter().any(|s| {
        matches!(
            s.state,
            crate::core::agent::AgentState::Done | crate::core::agent::AgentState::Idle
        )
    });
    // sessions grouped by project (one submenu per project); branch looked up in crate::core::project (30s cache).
    // group order = appearance order of the first session in the group — sessions are already sorted by sort_sessions,
    // so "projects with someone waiting" naturally come first, the same rule as the bubble group headers.
    let now_s = crate::core::agent::now_secs();
    let branch_of = |cwd: &str| crate::core::project::meta(cwd, now_s).branch;
    let (sections, ungrouped) = crate::core::tray::sections(strs, &sessions, &branch_of);
    let summary = crate::core::tray::summary_text(strs, working, waiting, done, sessions.len());
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
        crate::core::tray::badge_for(working, waiting),
        crate::core::tray::tooltip_text(strs, &sessions)
    );
    Ok(())
}

/// Clear finished/idle sessions (tray "Clear finished").
pub(crate) fn clear_finished_sessions() {
    let Some(store) = crate::core::shared_store() else {
        return;
    };
    let Ok(mut s) = store.lock() else {
        return;
    };
    let ids: Vec<String> = s
        .active(crate::core::agent::now_secs())
        .iter()
        .filter(|x| {
            matches!(
                x.state,
                crate::core::agent::AgentState::Done | crate::core::agent::AgentState::Idle
            )
        })
        .map(|x| x.id.clone())
        .collect();
    for id in ids {
        s.dismiss(&id);
    }
}

pub(crate) fn set_pet_visible(app: &tauri::AppHandle, v: bool) {
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
pub(crate) fn set_bubble_visible(app: &tauri::AppHandle, v: bool) {
    let mut s = read_settings_file();
    if let Some(o) = s.as_object_mut() {
        o.insert("bubbleEnabled".to_string(), serde_json::json!(v));
    }
    let _ = write_settings_file(&s);
    sync_tray_bubble_check(app, v);
}
