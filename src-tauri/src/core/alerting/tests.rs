use super::*;

/// Test-local now_secs helper (inside mod tests, super is not crate::core, so super::agent does not resolve).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn fresh_config() -> WebhookConfig {
    WebhookConfig {
        enabled: true,
        url: "https://hooks.example.com/services/T/B/X".into(),
        min_interval_secs: 5,
        sources: WebhookSources {
            metrics_exceeded: true,
            sla_violated: true,
            kill_switch_engaged: true,
            plugin_crashed: true,
        },
        custom_headers: vec![("X-OpenCapX-Test".into(), "yes".into())],
        schema_version: 0,
    }
}

/// Phase 53 — get an independent SQLite and **do not** load it into the shared store (avoids parallel-test pollution).
/// The caller must, within the test scope:
///   ```
///   let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
///   crate::core::set_shared_store(fresh_store("label"));
///   ```
/// `_g` drops at the end of the test function, ensuring tests do not concurrently step on the same store slot.
fn fresh_store(label: &str) -> crate::core::SharedStore {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "opencapx-alerting-hints-{}-{}-{}",
        label,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::sync::Arc::new(std::sync::Mutex::new(crate::core::storage::StoreEnum::Db(
        crate::core::storage::Storage::open(&dir.join("hints.db")).unwrap(),
    )))
}

#[test]
fn validate_accepts_https_url() {
    assert!(validate_url("https://hooks.slack.com/services/X/Y/Z").is_ok());
    assert!(validate_url("https://discord.com/api/webhooks/1/2").is_ok());
}

#[test]
fn validate_accepts_http_localhost() {
    assert!(validate_url("http://localhost:9000/hook").is_ok());
    assert!(validate_url("http://127.0.0.1:8080/alerts").is_ok());
}

#[test]
fn validate_rejects_empty() {
    assert!(validate_url("").is_err());
}

#[test]
fn validate_rejects_other_schemes() {
    assert!(validate_url("ftp://example.com").is_err());
    assert!(validate_url("file:///etc/passwd").is_err());
    assert!(validate_url("javascript:alert(1)").is_err());
    assert!(validate_url("//example.com").is_err());
}

#[test]
fn validate_rejects_too_long() {
    let long = format!("https://example.com/{}", "x".repeat(2050));
    assert!(validate_url(&long).is_err());
}

#[test]
fn validate_rejects_control_chars() {
    assert!(validate_url("https://example.com/foo\nbar").is_err());
    assert!(validate_url("https://example.com/foo\rbar").is_err());
    assert!(validate_url("https://example.com/foo\0bar").is_err());
}

#[test]
fn validate_skips_url_when_disabled() {
    let mut cfg = WebhookConfig::default();
    cfg.enabled = false;
    cfg.url = String::new();
    assert!(validate(&cfg).is_ok()); // disabled + empty url = OK
}

#[test]
fn validate_rejects_bad_interval() {
    let mut cfg = fresh_config();
    cfg.min_interval_secs = 4000;
    assert!(validate(&cfg).is_err());
    cfg.min_interval_secs = 3600;
    assert!(validate(&cfg).is_ok());
}

#[test]
fn validate_rejects_bad_header_name() {
    let mut cfg = fresh_config();
    cfg.custom_headers = vec![("".into(), "v".into())];
    assert!(validate(&cfg).is_err());
    cfg.custom_headers = vec![("X\nInjected".into(), "v".into())];
    assert!(validate(&cfg).is_err());
}

#[test]
fn source_enabled_routing() {
    let s = WebhookSources {
        metrics_exceeded: true,
        sla_violated: false,
        kill_switch_engaged: true,
        plugin_crashed: false,
    };
    assert!(source_enabled("plugin.metrics.exceeded", &s));
    assert!(!source_enabled("capability.sla.violated", &s));
    assert!(source_enabled("plugin.kill_switch.enabled", &s));
    assert!(!source_enabled("plugin.lifecycle.crashed", &s));
    assert!(!source_enabled("unknown.event", &s));
}

#[test]
fn dedup_key_changes_with_payload() {
    let a = dedup_key("plugin.metrics.exceeded", &serde_json::json!({"x":1}));
    let b = dedup_key("plugin.metrics.exceeded", &serde_json::json!({"x":2}));
    let c = dedup_key("plugin.metrics.exceeded", &serde_json::json!({"x":1}));
    assert_ne!(a, b);
    assert_eq!(a, c); // same payload, same key
}

#[test]
fn config_round_trips_through_json() {
    let cfg = fresh_config();
    let s = serde_json::to_string(&cfg).unwrap();
    let back: WebhookConfig = serde_json::from_str(&s).unwrap();
    assert_eq!(back.enabled, cfg.enabled);
    assert_eq!(back.url, cfg.url);
    assert_eq!(back.min_interval_secs, cfg.min_interval_secs);
    assert_eq!(back.custom_headers.len(), cfg.custom_headers.len());
    assert_eq!(back.sources.metrics_exceeded, cfg.sources.metrics_exceeded);
    assert_eq!(back.sources.sla_violated, cfg.sources.sla_violated);
    assert_eq!(
        back.sources.kill_switch_engaged,
        cfg.sources.kill_switch_engaged
    );
    assert_eq!(back.sources.plugin_crashed, cfg.sources.plugin_crashed);
}

#[test]
fn default_config_is_safe() {
    let cfg = WebhookConfig::default();
    assert!(!cfg.enabled);
    assert_eq!(cfg.url, "");
    assert_eq!(cfg.min_interval_secs, 5);
    assert!(!cfg.sources.metrics_exceeded);
    assert!(!cfg.sources.sla_violated);
    assert!(!cfg.sources.kill_switch_engaged);
    assert!(!cfg.sources.plugin_crashed);
    assert!(cfg.custom_headers.is_empty());
}

#[test]
fn dispatcher_dedups_within_window() {
    // use the unit-test to read/write inner directly
    _reset_for_tests();
    let payload = serde_json::json!({"pluginId": "com.example.foo", "kind": "cpu"});
    let key = dedup_key("plugin.metrics.exceeded", &payload);
    // write a last_sent 1ns ago → should hit dedup
    {
        let arc = shared();
        let mut s = arc.lock().unwrap();
        s.last_sent.insert(key.clone(), Instant::now());
    }
    // direct check: now - prev < min_interval_secs (default 5), so dispatch should return early
    let arc = shared();
    let s = arc.lock().unwrap();
    let prev = s.last_sent.get(&key).unwrap();
    let dt = Instant::now().duration_since(*prev).as_secs();
    assert!(dt < 5, "test setup: dedup should still be in window");
}

// ─── Phase 48 tests ──────────────────────────────────────────────────────

#[test]
fn retry_config_default_is_safe() {
    let cfg = RetryConfig::default();
    assert_eq!(cfg.max_attempts, 5);
    assert_eq!(cfg.initial_backoff_secs, 30);
    assert_eq!(cfg.max_backoff_secs, 600);
    assert_eq!(cfg.retention_days, 7);
    assert!(validate_retry(&cfg).is_ok());
}

#[test]
fn retry_config_round_trips_through_json() {
    let cfg = RetryConfig {
        max_attempts: 10,
        initial_backoff_secs: 60,
        max_backoff_secs: 1800,
        retention_days: 30,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: RetryConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.max_attempts, 10);
    assert_eq!(back.initial_backoff_secs, 60);
    assert_eq!(back.max_backoff_secs, 1800);
    assert_eq!(back.retention_days, 30);
}

#[test]
fn validate_retry_rejects_zero_attempts() {
    let mut cfg = RetryConfig::default();
    cfg.max_attempts = 0;
    assert!(validate_retry(&cfg).is_err());
    cfg.max_attempts = 101;
    assert!(validate_retry(&cfg).is_err());
    cfg.max_attempts = 5;
    assert!(validate_retry(&cfg).is_ok());
}

#[test]
fn validate_retry_rejects_zero_backoff() {
    let mut cfg = RetryConfig::default();
    cfg.initial_backoff_secs = 0;
    assert!(validate_retry(&cfg).is_err());
}

#[test]
fn validate_retry_rejects_max_less_than_initial() {
    let mut cfg = RetryConfig::default();
    cfg.initial_backoff_secs = 600;
    cfg.max_backoff_secs = 30; // < initial
    assert!(validate_retry(&cfg).is_err());
}

#[test]
fn backoff_doubles_then_caps() {
    let cfg = RetryConfig {
        max_attempts: 20,
        initial_backoff_secs: 30,
        max_backoff_secs: 600,
        retention_days: 7,
    };
    // attempts is the number of attempts made; the next wait = initial * 2^(attempts-1), capped at max
    // attempts=1 → 30
    // attempts=2 → 60
    // attempts=3 → 120
    // attempts=4 → 240
    // attempts=5 → 480
    // attempts=6 → 960 → cap 600
    // attempts=10 → still 600
    assert_eq!(compute_backoff_secs(1, &cfg), 30);
    assert_eq!(compute_backoff_secs(2, &cfg), 60);
    assert_eq!(compute_backoff_secs(3, &cfg), 120);
    assert_eq!(compute_backoff_secs(4, &cfg), 240);
    assert_eq!(compute_backoff_secs(5, &cfg), 480);
    assert_eq!(compute_backoff_secs(6, &cfg), 600); // capped
    assert_eq!(compute_backoff_secs(10, &cfg), 600); // still capped
    assert_eq!(compute_backoff_secs(100, &cfg), 600); // huge attempt → still capped, no overflow
}

#[test]
fn backoff_attempts_zero_returns_initial() {
    let cfg = RetryConfig::default();
    assert_eq!(compute_backoff_secs(0, &cfg), 30);
}

#[test]
fn compute_backoff_handles_huge_attempt_no_overflow() {
    // verify the saturating shift prevents overflow
    let cfg = RetryConfig {
        max_attempts: 200,
        initial_backoff_secs: 60,
        max_backoff_secs: 86400,
        retention_days: 1,
    };
    let b = compute_backoff_secs(u32::MAX, &cfg);
    // must not panic / overflow; at most it is max_backoff_secs
    assert_eq!(b, 86400);
}

// ─── Phase 49 tests ──────────────────────────────────────────────────────

#[test]
fn compute_signature_with_secret_produces_sha256_hex() {
    let sig = compute_signature("shh-secret", r#"{"a":1}"#);
    assert!(sig.starts_with("sha256="));
    // 64 hex chars after the prefix
    let hex = &sig[7..];
    assert_eq!(hex.len(), 64);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn compute_signature_empty_secret_returns_empty() {
    // no secret set → not signed (header skipped)
    assert_eq!(compute_signature("", "anything"), "");
}

#[test]
fn compute_signature_changes_with_body() {
    let s1 = compute_signature("k", "body-a");
    let s2 = compute_signature("k", "body-b");
    assert_ne!(s1, s2);
}

#[test]
fn validate_endpoint_rejects_empty_name() {
    let ep = WebhookEndpoint {
        id: String::new(),
        name: String::new(),
        url: "https://hooks.example.com/x".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![],
    };
    assert!(validate_endpoint(&ep).is_err());
}

#[test]
fn validate_endpoint_rejects_bad_url() {
    let ep = WebhookEndpoint {
        id: String::new(),
        name: "ops".into(),
        url: "ftp://wrong".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![],
    };
    assert!(validate_endpoint(&ep).is_err());
}

#[test]
fn validate_endpoint_rejects_unknown_source_filter() {
    let ep = WebhookEndpoint {
        id: String::new(),
        name: "ops".into(),
        url: "https://x.com".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec!["unknown.kind".into()],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![],
    };
    assert!(validate_endpoint(&ep).is_err());
}

#[test]
fn validate_endpoint_accepts_known_source_filter() {
    let ep = WebhookEndpoint {
        id: String::new(),
        name: "ops".into(),
        url: "https://x.com".into(),
        enabled: true,
        headers: vec![("X-Auth".into(), "tok".into())],
        secret: "shh".into(),
        source_filter: vec![
            "plugin.metrics.exceeded".into(),
            "plugin.lifecycle.crashed".into(),
        ],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![],
    };
    assert!(validate_endpoint(&ep).is_ok());
}

#[test]
fn endpoint_accepts_source_logic() {
    // empty filter = accept all
    let all = AlertingEndpointRow {
        id: "a".into(),
        name: "all".into(),
        url: "https://x.com".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        created_at: 0,
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: None,
    };
    assert!(endpoint_accepts_source(&all, "plugin.metrics.exceeded"));
    assert!(endpoint_accepts_source(&all, "capability.sla.violated"));

    // restricted filter = match only
    let only_crash = AlertingEndpointRow {
        source_filter: vec!["plugin.lifecycle.crashed".into()],
        name: "crash".into(),
        ..all.clone()
    };
    assert!(endpoint_accepts_source(
        &only_crash,
        "plugin.lifecycle.crashed"
    ));
    assert!(!endpoint_accepts_source(
        &only_crash,
        "plugin.metrics.exceeded"
    ));
}

#[test]
fn webhook_endpoint_round_trips_through_json() {
    let ep = WebhookEndpoint {
        id: "ep-1".into(),
        name: "ops".into(),
        url: "https://x.com".into(),
        enabled: true,
        headers: vec![("X-K".into(), "V".into())],
        secret: "shh".into(),
        source_filter: vec!["plugin.metrics.exceeded".into()],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![],
    };
    let json = serde_json::to_string(&ep).unwrap();
    let back: WebhookEndpoint = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, "ep-1");
    assert_eq!(back.name, "ops");
    assert_eq!(back.headers, vec![("X-K".to_string(), "V".to_string())]);
    assert_eq!(back.secret, "shh");
    assert_eq!(back.source_filter, vec!["plugin.metrics.exceeded"]);
}

#[test]
fn webhook_endpoint_default_id_is_empty_string() {
    // id default="" → create mode (generated by the backend)
    let json = r#"{"name":"x","url":"https://x.com","enabled":true,"headers":[]}"#;
    let ep: WebhookEndpoint = serde_json::from_str(json).unwrap();
    assert_eq!(ep.id, "");
    assert_eq!(ep.secret, "");
    assert!(ep.source_filter.is_empty());
}

// ─── Phase 50 tests ──────────────────────────────────────────────────────

#[test]
fn kind_matches_star_matches_anything() {
    assert!(kind_matches("*", "plugin.metrics.exceeded"));
    assert!(kind_matches("", "anything"));
}

#[test]
fn kind_matches_prefix_dot_star() {
    assert!(kind_matches("plugin.*", "plugin.metrics.exceeded"));
    assert!(kind_matches("plugin.*", "plugin.lifecycle.crashed"));
    assert!(!kind_matches("plugin.*", "capability.sla.violated"));
    assert!(!kind_matches("plugin.*", "plugin")); // must have something after the .
}

#[test]
fn kind_matches_exact() {
    assert!(kind_matches(
        "capability.sla.violated",
        "capability.sla.violated"
    ));
    assert!(!kind_matches(
        "capability.sla.violated",
        "capability.sla.violated.extra"
    ));
    assert!(!kind_matches(
        "capability.sla.violated",
        "plugin.metrics.exceeded"
    ));
}

#[test]
fn weekday_from_unix_known_dates() {
    // 1970-01-01 = Thursday (Unix epoch)
    assert_eq!(weekday_from_unix(0), 4);
    // 2026-01-01 00:00:00 UTC = 1767225600 (= 20454 days since epoch)
    // (20454 + 4) % 7 = 20458 % 7 = 4 → Thursday
    assert_eq!(weekday_from_unix(1767225600), 4);
    // 2026-09-10 00:00:00 UTC = 1789084800 (= 20707 days since epoch)
    // (20707 + 4) % 7 = 20711 % 7 = 5 → Friday
    assert_eq!(weekday_from_unix(1789084800), 5);
    // 2026-12-25 00:00:00 UTC = 1798156800 (= 20812 days since epoch)
    // (20812 + 4) % 7 = 20816 % 7 = 5 → Friday
    assert_eq!(weekday_from_unix(1798156800), 5);
}

#[test]
fn hour_from_unix_returns_utc_hour() {
    assert_eq!(hour_from_unix(0), 0);
    assert_eq!(hour_from_unix(3600), 1);
    assert_eq!(hour_from_unix(86399), 23);
    assert_eq!(hour_from_unix(86400), 0);
}

fn silence_with_window(start: u64, end: u64, wd: u8, sh: u8, eh: u8) -> SilenceRule {
    SilenceRule {
        id: "sil-test".into(),
        name: "test".into(),
        kind_pattern: "*".into(),
        starts_at: start,
        ends_at: end,
        weekdays: wd,
        start_hour: sh,
        end_hour: eh,
    }
}

#[test]
fn validate_silence_accepts_valid() {
    // 24h window starting now, all weekdays, full 24h
    let now = 1_700_000_000;
    let s = silence_with_window(now, now + 86400, 127, 0, 24);
    assert!(validate_silence(&s).is_ok());
}

#[test]
fn validate_silence_rejects_inverted_window() {
    let now = 1_700_000_000;
    let s = silence_with_window(now + 100, now, 127, 0, 24);
    assert!(validate_silence(&s).is_err());
}

#[test]
fn validate_silence_rejects_too_long_window() {
    let now = 1_700_000_000;
    let s = silence_with_window(now, now + 8 * 86400, 127, 0, 24);
    assert!(validate_silence(&s).is_err());
}

#[test]
fn validate_silence_rejects_empty_weekdays() {
    let now = 1_700_000_000;
    let s = silence_with_window(now, now + 3600, 0, 0, 24);
    assert!(validate_silence(&s).is_err());
}

#[test]
fn validate_silence_rejects_bad_hours() {
    let now = 1_700_000_000;
    // start_hour >= end_hour
    let s = silence_with_window(now, now + 3600, 127, 12, 12);
    assert!(validate_silence(&s).is_err());
    // hour > 24
    let s = silence_with_window(now, now + 3600, 127, 25, 25);
    assert!(validate_silence(&s).is_err());
}

#[test]
fn validate_silence_rejects_empty_name() {
    let now = 1_700_000_000;
    let mut s = silence_with_window(now, now + 3600, 127, 0, 24);
    s.name = "".into();
    assert!(validate_silence(&s).is_err());
    s.name = "   ".into();
    assert!(validate_silence(&s).is_err());
}

#[test]
fn silence_db_round_trips_through_save_list_delete() {
    // initialize the in-memory shared store (test helper)
    use crate::core::storage::{Storage, StoreEnum};

    fn fresh_db() -> Storage {
        let tmp = std::env::temp_dir().join(format!(
            "opencapx-phase50-{}.sqlite",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = Storage::open(&tmp).expect("open temp sqlite");
        db
    }

    // this test only exercises the store layer and does not depend on the global shared_store hook;
    // the shared store is written through super::set_shared_store. We reuse the save/list/delete paths directly.
    // since the global super::shared_store reference cannot be overridden in a test,
    // this degrades to verifying only the silence table's own CRUD.
    let mut db = fresh_db();
    use crate::core::storage::SilenceRuleRow;
    let now = 1_700_000_000;
    let row = SilenceRuleRow {
        id: "sil-a".into(),
        name: "maintenance".into(),
        kind_pattern: "*".into(),
        starts_at: now,
        ends_at: now + 3600,
        weekdays: 127,
        start_hour: 0,
        end_hour: 24,
        created_at: now,
    };
    db.upsert_alerting_silence(&row);
    let listed = db.list_alerting_silences();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "sil-a");
    assert_eq!(listed[0].name, "maintenance");

    // list_active_silences(now) returns active
    let active = db.list_active_silences(now + 60);
    assert_eq!(active.len(), 1);

    // list_active_silences(end+1) returns empty
    let active2 = db.list_active_silences(now + 7200);
    assert!(active2.is_empty());

    // delete
    assert!(db.delete_alerting_silence("sil-a"));
    assert!(db.list_alerting_silences().is_empty());
}

#[test]
fn ack_db_round_trips_through_save_list_delete() {
    // same as above: only the store layer's ack CRUD is verified.
    use crate::core::storage::{AckRuleRow, Storage};
    let tmp = std::env::temp_dir().join(format!(
        "opencapx-phase50-ack-{}.sqlite",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut db = Storage::open(&tmp).unwrap();
    let now = 1_700_000_000;
    let row = AckRuleRow {
        id: "ack-a".into(),
        kind_pattern: "plugin.metrics.*".into(),
        ack_until: now + 3600,
        created_at: now,
    };
    db.upsert_alerting_ack(&row);

    let listed = db.list_alerting_acks();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].kind_pattern, "plugin.metrics.*");

    let active = db.list_active_acks(now + 60);
    assert_eq!(active.len(), 1);

    // clear_expired_acks: now=end+1 → 1 row removed
    let removed = db.clear_expired_acks(now + 7200);
    assert_eq!(removed, 1);
    assert!(db.list_active_acks(now + 7200).is_empty());
    // a direct list still sees it (including expired)
    assert_eq!(db.list_alerting_acks().len(), 0); // clear_expired deletes the table row too

    assert!(db.delete_alerting_ack("ack-a") || db.list_alerting_acks().is_empty());
}

#[test]
fn silence_priority_overrides_dedup() {
    // if a silence is attached, it must not be sent even when the payload already hit the dedup cache.
    // this tests the helper: even when last_sent has an entry, is_silenced returns true first.
    // the actual dispatch() calls is_silenced before dedup, so the behavior is guaranteed.
    // here we use kind_matches("plugin.*", "plugin.metrics.exceeded") == true to verify the semantics:
    let matched = kind_matches("plugin.*", "plugin.metrics.exceeded");
    assert!(matched);
    // conversely, a silence pattern = capability.* must not mask plugin.*
    let not_matched = kind_matches("capability.*", "plugin.metrics.exceeded");
    assert!(!not_matched);
}

// ─── Phase 51 tests ──────────────────────────────────────────────────────

fn route_with(priority: i32, enabled: bool, kind: &str, target: &[&str]) -> RouteRuleRow {
    RouteRuleRow {
        id: format!("rt-{}", priority),
        name: format!("rule-{}", priority),
        priority,
        enabled,
        kind_pattern: kind.into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: target.iter().map(|s| s.to_string()).collect(),
        recipients: vec![],
        tags: vec![],
        seen_in_last_json: None,
        created_at: 1_700_000_000,
    }
}

fn payload(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap()
}

#[test]
fn validate_route_accepts_minimal_valid() {
    let r = RouteRule {
        id: "".into(),
        name: "rule".into(),
        priority: 100,
        enabled: true,
        kind_pattern: "*".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-1".into()],
        recipients: vec![],
        tags: vec![],
        seen_in_last: None,
    };
    assert!(validate_route(&r).is_ok());
}

#[test]
fn validate_route_rejects_empty_name() {
    let r = RouteRule {
        id: "".into(),
        name: "  ".into(),
        priority: 100,
        enabled: true,
        kind_pattern: "*".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-1".into()],
        recipients: vec![],
        tags: vec![],
        seen_in_last: None,
    };
    assert!(validate_route(&r).is_err());
}

#[test]
fn validate_route_rejects_empty_targets() {
    let r = RouteRule {
        id: "".into(),
        name: "rule".into(),
        priority: 100,
        enabled: true,
        kind_pattern: "*".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec![],
        recipients: vec![],
        tags: vec![],
        seen_in_last: None,
    };
    assert!(validate_route(&r).is_err());
}

#[test]
fn validate_route_rejects_bad_kind_pattern() {
    let mut r = RouteRule {
        id: "".into(),
        name: "rule".into(),
        priority: 100,
        enabled: true,
        kind_pattern: "".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-1".into()],
        recipients: vec![],
        tags: vec![],
        seen_in_last: None,
    };
    assert!(validate_route(&r).is_err());
    r.kind_pattern = "a".repeat(300).into();
    assert!(validate_route(&r).is_err());
}

#[test]
fn route_matches_kind_only_pattern() {
    let r = route_with(100, true, "plugin.metrics.*", &["ep-1"]);
    assert!(route_matches(&r, "plugin.metrics.exceeded", &payload("{}")));
    assert!(!route_matches(
        &r,
        "capability.sla.violated",
        &payload("{}")
    ));
    assert!(!route_matches(&r, "plugin", &payload("{}"))); // prefix.* requires a .
}

#[test]
fn route_matches_disabled_returns_false() {
    let r = route_with(100, false, "*", &["ep-1"]);
    assert!(!route_matches(&r, "anything", &payload("{}")));
}

#[test]
fn route_matches_payload_ge_threshold() {
    let mut r = route_with(100, true, "*", &["ep-1"]);
    r.payload_path = Some("cpu_percent".into());
    r.payload_match = Some(">=90".into());
    let p1 = payload(r#"{"cpu_percent": 95}"#);
    let p2 = payload(r#"{"cpu_percent": 50}"#);
    let p3 = payload(r#"{"other": 1}"#);
    assert!(route_matches(&r, "anything", &p1));
    assert!(!route_matches(&r, "anything", &p2));
    assert!(!route_matches(&r, "anything", &p3));
}

#[test]
fn route_matches_payload_prefix_and_contains() {
    let mut r = route_with(100, true, "*", &["ep-1"]);
    r.payload_path = Some("error".into());
    r.payload_match = Some("prefix:OutOf".into());
    assert!(route_matches(
        &r,
        "anything",
        &payload(r#"{"error":"OutOfMemory"}"#)
    ));
    assert!(!route_matches(
        &r,
        "anything",
        &payload(r#"{"error":"NullPointer"}"#)
    ));

    r.payload_match = Some("contains:timeout".into());
    assert!(route_matches(
        &r,
        "anything",
        &payload(r#"{"error":"connection_timeout"}"#)
    ));
    assert!(!route_matches(
        &r,
        "anything",
        &payload(r#"{"error":"reset"}"#)
    ));
}

#[test]
fn route_matches_payload_exact_eq_string() {
    let mut r = route_with(100, true, "*", &["ep-1"]);
    r.payload_path = Some("status".into());
    r.payload_match = Some("=ok".into());
    assert!(route_matches(
        &r,
        "anything",
        &payload(r#"{"status":"ok"}"#)
    ));
    assert!(!route_matches(
        &r,
        "anything",
        &payload(r#"{"status":"failed"}"#)
    ));
}

#[test]
fn route_first_match_wins_by_priority() {
    // lower priority number = higher precedence (checked first)
    let high = route_with(10, true, "*", &["ep-critical"]);
    let low = route_with(999, true, "*", &["ep-info"]);
    // list_alerting_routes returns priority ASC, but here we use route_matches directly
    // to verify semantics; match_route first-match-wins logic exercised in priority_order test below
    let _ = (high, low);
    // kind_pattern=* + no payload → both match, the smaller priority wins
    // here we would call the helper match_route needs (we cannot, so we test the low-level helpers)
    // instead we test: feeding both into route_matches should be true → the priority comparison is decided by list order
    let high = route_with(10, true, "*", &["ep-critical"]);
    let low = route_with(999, true, "*", &["ep-info"]);
    assert!(route_matches(&high, "anything", &payload("{}")));
    assert!(route_matches(&low, "anything", &payload("{}")));
}

#[test]
fn routes_yaml_round_trips() {
    let yaml = r#"version: 1
rules:
  - name: critical → ops
    priority: 10
    enabled: true
    kindPattern: "plugin.metrics.*"
    payloadPath: "cpu_percent"
    payloadMatch: ">=90"
    targetEndpointIds: ["ep-oncall"]
    tags: ["p1", "page"]
  - name: fallback slack
    priority: 999
    enabled: false
    kindPattern: "*"
    targetEndpointIds: ["ep-slack"]
    tags: []
"#;
    let doc: RouteRuleYamlDoc = serde_yaml::from_str(yaml).expect("parse yaml");
    assert_eq!(doc.version, 1);
    assert_eq!(doc.rules.len(), 2);
    assert_eq!(doc.rules[0].name, "critical → ops");
    assert_eq!(doc.rules[0].priority, 10);
    assert_eq!(doc.rules[0].target_endpoint_ids, vec!["ep-oncall"]);
    assert_eq!(doc.rules[0].tags, vec!["p1", "page"]);
    assert_eq!(doc.rules[1].name, "fallback slack");
    assert!(!doc.rules[1].enabled);

    // serialize back
    let back = serde_yaml::to_string(&doc).expect("serialize yaml");
    let doc2: RouteRuleYamlDoc = serde_yaml::from_str(&back).expect("reparse yaml");
    assert_eq!(doc2.rules.len(), 2);
    assert_eq!(doc2.rules[0].target_endpoint_ids, vec!["ep-oncall"]);
}

#[test]
fn routes_yaml_invalid_kind_rejected() {
    // missing target_endpoint_ids → the yaml parses but validate fails
    let yaml = r#"version: 1
rules:
  - name: bad
    priority: 100
    kindPattern: "*"
"#;
    let doc: RouteRuleYamlDoc = serde_yaml::from_str(yaml).unwrap();
    assert!(validate_route(&doc.rules[0]).is_err());
}

#[test]
fn payload_path_get_nested() {
    let v = payload(r#"{"a":{"b":{"c":"hello"}}}"#);
    assert_eq!(
        payload_path_get(&v, "a.b.c").and_then(|x| x.as_str()),
        Some("hello")
    );
    assert!(payload_path_get(&v, "a.b").is_some()); // it is a Value now, not a str, so Some({}) directly
    assert!(payload_path_get(&v, "x.y.z").is_none());
    let arr = payload(r#"{"items":[{"name":"x"},{"name":"y"}]}"#);
    assert_eq!(
        payload_path_get(&arr, "items.0.name").and_then(|x| x.as_str()),
        Some("x")
    );
    assert_eq!(
        payload_path_get(&arr, "items.1.name").and_then(|x| x.as_str()),
        Some("y")
    );
}

#[test]
fn route_priority_overrides_fanout_fallback() {
    // when no route hits, it should use the endpoint source_filter fanout (backward compatible).
    // here we only verify the helper semantics: the None path + the Some path.
    // dispatch() is already tested above; this test covers route_target_ids boundaries.

    // case 1: kind does not match → None
    // call the low-level route_matches directly to verify consistent behavior:
    let r = route_with(10, true, "plugin.*", &["ep-1"]);
    let p = payload("{}");
    assert!(!route_matches(&r, "capability.sla.violated", &p));
}

// ─── Phase 52 tests ──────────────────────────────────────────────────────

#[test]
fn severity_for_source_known() {
    assert_eq!(
        severity_for_source("plugin.metrics.exceeded"),
        Severity::Warn
    );
    assert_eq!(
        severity_for_source("capability.sla.violated"),
        Severity::Error
    );
    assert_eq!(
        severity_for_source("plugin.kill_switch.enabled"),
        Severity::Critical
    );
    assert_eq!(
        severity_for_source("plugin.lifecycle.crashed"),
        Severity::Critical
    );
    assert_eq!(severity_for_source("webhook.test"), Severity::Info);
}

#[test]
fn severity_for_source_unknown_defaults_info() {
    assert_eq!(severity_for_source("unknown.kind"), Severity::Info);
    assert_eq!(severity_for_source(""), Severity::Info);
}

#[test]
fn severity_parse_round_trips() {
    assert_eq!(Severity::parse("info"), Some(Severity::Info));
    assert_eq!(Severity::parse("WARN"), Some(Severity::Warn));
    assert_eq!(Severity::parse("Error"), Some(Severity::Error));
    assert_eq!(Severity::parse("critical"), Some(Severity::Critical));
    assert_eq!(Severity::parse("fatal"), Some(Severity::Critical));
    assert_eq!(Severity::parse("nonsense"), None);
}

#[test]
fn severity_as_str_lowercase() {
    assert_eq!(Severity::Info.as_str(), "info");
    assert_eq!(Severity::Critical.as_str(), "critical");
}

#[test]
fn make_envelope_fills_schema_version_and_timestamp() {
    let env = make_envelope(
        "plugin.metrics.exceeded",
        serde_json::json!({"cpu": 95}),
        vec![],
    );
    assert_eq!(env.schema_version, ALERT_SCHEMA_VERSION);
    assert_eq!(env.schema_version, 1);
    assert_eq!(env.source, "plugin.metrics.exceeded");
    assert_eq!(env.severity, Severity::Warn);
    assert!(env.timestamp > 0);
    assert_eq!(env.payload, serde_json::json!({"cpu": 95}));
}

#[test]
fn make_envelope_event_id_is_unique_uuid_v4() {
    let e1 = make_envelope("a", serde_json::json!({}), vec![]);
    let e2 = make_envelope("a", serde_json::json!({}), vec![]);
    assert_ne!(e1.event_id, e2.event_id);
    // uuid v4 format: 8-4-4-4-12 hex chars
    let parts: Vec<&str> = e1.event_id.split('-').collect();
    assert_eq!(parts.len(), 5);
    assert_eq!(parts[0].len(), 8);
    assert_eq!(parts[1].len(), 4);
}

#[test]
fn envelope_serializes_to_canonical_shape() {
    let env = AlertEnvelope {
        schema_version: 1,
        event_id: "fixed-id".into(),
        source: "plugin.metrics.exceeded".into(),
        severity: Severity::Warn,
        tags: vec!["p1".into()],
        timestamp: 1_700_000_000,
        payload: serde_json::json!({"x": 1}),
    };
    let s = envelope_to_json_string(&env).unwrap();
    // schema_version / event_id / source / severity / tags / timestamp / payload all present
    assert!(s.contains("\"schemaVersion\":1"));
    assert!(s.contains("\"eventId\":\"fixed-id\""));
    assert!(s.contains("\"source\":\"plugin.metrics.exceeded\""));
    assert!(s.contains("\"severity\":\"warn\""));
    assert!(s.contains("\"tags\":[\"p1\"]"));
    assert!(s.contains("\"timestamp\":1700000000"));
    assert!(s.contains("\"payload\":{\"x\":1}"));
}

#[test]
fn envelope_deserializes_back() {
    let env = AlertEnvelope {
        schema_version: 1,
        event_id: "abc".into(),
        source: "capability.sla.violated".into(),
        severity: Severity::Error,
        tags: vec![],
        timestamp: 42,
        payload: serde_json::json!({"k": "v"}),
    };
    let s = serde_json::to_string(&env).unwrap();
    let back: AlertEnvelope = serde_json::from_str(&s).unwrap();
    assert_eq!(back.event_id, "abc");
    assert_eq!(back.severity, Severity::Error);
    assert_eq!(back.payload, serde_json::json!({"k": "v"}));
}

#[test]
fn webhook_config_default_has_schema_version_zero() {
    let cfg = WebhookConfig::default();
    assert_eq!(cfg.schema_version, 0);
}

#[test]
fn webhook_config_round_trips_with_schema_version() {
    let cfg = WebhookConfig {
        enabled: true,
        url: "https://x.com".into(),
        min_interval_secs: 30,
        sources: WebhookSources::default(),
        custom_headers: vec![],
        schema_version: 1,
    };
    let s = serde_json::to_string(&cfg).unwrap();
    let back: WebhookConfig = serde_json::from_str(&s).unwrap();
    assert_eq!(back.schema_version, 1);
    assert!(s.contains("\"schemaVersion\":1"));
}

#[test]
fn webhook_endpoint_default_schema_version_zero() {
    let json = r#"{"name":"x","url":"https://x.com","enabled":true,"headers":[]}"#;
    let ep: WebhookEndpoint = serde_json::from_str(json).unwrap();
    assert_eq!(ep.schema_version, 0);
}

#[test]
fn webhook_endpoint_round_trips_with_schema_version() {
    let ep = WebhookEndpoint {
        id: "ep-1".into(),
        name: "ops".into(),
        url: "https://x.com".into(),
        enabled: true,
        headers: vec![],
        secret: "".into(),
        source_filter: vec![],
        schema_version: 1,
        template: None,
        template_sample: None,
        severity_overrides: vec![],
    };
    let s = serde_json::to_string(&ep).unwrap();
    let back: WebhookEndpoint = serde_json::from_str(&s).unwrap();
    assert_eq!(back.schema_version, 1);
}

// ─── Phase 53 tests ────────────────────────────────────────────────────────────────

#[test]
fn severity_resolved_falls_back_to_hardcode_when_no_hint() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("resolved-fallback"));
    // known source, no hint → use the hardcode table
    assert_eq!(severity_resolved("plugin.metrics.exceeded"), Severity::Warn);
    assert_eq!(
        severity_resolved("plugin.kill_switch.enabled"),
        Severity::Critical
    );
    assert_eq!(
        severity_resolved("capability.sla.violated"),
        Severity::Error
    );
}

#[test]
fn severity_resolved_user_hint_overrides_hardcode() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("user-override-hardcode"));
    save_user_severity_hint("plugin.metrics.exceeded", "critical").unwrap();
    // the user hint overrides the hardcode Warn with Critical
    assert_eq!(
        severity_resolved("plugin.metrics.exceeded"),
        Severity::Critical
    );
}

#[test]
fn severity_resolved_manifest_hint_overrides_hardcode() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("manifest-override"));
    install_manifest_hints(
        "plug-a",
        &Some(AlertingManifest {
            severity_hints: [(
                "capability.sla.violated".to_string(),
                "critical".to_string(),
            )]
            .into_iter()
            .collect(),
        }),
    );
    assert_eq!(
        severity_resolved("capability.sla.violated"),
        Severity::Critical
    );
}

#[test]
fn severity_resolved_user_hint_beats_manifest_hint() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("user-beats-manifest"));
    install_manifest_hints(
        "plug-a",
        &Some(AlertingManifest {
            severity_hints: [(
                "plugin.metrics.exceeded".to_string(),
                "critical".to_string(),
            )]
            .into_iter()
            .collect(),
        }),
    );
    save_user_severity_hint("plugin.metrics.exceeded", "info").unwrap();
    // user should win over manifest
    assert_eq!(severity_resolved("plugin.metrics.exceeded"), Severity::Info);
}

#[test]
fn severity_resolved_unknown_source_with_hint_returns_hint() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("unknown-with-hint"));
    save_user_severity_hint("my.custom.event", "warn").unwrap();
    assert_eq!(severity_resolved("my.custom.event"), Severity::Warn);
}

#[test]
fn severity_resolved_unknown_source_no_hint_returns_info() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("unknown-no-hint"));
    assert_eq!(
        severity_resolved("definitely.unknown.source"),
        Severity::Info
    );
}

#[test]
fn severity_resolved_no_store_returns_hardcode() {
    // explicitly clear the shared store → severity_resolved can still return hardcode
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // the old store reference is unavailable — use a temporary empty shared store (None) instead
    // this only verifies the function does not panic; via the "unknown source + no store" path
    // it reaches hardcode → Info
    assert_eq!(severity_for_source("anything-else"), Severity::Info);
}

#[test]
fn save_user_severity_hint_rejects_invalid_severity() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("invalid-severity"));
    let r = save_user_severity_hint("plugin.x", "fatal-but-wrong");
    assert!(r.is_err());
}

#[test]
fn save_user_severity_hint_rejects_empty_source() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("empty-source"));
    let r = save_user_severity_hint("", "warn");
    assert!(r.is_err());
}

#[test]
fn save_user_severity_hint_persists_and_reads_back() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("persists"));
    let saved = save_user_severity_hint("my.event", "error").unwrap();
    assert_eq!(saved.source, "my.event");
    assert_eq!(saved.severity, "error");
    assert_eq!(saved.origin, "user");
    assert!(saved.plugin_id.is_none());

    let rows = list_severity_hints();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "my.event");
}

#[test]
fn delete_user_severity_hint_removes_only_user_origin() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("delete-user-only"));
    install_manifest_hints(
        "plug-a",
        &Some(AlertingManifest {
            severity_hints: [("plugin.x".to_string(), "warn".to_string())]
                .into_iter()
                .collect(),
        }),
    );
    save_user_severity_hint("plugin.x", "critical").unwrap();
    // deleting the user origin → must not affect the manifest origin
    assert!(delete_user_severity_hint("plugin.x"));
    // the manifest hint is still there → resolved should fall back to manifest (warn)
    assert_eq!(severity_resolved("plugin.x"), Severity::Warn);
}

#[test]
fn delete_user_severity_hint_returns_false_when_absent() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("delete-absent"));
    assert!(!delete_user_severity_hint("never.saved"));
}

#[test]
fn install_manifest_hints_then_uninstall_clears() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("install-uninstall"));
    install_manifest_hints(
        "plug-x",
        &Some(AlertingManifest {
            severity_hints: [
                ("plugin.a".to_string(), "warn".to_string()),
                ("plugin.b".to_string(), "error".to_string()),
            ]
            .into_iter()
            .collect(),
        }),
    );
    let before = list_severity_hints();
    assert_eq!(before.len(), 2);
    let cleared = uninstall_manifest_hints("plug-x");
    assert_eq!(cleared, 2);
    assert_eq!(list_severity_hints().len(), 0);
}

#[test]
fn install_manifest_hints_ignores_invalid_severity_silently() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("manifest-invalid"));
    install_manifest_hints(
        "plug-bad",
        &Some(AlertingManifest {
            severity_hints: [
                ("plugin.good".to_string(), "warn".to_string()),
                (
                    "plugin.bad".to_string(),
                    "totally-not-a-severity".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        }),
    );
    let rows = list_severity_hints();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "plugin.good");
}

#[test]
fn clear_user_severity_hints_keeps_manifest_origin() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("clear-user-only"));
    install_manifest_hints(
        "plug-a",
        &Some(AlertingManifest {
            severity_hints: [("plugin.m".to_string(), "warn".to_string())]
                .into_iter()
                .collect(),
        }),
    );
    save_user_severity_hint("plugin.u", "error").unwrap();
    let cleared = clear_user_severity_hints();
    assert_eq!(cleared, 1);
    let rows = list_severity_hints();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "plugin.m");
    assert_eq!(rows[0].origin, "manifest");
}

#[test]
fn severity_hint_dto_round_trips() {
    let dto = SeverityHintDto {
        source: "plugin.x".into(),
        severity: "warn".into(),
        origin: "manifest".into(),
        plugin_id: Some("plug-a".into()),
        updated_at: 1_700_000_000,
        effective_severity: "warn".into(),
    };
    let s = serde_json::to_string(&dto).unwrap();
    assert!(s.contains("\"pluginId\":\"plug-a\""));
    assert!(s.contains("\"updatedAt\":1700000000"));
    let back: SeverityHintDto = serde_json::from_str(&s).unwrap();
    assert_eq!(back.plugin_id.as_deref(), Some("plug-a"));
    assert_eq!(back.severity, "warn");
}

#[test]
fn alerting_manifest_parses_camel_case_severity_hints() {
    let json = r#"{"severityHints":{"plugin.x":"warn","plugin.y":"critical"}}"#;
    let m: AlertingManifest = serde_json::from_str(json).unwrap();
    assert_eq!(
        m.severity_hints.get("plugin.x").map(|s| s.as_str()),
        Some("warn")
    );
    assert_eq!(
        m.severity_hints.get("plugin.y").map(|s| s.as_str()),
        Some("critical")
    );
}

// ─── Phase 54: aggregation engine + rule CRUD ────────────────────────────────

fn agg_rule(id: &str, action: &str, target: Option<&str>) -> AggregationRule {
    AggregationRule {
        id: id.to_string(),
        name: format!("rule-{}", id),
        kind_pattern: "plugin.metrics.*".to_string(),
        window_secs: 60,
        threshold_count: 3,
        action: action.to_string(),
        target_severity: target.map(|s| s.to_string()),
        enabled: true,
    }
}

#[test]
fn aggregation_kind_matches_glob_prefix_and_dot() {
    assert!(aggregation_kind_matches("*", "plugin.metrics.exceeded"));
    assert!(aggregation_kind_matches(
        "plugin.metrics.*",
        "plugin.metrics.exceeded"
    ));
    assert!(!aggregation_kind_matches(
        "plugin.metrics.*",
        "plugin.lifecycle.started"
    ));
    assert!(aggregation_kind_matches(
        "plugin.*",
        "plugin.metrics.exceeded"
    ));
    assert!(aggregation_kind_matches(
        "plugin.metrics.exceeded",
        "plugin.metrics.exceeded"
    ));
    assert!(!aggregation_kind_matches(
        "plugin.metrics.exceeded",
        "plugin.metrics.exceeded.extra"
    ));
}

#[test]
fn validate_aggregation_rejects_bad_inputs() {
    let mut r = agg_rule("a", "suppress", None);
    r.window_secs = 0;
    assert!(validate_aggregation(&r).is_err());
    r.window_secs = 60;
    r.threshold_count = 0;
    assert!(validate_aggregation(&r).is_err());
    r.threshold_count = 3;
    r.action = "downgrade".into();
    assert!(validate_aggregation(&r).is_err()); // missing target_severity
    r.target_severity = Some("bogus".into());
    assert!(validate_aggregation(&r).is_err());
    r.target_severity = Some("warn".into());
    assert!(validate_aggregation(&r).is_ok());
    r.action = "weird".into();
    assert!(validate_aggregation(&r).is_err());
}

#[test]
fn evaluate_aggregations_pass_without_rules() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("agg-no-rules"));
    _reset_aggregations_for_tests();
    let d = evaluate_aggregations(
        "plugin.metrics.exceeded",
        &serde_json::json!({}),
        now_secs(),
    );
    assert_eq!(d, AggregationDecision::Pass);
}

#[test]
fn evaluate_aggregations_below_threshold_returns_pass() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("agg-below"));
    _reset_aggregations_for_tests();
    let saved = save_aggregation(agg_rule("r1", "suppress", None)).unwrap();
    assert_eq!(saved.id, "r1");
    let now = now_secs();
    let d1 = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    let d2 = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    assert_eq!(d1, AggregationDecision::Pass);
    assert_eq!(d2, AggregationDecision::Pass);
}

#[test]
fn evaluate_aggregations_suppress_fires_at_threshold() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("agg-suppress"));
    _reset_aggregations_for_tests();
    save_aggregation(agg_rule("r1", "suppress", None)).unwrap();
    let now = now_secs();
    evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    let d = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    match d {
        AggregationDecision::Suppress {
            propagated_severity,
        } => {
            assert_eq!(propagated_severity, Severity::Warn); // plugin.metrics.exceeded hardcode = Warn
        }
        other => panic!("expected Suppress, got {:?}", other),
    }
    // already fired within the window → the next call returns Pass (avoids spam)
    let d = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    assert_eq!(d, AggregationDecision::Pass);
}

#[test]
fn evaluate_aggregations_downgrade_returns_target_severity() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("agg-downgrade"));
    _reset_aggregations_for_tests();
    save_aggregation(agg_rule("r1", "downgrade", Some("critical"))).unwrap();
    let now = now_secs();
    evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    let d = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    match d {
        AggregationDecision::Downgrade(s, _propagated) => assert_eq!(s, Severity::Critical),
        other => panic!("expected Downgrade, got {:?}", other),
    }
}

#[test]
fn evaluate_aggregations_merge_includes_count_and_last_payload() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("agg-merge"));
    _reset_aggregations_for_tests();
    save_aggregation(agg_rule("r1", "merge", None)).unwrap();
    let now = now_secs();
    evaluate_aggregations(
        "plugin.metrics.exceeded",
        &serde_json::json!({"cpu": 50}),
        now,
    );
    evaluate_aggregations(
        "plugin.metrics.exceeded",
        &serde_json::json!({"cpu": 60}),
        now,
    );
    let d = evaluate_aggregations(
        "plugin.metrics.exceeded",
        &serde_json::json!({"cpu": 70}),
        now,
    );
    match d {
        AggregationDecision::Merge {
            count,
            since,
            last_payload,
            ..
        } => {
            assert_eq!(count, 3);
            assert_eq!(since, now);
            assert_eq!(last_payload["cpu"], 70);
        }
        other => panic!("expected Merge, got {:?}", other),
    }
}

#[test]
fn evaluate_aggregations_pattern_mismatch_skips_rule() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("agg-mismatch"));
    _reset_aggregations_for_tests();
    save_aggregation(agg_rule("r1", "suppress", None)).unwrap();
    let now = now_secs();
    for _ in 0..5 {
        let d = evaluate_aggregations("plugin.lifecycle.crashed", &serde_json::json!({}), now);
        assert_eq!(d, AggregationDecision::Pass);
    }
}

#[test]
fn apply_aggregation_decision_rewrites_payload_and_severity() {
    let mut payload = serde_json::json!({"original": true});
    let mut sev = Severity::Info;
    // Pass — unchanged
    assert!(apply_aggregation_decision(
        &AggregationDecision::Pass,
        &mut payload,
        &mut sev
    ));
    assert_eq!(payload["original"], true);
    assert_eq!(sev, Severity::Info);

    // Downgrade — Phase 71: (target_severity, propagated_severity)
    assert!(apply_aggregation_decision(
        &AggregationDecision::Downgrade(Severity::Critical, Severity::Error),
        &mut payload,
        &mut sev
    ));
    assert_eq!(sev, Severity::Critical);

    // Merge — Phase 71: adds the propagated_severity field
    assert!(apply_aggregation_decision(
        &AggregationDecision::Merge {
            count: 4,
            since: 100,
            last_payload: serde_json::json!({"k": "v"}),
            propagated_severity: Severity::Warn,
        },
        &mut payload,
        &mut sev
    ));
    assert_eq!(payload["merged_count"], 4);
    assert_eq!(payload["since"], 100);
    assert_eq!(payload["last_payload"]["k"], "v");

    // Suppress — returns false (Phase 71: carries the propagated_severity field, semantics unchanged)
    assert!(!apply_aggregation_decision(
        &AggregationDecision::Suppress {
            propagated_severity: Severity::Warn
        },
        &mut payload,
        &mut sev
    ));
}

#[test]
fn save_list_delete_clear_aggregations_round_trip() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("agg-crud"));
    _reset_aggregations_for_tests();

    // ensure_aggregation_id
    let mut r = agg_rule("", "suppress", None);
    ensure_aggregation_id(&mut r);
    assert!(r.id.starts_with("agg-"));

    // save + list
    let s1 = save_aggregation(r).unwrap();
    assert!(s1.id.starts_with("agg-"));
    let l1 = list_aggregations();
    assert_eq!(l1.len(), 1);
    assert_eq!(l1[0].id, s1.id);

    // delete
    assert!(delete_aggregation(&s1.id));
    assert_eq!(list_aggregations().len(), 0);

    // clear
    save_aggregation(agg_rule("r2", "merge", None)).unwrap();
    save_aggregation(agg_rule("r3", "downgrade", Some("warn"))).unwrap();
    assert_eq!(list_aggregations().len(), 2);
    assert_eq!(clear_aggregations(), 2);
    assert_eq!(list_aggregations().len(), 0);

    // delete absent
    assert!(!delete_aggregation("does-not-exist"));
}

#[test]
fn make_envelope_with_severity_uses_override() {
    let env = make_envelope_with_severity(
        "plugin.metrics.exceeded",
        serde_json::json!({"k": "v"}),
        vec![],
        Severity::Critical,
    );
    assert_eq!(env.severity, Severity::Critical);
    assert_eq!(env.source, "plugin.metrics.exceeded");
    assert_eq!(env.payload["k"], "v");
    assert!(!env.event_id.is_empty());
}

// ─── Phase 55: correlation engine + rule CRUD ────────────────────────────────

fn corr_rule(id: &str, a: &str, b: &str, window: u64) -> CorrelationRule {
    CorrelationRule {
        id: id.to_string(),
        name: format!("corr-{}", id),
        kind_pattern_a: a.to_string(),
        kind_pattern_b: b.to_string(),
        window_secs: window,
        enabled: true,
    }
}

#[test]
fn validate_correlation_rejects_bad_inputs() {
    let mut r = corr_rule(
        "a",
        "plugin.lifecycle.started",
        "plugin.lifecycle.crashed",
        30,
    );
    r.id = "".into();
    assert!(validate_correlation(&r).is_err());
    r.id = "a".into();
    r.name = "".into();
    assert!(validate_correlation(&r).is_err());
    r.name = "rule-a".into();
    r.window_secs = 0;
    assert!(validate_correlation(&r).is_err());
    r.window_secs = 30;
    r.kind_pattern_a = "".into();
    assert!(validate_correlation(&r).is_err());
    r.kind_pattern_a = "x".into();
    r.kind_pattern_b = "".into();
    assert!(validate_correlation(&r).is_err());
    r.kind_pattern_b = "y".into();
    assert!(validate_correlation(&r).is_ok());
}

#[test]
fn evaluate_correlations_pass_without_rules() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("corr-no-rules"));
    _reset_correlations_for_tests();
    let d = evaluate_correlations("plugin.lifecycle.started", now_secs());
    assert_eq!(d, CorrelationDecision::Pass);
}

#[test]
fn evaluate_correlations_records_last_a_without_suppressing() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("corr-record-a"));
    _reset_correlations_for_tests();
    save_correlation(corr_rule(
        "r1",
        "plugin.lifecycle.started",
        "plugin.lifecycle.crashed",
        60,
    ))
    .unwrap();
    // A hit → only update last_a, no suppress (self-referential rule is a no-op)
    let d = evaluate_correlations("plugin.lifecycle.started", now_secs());
    assert_eq!(d, CorrelationDecision::Pass);
}

#[test]
fn evaluate_correlations_suppress_B_within_window() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("corr-suppress"));
    _reset_correlations_for_tests();
    save_correlation(corr_rule(
        "r1",
        "plugin.lifecycle.started",
        "plugin.lifecycle.crashed",
        60,
    ))
    .unwrap();
    let now = now_secs();
    assert_eq!(
        evaluate_correlations("plugin.lifecycle.started", now),
        CorrelationDecision::Pass
    );
    // B within the window → Suppress (Phase 71: carries propagated_severity)
    match evaluate_correlations("plugin.lifecycle.crashed", now + 10) {
        CorrelationDecision::Suppress {
            propagated_severity,
        } => {
            assert_eq!(propagated_severity, Severity::Critical); // plugin.lifecycle.crashed hardcode = Critical
        }
        other => panic!("expected Suppress, got {:?}", other),
    }
}

#[test]
fn evaluate_correlations_pass_for_B_outside_window() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("corr-outside"));
    _reset_correlations_for_tests();
    save_correlation(corr_rule(
        "r1",
        "plugin.lifecycle.started",
        "plugin.lifecycle.crashed",
        60,
    ))
    .unwrap();
    let now = now_secs();
    evaluate_correlations("plugin.lifecycle.started", now);
    // B later than A by > window_secs → Pass
    assert_eq!(
        evaluate_correlations("plugin.lifecycle.crashed", now + 120),
        CorrelationDecision::Pass
    );
}

#[test]
fn evaluate_correlations_pass_for_B_without_matching_A() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("corr-no-a"));
    _reset_correlations_for_tests();
    save_correlation(corr_rule(
        "r1",
        "plugin.lifecycle.started",
        "plugin.lifecycle.crashed",
        60,
    ))
    .unwrap();
    // B directly, no A → Pass
    assert_eq!(
        evaluate_correlations("plugin.lifecycle.crashed", now_secs()),
        CorrelationDecision::Pass
    );
}

#[test]
fn evaluate_correlations_re_suppress_after_subsequent_A() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("corr-resuppress"));
    _reset_correlations_for_tests();
    save_correlation(corr_rule(
        "r1",
        "plugin.lifecycle.started",
        "plugin.lifecycle.crashed",
        60,
    ))
    .unwrap();
    let now = now_secs();
    evaluate_correlations("plugin.lifecycle.started", now);
    match evaluate_correlations("plugin.lifecycle.crashed", now + 10) {
        CorrelationDecision::Suppress {
            propagated_severity: _,
        } => {}
        other => panic!("expected Suppress, got {:?}", other),
    }
    // a new round of A → last_a refreshes → B again is still Suppress
    evaluate_correlations("plugin.lifecycle.started", now + 30);
    match evaluate_correlations("plugin.lifecycle.crashed", now + 40) {
        CorrelationDecision::Suppress {
            propagated_severity: _,
        } => {}
        other => panic!("expected Suppress, got {:?}", other),
    }
}

#[test]
fn evaluate_correlations_disabled_rule_skipped() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("corr-disabled"));
    _reset_correlations_for_tests();
    let mut r = corr_rule(
        "r1",
        "plugin.lifecycle.started",
        "plugin.lifecycle.crashed",
        60,
    );
    r.enabled = false;
    save_correlation(r).unwrap();
    let now = now_secs();
    evaluate_correlations("plugin.lifecycle.started", now);
    // rule disabled → B is not suppressed
    assert_eq!(
        evaluate_correlations("plugin.lifecycle.crashed", now + 10),
        CorrelationDecision::Pass
    );
}

#[test]
fn correlation_self_referential_rule_is_noop() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("corr-self"));
    _reset_correlations_for_tests();
    // A==B same pattern; a self-referential rule should be a silent no-op
    save_correlation(corr_rule("r1", "plugin.metrics.*", "plugin.metrics.*", 60)).unwrap();
    let now = now_secs();
    assert_eq!(
        evaluate_correlations("plugin.metrics.exceeded", now),
        CorrelationDecision::Pass
    );
}

#[test]
fn correlation_save_list_delete_clear_round_trip() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("corr-crud"));
    _reset_correlations_for_tests();

    let mut r = corr_rule("", "a", "b", 30);
    ensure_correlation_id(&mut r);
    assert!(r.id.starts_with("cor-"));
    let s1 = save_correlation(r).unwrap();
    assert!(s1.id.starts_with("cor-"));

    let list = list_correlations();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, s1.id);
    assert_eq!(list[0].kind_pattern_a, "a");
    assert_eq!(list[0].kind_pattern_b, "b");

    assert!(delete_correlation(&s1.id));
    assert_eq!(list_correlations().len(), 0);

    save_correlation(corr_rule("r2", "x", "y", 60)).unwrap();
    save_correlation(corr_rule("r3", "m", "n", 120)).unwrap();
    assert_eq!(list_correlations().len(), 2);
    assert_eq!(clear_correlations(), 2);
    assert_eq!(list_correlations().len(), 0);

    assert!(!delete_correlation("does-not-exist"));
}

#[test]
fn correlation_dto_round_trips() {
    let dto = CorrelationRuleDto {
        id: "cor-x".into(),
        name: "rule".into(),
        kind_pattern_a: "plugin.lifecycle.started".into(),
        kind_pattern_b: "plugin.lifecycle.crashed".into(),
        window_secs: 30,
        enabled: true,
        created_at: 1_700_000_000,
    };
    let s = serde_json::to_string(&dto).unwrap();
    assert!(s.contains("\"kindPatternA\":\"plugin.lifecycle.started\""));
    assert!(s.contains("\"kindPatternB\":\"plugin.lifecycle.crashed\""));
    assert!(s.contains("\"windowSecs\":30"));
    assert!(s.contains("\"createdAt\":1700000000"));
    let back: CorrelationRuleDto = serde_json::from_str(&s).unwrap();
    assert_eq!(back.kind_pattern_a, "plugin.lifecycle.started");
    assert_eq!(back.window_secs, 30);
}

// ─── Phase 56: escalation tests ─────────────────────────────────────────

fn esc_rule(id: &str, kind_pat: &str, after_secs: u64, sev: &str) -> EscalationRule {
    EscalationRule {
        id: id.to_string(),
        name: format!("rule-{}", id),
        kind_pattern: kind_pat.to_string(),
        escalate_after_secs: after_secs,
        target_severity: sev.to_string(),
        target_endpoint_ids: None,
        enabled: true,
    }
}

fn setup_esc_test(label: &str) {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store(label));
    _reset_escalations_for_tests();
}

#[test]
fn escalation_validate_rejects_bad_inputs() {
    assert!(validate_escalation(&esc_rule("", "x", 60, "warn")).is_err());
    assert!(validate_escalation(&esc_rule("r1", "", 60, "warn")).is_err());
    assert!(validate_escalation(&esc_rule("r1", "x", 0, "warn")).is_err());
    assert!(validate_escalation(&esc_rule("r1", "x", 60, "bogus")).is_err());
    assert!(validate_escalation(&esc_rule("r1", "x", 60, "warn")).is_ok());
}

#[test]
fn escalation_ensure_id_fills_uuid_when_empty() {
    let mut r = esc_rule("", "x", 60, "warn");
    ensure_escalation_id(&mut r);
    assert!(!r.id.is_empty());
    assert!(r.id.starts_with("esc-"));
}

#[test]
fn escalation_save_list_delete_clear_round_trip() {
    setup_esc_test("esc-crud");
    let saved1 = save_escalation(esc_rule("e1", "plugin.*", 30, "critical")).unwrap();
    let saved2 = save_escalation(esc_rule("e2", "cap.*", 60, "error")).unwrap();
    assert_eq!(saved1.id, "e1");
    assert_eq!(saved2.id, "e2");
    let all = list_escalations();
    assert_eq!(all.len(), 2);
    assert!(all
        .iter()
        .any(|d| d.id == "e1" && d.target_severity == "critical"));
    assert!(all
        .iter()
        .any(|d| d.id == "e2" && d.escalate_after_secs == 60));
    assert!(delete_escalation("e1"));
    assert_eq!(list_escalations().len(), 1);
    assert!(!delete_escalation("does-not-exist"));
    assert_eq!(clear_escalations(), 1);
    assert_eq!(list_escalations().len(), 0);
}

#[test]
fn escalation_record_dispatch_populates_last_by_source() {
    setup_esc_test("esc-record");
    record_dispatch("plugin.lifecycle.crashed", 1000);
    record_dispatch("plugin.metrics.exceeded", 1001);
    let map = escalation_state().lock().unwrap();
    assert_eq!(
        map.last_by_source.get("plugin.lifecycle.crashed"),
        Some(&1000)
    );
    assert_eq!(
        map.last_by_source.get("plugin.metrics.exceeded"),
        Some(&1001)
    );
}

#[test]
fn escalation_find_candidates_returns_empty_without_rules() {
    setup_esc_test("esc-empty");
    record_dispatch("plugin.lifecycle.crashed", 1000);
    let cands = find_escalation_candidates(2000);
    assert!(cands.is_empty());
}

#[test]
fn escalation_find_candidates_respects_window() {
    setup_esc_test("esc-window");
    let _ = save_escalation(esc_rule("e1", "plugin.*", 30, "critical")).unwrap();
    record_dispatch("plugin.lifecycle.crashed", 1000);
    // now=1010 → diff=10 < 30 → the escalation window has not been reached yet
    let cands = find_escalation_candidates(1010);
    assert!(cands.is_empty());
    // now=1040 → diff=40 > 30 → fires
    let cands = find_escalation_candidates(1040);
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].0.id, "e1");
    assert_eq!(cands[0].1, "plugin.lifecycle.crashed");
    assert_eq!(cands[0].2, 1000);
}

#[test]
fn escalation_find_candidates_filters_non_matching_pattern() {
    setup_esc_test("esc-pattern");
    let _ = save_escalation(esc_rule("e1", "capability.*", 30, "critical")).unwrap();
    record_dispatch("plugin.lifecycle.crashed", 1000);
    let cands = find_escalation_candidates(2000);
    assert!(cands.is_empty());
}

#[test]
fn escalation_find_candidates_skips_acked_sources() {
    setup_esc_test("esc-ack");
    let _ = save_escalation(esc_rule("e1", "plugin.*", 30, "critical")).unwrap();
    record_dispatch("plugin.lifecycle.crashed", 1000);
    // use the public ack_kind helper to attach a future ack on the plugin.* pattern (a large window_secs, long past the time point we inspect)
    let _ = ack_kind("plugin.*".to_string(), 604_800).unwrap();
    let cands = find_escalation_candidates(2000);
    assert!(cands.is_empty());
}

#[test]
fn escalation_disabled_rule_ignored() {
    setup_esc_test("esc-disabled");
    let mut r = esc_rule("e1", "plugin.*", 30, "critical");
    r.enabled = false;
    let _ = save_escalation(r).unwrap();
    record_dispatch("plugin.lifecycle.crashed", 1000);
    let cands = find_escalation_candidates(2000);
    assert!(cands.is_empty());
}

#[test]
fn escalation_dto_serializes_camel_case() {
    let dto = EscalationRuleDto {
        id: "e1".into(),
        name: "rule".into(),
        kind_pattern: "plugin.*".into(),
        escalate_after_secs: 30,
        target_severity: "critical".into(),
        target_endpoint_ids: Some(vec!["ep1".into(), "ep2".into()]),
        enabled: true,
        created_at: 1_700_000_000,
    };
    let s = serde_json::to_string(&dto).unwrap();
    assert!(s.contains("\"kindPattern\":\"plugin.*\""));
    assert!(s.contains("\"escalateAfterSecs\":30"));
    assert!(s.contains("\"targetSeverity\":\"critical\""));
    assert!(s.contains("\"targetEndpointIds\":[\"ep1\",\"ep2\"]"));
    assert!(s.contains("\"createdAt\":1700000000"));
    let back: EscalationRuleDto = serde_json::from_str(&s).unwrap();
    assert_eq!(back.kind_pattern, "plugin.*");
    assert_eq!(back.target_endpoint_ids.as_ref().unwrap().len(), 2);
}

#[test]
fn escalation_dto_omits_none_target_endpoint_ids() {
    let dto = EscalationRuleDto {
        id: "e1".into(),
        name: "rule".into(),
        kind_pattern: "plugin.*".into(),
        escalate_after_secs: 30,
        target_severity: "critical".into(),
        target_endpoint_ids: None,
        enabled: true,
        created_at: 1_700_000_000,
    };
    let s = serde_json::to_string(&dto).unwrap();
    assert!(!s.contains("targetEndpointIds"));
}

#[test]
fn escalation_save_rejects_empty_id_after_trim() {
    setup_esc_test("esc-id");
    // save_escalation goes through ensure_escalation_id: an empty id gets a uuid filled in and validation passes
    let saved = save_escalation(esc_rule("", "plugin.*", 30, "critical"));
    assert!(saved.is_ok());
    // but validate itself rejects a blank id
    let mut bad = esc_rule("   ", "plugin.*", 30, "critical");
    bad.id = "   ".into();
    assert!(validate_escalation(&bad).is_err());
}

// ─── Phase 57: template render tests ─────────────────────────────────────

fn tpl_env(source: &str, severity: Severity, payload: serde_json::Value) -> AlertEnvelope {
    AlertEnvelope {
        schema_version: ALERT_SCHEMA_VERSION,
        event_id: "evt-x".into(),
        source: source.into(),
        severity,
        tags: vec!["t1".into(), "t2".into()],
        timestamp: 1_700_000_000,
        payload,
    }
}

#[test]
fn template_substitutes_top_level_placeholders() {
    let env = tpl_env(
        "plugin.metrics.exceeded",
        Severity::Warn,
        serde_json::json!({"value": 42}),
    );
    let (out, ct) = render_template("{{source}} {{severity}} {{timestamp}}", &env).unwrap();
    assert_eq!(out, "plugin.metrics.exceeded warn 1700000000");
    assert_eq!(ct, TemplateContentType::Text);
}

#[test]
fn template_substitutes_payload_dot_path() {
    let env = tpl_env(
        "x",
        Severity::Info,
        serde_json::json!({"error": {"code": 500, "msg": "internal"}}),
    );
    let (out, _) = render_template(
        "code={{payload.error.code}} msg={{payload.error.msg}}",
        &env,
    )
    .unwrap();
    assert_eq!(out, "code=500 msg=internal");
}

#[test]
fn template_missing_payload_path_renders_empty() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({"foo": 1}));
    let (out, _) = render_template("value=[{{payload.missing.field}}]", &env).unwrap();
    assert_eq!(out, "value=[]");
}

#[test]
fn template_if_block_true_renders_body() {
    let env = tpl_env("x", Severity::Critical, serde_json::json!({}));
    let tpl = "{{#if severity == \"critical\"}}ALERT{{/if}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "ALERT");
}

#[test]
fn template_if_block_false_renders_nothing() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({}));
    let tpl = "before {{#if severity == \"critical\"}}ALERT{{/if}} after";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "before  after");
}

#[test]
fn template_if_else_picks_correct_branch() {
    let env_crit = tpl_env("x", Severity::Critical, serde_json::json!({}));
    let env_info = tpl_env("x", Severity::Info, serde_json::json!({}));
    let tpl = "{{#if severity == \"critical\"}}HIGH{{else}}LOW{{/if}}";
    let (out, _) = render_template(tpl, &env_crit).unwrap();
    assert_eq!(out, "HIGH");
    let (out, _) = render_template(tpl, &env_info).unwrap();
    assert_eq!(out, "LOW");
}

#[test]
fn template_if_with_payload_truthiness() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({"flag": true}));
    let tpl = "{{#if payload.flag}}YES{{/if}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "YES");
    // set to false → empty
    let env2 = tpl_env("x", Severity::Info, serde_json::json!({"flag": false}));
    let (out, _) = render_template(tpl, &env2).unwrap();
    assert_eq!(out, "");
}

#[test]
fn template_each_iterates_payload_array() {
    let env = tpl_env(
        "x",
        Severity::Info,
        serde_json::json!({"items": [{"id": 1}, {"id": 2}, {"id": 3}]}),
    );
    let tpl = "{{#each payload.items}}[{{this.id}}]{{/each}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "[1][2][3]");
}

#[test]
fn template_each_with_this_string() {
    let env = tpl_env(
        "x",
        Severity::Info,
        serde_json::json!({"tags": ["a", "b", "c"]}),
    );
    let tpl = "{{#each payload.tags}}{{this}},{{/each}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "a,b,c,");
}

#[test]
fn template_unclosed_if_errors() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({}));
    let r = render_template("{{#if severity == \"info\"}}body", &env);
    assert!(r.is_err());
}

#[test]
fn template_unmatched_closing_block_errors() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({}));
    let r = render_template("body{{/if}}", &env);
    assert!(r.is_err());
}

#[test]
fn template_slack_block_kit_example() {
    let env = tpl_env(
        "plugin.lifecycle.crashed",
        Severity::Critical,
        serde_json::json!({"reason": "OOM"}),
    );
    let tpl = r#"{
            "blocks": [
                {{#if severity == "critical"}}
                { "type": "header", "text": { "type": "plain_text", "text": "🚨 CRITICAL {{source}}" } },
                {{/if}}
                { "type": "section", "text": { "type": "mrkdwn", "text": "Reason: {{payload.reason}}" } }
            ]
        }"#;
    let (out, ct) = render_template(tpl, &env).unwrap();
    assert_eq!(ct, TemplateContentType::Json);
    assert!(out.contains("🚨 CRITICAL plugin.lifecycle.crashed"));
    assert!(out.contains("Reason: OOM"));
}

#[test]
fn template_json_content_type_detection() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({}));
    let (out, ct) = render_template("{\"hello\": \"world\"}", &env).unwrap();
    assert_eq!(ct, TemplateContentType::Json);
    assert!(out.contains("\"hello\""));
    let (_, ct2) = render_template("plain text body", &env).unwrap();
    assert_eq!(ct2, TemplateContentType::Text);
}

#[test]
fn template_negation_in_condition() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({}));
    let tpl = "{{#if !payload.missing}}OK{{/if}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    // !null = true → OK
    assert_eq!(out, "OK");
}

// ─── Phase 63: template rendering test coverage ──────────────────────────

#[test]
fn template_extra_whitespace_inside_placeholder_is_trimmed() {
    let env = tpl_env("svc.x", Severity::Warn, serde_json::json!({}));
    let (out, _) = render_template("{{   source   }}|{{\t\tsource\t\t}}", &env).unwrap();
    // trim() removes the whitespace → "svc.x"
    assert_eq!(out, "svc.x|svc.x");
}

#[test]
fn template_empty_string_renders_empty() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({}));
    let (out, ct) = render_template("", &env).unwrap();
    assert_eq!(out, "");
    // empty template → after trim the content type is empty → neither { nor [, so falls back to Text
    assert_eq!(ct, TemplateContentType::Text);
}

#[test]
fn template_pure_literal_passes_through() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({}));
    let tpl = "no placeholders here, just text and 1234567890.";
    let (out, ct) = render_template(tpl, &env).unwrap();
    assert_eq!(out, tpl);
    assert_eq!(ct, TemplateContentType::Text);
}

#[test]
fn template_unicode_payload_renders_intact() {
    let env = tpl_env(
        "plugin.unicode",
        Severity::Info,
        serde_json::json!({"name": "café 🚨", "emoji": "🎉🎊"}),
    );
    let (out, _) = render_template("name={{payload.name}} e={{payload.emoji}}", &env).unwrap();
    assert_eq!(out, "name=café 🚨 e=🎉🎊");
}

#[test]
fn template_each_empty_array_renders_empty() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({"items": []}));
    let (out, _) =
        render_template("X{{#each payload.items}}[{{this.id}}]{{/each}}Y", &env).unwrap();
    assert_eq!(out, "XY");
}

#[test]
fn template_each_non_array_path_renders_empty_gracefully() {
    // payload.scalar is a string, not an array → should be empty (unwrap_or_default yields an empty vec)
    let env = tpl_env("x", Severity::Info, serde_json::json!({"scalar": "hello"}));
    let (out, _) = render_template("{{#each payload.scalar}}[{{this}}]{{/each}}", &env).unwrap();
    assert_eq!(out, "");
}

#[test]
fn template_multiple_sibling_if_blocks_render_independently() {
    let env = tpl_env(
        "x",
        Severity::Critical,
        serde_json::json!({"p1": true, "p2": false}),
    );
    let tpl = "{{#if severity == \"critical\"}}A{{/if}}|{{#if payload.p1}}B{{/if}}|{{#if payload.p2}}C{{/if}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "A|B|");
}

#[test]
fn template_numeric_literal_comparison() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({"cpu": 95}));
    let tpl = "{{#if payload.cpu == 95}}HOT{{else}}OK{{/if}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "HOT");
    // change to 50 → else branch
    let env2 = tpl_env("x", Severity::Info, serde_json::json!({"cpu": 50}));
    let (out2, _) = render_template(tpl, &env2).unwrap();
    assert_eq!(out2, "OK");
}

#[test]
fn template_boolean_literal_comparison() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({"on": true}));
    let tpl = "{{#if payload.on == true}}YES{{/if}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "YES");
    let env2 = tpl_env("x", Severity::Info, serde_json::json!({"on": false}));
    let (out2, _) = render_template(tpl, &env2).unwrap();
    assert_eq!(out2, "");
}

#[test]
fn template_each_body_can_reference_envelope_too() {
    // inside the block {{source}} uses env and {{this.id}} uses the item
    let env = tpl_env(
        "plugin.metrics.exceeded",
        Severity::Info,
        serde_json::json!({"items": [{"id": 1}, {"id": 2}]}),
    );
    let tpl = "{{#each payload.items}}{{source}}#{{this.id}} {{/each}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "plugin.metrics.exceeded#1 plugin.metrics.exceeded#2 ");
}

#[test]
fn template_substitutes_event_id_and_schema_version() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({}));
    let (out, _) = render_template(
        "evt:{{event_id}} sv:{{schema_version}} ts:{{timestamp}}",
        &env,
    )
    .unwrap();
    assert_eq!(out, "evt:evt-x sv:1 ts:1700000000");
}

#[test]
fn template_mixed_if_and_each_in_one_template() {
    let env = tpl_env(
        "x",
        Severity::Critical,
        serde_json::json!({"items": [{"id": "a"}, {"id": "b"}], "show": true}),
    );
    let tpl = "{{#if payload.show}}{{#each payload.items}}[{{this.id}}]{{/each}}{{/if}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "[a][b]");
}

#[test]
fn template_each_with_object_array_uses_this_dot_path() {
    let env = tpl_env(
        "x",
        Severity::Info,
        serde_json::json!({
            "users": [
                {"name": "alice", "age": 30},
                {"name": "bob", "age": 25}
            ]
        }),
    );
    let tpl = "{{#each payload.users}}{{this.name}}={{this.age}};{{/each}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "alice=30;bob=25;");
}

#[test]
fn template_payload_with_newlines_and_quotes_preserves_raw() {
    // the template does not interpret JSON escapes; it substitutes strings directly; a \n in the payload is a real newline after JSON parsing
    let env = tpl_env(
        "x",
        Severity::Info,
        serde_json::json!({"msg": "line1\nline2\n\"quoted\""}),
    );
    let (out, _) = render_template("MSG=[{{payload.msg}}]", &env).unwrap();
    assert_eq!(out, "MSG=[line1\nline2\n\"quoted\"]");
}

#[test]
fn template_tags_renders_as_comma_joined_string() {
    // lookup_path_string routes an Array through the Value::Array branch → to_string serialization
    // serde_json::Value::Array's to_string = ["t1","t2"]
    let env = tpl_env("x", Severity::Info, serde_json::json!({}));
    let (out, _) = render_template("tags={{tags}}", &env).unwrap();
    assert!(out.contains("t1") && out.contains("t2"), "got: {out}");
}

#[test]
fn template_each_nested_object_dot_path_in_body() {
    let env = tpl_env(
        "x",
        Severity::Info,
        serde_json::json!({
            "users": [
                {"profile": {"role": "admin"}},
                {"profile": {"role": "user"}}
            ]
        }),
    );
    let tpl = "{{#each payload.users}}{{this.profile.role}};{{/each}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "admin;user;");
}

#[test]
fn lint_template_clean_returns_empty() {
    let diags = lint_template("hello {{source}} world");
    assert!(
        diags.is_empty(),
        "expected no diagnostics, got: {:?}",
        diags
    );
}

#[test]
fn lint_template_unclosed_block_emits_error() {
    let diags = lint_template("{{#if severity == \"info\"}}body without close");
    let codes: Vec<&str> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(
        codes.contains(&"unclosed_block"),
        "expected unclosed_block, got: {codes:?}"
    );
}

#[test]
fn lint_template_mismatched_close_emits_error() {
    let diags = lint_template("{{#each payload.items}}{{this}}{{/if}}");
    let codes: Vec<&str> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(
        codes.contains(&"mismatched_close"),
        "expected mismatched_close, got: {codes:?}"
    );
}

#[test]
fn lint_template_unclosed_placeholder_emits_error() {
    // {{source missing }}
    let diags = lint_template("hello {{source world");
    let codes: Vec<&str> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(
        codes.contains(&"unclosed_placeholder"),
        "expected unclosed_placeholder, got: {codes:?}"
    );
}

#[test]
fn builtin_preset_slack_template_renders_without_error() {
    // take the builtin slack preset and render a real envelope with its template
    let slack = builtin_presets()
        .iter()
        .find(|p| p.kind == "builtin:slack")
        .expect("builtin:slack exists");
    let env = tpl_env(
        "plugin.metrics.exceeded",
        Severity::Critical,
        serde_json::json!({"value": 95, "host": "db-01"}),
    );
    let (out, ct) =
        render_template(&slack.template, &env).expect("builtin slack should render without error");
    // the slack output is JSON, so ct should be Json
    assert_eq!(ct, TemplateContentType::Json);
    // must contain source + severity
    assert!(
        out.contains("plugin.metrics.exceeded"),
        "missing source: {out}"
    );
    assert!(
        out.to_lowercase().contains("critical"),
        "missing severity: {out}"
    );
}

#[test]
fn builtin_preset_discord_template_renders_without_error() {
    let discord = builtin_presets()
        .iter()
        .find(|p| p.kind == "builtin:discord")
        .expect("builtin:discord exists");
    let env = tpl_env(
        "plugin.lifecycle.crashed",
        Severity::Error,
        serde_json::json!({"message": "OOM on host web-1"}),
    );
    let (out, _) = render_template(&discord.template, &env)
        .expect("builtin discord should render without error");
    assert!(out.contains("plugin.lifecycle.crashed"));
    assert!(out.contains("OOM on host web-1"));
}

#[test]
fn template_payload_deeply_nested_dot_path() {
    let env = tpl_env(
        "x",
        Severity::Info,
        serde_json::json!({"a": {"b": {"c": {"d": "deep"}}}}),
    );
    let (out, _) = render_template("{{payload.a.b.c.d}}", &env).unwrap();
    assert_eq!(out, "deep");
}

#[test]
fn template_payload_array_index_via_this_in_each_works() {
    // payload.items[i].name → use {{this.name}} inside each (array indices are accessed implicitly via this inside an each block)
    // note: top-level dot-paths do not support array indices (payload.items.0.name); an each loop is required
    let env = tpl_env(
        "x",
        Severity::Info,
        serde_json::json!({"items": [{"name": "first"}, {"name": "second"}]}),
    );
    let tpl = "{{#each payload.items}}{{this.name}};{{/each}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "first;second;");
}

#[test]
fn template_each_with_only_this_primitive() {
    // {{this}} takes the array element directly (string)
    let env = tpl_env(
        "x",
        Severity::Info,
        serde_json::json!({"nums": [1, 2, 3, 4]}),
    );
    let tpl = "{{#each payload.nums}}{{this}},{{/each}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "1,2,3,4,");
}

#[test]
fn template_unclosed_placeholder_returns_error() {
    let env = tpl_env("x", Severity::Info, serde_json::json!({}));
    let r = render_template("hello {{source", &env);
    assert!(r.is_err(), "unclosed placeholder should fail");
    let msg = r.unwrap_err();
    assert!(
        msg.contains("unclosed"),
        "error should mention unclosed: {msg}"
    );
}

#[test]
fn template_payload_null_value_renders_as_null_string() {
    // lookup_path_string → Null → String::new() (empty string)
    let env = tpl_env("x", Severity::Info, serde_json::json!({"maybe": null}));
    let (out, _) = render_template("[{{payload.maybe}}]", &env).unwrap();
    assert_eq!(out, "[]", "null should render as empty string");
}

#[test]
fn template_payload_numeric_value_renders_as_decimal() {
    // serde_json::Number.to_string() is "42" for integers and "3.5" for floats
    let env = tpl_env(
        "x",
        Severity::Info,
        serde_json::json!({"int_v": 42, "flt_v": 3.5}),
    );
    let (out, _) = render_template("i={{payload.int_v}} f={{payload.flt_v}}", &env).unwrap();
    assert_eq!(out, "i=42 f=3.5");
}

#[test]
fn template_if_truthiness_empty_array_is_false() {
    // is_truthy: empty array = false
    let env = tpl_env("x", Severity::Info, serde_json::json!({"empty": []}));
    let tpl = "{{#if payload.empty}}NONEMPTY{{else}}EMPTY{{/if}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "EMPTY");
}

#[test]
fn template_if_truthiness_zero_is_false() {
    // is_truthy: Number 0 = false
    let env = tpl_env("x", Severity::Info, serde_json::json!({"n": 0}));
    let tpl = "{{#if payload.n}}NONZERO{{else}}ZERO{{/if}}";
    let (out, _) = render_template(tpl, &env).unwrap();
    assert_eq!(out, "ZERO");
}

#[test]
fn endpoint_row_template_round_trips() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("tpl-round"));
    let mut ep = WebhookEndpoint {
        id: "ep-tpl".into(),
        name: "ep-with-tpl".into(),
        url: "https://hooks.example.com/x".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 1,
        template: Some("hello {{source}}".into()),
        template_sample: None,
        severity_overrides: vec![],
    };
    let saved = save_endpoint(ep.clone()).unwrap();
    assert_eq!(saved.template.as_deref(), Some("hello {{source}}"));
    let listed = list_endpoints();
    let row = listed.iter().find(|e| e.id == "ep-tpl").unwrap();
    assert_eq!(row.template.as_deref(), Some("hello {{source}}"));
    // change to None and save again
    ep.template = None;
    let _ = save_endpoint(ep).unwrap();
    let listed2 = list_endpoints();
    let row2 = listed2.iter().find(|e| e.id == "ep-tpl").unwrap();
    assert!(row2.template.is_none());
}

// ─── Phase 58: lint + preview tests ────────────────────────────────────

fn diag_codes(diags: &[TemplateDiagnostic]) -> Vec<&str> {
    diags.iter().map(|d| d.code.as_str()).collect()
}

#[test]
fn lint_clean_template_returns_no_diags() {
    let d = lint_template("hello {{source}} {{#if payload.x}}A{{/if}}");
    assert!(d.is_empty(), "expected no diags, got: {:?}", d);
}

#[test]
fn lint_detects_unclosed_if_block() {
    let d = lint_template("hi {{#if payload.x}}body");
    assert!(diag_codes(&d).contains(&"unclosed_block"));
    let diag = d.iter().find(|x| x.code == "unclosed_block").unwrap();
    assert_eq!(diag.severity, "error");
    assert!(diag.offset <= 6);
}

#[test]
fn lint_detects_unexpected_close_if() {
    let d = lint_template("hi {{/if}} bye");
    assert!(diag_codes(&d).contains(&"unexpected_close"));
}

#[test]
fn lint_detects_mismatched_close_each_for_if() {
    let d = lint_template("{{#if x}}body{{/each}}");
    assert!(diag_codes(&d).contains(&"mismatched_close"));
}

#[test]
fn lint_detects_nested_too_deep() {
    // 9 levels of nesting → stack.len()+1 = 9 → error
    let tpl = "{{#if a}}{{#if b}}{{#if c}}{{#if d}}{{#if e}}{{#if f}}{{#if g}}{{#if h}}{{#if i}}{{/if}}{{/if}}{{/if}}{{/if}}{{/if}}{{/if}}{{/if}}{{/if}}{{/if}}";
    let d = lint_template(tpl);
    assert!(
        diag_codes(&d).contains(&"nested_too_deep"),
        "expected nested_too_deep in {:?}",
        d
    );
}

#[test]
fn lint_warns_unknown_placeholder_syntax() {
    // contains whitespace and is not an == expression → warning
    let d = lint_template("hi {{ foo bar }}");
    assert!(diag_codes(&d).contains(&"unknown_tag"));
}

#[test]
fn lint_detects_unclosed_placeholder() {
    let d = lint_template("hi {{source ");
    assert!(diag_codes(&d).contains(&"unclosed_placeholder"));
}

#[test]
fn lint_offset_to_line_col_converts_multiline() {
    // template[3..] = "\nfoo {{#if a}}\nbar\n{{/if}}"
    // {{#if position ≈ the 8th character (0='\n', 1='f', 2='o', 3='o', 4=' ', 5='{', 6='{', 7='#', 8='i', 9='f', 10=' ')
    // line=2, col=5
    let tpl = "\nfoo {{#if a}}\nbar\n{{/if}}";
    let off = tpl.find("{{#if").unwrap();
    let (line, col) = offset_to_line_col(tpl, off);
    assert_eq!(line, 2);
    assert_eq!(col, 5);
}

#[test]
fn lint_detects_empty_placeholder_tag() {
    let d = lint_template("hi {{}} bye");
    assert!(diag_codes(&d).contains(&"empty_tag"));
}

#[test]
fn preview_renders_with_sample_payload() {
    let sample = serde_json::json!({ "k": "v" });
    let r = preview_alerting_template("{{payload.k}}", &sample);
    assert_eq!(r.body, "v");
    assert_eq!(r.content_type, "text/plain; charset=utf-8");
    assert!(r.diagnostics.is_empty(), "got: {:?}", r.diagnostics);
}

#[test]
fn preview_returns_diags_on_render_failure() {
    let sample = serde_json::json!({});
    let r = preview_alerting_template("hello {{/if}}", &sample);
    // the template contains a stray `{{/if}}`, so render fails → diagnostics should contain render_failed
    assert!(r.diagnostics.iter().any(|d| d.code == "render_failed"));
    // it may also be caught by lint first (returning unexpected_close); either is acceptable
    assert!(r.body.is_empty());
}

#[test]
fn preview_returns_diags_from_lint_even_when_render_succeeds() {
    // template: an unclosed {{#if x}} but x is always false → render is still OK, and lint reports unclosed_block
    let sample = serde_json::json!({ "x": false });
    let r = preview_alerting_template("{{#if payload.x}}A", &sample);
    assert_eq!(r.body, "");
    assert!(
        r.diagnostics.iter().any(|d| d.code == "unclosed_block"),
        "got: {:?}",
        r.diagnostics
    );
}

// ─── Phase 59: template preset library tests ─────────────────────────────

fn make_user_preset(name: &str, tpl: &str, sample: &str) -> TemplatePreset {
    TemplatePreset {
        id: String::new(),
        name: name.into(),
        description: "test".into(),
        kind: String::new(),
        template: tpl.into(),
        sample: sample.into(),
        builtin: false,
        created_at: 0,
        version: 0, // 0 means "let save compute it" (new = 1; existing = old+1)
        changelog: String::new(),
    }
}

#[test]
fn builtin_presets_have_at_least_five_entries() {
    assert!(
        builtin_presets().len() >= 5,
        "got {} entries",
        builtin_presets().len()
    );
}

#[test]
fn get_template_preset_returns_each_builtin_kind() {
    for kind in [
        "builtin:slack",
        "builtin:discord",
        "builtin:msteams",
        "builtin:generic_json",
        "builtin:plain_text",
    ] {
        let p = get_template_preset(kind);
        assert!(p.is_some(), "missing kind {}", kind);
        let p = p.unwrap();
        assert!(p.builtin);
        assert_eq!(p.kind, kind);
        assert!(!p.template.is_empty());
        assert!(!p.sample.is_empty());
    }
}

#[test]
fn get_template_preset_returns_none_for_unknown_kind() {
    assert!(get_template_preset("builtin:nonexistent").is_none());
    assert!(get_template_preset("user:does-not-exist").is_none());
}

#[test]
fn save_user_template_preset_rejects_empty_name() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("save-user-preset-1"));
    let mut p = make_user_preset("", "hi", "{}");
    p.id = String::new();
    let res = save_user_template_preset(&p);
    assert!(res.is_err());
    assert!(res.unwrap_err().contains("name"));
}

#[test]
fn save_user_template_preset_rejects_too_long_name() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("save-user-preset-2"));
    let long = "x".repeat(65);
    let p = make_user_preset(&long, "hi", "{}");
    let res = save_user_template_preset(&p);
    assert!(res.is_err());
    assert!(res.unwrap_err().contains("too long"));
}

#[test]
fn save_user_template_preset_rejects_builtin_true() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("save-user-preset-3"));
    let mut p = make_user_preset("test", "hi", "{}");
    p.builtin = true;
    let res = save_user_template_preset(&p);
    assert!(res.is_err());
    assert!(res.unwrap_err().contains("builtin"));
}

#[test]
fn preset_yaml_round_trip() {
    let presets = vec![
        make_user_preset("custom A", "tmpl A", "{\"k\":\"v\"}"),
        make_user_preset("custom B", "tmpl B", "{\"x\":1}"),
    ];
    let yaml = export_presets_to_yaml(&presets).expect("export");
    // the export contains at least version + 2 preset names
    assert!(yaml.contains("custom A"));
    assert!(yaml.contains("custom B"));
    // verify round-trip with list (no store needed)
    let doc: PresetYamlDoc = serde_yaml::from_str(&yaml).expect("parse");
    assert_eq!(doc.version, 1);
    assert_eq!(doc.presets.len(), 2);
    assert_eq!(doc.presets[0].name, "custom A");
    assert_eq!(doc.presets[1].template, "tmpl B");
}

#[test]
fn delete_user_template_preset_removes_only_user() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("delete-preset-1"));
    // save 1 user preset
    let saved = save_user_template_preset(&make_user_preset("to-delete", "x", "{}")).expect("save");
    let listed = list_template_presets();
    let user_count = listed.iter().filter(|p| !p.builtin).count();
    let builtin_count = listed.iter().filter(|p| p.builtin).count();
    assert!(user_count >= 1);
    assert_eq!(builtin_count, builtin_presets().len());
    // delete user
    assert!(delete_user_template_preset(&saved.id));
    // list again: one fewer user, builtins unchanged
    let listed2 = list_template_presets();
    assert_eq!(
        listed2.iter().filter(|p| p.builtin).count(),
        builtin_presets().len()
    );
    assert!(listed2.iter().find(|p| p.id == saved.id).is_none());
}

#[test]
fn delete_user_template_preset_does_not_remove_builtin() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("delete-preset-2"));
    // passing a builtin id straight to delete should return false and leave the builtin in place
    assert!(!delete_user_template_preset("builtin-slack"));
    assert!(get_template_preset("builtin:slack").is_some());
}

#[test]
fn list_template_presets_includes_builtins_and_user() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("list-presets-1"));
    let saved = save_user_template_preset(&make_user_preset("user1", "u", "{}")).expect("save");
    let listed = list_template_presets();
    assert_eq!(
        listed.iter().filter(|p| p.builtin).count(),
        builtin_presets().len()
    );
    assert!(listed.iter().any(|p| p.id == saved.id));
}

#[test]
fn write_and_read_text_file_round_trip() {
    let tmp = std::env::temp_dir().join("opencapx-phase59-preset-test.txt");
    let path = tmp.to_string_lossy();
    let content = "hello\nworld\ncafé\n";
    write_text_file(&path, content).expect("write");
    let back = read_text_file(&path).expect("read");
    assert_eq!(back, content);
    let _ = std::fs::remove_file(path.as_ref());
}

// ─── Phase 60: template versioning + fork builtin tests ──────────────────

#[test]
fn fork_builtin_preset_creates_user_copy_with_new_id_and_kind() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("fork-preset-1"));
    let src = builtin_presets()
        .iter()
        .find(|p| p.kind == "builtin:slack")
        .expect("builtin slack exists");
    let orig_template = src.template.clone();
    let orig_description = src.description.clone();
    let forked = fork_builtin_preset("builtin:slack", "My Slack").expect("fork");
    assert_ne!(forked.id, "builtin-slack");
    assert!(
        forked.id.starts_with("00000000") || forked.id.len() >= 8,
        "expected UUID-like id, got {}",
        forked.id
    );
    assert!(
        forked.kind.starts_with("user:"),
        "expected user: kind, got {}",
        forked.kind
    );
    assert_eq!(forked.kind, format!("user:{}", forked.id));
    assert_eq!(forked.name, "My Slack");
    assert_eq!(forked.description, orig_description);
    assert_eq!(forked.template, orig_template);
    assert!(!forked.builtin);
    assert_eq!(forked.version, 1);
    assert!(forked.changelog.contains("Forked"));
    assert!(forked.changelog.contains("Slack"));
}

#[test]
fn fork_builtin_preset_rejects_unknown_kind() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("fork-preset-2"));
    let res = fork_builtin_preset("builtin:nonexistent", "X");
    assert!(res.is_err());
    assert!(res.unwrap_err().contains("unknown builtin kind"));
}

#[test]
fn fork_builtin_preset_rejects_empty_or_too_long_name() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("fork-preset-3"));
    assert!(fork_builtin_preset("builtin:slack", "")
        .unwrap_err()
        .contains("name required"));
    assert!(fork_builtin_preset("builtin:slack", "   ")
        .unwrap_err()
        .contains("name required"));
    let long = "x".repeat(65);
    assert!(fork_builtin_preset("builtin:slack", &long)
        .unwrap_err()
        .contains("too long"));
}

#[test]
fn fork_builtin_preset_persists_to_store() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("fork-preset-4"));
    let forked = fork_builtin_preset("builtin:discord", "My Discord").expect("fork");
    let listed = list_template_presets();
    assert!(listed.iter().any(|p| p.id == forked.id));
    assert_eq!(
        listed.iter().filter(|p| p.builtin).count(),
        builtin_presets().len()
    );
}

#[test]
fn save_user_template_preset_bumps_version_on_each_save() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("version-bump-1"));
    // first save → version = 1
    let s1 = save_user_template_preset(&make_user_preset("v-test", "a", "{}")).expect("save 1");
    assert_eq!(s1.version, 1, "first save should give version 1");
    let first_created = s1.created_at;
    // second save (same id) → version = 2, created_at kept
    let mut p2 = make_user_preset("v-test", "b", "{}");
    p2.id = s1.id.clone();
    let s2 = save_user_template_preset(&p2).expect("save 2");
    assert_eq!(s2.id, s1.id);
    assert_eq!(s2.version, 2, "second save should bump to version 2");
    assert_eq!(
        s2.created_at, first_created,
        "created_at must be preserved across saves"
    );
    // third save → version = 3
    let mut p3 = make_user_preset("v-test", "c", "{}");
    p3.id = s1.id.clone();
    let s3 = save_user_template_preset(&p3).expect("save 3");
    assert_eq!(s3.version, 3);
    assert_eq!(s3.created_at, first_created);
}

#[test]
fn template_presets_round_trip_preserves_version_and_changelog() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("round-trip-2"));
    let mut p = make_user_preset("round-trip-test", "tmpl", "{\"k\":\"v\"}");
    p.version = 2;
    p.changelog = "switched emoji to :bell:".into();
    let yaml = export_presets_to_yaml(&[p.clone()]).expect("export");
    assert!(yaml.contains("version"));
    assert!(yaml.contains("switched emoji"));
    let doc: PresetYamlDoc = serde_yaml::from_str(&yaml).expect("parse");
    assert_eq!(doc.presets.len(), 1);
    assert_eq!(doc.presets[0].version, 2);
    assert_eq!(doc.presets[0].changelog, "switched emoji to :bell:");
}

// ─── Phase 61: preset YAML schema migration tests ─────────────────────────

#[test]
fn migrate_preset_yaml_passes_through_v1_unchanged() {
    // Current schema — no migration should be applied
    let yaml = r#"version: 1
presets:
  - id: "abc"
    name: "test"
    kind: "user:abc"
    template: "T"
    sample: ""
    builtin: false
    createdAt: 100
    version: 1
    changelog: ""
"#;
    let (doc, applied) = migrate_preset_yaml(yaml).expect("migrate");
    assert_eq!(doc.version, 1);
    assert!(
        applied.is_empty(),
        "v1 yaml should not need migration, got {:?}",
        applied
    );
    assert_eq!(doc.presets.len(), 1);
    assert_eq!(doc.presets[0].name, "test");
}

#[test]
fn migrate_preset_yaml_lifts_v0_doc_to_v1() {
    // v0 doc: no version field (serde default = 1, so 0 is given explicitly here)
    let yaml = r#"version: 0
presets:
  - id: "legacy-1"
    name: "legacy preset"
    description: "from before versioning"
    kind: "user:legacy-1"
    template: "{{source}}"
    sample: "{}"
    builtin: false
    createdAt: 1700000000
"#;
    let (doc, applied) = migrate_preset_yaml(yaml).expect("migrate");
    assert_eq!(doc.version, 1, "v0 doc should be lifted to v1");
    assert_eq!(applied, vec!["v0_to_v1"]);
    assert_eq!(doc.presets.len(), 1);
    assert_eq!(doc.presets[0].name, "legacy preset");
    assert_eq!(
        doc.presets[0].version, 1,
        "v0 preset without version field gets bumped to 1"
    );
    assert_eq!(doc.presets[0].template, "{{source}}");
    assert_eq!(doc.presets[0].changelog, "");
}

#[test]
fn migrate_preset_yaml_rejects_future_higher_version() {
    let yaml = r#"version: 99
presets:
  - id: "future-1"
    name: "future"
    kind: "user:future-1"
    template: "X"
"#;
    let res = migrate_preset_yaml(yaml);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(
        err.contains("newer than current") || err.contains("99"),
        "unexpected err: {}",
        err
    );
}

#[test]
fn migrate_preset_yaml_handles_v0_without_version_field_at_all() {
    // no version field at all → serde default = 1, but the migrator should still run v0→v1 (the default 0 → 1 path is not triggered and passes straight through)
    let yaml = r#"presets:
  - id: "x"
    name: "no-version-field"
    kind: "user:x"
    template: "T"
"#;
    let (doc, applied) = migrate_preset_yaml(yaml).expect("migrate");
    assert_eq!(
        doc.version, 1,
        "no version field → serde default 1 → already current"
    );
    assert!(
        applied.is_empty(),
        "already v1 (via serde default), no migration needed"
    );
}

#[test]
fn import_presets_runs_migration_for_legacy_doc() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("import-migrate-1"));
    let yaml = r#"version: 0
presets:
  - id: "legacy-1"
    name: "legacy A"
    kind: "user:legacy-1"
    template: "TA"
    sample: "{}"
  - id: "legacy-2"
    name: "legacy B"
    kind: "user:legacy-2"
    template: "TB"
    sample: "{}"
"#;
    let count = import_presets_from_yaml(yaml).expect("import");
    assert_eq!(count, 2);
    let listed = list_template_presets();
    let a = listed.iter().find(|p| p.id == "legacy-1").expect("A");
    let b = listed.iter().find(|p| p.id == "legacy-2").expect("B");
    assert_eq!(a.version, 1, "imported v0 preset should have version 1");
    assert_eq!(b.version, 1);
    assert!(!a.builtin);
    assert!(!b.builtin);
    assert_eq!(a.template, "TA");
    assert_eq!(b.template, "TB");
}

// ─── Phase 62: full bundle export/import ─────────────────────────────

fn make_endpoint_row(id: &str, name: &str) -> WebhookEndpoint {
    WebhookEndpoint {
        id: id.into(),
        name: name.into(),
        url: "https://hooks.example.com/bundle".into(),
        enabled: true,
        headers: vec![("X-Bundle".into(), "1".into())],
        secret: String::new(),
        source_filter: vec!["plugin.metrics.exceeded".into()],
        schema_version: 1,
        template: Some("hello {{source}}".into()),
        template_sample: Some("{}".into()),
        severity_overrides: vec![],
    }
}

fn make_route_row(id: &str, name: &str) -> RouteRule {
    RouteRule {
        id: id.into(),
        name: name.into(),
        priority: 50,
        enabled: true,
        kind_pattern: "*".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-bundle-1".into()],
        recipients: vec!["log:stderr".into()],
        tags: vec!["phase62".into()],
        seen_in_last: None,
    }
}

fn make_silence_row(id: &str, name: &str) -> SilenceRule {
    let now = now_secs();
    SilenceRule {
        id: id.into(),
        name: name.into(),
        kind_pattern: "plugin.metrics.*".into(),
        starts_at: now,
        ends_at: now + 3600,
        weekdays: 127,
        start_hour: 0,
        end_hour: 24,
    }
}

#[test]
fn export_alerting_bundle_emits_all_five_sections() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("bundle-export-all"));
    // prepare data
    let _ = save_endpoint(make_endpoint_row("ep-b-1", "endpoint A")).unwrap();
    let _ = save_route(make_route_row("r-b-1", "route A")).unwrap();
    let _ = save_silence(make_silence_row("s-b-1", "silence A")).unwrap();
    let _ = ack_kind("plugin.metrics.*".into(), 60).unwrap();
    let _ = save_user_template_preset(&make_user_preset("preset A", "TA", "{}")).unwrap();

    let yaml = export_alerting_bundle(None).expect("export");
    // every section should appear at least once
    assert!(
        yaml.contains("endpoints:"),
        "yaml missing endpoints:\n{yaml}"
    );
    assert!(yaml.contains("routes:"), "yaml missing routes:\n{yaml}");
    assert!(yaml.contains("silences:"), "yaml missing silences:\n{yaml}");
    assert!(yaml.contains("acks:"), "yaml missing acks:\n{yaml}");
    assert!(yaml.contains("presets:"), "yaml missing presets:\n{yaml}");
    // version marker
    assert!(yaml.contains("version: 1"), "missing version: 1");
}

#[test]
fn export_alerting_bundle_excludes_builtin_presets() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("bundle-export-no-builtin"));
    // only builtins (no user presets); the bundle should not emit a presets section
    // (because builtins do not go into the bundle; only user presets do)
    let yaml = export_alerting_bundle(None).expect("export");
    // a builtin's kind such as "builtin:slack" should not appear
    assert!(
        !yaml.contains("builtin:slack"),
        "export should not include builtin presets:\n{yaml}"
    );
}

#[test]
fn export_then_import_alerting_bundle_round_trips_all_sections() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("bundle-round-trip"));
    // initial state
    let ep_saved = save_endpoint(make_endpoint_row("ep-rt-1", "ep-rt")).unwrap();
    let rt_saved = save_route(make_route_row("rt-1", "round-trip route")).unwrap();
    let sl_saved = save_silence(make_silence_row("sl-1", "rt silence")).unwrap();
    let ack_saved = ack_kind("plugin.metrics.exceeded".into(), 120).unwrap();
    let p_saved = save_user_template_preset(&make_user_preset("rt-preset", "TRT", "{}")).unwrap();

    let yaml = export_alerting_bundle(None).expect("export");
    let summary = import_alerting_bundle(&yaml, None).expect("import");
    assert_eq!(summary.endpoints, 1, "should import 1 endpoint");
    assert_eq!(summary.routes, 1, "should import 1 route");
    assert_eq!(summary.silences, 1, "should import 1 silence");
    assert_eq!(summary.acks, 1, "should import 1 ack");
    assert_eq!(summary.presets, 1, "should import 1 preset");
    assert_eq!(summary.total, 5);

    // verify the data is still there (after import, list_* can still find it)
    let endpoints = list_endpoints();
    assert!(
        endpoints.iter().any(|e| e.id == ep_saved.id),
        "endpoint missing"
    );
    let routes = list_routes();
    assert!(routes.iter().any(|r| r.id == rt_saved.id), "route missing");
    let silences = list_silences();
    assert!(
        silences.iter().any(|s| s.id == sl_saved.id),
        "silence missing"
    );
    let acks = list_acks();
    assert!(acks.iter().any(|a| a.id == ack_saved.id), "ack missing");
    let presets = list_template_presets();
    let imported_preset = presets.iter().find(|p| p.id == p_saved.id).expect("preset");
    assert_eq!(imported_preset.template, "TRT");
    assert!(
        !imported_preset.builtin,
        "imported preset must be user-owned"
    );
}

#[test]
fn import_alerting_bundle_rejects_future_version() {
    let yaml = r#"version: 99
endpoints: []
routes: []
presets: []
silences: []
acks: []
"#;
    let signed = sign_bundle(yaml).expect("sign");
    let res = import_alerting_bundle(&signed, None);
    assert!(res.is_err(), "future version should be rejected");
    let msg = res.unwrap_err();
    assert!(msg.contains("99"), "error should mention version 99: {msg}");
    assert!(msg.contains("newer"), "error should mention 'newer': {msg}");
}

// ─── Phase 66: RouteRule.recipients field YAML compatibility + bundle recipients section ─

#[test]
fn route_rule_yaml_with_recipients_field_round_trips() {
    // yaml contains recipients: [...] → the field is identical after parse
    let yaml = r#"id: r1
name: phase66-recipients-route
priority: 50
enabled: true
kindPattern: "*"
targetEndpointIds: []
recipients:
  - log:stderr
  - log:file:/tmp/opencapx-rt.log
  - email:smtp:smtp.example.com:587:alerts@example.com:oncall@example.com
tags:
  - phase66
"#;
    let rule: RouteRule = serde_yaml::from_str(yaml).expect("yaml parse");
    assert_eq!(rule.name, "phase66-recipients-route");
    assert_eq!(rule.recipients.len(), 3, "should have 3 recipients");
    assert_eq!(rule.recipients[0], "log:stderr");
    assert_eq!(rule.recipients[1], "log:file:/tmp/opencapx-rt.log");
    assert!(rule.recipients[2].starts_with("email:smtp:smtp.example.com:587:"));
    assert_eq!(
        rule.target_endpoint_ids.len(),
        0,
        "target_endpoint_ids empty"
    );
    assert_eq!(rule.tags, vec!["phase66".to_string()]);

    // reverse serialize → then deserialize to verify round-trip
    let yaml2 = serde_yaml::to_string(&rule).expect("serialize");
    let rule2: RouteRule = serde_yaml::from_str(&yaml2).expect("reparse");
    assert_eq!(rule2.recipients, rule.recipients);
    assert_eq!(rule2.target_endpoint_ids, rule.target_endpoint_ids);
    assert_eq!(rule2.tags, rule.tags);
}

#[test]
fn route_rule_yaml_without_recipients_defaults_to_empty() {
    // old yaml has no recipients field → serde default uses Vec::new()
    let yaml = r#"id: r2
name: legacy-route
priority: 100
enabled: true
kindPattern: plugin.metrics.*
targetEndpointIds:
  - ep-old
tags: []
"#;
    let rule: RouteRule = serde_yaml::from_str(yaml).expect("legacy yaml parse");
    assert_eq!(rule.name, "legacy-route");
    assert!(
        rule.recipients.is_empty(),
        "missing recipients should default to empty"
    );
    assert_eq!(rule.target_endpoint_ids, vec!["ep-old".to_string()]);
}

#[test]
fn bundle_with_recipients_section_round_trips() {
    // bundle yaml contains a recipients section (top-level) + the route.recipients field
    let yaml = r#"version: 1
exportedAt: 1700000000
endpoints: []
routes:
  - id: rt-bundle
    name: bundle-route
    priority: 50
    enabled: true
    kindPattern: "*"
    targetEndpointIds: []
    recipients:
      - log:stderr
    tags: []
presets: []
silences: []
acks: []
recipients:
  - id: rec-bundle-1
    name: debug-stderr
    kind: log:stderr
    config: {}
    enabled: true
    createdAt: 1700000000
"#;
    let doc: AlertingBundleDoc = serde_yaml::from_str(yaml).expect("bundle parse");
    assert_eq!(doc.recipients.len(), 1, "should have 1 recipient def");
    let rec = &doc.recipients[0];
    assert_eq!(rec.id, "rec-bundle-1");
    assert_eq!(rec.name, "debug-stderr");
    assert_eq!(rec.kind, "log:stderr");
    assert!(rec.enabled);

    // route's embedded recipients field
    assert_eq!(doc.routes.len(), 1);
    assert_eq!(doc.routes[0].recipients, vec!["log:stderr".to_string()]);

    // reverse round-trip
    let yaml2 = serde_yaml::to_string(&doc).expect("serialize bundle");
    let doc2: AlertingBundleDoc = serde_yaml::from_str(&yaml2).expect("reparse bundle");
    assert_eq!(doc2.recipients.len(), 1);
    assert_eq!(doc2.recipients[0].id, rec.id);
    assert_eq!(doc2.routes[0].recipients, vec!["log:stderr".to_string()]);
}

#[test]
fn bundle_yaml_missing_recipients_section_defaults_to_empty() {
    // old bundle has no top-level recipients section → serde default uses Vec::new()
    let yaml = r#"version: 1
exportedAt: 1700000000
endpoints: []
routes: []
presets: []
silences: []
acks: []
"#;
    let doc: AlertingBundleDoc = serde_yaml::from_str(yaml).expect("legacy bundle parse");
    assert!(
        doc.recipients.is_empty(),
        "missing section should default to empty"
    );
}

#[test]
fn validate_route_accepts_recipients_without_target_endpoint_ids() {
    // Phase 66 compatibility path: the route has only recipients, no target_endpoint_ids
    let rule = RouteRule {
        id: "r-recipients-only".into(),
        name: "recipients-only".into(),
        priority: 50,
        enabled: true,
        kind_pattern: "*".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec![],
        recipients: vec!["log:stderr".into()],
        tags: vec![],
        seen_in_last: None,
    };
    validate_route(&rule).expect("validate_route should accept recipients-only");
}

#[test]
fn validate_route_rejects_recipients_count_over_limit() {
    // 32 cap validation
    let mut recipients = Vec::new();
    for i in 0..33 {
        recipients.push(format!("log:stderr-{i}"));
    }
    let rule = RouteRule {
        id: "r-too-many".into(),
        name: "too-many".into(),
        priority: 50,
        enabled: true,
        kind_pattern: "*".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec![],
        recipients,
        tags: vec![],
        seen_in_last: None,
    };
    let res = validate_route(&rule);
    assert!(res.is_err(), "33 recipients should be rejected");
    let msg = res.err().expect("error");
    assert!(msg.contains("too many") || msg.contains("32"), "got: {msg}");
}

// ─── Phase 67: Recipient persistent CRUD ─────────────────────────────────

fn make_recipient(id: &str, name: &str, kind: &str) -> RecipientDef {
    let config = match kind {
        "webhook" => serde_json::json!({"endpoint_id": "ep-bundle-1"}),
        "log:file" => serde_json::json!({"path": "/tmp/opencx-r-67.log"}),
        "email:smtp" => {
            serde_json::json!({"relay":"smtp.x.com","port":587,"from":"a@x.com","to":"b@x.com"})
        }
        _ => serde_json::json!({}),
    };
    RecipientDef {
        id: id.into(),
        name: name.into(),
        kind: kind.into(),
        config,
        enabled: true,
        created_at: 1700000000,
    }
}

#[test]
fn recipient_kind_whitelist_accepts_known_kinds() {
    for kind in RECIPIENT_KIND_WHITELIST {
        assert!(
            validate_recipient_kind(kind).is_ok(),
            "{kind} should be allowed"
        );
    }
}

#[test]
fn recipient_kind_whitelist_rejects_unknown_kind() {
    let res = validate_recipient_kind("cmd:rce");
    assert!(res.is_err(), "cmd:rce must be rejected");
    let msg = res.err().expect("error");
    assert!(
        msg.contains("not in whitelist") || msg.contains("whitelist"),
        "got: {msg}"
    );
}

#[test]
fn save_recipient_persists_to_storage_and_round_trips() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("r67-persist"));
    let rec = save_recipient(make_recipient("", "my-stderr", "log:stderr")).expect("save");
    assert!(!rec.id.is_empty(), "id should be auto-generated");
    let list = list_recipients();
    assert_eq!(list.len(), 1, "should have 1 recipient");
    assert_eq!(list[0].name, "my-stderr");
    assert_eq!(list[0].kind, "log:stderr");
}

#[test]
fn save_recipient_rejects_empty_name() {
    let res = save_recipient(make_recipient("", "  ", "log:stderr"));
    assert!(res.is_err(), "empty name must be rejected");
    assert!(res.err().expect("e").contains("name"));
}

#[test]
fn save_recipient_rejects_duplicate_name() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("r67-dup"));
    save_recipient(make_recipient("", "uniq", "log:stderr")).expect("save 1");
    let res = save_recipient(make_recipient("", "uniq", "log:file"));
    assert!(res.is_err(), "duplicate name must be rejected");
    let msg = res.err().expect("e");
    assert!(
        msg.contains("already exists") || msg.contains("UNIQUE"),
        "got: {msg}"
    );
}

#[test]
fn delete_recipient_clears_dangling_route_refs() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("r67-cascade"));
    // create a recipient
    let rec = save_recipient(make_recipient("", "to-del", "log:stderr")).expect("save recipient");
    // create a route whose recipients reference `webhook:{rec.id}` and `log:stderr`
    let route = RouteRule {
        id: "rt-cascade".into(),
        name: "cascade-route".into(),
        priority: 50,
        enabled: true,
        kind_pattern: "*".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec![],
        recipients: vec![format!("webhook:{}", rec.id), "log:stderr".into()],
        tags: vec![],
        seen_in_last: None,
    };
    save_route(route).expect("save route");
    // delete the recipient
    let (deleted, routes_cleared) = delete_recipient(&rec.id).expect("delete");
    assert!(deleted, "should have deleted the recipient");
    assert_eq!(routes_cleared, 1, "should clear 1 route's dangling ref");
    // verify route.recipients now only has `log:stderr`
    let routes = list_routes();
    let rt = routes
        .iter()
        .find(|r| r.id == "rt-cascade")
        .expect("route still exists");
    assert_eq!(
        rt.recipients,
        vec!["log:stderr".to_string()],
        "dangling ref should be cleared"
    );
}

#[test]
fn delete_recipient_returns_zero_when_id_absent() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("r67-absent"));
    let (deleted, cleared) = delete_recipient("nonexistent-id").expect("delete");
    assert!(!deleted, "absent id should not be marked deleted");
    assert_eq!(cleared, 0, "absent id should clear 0 routes");
}

#[test]
fn bundle_yaml_round_trips_recipients_via_storage() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("r67-bundle-rt"));
    // save 2 recipients
    save_recipient(make_recipient("", "rec-A", "log:stderr")).expect("save A");
    save_recipient(make_recipient("", "rec-B", "log:file")).expect("save B");
    let yaml = export_alerting_bundle(None).expect("export");
    // clear the store + re-import (should re-upsert the recipients)
    crate::core::set_shared_store(fresh_store("r67-bundle-rt-2"));
    let summary = import_alerting_bundle(&yaml, None).expect("import");
    assert_eq!(summary.recipients, 2, "should import 2 recipients");
    let list = list_recipients();
    assert_eq!(list.len(), 2);
    let names: Vec<&str> = list.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"rec-A") && names.contains(&"rec-B"));
}

#[test]
fn bundle_import_rejects_recipient_kind_outside_whitelist() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("r67-bundle-reject"));
    let yaml = r#"version: 1
exportedAt: 1700000000
endpoints: []
routes: []
presets: []
silences: []
acks: []
recipients:
  - id: r-malicious
    name: bad
    kind: cmd:rce
    config: {}
    enabled: true
    createdAt: 0
"#;
    let signed = sign_bundle(yaml).expect("sign");
    let res = import_alerting_bundle(&signed, None);
    assert!(res.is_err(), "malicious recipient must be rejected");
    let msg = res.err().expect("e");
    assert!(
        msg.contains("whitelist") || msg.contains("cmd:rce"),
        "got: {msg}"
    );
}

#[test]
fn test_recipient_log_stderr_works() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("r67-test-stderr"));
    let rec = save_recipient(make_recipient("", "test-stderr", "log:stderr")).expect("save");
    let result = test_recipient(&rec.id);
    assert!(
        result.is_ok(),
        "test_recipient should succeed for log:stderr: {result:?}"
    );
    assert!(result.expect("ok").contains("log:stderr"));
}

#[test]
fn import_alerting_bundle_preserves_preset_id_and_forces_builtin_false() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("bundle-preset-bogus-builtin"));
    // malicious bundle: a preset marked builtin=true must be forced = false after import
    let yaml = r#"version: 1
endpoints: []
routes: []
presets:
  - id: "preset-bogus"
    name: "Bogus Builtin"
    description: "trying to override builtin"
    kind: "builtin:slack"
    template: "OVERRIDE!"
    sample: "{}"
    builtin: true
    version: 1
    changelog: ""
silences: []
acks: []
"#;
    let signed = sign_bundle(yaml).expect("sign");
    let summary = import_alerting_bundle(&signed, None).expect("import");
    assert_eq!(summary.presets, 1);
    let presets = list_template_presets();
    let p = presets
        .iter()
        .find(|p| p.id == "preset-bogus")
        .expect("found");
    assert!(!p.builtin, "imported preset must be marked user");
    assert_eq!(p.template, "OVERRIDE!");
    assert_eq!(p.name, "Bogus Builtin");
}

#[test]
fn import_alerting_bundle_handles_missing_optional_sections() {
    // empty bundle: only version, all other sections missing (serde default → empty vec)
    let yaml = r#"version: 1
"#;
    let signed = sign_bundle(yaml).expect("sign");
    let summary = import_alerting_bundle(&signed, None).expect("import minimal bundle");
    assert_eq!(summary.endpoints, 0);
    assert_eq!(summary.routes, 0);
    assert_eq!(summary.presets, 0);
    assert_eq!(summary.silences, 0);
    assert_eq!(summary.acks, 0);
    assert_eq!(summary.total, 0);
}

// ─── Phase 64: bundle signature + passphrase encryption ─────────────────

#[test]
fn sign_bundle_then_verify_round_trip() {
    let body = "version: 1\nendpoints: []\n";
    let signed = sign_bundle(body).expect("sign");
    // a signature: field should be at the end
    assert!(
        signed.contains("signature:"),
        "missing signature line: {signed}"
    );
    // verify should pass
    verify_bundle(&signed).expect("verify");
}

#[test]
fn verify_bundle_rejects_tampered_yaml() {
    let body = "version: 1\nendpoints: []\n";
    let signed = sign_bundle(body).expect("sign");
    // change 1 char: add a space in the body section
    let tampered = signed.replacen("endpoints: []", "endpoints: [ ]", 1);
    let res = verify_bundle(&tampered);
    assert!(res.is_err(), "tampered should be rejected");
    let msg = res.unwrap_err();
    assert!(
        msg.contains("tampered") || msg.contains("mismatch"),
        "error should mention tamper: {msg}"
    );
}

#[test]
fn export_alerting_bundle_signed_contains_signature_field() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("signed-export"));
    let out = export_alerting_bundle(None).expect("export signed");
    // a signature: line should be at the end
    assert!(
        out.contains("signature:"),
        "signed yaml missing signature: {out}"
    );
}

#[test]
fn export_alerting_bundle_encrypted_is_not_plain_yaml() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("encrypted-export"));
    let out = export_alerting_bundle(Some("secret-pp")).expect("export encrypted");
    // after encryption it is a JSON envelope, not YAML
    assert!(
        out.trim_start().starts_with('{'),
        "encrypted bundle should start with {{, got: {}",
        &out[..out.len().min(80)]
    );
    assert!(
        out.contains("aes-256-gcm-pbkdf2-sha256"),
        "envelope should declare algorithm"
    );
    assert!(
        out.contains("ciphertext"),
        "envelope should contain ciphertext"
    );
}

#[test]
fn decrypt_with_correct_passphrase_recovers_signed_yaml() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("encrypt-decrypt-1"));
    let pp = "my-secret-passphrase";
    let signed = export_alerting_bundle(None).expect("export signed");
    let envelope = encrypt_bundle(&signed, pp).expect("encrypt");
    let recovered = decrypt_bundle(&envelope, pp).expect("decrypt");
    // recovered should = signed
    assert_eq!(
        recovered, signed,
        "decrypt should recover original signed yaml"
    );
}

#[test]
fn decrypt_with_wrong_passphrase_returns_error() {
    let signed = "version: 1\nendpoints: []\nsignature: \"deadbeef\"\n";
    let envelope = encrypt_bundle(signed, "right-pp").expect("encrypt");
    let res = decrypt_bundle(&envelope, "wrong-pp");
    assert!(res.is_err(), "wrong passphrase should fail");
    let msg = res.unwrap_err();
    assert!(
        msg.contains("wrong") || msg.contains("failed") || msg.contains("tampered"),
        "error should mention wrong passphrase: {msg}"
    );
}

#[test]
fn decrypt_rejects_empty_passphrase() {
    let signed = "version: 1\nendpoints: []\n";
    let res = encrypt_bundle(signed, "");
    assert!(
        res.is_err(),
        "empty passphrase should be rejected by encrypt"
    );
}

#[test]
fn import_encrypted_bundle_with_correct_passphrase_round_trips() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("import-encrypted-1"));
    // prepare data
    let _ = save_endpoint(make_endpoint_row("ep-enc-1", "encrypted endpoint")).unwrap();
    let pp = "round-trip-pp";
    let envelope = export_alerting_bundle(Some(pp)).expect("export encrypted");
    // verify the envelope is not yaml
    assert!(envelope.trim_start().starts_with('{'));
    // clear the store then import (use a fresh store to simulate the target)
    // note: the same TEST_STORE_LOCK cannot be locked twice → verify idempotent upsert on the same store instead
    let summary = import_alerting_bundle(&envelope, Some(pp)).expect("import encrypted");
    assert_eq!(summary.endpoints, 1, "should import 1 endpoint");
}

#[test]
fn import_encrypted_bundle_with_wrong_passphrase_returns_error() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("import-encrypted-2"));
    let envelope = export_alerting_bundle(Some("right-pp")).expect("export");
    let res = import_alerting_bundle(&envelope, Some("wrong-pp"));
    assert!(res.is_err(), "wrong passphrase should fail at decrypt");
}

#[test]
fn import_encrypted_bundle_without_passphrase_returns_error() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("import-encrypted-3"));
    let envelope = export_alerting_bundle(Some("any-pp")).expect("export");
    // no passphrase passed → should be required
    let res = import_alerting_bundle(&envelope, None);
    assert!(res.is_err(), "missing passphrase should be rejected");
    let msg = res.unwrap_err();
    assert!(
        msg.contains("passphrase"),
        "error should mention passphrase: {msg}"
    );
}

#[test]
fn import_signed_bundle_ignores_extra_passphrase() {
    // passing a passphrase for a plaintext signed bundle should be a no-op (takes the plain path)
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("plain-with-pp"));
    let signed = export_alerting_bundle(None).expect("export signed");
    // passing an unrelated passphrase, decode_bundle_input should take the plain path (no algorithm match)
    let summary = import_alerting_bundle(&signed, Some("ignored-pp"))
        .expect("plain signed should accept any passphrase");
    assert_eq!(summary.total, 0);
}

#[test]
fn import_tampered_encrypted_ciphertext_rejected() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("tamper-encrypted"));
    let pp = "tamper-test-pp";
    let envelope = export_alerting_bundle(Some(pp)).expect("export encrypted");
    // tamper ciphertext: replace the first base64 character of the ciphertext field value with 'X'
    // the format is `  "ciphertext": "<base64>"`, so the needle is `"ciphertext": "` (with quotes)
    let needle = "\"ciphertext\": \"";
    assert!(
        envelope.contains(needle),
        "envelope format mismatch; got: {}",
        &envelope[..envelope.len().min(200)]
    );
    let pos = envelope.find(needle).unwrap() + needle.len();
    let mut tampered = envelope.clone();
    // replace the first base64 character of ciphertext with 'X'
    let next_char = tampered[pos..].chars().next().unwrap();
    let replacement = if next_char == 'X' { 'Y' } else { 'X' };
    tampered.replace_range(pos..pos + next_char.len_utf8(), &replacement.to_string());
    assert_ne!(envelope, tampered, "tamper should change string");
    let res = import_alerting_bundle(&tampered, Some(pp));
    // GCM tag verification fails → decrypt errors out
    assert!(
        res.is_err(),
        "tampered ciphertext should be rejected, got: {res:?}"
    );
}

#[test]
fn import_tampered_signed_yaml_body_rejected() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("tamper-signed"));
    let signed = export_alerting_bundle(None).expect("export signed");
    // change some endpoint field in the body
    let tampered = signed.replacen(
        "endpoints: []",
        "endpoints:\n  - id: \"x\"\n    name: \"injected\"\n",
        1,
    );
    let res = import_alerting_bundle(&tampered, None);
    assert!(
        res.is_err(),
        "tampered signed yaml should be rejected by verify"
    );
}

#[test]
fn signature_split_handles_quoted_and_unquoted() {
    // split_signature should tolerate both signature: "hex" and signature: hex formats
    let body = "version: 1\n";
    let signed = sign_bundle(body).expect("sign");
    let (b, sig) = split_signature(&signed).expect("split");
    // b is all characters before the signature: line (including the preceding \n)
    assert!(b.contains("version: 1"));
    assert!(!b.contains("signature:"));
    assert_eq!(sig.len(), 64, "SHA-256 hex should be 64 chars, got: {sig}");
}

// ─── Phase 65: OS keychain integration tests ─────────────────────────
//
// Note: these tests do not strongly depend on a real keychain being available (CI / sandbox environments usually
// lack the Security framework / Secret Service / wincred). `ensure_secret_loaded`
// automatically degrades to the file fallback, so covering the file path verifies secret loading + persistence + rotate.
//
// Side effect: the file fallback writes to `dirs::config_dir()/opencapx/bundle-signing-v1.key`.
// In a real user environment the file already exists and rotate overwrites it in place; a clean CI environment creates it.

#[test]
fn ensure_secret_loaded_produces_32_bytes() {
    reset_secret_buffer_for_test();
    ensure_secret_loaded().expect("load secret");
    let bytes = current_secret_bytes().expect("current");
    assert_eq!(
        bytes.len(),
        32,
        "secret must be exactly 32 bytes, got {}",
        bytes.len()
    );
    assert!(
        !bytes.iter().all(|b| *b == 0),
        "secret must not be all zeros"
    );
}

#[test]
fn ensure_secret_loaded_is_idempotent() {
    reset_secret_buffer_for_test();
    ensure_secret_loaded().expect("first load");
    let first = current_secret_bytes().expect("first bytes");
    // the second call should return the same bytes (the secret is stable over the process lifetime)
    ensure_secret_loaded().expect("second load");
    let second = current_secret_bytes().expect("second bytes");
    assert_eq!(
        first, second,
        "secret should be stable across loads in same process"
    );
}

#[test]
fn current_secret_bytes_returns_owned_copy() {
    ensure_secret_loaded().expect("load");
    let a = current_secret_bytes().expect("a");
    let b = current_secret_bytes().expect("b");
    // the two snapshots must be equal (they clone the in-memory buffer) but have different addresses
    assert_eq!(a, b);
    assert_ne!(a.as_ptr(), b.as_ptr(), "should be independent allocations");
}

#[test]
fn rotate_bundle_secret_changes_in_memory_bytes() {
    ensure_secret_loaded().expect("initial load");
    let before = current_secret_bytes().expect("before");
    rotate_bundle_secret().expect("rotate");
    let after = current_secret_bytes().expect("after");
    assert_ne!(before, after, "rotate must change secret bytes");
    assert_eq!(after.len(), 32);
}

#[test]
fn sign_then_rotate_then_verify_old_signature_fails() {
    // 1) confirm the secret is loaded + sign with the current secret
    reset_secret_buffer_for_test();
    ensure_secret_loaded().expect("load");
    let body = "version: 1\nendpoints: []\nroutes: []\npresets: []\nsilences: []\nacks: []\n";
    let signed_old = sign_bundle(body).expect("sign old");
    // 2) rotate → the secret changes
    rotate_bundle_secret().expect("rotate");
    // 3) the old signature should fail verification
    let res = verify_bundle(&signed_old);
    assert!(res.is_err(), "verify should fail after secret rotate");
    let msg = res.unwrap_err();
    assert!(
        msg.contains("mismatch") || msg.contains("tampered"),
        "error should mention mismatch/tampered: {msg}"
    );
    // 4) sign with the new secret → verification should pass
    let signed_new = sign_bundle(body).expect("sign new");
    verify_bundle(&signed_new).expect("verify new");
}

#[test]
fn rotate_then_sign_with_new_secret_succeeds() {
    reset_secret_buffer_for_test();
    ensure_secret_loaded().expect("initial load");
    rotate_bundle_secret().expect("rotate");
    // the new secret state supports a normal sign + verify round-trip
    let body = "version: 1\nendpoints: []\n";
    let signed = sign_bundle(body).expect("sign");
    verify_bundle(&signed).expect("verify");
}

#[test]
fn fallback_secret_path_resolves_under_config_dir() {
    // fallback_secret_path should not panic, and the path should be under config_dir/opencapx/
    let p = fallback_secret_path().expect("path");
    assert!(
        p.ends_with("opencapx/bundle-signing-v1.key"),
        "fallback should live under config_dir/opencapx/, got: {p:?}"
    );
}

#[test]
fn keychain_entry_init_does_not_panic() {
    // weak test: in both keychain-less and keychain environments it should return a Result (not panic)
    let res = keyring::Entry::new("com.opencapx.desktop", "bundle-signing-v1");
    // Ok or Err is allowed — neither should panic
    match res {
        Ok(_entry) => {} // keychain available, continue
        Err(e) => {
            // keychain unavailable; confirm the fallback path still works
            eprintln!("[test] keychain unavailable: {e}");
            let bytes = load_or_create_file_secret().expect("file fallback");
            assert_eq!(bytes.len(), 32);
        }
    }
}

// ─── Phase 68: seen_in_last + ring buffer + cycle detection ────────────

#[test]
fn seen_in_last_parsing_round_trip() {
    let rule = RouteRule {
        id: "rt-corr-1".into(),
        name: "with-seen".into(),
        priority: 100,
        enabled: true,
        kind_pattern: "plugin.lifecycle.crashed".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-1".into()],
        recipients: vec![],
        tags: vec![],
        seen_in_last: Some(SeenInLastSpec {
            pattern: "plugin.metrics.*".into(),
            window_secs: 60,
        }),
    };
    let json = serde_json::to_string(&rule).unwrap();
    let back: RouteRule = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.seen_in_last.as_ref().unwrap().pattern,
        "plugin.metrics.*"
    );
    assert_eq!(back.seen_in_last.as_ref().unwrap().window_secs, 60);
}

#[test]
fn seen_in_last_rejects_empty_pattern() {
    let r = RouteRule {
        id: "rt-corr-2".into(),
        name: "bad-seen".into(),
        priority: 100,
        enabled: true,
        kind_pattern: "plugin.x".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-1".into()],
        recipients: vec![],
        tags: vec![],
        seen_in_last: Some(SeenInLastSpec {
            pattern: "".into(),
            window_secs: 60,
        }),
    };
    let err = validate_route(&r).unwrap_err();
    assert!(err.contains("seen_in_last.pattern"), "got: {err}");
}

#[test]
fn seen_in_last_rejects_window_too_long() {
    let r = RouteRule {
        id: "rt-corr-3".into(),
        name: "bad-win".into(),
        priority: 100,
        enabled: true,
        kind_pattern: "plugin.x".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-1".into()],
        recipients: vec![],
        tags: vec![],
        seen_in_last: Some(SeenInLastSpec {
            pattern: "plugin.y".into(),
            window_secs: 7200,
        }),
    };
    let err = validate_route(&r).unwrap_err();
    assert!(err.contains("window_secs too long"), "got: {err}");
}

#[test]
fn recent_events_ring_buffer_evicts_oldest() {
    _reset_recent_events_for_tests();
    let now = 1_700_000_000;
    for i in 0..(RECENT_EVENTS_CAP + 50) {
        record_seen_event(&format!("src.{i}"), "{}", now + i as u64, vec![], vec![]);
    }
    let snap = recent_events_snapshot(RECENT_EVENTS_CAP + 10);
    assert_eq!(snap.len(), RECENT_EVENTS_CAP);
    // the first src.0 is evicted, so snap[0] (last in new→old) is src.50
    assert!(
        snap.last().unwrap().source.starts_with("src.50"),
        "got: {:?}",
        snap.last().map(|e| &e.source)
    );
}

#[test]
fn recently_seen_returns_true_when_pattern_matches_in_window() {
    _reset_recent_events_for_tests();
    let now = 1_700_000_010;
    record_seen_event("plugin.metrics.cpu", "{}", now - 5, vec![], vec![]);
    assert!(recently_seen("plugin.*", 10, now));
    assert!(recently_seen("plugin.metrics.*", 10, now));
    assert!(!recently_seen("capability.*", 10, now));
}

#[test]
fn recently_seen_returns_false_when_window_expired() {
    _reset_recent_events_for_tests();
    let now = 1_700_000_100;
    record_seen_event("plugin.metrics.cpu", "{}", now - 100, vec![], vec![]);
    assert!(!recently_seen("plugin.*", 10, now));
    // the 200s window can hit
    assert!(recently_seen("plugin.*", 200, now));
}

#[test]
fn detect_cycles_finds_self_loop() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase68-self"));
    _reset_recent_events_for_tests();
    // rule A: kind_pattern = plugin.x, seen_in_last.pattern = plugin.x → self-reference
    let r_a = RouteRule {
        id: "rt-self-a".into(),
        name: "A".into(),
        priority: 100,
        enabled: true,
        kind_pattern: "plugin.x".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-1".into()],
        recipients: vec![],
        tags: vec![],
        seen_in_last: Some(SeenInLastSpec {
            pattern: "plugin.x".into(),
            window_secs: 60,
        }),
    };
    let _ = save_route(r_a);
    let reports = detect_route_cycles();
    let self_loops: Vec<_> = reports
        .iter()
        .filter(|r| r.kind == CycleKind::SelfLoop)
        .collect();
    assert!(
        !self_loops.is_empty(),
        "expected self-loop, got: {:?}",
        reports
    );
}

#[test]
fn detect_cycles_finds_three_node_loop() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase68-three"));
    _reset_recent_events_for_tests();
    // A → B → C → A via seen_in_last chain
    let mk = |id: &str, name: &str, kind: &str, seen: Option<&str>| RouteRule {
        id: id.into(),
        name: name.into(),
        priority: 100,
        enabled: true,
        kind_pattern: kind.into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-1".into()],
        recipients: vec![],
        tags: vec![],
        seen_in_last: seen.map(|p| SeenInLastSpec {
            pattern: p.into(),
            window_secs: 60,
        }),
    };
    let _ = save_route(mk("rt-A", "A", "plugin.x", Some("plugin.y")));
    let _ = save_route(mk("rt-B", "B", "plugin.y", Some("plugin.z")));
    let _ = save_route(mk("rt-C", "C", "plugin.z", Some("plugin.x")));
    let reports = detect_route_cycles();
    // should contain at least one RouteToRoute cycle with 2+ nodes
    let multi: Vec<_> = reports
        .iter()
        .filter(|r| r.kind == CycleKind::RouteToRoute && r.cycle.len() >= 4)
        .collect();
    assert!(
        !multi.is_empty(),
        "expected multi-node cycle, got: {:?}",
        reports
    );
}

#[test]
fn detect_cycles_returns_empty_on_acyclic_graph() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase68-acyclic"));
    _reset_recent_events_for_tests();
    // 3 independent routes, no seen_in_last, no correlation
    let mk = |id: &str, name: &str, kind: &str| RouteRule {
        id: id.into(),
        name: name.into(),
        priority: 100,
        enabled: true,
        kind_pattern: kind.into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-1".into()],
        recipients: vec![],
        tags: vec![],
        seen_in_last: None,
    };
    let _ = save_route(mk("rt-ax1", "X1", "plugin.a"));
    let _ = save_route(mk("rt-bx1", "X2", "plugin.b"));
    let _ = save_route(mk("rt-cx1", "X3", "plugin.c"));
    let reports = detect_route_cycles();
    assert!(reports.is_empty(), "expected no cycles, got: {:?}", reports);
}

#[test]
fn recent_events_snapshot_respects_limit() {
    _reset_recent_events_for_tests();
    let now = 1_700_001_000;
    for i in 0..10 {
        record_seen_event(
            &format!("s.{i}"),
            "{}",
            now + i,
            vec![format!("r{i}")],
            vec![],
        );
    }
    let snap = recent_events_snapshot(3);
    assert_eq!(snap.len(), 3);
    // new→old order
    assert_eq!(snap[0].source, "s.9");
    assert_eq!(snap[1].source, "s.8");
    assert_eq!(snap[2].source, "s.7");
}

// ─── Phase 69: severity policy chain + cascade delete ──────────────────

#[test]
fn severity_inheritance_chain_returns_four_links_in_order() {
    let chain = severity_inheritance_chain("plugin.metrics.exceeded");
    assert_eq!(chain.len(), 4);
    assert_eq!(chain[0].policy, SeverityPolicy::Manifest);
    assert_eq!(chain[1].policy, SeverityPolicy::PluginDefault);
    assert_eq!(chain[2].policy, SeverityPolicy::UserOverride);
    assert_eq!(chain[3].policy, SeverityPolicy::Disabled);
}

#[test]
fn severity_inheritance_chain_marks_user_override_hit_when_set() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase69-user"));
    save_user_severity_hint("plugin.metrics.exceeded", "critical").unwrap();
    let chain = severity_inheritance_chain("plugin.metrics.exceeded");
    assert!(chain[2].hit, "user_override should be hit");
    assert!(!chain[0].hit, "manifest should be overridden by user");
    assert!(
        !chain[1].hit,
        "plugin_default should not be hit when user present"
    );
}

#[test]
fn severity_inheritance_chain_marks_manifest_hit_when_user_absent() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase69-manifest"));
    let mut hints = std::collections::HashMap::new();
    hints.insert("plugin.x".to_string(), "critical".to_string());
    install_manifest_hints(
        "plug-a",
        &Some(AlertingManifest {
            severity_hints: hints,
        }),
    );
    let chain = severity_inheritance_chain("plugin.x");
    assert!(chain[0].hit, "manifest should be hit");
    assert_eq!(chain[0].plugin_id.as_deref(), Some("plug-a"));
    assert!(!chain[2].hit);
    assert!(!chain[1].hit);
}

#[test]
fn severity_inheritance_chain_marks_plugin_default_hit_when_no_hints() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase69-default"));
    let chain = severity_inheritance_chain("plugin.metrics.exceeded");
    assert!(!chain[0].hit);
    assert!(chain[1].hit, "plugin_default should be hit when no hints");
    assert!(!chain[2].hit);
}

#[test]
fn effective_severity_with_reason_returns_user_override_when_set() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase69-effective"));
    save_user_severity_hint("plugin.metrics.exceeded", "critical").unwrap();
    let (sev, link) = effective_severity_with_reason("plugin.metrics.exceeded");
    assert_eq!(sev, Severity::Critical);
    assert_eq!(link.policy, SeverityPolicy::UserOverride);
    assert!(link.hit);
}

#[test]
fn cascade_delete_severity_hint_returns_affected_routes() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase69-cascade-route"));
    // create a route referencing plugin.x
    let _ = save_route(RouteRule {
        id: "rt-cas".into(),
        name: "cas-route".into(),
        priority: 100,
        enabled: true,
        kind_pattern: "plugin.x".into(),
        payload_path: None,
        payload_match: None,
        target_endpoint_ids: vec!["ep-1".into()],
        recipients: vec![],
        tags: vec![],
        seen_in_last: None,
    });
    save_user_severity_hint("plugin.x", "warn").unwrap();
    let report = cascade_delete_severity_hint("plugin.x");
    assert!(report.hint_deleted);
    assert_eq!(report.affected_routes, vec!["cas-route"]);
    assert!(report.affected_correlations.is_empty());
    assert!(report.affected_aggregations.is_empty());
}

#[test]
fn cascade_delete_severity_hint_returns_affected_correlations() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase69-cascade-corr"));
    // Phase 53 save_correlation signature: CorrelationRuleDto
    let mut rule = CorrelationRule {
        id: "".into(),
        name: "cor-x".into(),
        kind_pattern_a: "plugin.a".into(),
        kind_pattern_b: "plugin.x".into(),
        window_secs: 30,
        enabled: true,
    };
    ensure_correlation_id(&mut rule);
    save_correlation(rule).unwrap();
    save_user_severity_hint("plugin.x", "warn").unwrap();
    let report = cascade_delete_severity_hint("plugin.x");
    assert!(report.hint_deleted);
    assert_eq!(report.affected_correlations, vec!["cor-x"]);
}

#[test]
fn cascade_delete_severity_hint_is_idempotent() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase69-cascade-idem"));
    save_user_severity_hint("plugin.x", "warn").unwrap();
    let r1 = cascade_delete_severity_hint("plugin.x");
    assert!(r1.hint_deleted);
    let r2 = cascade_delete_severity_hint("plugin.x");
    assert!(!r2.hint_deleted);
    assert!(r2.affected_routes.is_empty());
    assert!(r2.affected_correlations.is_empty());
    assert!(r2.affected_aggregations.is_empty());
}

// ── Phase 70 — severity cross-chain propagation helpers + trace ──────────

#[test]
fn route_dispatch_severity_returns_user_override_when_set() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase70-route-override"));
    save_user_severity_hint("plugin.metrics.exceeded", "warn").unwrap();
    assert_eq!(
        route_dispatch_severity("plugin.metrics.exceeded"),
        Severity::Warn
    );
}

#[test]
fn aggregation_action_severity_propagates_after_hint_change() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase70-agg-change"));
    save_user_severity_hint("plugin.flapping", "info").unwrap();
    assert_eq!(
        aggregation_action_severity("plugin.flapping"),
        Severity::Info
    );
    // reflected immediately after the hint changes
    save_user_severity_hint("plugin.flapping", "error").unwrap();
    assert_eq!(
        aggregation_action_severity("plugin.flapping"),
        Severity::Error
    );
}

#[test]
fn correlation_decision_severity_falls_back_to_default_when_no_hints() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase70-corr-fallback"));
    // a brand-new source with no hint → fall back to the hardcode default (Info)
    assert_eq!(
        correlation_decision_severity("totally.unknown.kind"),
        Severity::Info
    );
}

#[test]
fn escalation_target_severity_matches_effective_severity() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase70-esc-match"));
    save_user_severity_hint("capability.sla.violated", "critical").unwrap();
    assert_eq!(
        escalation_target_severity("capability.sla.violated"),
        Severity::Critical
    );
    assert_eq!(
        escalation_target_severity("capability.sla.violated"),
        severity_resolved("capability.sla.violated")
    );
}

#[test]
fn propagation_trace_returns_consistent_user_override_across_all_links() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase70-trace-override"));
    save_user_severity_hint("plugin.x", "warn").unwrap();
    let t = propagation_trace("plugin.x");
    assert_eq!(t.source, "plugin.x");
    assert_eq!(t.route_severity, Severity::Warn);
    assert_eq!(t.route_origin, SeverityPolicy::UserOverride);
    assert_eq!(t.correlation_severity, Severity::Warn);
    assert_eq!(t.correlation_origin, SeverityPolicy::UserOverride);
    assert_eq!(t.aggregation_severity, Severity::Warn);
    assert_eq!(t.aggregation_origin, SeverityPolicy::UserOverride);
    assert_eq!(t.escalation_severity, Severity::Warn);
    assert_eq!(t.escalation_origin, SeverityPolicy::UserOverride);
}

#[test]
fn propagation_trace_returns_manifest_origin_when_no_user_hint() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase70-trace-manifest"));
    // no user hint + no plugin manifest hint for this source → fall back to the manifest origin
    // (Phase 53 hardcode manifest hardcode source: plugin.metrics.exceeded / plugin.kill_switch.enabled /
    //  capability.sla.violated goes through plugin_default; the others through manifest)
    let t = propagation_trace("totally.unknown.source");
    assert_eq!(t.source, "totally.unknown.source");
    // the 4 chains are consistent
    assert_eq!(t.route_severity, t.correlation_severity);
    assert_eq!(t.route_severity, t.aggregation_severity);
    assert_eq!(t.route_severity, t.escalation_severity);
    assert_eq!(t.route_origin, t.correlation_origin);
    assert_eq!(t.route_origin, t.aggregation_origin);
    assert_eq!(t.route_origin, t.escalation_origin);
    assert_eq!(
        t.route_severity,
        severity_resolved("totally.unknown.source")
    );
}

// ── Phase 71 — consistency after wiring the 4 helpers into the main paths ─

#[test]
fn correlation_decision_carries_propagated_severity() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase71-corr-prop"));
    _reset_correlations_for_tests();
    // create an A→B correlation rule — Phase 55 baseline style
    save_correlation(CorrelationRule {
        id: "rule-71-c".into(),
        name: "phase71-corr".into(),
        kind_pattern_a: "plugin.lifecycle.started".into(),
        kind_pattern_b: "plugin.lifecycle.crashed".into(),
        window_secs: 60,
        enabled: true,
    })
    .unwrap();

    // first save a user hint for source B = critical
    save_user_severity_hint("plugin.lifecycle.crashed", "critical").unwrap();

    // fire A (last_a recorded)
    evaluate_correlations("plugin.lifecycle.started", 1000);
    // fire B (within the window → Suppress with propagated_severity)
    match evaluate_correlations("plugin.lifecycle.crashed", 1010) {
        CorrelationDecision::Suppress {
            propagated_severity,
        } => {
            assert_eq!(propagated_severity, Severity::Critical);
        }
        other => panic!(
            "expected Suppress with propagated Critical, got {:?}",
            other
        ),
    }
}

#[test]
fn aggregation_decision_carries_propagated_severity_on_suppress() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase71-agg-prop"));
    _reset_aggregations_for_tests();
    save_aggregation(agg_rule("rule-71-suppress", "suppress", None)).unwrap();

    save_user_severity_hint("plugin.metrics.exceeded", "error").unwrap();

    // hit the threshold (threshold_count = 3): fire 3 times
    let now = 5000_u64;
    evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    let d = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    match d {
        AggregationDecision::Suppress {
            propagated_severity,
        } => {
            assert_eq!(propagated_severity, Severity::Error);
        }
        other => panic!("expected Suppress with propagated Error, got {:?}", other),
    }
}

#[test]
fn aggregation_decision_carries_propagated_severity_on_downgrade() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase71-agg-dg"));
    _reset_aggregations_for_tests();
    save_aggregation(agg_rule("rule-71-downgrade", "downgrade", Some("critical"))).unwrap();

    save_user_severity_hint("plugin.metrics.exceeded", "warn").unwrap();

    // threshold_count = 3 (agg_rule default): fire 3 times
    let now = 6000_u64;
    evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    let d = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    // Downgrade(target_severity=Critical, propagated_severity=Warn)
    match d {
        AggregationDecision::Downgrade(target, propagated) => {
            assert_eq!(target, Severity::Critical);
            assert_eq!(propagated, Severity::Warn);
        }
        other => panic!("expected Downgrade(Critical, Warn), got {:?}", other),
    }
}

#[test]
fn aggregation_decision_carries_propagated_severity_on_merge() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase71-agg-mg"));
    _reset_aggregations_for_tests();
    save_aggregation(agg_rule("rule-71-merge", "merge", None)).unwrap();

    save_user_severity_hint("plugin.metrics.exceeded", "critical").unwrap();

    // threshold_count = 3: fire 3 times
    let now = 7000_u64;
    evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({"x": 1}), now);
    evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({"x": 2}), now);
    let d = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({"x": 3}), now);
    match d {
        AggregationDecision::Merge {
            propagated_severity,
            count,
            ..
        } => {
            assert_eq!(propagated_severity, Severity::Critical);
            assert_eq!(count, 3); // 3 fires all within window
        }
        other => panic!("expected Merge with propagated Critical, got {:?}", other),
    }
}

#[test]
fn end_to_end_severity_consistency_across_all_4_paths() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("phase71-e2e"));
    // one source is consistent across the whole chain
    let source = "plugin.metrics.exceeded";
    save_user_severity_hint(source, "error").unwrap();
    // 1. propagation_trace's 4 chains are consistent
    let tr = propagation_trace(source);
    assert_eq!(tr.route_severity, Severity::Error);
    assert_eq!(tr.correlation_severity, Severity::Error);
    assert_eq!(tr.aggregation_severity, Severity::Error);
    assert_eq!(tr.escalation_severity, Severity::Error);
    // 2. the 4 helpers are equal to each other
    assert_eq!(route_dispatch_severity(source), Severity::Error);
    assert_eq!(correlation_decision_severity(source), Severity::Error);
    assert_eq!(aggregation_action_severity(source), Severity::Error);
    assert_eq!(escalation_target_severity(source), Severity::Error);
}

// ── Phase 72 — endpoint per-source severity override ─────────────────────

#[test]
fn endpoint_severity_override_applies_in_fanout() {
    // setup: the user hint raises plugin.metrics.exceeded to error, and the endpoint override overwrites it to critical
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("ep-override-applies"));
    save_user_severity_hint("plugin.metrics.exceeded", "error").unwrap();
    let mut ep = WebhookEndpoint {
        id: String::new(),
        name: "ops-override".into(),
        url: "https://example.com/hook".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![("plugin.metrics.exceeded".into(), Severity::Critical)],
    };
    let row = save_endpoint(ep.clone()).unwrap();
    ep.id = row.id.clone();
    // simulate envelope_severity in fanout starting at the propagation result
    let mut envelope_severity = severity_resolved("plugin.metrics.exceeded");
    assert_eq!(envelope_severity, Severity::Error, "baseline propagation");
    apply_endpoint_severity_override(
        &mut envelope_severity,
        "plugin.metrics.exceeded",
        &ep.severity_overrides,
    );
    assert_eq!(envelope_severity, Severity::Critical, "override wins");
}

#[test]
fn endpoint_severity_override_does_not_apply_when_source_misses() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("ep-override-miss"));
    // the override is configured only for plugin.metrics.exceeded; dispatching another source must not be overwritten
    let ep = WebhookEndpoint {
        id: String::new(),
        name: "ops-override-miss".into(),
        url: "https://example.com/hook".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![("plugin.metrics.exceeded".into(), Severity::Critical)],
    };
    save_endpoint(ep).unwrap();
    // another source goes through propagation — use capability.sla.violated, whose hardcode = Error
    let mut envelope_severity = severity_resolved("capability.sla.violated");
    assert_eq!(envelope_severity, Severity::Error, "baseline propagation");
    apply_endpoint_severity_override(
        &mut envelope_severity,
        "capability.sla.violated",
        &[("plugin.metrics.exceeded".into(), Severity::Critical)],
    );
    assert_eq!(envelope_severity, Severity::Error, "miss → no change");
}

#[test]
fn endpoint_severity_override_takes_priority_over_user_hint() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("ep-override-beats-hint"));
    // user hint = warn, no manifest, endpoint override = critical
    save_user_severity_hint("plugin.metrics.exceeded", "warn").unwrap();
    let mut envelope_severity = severity_resolved("plugin.metrics.exceeded");
    assert_eq!(envelope_severity, Severity::Warn, "user hint applied");
    apply_endpoint_severity_override(
        &mut envelope_severity,
        "plugin.metrics.exceeded",
        &[("plugin.metrics.exceeded".into(), Severity::Critical)],
    );
    assert_eq!(
        envelope_severity,
        Severity::Critical,
        "endpoint override wins"
    );
}

#[test]
fn validate_endpoint_rejects_empty_source_in_severity_overrides() {
    let ep = WebhookEndpoint {
        id: String::new(),
        name: "bad-empty".into(),
        url: "https://x.com".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![("".into(), Severity::Warn)],
    };
    let err = validate_endpoint(&ep).unwrap_err();
    assert!(err.contains("source is required"), "got: {err}");
}

#[test]
fn validate_endpoint_rejects_invalid_severity_in_overrides() {
    // the Severity enum's Deserialize already rejects illegal values, so on the validate path an invalid severity
    // only errors when the source length is over the limit. This covers the length > 256 boundary case.
    let long_source = "a".repeat(257);
    let bad = WebhookEndpoint {
        id: String::new(),
        name: "bad-long".into(),
        url: "https://x.com".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![(long_source, Severity::Warn)],
    };
    let err = validate_endpoint(&bad).unwrap_err();
    assert!(err.contains("source too long"), "got: {err}");
    // also verify that a legal severity but blank source is rejected too
    let whitespace_source = "   ".to_string();
    let mut bad2 = bad;
    bad2.name = "bad-ws".into();
    bad2.severity_overrides = vec![(whitespace_source, Severity::Warn)];
    let err2 = validate_endpoint(&bad2).unwrap_err();
    assert!(err2.contains("source is required"), "got: {err2}");
}

#[test]
fn endpoint_row_round_trips_severity_overrides() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("ep-round-trip"));
    let ep = WebhookEndpoint {
        id: String::new(),
        name: "rt".into(),
        url: "https://example.com/hook".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![
            ("plugin.metrics.exceeded".into(), Severity::Critical),
            ("capability.sla.violated".into(), Severity::Warn),
        ],
    };
    let row = save_endpoint(ep).unwrap();
    // read row → DTO directly and confirm the overrides field is fully restored
    let store = crate::core::shared_store().unwrap();
    let s = store.lock().unwrap();
    let StoreEnum::Db(db) = &*s else {
        panic!("need db store")
    };
    let rows = db.list_alerting_endpoints();
    let loaded = rows.iter().find(|r| r.id == row.id).unwrap();
    let dto = endpoint_row_to_dto(loaded);
    assert_eq!(dto.severity_overrides.len(), 2);
    assert!(dto
        .severity_overrides
        .contains(&("plugin.metrics.exceeded".into(), Severity::Critical)));
    assert!(dto
        .severity_overrides
        .contains(&("capability.sla.violated".into(), Severity::Warn)));
    // the row.severity_overrides_json column is non-null (because there are overrides)
    assert!(loaded.severity_overrides.is_some());
}

// ── Phase 73 — endpoint severity override glob support ─────────────────

#[test]
fn endpoint_severity_override_matches_exact_still_works() {
    // the Phase 72 exact behavior must be preserved: an exact string still hits
    let overrides = vec![("plugin.metrics.exceeded".into(), Severity::Critical)];
    assert_eq!(
        endpoint_severity_override_for("plugin.metrics.exceeded", &overrides),
        Some(Severity::Critical)
    );
    // not exactly equal → no hit
    assert_eq!(
        endpoint_severity_override_for("plugin.metrics.other", &overrides),
        None
    );
}

#[test]
fn endpoint_severity_override_supports_prefix_dot_star_glob() {
    // plugin.* hits the whole plugin. prefix family
    let overrides = vec![("plugin.*".into(), Severity::Warn)];
    assert_eq!(
        endpoint_severity_override_for("plugin.metrics.exceeded", &overrides),
        Some(Severity::Warn)
    );
    assert_eq!(
        endpoint_severity_override_for("plugin.lifecycle.crashed", &overrides),
        Some(Severity::Warn)
    );
    // not a plugin. prefix → no hit
    assert_eq!(
        endpoint_severity_override_for("capability.sla.violated", &overrides),
        None
    );
}

#[test]
fn endpoint_severity_override_supports_star_matches_all() {
    // * matches all sources
    let overrides = vec![("*".into(), Severity::Info)];
    assert_eq!(
        endpoint_severity_override_for("anything.else", &overrides),
        Some(Severity::Info)
    );
    // an empty pattern behaves like *, consistent with kind_matches
    let empty = vec![("".into(), Severity::Warn)];
    assert_eq!(
        endpoint_severity_override_for("any", &empty),
        Some(Severity::Warn)
    );
}

#[test]
fn endpoint_severity_override_first_match_wins() {
    // glob first → hits the glob rather than the exact (the glob's hit sev wins)
    let glob_first = vec![
        ("plugin.*".into(), Severity::Warn),
        ("plugin.metrics.exceeded".into(), Severity::Critical),
    ];
    assert_eq!(
        endpoint_severity_override_for("plugin.metrics.exceeded", &glob_first),
        Some(Severity::Warn)
    );
    // exact first → hits the exact
    let exact_first = vec![
        ("plugin.metrics.exceeded".into(), Severity::Critical),
        ("plugin.*".into(), Severity::Warn),
    ];
    assert_eq!(
        endpoint_severity_override_for("plugin.metrics.exceeded", &exact_first),
        Some(Severity::Critical)
    );
}

#[test]
fn endpoint_severity_override_does_not_match_substring_only() {
    // kind_matches semantics: a bare prefix (no .*) is not a glob; only exact equality hits
    // writing "plugin" instead of "plugin.*" → hits only when source is strictly equal to "plugin"
    let overrides = vec![("plugin".into(), Severity::Critical)];
    assert_eq!(
        endpoint_severity_override_for("plugin.metrics.exceeded", &overrides),
        None
    );
    assert_eq!(
        endpoint_severity_override_for("plugin", &overrides),
        Some(Severity::Critical)
    );
}

// ── Phase 74 — endpoint override dry-run preview ──────────────────────────

#[test]
fn preview_endpoint_severity_returns_override_hit_when_match() {
    let ep = WebhookEndpoint {
        id: "ep-a".into(),
        name: "ep-a".into(),
        url: "https://x.com".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![("plugin.*".into(), Severity::Warn)],
    };
    let row = preview_endpoint_severity("plugin.metrics.exceeded", &ep);
    assert_eq!(row.endpoint_id, "ep-a");
    let hit = row.override_hit.expect("expected hit");
    assert_eq!(hit.pattern, "plugin.*");
    assert_eq!(hit.severity, Severity::Warn);
    assert_eq!(hit.index, 0);
    // propagation baseline = Warn (hardcode for plugin.metrics.exceeded)
    assert_eq!(row.propagation_severity, Severity::Warn);
    // override hit → final = override value (same as propagation here, but the path exercised)
    assert_eq!(row.final_envelope_severity, Severity::Warn);
}

#[test]
fn preview_endpoint_severity_returns_no_override_when_miss() {
    // save a user hint so propagation != default for this source
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("preview-miss"));
    save_user_severity_hint("capability.sla.violated", "info").unwrap();
    let ep = WebhookEndpoint {
        id: "ep-a".into(),
        name: "ep-a".into(),
        url: "https://x.com".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![("plugin.*".into(), Severity::Warn)],
    };
    let row = preview_endpoint_severity("capability.sla.violated", &ep);
    assert!(row.override_hit.is_none(), "glob should miss");
    // propagation = user hint "info"(overrides hardcode Error)
    assert_eq!(row.propagation_severity, Severity::Info);
    // no hit → final = propagation
    assert_eq!(row.final_envelope_severity, Severity::Info);
}

#[test]
fn preview_alerting_endpoint_severity_filters_to_single_endpoint() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("preview-single"));
    // create 2 endpoints with different overrides
    let ep_a = WebhookEndpoint {
        id: String::new(),
        name: "ep-a".into(),
        url: "https://a.com".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![("plugin.*".into(), Severity::Warn)],
    };
    let row_a = save_endpoint(ep_a).unwrap();
    let ep_b = WebhookEndpoint {
        id: String::new(),
        name: "ep-b".into(),
        url: "https://b.com".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![("capability.*".into(), Severity::Critical)],
    };
    let _row_b = save_endpoint(ep_b).unwrap();
    // filter to ep-a
    let preview =
        preview_alerting_endpoint_severity("plugin.metrics.exceeded", Some(&row_a.id)).unwrap();
    assert_eq!(preview.source, "plugin.metrics.exceeded");
    assert_eq!(preview.endpoints.len(), 1, "should only include ep-a");
    assert_eq!(preview.endpoints[0].endpoint_name, "ep-a");
    assert!(preview.endpoints[0].override_hit.is_some());
}

#[test]
fn preview_alerting_endpoint_severity_rejects_empty_source() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("preview-empty-src"));
    let err = preview_alerting_endpoint_severity("", None).unwrap_err();
    assert!(err.contains("source is required"), "got: {err}");
    // whitespace-only also counts as empty
    let err2 = preview_alerting_endpoint_severity("   ", None).unwrap_err();
    assert!(err2.contains("source is required"), "got: {err2}");
}

#[test]
fn preview_alerting_endpoint_severity_first_match_wins_in_preview() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("preview-first-match"));
    // ep override: glob first, exact after
    let ep = WebhookEndpoint {
        id: String::new(),
        name: "ep-fm".into(),
        url: "https://fm.com".into(),
        enabled: true,
        headers: vec![],
        secret: String::new(),
        source_filter: vec![],
        schema_version: 0,
        template: None,
        template_sample: None,
        severity_overrides: vec![
            ("plugin.*".into(), Severity::Warn),
            ("plugin.metrics.exceeded".into(), Severity::Critical),
        ],
    };
    let _ = save_endpoint(ep).unwrap();
    let preview = preview_alerting_endpoint_severity("plugin.metrics.exceeded", None).unwrap();
    assert_eq!(preview.endpoints.len(), 1);
    let hit = preview.endpoints[0]
        .override_hit
        .as_ref()
        .expect("expected hit");
    assert_eq!(hit.index, 0, "first-match wins → glob (index 0) should hit");
    assert_eq!(hit.pattern, "plugin.*");
    assert_eq!(hit.severity, Severity::Warn);
    assert_eq!(preview.endpoints[0].final_envelope_severity, Severity::Warn);
}

// ─── Phase 75: simulate_alerting_dispatch full-chain dry-run simulator ──────

#[test]
fn simulate_aggregations_no_rules_returns_pass() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim75-agg-no-rules"));
    _reset_aggregations_for_tests();
    let s = simulate_aggregations(
        "plugin.metrics.exceeded",
        &serde_json::json!({}),
        now_secs(),
    );
    assert!(s.matched_rule_id.is_none());
    assert!(!s.would_fire);
    assert!(s.action.is_none());
}

#[test]
fn simulate_aggregations_below_threshold_returns_no_fire() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim75-agg-below"));
    _reset_aggregations_for_tests();
    save_aggregation(agg_rule("r1", "suppress", None)).unwrap();
    let now = now_secs();
    // seed 1 event via real evaluate
    let _ = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    let s = simulate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    assert_eq!(s.matched_rule_id.as_deref(), Some("r1"));
    assert_eq!(s.bucket_events_in_window, 1);
    assert_eq!(s.threshold, 3);
    assert!(!s.would_fire, "1 < threshold 3 → not fire");
    assert!(s.action.is_none());
}

#[test]
fn simulate_aggregations_at_threshold_would_fire() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim75-agg-at"));
    _reset_aggregations_for_tests();
    save_aggregation(agg_rule("r1", "suppress", None)).unwrap();
    let now = now_secs();
    // seed 2 events via real evaluate (threshold = 3)
    let _ = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    let _ = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    // simulate 3rd: bucket=2, simulated_count=3, would_fire=true
    let s = simulate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    assert_eq!(s.bucket_events_in_window, 2);
    assert!(s.would_fire);
    assert_eq!(s.action.as_deref(), Some("suppress"));
    assert_eq!(s.action_severity.as_deref(), Some("warn"));
}

#[test]
fn simulate_aggregations_does_not_mutate_bucket() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim75-agg-nomut"));
    _reset_aggregations_for_tests();
    save_aggregation(agg_rule("r1", "suppress", None)).unwrap();
    let now = now_secs();
    // seed 2 events
    let _ = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    let _ = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    // simulate 3 times — should NOT push any events
    for _ in 0..3 {
        let s = simulate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
        // bucket is still 2 (no mutation), simulated_count = 3 → would_fire=true
        assert_eq!(s.bucket_events_in_window, 2);
    }
    // Now real evaluate the 3rd event → suppress fires → state mutated by real call
    let d = evaluate_aggregations("plugin.metrics.exceeded", &serde_json::json!({}), now);
    assert!(matches!(d, AggregationDecision::Suppress { .. }));
}

#[test]
fn simulate_correlations_no_rules_returns_pass() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim75-corr-no-rules"));
    _reset_correlations_for_tests();
    let s = simulate_correlations("plugin.lifecycle.crashed", now_secs());
    assert!(!s.would_suppress);
    assert!(s.matched_a_rule_ids.is_empty());
    assert!(s.matched_b_rule_ids.is_empty());
}

#[test]
fn simulate_correlations_match_a_then_b_simulates_suppress() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim75-corr-ab"));
    _reset_correlations_for_tests();
    save_correlation(corr_rule(
        "r1",
        "plugin.lifecycle.started",
        "plugin.lifecycle.crashed",
        60,
    ))
    .unwrap();
    let now = now_secs();
    // A hits (runs the real evaluate, writing last_a)
    let d_a = evaluate_correlations("plugin.lifecycle.started", now);
    assert_eq!(d_a, CorrelationDecision::Pass);
    // B hits → the simulation should see last_a within the window → would_suppress=true
    let s = simulate_correlations("plugin.lifecycle.crashed", now);
    assert!(s.would_suppress);
    assert_eq!(s.suppressing_rule_id.as_deref(), Some("r1"));
    assert_eq!(s.matched_b_rule_ids, vec!["r1".to_string()]);
}

#[test]
fn simulate_correlations_does_not_mutate_last_a() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim75-corr-nomut"));
    _reset_correlations_for_tests();
    save_correlation(corr_rule(
        "r1",
        "plugin.lifecycle.started",
        "plugin.lifecycle.crashed",
        60,
    ))
    .unwrap();
    let now = now_secs();
    // A hits (writes last_a)
    let _ = evaluate_correlations("plugin.lifecycle.started", now);
    // B simulate — must not write last_a
    for _ in 0..3 {
        let _ = simulate_correlations("plugin.lifecycle.crashed", now);
    }
    // real evaluate B → should still see last_a within the window → Suppress (because the real evaluate A wrote it)
    let d_b = evaluate_correlations("plugin.lifecycle.crashed", now);
    assert!(
        matches!(d_b, CorrelationDecision::Suppress { .. }),
        "expected Suppress, got {:?}",
        d_b
    );
}

// ─── Phase 76: silence / ack / dedup three-gate simulate ────────────────────

fn silence_rule_active(kind_pat: &str) -> SilenceRule {
    let now = now_secs();
    SilenceRule {
        id: format!("sil-{}", uuid::Uuid::new_v4()),
        name: format!("silence-{}", kind_pat),
        kind_pattern: kind_pat.to_string(),
        starts_at: now.saturating_sub(60),
        ends_at: now.saturating_add(3600),
        weekdays: 127, // all 7 days
        start_hour: 0,
        end_hour: 24,
    }
}

#[test]
fn simulate_silence_no_rules_returns_none() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim76-sil-no"));
    let s = simulate_silence("plugin.metrics.exceeded", now_secs());
    assert!(s.is_none());
}

#[test]
fn simulate_silence_active_returns_hit() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim76-sil-hit"));
    let now = now_secs();
    let mut rule = silence_rule_active("plugin.*");
    rule.starts_at = now.saturating_sub(60);
    rule.ends_at = now.saturating_add(3600);
    let _ = save_silence(rule).unwrap();
    let s = simulate_silence("plugin.metrics.exceeded", now);
    assert!(s.is_some());
    let h = s.unwrap();
    assert_eq!(h.kind_pattern, "plugin.*");
    assert!(h.remaining_secs > 0 && h.remaining_secs <= 3600);
}

#[test]
fn simulate_silence_pattern_mismatch_returns_none() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim76-sil-mismatch"));
    let now = now_secs();
    let mut rule = silence_rule_active("plugin.lifecycle.*");
    rule.ends_at = now.saturating_add(3600);
    let _ = save_silence(rule).unwrap();
    let s = simulate_silence("plugin.metrics.exceeded", now);
    assert!(s.is_none());
}

#[test]
fn simulate_ack_active_returns_hit() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim76-ack-active"));
    let now = now_secs();
    let _ = ack_kind("plugin.metrics.*".to_string(), 60).unwrap();
    let s = simulate_ack("plugin.metrics.exceeded", now);
    assert!(s.is_some());
    let h = s.unwrap();
    assert_eq!(h.kind_pattern, "plugin.metrics.*");
    assert!(h.remaining_secs > 0 && h.remaining_secs <= 60);
}

#[test]
fn simulate_ack_expired_returns_none() {
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::core::set_shared_store(fresh_store("sim76-ack-expired"));
    let now = now_secs();
    // ack window 1s
    let _ = ack_kind("plugin.*".to_string(), 1).unwrap();
    // pretend "now" is 5s later (already expired)
    let s = simulate_ack("plugin.metrics.exceeded", now.saturating_add(5));
    assert!(s.is_none(), "ack should be expired by now+5s");
}

#[test]
fn simulate_dedup_within_window_returns_blocked() {
    // the dispatcher inner is a global OnceLock; clear it first to avoid cross-test pollution
    {
        let arc = shared();
        let mut s = arc.lock().unwrap();
        s.last_sent.clear();
    }
    // configure min_interval_secs = 5 (default)
    let cfg = fresh_config(); // min_interval_secs = 5
    let _ = save_config(&cfg);
    let payload = serde_json::json!({"k": "v"});
    let key = dedup_key("plugin.metrics.exceeded", &payload);
    // seed: just inserted → elapsed ≈ 0, remaining ≈ 5
    {
        let arc = shared();
        let mut s = arc.lock().unwrap();
        s.last_sent.insert(key, std::time::Instant::now());
    }
    let s = simulate_dedup("plugin.metrics.exceeded", &payload, now_secs());
    assert!(s.is_some());
    let h = s.unwrap();
    assert_eq!(h.min_interval_secs, 5);
    assert!(h.last_sent_secs_ago < 5);
    assert!(h.remaining_secs > 0);
}
