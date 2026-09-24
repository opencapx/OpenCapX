//! correlation suppression rules.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Phase 55 — Public DTO for correlation rules (settings UI + tauri commands).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrelationRuleDto {
    pub id: String,
    pub name: String,
    #[serde(rename = "kindPatternA")]
    pub kind_pattern_a: String,
    #[serde(rename = "kindPatternB")]
    pub kind_pattern_b: String,
    #[serde(rename = "windowSecs")]
    pub window_secs: u64,
    pub enabled: bool,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// Phase 55 — settings UI form arguments.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrelationRule {
    pub id: String,
    pub name: String,
    #[serde(rename = "kindPatternA")]
    pub kind_pattern_a: String,
    #[serde(rename = "kindPatternB")]
    pub kind_pattern_b: String,
    #[serde(rename = "windowSecs")]
    pub window_secs: u64,
    pub enabled: bool,
}

/// Phase 55 — decision result of the correlation engine. Phase 71 — Suppress adds `propagated_severity` so upstream callers
/// (the dispatch main path) see the source's effective severity, provided by `correlation_decision_severity(source)`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrelationDecision {
    Pass,
    Suppress { propagated_severity: Severity },
}

/// In-memory state: the most recent A hit timestamp for each rule.
#[derive(Debug, Default)]
pub(crate) struct CorrelationState {
    pub(crate) last_a: std::collections::HashMap<String, u64>,
}

static CORRELATION_STATE: std::sync::OnceLock<std::sync::Mutex<CorrelationState>> =
    std::sync::OnceLock::new();

pub(crate) fn correlation_state() -> &'static std::sync::Mutex<CorrelationState> {
    CORRELATION_STATE.get_or_init(|| std::sync::Mutex::new(CorrelationState::default()))
}

fn correlation_rule_from_row(r: CorrelationRuleRow) -> CorrelationRule {
    CorrelationRule {
        id: r.id,
        name: r.name,
        kind_pattern_a: r.kind_pattern_a,
        kind_pattern_b: r.kind_pattern_b,
        window_secs: r.window_secs,
        enabled: r.enabled,
    }
}

fn correlation_dto_from_row(r: CorrelationRuleRow) -> CorrelationRuleDto {
    CorrelationRuleDto {
        id: r.id,
        name: r.name,
        kind_pattern_a: r.kind_pattern_a,
        kind_pattern_b: r.kind_pattern_b,
        window_secs: r.window_secs,
        enabled: r.enabled,
        created_at: r.created_at,
    }
}

pub(crate) fn load_enabled_correlation_rules() -> Vec<CorrelationRule> {
    crate::core::shared_store()
        .and_then(|s| {
            let g = s.lock().ok()?;
            let rows = g.list_enabled_alerting_correlations();
            Some(rows.into_iter().map(correlation_rule_from_row).collect())
        })
        .unwrap_or_default()
}

pub fn ensure_correlation_id(rule: &mut CorrelationRule) {
    if rule.id.trim().is_empty() {
        rule.id = format!("cor-{}", uuid::Uuid::new_v4());
    }
}

pub fn validate_correlation(r: &CorrelationRule) -> Result<(), String> {
    if r.id.trim().is_empty() {
        return Err("id required".into());
    }
    if r.name.trim().is_empty() {
        return Err("name required".into());
    }
    if r.window_secs == 0 {
        return Err("window_secs must be > 0".into());
    }
    if r.kind_pattern_a.trim().is_empty() {
        return Err("kind_pattern_a required".into());
    }
    if r.kind_pattern_b.trim().is_empty() {
        return Err("kind_pattern_b required".into());
    }
    Ok(())
}

/// Phase 55 — evaluate one event, update last_a + check whether B should be suppressed.
/// Self-referential rules (A pattern == B pattern) are a silent no-op (otherwise they would self-suppress).
pub fn evaluate_correlations(source: &str, now_secs: u64) -> CorrelationDecision {
    let rules = load_enabled_correlation_rules();
    if rules.is_empty() {
        return CorrelationDecision::Pass;
    }
    let arc = correlation_state();
    let mut state = match arc.lock() {
        Ok(s) => s,
        Err(_) => return CorrelationDecision::Pass,
    };

    // Phase 1: rules whose A pattern matches the event → update last_a
    let mut a_rule_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in &rules {
        if aggregation_kind_matches(&r.kind_pattern_a, source) {
            state.last_a.insert(r.id.clone(), now_secs);
            a_rule_ids.insert(r.id.clone());
        }
    }

    // Phase 2: rules whose B pattern matches the event → check whether last_a is within the window
    for r in &rules {
        if !aggregation_kind_matches(&r.kind_pattern_b, source) {
            continue;
        }
        // self-reference (A==B and this event hits A too) → no-op, avoiding self-suppress
        if a_rule_ids.contains(&r.id) {
            continue;
        }
        if let Some(&last_a) = state.last_a.get(&r.id) {
            if now_secs.saturating_sub(last_a) <= r.window_secs {
                // Phase 71: carry propagated_severity so the dispatch layer sees the effective severity
                return CorrelationDecision::Suppress {
                    propagated_severity: correlation_decision_severity(source),
                };
            }
        }
    }
    CorrelationDecision::Pass
}

pub fn list_correlations() -> Vec<CorrelationRuleDto> {
    crate::core::shared_store()
        .and_then(|s| {
            let g = s.lock().ok()?;
            Some(
                g.list_alerting_correlations()
                    .into_iter()
                    .map(correlation_dto_from_row)
                    .collect(),
            )
        })
        .unwrap_or_default()
}

pub fn save_correlation(mut rule: CorrelationRule) -> Result<CorrelationRuleDto, String> {
    ensure_correlation_id(&mut rule);
    validate_correlation(&rule)?;
    let now = crate::core::agent::now_secs();
    let row = CorrelationRuleRow {
        id: rule.id.clone(),
        name: rule.name,
        kind_pattern_a: rule.kind_pattern_a,
        kind_pattern_b: rule.kind_pattern_b,
        window_secs: rule.window_secs,
        enabled: rule.enabled,
        created_at: now,
    };
    let store = crate::core::shared_store().ok_or_else(|| "store unavailable".to_string())?;
    let mut g = store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    g.upsert_alerting_correlation(&row);
    Ok(correlation_dto_from_row(row))
}

pub fn delete_correlation(id: &str) -> bool {
    crate::core::shared_store()
        .and_then(|s| {
            let mut g = s.lock().ok()?;
            Some(g.delete_alerting_correlation(id))
        })
        .unwrap_or(false)
}

pub fn clear_correlations() -> usize {
    crate::core::shared_store()
        .and_then(|s| {
            let mut g = s.lock().ok()?;
            Some(g.clear_alerting_correlations())
        })
        .unwrap_or(0)
}

/// Phase 55 — test-only reset: clear last_a memory + clear the rule table.
pub fn _reset_correlations_for_tests() {
    if let Some(arc) = crate::core::shared_store() {
        if let Ok(mut g) = arc.lock() {
            let _ = g.clear_alerting_correlations();
        }
    }
    if let Ok(mut map) = correlation_state().lock() {
        map.last_a.clear();
    }
}
