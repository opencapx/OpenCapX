//! escalation chains and redispatch.
//! Mechanical move from core/alerting.rs.

use super::*;

const POLL_ESCALATION_SECS: u64 = 15;

/// Phase 56 — Public DTO for escalation rules (settings UI + tauri commands).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationRuleDto {
    pub id: String,
    pub name: String,
    #[serde(rename = "kindPattern")]
    pub kind_pattern: String,
    #[serde(rename = "escalateAfterSecs")]
    pub escalate_after_secs: u64,
    #[serde(rename = "targetSeverity")]
    pub target_severity: String,
    #[serde(rename = "targetEndpointIds", skip_serializing_if = "Option::is_none")]
    pub target_endpoint_ids: Option<Vec<String>>,
    pub enabled: bool,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// Phase 56 — settings UI form arguments.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationRule {
    pub id: String,
    pub name: String,
    #[serde(rename = "kindPattern")]
    pub kind_pattern: String,
    #[serde(rename = "escalateAfterSecs")]
    pub escalate_after_secs: u64,
    pub target_severity: String,
    #[serde(rename = "targetEndpointIds", skip_serializing_if = "Option::is_none")]
    pub target_endpoint_ids: Option<Vec<String>>,
    pub enabled: bool,
}

/// Phase 56 — escalation decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationDecision {
    Pass,
    Escalate {
        severity: Severity,
        endpoint_ids: Vec<String>,
    },
}

/// In-memory state: the most recent successful dispatch timestamp for each source.
#[derive(Debug, Default)]
pub(crate) struct EscalationState {
    pub(crate) last_by_source: std::collections::HashMap<String, u64>,
}

static ESCALATION_STATE: std::sync::OnceLock<std::sync::Mutex<EscalationState>> =
    std::sync::OnceLock::new();

pub(crate) fn escalation_state() -> &'static std::sync::Mutex<EscalationState> {
    ESCALATION_STATE.get_or_init(|| std::sync::Mutex::new(EscalationState::default()))
}

/// Phase 56 — Called after a successful dispatch delivery; records source → now_secs (for the escalation poller).
pub fn record_dispatch(source: &str, now_secs: u64) {
    if let Ok(mut s) = escalation_state().lock() {
        s.last_by_source.insert(source.to_string(), now_secs);
    }
}

fn escalation_rule_from_row(r: EscalationRuleRow) -> EscalationRule {
    EscalationRule {
        id: r.id,
        name: r.name,
        kind_pattern: r.kind_pattern,
        escalate_after_secs: r.escalate_after_secs,
        target_severity: r.target_severity,
        target_endpoint_ids: r.target_endpoint_ids,
        enabled: r.enabled,
    }
}

fn escalation_dto_from_row(r: EscalationRuleRow) -> EscalationRuleDto {
    EscalationRuleDto {
        id: r.id,
        name: r.name,
        kind_pattern: r.kind_pattern,
        escalate_after_secs: r.escalate_after_secs,
        target_severity: r.target_severity,
        target_endpoint_ids: r.target_endpoint_ids,
        enabled: r.enabled,
        created_at: r.created_at,
    }
}

pub(crate) fn load_enabled_escalation_rules() -> Vec<EscalationRule> {
    crate::core::shared_store()
        .and_then(|s| {
            let g = s.lock().ok()?;
            let rows = g.list_enabled_alerting_escalations();
            Some(rows.into_iter().map(escalation_rule_from_row).collect())
        })
        .unwrap_or_default()
}

pub fn ensure_escalation_id(rule: &mut EscalationRule) {
    if rule.id.trim().is_empty() {
        rule.id = format!("esc-{}", uuid::Uuid::new_v4());
    }
}

pub fn validate_escalation(r: &EscalationRule) -> Result<(), String> {
    if r.id.trim().is_empty() {
        return Err("id required".into());
    }
    if r.name.trim().is_empty() {
        return Err("name required".into());
    }
    if r.kind_pattern.is_empty() {
        return Err("kind_pattern required".into());
    }
    if r.escalate_after_secs == 0 {
        return Err("escalate_after_secs must be > 0".into());
    }
    if Severity::parse(&r.target_severity).is_none() {
        return Err(format!("invalid target_severity: {}", r.target_severity));
    }
    Ok(())
}

/// Phase 56 — Find all sources that "should escalate but have not escalated yet". Used by the poller.
pub fn find_escalation_candidates(now_secs: u64) -> Vec<(EscalationRule, String, u64)> {
    let rules = load_enabled_escalation_rules();
    if rules.is_empty() {
        return Vec::new();
    }
    let state_lock = match escalation_state().lock() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for (source, &last_ts) in state_lock.last_by_source.iter() {
        // skip when acked — an already-acked source does not need escalation
        if is_acked(source, now_secs) {
            continue;
        }
        for rule in &rules {
            if !aggregation_kind_matches(&rule.kind_pattern, source) {
                continue;
            }
            if now_secs.saturating_sub(last_ts) > rule.escalate_after_secs {
                out.push((rule.clone(), source.clone(), last_ts));
            }
        }
    }
    out
}

pub fn list_escalations() -> Vec<EscalationRuleDto> {
    crate::core::shared_store()
        .and_then(|s| {
            let g = s.lock().ok()?;
            Some(
                g.list_alerting_escalations()
                    .into_iter()
                    .map(escalation_dto_from_row)
                    .collect(),
            )
        })
        .unwrap_or_default()
}

pub fn save_escalation(mut rule: EscalationRule) -> Result<EscalationRuleDto, String> {
    ensure_escalation_id(&mut rule);
    validate_escalation(&rule)?;
    let now = crate::core::agent::now_secs();
    let row = EscalationRuleRow {
        id: rule.id.clone(),
        name: rule.name,
        kind_pattern: rule.kind_pattern,
        escalate_after_secs: rule.escalate_after_secs,
        target_severity: rule.target_severity,
        target_endpoint_ids: rule.target_endpoint_ids,
        enabled: rule.enabled,
        created_at: now,
    };
    let store = crate::core::shared_store().ok_or_else(|| "store unavailable".to_string())?;
    let mut g = store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    g.upsert_alerting_escalation(&row);
    Ok(escalation_dto_from_row(row))
}

pub fn delete_escalation(id: &str) -> bool {
    crate::core::shared_store()
        .and_then(|s| {
            let mut g = s.lock().ok()?;
            Some(g.delete_alerting_escalation(id))
        })
        .unwrap_or(false)
}

pub fn clear_escalations() -> usize {
    crate::core::shared_store()
        .and_then(|s| {
            let mut g = s.lock().ok()?;
            Some(g.clear_alerting_escalations())
        })
        .unwrap_or(0)
}

/// Phase 56 — test-only reset: clear last_by_source + clear the rule table.
pub fn _reset_escalations_for_tests() {
    if let Some(arc) = crate::core::shared_store() {
        if let Ok(mut g) = arc.lock() {
            let _ = g.clear_alerting_escalations();
        }
    }
    if let Ok(mut map) = escalation_state().lock() {
        map.last_by_source.clear();
    }
}

/// Phase 56 — background poller: scan for candidates every POLL_ESCALATION_SECS and fire a re-dispatch.
/// Note: it does not go through dispatch() (to avoid recursion into dedup / silence / ack / aggregation / correlation),
/// and instead sends once directly using make_envelope_with_severity + the existing endpoints.
pub fn spawn_escalation_loop() {
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(POLL_ESCALATION_SECS));
        let now = crate::core::agent::now_secs();
        let candidates = find_escalation_candidates(now);
        if candidates.is_empty() {
            continue;
        }
        for (rule, source, _last_ts) in candidates {
            let sev = match Severity::parse(&rule.target_severity) {
                Some(s) => s,
                None => continue,
            };
            // mark "just escalated" as last_ts = now — a cooldown preventing an immediate repeat fire
            if let Ok(mut s) = escalation_state().lock() {
                s.last_by_source.insert(source.clone(), now);
            }
            let ep_ids = rule.target_endpoint_ids.clone().unwrap_or_default();
            escalation_redispatch(&source, &ep_ids, sev);
        }
    });
}

/// Phase 56 — actual escalation delivery: build the envelope directly + send to the given endpoints (empty means fanout all enabled).
fn escalation_redispatch(source: &str, endpoint_ids: &[String], severity: Severity) {
    let payload = serde_json::json!({
        "escalation": true,
        "source": source,
        "escalated_at": crate::core::agent::now_secs(),
    });
    let env = make_envelope_with_severity(source, payload, vec![], severity);
    let endpoints = load_enabled_endpoints();
    let targets: Vec<AlertingEndpointRow> = if endpoint_ids.is_empty() {
        endpoints
    } else {
        endpoints
            .into_iter()
            .filter(|ep| endpoint_ids.iter().any(|id| id == &ep.id))
            .collect()
    };
    for ep in targets {
        // Phase 57: per-endpoint template takes precedence; otherwise envelope JSON.
        let (body, content_type) = if let Some(tpl) = ep.template.as_deref() {
            match render_template(tpl, &env) {
                Ok((s, ct)) => (s, ct.as_content_type().to_string()),
                Err(e) => {
                    eprintln!(
                        "[alerting] escalation template render error on {}: {}",
                        ep.name, e
                    );
                    (
                        envelope_to_json_string(&env).unwrap_or_else(|_| "null".into()),
                        "application/json".to_string(),
                    )
                }
            }
        } else {
            (
                envelope_to_json_string(&env).unwrap_or_else(|_| "null".into()),
                "application/json".to_string(),
            )
        };
        let url = ep.url.clone();
        let content_type_for_thread = content_type;
        std::thread::spawn(move || {
            let headers = build_request_headers(&ep, &body);
            let _ = send_http_with_body(&url, &headers, &body, &content_type_for_thread);
        });
    }
}
