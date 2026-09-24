//! the dispatch pipeline hub, dedup state, HTTP transport, canonical envelopes, EventBus subscription.
//! Mechanical move from core/alerting.rs.

use super::*;

#[derive(Default)]
pub(crate) struct DispatcherInner {
    /// key = dedup_key(source, payload) → last sent time
    pub(crate) last_sent: HashMap<String, Instant>,
}

static SHARED: OnceLock<Arc<Mutex<DispatcherInner>>> = OnceLock::new();

pub(crate) fn shared() -> Arc<Mutex<DispatcherInner>> {
    SHARED
        .get_or_init(|| Arc::new(Mutex::new(DispatcherInner::default())))
        .clone()
}

/// Compute a stable dedup key for the payload (same source + payload sent only once within a short window).
pub(crate) fn dedup_key(source: &str, payload: &serde_json::Value) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    // to_string() is deterministic JSON → same payload, same hash
    payload.to_string().hash(&mut h);
    format!("{}:{:016x}", source, h.finish())
}

pub(crate) fn source_enabled(source: &str, sources: &WebhookSources) -> bool {
    match source {
        "plugin.metrics.exceeded" => sources.metrics_exceeded,
        "capability.sla.violated" => sources.sla_violated,
        "plugin.kill_switch.enabled" => sources.kill_switch_engaged,
        "plugin.lifecycle.crashed" => sources.plugin_crashed,
        _ => false,
    }
}

/// Phase 49 — whether an endpoint accepts a source. Empty filter = accept all.
pub(crate) fn endpoint_accepts_source(ep: &AlertingEndpointRow, source: &str) -> bool {
    ep.source_filter.is_empty() || ep.source_filter.iter().any(|s| s == source)
}

/// Phase 49 — Fetch all enabled endpoints (an empty store returns empty, fanout skipped).
pub(crate) fn load_enabled_endpoints() -> Vec<AlertingEndpointRow> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let Ok(s) = store.lock() else {
        return Vec::new();
    };
    if let StoreEnum::Db(db) = &*s {
        db.list_enabled_alerting_endpoints()
    } else {
        Vec::new()
    }
}

/// Phase 49 — Compute the HMAC-SHA256 signature used as the `X-OpenCapX-Signature` header value.
/// Format `sha256=<hex>` (aligned with GitHub webhook style). An empty secret returns an empty string → the caller skips adding the header.
pub fn compute_signature(secret: &str, body: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    let hex = hmac_sha256_hex(secret.as_bytes(), body.as_bytes());
    format!("sha256={}", hex)
}

/// dispatcher entry point.
/// - Phase 47 v1: when no endpoint is registered, use the global `WebhookConfig.url` single-endpoint path.
/// - Phase 49 v2: when endpoints exist, fan out to every enabled endpoint whose source_filter matches.
pub fn dispatch(source: &str, payload: serde_json::Value) {
    let cfg = load_config();
    // Phase 50: silence + ack short-circuit earliest, before dedup / endpoint routing
    let now_secs = crate::core::agent::now_secs();
    if is_silenced(source, now_secs) {
        return;
    }
    if is_acked(source, now_secs) {
        return;
    }
    let endpoints = load_enabled_endpoints();
    if endpoints.is_empty() {
        // v1 compatibility path
        if !cfg.enabled {
            return;
        }
        if !source_enabled(source, &cfg.sources) {
            return;
        }
        if cfg.url.is_empty() {
            return;
        }
    }
    let key = dedup_key(source, &payload);
    let now = Instant::now();
    {
        let arc = shared();
        let mut s = match arc.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if let Some(prev) = s.last_sent.get(&key) {
            if now.duration_since(*prev).as_secs() < cfg.min_interval_secs as u64 {
                return;
            }
        }
        s.last_sent.insert(key, now);
    }

    // Phase 55: correlation suppression — evaluated before aggregation. Suppress is the outermost short-circuit.
    if matches!(
        evaluate_correlations(source, now_secs),
        CorrelationDecision::Suppress { .. }
    ) {
        return;
    }

    // Phase 54: frequency threshold aggregation — evaluated after dedup, before building the envelope.
    //   - Suppress → the whole event is dropped (return)
    //   - Downgrade / Merge → modify severity / payload; the subsequent envelope picks it up automatically
    let mut envelope_severity = severity_resolved(source);
    let agg_decision = evaluate_aggregations(source, &payload, now_secs);
    let mut effective_payload = payload.clone();
    if !apply_aggregation_decision(
        &agg_decision,
        &mut effective_payload,
        &mut envelope_severity,
    ) {
        return;
    }

    if endpoints.is_empty() {
        // single-endpoint compatibility path (Phase 47 behavior)
        let url = cfg.url.clone();
        let headers = cfg.custom_headers.clone();
        let schema_version = cfg.schema_version;
        let source_owned = source.to_string();
        // Phase 54: aggregation may have changed payload + severity; capture the override version here
        let effective_payload_for_thread = effective_payload.clone();
        let envelope_severity_for_thread = envelope_severity;
        // Phase 56: escalation chain — record the source + ts of a successful dispatch for the escalation poller
        record_dispatch(source, now_secs);
        std::thread::spawn(move || {
            // Phase 52: wrap the canonical envelope when schema_version=1
            // Phase 54: frequency threshold overrode severity and payload
            let (body_str, result) = if schema_version >= 1 {
                let env = make_envelope_with_severity(
                    &source_owned,
                    effective_payload_for_thread,
                    vec![],
                    envelope_severity_for_thread,
                );
                let bs = envelope_to_json_string(&env).unwrap_or_else(|_| "null".into());
                let r = send_http_with_body(&url, &headers, &bs, "application/json");
                (bs, r)
            } else {
                let result =
                    send_http(&url, &headers, &effective_payload_for_thread, &source_owned);
                let bs = serde_json::to_string(&effective_payload_for_thread)
                    .unwrap_or_else(|_| "null".into());
                (bs, result)
            };
            if let Err(err_msg) = result {
                let retry_cfg = load_retry_config();
                let now = crate::core::agent::now_secs();
                let next_retry = now.saturating_add(retry_cfg.initial_backoff_secs as u64);
                enqueue_failed_delivery(
                    &source_owned,
                    &url,
                    &body_str,
                    now,
                    retry_cfg.max_attempts,
                    next_retry,
                    &err_msg,
                    None,
                );
                eprintln!("[alerting] {} queued for retry: {}", source_owned, err_msg);
            }
        });
        return;
    }

    // Phase 51: DSL routing has the highest priority — on a hit, send only to that route's target endpoint and skip fanout.
    // Phase 54: route matching uses the post-aggregation effective_payload (after payload merge).
    let matched_route = match_route(source, &effective_payload);
    let route_targets = matched_route
        .as_ref()
        .map(|r| r.target_endpoint_ids.clone());
    // Phase 49 fanout path
    let matched: Vec<AlertingEndpointRow> = match route_targets {
        Some(target_ids) => endpoints
            .into_iter()
            .filter(|ep| target_ids.iter().any(|t| t == &ep.id))
            .collect(),
        None => endpoints
            .into_iter()
            .filter(|ep| endpoint_accepts_source(ep, source))
            .collect(),
    };
    if matched.is_empty() {
        return;
    }
    // Phase 56: escalation chain — record one fanout dispatch
    record_dispatch(source, now_secs);
    // Phase 68: record the seen event into the ring (used by seen_in_last + timeline).
    let mut fired_names: Vec<String> = Vec::new();
    if let Some(r) = &matched_route {
        fired_names.push(r.name.clone());
    }
    let payload_summary = serde_json::to_string(&effective_payload)
        .unwrap_or_default()
        .chars()
        .take(64)
        .collect::<String>();
    record_seen_event(source, &payload_summary, now_secs, fired_names, Vec::new());
    for ep in matched {
        // Phase 72 — per-endpoint severity override takes precedence over propagation.
        // On the main thread, before spawning, parse the overrides JSON + overwrite the envelope_severity copy.
        let mut envelope_severity_clone = envelope_severity;
        if let Some(s) = ep.severity_overrides.as_deref() {
            if let Ok(pairs) = serde_json::from_str::<Vec<(String, String)>>(s) {
                let overrides: Vec<(String, Severity)> = pairs
                    .into_iter()
                    .filter_map(|(src, sev)| Severity::parse(&sev).map(|p| (src, p)))
                    .collect();
                apply_endpoint_severity_override(&mut envelope_severity_clone, source, &overrides);
            }
        }
        let payload_clone = effective_payload.clone();
        let source_owned = source.to_string();
        std::thread::spawn(move || {
            // Phase 57: per-endpoint template takes precedence; otherwise follow schema_version for envelope / legacy.
            let (body_str, content_type) = if let Some(tpl) = ep.template.as_deref() {
                let env = make_envelope_with_severity(
                    &source_owned,
                    payload_clone,
                    vec![],
                    envelope_severity_clone,
                );
                match render_template(tpl, &env) {
                    Ok((s, ct)) => (s, ct.as_content_type().to_string()),
                    Err(e) => {
                        eprintln!("[alerting] template render error on {}: {}", ep.name, e);
                        (
                            envelope_to_json_string(&env).unwrap_or_else(|_| "null".into()),
                            "application/json".to_string(),
                        )
                    }
                }
            } else if ep.schema_version >= 1 {
                let env = make_envelope_with_severity(
                    &source_owned,
                    payload_clone,
                    vec![],
                    envelope_severity_clone,
                );
                (
                    envelope_to_json_string(&env).unwrap_or_else(|_| "null".into()),
                    "application/json".to_string(),
                )
            } else {
                let body = serde_json::json!({
                    "source": source_owned,
                    "timestamp": crate::core::agent::now_secs(),
                    "data": payload_clone,
                });
                (
                    serde_json::to_string(&body).unwrap_or_else(|_| "null".into()),
                    "application/json".to_string(),
                )
            };
            let headers = build_request_headers(&ep, &body_str);
            let result = send_http_with_body(&ep.url, &headers, &body_str, &content_type);
            if let Err(err_msg) = result {
                let retry_cfg = load_retry_config();
                let now = crate::core::agent::now_secs();
                let next_retry = now.saturating_add(retry_cfg.initial_backoff_secs as u64);
                enqueue_failed_delivery(
                    &source_owned,
                    &ep.url,
                    &body_str,
                    now,
                    retry_cfg.max_attempts,
                    next_retry,
                    &err_msg,
                    Some(&ep.id),
                );
                eprintln!(
                    "[alerting] {} → endpoint {} queued for retry: {}",
                    source_owned, ep.name, err_msg
                );
            }
        });
    }
}

/// Phase 49 — Build the header list for a single request (custom headers + optional signature).
pub(crate) fn build_request_headers(
    ep: &AlertingEndpointRow,
    body_str: &str,
) -> Vec<(String, String)> {
    let mut h = ep.headers.clone();
    let sig = compute_signature(&ep.secret, body_str);
    if !sig.is_empty() {
        h.push(("X-OpenCapX-Signature".to_string(), sig));
    }
    h
}

/// POST once, returning the status code or Err(reason). `Ok(2xx)` is treated as success.
fn send_http(
    url: &str,
    headers: &[(String, String)],
    payload: &serde_json::Value,
    source: &str,
) -> Result<u16, String> {
    let body = serde_json::json!({
        "source": source,
        "timestamp": crate::core::agent::now_secs(),
        "data": payload,
    });
    let body_str = serde_json::to_string(&body).map_err(|e| format!("serialize: {}", e))?;
    send_http_with_body(url, headers, &body_str, "application/json")
}

/// Phase 49 — Pass the body string directly (signature computation needs exact bytes, avoiding `.json()` re-serialization differences).
/// Phase 57: `content_type` is passed explicitly (default `application/json`; template rendering may switch to text/plain).
pub(crate) fn send_http_with_body(
    url: &str,
    headers: &[(String, String)],
    body_str: &str,
    content_type: &str,
) -> Result<u16, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let mut req = client.post(url).header("Content-Type", content_type);
    for (k, v) in headers {
        if !k.is_empty() {
            req = req.header(k, v);
        }
    }
    req = req.body(body_str.to_string());
    let resp = req.send().map_err(|e| format!("send failed: {}", e))?;
    let status = resp.status().as_u16();
    if resp.status().is_success() {
        Ok(status)
    } else {
        Err(format!("HTTP {}", status))
    }
}

/// Manually trigger one test_send (dedup disabled), returning the HTTP status code.
/// Used by the settings UI "Test" button — lets the user immediately see whether the webhook works.
pub fn test_send() -> Result<u16, String> {
    let cfg = load_config();
    if !cfg.enabled {
        return Err("webhook not enabled".into());
    }
    validate_url(&cfg.url)?;
    let body = serde_json::json!({
        "source": "webhook.test",
        "timestamp": crate::core::agent::now_secs(),
        "data": { "message": "Test alert from OpenCapX" },
    });
    let body_str = serde_json::to_string(&body).map_err(|e| format!("serialize: {}", e))?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let mut req = client
        .post(&cfg.url)
        .header("Content-Type", "application/json");
    for (k, v) in &cfg.custom_headers {
        if !k.is_empty() {
            req = req.header(k, v);
        }
    }
    req = req.body(body_str);
    let resp = req.send().map_err(|e| format!("send: {}", e))?;
    let status = resp.status().as_u16();
    if resp.status().is_success() {
        Ok(status)
    } else {
        Err(format!("HTTP {}", status))
    }
}

/// Start the dispatcher background thread: subscribe to the EventBus and bridge the 4 alert kinds to the webhook.
/// Idempotent: only the first call spawns the thread.
pub fn spawn_dispatcher() {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    let bus = EventBus::shared();
    std::thread::spawn(move || {
        for e in bus.subscribe() {
            let kind = e.kind.as_str();
            match kind {
                "plugin.metrics.exceeded"
                | "capability.sla.violated"
                | "plugin.kill_switch.enabled"
                | "plugin.lifecycle.crashed" => {
                    dispatch(kind, e.payload);
                }
                _ => {}
            }
        }
    });
}

/// Test helper: clear the dedup cache (forces the next dispatch to re-send).
#[cfg(test)]
pub fn _reset_for_tests() {
    let arc = shared();
    let result = arc.lock();
    if let Ok(mut s) = result {
        s.last_sent.clear();
    }
}

/// Return the default severity for a source kind; unknown sources use info.
pub fn severity_for_source(source: &str) -> Severity {
    match source {
        "plugin.metrics.exceeded" => Severity::Warn,
        "capability.sla.violated" => Severity::Error,
        "plugin.kill_switch.enabled" => Severity::Critical,
        "plugin.lifecycle.crashed" => Severity::Critical,
        "webhook.test" => Severity::Info,
        _ => Severity::Info,
    }
}

/// Generate a UUID v4 event_id. Uses the uuid crate v4 feature (already in deps, used directly).
pub fn gen_event_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Build the canonical envelope from `(source, payload)` (schema_version=1).
/// `tags` defaults to empty; Phase 51 routes may put extra tags there, but dispatch() does not read them yet.
/// severity follows the Phase 53 chain (stored hint → hardcoded default → Info), ensuring the plugin manifest takes effect.
pub fn make_envelope(source: &str, payload: serde_json::Value, tags: Vec<String>) -> AlertEnvelope {
    AlertEnvelope {
        schema_version: ALERT_SCHEMA_VERSION,
        event_id: gen_event_id(),
        source: source.to_string(),
        severity: severity_resolved(source),
        tags,
        timestamp: crate::core::agent::now_secs(),
        payload,
    }
}

/// Phase 54 — severity override version of make_envelope, used by frequency threshold aggregation (downgrade).
/// Other fields are unchanged; event_id is still regenerated each time (each delivery is a new event).
pub fn make_envelope_with_severity(
    source: &str,
    payload: serde_json::Value,
    tags: Vec<String>,
    severity: Severity,
) -> AlertEnvelope {
    AlertEnvelope {
        schema_version: ALERT_SCHEMA_VERSION,
        event_id: gen_event_id(),
        source: source.to_string(),
        severity,
        tags,
        timestamp: crate::core::agent::now_secs(),
        payload,
    }
}

/// Serialize the envelope to a JSON string; the signature is computed over the whole string's bytes (avoiding re-serialization drift).
pub fn envelope_to_json_string(env: &AlertEnvelope) -> Result<String, String> {
    serde_json::to_string(env).map_err(|e| format!("envelope serialize: {}", e))
}
