//! dead-letter queue, exponential backoff, retry loops.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Retry parameters. `max_attempts` includes the first attempt (i.e. attempts=1 → there are max_attempts-1 retries left).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub initial_backoff_secs: u32,
    pub max_backoff_secs: u32,
    pub retention_days: u32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff_secs: 30,
            max_backoff_secs: 600,
            retention_days: 7,
        }
    }
}

pub fn validate_retry(cfg: &RetryConfig) -> Result<(), String> {
    if cfg.max_attempts == 0 || cfg.max_attempts > 100 {
        return Err(format!("max_attempts out of range: {}", cfg.max_attempts));
    }
    if cfg.initial_backoff_secs == 0 || cfg.initial_backoff_secs > 86400 {
        return Err(format!(
            "initial_backoff_secs out of range: {}",
            cfg.initial_backoff_secs
        ));
    }
    if cfg.max_backoff_secs < cfg.initial_backoff_secs {
        return Err("max_backoff_secs must be >= initial_backoff_secs".into());
    }
    if cfg.max_backoff_secs > 86400 * 7 {
        return Err("max_backoff_secs too large (max 7 days)".into());
    }
    if cfg.retention_days > 365 {
        return Err("retention_days too large (max 365)".into());
    }
    Ok(())
}

pub fn load_retry_config() -> RetryConfig {
    let Some(store) = crate::core::shared_store() else {
        return RetryConfig::default();
    };
    store
        .lock()
        .ok()
        .and_then(|s| s.get_setting(RETRY_CONFIG_KEY))
        .and_then(|json| serde_json::from_str::<RetryConfig>(&json).ok())
        .unwrap_or_default()
}

pub fn save_retry_config(cfg: &RetryConfig) -> Result<(), String> {
    validate_retry(cfg)?;
    let store = crate::core::shared_store().ok_or_else(|| "store not initialized".to_string())?;
    let mut s = store.lock().map_err(|_| "store poisoned".to_string())?;
    let json = serde_json::to_string(cfg).map_err(|e| format!("serialize: {}", e))?;
    s.set_setting(RETRY_CONFIG_KEY, &json);
    Ok(())
}

/// Exponential backoff: `initial * 2^(attempts-1)`, capped at `max_backoff_secs`.
/// `attempts` is the "number of attempts made" (attempt 1 fails → wait initial_backoff_secs before retrying).
pub fn compute_backoff_secs(attempts: u32, cfg: &RetryConfig) -> u64 {
    if attempts == 0 || cfg.initial_backoff_secs == 0 {
        return cfg.initial_backoff_secs as u64;
    }
    // use a saturating shift to prevent overflow when attempts is very large
    let shift = attempts.saturating_sub(1).min(20);
    let factor: u64 = 1u64 << shift;
    let raw = (cfg.initial_backoff_secs as u64).saturating_mul(factor);
    raw.min(cfg.max_backoff_secs as u64)
}

/// Generate a unique ID (based on nanos + an atomic counter). Phase 41 backup uses a similar approach.
fn gen_delivery_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("dl-{}-{}", nanos, n)
}

/// Enqueue into the dead-letter queue — called when a POST fails. `endpoint_id` records ownership in multi-endpoint mode; None takes the legacy single-endpoint compatibility path.
pub fn enqueue_failed_delivery(
    source: &str,
    url: &str,
    payload_json: &str,
    now_ts: u64,
    max_attempts: u32,
    next_retry_ts: u64,
    last_error: &str,
    endpoint_id: Option<&str>,
) {
    let Some(store) = crate::core::shared_store() else {
        return;
    };
    let Ok(mut s) = store.lock() else { return };
    let id = gen_delivery_id();
    if let StoreEnum::Db(db) = &mut *s {
        db.insert_failed_delivery(
            &id,
            source,
            url,
            payload_json,
            now_ts,
            max_attempts,
            next_retry_ts,
            last_error,
            endpoint_id,
        );
    }
}

/// List dead letters for a state (state=None lists all). limit defaults to 100, clamped to [1, 1000].
pub fn list_failed_deliveries(state: Option<&str>, limit: usize) -> Vec<FailedDeliveryRow> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let Ok(s) = store.lock() else {
        return Vec::new();
    };
    if let StoreEnum::Db(db) = &*s {
        db.list_failed_deliveries(state, limit.max(1).min(1000))
    } else {
        Vec::new()
    }
}

/// Manually retry one — resets attempts=0 / state=pending / next_retry_ts=now.
pub fn manual_retry_failed_delivery(id: &str) -> bool {
    let Some(store) = crate::core::shared_store() else {
        return false;
    };
    let Ok(mut s) = store.lock() else {
        return false;
    };
    if let StoreEnum::Db(db) = &mut *s {
        db.reset_failed_delivery_for_retry(id, crate::core::agent::now_secs());
        true
    } else {
        false
    }
}

/// Delete a dead letter.
pub fn delete_failed_delivery(id: &str) -> bool {
    let Some(store) = crate::core::shared_store() else {
        return false;
    };
    let Ok(mut s) = store.lock() else {
        return false;
    };
    if let StoreEnum::Db(db) = &mut *s {
        db.delete_failed_delivery(id)
    } else {
        false
    }
}

/// Clear all exhausted + resolved rows (for the user emptying the panel).
pub fn clear_resolved_failed_deliveries() -> usize {
    let Some(store) = crate::core::shared_store() else {
        return 0;
    };
    let Ok(mut s) = store.lock() else { return 0 };
    if let StoreEnum::Db(db) = &mut *s {
        db.clear_resolved_failed_deliveries()
    } else {
        0
    }
}

/// Purge old rows past retention — run periodically by the retry loop.
pub fn prune_old_failed_deliveries() {
    let cfg = load_retry_config();
    let now = crate::core::agent::now_secs();
    let Some(store) = crate::core::shared_store() else {
        return;
    };
    let Ok(mut s) = store.lock() else { return };
    if let StoreEnum::Db(db) = &mut *s {
        db.prune_old_failed_deliveries(now, cfg.retention_days);
    }
}

/// Fetch a batch of due pending rows → re-POST each → update the result. Returns (processed, resolved).
/// `now_ts` is passed explicitly to make testing easier.
/// Phase 49: if the dead letter carries an endpoint_id, try to fetch that endpoint's headers+secret; otherwise use empty headers.
pub fn retry_due_deliveries(now_ts: u64) -> (usize, usize) {
    let due = {
        let Some(store) = crate::core::shared_store() else {
            return (0, 0);
        };
        let Ok(s) = store.lock() else { return (0, 0) };
        match &*s {
            StoreEnum::Db(db) => db.fetch_due_failed_deliveries(now_ts, MAX_FETCH_PER_SCAN),
            StoreEnum::Mem(_) => return (0, 0),
        }
    };
    if due.is_empty() {
        return (0, 0);
    }
    let cfg = load_retry_config();
    let mut processed = 0usize;
    let mut resolved = 0usize;
    for row in due {
        processed += 1;
        let payload: serde_json::Value =
            serde_json::from_str(&row.payload).unwrap_or(serde_json::Value::Null);

        // Phase 49 — restore the original endpoint's headers+secret to preserve signature semantics.
        // Phase 57 — if the endpoint has a template, re-render with it (following the consistent envelope shape);
        // otherwise a legacy JSON wrapper.
        let (headers, body_str, content_type): (Vec<(String, String)>, String, String) = {
            let store = crate::core::shared_store();
            let lock = store.as_ref().and_then(|s| s.lock().ok());
            match (row.endpoint_id.as_deref(), lock.as_deref()) {
                (Some(eid), Some(StoreEnum::Db(db))) => match db.get_alerting_endpoint(eid) {
                    Some(ep) => {
                        if let Some(tpl) = ep.template.as_deref() {
                            // rebuild the envelope: the row is the dead-letter payload; follow the envelope shape + the retry ts.
                            let env = make_envelope_with_severity(
                                &row.source,
                                payload.clone(),
                                vec![],
                                severity_resolved(&row.source),
                            );
                            // change the timestamp to the retry ts
                            let mut env = env;
                            env.timestamp = now_ts;
                            match render_template(tpl, &env) {
                                Ok((s, ct)) => (
                                    build_request_headers(&ep, &s),
                                    s,
                                    ct.as_content_type().to_string(),
                                ),
                                Err(e) => {
                                    eprintln!(
                                        "[alerting] retry template render error on {}: {}",
                                        ep.name, e
                                    );
                                    let body = serde_json::json!({
                                        "source": row.source,
                                        "timestamp": now_ts,
                                        "data": payload,
                                    });
                                    let body_str = serde_json::to_string(&body)
                                        .unwrap_or_else(|_| row.payload.clone());
                                    (
                                        build_request_headers(&ep, &body_str),
                                        body_str,
                                        "application/json".to_string(),
                                    )
                                }
                            }
                        } else {
                            let body = serde_json::json!({
                                "source": row.source,
                                "timestamp": now_ts,
                                "data": payload,
                            });
                            let body_str = serde_json::to_string(&body)
                                .unwrap_or_else(|_| row.payload.clone());
                            (
                                build_request_headers(&ep, &body_str),
                                body_str,
                                "application/json".to_string(),
                            )
                        }
                    }
                    None => (
                        Vec::new(),
                        row.payload.clone(),
                        "application/json".to_string(),
                    ),
                },
                _ => (
                    Vec::new(),
                    row.payload.clone(),
                    "application/json".to_string(),
                ),
            }
        };

        let result = send_http_with_body(&row.url, &headers, &body_str, &content_type);
        let Some(store) = crate::core::shared_store() else {
            continue;
        };
        let Ok(mut s) = store.lock() else { continue };
        let StoreEnum::Db(db) = &mut *s else { continue };
        match result {
            Ok(_) => {
                db.mark_failed_delivery_resolved(&row.id, now_ts);
                resolved += 1;
            }
            Err(err_msg) => {
                let next_attempts = row.attempts + 1;
                let next_ts = if next_attempts >= row.max_attempts {
                    0 // trigger exhausted
                } else {
                    let backoff = compute_backoff_secs(next_attempts, &cfg);
                    now_ts.saturating_add(backoff)
                };
                db.update_failed_delivery_retry(&row.id, now_ts, next_ts, &err_msg);
            }
        }
    }
    (processed, resolved)
}

/// Start the retry loop background thread: scan for due dead letters every 30s and retry them.
/// Idempotent: only the first call spawns the thread.
pub fn spawn_retry_loop() {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(POLL_RETRY_SECS));
        let now = crate::core::agent::now_secs();
        let _ = retry_due_deliveries(now);
        prune_old_failed_deliveries();
    });
}
