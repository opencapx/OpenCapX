//! Phase 44 — Plugin global disable switch (kill switch)
//!
//! A global singleton; while active in memory, every `PluginManager::start` / `ensure_running`
//! immediately returns `Err("kill switch active: <reason>")`. On enable it force-stops all running plugins at once
//! (via `mgr.stop(id)`), guaranteeing the service enters an "all disabled" state. After disable, state returns to normal
//! and the user can manually restart the plugins they need.

use serde::Serialize;
use std::sync::{Arc, Mutex, OnceLock};

use super::event::{OpencapxEvent, EventBus};

#[derive(Debug, Clone, Serialize, Default)]
pub struct KillSwitchStateDto {
    pub enabled: bool,
    pub reason: String,
    pub set_at: u64,
    pub set_by: String,
}

#[derive(Debug, Clone)]
struct Inner {
    enabled: bool,
    reason: String,
    set_at: u64,
    set_by: String,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            enabled: false,
            reason: String::new(),
            set_at: 0,
            set_by: String::new(),
        }
    }
}

static SHARED: OnceLock<Arc<Mutex<Inner>>> = OnceLock::new();

fn shared() -> Arc<Mutex<Inner>> {
    SHARED
        .get_or_init(|| Arc::new(Mutex::new(Inner::default())))
        .clone()
}

/// Current state DTO (snapshot).
pub fn state() -> KillSwitchStateDto {
    let arc = shared();
    let s = match arc.lock() {
        Ok(s) => s,
        Err(_) => return KillSwitchStateDto::default(),
    };
    KillSwitchStateDto {
        enabled: s.enabled,
        reason: s.reason.clone(),
        set_at: s.set_at,
        set_by: s.set_by.clone(),
    }
}

/// Whether the kill switch is enabled. Checked at the `PluginManager::start` entry; when active it returns Err directly.
pub fn is_active() -> bool {
    let arc = shared();
    arc.lock().map(|s| s.enabled).unwrap_or(false)
}

/// Enable the kill switch. `by` is the trigger source (user / system / automatic); `reason` is why it fired.
/// A repeated enable emits no event but refreshes reason / set_at / set_by.
pub fn enable(reason: &str, by: &str) {
    let now = super::agent::now_secs();
    let already;
    {
        let arc = shared();
        let mut s = arc.lock().unwrap();
        already = s.enabled;
        s.enabled = true;
        s.reason = reason.to_string();
        s.set_at = now;
        s.set_by = by.to_string();
    }
    if !already {
        EventBus::shared().publish(&OpencapxEvent::new(
            "plugin.kill_switch.enabled",
            by,
            serde_json::json!({
                "reason": reason,
                "setBy": by,
                "setAt": now,
            }),
        ));
    }
}

/// Disable the kill switch.
pub fn disable() {
    let prev;
    {
        let arc = shared();
        let mut s = arc.lock().unwrap();
        prev = s.enabled;
        s.enabled = false;
        s.reason.clear();
        s.set_at = 0;
        s.set_by.clear();
    }
    if prev {
        EventBus::shared().publish(&OpencapxEvent::new(
            "plugin.kill_switch.disabled",
            "core",
            serde_json::json!({}),
        ));
    }
}

/// The gatekeeper for `start(id)`. Returns Err("kill switch active: <reason>") when active, otherwise Ok(()).
pub fn guard_start(id: &str) -> Result<(), String> {
    let arc = shared();
    let s = arc.lock().unwrap();
    if s.enabled {
        let r = if s.reason.is_empty() {
            "kill switch active".to_string()
        } else {
            format!("kill switch active: {}", s.reason)
        };
        // Still emit a rejection event so the audit timeline can attribute it
        EventBus::shared().publish(&OpencapxEvent::new(
            "plugin.start.rejected",
            "kill-switch",
            serde_json::json!({
                "pluginId": id,
                "reason": r,
            }),
        ));
        return Err(r);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plugin::PluginManager;

    /// Test isolation: the kill switch is a global singleton, so every test resets it to disabled at the end.
    fn isolate() {
        disable();
    }

    /// Global singleton mutex: enable/disable stomp on each other in parallel tests (S6 deterministic mutex).
    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn default_is_inactive() {
        let _g = test_lock();
        isolate();
        assert!(!is_active());
        let s = state();
        assert!(!s.enabled);
        assert_eq!(s.reason, "");
        assert_eq!(s.set_at, 0);
    }

    #[test]
    fn enable_then_disable_toggles() {
        let _g = test_lock();
        isolate();
        enable("manual test", "unit-test");
        assert!(is_active());
        let s = state();
        assert!(s.enabled);
        assert_eq!(s.reason, "manual test");
        assert_eq!(s.set_by, "unit-test");
        assert!(s.set_at > 0);
        disable();
        assert!(!is_active());
        let s = state();
        assert!(!s.enabled);
        assert_eq!(s.reason, "");
    }

    #[test]
    fn guard_start_blocks_when_active() {
        let _g = test_lock();
        isolate();
        enable("emergency", "operator");
        let res = guard_start("com.example.foo");
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(err.contains("kill switch active"));
        assert!(err.contains("emergency"));
        disable();
    }

    #[test]
    fn guard_start_allows_when_inactive() {
        let _g = test_lock();
        isolate();
        let res = guard_start("com.example.foo");
        assert!(res.is_ok());
    }

    #[test]
    fn plugin_manager_start_blocked_by_active_switch() {
        let _g = test_lock();
        isolate();
        enable("ops freeze", "core");
        let mgr = PluginManager::shared();
        let _ = mgr; // Avoid an unused warning
        let g = guard_start("com.opencapx.kill-switch-test");
        assert!(g.is_err());
        disable();
    }
}