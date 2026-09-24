//! frequency-threshold aggregation rules.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Phase 54 — Public DTO for frequency threshold rules (used by the settings UI + tauri commands).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregationRuleDto {
    pub id: String,
    pub name: String,
    #[serde(rename = "kindPattern")]
    pub kind_pattern: String,
    #[serde(rename = "windowSecs")]
    pub window_secs: u64,
    #[serde(rename = "thresholdCount")]
    pub threshold_count: u32,
    pub action: String, // "downgrade" | "suppress" | "merge"
    #[serde(rename = "targetSeverity", skip_serializing_if = "Option::is_none")]
    pub target_severity: Option<String>,
    pub enabled: bool,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// Phase 54 — argument struct for the settings UI form (id optional, auto-filled by save_aggregation).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregationRule {
    pub id: String,
    pub name: String,
    #[serde(rename = "kindPattern")]
    pub kind_pattern: String,
    #[serde(rename = "windowSecs")]
    pub window_secs: u64,
    #[serde(rename = "thresholdCount")]
    pub threshold_count: u32,
    pub action: String,
    #[serde(rename = "targetSeverity", skip_serializing_if = "Option::is_none")]
    pub target_severity: Option<String>,
    pub enabled: bool,
}

/// Phase 54 — decision result of the aggregation engine. `Pass` = do not modify the envelope; `Downgrade` = rewrite severity;
/// `Suppress` = drop the whole event; `Merge` = replace the original payload with a count summary.
/// Phase 71 — adds `propagated_severity: Severity` so the dispatch main path sees the source's effective severity
/// (provided by `aggregation_action_severity(source)`, consistent across chains)
#[derive(Debug, Clone, PartialEq)]
pub enum AggregationDecision {
    Pass,
    Downgrade(Severity, Severity), // (target_severity, propagated_severity)
    Suppress {
        propagated_severity: Severity,
    }, // Phase 71
    Merge {
        count: u64,
        since: u64,
        last_payload: serde_json::Value,
        propagated_severity: Severity, // Phase 71
    },
}

/// Internal bucket: a queue of hit timestamps for one source within window_secs + the last fired time.
#[derive(Debug, Default)]
pub(crate) struct AggregationBucket {
    pub(crate) events: std::collections::VecDeque<u64>,
    pub(crate) last_fired: u64,
}

static AGGREGATION_STATE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, AggregationBucket>>,
> = std::sync::OnceLock::new();

pub(crate) fn aggregation_state(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, AggregationBucket>> {
    AGGREGATION_STATE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn rule_from_row(r: AggregationRuleRow) -> AggregationRule {
    AggregationRule {
        id: r.id,
        name: r.name,
        kind_pattern: r.kind_pattern,
        window_secs: r.window_secs,
        threshold_count: r.threshold_count,
        action: r.action,
        target_severity: r.target_severity,
        enabled: r.enabled,
    }
}

fn dto_from_row(r: AggregationRuleRow) -> AggregationRuleDto {
    AggregationRuleDto {
        id: r.id,
        name: r.name,
        kind_pattern: r.kind_pattern,
        window_secs: r.window_secs,
        threshold_count: r.threshold_count,
        action: r.action,
        target_severity: r.target_severity,
        enabled: r.enabled,
        created_at: r.created_at,
    }
}

pub(crate) fn load_enabled_aggregation_rules() -> Vec<AggregationRule> {
    crate::core::shared_store()
        .and_then(|s| {
            let g = s.lock().ok()?;
            let rows = g.list_enabled_alerting_aggregations();
            Some(rows.into_iter().map(rule_from_row).collect())
        })
        .unwrap_or_default()
}

/// Phase 54 — glob-style pattern matching: supports `*`, `prefix*`, `prefix.*`, and exact match.
pub(crate) fn aggregation_kind_matches(pattern: &str, kind: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern == kind {
        return true;
    }
    if let Some(p) = pattern.strip_suffix(".*") {
        if kind.starts_with(p) && kind.len() > p.len() && kind.as_bytes()[p.len()] == b'.' {
            return true;
        }
    }
    if let Some(p) = pattern.strip_suffix('*') {
        if kind.starts_with(p) {
            return true;
        }
    }
    false
}

/// Phase 54 — Validate rule fields. downgrade needs target_severity; window / threshold must be > 0.
pub fn validate_aggregation(r: &AggregationRule) -> Result<(), String> {
    if r.id.trim().is_empty() {
        return Err("id required".into());
    }
    if r.name.trim().is_empty() {
        return Err("name required".into());
    }
    if r.window_secs == 0 {
        return Err("window_secs must be > 0".into());
    }
    if r.threshold_count == 0 {
        return Err("threshold_count must be > 0".into());
    }
    match r.action.as_str() {
        "downgrade" => {
            let ts = r
                .target_severity
                .as_deref()
                .ok_or_else(|| "downgrade requires target_severity".to_string())?;
            if Severity::parse(ts).is_none() {
                return Err(format!("invalid target_severity: {}", ts));
            }
        }
        "suppress" | "merge" => {}
        other => return Err(format!("invalid action: {}", other)),
    }
    Ok(())
}

/// Phase 54 — Evaluate the rules for one event and return a decision. Invalid rule fields always return Pass (silent fallback).
pub fn evaluate_aggregations(
    source: &str,
    payload: &serde_json::Value,
    now_secs: u64,
) -> AggregationDecision {
    let rules = load_enabled_aggregation_rules();
    if rules.is_empty() {
        return AggregationDecision::Pass;
    }
    // take the first rule whose pattern hits (priority = configuration order, created_at ASC).
    let mut rule_match: Option<AggregationRule> = None;
    for r in &rules {
        if aggregation_kind_matches(&r.kind_pattern, source) {
            rule_match = Some(r.clone());
            break;
        }
    }
    let Some(rule) = rule_match else {
        return AggregationDecision::Pass;
    };

    let arc = aggregation_state();
    let mut state = match arc.lock() {
        Ok(s) => s,
        Err(_) => return AggregationDecision::Pass,
    };
    let bucket = state.entry(source.to_string()).or_default();
    // prune expired events (front of deque is oldest)
    while let Some(&front) = bucket.events.front() {
        if now_secs.saturating_sub(front) > rule.window_secs {
            bucket.events.pop_front();
        } else {
            break;
        }
    }
    bucket.events.push_back(now_secs);

    // already fired within the current window → Pass (avoids ongoing spam)
    if bucket.last_fired != 0 && now_secs.saturating_sub(bucket.last_fired) <= rule.window_secs {
        return AggregationDecision::Pass;
    }
    if (bucket.events.len() as u32) < rule.threshold_count {
        return AggregationDecision::Pass;
    }
    bucket.last_fired = now_secs;

    match rule.action.as_str() {
        "suppress" => AggregationDecision::Suppress {
            propagated_severity: aggregation_action_severity(source),
        },
        "downgrade" => {
            let sev_str = rule.target_severity.as_deref().unwrap_or("info");
            match Severity::parse(sev_str) {
                // Phase 71: (target_severity, propagated_severity) — consistent across chains
                Some(s) => AggregationDecision::Downgrade(s, aggregation_action_severity(source)),
                None => AggregationDecision::Pass,
            }
        }
        "merge" => AggregationDecision::Merge {
            count: bucket.events.len() as u64,
            since: *bucket.events.front().unwrap_or(&now_secs),
            last_payload: payload.clone(),
            propagated_severity: aggregation_action_severity(source), // Phase 71
        },
        _ => AggregationDecision::Pass,
    }
}

/// Phase 54 — Fill in an id for the passed rule (UUID v4 when empty).
pub fn ensure_aggregation_id(rule: &mut AggregationRule) {
    if rule.id.trim().is_empty() {
        rule.id = format!("agg-{}", uuid::Uuid::new_v4());
    }
}

pub fn list_aggregations() -> Vec<AggregationRuleDto> {
    crate::core::shared_store()
        .and_then(|s| {
            let g = s.lock().ok()?;
            Some(
                g.list_alerting_aggregations()
                    .into_iter()
                    .map(dto_from_row)
                    .collect(),
            )
        })
        .unwrap_or_default()
}

pub fn save_aggregation(mut rule: AggregationRule) -> Result<AggregationRuleDto, String> {
    ensure_aggregation_id(&mut rule);
    validate_aggregation(&rule)?;
    let now = crate::core::agent::now_secs();
    let row = AggregationRuleRow {
        id: rule.id.clone(),
        name: rule.name,
        kind_pattern: rule.kind_pattern,
        window_secs: rule.window_secs,
        threshold_count: rule.threshold_count,
        action: rule.action,
        target_severity: rule.target_severity,
        enabled: rule.enabled,
        created_at: now,
    };
    let store = crate::core::shared_store().ok_or_else(|| "store unavailable".to_string())?;
    let mut g = store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    g.upsert_alerting_aggregation(&row);
    Ok(dto_from_row(row))
}

pub fn delete_aggregation(id: &str) -> bool {
    crate::core::shared_store()
        .and_then(|s| {
            let mut g = s.lock().ok()?;
            Some(g.delete_alerting_aggregation(id))
        })
        .unwrap_or(false)
}

pub fn clear_aggregations() -> usize {
    crate::core::shared_store()
        .and_then(|s| {
            let mut g = s.lock().ok()?;
            Some(g.clear_alerting_aggregations())
        })
        .unwrap_or(0)
}

/// Phase 54 — Used by dispatch: modify (envelope_severity, payload) per AggregationDecision, or skip.
/// Returning false means suppress; the call site should return immediately.
pub fn apply_aggregation_decision(
    decision: &AggregationDecision,
    payload: &mut serde_json::Value,
    envelope_severity: &mut Severity,
) -> bool {
    match decision {
        AggregationDecision::Pass => true,
        // Phase 71: (target_severity, propagated_severity) — rewrite the envelope with target; propagated is trace-only
        AggregationDecision::Downgrade(sev, _propagated) => {
            *envelope_severity = *sev;
            true
        }
        // Phase 71: Suppress now carries a propagated_severity field; semantics unchanged (still returns false)
        AggregationDecision::Suppress { .. } => false,
        AggregationDecision::Merge {
            count,
            since,
            last_payload,
            ..
        } => {
            *payload = serde_json::json!({
                "merged_count": count,
                "since": since,
                "last_payload": last_payload,
            });
            true
        }
    }
}

/// Phase 54 — test-only reset: clear buckets + clear the rule table.
pub fn _reset_aggregations_for_tests() {
    if let Some(arc) = crate::core::shared_store() {
        if let Ok(mut g) = arc.lock() {
            let _ = g.clear_alerting_aggregations();
        }
    }
    if let Ok(mut map) = aggregation_state().lock() {
        map.clear();
    }
}
