//! webhook config load/save + validation + the Phase 72 per-endpoint severity overrides.
//! Mechanical move from core/alerting.rs.

use super::*;

pub fn validate_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("url is required when webhook is enabled".into());
    }
    if url.len() > 2048 {
        return Err("url too long (max 2048 chars)".into());
    }
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("https://") && !lower.starts_with("http://") {
        return Err("url must start with http:// or https://".into());
    }
    // guard against header injection / path traversal characters
    if url.contains('\n') || url.contains('\r') || url.contains('\0') {
        return Err("url contains invalid control characters".into());
    }
    Ok(())
}

pub fn validate(cfg: &WebhookConfig) -> Result<(), String> {
    if !cfg.enabled {
        return Ok(()); // when disabled the url may be empty, and the other fields are not enforced either
    }
    validate_url(&cfg.url)?;
    if cfg.min_interval_secs > 3600 {
        return Err(format!(
            "min_interval_secs out of range (0..=3600): {}",
            cfg.min_interval_secs
        ));
    }
    for (k, _) in &cfg.custom_headers {
        if k.is_empty() || k.contains('\n') || k.contains('\r') {
            return Err("custom header name invalid".into());
        }
    }
    Ok(())
}

/// Phase 49 — Validate endpoint arguments.
pub fn validate_endpoint(ep: &WebhookEndpoint) -> Result<(), String> {
    let name = ep.name.trim();
    if name.is_empty() {
        return Err("endpoint name is required".into());
    }
    if name.len() > 128 {
        return Err("endpoint name too long (max 128 chars)".into());
    }
    validate_url(&ep.url)?;
    for (k, _) in &ep.headers {
        if k.is_empty() || k.contains('\n') || k.contains('\r') {
            return Err("endpoint header name invalid".into());
        }
    }
    if ep.secret.len() > 1024 {
        return Err("endpoint secret too long (max 1024 chars)".into());
    }
    // source_filter is limited to the 4 known kinds (prevents the frontend from passing arbitrary strings).
    for s in &ep.source_filter {
        if !matches!(
            s.as_str(),
            "plugin.metrics.exceeded"
                | "capability.sla.violated"
                | "plugin.kill_switch.enabled"
                | "plugin.lifecycle.crashed"
        ) {
            return Err(format!("unknown source filter: {}", s));
        }
    }
    // Phase 72 — severity_overrides validation (empty / length / valid severity string)
    for (i, (src, sev)) in ep.severity_overrides.iter().enumerate() {
        if src.trim().is_empty() {
            return Err(format!("severity_overrides[{}]: source is required", i));
        }
        if src.len() > 256 {
            return Err(format!(
                "severity_overrides[{}]: source too long (max 256 chars)",
                i
            ));
        }
        if Severity::parse(sev.as_str()).is_none() {
            return Err(format!(
                "severity_overrides[{}]: invalid severity '{}' (expected info|warn|error|critical)",
                i,
                sev.as_str()
            ));
        }
    }
    Ok(())
}

/// Given a source + endpoint override list, find the **first** matching severity by glob semantics, or None.
/// glob rules (same as Phase 51 `kind_matches`):
///   - `*` or `""` → matches all sources
///   - `prefix.*` → matches a prefix start + at least one character after `.`
///   - `exact` → exact match
/// Order: iterate in Vec order, **first-match wins** (consistent with the Phase 51 route DSL).
pub fn endpoint_severity_override_for(
    source: &str,
    overrides: &[(String, Severity)],
) -> Option<Severity> {
    overrides
        .iter()
        .find(|(pattern, _)| kind_matches(pattern, source))
        .map(|(_, sev)| *sev)
}

/// During fanout: if there is an endpoint override, overwrite envelope_severity; otherwise keep the propagation result
pub fn apply_endpoint_severity_override(
    envelope_severity: &mut Severity,
    source: &str,
    overrides: &[(String, Severity)],
) {
    if let Some(sev) = endpoint_severity_override_for(source, overrides) {
        *envelope_severity = sev;
    }
}

pub fn load_config() -> WebhookConfig {
    let Some(store) = crate::core::shared_store() else {
        return WebhookConfig::default();
    };
    store
        .lock()
        .ok()
        .and_then(|s| s.get_setting(CONFIG_KEY))
        .and_then(|json| serde_json::from_str::<WebhookConfig>(&json).ok())
        .unwrap_or_default()
}

pub fn save_config(cfg: &WebhookConfig) -> Result<(), String> {
    validate(cfg)?;
    let store = crate::core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let mut s = store.lock().map_err(|_| "store poisoned".to_string())?;
    let json = serde_json::to_string(cfg).map_err(|e| format!("serialize: {}", e))?;
    s.set_setting(CONFIG_KEY, &json);
    Ok(())
}
