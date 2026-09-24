//! Reverse-call path: rate limiting/dedup (S3), permission-gate waiters, and handle_reverse dispatch.
//! Mechanical move from core/plugin.rs.

use super::*;

/// Reverse rate-limit parameters (settings can override): `reverse_rate_per_sec` (default 50/s), `reverse_burst` (default 100).
/// review F5 — 5s TTL cache: avoid locking SQLite on every frame during a flood (the limiter must not be turned into an amplifier by DB locks).
fn reverse_limits() -> (f64, f64) {
    static CACHE: OnceLock<Mutex<Option<(std::time::Instant, f64, f64)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((at, r, b)) = *g {
            if at.elapsed().as_secs_f64() < 5.0 {
                return (r, b);
            }
        }
    }
    let read = |k: &str| -> Option<f64> {
        crate::core::shared_store()
            .and_then(|s| s.lock().ok().and_then(|g| g.get_setting(k)))
            .and_then(|v| v.parse::<f64>().ok())
    };
    let rate = read("reverse_rate_per_sec").unwrap_or(50.0).max(1.0);
    let burst = read("reverse_burst").unwrap_or(100.0).max(1.0);
    if let Ok(mut g) = cache.lock() {
        *g = Some((std::time::Instant::now(), rate, burst));
    }
    (rate, burst)
}

/// per-plugin token bucket (combined limit for core.emit + core.log).
struct ReverseBucket {
    tokens: f64,
    last_refill: std::time::Instant,
    /// Start of the current drop window (a summary is emitted when the window closes after 1s).
    window_start: Option<std::time::Instant>,
    window_dropped: u64,
    last_summary: Option<std::time::Instant>,
}

fn reverse_buckets() -> &'static Mutex<HashMap<String, ReverseBucket>> {
    static BUCKETS: OnceLock<Mutex<HashMap<String, ReverseBucket>>> = OnceLock::new();
    BUCKETS.get_or_init(|| {
        // review F5 — a background flush of lazy windows every minute: even if the flood abruptly stops (no more frames to trigger it), the final dropped count is still reported.
        std::thread::spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            flush_all_reverse_windows();
        });
        Mutex::new(HashMap::new())
    })
}

/// Background fallback for lazy windows: iterate all buckets, close windows that have been full for 1s, and report (emit events outside the lock).
fn flush_all_reverse_windows() {
    let pending = {
        let Ok(mut map) = reverse_buckets().lock() else {
            return;
        };
        let now = std::time::Instant::now();
        let mut acc: Vec<(String, u64, f64)> = Vec::new();
        for (pid, b) in map.iter_mut() {
            if let Some((dropped, secs)) = flush_reverse_window(b, now) {
                acc.push((pid.clone(), dropped, secs));
            }
        }
        acc
    };
    for (pid, dropped, secs) in pending {
        publish_throttled(&pid, dropped, secs);
    }
}

/// Close a drop window that has been full for 1s → produce a summary (the summary is itself rate-limited: ≥1s since the last one, preventing a feedback loop).
fn flush_reverse_window(b: &mut ReverseBucket, now: std::time::Instant) -> Option<(u64, f64)> {
    let start = b.window_start?;
    let elapsed = now.duration_since(start).as_secs_f64();
    if elapsed < 1.0 {
        return None;
    }
    if let Some(last) = b.last_summary {
        if now.duration_since(last).as_secs_f64() < 1.0 {
            return None;
        }
    }
    let out = Some((b.window_dropped, elapsed));
    b.window_dropped = 0;
    b.window_start = None;
    b.last_summary = Some(now);
    out
}

/// Whether a reverse event is allowed; over the limit it is dropped and (on window close) one `plugin.throttled` summary is emitted.
/// (O2 — also reused by process.rs's stderr path; chatty stderr must not bypass rate limiting.)
pub(crate) fn reverse_allow(plugin_id: &str) -> bool {
    let (rate, burst) = reverse_limits();
    let now = std::time::Instant::now();
    let (allowed, summary) = {
        let Ok(mut map) = reverse_buckets().lock() else {
            return true;
        };
        let b = map
            .entry(plugin_id.to_string())
            .or_insert_with(|| ReverseBucket {
                tokens: burst,
                last_refill: now,
                window_start: None,
                window_dropped: 0,
                last_summary: None,
            });
        let dt = now.duration_since(b.last_refill).as_secs_f64();
        b.tokens = (b.tokens + dt * rate).min(burst);
        b.last_refill = now;
        let summary = flush_reverse_window(b, now);
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            (true, summary)
        } else {
            if b.window_start.is_none() {
                b.window_start = Some(now);
            }
            b.window_dropped += 1;
            (false, summary)
        }
    };
    if let Some((dropped, window_secs)) = summary {
        publish_throttled(plugin_id, dropped, window_secs);
    }
    allowed
}

/// Emit one `plugin.throttled` summary (on window close; the throttle event itself is also constrained to a ≥1s window).
fn publish_throttled(plugin_id: &str, dropped: u64, window_secs: f64) {
    let (rate, burst) = reverse_limits();
    crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
        "plugin.throttled",
        &format!("plugin:{}", plugin_id),
        json!({
            "pluginId": plugin_id,
            "dropped": dropped,
            "windowSecs": window_secs,
            "ratePerSec": rate,
            "burst": burst,
        }),
    ));
}

/// In-flight requestPermission waiters: the same (plugin, permission) shares a single decision,
/// and all are replied to when the decision completes — N requests produce only 1 prompt.
struct PermWaiters {
    replies: Vec<(serde_json::Value, crate::core::process::Reply)>,
}

fn permission_pending() -> &'static Mutex<HashMap<(String, String), PermWaiters>> {
    static PENDING: OnceLock<Mutex<HashMap<(String, String), PermWaiters>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registers a waiter; returns whether this is the first request for the key (the first one runs the gate).
pub(crate) fn register_permission_waiter(
    key: &(String, String),
    id: Option<serde_json::Value>,
    reply: crate::core::process::Reply,
) -> bool {
    let Ok(mut map) = permission_pending().lock() else {
        return false;
    };
    match map.get_mut(key) {
        Some(w) => {
            if let Some(id) = id {
                w.replies.push((id, reply));
            }
            false
        }
        None => {
            let mut w = PermWaiters {
                replies: Vec::new(),
            };
            if let Some(id) = id {
                w.replies.push((id, reply));
            }
            map.insert(key.clone(), w);
            true
        }
    }
}

/// Decision complete: remove the key and reply to all waiters (newly arriving ones are under the same key too).
pub(crate) fn settle_permission_waiters(key: &(String, String), granted: bool) {
    let waiters = permission_pending()
        .lock()
        .ok()
        .and_then(|mut m| m.remove(key));
    if let Some(w) = waiters {
        for (id, reply) in w.replies {
            reply(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "granted": granted }
            }));
        }
    }
}

#[cfg(test)]
pub(crate) static PERM_GATE_RUNS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Runs the permission gate once (same source as the old behavior: known precheck + gate).
fn run_permission_gate(plugin_id: &str, permission: &str, reason: Option<&str>) -> bool {
    #[cfg(test)]
    {
        PERM_GATE_RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // the concurrency-dedup test needs deterministic overlap: make the gate slightly slow so the second request necessarily attaches to the same decision.
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    crate::core::permission::known(permission)
        && crate::core::permission::gate(
            &PluginManager::store().unwrap_or_else(|| {
                Arc::new(Mutex::new(crate::core::storage::StoreEnum::Mem(
                    crate::core::agent::SessionStore::new(),
                )))
            }),
            plugin_id,
            permission,
            "plugin.reverse",
            reason,
        ) == crate::core::permission::Decision::Granted
}

/// Dedup + async: the first request runs the gate on a separate thread (the gate blocks waiting for the user and must not stall the same plugin's other reverse frames).
pub(crate) fn enqueue_permission_request(
    plugin_id: String,
    permission: String,
    reason: Option<String>,
    id: Option<serde_json::Value>,
    reply: crate::core::process::Reply,
) {
    let key = (plugin_id.clone(), permission.clone());
    if !register_permission_waiter(&key, id, reply) {
        return;
    }
    std::thread::spawn(move || {
        let granted = run_permission_gate(&plugin_id, &permission, reason.as_deref());
        settle_permission_waiters(&key, granted);
    });
}

/// Plugin reverse requests/notifications. See docs/plugin-protocol.md.
pub(crate) fn handle_reverse(
    plugin_id: &str,
    v: serde_json::Value,
    reply: crate::core::process::Reply,
) {
    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    match method {
        "core.log" => {
            if !reverse_allow(plugin_id) {
                return;
            }
            let level = v
                .pointer("/params/level")
                .and_then(|x| x.as_str())
                .unwrap_or("info");
            let msg = v
                .pointer("/params/message")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            eprintln!("[plugin:{}] [{}] {}", plugin_id, level, msg);
            crate::core::event::EventBus::shared().publish(
                &crate::core::event::OpencapxEvent::new(
                    "plugin.log",
                    &format!("plugin:{}", plugin_id),
                    json!({
                        "pluginId": plugin_id,
                        "level": level,
                        "source": "reverse",
                        "message": msg,
                    }),
                ),
            );
        }
        "core.emit" => {
            if !reverse_allow(plugin_id) {
                return;
            }
            let kind = v
                .pointer("/params/type")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if !kind.is_empty() {
                crate::core::event::EventBus::shared().publish(
                    &crate::core::event::OpencapxEvent::new(
                        &format!("{}.{}", plugin_id, kind),
                        &format!("plugin:{}", plugin_id),
                        v.pointer("/params/payload").cloned().unwrap_or(json!(null)),
                    ),
                );
            }
        }
        "core.requestPermission" => {
            let permission = v
                .pointer("/params/permission")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let reason = v
                .pointer("/params/reason")
                .and_then(|x| x.as_str())
                .map(str::to_string);
            let id = v.get("id").cloned();
            // S3: dedup (N requests, 1 prompt) + move off the reader thread (so waiting on the user does not stall the same plugin's other frames)
            enqueue_permission_request(plugin_id.to_string(), permission, reason, id, reply);
        }
        "plugin.subscribe" => {
            let kind = v
                .pointer("/params/kind")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let res =
                crate::core::subscriber::SubscriptionRegistry::shared().subscribe(plugin_id, kind);
            if let Some(id) = v.get("id").cloned() {
                let (ok, msg) = match res {
                    Ok(_) => (true, serde_json::Value::Null),
                    Err(e) => (false, serde_json::Value::String(e)),
                };
                reply(json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": ok, "error": msg } }));
            }
        }
        "plugin.unsubscribe" => {
            let kind = v
                .pointer("/params/kind")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            crate::core::subscriber::SubscriptionRegistry::shared().unsubscribe(plugin_id, kind);
            if let Some(id) = v.get("id").cloned() {
                reply(json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": true } }));
            }
        }
        _ if method.starts_with("config.") => {
            // of the form config.{op}: get / set / delete / all / list
            let op = method.trim_start_matches("config.");
            let params = v.get("params").cloned().unwrap_or(json!({}));
            let res = crate::core::config::dispatch(plugin_id, op, &params);
            if let Some(id) = v.get("id").cloned() {
                match res {
                    Ok(value) => reply(json!({ "jsonrpc": "2.0", "id": id, "result": value })),
                    Err(e) => reply(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32603, "message": e }
                    })),
                }
            }
        }
        _ => {
            if let Some(id) = v.get("id").cloned() {
                reply(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("unknown reverse method {}", method) }
                }));
            }
        }
    }
}
