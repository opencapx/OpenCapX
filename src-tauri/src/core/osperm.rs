//! v1.2 OS permission probe (the `system.permission_status` builtin provider).
//! Read-only, exempt from the Agent-layer gate (same exemption class as list_capabilities; rpc.rs tool_permission
//! returning None means skip) — its purpose is up-front routing: an agent checks before calling screen.capture
//! for "Screen Recording: not authorized", turning errors from "reported afterward" into "routed around beforehand".
//!
//! All macOS FFI uses the preflight variants (**no prompt**):
//! - CGPreflightScreenCaptureAccess (CoreGraphics): Screen Recording TCC state
//! - AXIsProcessTrusted (ApplicationServices): Accessibility state (Boolean = u8)
//!
//! Non-macOS is classified by the environment, not by a TCC lookup: on Linux the builtin
//! capture is `scrot` (X11 only), so a Wayland or headless session is reported as the fourth
//! state `unavailable` — "the builtin cannot work here, and no user grant would fix it" —
//! which is distinct from `denied` (the user can grant it in system settings). On Windows the
//! probe enumerates xcap monitors: an interactive desktop enumerates, a disconnected RDP
//! session does not. (The Wayland desktop-portal capture path is a separate feature.)

use serde_json::{json, Value};

/// Display-session classifier for the Linux probe. Pure: callers inject the environment so
/// tests need no process-global state. `XDG_SESSION_TYPE` is authoritative when present;
/// otherwise infer from the display vars, Wayland taking precedence when both are set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinuxSession {
    X11,
    Wayland,
    Headless,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn classify_linux(
    session_type: Option<&str>,
    wayland_display: bool,
    x_display: bool,
) -> LinuxSession {
    match session_type.map(|s| s.trim().to_ascii_lowercase()) {
        Some(s) if s == "wayland" => LinuxSession::Wayland,
        Some(s) if s == "x11" => LinuxSession::X11,
        _ => {
            if wayland_display {
                LinuxSession::Wayland
            } else if x_display {
                LinuxSession::X11
            } else {
                LinuxSession::Headless
            }
        }
    }
}

/// inputctl (v1.3) reuses ax_trusted: AXIsProcessTrusted is declared at a single site repo-wide,
/// since a duplicate extern declaration with a mismatched signature triggers clashing_extern_declarations.
#[cfg(target_os = "macos")]
pub(crate) mod ffi {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        /// macOS Boolean is unsigned char, not C bool
        fn AXIsProcessTrusted() -> u8;
    }

    pub fn screen_capture_access() -> bool {
        unsafe { CGPreflightScreenCaptureAccess() }
    }
    pub fn ax_trusted() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }
}

/// area values: granted | denied | unavailable | not_required.
/// `unavailable` = the builtin cannot work in this environment and no user grant would change
/// that; `denied` = the user can grant it. See the module docs.
#[cfg(target_os = "macos")]
fn screen_recording() -> &'static str {
    if ffi::screen_capture_access() {
        "granted"
    } else {
        "denied"
    }
}

#[cfg(target_os = "linux")]
fn screen_recording() -> &'static str {
    let session_type = std::env::var("XDG_SESSION_TYPE").ok();
    let session = classify_linux(
        session_type.as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var_os("DISPLAY").is_some(),
    );
    match session {
        // scrot is an X11 tool; on X11 the question is only whether it is installed.
        LinuxSession::X11 => {
            if which("scrot") {
                "not_required"
            } else {
                "unavailable"
            }
        }
        LinuxSession::Wayland | LinuxSession::Headless => "unavailable",
    }
}

/// No prompt, no side effect: enumerating monitors succeeds on an interactive desktop and
/// fails on a disconnected RDP session (the case `not_required` used to miss).
#[cfg(target_os = "windows")]
fn screen_recording() -> &'static str {
    match xcap::Monitor::all() {
        Ok(monitors) if !monitors.is_empty() => "granted",
        _ => "unavailable",
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn screen_recording() -> &'static str {
    "not_required"
}

/// PATH lookup without spawning a process.
#[cfg(target_os = "linux")]
fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn accessibility() -> &'static str {
    if ffi::ax_trusted() {
        "granted"
    } else {
        "denied"
    }
}
#[cfg(not(target_os = "macos"))]
fn accessibility() -> &'static str {
    "not_required"
}

/// v1.5 first-use guidance: an area that needs action carries remediation guidance (the deep
/// link can be used by an agent via url.scheme.open to open the matching system settings pane
/// for the user; steps are for humans to read). granted / not_required carry none.
///
/// `settingsUrl` is **optional**: only macOS has a settings deep link to hand over. Linux and
/// Windows guide through a terminal package or a session change, so they carry `steps` only.
#[cfg(target_os = "macos")]
fn guidance_for(area: &str) -> Option<Value> {
    let (url, steps) = match area {
        "screen_recording" => (
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
            "System Settings → Privacy & Security → Screen & Audio Recording → enable OpenCapX (app restart required)",
        ),
        "accessibility" => (
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            "System Settings → Privacy & Security → Accessibility → enable OpenCapX",
        ),
        _ => return None,
    };
    Some(json!({ "settingsUrl": url, "steps": steps }))
}

/// Linux guidance as a pure function of the classified session, so the shape is testable on
/// any platform. No `settingsUrl` key at all — there is no settings pane to deep-link to.
#[cfg(any(target_os = "linux", test))]
fn linux_guidance(session: LinuxSession, scrot_present: bool) -> Value {
    let steps = match session {
        LinuxSession::Wayland => "builtin screen capture uses scrot (X11 only). Run an X11/XWayland session, or install a provider plugin that captures through the desktop portal.",
        LinuxSession::Headless => "no display session is active, so there is nothing to capture. Start a graphical session and retry.",
        LinuxSession::X11 => {
            if scrot_present {
                "scrot is on PATH but the X11 session exposes no capturable display; check that DISPLAY points at a live X server."
            } else {
                "install scrot (Debian/Ubuntu: sudo apt install scrot; Fedora: sudo dnf install scrot)"
            }
        }
    };
    json!({ "steps": steps })
}

#[cfg(target_os = "linux")]
fn guidance_for(area: &str) -> Option<Value> {
    match area {
        // accessibility stays not_required on Linux: there is no advance probe for it here.
        "screen_recording" => {
            let session_type = std::env::var("XDG_SESSION_TYPE").ok();
            let session = classify_linux(
                session_type.as_deref(),
                std::env::var_os("WAYLAND_DISPLAY").is_some(),
                std::env::var_os("DISPLAY").is_some(),
            );
            Some(linux_guidance(session, which("scrot")))
        }
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn guidance_for(area: &str) -> Option<Value> {
    match area {
        "screen_recording" => Some(json!({
            "steps": "screen capture needs an interactive desktop session (reconnect if this machine is running over a disconnected RDP session)"
        })),
        _ => None,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn guidance_for(_area: &str) -> Option<Value> {
    None
}

/// system.permission_status builtin implementation.
pub fn status(_input: &Value) -> Result<Value, String> {
    let screen = screen_recording();
    let ax = accessibility();
    let mut guidance = serde_json::Map::new();
    for (area, st) in [("screen_recording", screen), ("accessibility", ax)] {
        // Both states need the user to do something: `denied` can be granted, `unavailable`
        // needs a session or tool change. Either way the area carries guidance.
        if st == "denied" || st == "unavailable" {
            if let Some(g) = guidance_for(area) {
                guidance.insert(area.to_string(), g);
            }
        }
    }
    Ok(json!({
        "areas": {
            "screen_recording": screen,
            "accessibility": ax,
        },
        "guidance": guidance,
        "os": std::env::consts::OS,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented area contract. `unavailable` was added alongside the original three:
    /// the builtin cannot work in this environment and no user grant would change that.
    const AREA_VALUES: [&str; 4] = ["granted", "denied", "unavailable", "not_required"];

    /// The two states that mean "the user has something to do".
    fn needs_action(st: &str) -> bool {
        st == "denied" || st == "unavailable"
    }

    /// Shape contract: both areas are in the enum, os is non-empty, and guidance appears for
    /// exactly the areas that need action — never for granted / not_required.
    /// On macOS it also verifies the FFI link resolves for real (no prompt, preflight variant).
    #[test]
    fn status_shape_and_enum_values() {
        let out = status(&json!({})).unwrap();
        let areas = out["areas"].as_object().expect("areas object");
        assert_eq!(areas.len(), 2, "{}", out);
        for key in ["screen_recording", "accessibility"] {
            let v = areas[key].as_str().expect("area is string");
            assert!(AREA_VALUES.contains(&v), "{} = {}", key, v);
        }
        assert!(!out["os"].as_str().unwrap_or("").is_empty());
        let guidance = out["guidance"].as_object().expect("guidance object");
        for (area, st) in [
            ("screen_recording", screen_recording()),
            ("accessibility", accessibility()),
        ] {
            if needs_action(st) {
                let g = guidance
                    .get(area)
                    .expect("action-needing area carries guidance");
                assert!(!g["steps"].as_str().unwrap_or("").is_empty(), "{}", g);
                // settingsUrl is optional: only macOS has a settings pane to deep-link to.
                if let Some(url) = g.get("settingsUrl") {
                    assert!(
                        url.as_str()
                            .unwrap_or("")
                            .starts_with("x-apple.systempreferences:"),
                        "{}",
                        g
                    );
                }
            } else {
                assert!(
                    !guidance.contains_key(area),
                    "granted / not_required area must not carry guidance: {} = {}",
                    area,
                    st
                );
            }
        }
    }

    /// Off macOS there is still no accessibility preflight, so that area is not_required and
    /// silent. screen_recording is environment-dependent, so only the enum is asserted.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_accessibility_is_not_required_and_silent() {
        let out = status(&json!({})).unwrap();
        assert_eq!(out["areas"]["accessibility"], "not_required");
        let guidance = out["guidance"].as_object().expect("guidance object");
        assert!(!guidance.contains_key("accessibility"), "{}", out);
        let screen = out["areas"]["screen_recording"].as_str().unwrap();
        assert!(
            AREA_VALUES.contains(&screen),
            "screen_recording = {}",
            screen
        );
        // An action-needing screen area must always be able to say what to do.
        if needs_action(screen) {
            assert!(guidance.contains_key("screen_recording"), "{}", out);
        }
    }

    // ── Linux display-session classifier (pure; runs on every platform) ──

    #[test]
    fn classify_linux_honours_explicit_session_type() {
        assert_eq!(
            classify_linux(Some("wayland"), false, false),
            LinuxSession::Wayland
        );
        assert_eq!(classify_linux(Some("x11"), false, false), LinuxSession::X11);
        // The env var is authoritative: it outranks contradicting display vars.
        assert_eq!(
            classify_linux(Some("wayland"), false, true),
            LinuxSession::Wayland
        );
        assert_eq!(classify_linux(Some("x11"), true, false), LinuxSession::X11);
    }

    #[test]
    fn classify_linux_is_case_and_whitespace_insensitive() {
        assert_eq!(
            classify_linux(Some(" WayLand "), false, false),
            LinuxSession::Wayland
        );
        assert_eq!(classify_linux(Some("X11"), false, false), LinuxSession::X11);
    }

    #[test]
    fn classify_linux_falls_back_to_display_vars() {
        // Unknown / missing session type → infer from the display vars.
        assert_eq!(classify_linux(None, true, false), LinuxSession::Wayland);
        assert_eq!(classify_linux(None, false, true), LinuxSession::X11);
        assert_eq!(
            classify_linux(Some("tty"), true, true),
            LinuxSession::Wayland
        );
        assert_eq!(classify_linux(Some(""), true, false), LinuxSession::Wayland);
    }

    #[test]
    fn classify_linux_with_no_display_is_headless() {
        assert_eq!(classify_linux(None, false, false), LinuxSession::Headless);
        assert_eq!(
            classify_linux(Some("tty"), false, false),
            LinuxSession::Headless
        );
    }

    // ── Linux guidance shape (pure; runs on every platform) ──

    #[test]
    fn linux_guidance_omits_settings_url_and_has_steps() {
        for (session, scrot) in [
            (LinuxSession::Wayland, false),
            (LinuxSession::Headless, false),
            (LinuxSession::X11, false),
            (LinuxSession::X11, true),
        ] {
            let g = linux_guidance(session, scrot);
            assert!(
                g.get("settingsUrl").is_none(),
                "linux guidance must not carry a settings deep link: {}",
                g
            );
            assert!(!g["steps"].as_str().unwrap_or("").is_empty(), "{}", g);
        }
    }

    #[test]
    fn linux_guidance_text_matches_the_session() {
        let wayland = linux_guidance(LinuxSession::Wayland, false)["steps"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(wayland.contains("scrot"), "{}", wayland);
        let headless = linux_guidance(LinuxSession::Headless, false)["steps"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(headless.contains("no display session"), "{}", headless);
        let missing = linux_guidance(LinuxSession::X11, false)["steps"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(missing.contains("install scrot"), "{}", missing);
    }
}
