//! the full-chain dry-run simulator (silence/ack/dedup/route/agg/corr/escalation).
//! Mechanical move from core/alerting.rs.

use super::*;

/// Phase 75 — Route chain decision: which DSL route was hit / fanout to all.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteSimulation {
    pub matched_rule: Option<RouteRule>,
    pub target_endpoint_ids: Vec<String>,
    pub fanout_all: bool,
}

/// Phase 75 — Aggregation chain decision, peeking bucket state (no writes).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregationSimulation {
    pub matched_rule_id: Option<String>,
    pub matched_rule_name: Option<String>,
    pub bucket_events_in_window: u64,
    pub threshold: u32,
    pub would_fire: bool,
    pub action: Option<String>,
    pub action_severity: Option<String>,
}

/// Phase 75 — Correlation chain decision (clones last_a, simulation writes nothing).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrelationSimulation {
    pub matched_a_rule_ids: Vec<String>,
    pub matched_b_rule_ids: Vec<String>,
    pub would_suppress: bool,
    pub suppressing_rule_id: Option<String>,
    pub propagated_severity: Option<String>,
}

/// Phase 75 — Escalation chain candidates (read-only on last_by_source).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationSimulation {
    pub last_dispatch_at: Option<u64>,
    pub candidate_rules: Vec<EscalationRuleDto>,
    pub would_escalate: bool,
    pub target_severity: Option<String>,
}

/// Phase 75 — Alerting full-chain dry-run aggregate DTO.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertingDispatchSimulation {
    pub source: String,
    pub payload_summary: String,
    /// Phase 76 — hit status of the three gates silence / ack / dedup (short-circuit at the start of dispatch).
    pub gates: DispatchGateSimulation,
    pub route: RouteSimulation,
    pub correlation: CorrelationSimulation,
    pub aggregation: AggregationSimulation,
    pub escalation: EscalationSimulation,
    pub endpoints: Vec<EndpointPreviewRow>,
}

/// Phase 76 — details of the hit silence rule (returns Some only when the source is currently silenced).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SilenceHitDto {
    pub id: String,
    pub name: String,
    pub kind_pattern: String,
    pub ends_at: u64,
    pub remaining_secs: u64,
}

/// Phase 76 — details of the hit ack record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckHitDto {
    pub id: String,
    pub kind_pattern: String,
    pub ack_until: u64,
    pub remaining_secs: u64,
}

/// Phase 76 — a dedup hit (same source+payload within the cfg.min_interval_secs window).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupHitDto {
    pub key: String,
    pub last_sent_secs_ago: u64,
    pub min_interval_secs: u32,
    pub remaining_secs: u64,
}

/// Phase 76 — three-gate aggregate. Any non-None → `would_be_dropped = true`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchGateSimulation {
    pub silenced: Option<SilenceHitDto>,
    pub acked: Option<AckHitDto>,
    pub dedup_blocked: Option<DedupHitDto>,
    pub would_be_dropped: bool,
}

impl DispatchGateSimulation {
    fn empty() -> Self {
        DispatchGateSimulation {
            silenced: None,
            acked: None,
            dedup_blocked: None,
            would_be_dropped: false,
        }
    }
}

/// Phase 76 — Silence simulation: replicates the `is_silenced` decision and returns the first hit silence's details.
pub fn simulate_silence(source: &str, now_ts: u64) -> Option<SilenceHitDto> {
    let store = crate::core::shared_store()?;
    let s = store.lock().ok()?;
    let StoreEnum::Db(db) = &*s else {
        return None;
    };
    for r in db.list_active_silences(now_ts) {
        if now_ts < r.starts_at || now_ts >= r.ends_at {
            continue;
        }
        let wd = weekday_from_unix(now_ts);
        let bit = ((wd as u16 + 6) % 7) as u8;
        if r.weekdays & (1u8 << bit) == 0 {
            continue;
        }
        let h = hour_from_unix(now_ts);
        if h < r.start_hour || h >= r.end_hour {
            continue;
        }
        if !kind_matches(&r.kind_pattern, source) {
            continue;
        }
        return Some(SilenceHitDto {
            id: r.id,
            name: r.name,
            kind_pattern: r.kind_pattern,
            ends_at: r.ends_at,
            remaining_secs: r.ends_at.saturating_sub(now_ts),
        });
    }
    None
}

/// Phase 76 — Ack simulation: replicates the `is_acked` decision and returns the first hit ack's details.
pub fn simulate_ack(source: &str, now_ts: u64) -> Option<AckHitDto> {
    let store = crate::core::shared_store()?;
    let s = store.lock().ok()?;
    let StoreEnum::Db(db) = &*s else {
        return None;
    };
    for r in db.list_active_acks(now_ts) {
        if !kind_matches(&r.kind_pattern, source) {
            continue;
        }
        return Some(AckHitDto {
            id: r.id,
            kind_pattern: r.kind_pattern.clone(),
            ack_until: r.ack_until,
            remaining_secs: r.ack_until.saturating_sub(now_ts),
        });
    }
    None
}

/// Phase 76 — Dedup simulation: peek `shared().last_sent` to see whether (source, payload) is within
/// `cfg.min_interval_secs`. min_interval=0 means no rate limit, returning None directly.
pub fn simulate_dedup(
    source: &str,
    payload: &serde_json::Value,
    now_secs: u64,
) -> Option<DedupHitDto> {
    let cfg = load_config();
    let min_interval = cfg.min_interval_secs as u64;
    if min_interval == 0 {
        return None;
    }
    let key = dedup_key(source, payload);
    let arc = shared();
    let s = arc.lock().ok()?;
    let prev = s.last_sent.get(&key).copied()?;
    let elapsed = prev.elapsed().as_secs();
    if elapsed >= min_interval {
        return None;
    }
    Some(DedupHitDto {
        key,
        last_sent_secs_ago: elapsed,
        min_interval_secs: cfg.min_interval_secs,
        remaining_secs: min_interval.saturating_sub(elapsed),
    })
}

/// Phase 75 — Route simulation (pure read, no side effects). No rule hit → fanout_all=true, targets empty.
pub fn simulate_route(source: &str, payload: &serde_json::Value) -> RouteSimulation {
    let matched = match_route(source, payload);
    let (target_endpoint_ids, fanout_all) = match &matched {
        Some(r) => (r.target_endpoint_ids.clone(), false),
        None => (Vec::new(), true),
    };
    RouteSimulation {
        matched_rule: matched.as_ref().map(route_row_to_dto),
        target_endpoint_ids,
        fanout_all,
    }
}

/// Phase 75 — Aggregation simulation: peek the bucket and compute would_fire, but do not push an event or change last_fired.
pub fn simulate_aggregations(
    source: &str,
    _payload: &serde_json::Value,
    now_secs: u64,
) -> AggregationSimulation {
    let rules = load_enabled_aggregation_rules();
    if rules.is_empty() {
        return AggregationSimulation {
            matched_rule_id: None,
            matched_rule_name: None,
            bucket_events_in_window: 0,
            threshold: 0,
            would_fire: false,
            action: None,
            action_severity: None,
        };
    }
    let rule = match rules
        .iter()
        .find(|r| aggregation_kind_matches(&r.kind_pattern, source))
    {
        Some(r) => r,
        None => {
            return AggregationSimulation {
                matched_rule_id: None,
                matched_rule_name: None,
                bucket_events_in_window: 0,
                threshold: 0,
                would_fire: false,
                action: None,
                action_severity: None,
            };
        }
    };

    // peek bucket without mutation: clone events + last_fired
    let (bucket_events_in_window, last_fired) = match aggregation_state().lock() {
        Ok(state) => {
            let bucket = state.get(source);
            let mut events = match bucket {
                Some(b) => b.events.clone(),
                None => std::collections::VecDeque::new(),
            };
            let last_fired = bucket.map(|b| b.last_fired).unwrap_or(0);
            // prune expired events (consistent with evaluate_aggregations)
            while let Some(&front) = events.front() {
                if now_secs.saturating_sub(front) > rule.window_secs {
                    events.pop_front();
                } else {
                    break;
                }
            }
            (events.len() as u64, last_fired)
        }
        Err(_) => (0, 0),
    };

    let simulated_count = bucket_events_in_window.saturating_add(1);
    let threshold_met = simulated_count >= rule.threshold_count as u64;
    let already_fired_in_window =
        last_fired != 0 && now_secs.saturating_sub(last_fired) <= rule.window_secs;
    let would_fire = threshold_met && !already_fired_in_window;

    let (action, action_severity) = if would_fire {
        (Some(rule.action.clone()), Some(action_severity_str(source)))
    } else {
        (None, None)
    };

    AggregationSimulation {
        matched_rule_id: Some(rule.id.clone()),
        matched_rule_name: Some(rule.name.clone()),
        bucket_events_in_window,
        threshold: rule.threshold_count,
        would_fire,
        action,
        action_severity,
    }
}

/// Phase 75 — Correlation simulation: clone last_a, apply the simulated update to a local copy, and do not write back to state.
pub fn simulate_correlations(source: &str, now_secs: u64) -> CorrelationSimulation {
    let rules = load_enabled_correlation_rules();
    if rules.is_empty() {
        return CorrelationSimulation {
            matched_a_rule_ids: Vec::new(),
            matched_b_rule_ids: Vec::new(),
            would_suppress: false,
            suppressing_rule_id: None,
            propagated_severity: None,
        };
    }
    let mut last_a: std::collections::HashMap<String, u64> = match correlation_state().lock() {
        Ok(s) => s.last_a.clone(),
        Err(_) => std::collections::HashMap::new(),
    };
    let mut matched_a: Vec<String> = Vec::new();
    let mut matched_b: Vec<String> = Vec::new();
    let mut a_rule_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in &rules {
        if aggregation_kind_matches(&r.kind_pattern_a, source) {
            last_a.insert(r.id.clone(), now_secs);
            a_rule_ids.insert(r.id.clone());
            matched_a.push(r.id.clone());
        }
    }
    let mut suppress: Option<(String, Severity)> = None;
    for r in &rules {
        if !aggregation_kind_matches(&r.kind_pattern_b, source) {
            continue;
        }
        matched_b.push(r.id.clone());
        // self-reference (A==B and this event hits A too) → no-op, avoiding self-suppress
        if a_rule_ids.contains(&r.id) {
            continue;
        }
        if let Some(&la) = last_a.get(&r.id) {
            if now_secs.saturating_sub(la) <= r.window_secs {
                suppress = Some((r.id.clone(), correlation_decision_severity(source)));
                break;
            }
        }
    }
    let (would_suppress, suppressing_rule_id, propagated_severity) = match suppress {
        Some((id, sev)) => (true, Some(id), Some(sev.as_str().to_string())),
        None => (false, None, None),
    };
    CorrelationSimulation {
        matched_a_rule_ids: matched_a,
        matched_b_rule_ids: matched_b,
        would_suppress,
        suppressing_rule_id,
        propagated_severity,
    }
}

/// Phase 75 — Escalation simulation: peek last_by_source and identify which rules already meet the escalation condition.
pub fn simulate_escalations(source: &str, now_secs: u64) -> EscalationSimulation {
    let last_at = match escalation_state().lock() {
        Ok(s) => s.last_by_source.get(source).copied(),
        Err(_) => None,
    };
    let last = last_at.unwrap_or(0);
    let rules = load_enabled_escalation_rules();
    let mut candidates: Vec<EscalationRuleDto> = Vec::new();
    let mut target_sev: Option<String> = None;
    for r in &rules {
        if !aggregation_kind_matches(&r.kind_pattern, source) {
            continue;
        }
        if last != 0 && now_secs.saturating_sub(last) >= r.escalate_after_secs {
            candidates.push(EscalationRuleDto {
                id: r.id.clone(),
                name: r.name.clone(),
                kind_pattern: r.kind_pattern.clone(),
                escalate_after_secs: r.escalate_after_secs,
                target_severity: r.target_severity.clone(),
                target_endpoint_ids: r.target_endpoint_ids.clone(),
                enabled: r.enabled,
                created_at: 0,
            });
            if target_sev.is_none() {
                target_sev = Some(r.target_severity.clone());
            }
        }
    }
    EscalationSimulation {
        last_dispatch_at: last_at,
        would_escalate: !candidates.is_empty(),
        candidate_rules: candidates,
        target_severity: target_sev,
    }
}

/// Phase 75 — stringified `aggregation_action_severity(source)` (consistent with Phase 71 behavior).
fn action_severity_str(source: &str) -> String {
    aggregation_action_severity(source).as_str().to_string()
}

/// Phase 75 — top-level orchestrator: strings together the 4 simulate_* + endpoint fanout.
/// Writes no state, sends no HTTP, triggers no dedup / silence / ack.
pub fn simulate_alerting_dispatch(
    source: &str,
    payload: Option<&str>,
) -> Result<AlertingDispatchSimulation, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("source is required".into());
    }
    if source.len() > 256 {
        return Err("source too long (max 256)".into());
    }
    let payload_v: serde_json::Value = match payload {
        None | Some("") => serde_json::json!({}),
        Some(s) => serde_json::from_str(s).map_err(|e| format!("invalid payload json: {}", e))?,
    };

    let now_secs = crate::core::agent::now_secs();

    // Phase 74 lesson: take the endpoints snapshot inside the block first, and let the lock drop at the end of the block
    // (the later preview_endpoint_severity / simulate_* calls do not need to hold the store lock)
    let endpoint_rows: Vec<WebhookEndpoint> = {
        let store =
            crate::core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
        let s = store.lock().map_err(|_| "store poisoned".to_string())?;
        let StoreEnum::Db(db) = &*s else {
            return Err("sqlite store required".into());
        };
        db.list_alerting_endpoints()
            .iter()
            .filter(|r| r.enabled)
            .map(endpoint_row_to_dto)
            .collect()
        // the lock drops at the end of the block
    };

    let route = simulate_route(source, &payload_v);
    let correlation = simulate_correlations(source, now_secs);
    let aggregation = simulate_aggregations(source, &payload_v, now_secs);
    let escalation = simulate_escalations(source, now_secs);
    let endpoints = endpoint_rows
        .iter()
        .map(|ep| preview_endpoint_severity(source, ep))
        .collect();

    // Phase 76 — evaluate the three gates first (silence / ack / dedup short-circuit at the start of dispatch)
    // each simulate_* holds the store / shared lock briefly on its own and they do not nest, so they can be called in sequence outside the block
    let silenced = simulate_silence(source, now_secs);
    let acked = simulate_ack(source, now_secs);
    let dedup_blocked = simulate_dedup(source, &payload_v, now_secs);
    let gates = DispatchGateSimulation {
        would_be_dropped: silenced.is_some() || acked.is_some() || dedup_blocked.is_some(),
        silenced,
        acked,
        dedup_blocked,
    };

    let payload_summary = serde_json::to_string(&payload_v)
        .unwrap_or_default()
        .chars()
        .take(64)
        .collect::<String>();

    Ok(AlertingDispatchSimulation {
        source: source.to_string(),
        payload_summary,
        gates,
        route,
        correlation,
        aggregation,
        escalation,
        endpoints,
    })
}
