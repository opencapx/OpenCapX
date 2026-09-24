//! v1.4 system.settings builtin provider.
//!
//! Permission `system.settings` (ask): light settings on the appearance/volume tier. The desktop pet's persona moves
//! (change the wallpaper, toggle dark mode) alter the user-visible environment without touching data, so ask is enough.
//!
//! - dark_mode / wallpaper: AppleScript System Events (triggers "Automation" TCC)
//! - volume: osascript `set volume` (no TCC needed)
//! Non-macOS reports an honest error; plugins can override.

use serde_json::{json, Value};
use std::time::Duration;

const SET_TIMEOUT: Duration = Duration::from_secs(15);

/// Allowed image extensions for wallpapers (compared lowercased; the wallpaper directory also accepts tiff/heic).
const WALLPAPER_EXTS: &[&str] = &["png", "jpg", "jpeg", "tiff", "tif", "heic", "gif", "bmp"];

/// Input validation (pure function): (setting, value as already shape-checked JSON).
fn validate(input: &Value) -> Result<(&'static str, Value), String> {
    // Match to static literals (the input reference does not outlive the return value)
    let setting = match input.get("setting").and_then(|s| s.as_str()) {
        Some("dark_mode") => "dark_mode",
        Some("wallpaper") => "wallpaper",
        Some("volume") => "volume",
        Some(other) => {
            return Err(format!(
                "invalid input: setting must be dark_mode|wallpaper|volume, got {}",
                other
            ))
        }
        None => {
            return Err(
                "invalid input: setting (string) required, one of dark_mode|wallpaper|volume"
                    .into(),
            )
        }
    };
    let value = input.get("value").cloned().unwrap_or(Value::Null);
    match setting {
        "dark_mode" => {
            if !value.is_boolean() {
                return Err("invalid input: value (boolean) required for setting=dark_mode".into());
            }
        }
        "volume" => {
            let Some(v) = value.as_i64() else {
                return Err(
                    "invalid input: value (integer 0-100) required for setting=volume".into(),
                );
            };
            if !(0..=100).contains(&v) {
                return Err("invalid input: value must be 0..=100 for setting=volume".into());
            }
        }
        "wallpaper" => {
            let Some(p) = value.as_str() else {
                return Err(
                    "invalid input: value (path string) required for setting=wallpaper".into(),
                );
            };
            let meta = std::fs::metadata(p).map_err(|e| format!("wallpaper unavailable: {}", e))?;
            if !meta.is_file() {
                return Err(format!("not a file: {}", p));
            }
            let ext = p
                .rsplit_once('.')
                .map(|(_, e)| e.to_ascii_lowercase())
                .unwrap_or_default();
            if !WALLPAPER_EXTS.contains(&ext.as_str()) {
                return Err(format!(
                    "unsupported wallpaper extension .{} (allowed: {})",
                    ext,
                    WALLPAPER_EXTS.join(", ")
                ));
            }
        }
        // The match above already rejected unknown values; the three here are exhaustive
        _ => unreachable!(),
    }
    Ok((setting, value))
}

/// system.settings builtin implementation.
pub fn set(input: &Value) -> Result<Value, String> {
    let (setting, value) = validate(input)?;
    #[cfg(target_os = "macos")]
    {
        let src = match setting {
            "dark_mode" => format!(
                r#"tell application "System Events" to tell appearance preferences to set dark mode to {}"#,
                value.as_bool().unwrap()
            ),
            "volume" => format!("set volume output volume {}", value.as_i64().unwrap()),
            _ => {
                let p = value.as_str().unwrap();
                // AppleScript string-literal escaping: backslashes doubled, quotes backslash-escaped
                format!(
                    r#"tell application "System Events" to tell every desktop to set picture to POSIX file "{}""#,
                    p.replace('\\', "\\\\").replace('"', "\\\"")
                )
            }
        };
        let _ = super::appctl::osascript_ok(&src, SET_TIMEOUT, "set setting")?;
        Ok(json!({ "ok": true, "setting": setting }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (setting, value);
        Err("system.settings builtin is macOS-only (System Events); a plugin can override".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_setting_branches() {
        assert!(validate(&json!({ "setting": "dark_mode", "value": true })).is_ok());
        assert!(validate(&json!({ "setting": "volume", "value": 42 })).is_ok());
        let dir = std::env::temp_dir().join(format!("opencapx-set-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wp = dir.join("w.png");
        std::fs::write(&wp, b"x").unwrap();
        assert!(
            validate(&json!({ "setting": "wallpaper", "value": wp.to_str().unwrap() })).is_ok()
        );
        // setting missing / unknown / wrong type / volume out of range / wallpaper nonexistent / bad extension
        assert!(validate(&json!({})).unwrap_err().contains("setting"));
        assert!(validate(&json!({ "setting": "dock", "value": 1 }))
            .unwrap_err()
            .contains("dark_mode|wallpaper|volume"));
        assert!(validate(&json!({ "setting": "dark_mode", "value": 1 }))
            .unwrap_err()
            .contains("boolean"));
        assert!(validate(&json!({ "setting": "volume", "value": 101 }))
            .unwrap_err()
            .contains("0..=100"));
        assert!(validate(&json!({ "setting": "volume" }))
            .unwrap_err()
            .contains("integer"));
        assert!(validate(
            &json!({ "setting": "wallpaper", "value": dir.join("no.png").to_str().unwrap() })
        )
        .unwrap_err()
        .contains("unavailable"));
        let bad = dir.join("w.txt");
        std::fs::write(&bad, b"x").unwrap();
        assert!(
            validate(&json!({ "setting": "wallpaper", "value": bad.to_str().unwrap() }))
                .unwrap_err()
                .contains("unsupported wallpaper")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn set_reports_platform_limit() {
        assert!(set(&json!({ "setting": "volume", "value": 1 }))
            .unwrap_err()
            .contains("macOS-only"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "changes real system settings; run with --ignored manually"]
    fn set_volume_manual() {
        // The original value cannot be read back (osascript set volume is one-way); only verify the write succeeded
        assert_eq!(
            set(&json!({ "setting": "volume", "value": 50 })).unwrap()["ok"],
            json!(true)
        );
    }
}
