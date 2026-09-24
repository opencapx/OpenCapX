//! endpoint CRUD + test send.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Generate an endpoint id (based on nanos + an atomic counter).
fn gen_endpoint_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ep-{}-{}", nanos, n)
}

/// List all endpoints (including disabled). Sorted by created_at ascending.
pub fn list_endpoints() -> Vec<AlertingEndpointRow> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let Ok(s) = store.lock() else {
        return Vec::new();
    };
    if let StoreEnum::Db(db) = &*s {
        db.list_alerting_endpoints()
    } else {
        Vec::new()
    }
}

/// Create or update an endpoint. Empty `id` = create; non-empty = update the row with the same id.
/// Validation failure → Err; name unique constraint → Err.
pub fn save_endpoint(ep: WebhookEndpoint) -> Result<AlertingEndpointRow, String> {
    validate_endpoint(&ep)?;
    let Some(store) = crate::core::shared_store() else {
        return Err("store not initialized".into());
    };
    let Ok(mut s) = store.lock() else {
        return Err("store poisoned".into());
    };
    let StoreEnum::Db(db) = &mut *s else {
        return Err("sqlite store required".into());
    };

    // same name but different id → reject (UNIQUE would reject too, but this gives a friendly error).
    for existing in db.list_alerting_endpoints() {
        if existing.name == ep.name && existing.id != ep.id {
            return Err(format!("endpoint name already in use: {}", ep.name));
        }
    }

    let id = if ep.id.is_empty() {
        gen_endpoint_id()
    } else {
        ep.id.clone()
    };
    let now_secs = crate::core::agent::now_secs();
    let row = AlertingEndpointRow {
        id: id.clone(),
        name: ep.name,
        url: ep.url,
        enabled: ep.enabled,
        headers: ep.headers,
        secret: ep.secret,
        source_filter: ep.source_filter,
        schema_version: ep.schema_version,
        template: ep.template,
        template_sample: ep.template_sample,
        // Phase 72 — serialize severity_overrides as a JSON Vec<(String, String)>.
        // empty list → store NULL (lets endpoint_row_to_dto return an empty Vec).
        severity_overrides: if ep.severity_overrides.is_empty() {
            None
        } else {
            let pairs: Vec<(String, String)> = ep
                .severity_overrides
                .iter()
                .map(|(s, sev)| (s.clone(), sev.as_str().to_string()))
                .collect();
            serde_json::to_string(&pairs).ok()
        },
        // use now for create; keep the existing row's created_at (avoids a time jump in the UI).
        created_at: db
            .get_alerting_endpoint(&id)
            .map(|e| e.created_at)
            .unwrap_or(now_secs),
    };
    db.upsert_alerting_endpoint(&row);
    Ok(row)
}

/// Delete an endpoint and cascade-clear its dead letters. Returns (deleted, deliveries_cleared).
pub fn delete_endpoint(id: &str) -> Result<(bool, usize), String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("store not initialized".into());
    };
    let Ok(mut s) = store.lock() else {
        return Err("store poisoned".into());
    };
    let StoreEnum::Db(db) = &mut *s else {
        return Err("sqlite store required".into());
    };
    Ok(db.delete_alerting_endpoint(id))
}

/// Fetch the endpoint config → immediately POST once with source="webhook.test" (bypassing dedup).
/// Failure → Err; success → Ok(status_code). Used by the settings UI per-endpoint Test button.
pub fn test_endpoint(id: &str) -> Result<u16, String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("store not initialized".into());
    };
    let Ok(s) = store.lock() else {
        return Err("store poisoned".into());
    };
    let StoreEnum::Db(db) = &*s else {
        return Err("sqlite store required".into());
    };
    let ep = db
        .get_alerting_endpoint(id)
        .ok_or_else(|| format!("endpoint not found: {}", id))?;
    drop(s);
    validate_url(&ep.url)?;
    let body = serde_json::json!({
        "source": "webhook.test",
        "timestamp": crate::core::agent::now_secs(),
        "data": { "endpoint": ep.name, "message": "Test alert from OpenCapX" },
    });
    let body_str = serde_json::to_string(&body).map_err(|e| format!("serialize: {}", e))?;
    let headers = build_request_headers(&ep, &body_str);
    send_http_with_body(&ep.url, &headers, &body_str, "application/json")
}
