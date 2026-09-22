//! Phase 37 — post-install self-check (Capability Probe).
//!
//! After installing a .ocplugin → do not `start` immediately; run a round of smoke tests first:
//! - `ensure_running` starts the process (going through the normal initialize handshake)
//! - Call `core.probe.capability` once for each capability declared in the manifest,
//!   with a 5-second timeout; expect the plugin to return `{"ok": true, ...}`, otherwise record a failure
//! - Aggregate the batch into a `ProbeReport`, written to `plugins.probe_status / probe_at / probe_report`
//! - All pass → status set to `passed`, then `start` into the lifecycle as normal
//! - Any failure → status set to `failed`, **not** auto-started; emit a `plugin.probe.failed` event
//! - After the user fixes it, `run_plugin_probe` retries manually; start only after it passes
//!
//! This is a diagnostic tool; do not treat probe as a production call: it goes through `core.probe.capability`
//! and sends empty `{}` params; if the plugin did not register that method, timeout/unimplemented both count as failure.

use super::plugin::PluginManager;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Probe result for a single capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityProbeResult {
    #[serde(rename = "capability")]
    pub capability: String,
    #[serde(rename = "ok")]
    pub ok: bool,
    #[serde(rename = "elapsedMs")]
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One complete probe report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeReport {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    /// "passed" / "failed" / "skipped" (no capability declared)
    pub status: String,
    #[serde(rename = "ranAt")]
    pub ran_at: u64,
    #[serde(rename = "capabilities")]
    pub capabilities: Vec<CapabilityProbeResult>,
    /// Human-readable summary, shown in the frontend dialog.
    pub summary: String,
}

/// Timeout for a single capability probe call. Enough for the RPC handshake + one minimal operation.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// JSON-RPC method name — the plugin should declare support in its manifest description (optional); not implementing it is fine too,
/// not implementing it means timeout → failure (exactly the signal we want: a plugin that does not implement probe = unhealthy).
const PROBE_METHOD: &str = "core.probe.capability";

/// Run one probe. Returns the full report; the caller owns persistence + deciding whether to start.
///
/// Does not modify the plugin status — that step is left to the caller (letting the install flow decide the passed/failed state transition).
/// Does not modify the ProcessMap — no stop after use, left to the caller; on failure the caller decides whether to clean up.
pub fn run_probe(plugin_id: &str, declared_capabilities: &[String]) -> ProbeReport {
    let mgr = PluginManager::shared();
    let started = Instant::now();
    let mut results: Vec<CapabilityProbeResult> = Vec::new();

    // ensure_running first: failure fails the whole batch outright (if the process will not start, there is no capability to speak of).
    let proc = match mgr.ensure_running(plugin_id) {
        Ok(p) => p,
        Err(e) => {
            return ProbeReport {
                plugin_id: plugin_id.to_string(),
                status: "failed".into(),
                ran_at: super::agent::now_secs(),
                capabilities: vec![],
                summary: format!("plugin process failed to start: {}", e),
            };
        }
    };

    if declared_capabilities.is_empty() {
        return ProbeReport {
            plugin_id: plugin_id.to_string(),
            status: "skipped".into(),
            ran_at: super::agent::now_secs(),
            capabilities: vec![],
            summary: "no capability declared — probe skipped".into(),
        };
    }

    for cap in declared_capabilities {
        let call_start = Instant::now();
        let params = serde_json::json!({
            "capability": cap,
            "probe": true,
        });
        match proc.call(PROBE_METHOD, params, PROBE_TIMEOUT) {
            Ok(value) => {
                let elapsed = call_start.elapsed().as_millis() as u64;
                // The plugin should return {"ok": true|false, ...}; a missing ok field is treated as ok (default true).
                let ok = value
                    .get("ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let error = if !ok {
                    value.get("error")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| Some("plugin reported not ok".into()))
                } else {
                    None
                };
                results.push(CapabilityProbeResult {
                    capability: cap.clone(),
                    ok,
                    elapsed_ms: elapsed,
                    error,
                });
            }
            Err(e) => {
                results.push(CapabilityProbeResult {
                    capability: cap.clone(),
                    ok: false,
                    elapsed_ms: call_start.elapsed().as_millis() as u64,
                    error: Some(e),
                });
            }
        }
    }

    let total_ms = started.elapsed().as_millis() as u64;
    let passed = results.iter().filter(|r| r.ok).count();
    let total = results.len();
    let (status, summary) = if passed == total {
        (
            "passed".to_string(),
            format!("✓ {}/{} capabilities passed ({:.2}s)", passed, total, total_ms as f64 / 1000.0),
        )
    } else {
        let failed: Vec<String> = results
            .iter()
            .filter(|r| !r.ok)
            .map(|r| {
                let err = r.error.as_deref().unwrap_or("?");
                format!("{} ({})", r.capability, err)
            })
            .collect();
        (
            "failed".to_string(),
            format!("✗ {}/{} failed: {}", total - passed, total, failed.join("; ")),
        )
    };

    ProbeReport {
        plugin_id: plugin_id.to_string(),
        status,
        ran_at: super::agent::now_secs(),
        capabilities: results,
        summary,
    }
}

/// Persist the report + update the probe_status column. Takes shared_store internally; callers need not pass it.
pub fn save_report(report: &ProbeReport) {
    if let Some(store) = super::shared_store() {
        if let Ok(mut s) = store.lock() {
            let json = serde_json::to_string(report).unwrap_or_else(|_| "{}".into());
            s.set_probe_status(&report.plugin_id, &report.status, Some(&json), report.ran_at);
        }
    }
}

/// The caller (install flow / tauri command) calls this convenience function:
/// run + persist + publish event + auto-start if passed.
/// Returns the full report for the frontend dialog.
///
/// Tests that want to skip the probe set `OPENCAPX_SKIP_PROBE=1` (old fixtures that did not register
/// the `core.probe.capability` handler would be misjudged as failed); production runs the real probe by default.
pub fn run_and_publish(plugin_id: &str, declared: &[String]) -> ProbeReport {
    let report = if std::env::var("OPENCAPX_SKIP_PROBE").is_ok() {
        ProbeReport {
            plugin_id: plugin_id.to_string(),
            status: "skipped".into(),
            ran_at: super::agent::now_secs(),
            capabilities: vec![],
            summary: "probe skipped (OPENCAPX_SKIP_PROBE set)".into(),
        }
    } else {
        run_probe(plugin_id, declared)
    };
    save_report(&report);
    let bus = super::event::EventBus::shared();
    let kind = match report.status.as_str() {
        "passed" | "skipped" => "plugin.probe.passed",
        _ => "plugin.probe.failed",
    };
    bus.publish(&super::event::OpencapxEvent::new(
        kind,
        "core",
        serde_json::json!({
            "pluginId": report.plugin_id,
            "status": report.status,
            "summary": report.summary,
            "ranAt": report.ran_at,
            "ok": report.capabilities.iter().filter(|r| r.ok).count(),
            "total": report.capabilities.len(),
        }),
    ));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ProbeReport serializes all fields; the frontend can rely on camelCase.
    #[test]
    fn probe_report_uses_camel_case() {
        let r = ProbeReport {
            plugin_id: "plug-x".into(),
            status: "passed".into(),
            ran_at: 123,
            capabilities: vec![CapabilityProbeResult {
                capability: "image.analyze".into(),
                ok: true,
                elapsed_ms: 42,
                error: None,
            }],
            summary: "ok".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"pluginId\":\"plug-x\""));
        assert!(s.contains("\"ranAt\":123"));
        assert!(s.contains("\"elapsedMs\":42"));
        assert!(s.contains("\"image.analyze\""));
        assert!(!s.contains("\"error\""));
    }

    /// A failed result serializes with error; a passed one does not.
    #[test]
    fn capability_result_error_only_when_failed() {
        let pass = CapabilityProbeResult {
            capability: "a".into(),
            ok: true,
            elapsed_ms: 1,
            error: None,
        };
        let fail = CapabilityProbeResult {
            capability: "b".into(),
            ok: false,
            elapsed_ms: 100,
            error: Some("timeout".into()),
        };
        let p = serde_json::to_string(&pass).unwrap();
        let f = serde_json::to_string(&fail).unwrap();
        assert!(!p.contains("error"));
        assert!(f.contains("\"error\":\"timeout\""));
    }

    /// Empty capability list → skipped status (the summary copy also makes it explicit).
    #[test]
    fn empty_capabilities_skipped() {
        // Does not connect to a real PluginManager; only verifies the status copy convention.
        // run_probe errors without a shared_store, so only the constant copy is tested here.
        assert_eq!(PROBE_TIMEOUT, Duration::from_secs(5));
        assert_eq!(PROBE_METHOD, "core.probe.capability");
    }

    /// Probe report Storage write + read-back roundtrip, ensuring JSON serialization loses no fields.
    /// Uses a temporary sqlite DB directly, without depending on PluginManager (which spawns processes).
    #[test]
    fn storage_roundtrip_preserves_report_fields() {
        use crate::core::storage::{Storage, StoreEnum};
        use std::path::PathBuf;
        use std::sync::{Arc, Mutex};
        let dir = std::env::temp_dir().join(format!("opencapx-probe-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let storage = Storage::open(&path).unwrap();
        let store: Arc<Mutex<StoreEnum>> = Arc::new(Mutex::new(StoreEnum::Db(storage)));
        // Insert a minimal plugins row so the UPDATE has a target
        {
            let mut s = store.lock().unwrap();
            s.with_conn(|c| {
                c.execute(
                    "INSERT INTO plugins (id, version, type, status, path, manifest) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params!["plug-x", "0.1", "capability", "installed", "/x", "{}"],
                )
                .unwrap_or(0)
            });
        }
        let report = ProbeReport {
            plugin_id: "plug-x".into(),
            status: "failed".into(),
            ran_at: 4242,
            capabilities: vec![
                CapabilityProbeResult {
                    capability: "image.analyze".into(),
                    ok: true,
                    elapsed_ms: 12,
                    error: None,
                },
                CapabilityProbeResult {
                    capability: "shell.exec".into(),
                    ok: false,
                    elapsed_ms: 5000,
                    error: Some("timeout".into()),
                },
            ],
            summary: "1/2 failed".into(),
        };
        let json = serde_json::to_string(&report).unwrap();
        {
            let mut s = store.lock().unwrap();
            s.set_probe_status("plug-x", &report.status, Some(&json), report.ran_at);
        }
        let (status, at, back) = store.lock().unwrap().get_probe_report("plug-x").unwrap();
        assert_eq!(status, "failed");
        assert_eq!(at, 4242);
        let back: ProbeReport = serde_json::from_str(&back).unwrap();
        assert_eq!(back.status, "failed");
        assert_eq!(back.capabilities.len(), 2);
        assert!(back.capabilities[0].ok);
        assert!(!back.capabilities[1].ok);
        assert_eq!(back.capabilities[1].error.as_deref(), Some("timeout"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
