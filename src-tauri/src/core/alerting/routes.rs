//! route CRUD, the when/then matching DSL, YAML import/export, dry-run.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Validate route rule arguments.
pub fn validate_route(r: &RouteRule) -> Result<(), String> {
    if r.name.trim().is_empty() {
        return Err("route name is required".into());
    }
    if r.name.len() > 128 {
        return Err("route name too long (max 128 chars)".into());
    }
    if r.kind_pattern.is_empty() {
        return Err("kind_pattern is required (use '*' for all)".into());
    }
    if r.kind_pattern.len() > 256 {
        return Err("kind_pattern too long (max 256 chars)".into());
    }
    if r.target_endpoint_ids.is_empty() && r.recipients.is_empty() {
        return Err("route must have either target_endpoint_ids or recipients".into());
    }
    if r.target_endpoint_ids.len() > 32 {
        return Err("target_endpoint_ids too many (max 32)".into());
    }
    if r.recipients.len() > 32 {
        return Err("recipients too many (max 32)".into());
    }
    for t in &r.target_endpoint_ids {
        if t.is_empty() || t.len() > 128 {
            return Err("target_endpoint_id invalid (empty or > 128 chars)".into());
        }
    }
    if r.priority < -1000 || r.priority > 10000 {
        return Err("priority must be in -1000..=10000".into());
    }
    if let Some(p) = &r.payload_path {
        if p.is_empty() || p.len() > 256 {
            return Err("payload_path invalid (empty or > 256 chars)".into());
        }
    }
    if let Some(m) = &r.payload_match {
        if m.len() > 1024 {
            return Err("payload_match too long (max 1024 chars)".into());
        }
    }
    if r.tags.len() > 32 {
        return Err("tags too many (max 32)".into());
    }
    for t in &r.tags {
        if t.is_empty() || t.len() > 64 {
            return Err("tag invalid (empty or > 64 chars)".into());
        }
    }
    // Phase 68 — seen_in_last validation
    if let Some(spec) = &r.seen_in_last {
        if spec.pattern.trim().is_empty() {
            return Err("seen_in_last.pattern is required".into());
        }
        if spec.pattern.len() > 256 {
            return Err("seen_in_last.pattern too long (max 256 chars)".into());
        }
        if spec.window_secs == 0 {
            return Err("seen_in_last.window_secs must be > 0".into());
        }
        if spec.window_secs > 3600 {
            return Err("seen_in_last.window_secs too long (max 3600)".into());
        }
    }
    Ok(())
}

/// Generate a route id.
fn gen_route_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("rt-{}-{}", nanos, n)
}

/// List all routes (including disabled).
pub fn list_routes() -> Vec<RouteRuleRow> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let Ok(s) = store.lock() else {
        return Vec::new();
    };
    if let StoreEnum::Db(db) = &*s {
        db.list_alerting_routes()
    } else {
        Vec::new()
    }
}

/// List only enabled routes (by priority ASC).
pub fn list_enabled_routes() -> Vec<RouteRuleRow> {
    list_routes().into_iter().filter(|r| r.enabled).collect()
}

/// Upsert a route (empty id = create). Keeps the old created_at to avoid a UI jump.
pub fn save_route(rule: RouteRule) -> Result<RouteRuleRow, String> {
    validate_route(&rule)?;
    let Some(store) = crate::core::shared_store() else {
        return Err("store not initialized".into());
    };
    let Ok(mut st) = store.lock() else {
        return Err("store poisoned".into());
    };
    let StoreEnum::Db(db) = &mut *st else {
        return Err("sqlite store required".into());
    };
    let id = if rule.id.is_empty() {
        gen_route_id()
    } else {
        rule.id.clone()
    };
    let now_secs = crate::core::agent::now_secs();
    let created_at = db
        .list_alerting_routes()
        .into_iter()
        .find(|r| r.id == id)
        .map(|r| r.created_at)
        .unwrap_or(now_secs);
    let row = RouteRuleRow {
        id,
        name: rule.name,
        priority: rule.priority,
        enabled: rule.enabled,
        kind_pattern: rule.kind_pattern,
        payload_path: rule.payload_path,
        payload_match: rule.payload_match,
        target_endpoint_ids: rule.target_endpoint_ids,
        recipients: rule.recipients,
        tags: rule.tags,
        seen_in_last_json: seen_in_last_to_json(&rule.seen_in_last),
        created_at,
    };
    db.upsert_alerting_route(&row);
    Ok(row)
}

pub fn delete_route(id: &str) -> bool {
    let Some(store) = crate::core::shared_store() else {
        return false;
    };
    let Ok(mut s) = store.lock() else {
        return false;
    };
    if let StoreEnum::Db(db) = &mut *s {
        db.delete_alerting_route(id)
    } else {
        false
    }
}

/// Take a `Value` reference from the payload using a simple JSON-pointer-style path (`a.b.c`).
/// Array indices are written as `a.0.b`; not found → None.
pub(crate) fn payload_path_get<'a>(
    payload: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let mut cur = payload;
    for seg in path.split('.') {
        if seg.is_empty() {
            return None;
        }
        // numeric segment treated as an array index
        if let Ok(idx) = seg.parse::<usize>() {
            cur = cur.get(idx)?;
        } else {
            cur = cur.get(seg)?;
        }
    }
    Some(cur)
}

/// Normalize any JSON Value into a comparable string or number.
/// - Number → Some(f64 + its original repr string)
/// - String → Some(f64 from string) OR (None, original_str)
/// - other types → return None (not part of numeric comparison)
fn value_as_number(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// Minimal payload_match expression matching:
/// - `>=N` / `<=N` / `>N` / `<N` / `==N` / `=N` → numeric comparison (when the payload field parses as f64)
/// - `prefix:foo` → string prefix
/// - `contains:foo` → string contains
/// - otherwise → exact string / numeric equality
fn payload_match(payload: &serde_json::Value, path: &str, expr: &str) -> bool {
    let Some(value) = payload_path_get(payload, path) else {
        return false;
    };
    let expr = expr.trim();
    // numeric comparison
    if let Some(rest) = expr.strip_prefix(">=") {
        return value_as_number(value)
            .zip(rest.trim().parse::<f64>().ok())
            .map(|(a, b)| a >= b)
            .unwrap_or(false);
    }
    if let Some(rest) = expr.strip_prefix("<=") {
        return value_as_number(value)
            .zip(rest.trim().parse::<f64>().ok())
            .map(|(a, b)| a <= b)
            .unwrap_or(false);
    }
    if let Some(rest) = expr.strip_prefix("==") {
        return value_as_number(value)
            .zip(rest.trim().parse::<f64>().ok())
            .map(|(a, b)| (a - b).abs() < f64::EPSILON)
            .unwrap_or(false);
    }
    if let Some(rest) = expr.strip_prefix('>') {
        return value_as_number(value)
            .zip(rest.trim().parse::<f64>().ok())
            .map(|(a, b)| a > b)
            .unwrap_or(false);
    }
    if let Some(rest) = expr.strip_prefix('<') {
        return value_as_number(value)
            .zip(rest.trim().parse::<f64>().ok())
            .map(|(a, b)| a < b)
            .unwrap_or(false);
    }
    if let Some(rest) = expr.strip_prefix('=') {
        let rest = rest.trim();
        // numeric equal
        if let (Some(a), Ok(b)) = (value_as_number(value), rest.parse::<f64>()) {
            return (a - b).abs() < f64::EPSILON;
        }
        // string equal
        return value.as_str().map(|s| s == rest).unwrap_or(false);
    }
    // string prefix / contains
    let Some(s) = value.as_str() else {
        return false;
    };
    if let Some(rest) = expr.strip_prefix("prefix:") {
        return s.starts_with(rest);
    }
    if let Some(rest) = expr.strip_prefix("contains:") {
        return s.contains(rest);
    }
    s == expr
}

/// Check whether a single route matches `(kind, payload)`.
pub fn route_matches(rule: &RouteRuleRow, kind: &str, payload: &serde_json::Value) -> bool {
    if !rule.enabled {
        return false;
    }
    if !kind_matches(&rule.kind_pattern, kind) {
        return false;
    }
    if let (Some(path), Some(expr)) = (&rule.payload_path, &rule.payload_match) {
        if !payload_match(payload, path, expr) {
            return false;
        }
    }
    // Phase 68 — temporal correlation condition
    if let Some(json) = &rule.seen_in_last_json {
        if let Ok(spec) = serde_json::from_str::<SeenInLastSpec>(json) {
            if !recently_seen(
                &spec.pattern,
                spec.window_secs,
                crate::core::agent::now_secs(),
            ) {
                return false;
            }
        }
    }
    true
}

/// Given `(kind, payload)`, return the first enabled + matching route (priority ASC).
pub fn match_route(kind: &str, payload: &serde_json::Value) -> Option<RouteRuleRow> {
    let routes = list_enabled_routes();
    routes.into_iter().find(|r| route_matches(r, kind, payload))
}

/// YAML import: parse + validate + atomic replace. Returns the number inserted.
pub fn import_routes_yaml(yaml: &str) -> Result<usize, String> {
    let doc: RouteRuleYamlDoc =
        serde_yaml::from_str(yaml).map_err(|e| format!("yaml parse error: {}", e))?;
    if doc.rules.is_empty() {
        return Err("yaml contains no rules".into());
    }
    for r in &doc.rules {
        validate_route(r)?;
    }
    let Some(store) = crate::core::shared_store() else {
        return Err("store not initialized".into());
    };
    let Ok(mut s) = store.lock() else {
        return Err("store poisoned".into());
    };
    let StoreEnum::Db(db) = &mut *s else {
        return Err("sqlite store required".into());
    };
    db.clear_alerting_routes();
    let now_secs = crate::core::agent::now_secs();
    for r in doc.rules {
        let row = RouteRuleRow {
            id: if r.id.is_empty() {
                gen_route_id()
            } else {
                r.id
            },
            name: r.name,
            priority: r.priority,
            enabled: r.enabled,
            kind_pattern: r.kind_pattern,
            payload_path: r.payload_path,
            payload_match: r.payload_match,
            target_endpoint_ids: r.target_endpoint_ids,
            recipients: r.recipients,
            tags: r.tags,
            seen_in_last_json: seen_in_last_to_json(&r.seen_in_last),
            created_at: now_secs,
        };
        db.upsert_alerting_route(&row);
    }
    Ok(db.list_alerting_routes().len())
}

/// YAML export: package the current routes into a yaml string.
pub fn export_routes_yaml() -> Result<String, String> {
    let rows = list_routes();
    let rules: Vec<RouteRule> = rows
        .into_iter()
        .map(|r| RouteRule {
            id: r.id,
            name: r.name,
            priority: r.priority,
            enabled: r.enabled,
            kind_pattern: r.kind_pattern,
            payload_path: r.payload_path,
            payload_match: r.payload_match,
            target_endpoint_ids: r.target_endpoint_ids,
            recipients: r.recipients,
            tags: r.tags,
            seen_in_last: r
                .seen_in_last_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<SeenInLastSpec>(s).ok()),
        })
        .collect();
    let doc = RouteRuleYamlDoc { version: 1, rules };
    serde_yaml::to_string(&doc).map_err(|e| format!("yaml serialize error: {}", e))
}

/// Dry-run: run a `(source, payload)` against the current route table to see which one hits.
/// payload_json is a string (so the frontend can pass a JSON literal directly).
pub fn dry_run_route(source: &str, payload_json: &str) -> Result<Option<RouteRuleRow>, String> {
    let payload: serde_json::Value =
        serde_json::from_str(payload_json).map_err(|e| format!("payload not valid json: {}", e))?;
    Ok(match_route(source, &payload))
}

/// Phase 51 dispatch integration hook: `route_targets` is the set of endpoint_ids of the hit routes.
/// If Some → send only to those endpoints (ignoring endpoint.source_filter / compatibility fanout).
/// If None → use the legacy fanout.
pub fn route_target_ids(kind: &str, payload: &serde_json::Value) -> Option<Vec<String>> {
    match_route(kind, payload).map(|r| r.target_endpoint_ids)
}
