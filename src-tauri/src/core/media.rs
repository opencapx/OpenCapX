//! v1.4 media.playback builtin provider (docs/capability.md "builtin providers").
//!
//! Permission `media.control` (ask): control the currently playing media. Low-risk but high-perception — the desktop pet's ace
//! move, "play a song", but audible content is still a human-facing surface, so ask each call.
//!
//! - macOS: Music / Spotify AppleScript (each triggers its own "Automation" TCC)
//! - Linux: playerctl (MPRIS; missing one prompts install)
//! - Windows: media keys (SendInput) — system-level play/pause/next, forwarded to the current
//!   player. Known limitation: media keys only have "toggle" semantics; play and pause both map to
//!   the same key, so forcing a specific state is impossible; the current track is also unavailable (no track returned).

use serde_json::{json, Value};
use std::time::Duration;

/// Timeout for one control call (AppleScript cold start can be slow).
const CMD_TIMEOUT: Duration = Duration::from_secs(15);

/// Input validation (pure function): (action, app). action is one of five; app ∈ music|spotify
/// is meaningful only on macOS; non-mac platforms ignore it and continue (no error, same input shape).
fn validate(input: &Value) -> Result<(&'static str, &'static str), String> {
    let action = match input.get("action").and_then(|a| a.as_str()) {
        Some("play") => "play",
        Some("pause") => "pause",
        Some("toggle") => "toggle",
        Some("next") => "next",
        Some("previous") => "previous",
        Some(other) => {
            return Err(format!(
                "invalid input: action must be play|pause|toggle|next|previous, got {}",
                other
            ))
        }
        None => return Err("invalid input: action (string) required".into()),
    };
    let app = match input.get("app").and_then(|a| a.as_str()).unwrap_or("music") {
        "music" => "music",
        "spotify" => "spotify",
        other => {
            return Err(format!("invalid input: app must be music|spotify, got {}", other))
        }
    };
    Ok((action, app))
}

/// AppleScript verb (pure function): Music and Spotify have identically shaped dictionaries.
#[cfg(target_os = "macos")]
fn as_verb(action: &str) -> &'static str {
    match action {
        "play" => "play",
        "pause" => "pause",
        "toggle" => "playpause",
        "next" => "next track",
        _ => "previous track",
    }
}

/// playerctl subcommand (pure function).
fn playerctl_verb(action: &str) -> &'static str {
    match action {
        "play" => "play",
        "pause" => "pause",
        "toggle" => "play-pause",
        "next" => "next",
        _ => "previous",
    }
}

/// Media-key Virtual-Key code (pure function, shared by the three platforms' unit tests).
#[allow(dead_code)] // Read only in the Windows build
pub(crate) fn media_vk(action: &str) -> Option<u16> {
    match action {
        "play" | "pause" | "toggle" => Some(0xB3), // VK_MEDIA_PLAY_PAUSE
        "next" => Some(0xB0),                      // VK_MEDIA_NEXT_TRACK
        "previous" => Some(0xB1),                  // VK_MEDIA_PREV_TRACK
        _ => None,
    }
}

/// media.playback builtin implementation.
pub fn playback(input: &Value) -> Result<Value, String> {
    let (action, app) = validate(input)?;

    #[cfg(target_os = "macos")]
    {
        let bundle = if app == "spotify" { "com.spotify.client" } else { "com.apple.Music" };
        let src = format!(
            r#"tell application id "{b}"
	{verb}
	delay 0.3
	set tn to ""
	try
		set tn to (name of current track) & " — " & (artist of current track)
	end try
	return tn
end tell"#,
            b = bundle,
            verb = as_verb(action),
        );
        let out = super::appctl::osascript_ok(&src, CMD_TIMEOUT, "media control failed")?;
        let track = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let mut v = json!({ "ok": true });
        if !track.is_empty() {
            v["track"] = json!(track.chars().take(300).collect::<String>());
        }
        Ok(v)
    }

    #[cfg(target_os = "linux")]
    {
        let _ = app;
        let mut cmd = std::process::Command::new("playerctl");
        cmd.arg(playerctl_verb(action));
        let out = super::appctl::run_with_timeout(cmd, CMD_TIMEOUT)
            .map_err(|e| format!("media control failed: {} (install playerctl)", e))?;
        if !out.status.success() {
            return Err(format!(
                "playerctl exited {:?}: {} (install playerctl / is a player actually playing?)",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).chars().take(200).collect::<String>()
            ));
        }
        // Track read-back: not getting one is not an error (no player online)
        let mut q = std::process::Command::new("playerctl");
        q.args(["metadata", "--format", "{{artist}} - {{title}}"]);
        let track = super::appctl::run_with_timeout(q, CMD_TIMEOUT)
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());
        let mut v = json!({ "ok": true });
        if let Some(t) = track {
            v["track"] = json!(t.chars().take(300).collect::<String>());
        }
        Ok(v)
    }

    #[cfg(target_os = "windows")]
    {
        let _ = app;
        let vk = media_vk(action).unwrap();
        if !super::inputctl::tap_key(vk) {
            return Err("SendInput rejected (elevated window?)".into());
        }
        Ok(json!({ "ok": true, "note": "media key is toggle-only on Windows; track unknown" }))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (action, app);
        Err("media.playback builtin not supported on this platform".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_action_and_app() {
        assert_eq!(validate(&json!({ "action": "next" })).unwrap(), ("next", "music"));
        assert_eq!(
            validate(&json!({ "action": "toggle", "app": "spotify" })).unwrap(),
            ("toggle", "spotify")
        );
        assert!(validate(&json!({})).unwrap_err().contains("action"));
        assert!(validate(&json!({ "action": "shuffle" })).unwrap_err().contains("play|pause|toggle|next|previous"));
        assert!(validate(&json!({ "action": "play", "app": "vlc" })).unwrap_err().contains("music|spotify"));
    }

    #[test]
    fn verb_tables() {
        assert_eq!(playerctl_verb("toggle"), "play-pause");
        assert_eq!(playerctl_verb("previous"), "previous");
        assert_eq!(media_vk("toggle"), Some(0xB3));
        assert_eq!(media_vk("next"), Some(0xB0));
        assert_eq!(media_vk("previous"), Some(0xB1));
        assert_eq!(media_vk("nope"), None);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(as_verb("toggle"), "playpause");
            assert_eq!(as_verb("next"), "next track");
            assert_eq!(as_verb("previous"), "previous track");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "controls real Music app; run with --ignored manually"]
    fn playback_toggle_manual() {
        let out = playback(&json!({ "action": "toggle" })).unwrap();
        assert_eq!(out["ok"], json!(true));
    }
}
