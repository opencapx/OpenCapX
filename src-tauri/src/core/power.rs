//! v1.4 system.sleep / system.lock builtin provider.
//!
//! Permission `power.control` (denied + high-risk): sleep/lock change machine-global state,
//! irreversible for running tasks; denied by default, only allow-once at runtime, per call.
//!
//! One system command per platform, no input parameters:
//! - sleep: macOS `pmset sleepnow` / Linux `systemctl suspend` /
//!   Windows `rundll32 powrprof.dll,SetSuspendState` (known limitation: with hibernation enabled
//!   on Windows this hibernates instead of sleeping — an OS-layer behavior the builtin cannot distinguish; noted in the docs)
//! - lock: macOS CGSession (when present; falls back to System Events Ctrl+Cmd+Q, which needs Accessibility
//!   ) / Linux `loginctl lock-session` / Windows `rundll32 user32.dll,LockWorkStation`

use serde_json::{json, Value};
use std::time::Duration;

/// Power-command timeout: the command returns at once; the timeout is only a backstop.
const POWER_TIMEOUT: Duration = Duration::from_secs(15);

/// The macOS CGSession binary (the standard lock-screen backdoor through Sonoma; prefer it when present,
/// no TCC; fall back to keystroke when missing, which needs Accessibility).
#[cfg(target_os = "macos")]
const CGSESSION: &str =
    "/System/Library/CoreServices/Menu Extras/User.menu/Contents/Resources/CGSession";

fn run(cmd: std::process::Command, doing: &str) -> Result<Value, String> {
    let out = super::appctl::run_with_timeout(cmd, POWER_TIMEOUT)
        .map_err(|e| format!("{} failed: {}", doing, e))?;
    if !out.status.success() {
        return Err(format!(
            "{} exited {:?}: {}",
            doing,
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }
    Ok(json!({ "ok": true }))
}

/// system.sleep builtin implementation.
pub fn sleep(_input: &Value) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    return run(
        {
            let mut c = std::process::Command::new("pmset");
            c.arg("sleepnow");
            c
        },
        "sleep",
    );
    #[cfg(target_os = "linux")]
    return run(
        {
            let mut c = std::process::Command::new("systemctl");
            c.arg("suspend");
            c
        },
        "sleep",
    );
    #[cfg(target_os = "windows")]
    return run(
        {
            let mut c = std::process::Command::new("rundll32.exe");
            c.args(["powrprof.dll,SetSuspendState", "0,1,0"]);
            c
        },
        "sleep",
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err("system.sleep builtin not supported on this platform".into())
    }
}

/// system.lock builtin implementation.
pub fn lock(_input: &Value) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        if std::path::Path::new(CGSESSION).exists() {
            return run(
                {
                    let mut c = std::process::Command::new(CGSESSION);
                    c.arg("-suspend");
                    c
                },
                "lock",
            );
        }
        // Fallback: Ctrl+Cmd+Q (needs Accessibility, report honestly first)
        if !super::osperm::ffi::ax_trusted() {
            return Err(
                "lock requires Accessibility permission for OpenCapX (System Settings → Privacy & Security → Accessibility)"
                    .into(),
            );
        }
        let src = r#"tell application "System Events" to keystroke "q" using {command down, control down}"#;
        let _ = super::appctl::osascript_ok(src, POWER_TIMEOUT, "lock")?;
        return Ok(json!({ "ok": true }));
    }
    #[cfg(target_os = "linux")]
    return run(
        {
            let mut c = std::process::Command::new("loginctl");
            c.arg("lock-session");
            c
        },
        "lock",
    );
    #[cfg(target_os = "windows")]
    return run(
        {
            let mut c = std::process::Command::new("rundll32.exe");
            c.arg("user32.dll,LockWorkStation");
            c
        },
        "lock",
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err("system.lock builtin not supported on this platform".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn cgsession_path_shape() {
        assert!(CGSESSION.starts_with("/System/Library/"));
        assert!(CGSESSION.ends_with("CGSession"));
    }

    #[test]
    #[ignore = "puts the machine to sleep NOW; run with --ignored manually"]
    fn sleep_manual() {
        assert_eq!(sleep(&json!({})).unwrap()["ok"], json!(true));
    }

    #[test]
    #[ignore = "locks the screen NOW; run with --ignored manually"]
    fn lock_manual() {
        assert_eq!(lock(&json!({})).unwrap()["ok"], json!(true));
    }
}
