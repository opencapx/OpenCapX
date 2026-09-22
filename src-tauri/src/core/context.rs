//! A7 Computer Context(i2 §4): the `context.get_current` builtin provider.
//! One Agent call gets "what the user is looking at right now" — it starts to truly live on the user's computer.
//!
//! macOS implementation, with two-path degradation:
//! ① AppleScript (System Events): app name + window title. Triggers the "Automation" TCC prompt,
//!    and the window title also needs "Accessibility"; on denial/failure →
//! ② lsappinfo (no TCC): the frontmost app's bundle id, window title set to null, with a degraded flag.
//! The clipboard snippet uses arboard capped at 500 characters (the clipboard is a sensitive surface: this capability's
//! Agent-layer permission maps to clipboard.read, see permissions.md).

use serde_json::{json, Value};

/// Cap on the clipboard snippet.
const CLIPBOARD_CAP: usize = 500;

/// AppleScript output ("Code\nplugin-manager.ts — repo") → (app, title).
fn parse_applescript(out: &str) -> (String, Option<String>) {
    let mut parts = out.splitn(2, '\n');
    let app = parts.next().unwrap_or("").trim().to_string();
    let title = parts
        .next()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    (app, title)
}

/// lsappinfo info -only bundleID output (` "bundleID"="com.apple.Terminal"`)
/// → bundle id. Takes the first non-empty non-separator segment inside `="…"`.
fn parse_lsappinfo_bundle(out: &str) -> Option<String> {
    let (_, right) = out.split_once("bundleID")?;
    right
        .split('"')
        .find(|s| !s.is_empty() && *s != "=")
        .map(|s| s.to_string())
}

/// Cap the clipboard text: truncate and flag when over-long.
fn cap_clipboard(text: &str) -> Value {
    if text.chars().count() <= CLIPBOARD_CAP {
        json!({ "text": text, "truncated": false })
    } else {
        json!({
            "text": text.chars().take(CLIPBOARD_CAP).collect::<String>(),
            "truncated": true,
        })
    }
}

/// ① AppleScript path: frontmost app name + window title (TCC: Automation + Accessibility).
#[cfg(target_os = "macos")]
fn applescript_frontmost() -> Option<(String, Option<String>)> {
    let script = r#"
tell application "System Events"
    set frontApp to name of first application process whose frontmost is true
    set winTitle to ""
    try
        set winTitle to name of front window of (first application process whose frontmost is true)
    end try
    return frontApp & linefeed & winTitle
end tell"#;
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let (app, title) = parse_applescript(&s);
    if app.is_empty() {
        None
    } else {
        Some((app, title))
    }
}

/// ② lsappinfo path (no TCC): frontmost app bundle id.
#[cfg(target_os = "macos")]
fn lsappinfo_frontmost() -> Option<String> {
    let front = std::process::Command::new("lsappinfo").arg("front").output().ok()?;
    let asn = String::from_utf8_lossy(&front.stdout).trim().to_string();
    if asn.is_empty() {
        return None;
    }
    let info = std::process::Command::new("lsappinfo")
        .args(["info", "-only", "bundleID", &asn])
        .output()
        .ok()?;
    parse_lsappinfo_bundle(&String::from_utf8_lossy(&info.stdout))
}

/// context.get_current builtin implementation.
pub fn current(_input: &Value) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    let (active_app, window_title, degraded) = match applescript_frontmost() {
        Some((app, title)) => (app, title, false),
        None => match lsappinfo_frontmost() {
            // Degraded: bundle id as the app name (reverse-domain form, e.g. com.microsoft.VSCode),
            // window title unavailable; honestly flag degraded + the reason
            Some(bundle) => (bundle, None, true),
            None => (String::new(), None, true),
        },
    };
    // Non-macOS has no AppleScript two-path; an explicit annotation lets the Windows/Linux side infer it too (None has no assignment point)
    #[cfg(not(target_os = "macos"))]
    let (active_app, window_title, degraded): (String, Option<String>, bool) =
        (String::new(), None, true);

    Ok(json!({
        "active_app": active_app,
        "window_title": window_title,
        "clipboard": super::clipboard::text().map(|t| cap_clipboard(&t)),
        // Whether screen.capture is available on this platform (the permission state is only known at actual capture time; only platform capability is reported here)
        "screen_available": cfg!(any(target_os = "macos", target_os = "linux", target_os = "windows")),
        "degraded": degraded,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_applescript_output() {
        let (app, title) = parse_applescript("Code\nplugin-manager.ts — OpenCapX");
        assert_eq!(app, "Code");
        assert_eq!(title.as_deref(), Some("plugin-manager.ts — OpenCapX"));
        // Empty window title (app with no window / denied)
        let (app, title) = parse_applescript("Finder\n");
        assert_eq!(app, "Finder");
        assert_eq!(title, None);
    }

    #[test]
    fn parses_lsappinfo_bundle() {
        assert_eq!(
            parse_lsappinfo_bundle(r#" "bundleID"="com.apple.Terminal""#),
            Some("com.apple.Terminal".into())
        );
        assert_eq!(parse_lsappinfo_bundle(r#" "bundleID"="""#), None);
        assert_eq!(parse_lsappinfo_bundle("garbage"), None);
    }

    #[test]
    fn caps_clipboard_snippet() {
        let short = cap_clipboard("hello");
        assert_eq!(short["text"], json!("hello"));
        assert_eq!(short["truncated"], json!(false));
        let long: String = "x".repeat(CLIPBOARD_CAP + 10);
        let capped = cap_clipboard(&long);
        assert_eq!(capped["text"].as_str().unwrap().chars().count(), CLIPBOARD_CAP);
        assert_eq!(capped["truncated"], json!(true));
    }

    /// Shape contract: all four fields present + a degraded flag (does not actually call osascript, so unit tests have no TCC
    /// prompt pollution; the real-machine path is covered by an ignored case).
    #[test]
    fn current_returns_shape() {
        let out = current(&json!({})).unwrap();
        assert!(out.get("active_app").is_some());
        assert!(out.get("window_title").is_some());
        assert!(out.get("clipboard").is_some());
        assert!(out.get("screen_available").is_some());
        assert!(out.get("degraded").is_some());
    }

    #[test]
    #[ignore = "touches real TCC/clipboard; run with --ignored manually"]
    fn current_real_machine_manual() {
        let out = current(&json!({})).unwrap();
        // A real machine always has a frontmost app (at least one of AppleScript or lsappinfo works)
        assert!(!out["active_app"].as_str().unwrap_or("").is_empty(), "{}", out);
    }
}
