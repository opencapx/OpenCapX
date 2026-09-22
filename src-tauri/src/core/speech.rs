//! v1.2 speech.synthesize builtin provider (docs/capability.md "builtin providers").
//! Platform TTS engines: macOS `say` / Linux `espeak` / Windows PowerShell System.Speech.
//! Audio lands in `~/.opencapx/cache/` (reusing vision::cache_dir), returning `{audio: path}`.
//!
//! Since v1.2 the permission maps to `notification.post` (it was storage.local, granted by default):
//! speech output and notifications belong to the same human-facing channel with the same spam/phishing surface, so they should go through ask.

use serde_json::{json, Value};

/// Input validation + speed conversion: text is required and non-empty, speed ∈ [0.5, 2.0].
/// Returns (text, voice, wpm): wpm = 175 × speed (the words-per-minute rate for espeak/say).
fn validate(input: &Value) -> Result<(String, Option<String>, u32), String> {
    let Some(text) = input.get("text").and_then(|t| t.as_str()) else {
        return Err("invalid input: text (string) required".into());
    };
    if text.trim().is_empty() {
        return Err("invalid input: text must not be empty".into());
    }
    let voice = input
        .get("voice")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string());
    let speed = input.get("speed").and_then(|s| s.as_f64()).unwrap_or(1.0);
    if !(0.5..=2.0).contains(&speed) {
        return Err("invalid input: speed must be between 0.5 and 2.0".into());
    }
    Ok((text.to_string(), voice, (175.0 * speed).round() as u32))
}

/// Output filename timestamp (same style as vision.rs screenshots).
fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// PowerShell single-quote escaping: ' doubles inside a single-quoted string. Pure function, testable on all three platforms.
fn escape_ps_single_quoted(s: &str) -> String {
    s.replace('\'', "''")
}

/// say arguments: output file + optional voice (-v)/rate (-r, words per minute) + text. AIFF container.
#[cfg(target_os = "macos")]
fn say_args(out: &str, text: &str, voice: &Option<String>, wpm: u32) -> Vec<String> {
    let mut args = vec!["-o".to_string(), out.to_string()];
    if let Some(v) = voice {
        args.push("-v".into());
        args.push(v.clone());
    }
    if wpm != 175 {
        args.push("-r".into());
        args.push(wpm.to_string());
    }
    args.push(text.to_string());
    args
}

/// espeak arguments: output wav (-w) + optional voice (-v)/rate (-s, words per minute).
#[cfg(target_os = "linux")]
fn espeak_args(out: &str, text: &str, voice: &Option<String>, wpm: u32) -> Vec<String> {
    let mut args = vec!["-w".to_string(), out.to_string()];
    if let Some(v) = voice {
        args.push("-v".into());
        args.push(v.clone());
    }
    if wpm != 175 {
        args.push("-s".into());
        args.push(wpm.to_string());
    }
    args.push(text.to_string());
    args
}

/// speech.synthesize builtin implementation. Success = engine exit code 0 and the output file exists.
pub fn synthesize(input: &Value) -> Result<Value, String> {
    let (text, voice, wpm) = validate(input)?;
    let dir = super::vision::cache_dir()?;

    #[cfg(target_os = "macos")]
    let (out_path, mut cmd) = {
        let out = dir.join(format!("speech-{}.aiff", nanos()));
        let mut c = std::process::Command::new("say");
        c.args(say_args(
            &out.to_string_lossy(),
            &text,
            &voice,
            wpm,
        ));
        (out, c)
    };
    #[cfg(target_os = "linux")]
    let (out_path, mut cmd) = {
        let out = dir.join(format!("speech-{}.wav", nanos()));
        let mut c = std::process::Command::new("espeak");
        c.args(espeak_args(
            &out.to_string_lossy(),
            &text,
            &voice,
            wpm,
        ));
        (out, c)
    };
    #[cfg(target_os = "windows")]
    let (out_path, mut cmd) = {
        let out = dir.join(format!("speech-{}.wav", nanos()));
        // System.Speech Rate ∈ [-10, 10]; speed=1.0 → 0
        let rate10 = (((input.get("speed").and_then(|s| s.as_f64()).unwrap_or(1.0) - 1.0)
            * 10.0)
            .round() as i64)
            .clamp(-10, 10);
        let mut script = String::from(
            "Add-Type -AssemblyName System.Speech; \
             $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; \
             $s.SetOutputToWaveFile('",
        );
        script.push_str(&escape_ps_single_quoted(&out.to_string_lossy()));
        script.push_str("');");
        if let Some(v) = &voice {
            script.push_str("$s.SelectVoice('");
            script.push_str(&escape_ps_single_quoted(v));
            script.push_str("');");
        }
        script.push_str(&format!("$s.Rate={};", rate10));
        script.push_str("$s.Speak('");
        script.push_str(&escape_ps_single_quoted(&text));
        script.push_str("');$s.Dispose()");
        let mut c = std::process::Command::new("powershell");
        c.args(["-NoProfile", "-Command", &script]);
        (out, c)
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (text, voice, wpm, &dir);
        return Err("speech.synthesize builtin not supported on this platform".into());
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        let status = cmd
            .status()
            .map_err(|e| {
                #[cfg(target_os = "linux")]
                let hint = " (install espeak)";
                #[cfg(not(target_os = "linux"))]
                let hint = "";
                format!("speech engine failed{}: {}", hint, e)
            })?;
        if !status.success() {
            return Err(format!("speech engine exited {:?}", status.code()));
        }
        if !out_path.exists() {
            return Err("speech engine produced no file".into());
        }
        Ok(json!({ "audio": out_path.to_string_lossy() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_text_and_speed() {
        assert!(validate(&json!({ "text": "hello" })).is_ok());
        let err = validate(&json!({})).unwrap_err();
        assert!(err.contains("text"), "{}", err);
        let err = validate(&json!({ "text": "  " })).unwrap_err();
        assert!(err.contains("empty"), "{}", err);
        let err = validate(&json!({ "text": "x", "speed": 3.0 })).unwrap_err();
        assert!(err.contains("speed"), "{}", err);
        // Bounds are inclusive
        assert!(validate(&json!({ "text": "x", "speed": 0.5 })).is_ok());
        assert!(validate(&json!({ "text": "x", "speed": 2.0 })).is_ok());
    }

    #[test]
    fn speed_maps_to_wpm() {
        let (_, _, wpm) = validate(&json!({ "text": "x", "speed": 2.0 })).unwrap();
        assert_eq!(wpm, 350);
        let (_, _, wpm) = validate(&json!({ "text": "x", "speed": 0.5 })).unwrap();
        assert_eq!(wpm, 88);
        let (_, _, wpm) = validate(&json!({ "text": "x" })).unwrap();
        assert_eq!(wpm, 175);
    }

    #[test]
    fn escapes_ps_single_quotes() {
        assert_eq!(escape_ps_single_quoted("it's"), "it''s");
        assert_eq!(escape_ps_single_quoted("plain"), "plain");
        assert_eq!(escape_ps_single_quoted("a'''b"), "a''''''b");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn say_args_full_voice_rate() {
        let a = say_args("/tmp/x.aiff", "hi there", &Some("Tingting".into()), 350);
        assert_eq!(a, vec!["-o", "/tmp/x.aiff", "-v", "Tingting", "-r", "350", "hi there"]);
        // The default rate does not include -r
        let a = say_args("/tmp/x.aiff", "hi", &None, 175);
        assert_eq!(a, vec!["-o", "/tmp/x.aiff", "hi"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "runs real `say`; run with --ignored manually"]
    fn synthesize_real_say_manual() {
        let out = synthesize(&json!({ "text": "OpenCapX speech test" })).unwrap();
        let p = out["audio"].as_str().unwrap();
        assert!(std::path::Path::new(p).exists());
        assert!(std::fs::metadata(p).unwrap().len() > 0);
        let _ = std::fs::remove_file(p);
    }
}
