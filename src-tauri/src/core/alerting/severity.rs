//! severity hints engine: inheritance chain, effective severity, previews, hints CRUD.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Phase 53 — Public DTO for one severity hint (used by the settings UI + tauri commands).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeverityHintDto {
    pub source: String,
    pub severity: String,
    pub origin: String,
    #[serde(rename = "pluginId", skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    /// Phase 69 — the severity actually in effect (resolved through the chain; the UI can display it directly without recomputing).
    #[serde(rename = "effectiveSeverity")]
    pub effective_severity: String,
}

/// Phase 69 — 4-stage enum for the severity chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityPolicy {
    /// Phase 53 — hint declared by the plugin manifest.
    Manifest,
    /// Phase 53 — hardcoded default (the `severity_for_source` static table).
    PluginDefault,
    /// Phase 53 — hint set manually by the user.
    UserOverride,
    /// Phase 69 — the user explicitly disabled alerts for a source (currently only a placeholder, no set implemented).
    Disabled,
}

/// Phase 69 — each link on the chain, with `hit` marking whether it took effect.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeverityLink {
    pub policy: SeverityPolicy,
    pub severity: Option<Severity>,
    pub source: String,
    #[serde(rename = "pluginId", skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    pub hit: bool,
}

/// Phase 69 — impact report for a cascade delete.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CascadeReport {
    pub hint_deleted: bool,
    pub affected_routes: Vec<String>,
    pub affected_correlations: Vec<String>,
    pub affected_aggregations: Vec<String>,
}

/// Phase 53 — public mirror of the `alerting.severityHints` sub-structure (loaded by the plugin module).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AlertingManifest {
    #[serde(default)]
    pub severity_hints: std::collections::HashMap<String, String>,
}

/// Phase 69 — Produce the 4-layer chain for a source (always returns 4 links, ordered Manifest → PluginDefault → UserOverride → Disabled).
/// Each layer's `hit` marks whether it actually took effect for that source.
pub fn severity_inheritance_chain(source: &str) -> Vec<SeverityLink> {
    let user_hit = severity_hint_lookup(source, "user").is_some();
    let manifest_row =
        crate::core::storage::with_store(|s| s.get_alerting_severity_hint(source, "manifest"))
            .flatten();
    let manifest_hit = manifest_row.is_some() && !user_hit;
    let default_hit = manifest_row.is_none() && !user_hit;
    let mut chain = Vec::with_capacity(4);
    chain.push(SeverityLink {
        policy: SeverityPolicy::Manifest,
        severity: severity_hint_lookup(source, "manifest"),
        source: source.to_string(),
        plugin_id: manifest_row.and_then(|r| r.plugin_id),
        hit: manifest_hit,
    });
    chain.push(SeverityLink {
        policy: SeverityPolicy::PluginDefault,
        severity: Some(severity_for_source(source)),
        source: source.to_string(),
        plugin_id: None,
        hit: default_hit,
    });
    chain.push(SeverityLink {
        policy: SeverityPolicy::UserOverride,
        severity: severity_hint_lookup(source, "user"),
        source: source.to_string(),
        plugin_id: None,
        hit: user_hit,
    });
    chain.push(SeverityLink {
        policy: SeverityPolicy::Disabled,
        severity: None,
        source: source.to_string(),
        plugin_id: None,
        hit: false, // Phase 69 does not implement set yet
    });
    chain
}

/// Phase 69 — Produce the source's effective severity + which layer was hit.
pub fn effective_severity_with_reason(source: &str) -> (Severity, SeverityLink) {
    let chain = severity_inheritance_chain(source);
    let hit_link = chain.into_iter().find(|l| l.hit).unwrap_or(SeverityLink {
        policy: SeverityPolicy::PluginDefault,
        severity: Some(Severity::Info),
        source: source.to_string(),
        plugin_id: None,
        hit: false,
    });
    (hit_link.severity.unwrap_or(Severity::Info), hit_link)
}

/// Phase 69 — Delete the user-origin hint + list affected routes / correlations / aggregations.
/// Manifest hints are not deleted (managed by the plugin lifecycle).
pub fn cascade_delete_severity_hint(source: &str) -> CascadeReport {
    let user_deleted = delete_user_severity_hint(source);
    let routes = list_routes();
    let affected_routes: Vec<String> = routes
        .into_iter()
        .filter(|r| {
            r.kind_pattern == source
                || r.seen_in_last_json
                    .as_deref()
                    .and_then(|j| serde_json::from_str::<SeenInLastSpec>(j).ok())
                    .map(|s| s.pattern == source)
                    .unwrap_or(false)
        })
        .map(|r| r.name)
        .collect();
    let corrs = list_correlations();
    let affected_correlations: Vec<String> = corrs
        .into_iter()
        .filter(|c| c.kind_pattern_a == source || c.kind_pattern_b == source)
        .map(|c| c.name)
        .collect();
    let aggs = list_aggregations();
    let affected_aggregations: Vec<String> = aggs
        .into_iter()
        .filter(|a| a.kind_pattern == source)
        .map(|a| a.name)
        .collect();
    CascadeReport {
        hint_deleted: user_deleted,
        affected_routes,
        affected_correlations,
        affected_aggregations,
    }
}

/// Precedence chain: user hint → manifest hint → hardcode → Info. With no store it falls straight back to hardcode.
pub fn severity_resolved(source: &str) -> Severity {
    effective_severity_with_reason(source).0
}

/// Phase 70 — Route chain takes severity (equivalent to `severity_resolved`)
pub fn route_dispatch_severity(source: &str) -> Severity {
    severity_resolved(source)
}

/// Phase 70 — Correlation chain takes severity (equivalent to `severity_resolved`)
pub fn correlation_decision_severity(source: &str) -> Severity {
    severity_resolved(source)
}

/// Phase 70 — Aggregation chain takes severity (equivalent to `severity_resolved`)
pub fn aggregation_action_severity(source: &str) -> Severity {
    severity_resolved(source)
}

/// Phase 70 — Escalation chain takes severity (equivalent to `severity_resolved`)
pub fn escalation_target_severity(source: &str) -> Severity {
    severity_resolved(source)
}

/// Phase 70 — Propagation consistency trace DTO.
///
/// When the four main chains take severity for one source, which final value each uses + which link of the chain
/// it comes from. `severity_inheritance_chain` (Phase 69) shows the chain as a whole;
/// `propagation_trace` (Phase 70) shows how the chain's hit is applied across the four downstream chains.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropagationTrace {
    pub source: String,
    pub route_severity: Severity,
    pub route_origin: SeverityPolicy,
    pub correlation_severity: Severity,
    pub correlation_origin: SeverityPolicy,
    pub aggregation_severity: Severity,
    pub aggregation_origin: SeverityPolicy,
    pub escalation_severity: Severity,
    pub escalation_origin: SeverityPolicy,
}

/// Phase 70 — Take a source's effective severity + origin across the 4 main chains.
pub fn propagation_trace(source: &str) -> PropagationTrace {
    let (sev, link) = effective_severity_with_reason(source);
    PropagationTrace {
        source: source.to_string(),
        route_severity: sev,
        route_origin: link.policy,
        correlation_severity: sev,
        correlation_origin: link.policy,
        aggregation_severity: sev,
        aggregation_origin: link.policy,
        escalation_severity: sev,
        escalation_origin: link.policy,
    }
}

/// Phase 74 — one override hit record
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointSeverityOverrideHit {
    pub pattern: String,
    pub severity: Severity,
    pub index: usize,
}

/// Phase 74 — preview result for a single endpoint (propagation + override hit + final)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointPreviewRow {
    pub endpoint_id: String,
    pub endpoint_name: String,
    pub override_hit: Option<EndpointSeverityOverrideHit>,
    pub propagation_severity: Severity,
    pub final_envelope_severity: Severity,
}

/// Phase 74 — overall result of one preview (the source + one row per endpoint)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointSeverityPreview {
    pub source: String,
    pub endpoints: Vec<EndpointPreviewRow>,
}

/// Phase 74 — pure function: compute one preview row for a source + endpoint DTO
pub fn preview_endpoint_severity(source: &str, ep: &WebhookEndpoint) -> EndpointPreviewRow {
    let propagation = severity_resolved(source);
    let hit = ep
        .severity_overrides
        .iter()
        .enumerate()
        .find(|(_, (pattern, _))| kind_matches(pattern, source))
        .map(|(i, (pattern, sev))| EndpointSeverityOverrideHit {
            pattern: pattern.clone(),
            severity: *sev,
            index: i,
        });
    let final_sev = hit.as_ref().map(|h| h.severity).unwrap_or(propagation);
    EndpointPreviewRow {
        endpoint_id: ep.id.clone(),
        endpoint_name: ep.name.clone(),
        override_hit: hit,
        propagation_severity: propagation,
        final_envelope_severity: final_sev,
    }
}

/// Phase 74 — entry point: preview from the fanout perspective
/// endpoint_id = None → all enabled endpoints; Some(id) → only that endpoint (enabled is not filtered, letting the user dry-run a disabled one)
pub fn preview_alerting_endpoint_severity(
    source: &str,
    endpoint_id: Option<&str>,
) -> Result<EndpointSeverityPreview, String> {
    if source.trim().is_empty() {
        return Err("source is required".into());
    }
    // Phase 74 — preview must be computed after releasing the store lock, because severity_resolved calls
    // severity_inheritance_chain → with_store locks the same Mutex again (std::sync::Mutex is not reentrant).
    let targets: Vec<WebhookEndpoint> = {
        let store =
            crate::core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
        let s = store.lock().map_err(|_| "store poisoned".to_string())?;
        let StoreEnum::Db(db) = &*s else {
            return Err("sqlite store required".into());
        };
        let rows = db.list_alerting_endpoints();
        match endpoint_id {
            Some(id) if !id.is_empty() => rows
                .into_iter()
                .find(|r| r.id == id)
                .map(|r| vec![endpoint_row_to_dto(&r)])
                .unwrap_or_default(),
            _ => rows
                .iter()
                .filter(|r| r.enabled)
                .map(endpoint_row_to_dto)
                .collect(),
        }
        // the lock drops at the end of the block
    };
    let endpoints = targets
        .iter()
        .map(|ep| preview_endpoint_severity(source, ep))
        .collect();
    Ok(EndpointSeverityPreview {
        source: source.to_string(),
        endpoints,
    })
}

/// Look up the hint severity hit by (source, origin). No store / no row / invalid severity → None.
fn severity_hint_lookup(source: &str, origin: &str) -> Option<Severity> {
    let row = crate::core::storage::with_store(|s| s.get_alerting_severity_hint(source, origin))
        .flatten()?;
    Severity::parse(&row.severity)
}

/// User manually saves a hint. Validates a non-empty source + a legal severity. Added in Phase 53.
pub fn save_user_severity_hint(source: &str, severity: &str) -> Result<SeverityHintDto, String> {
    if source.trim().is_empty() {
        return Err("source is required".into());
    }
    let sev = Severity::parse(severity).ok_or_else(|| {
        format!(
            "invalid severity: {} (expected info|warn|error|critical)",
            severity
        )
    })?;
    let now = crate::core::agent::now_secs() as i64;
    crate::core::storage::with_store(|s| {
        s.upsert_alerting_severity_hint(source, sev.as_str(), "user", None, now)
    })
    .ok_or_else(|| "storage unavailable".to_string())??;
    // re-read once to ensure the DTO fields are complete
    let row = crate::core::storage::with_store(|s| s.get_alerting_severity_hint(source, "user"))
        .ok_or_else(|| "storage unavailable".to_string())?
        .ok_or_else(|| "hint not persisted".to_string())?;
    Ok(SeverityHintDto {
        source: row.source.clone(),
        severity: row.severity,
        origin: row.origin,
        plugin_id: row.plugin_id,
        updated_at: row.updated_at,
        effective_severity: severity_resolved(&row.source).as_str().to_string(),
    })
}

/// Delete one user-origin hint. Added in Phase 53.
pub fn delete_user_severity_hint(source: &str) -> bool {
    crate::core::storage::with_store(|s| {
        s.delete_alerting_severity_hint_by_source_and_origin(source, "user")
    })
    .unwrap_or(false)
}

/// List all hints (any origin). Added in Phase 53.
pub fn list_severity_hints() -> Vec<SeverityHintDto> {
    crate::core::storage::with_store(|s| s.list_alerting_severity_hints())
        .unwrap_or_default()
        .into_iter()
        .map(|r| SeverityHintDto {
            source: r.source.clone(),
            severity: r.severity,
            origin: r.origin,
            plugin_id: r.plugin_id,
            updated_at: r.updated_at,
            effective_severity: severity_resolved(&r.source).as_str().to_string(),
        })
        .collect()
}

/// Clear all user-origin hints. Added in Phase 53.
pub fn clear_user_severity_hints() -> usize {
    crate::core::storage::with_store(|s| s.clear_user_alerting_severity_hints()).unwrap_or(0)
}

/// Load the hints declared by a manifest (called on plugin install). Added in Phase 53.
/// Missing / no hint / invalid severity → silently skipped, without affecting the install main flow.
pub fn install_manifest_hints(plugin_id: &str, alerting: &Option<AlertingManifest>) {
    let Some(a) = alerting else {
        return;
    };
    let now = crate::core::agent::now_secs() as i64;
    for (source, severity_str) in &a.severity_hints {
        if let Some(sev) = Severity::parse(severity_str) {
            let _ = crate::core::storage::with_store(|s| {
                s.upsert_alerting_severity_hint(
                    source,
                    sev.as_str(),
                    "manifest",
                    Some(plugin_id),
                    now,
                )
            });
        }
    }
}

/// Unload manifest-origin hints (called on plugin uninstall). Added in Phase 53.
pub fn uninstall_manifest_hints(plugin_id: &str) -> usize {
    crate::core::storage::with_store(|s| {
        s.delete_alerting_severity_hints_by_origin_plugin("manifest", plugin_id)
    })
    .unwrap_or(0)
}
