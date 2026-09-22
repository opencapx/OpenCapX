//! v1.3 audio.play builtin provider (docs/capability.md "builtin providers").
//! macOS `afplay` / Linux `paplay` / Windows PowerShell Media.SoundPlayer (wav-only).
//!
//! Permission `audio.output` (ask): the audio-output channel, same tier as notification.post / speech
//! — a TOFU-registered agent should not be able to make noise by default, so ask each call.

use serde_json::{json, Value};
use std::time::Duration;

/// Wait cap when wait=true: kill the player on timeout, preventing an agent call from hanging.
const PLAY_TIMEOUT: Duration = Duration::from_secs(120);
/// Allowed audio extensions (compared lowercased).
const AUDIO_EXTS: &[&str] = &["wav", "mp3", "aiff", "aif", "aac", "m4a", "ogg", "flac"];

/// Input validation (pure function): (path, volume, wait). The file must already exist and be a file;
/// extension allowlist; volume ∈ [0,1].
fn validate(input: &Value) -> Result<(String, Option<f64>, bool), String> {
    let Some(path) = input.get("path").and_then(|p| p.as_str()) else {
        return Err("invalid input: path (string) required".into());
    };
    if path.is_empty() {
        return Err("invalid input: path must be non-empty".into());
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("audio file unavailable: {}", e))?;
    if !meta.is_file() {
        return Err(format!("not a file: {}", path));
    }
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !AUDIO_EXTS.contains(&ext.as_str()) {
        return Err(format!(
            "unsupported audio extension .{} (allowed: {})",
            ext,
            AUDIO_EXTS.join(", ")
        ));
    }
    let volume = input.get("volume").and_then(|v| v.as_f64());
    if let Some(v) = volume {
        if !(0.0..=1.0).contains(&v) {
            return Err("invalid input: volume must be between 0.0 and 1.0".into());
        }
    }
    let wait = input.get("wait").and_then(|w| w.as_bool()).unwrap_or(false);
    Ok((path.to_string(), volume, wait))
}

/// Linux-specific install hint.
#[cfg(target_os = "linux")]
fn player_hint() -> &'static str {
    " (install pulseaudio-utils)"
}
#[cfg(not(target_os = "linux"))]
fn player_hint() -> &'static str {
    ""
}

/// audio.play builtin implementation.
pub fn play(input: &Value) -> Result<Value, String> {
    let (path, volume, wait) = validate(input)?;

    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("afplay");
        if let Some(v) = volume {
            // afplay volume 0..=255 (linear)
            c.arg("-v").arg(((v * 255.0).round() as i64).clamp(0, 255).to_string());
        }
        c.arg(&path);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("paplay");
        if let Some(v) = volume {
            // paplay volume 0..=65536
            c.arg(format!("--volume={}", (v * 65536.0).round() as i64));
        }
        c.arg(&path);
        c
    };
    #[cfg(target_os = "windows")]
    let (mut cmd, wait) = {
        // The outer wait is overridden by the true below on Windows (SoundPlayer can only play synchronously), so consume it first
        let _ = wait;
        if volume.is_some() {
            return Err("volume not supported on Windows builtin (Media.SoundPlayer)".into());
        }
        if !path.to_ascii_lowercase().ends_with(".wav") {
            return Err("Windows builtin plays .wav only (Media.SoundPlayer)".into());
        }
        // SoundPlayer.Play() makes sound on a background thread and stops as soon as the host process exits — so play synchronously
        let script = format!(
            "$p=New-Object Media.SoundPlayer('{}'); $p.PlaySync()",
            path.replace('\'', "''")
        );
        let mut c = std::process::Command::new("powershell");
        c.args(["-NoProfile", "-Command", &script]);
        (c, true)
    };

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (path, volume, wait);
        return Err("audio.play builtin not supported on this platform".into());
    }
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        if wait {
            let out = super::appctl::run_with_timeout(cmd, PLAY_TIMEOUT)
                .map_err(|e| format!("audio playback failed: {}", e))?;
            if !out.status.success() {
                return Err(format!(
                    "audio player exited {:?}: {}{}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).chars().take(200).collect::<String>(),
                    player_hint()
                ));
            }
            Ok(json!({ "ok": true, "waited": true }))
        } else {
            // Do not wait for completion: spawn accepts right away (format/device issues are left to the system player)
            cmd.spawn()
                .map_err(|e| format!("audio player failed{}: {}", player_hint(), e))?;
            Ok(json!({ "ok": true, "waited": false }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_path_volume_ext() {
        let dir = std::env::temp_dir().join(format!("opencapx-audio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("a.wav");
        std::fs::write(&ok, b"RIFF").unwrap();
        let bad = dir.join("b.txt");
        std::fs::write(&bad, b"x").unwrap();

        assert!(validate(&json!({ "path": ok.to_str().unwrap() })).is_ok());
        let (_, vol, wait) = validate(&json!({ "path": ok.to_str().unwrap() })).unwrap();
        assert!(vol.is_none());
        assert!(!wait, "default is not to wait for completion");
        assert!(validate(&json!({ "path": ok.to_str().unwrap(), "volume": 0.5, "wait": true })).is_ok());
        // path missing / empty / nonexistent / not a file / bad extension / volume out of range
        assert!(validate(&json!({})).unwrap_err().contains("path"));
        assert!(validate(&json!({ "path": "" })).unwrap_err().contains("non-empty"));
        assert!(validate(&json!({ "path": dir.join("nope.wav").to_str().unwrap() }))
            .unwrap_err()
            .contains("unavailable"));
        assert!(validate(&json!({ "path": dir.to_str().unwrap() })).unwrap_err().contains("not a file"));
        assert!(validate(&json!({ "path": bad.to_str().unwrap() }))
            .unwrap_err()
            .contains("unsupported audio extension"));
        assert!(validate(&json!({ "path": ok.to_str().unwrap(), "volume": 1.5 }))
            .unwrap_err()
            .contains("volume"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    #[test]
    fn play_reports_platform_limit() {
        assert!(play(&json!({ "path": "/x.wav" })).unwrap_err().contains("not supported"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "plays real audio; run with --ignored manually"]
    fn play_system_sound_manual() {
        let out = play(&json!({ "path": "/System/Library/Sounds/Ping.aiff", "wait": true })).unwrap();
        assert_eq!(out["waited"], json!(true));
    }
}
