//! v1.3 automation.run builtin provider (docs/capability.md "builtin providers").
//! macOS AppleScript: launch, activate, or quit other apps by bundle id / app name,
//! or run a piece of AppleScript in the target app's context. Non-macOS has no AppleScript, so it reports an honest error
//! (a plugin can override).
//!
//! Permission `automation.control` (ask + high-risk, no Always): this is the "drive other apps
//! on the user's behalf" surface; AppleScript reaches far more app capability than this process, so authorization is per call.

use serde_json::{json, Value};
use std::io::Read;
use std::time::{Duration, Instant};

/// Cap for the app identifier (bundle id or app name).
const APP_MAX: usize = 256;
/// Cap for embedded AppleScript.
const SCRIPT_MAX: usize = 10_000;
/// Per-execution timeout: a stuck osascript (waiting on the app) must not drag down the agent call.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);
/// Cap on returned stdout (characters).
const OUTPUT_CAP: usize = 4_000;

/// Run a subprocess with a timeout: kill + error on timeout. stdout/stderr are collected by reader threads,
/// avoiding the child blocking on a full pipe buffer while we poll (the classic pipe deadlock).
/// Extracted as shared in v1.3: reused by audio.play (waiting for playback to finish) and pim (AppleScript / swift cold start).
pub(crate) fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {}", e))?;
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("command timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("wait failed: {}", e)),
        }
    }
    let status = child.wait().map_err(|e| format!("wait failed: {}", e))?;
    let stdout = t_out.join().unwrap_or_default();
    let stderr = t_err.join().unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout: stdout.into_bytes(),
        stderr: stderr.into_bytes(),
    })
}

/// Run a piece of AppleScript; a non-zero exit code is Err (ctx + stderr summary).
/// Extracted as shared in v1.4: reused by media / pim extensions (notes/reminders/mail).
#[cfg(target_os = "macos")]
pub(crate) fn osascript_ok(
    src: &str,
    timeout: Duration,
    ctx: &str,
) -> Result<std::process::Output, String> {
    let mut cmd = std::process::Command::new("osascript");
    cmd.arg("-e").arg(src);
    let out = run_with_timeout(cmd, timeout).map_err(|e| format!("{}: {}", ctx, e))?;
    if !out.status.success() {
        return Err(format!(
            "{} exited {:?}: {}",
            ctx,
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()
        ));
    }
    Ok(out)
}

/// Input validation (pure function): (app, action, script). app gets embedded into AppleScript source, so
/// quotes/control characters are rejected; action ∈ activate|launch|quit; when script is given, action is ignored.
fn validate(input: &Value) -> Result<(String, &'static str, Option<String>), String> {
    let Some(app) = input.get("app").and_then(|a| a.as_str()) else {
        return Err("invalid input: app (string) required".into());
    };
    if app.is_empty() {
        return Err("invalid input: app must be non-empty".into());
    }
    if app.len() > APP_MAX {
        return Err(format!("invalid input: app must be ≤{} chars", APP_MAX));
    }
    if app.contains('"') || app.bytes().any(|b| b.is_ascii_control()) {
        return Err("invalid input: app must not contain quotes or control chars".into());
    }
    let action = match input.get("action").and_then(|a| a.as_str()).unwrap_or("activate") {
        "activate" => "activate",
        "launch" => "launch",
        "quit" => "quit",
        other => {
            return Err(format!("invalid input: action must be activate|launch|quit, got {}", other))
        }
    };
    let script = input
        .get("script")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    if let Some(s) = &script {
        if s.len() > SCRIPT_MAX {
            return Err(format!("invalid input: script must be ≤{} chars", SCRIPT_MAX));
        }
    }
    Ok((app.to_string(), action, script))
}

/// App reference in AppleScript (pure function): reverse-domain style (com.apple.Safari) is treated as
/// a bundle id, everything else as an app name. app has passed validate, so interpolation is safe.
#[cfg(target_os = "macos")]
fn app_ref(app: &str) -> String {
    if app.contains('.') && !app.contains(' ') {
        format!("application id \"{}\"", app)
    } else {
        format!("application \"{}\"", app)
    }
}

/// AppleScript source (pure function): if script is given, wrap it in a tell block and run it; otherwise a one-line action.
#[cfg(target_os = "macos")]
fn applescript_for(app: &str, action: &str, script: &Option<String>) -> String {
    let target = app_ref(app);
    match script {
        Some(s) => format!("tell {}\n{}\nend tell", target, s),
        None => format!("tell {} to {}", target, action),
    }
}

/// automation.run builtin implementation.
pub fn run(input: &Value) -> Result<Value, String> {
    let (app, action, script) = validate(input)?;
    #[cfg(target_os = "macos")]
    {
        let src = applescript_for(&app, action, &script);
        let mut cmd = std::process::Command::new("osascript");
        cmd.arg("-e").arg(&src);
        let out = run_with_timeout(cmd, RUN_TIMEOUT)
            .map_err(|e| format!("automation failed: {}", e))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "automation exited {:?}: {} (macOS: grant Automation permission for this app in System Settings)",
                out.status.code(),
                err.chars().take(300).collect::<String>()
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let mut v = json!({ "ok": true });
        if !stdout.is_empty() {
            v["output"] = json!(stdout.chars().take(OUTPUT_CAP).collect::<String>());
        }
        Ok(v)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, action, script);
        Err("automation.run builtin is macOS-only (AppleScript); a plugin can override".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_app_action_script() {
        assert!(validate(&json!({ "app": "Safari" })).is_ok());
        let (app, action, script) = validate(&json!({ "app": "Safari" })).unwrap();
        assert_eq!(app, "Safari");
        assert_eq!(action, "activate", "default action");
        assert!(script.is_none());
        assert_eq!(validate(&json!({ "app": "Safari", "action": "quit" })).unwrap().1, "quit");
        // app missing / empty / quotes / control characters / over-long
        assert!(validate(&json!({})).unwrap_err().contains("app"));
        assert!(validate(&json!({ "app": "" })).unwrap_err().contains("non-empty"));
        assert!(validate(&json!({ "app": "Sa\"fari" })).unwrap_err().contains("quotes"));
        assert!(validate(&json!({ "app": "Sa\nfari" })).unwrap_err().contains("quotes"));
        assert!(validate(&json!({ "app": "x".repeat(257) })).unwrap_err().contains("256"));
        // Unknown action
        assert!(validate(&json!({ "app": "S", "action": "destroy" })).unwrap_err().contains("activate|launch|quit"));
        // script over-long
        assert!(validate(&json!({ "app": "S", "script": "x".repeat(10_001) }))
            .unwrap_err()
            .contains("10"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_variants() {
        assert_eq!(
            applescript_for("com.apple.Safari", "activate", &None),
            "tell application id \"com.apple.Safari\" to activate"
        );
        // Non-reverse-domain style is treated as an app name
        assert_eq!(
            applescript_for("Safari", "quit", &None),
            "tell application \"Safari\" to quit"
        );
        let s = applescript_for("Safari", "activate", &Some("count windows".into()));
        assert!(s.starts_with("tell application \"Safari\"\ncount windows\nend tell"));
    }

    /// run_with_timeout: a normal command returns stdout; a timeout gets killed.
    #[cfg(unix)]
    #[test]
    fn run_with_timeout_roundtrip_and_kill() {
        let out = run_with_timeout(
            {
                let mut c = std::process::Command::new("/bin/echo");
                c.arg("hi");
                c
            },
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
        // 30s sleep + 1s timeout → killed
        let err = run_with_timeout(
            {
                let mut c = std::process::Command::new("/bin/sleep");
                c.arg("30");
                c
            },
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(err.contains("timed out"), "{}", err);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn run_reports_platform_limit() {
        assert!(run(&json!({ "app": "X" })).unwrap_err().contains("macOS-only"));
    }
}
