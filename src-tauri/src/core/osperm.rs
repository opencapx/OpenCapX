//! v1.2 OS permission probe (the `system.permission_status` builtin provider).
//! Read-only, exempt from the Agent-layer gate (same exemption class as list_capabilities; rpc.rs tool_permission
//! returning None means skip) — its purpose is up-front routing: an agent checks before calling screen.capture
//! for "Screen Recording: not authorized", turning errors from "reported afterward" into "routed around beforehand".
//!
//! All macOS FFI uses the preflight variants (**no prompt**):
//! - CGPreflightScreenCaptureAccess (CoreGraphics): Screen Recording TCC state
//! - AXIsProcessTrusted (ApplicationServices): Accessibility state (Boolean = u8)
//! Windows/Linux have no TCC equivalent → "not_required" (the Wayland portal is not in this cycle).

use serde_json::{json, Value};

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

/// area values: granted | denied | not_required (non-macOS has no TCC).
#[cfg(target_os = "macos")]
fn screen_recording() -> &'static str {
    if ffi::screen_capture_access() {
        "granted"
    } else {
        "denied"
    }
}
#[cfg(not(target_os = "macos"))]
fn screen_recording() -> &'static str {
    "not_required"
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

/// v1.5 first-use guidance: a denied area carries remediation guidance (the deep link can be used by an agent
/// via url.scheme.open to open the matching system settings pane for the user; steps are for humans to read).
/// granted / not_required carry none — guidance appears only when "action is needed".
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

#[cfg(not(target_os = "macos"))]
fn guidance_for(_area: &str) -> Option<Value> {
    // Win32/X11 have no TCC equivalent (not_required needs no guidance anyway); the Wayland portal is not in this cycle.
    None
}

/// system.permission_status builtin implementation.
pub fn status(_input: &Value) -> Result<Value, String> {
    let screen = screen_recording();
    let ax = accessibility();
    let mut guidance = serde_json::Map::new();
    for (area, st) in [("screen_recording", screen), ("accessibility", ax)] {
        if st == "denied" {
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

    /// Shape contract: both areas are in the enum, os is non-empty.
    /// On macOS it also verifies the FFI link resolves for real (no prompt, preflight variant).
    #[test]
    fn status_shape_and_enum_values() {
        let out = status(&json!({})).unwrap();
        let areas = out["areas"].as_object().expect("areas object");
        assert_eq!(areas.len(), 2, "{}", out);
        for key in ["screen_recording", "accessibility"] {
            let v = areas[key].as_str().expect("area is string");
            assert!(
                v == "granted" || v == "denied" || v == "not_required",
                "{} = {}",
                key,
                v
            );
        }
        assert!(!out["os"].as_str().unwrap_or("").is_empty());
        // v1.5 guidance contract: only denied areas appear; entries carry settingsUrl + steps
        let guidance = out["guidance"].as_object().expect("guidance object");
        for (area, st) in [("screen_recording", screen_recording()), ("accessibility", accessibility())] {
            match st {
                "denied" => {
                    let g = guidance.get(area).expect("denied area carries guidance");
                    assert!(g["settingsUrl"].as_str().unwrap_or("").starts_with("x-apple.systempreferences:"), "{}", g);
                    assert!(!g["steps"].as_str().unwrap_or("").is_empty());
                }
                _ => assert!(!guidance.contains_key(area), "non-denied area must not carry guidance"),
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_reports_not_required_without_guidance() {
        let out = status(&json!({})).unwrap();
        assert_eq!(out["areas"]["screen_recording"], "not_required");
        assert_eq!(out["areas"]["accessibility"], "not_required");
        assert_eq!(out["guidance"].as_object().map(|m| m.len()), Some(0));
    }
}
