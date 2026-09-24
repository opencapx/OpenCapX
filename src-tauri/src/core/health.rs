//! Phase 40 — Plugin health check / auto-restart strategy (per-plugin override).
//!
//! Data model:
//! - `plugin_health_config` one row per plugin, fields: heartbeat_sec / ping_timeout_ms /
//!   max_retries / backoff_initial_ms / enabled.
//! - With no row, the watchdog uses hardcoded defaults (behavior compatible with Phase 10).
//!
//! Algorithm:
//! - `compute_backoff_ms(attempt, initial_ms)`: 2^(attempt-1) * initial_ms, capped at 30s.
//! - `validate(cfg)` covers 4 bounds; illegal values return Err to block the write.
//!
//! Constraints:
//! - The DB is keyed per-plugin, so an UPSERT means "change this plugin's policy".
//! - The Watchdog reads a snapshot from the store at startup and no longer hits the DB in the loop (avoiding shared_store lock contention).

use serde::{Deserialize, Serialize};

/// Per-plugin watchdog / ping config.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginHealthConfig {
    /// Heartbeat interval in seconds. 0 = disable ping, detecting the process via is_alive only (Phase 10 behavior).
    pub heartbeat_sec: u32,
    /// ping timeout (milliseconds). Only N consecutive non-responses count as failure.
    pub ping_timeout_ms: u32,
    /// Cumulative failure cap. Exceeding it → switch status="error" + emit a plugin.watchdog_disabled event.
    /// 0 = disable the watchdog (never auto-restart, unlike Phase 10's behavior).
    pub max_retries: u32,
    /// Backoff milliseconds before the first retry; doubles each retry after that, capped at 30s.
    pub backoff_initial_ms: u32,
    /// Master switch. false → the watchdog loop exits outright, equivalent to a manual stop.
    pub enabled: bool,
}

impl Default for PluginHealthConfig {
    fn default() -> Self {
        // Match Phase 10's hardcoded defaults: no ping, 3 retries, starting at 1s.
        Self {
            heartbeat_sec: 0,
            ping_timeout_ms: 1000,
            max_retries: 3,
            backoff_initial_ms: 1000,
            enabled: true,
        }
    }
}

/// Bounds + validation. Returns Err to block the write.
pub fn validate(cfg: &PluginHealthConfig) -> Result<(), String> {
    if cfg.heartbeat_sec > 3600 {
        return Err(format!(
            "heartbeat_sec out of range (0..=3600): {}",
            cfg.heartbeat_sec
        ));
    }
    if !(100..=60000).contains(&cfg.ping_timeout_ms) {
        return Err(format!(
            "ping_timeout_ms out of range (100..=60000): {}",
            cfg.ping_timeout_ms
        ));
    }
    if cfg.max_retries > 100 {
        return Err(format!(
            "max_retries out of range (0..=100): {}",
            cfg.max_retries
        ));
    }
    if cfg.backoff_initial_ms > 30_000 {
        return Err(format!(
            "backoff_initial_ms out of range (0..=30000): {}",
            cfg.backoff_initial_ms
        ));
    }
    Ok(())
}

/// Given the nth retry (1-indexed) and the initial backoff, returns how long to wait before retrying.
/// `2^(n-1) * initial`, capped at 30s (30_000ms).
pub fn compute_backoff_ms(attempt: u32, initial_ms: u32) -> u64 {
    if attempt == 0 {
        return 0;
    }
    let shift = (attempt - 1).min(10) as u32;
    let factor = 1u64 << shift;
    let raw = (initial_ms as u64).saturating_mul(factor);
    raw.min(30_000)
}

/// For the watchdog: turn a raw row into a cfg, falling back to defaults when missing / corrupt.
pub fn config_from_row(
    heartbeat: Option<i64>,
    ping_timeout: Option<i64>,
    max_retries: Option<i64>,
    backoff_initial: Option<i64>,
    enabled: Option<i64>,
) -> PluginHealthConfig {
    let mut cfg = PluginHealthConfig::default();
    if let Some(v) = heartbeat {
        cfg.heartbeat_sec = v.clamp(0, 3600) as u32;
    }
    if let Some(v) = ping_timeout {
        cfg.ping_timeout_ms = v.clamp(100, 60000) as u32;
    }
    if let Some(v) = max_retries {
        cfg.max_retries = v.clamp(0, 100) as u32;
    }
    if let Some(v) = backoff_initial {
        cfg.backoff_initial_ms = v.clamp(0, 30_000) as u32;
    }
    if let Some(v) = enabled {
        cfg.enabled = v != 0;
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_caps_at_30s() {
        assert_eq!(compute_backoff_ms(1, 1000), 1000);
        assert_eq!(compute_backoff_ms(2, 1000), 2000);
        assert_eq!(compute_backoff_ms(3, 1000), 4000);
        assert_eq!(compute_backoff_ms(4, 1000), 8000);
        assert_eq!(compute_backoff_ms(5, 1000), 16000);
        assert_eq!(compute_backoff_ms(6, 1000), 30000); // cap
        assert_eq!(compute_backoff_ms(20, 1000), 30000);
    }

    #[test]
    fn backoff_handles_zero_initial() {
        assert_eq!(compute_backoff_ms(3, 0), 0);
    }

    #[test]
    fn backoff_handles_zero_attempt() {
        assert_eq!(compute_backoff_ms(0, 1000), 0);
    }

    #[test]
    fn validate_accepts_defaults() {
        let cfg = PluginHealthConfig::default();
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn validate_rejects_out_of_range() {
        let mut cfg = PluginHealthConfig::default();
        cfg.heartbeat_sec = 3601;
        assert!(validate(&cfg).is_err());
        cfg.heartbeat_sec = 0;
        cfg.ping_timeout_ms = 50;
        assert!(validate(&cfg).is_err());
        cfg.ping_timeout_ms = 1000;
        cfg.max_retries = 101;
        assert!(validate(&cfg).is_err());
        cfg.max_retries = 0;
        cfg.backoff_initial_ms = 30_001;
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn config_from_row_falls_back_to_defaults() {
        let cfg = config_from_row(None, None, None, None, None);
        assert_eq!(cfg, PluginHealthConfig::default());

        let cfg = config_from_row(Some(30), Some(500), Some(5), Some(2000), Some(0));
        assert_eq!(cfg.heartbeat_sec, 30);
        assert_eq!(cfg.ping_timeout_ms, 500);
        assert_eq!(cfg.max_retries, 5);
        assert_eq!(cfg.backoff_initial_ms, 2000);
        assert!(!cfg.enabled);

        // Out-of-range values are clamped without error (a fallback for old data / SQLite corruption)
        let cfg = config_from_row(Some(99999), Some(1), Some(99999), Some(99999), Some(0));
        assert_eq!(cfg.heartbeat_sec, 3600);
        assert_eq!(cfg.ping_timeout_ms, 100);
        assert_eq!(cfg.max_retries, 100);
        assert_eq!(cfg.backoff_initial_ms, 30_000);
    }

    #[test]
    fn roundtrip_json() {
        let cfg = PluginHealthConfig {
            heartbeat_sec: 30,
            ping_timeout_ms: 1500,
            max_retries: 5,
            backoff_initial_ms: 2000,
            enabled: true,
        };
        let s = serde_json::to_string(&cfg).unwrap();
        let back: PluginHealthConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(cfg, back);
    }
}
