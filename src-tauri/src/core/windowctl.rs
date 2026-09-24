//! v1.4 window.list / window.focus builtin provider.
//!
//! Permission `window.management` (denied + high-risk): enumerating/focusing other apps' windows =
//! the starting operation of the computer-use surface (it can see and steal focus), denied by default.
//!
//! Implementation = System Events AppleScript (macOS-only, requires Accessibility TCC,
//! with an ax_trusted pre-check; no permission without granting it):
//! - list: one line per window of every frontmost visible process, fields tab-separated, title last
//!   (the title may contain any characters; splitn(7) keeps it intact): proc|front|x|y|w|h|title
//! - focus: `set frontmost of process "P" to true` + optionally, for a named window,
//!   `perform action "AXRaise"` (a failed raise does not fail the whole call — frontmost is enough)
//! Linux/Windows are not done in v1 (honest error, plugins can override).

use serde_json::{json, Value};
use std::time::Duration;

const WIN_TIMEOUT: Duration = Duration::from_secs(15);
/// Cap on window rows in a single enumeration.
const WIN_CAP: usize = 200;

/// Validation before a process name enters an AppleScript string literal: no quotes/backslashes/control chars.
/// (Validation rather than escaping — the process name comes from agent input, and an allowlist is more robust than escaping.)
fn sane_process_name(p: &str) -> Result<(), String> {
    if p.is_empty() || p.len() > 200 {
        return Err("invalid input: process must be 1..=200 chars".into());
    }
    if p.chars().any(|c| c == '"' || c == '\\' || c.is_control()) {
        return Err(format!(
            "invalid input: process has unsafe characters: {:?}",
            p
        ));
    }
    Ok(())
}

/// window.list builtin implementation.
pub fn list(_input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err(
            "window.list builtin is macOS-only (System Events); a plugin can override".into(),
        );
    }
    #[cfg(target_os = "macos")]
    {
        if !super::osperm::ffi::ax_trusted() {
            return Err(
                "window.list requires Accessibility permission for OpenCapX (System Settings → Privacy & Security → Accessibility)"
                    .into(),
            );
        }
        // Fields are tab-separated, title last (the title may contain pipes/newlines? A newline cannot be cleanly split on the AppleScript
        // side — take it as-is for now; newlines in titles are rare, v1 accepts it)
        let src = r#"
            tell application "System Events"
                set out to ""
                repeat with p in (every process whose background only is false)
                    repeat with w in (every window of p)
                        set out to out & (name of p) & tab
                            & ((frontmost of p) as integer) & tab
                            & ((position of w) as text) & tab
                            & ((size of w) as text) & tab
                            & (name of w) & linefeed
                    end repeat
                end repeat
                return out
            end tell
        "#;
        let out = super::appctl::osascript_ok(src, WIN_TIMEOUT, "list windows")?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut wins = Vec::new();
        let mut truncated = false;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // position/size's "x, y" already contains a comma+space; the line = 7 fields
            let parts: Vec<&str> = line.splitn(7, '\t').collect();
            if parts.len() != 7 {
                continue;
            }
            if wins.len() >= WIN_CAP {
                truncated = true;
                break;
            }
            let pos: Vec<&str> = parts[2].split(',').map(|s| s.trim()).collect();
            let size: Vec<&str> = parts[3].split(',').map(|s| s.trim()).collect();
            if pos.len() != 2 || size.len() != 2 {
                continue;
            }
            let (Ok(x), Ok(y)) = (pos[0].parse::<i64>(), pos[1].parse::<i64>()) else {
                continue;
            };
            let (Ok(w), Ok(h)) = (size[0].parse::<i64>(), size[1].parse::<i64>()) else {
                continue;
            };
            wins.push(json!({
                "process": parts[0],
                "front": parts[1] == "1",
                "x": x, "y": y, "w": w, "h": h,
                "title": parts[6],
            }));
        }
        Ok(json!({ "count": wins.len(), "windows": wins, "truncated": truncated }))
    }
}

/// window.focus builtin implementation.
pub fn focus(input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        return Err(
            "window.focus builtin is macOS-only (System Events); a plugin can override".into(),
        );
    }
    #[cfg(target_os = "macos")]
    {
        let Some(process) = input.get("process").and_then(|p| p.as_str()) else {
            return Err("invalid input: process (string) required".into());
        };
        sane_process_name(process)?;
        let title = input.get("title").and_then(|t| t.as_str());
        if let Some(t) = title {
            if t.contains('"') || t.contains('\\') || t.chars().any(|c| c.is_control()) {
                return Err("invalid input: title has unsafe characters".into());
            }
        }
        if !super::osperm::ffi::ax_trusted() {
            return Err(
                "window.focus requires Accessibility permission for OpenCapX (System Settings → Privacy & Security → Accessibility)"
                    .into(),
            );
        }
        // When title is given, also raise that window; when not, only switch frontmost.
        // A failed raise (window not found / a rare app not supporting it) does not fail the whole call; frontmost is already achieved.
        let src = if let Some(t) = title {
            format!(
                r#"tell application "System Events"
                    set frontmost of process "{p}" to true
                    try
                        perform action "AXRaise" of (first window of process "{p}" whose name is "{t}")
                    end try
                end tell"#,
                p = process,
                t = t
            )
        } else {
            format!(
                r#"tell application "System Events" to set frontmost of process "{}" to true"#,
                process
            )
        };
        let _ = super::appctl::osascript_ok(&src, WIN_TIMEOUT, "focus window")?;
        Ok(json!({ "ok": true, "process": process }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_name_whitelist() {
        assert!(sane_process_name("Google Chrome").is_ok());
        assert!(sane_process_name("com.apple.Safari").is_ok());
        assert!(sane_process_name("").is_err());
        assert!(sane_process_name("say \"hi\"").is_err());
        assert!(sane_process_name("back\\slash").is_err());
        assert!(sane_process_name("ctl\u{1}char").is_err());
        assert!(sane_process_name(&"x".repeat(201)).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn focus_validates_process_before_tcc() {
        // Validation precedes the TCC pre-check: bad input should not incur the permission prompt burden
        assert!(focus(&json!({}))
            .unwrap_err()
            .contains("process (string) required"));
        assert!(focus(&json!({ "process": "a\"b" }))
            .unwrap_err()
            .contains("unsafe"));
        assert!(focus(&json!({ "process": "x", "title": "y\"z" }))
            .unwrap_err()
            .contains("unsafe"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn windows_report_platform_limit() {
        assert!(list(&json!({})).unwrap_err().contains("macOS-only"));
        assert!(focus(&json!({ "process": "x" }))
            .unwrap_err()
            .contains("macOS-only"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "needs Accessibility TCC + GUI session; run with --ignored manually"]
    fn list_real_manual() {
        let out = list(&json!({})).unwrap();
        assert!(
            out["count"].as_i64().unwrap() >= 1,
            "at least Finder/its own window"
        );
        for w in out["windows"].as_array().unwrap() {
            assert!(w["process"].as_str().is_some());
            assert!(w["w"].as_i64().unwrap() > 0);
        }
    }
}
