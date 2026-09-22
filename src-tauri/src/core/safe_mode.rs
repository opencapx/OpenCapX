//! `--safe-mode`: start only the Core, loading no third-party plugins.
//!
//! Division of labor with kill_switch: the kill switch is a **runtime** emergency stop (can be toggled anytime);
//! safe mode is a **startup-time** diagnostic mode — when plugins repeatedly break the startup flow, or you need to determine
//! "is the problem in the core or the plugin", start with `OpenCapX --safe-mode`. Startup recovery skips
//! all third-party plugins, and a manual start is rejected too (the error carries the reason). Builtin capabilities are unaffected.

use std::sync::atomic::{AtomicBool, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set by a CLI flag early in startup; it never changes for the life of the process (restart the app to leave safe mode).
pub fn set_active(v: bool) {
    ACTIVE.store(v, Ordering::SeqCst);
}

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// Same shape as `kill_switch::guard_start`: `start_inner` calls it before any state transition.
/// When active it rejects and emits `plugin.start.rejected` (source = safe-mode).
pub fn guard_start(id: &str) -> Result<(), String> {
    if !is_active() {
        return Ok(());
    }
    let reason = "safe mode active: plugin start rejected (--safe-mode)".to_string();
    super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
        "plugin.start.rejected",
        "safe-mode",
        serde_json::json!({ "pluginId": id, "reason": &reason }),
    ));
    Err(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        set_active(false);
    }

    #[test]
    fn guard_allows_when_inactive() {
        reset();
        assert!(guard_start("com.example.foo").is_ok());
        assert!(!is_active());
    }

    #[test]
    fn guard_blocks_when_active() {
        reset();
        set_active(true);
        let err = guard_start("com.example.foo").unwrap_err();
        assert!(err.contains("safe mode"), "unexpected error: {err}");
        reset();
    }

    #[test]
    fn flag_roundtrip() {
        reset();
        assert!(!is_active());
        set_active(true);
        assert!(is_active());
        reset();
    }
}
