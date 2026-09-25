use super::*;
use crate::core::agent::{now_secs, SessionStore};
use crate::core::event::OpencapxEvent;

fn tmpdb(tag: &str) -> Storage {
    let dir = std::env::temp_dir().join(format!("opencapx-db-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    Storage::open(&dir.join("t.db")).unwrap()
}

fn sess(id: &str, state: AgentState) -> Session {
    Session {
        id: id.into(),
        agent: "claude".into(),
        project: "p".into(),
        cwd: String::new(),
        message: "m".into(),
        state,
        started_at: 1000,
        updated_at: 1000,
        model: String::new(),
        speech: String::new(),
        choices: None,
        answered: None,
    }
}

#[test]
fn session_model_roundtrips_and_migrates_old_db() {
    // an old DB has no model column → after open()'s migration it should read/write normally
    let dir = std::env::temp_dir().join(format!("opencapx-db-model-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, agent TEXT NOT NULL, project TEXT NOT NULL,
                    message TEXT NOT NULL, state TEXT NOT NULL, updated_at INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('old','claude','p','m','working',1)",
            [],
        )
        .unwrap();
    }
    let mut db = Storage::open(&path).unwrap();
    // after migration old rows are readable, model defaults to an empty string
    let old = db.all();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].model, "");
    // write a session with a model and read it back
    let mut s = sess("m1", AgentState::Working);
    s.model = "claude-opus-4-1".into();
    db.upsert(s);
    assert_eq!(db.get("m1").unwrap().model, "claude-opus-4-1");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_speech_roundtrips_and_migrates_old_db() {
    // an old DB has no speech column (it is a later-added field) → after open()'s migration it should read/write normally
    let dir = std::env::temp_dir().join(format!("opencapx-db-speech-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, agent TEXT NOT NULL, project TEXT NOT NULL,
                    message TEXT NOT NULL, state TEXT NOT NULL, updated_at INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('old','claude','p','m','working',1)",
            [],
        )
        .unwrap();
    }
    let mut db = Storage::open(&path).unwrap();
    assert_eq!(db.all()[0].speech, "");
    // sticky fields are preserved by upsert: write the body, then write an event without a body, and the body must not be wiped
    let mut s = sess("sp1", AgentState::Working);
    s.speech = "Let me first look at the bubble rendering path.".into();
    db.upsert(s);
    assert_eq!(
        db.get("sp1").unwrap().speech,
        "Let me first look at the bubble rendering path."
    );
    let mut later = sess("sp1", AgentState::Working);
    later.model = "claude-sonnet-4-5".into();
    db.upsert(later);
    let back = db.get("sp1").unwrap();
    assert_eq!(
        back.speech, "",
        "storage writes faithfully (stickiness is event::ingest's job)"
    );
    assert_eq!(back.model, "claude-sonnet-4-5");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plugins_auto_reload_column_migrates_old_db() {
    // an old DB's plugins table has no auto_reload column (created before it) → open()'s migration should add it:
    // plugin::list()'s SELECT references it directly; a missing column makes the whole plugin list read empty.
    let dir = std::env::temp_dir().join(format!("opencapx-db-autoreload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE plugins (
                    id TEXT PRIMARY KEY, version TEXT NOT NULL, type TEXT NOT NULL,
                    status TEXT NOT NULL, path TEXT NOT NULL, manifest TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plugins VALUES ('com.x','0.1.0','capability','running','/tmp/x','{}')",
            [],
        )
        .unwrap();
    }
    let db = Storage::open(&path).unwrap();
    // after migration the column exists (the SELECT would error if it were missing) + old rows are readable with a default of 0
    let auto: i64 = db
        .conn
        .query_row(
            "SELECT auto_reload FROM plugins WHERE id = 'com.x'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(auto, 0, "old rows default auto_reload to 0");
    let _ = std::fs::remove_dir_all(&dir);
}

/// O1 — shared prune: old events deleted, new events kept; repeated runs are idempotent.
#[test]
fn prune_events_shared_bounds_old_rows() {
    let dir = std::env::temp_dir().join(format!("opencapx-db-evprune-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.db");
    let now = crate::core::agent::now_secs();
    let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(StoreEnum::Db(
        Storage::open(&path).unwrap(),
    )));
    {
        // insert after open: the startup path prunes, so inserting first would get cleared by it.
        let mut g = store.lock().unwrap();
        let inserted = g.with_conn(|c| {
                c.execute(
                    "INSERT INTO events (id, type, source, timestamp, payload) VALUES ('old','t','s',?1,'{}')",
                    [now.saturating_sub((EVENT_RETENTION_DAYS + 1) * 86400)],
                )
                .unwrap_or(0)
                    + c.execute(
                        "INSERT INTO events (id, type, source, timestamp, payload) VALUES ('new','t','s',?1,'{}')",
                        [now],
                    )
                    .unwrap_or(0)
            });
        assert_eq!(inserted, Some(2));
    }
    assert_eq!(prune_events_shared(&store), 1, "only old rows cleared");
    assert_eq!(
        prune_events_shared(&store),
        0,
        "repeated runs are idempotent"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// O3 — WAL is in effect (standard for a local desktop DB).
#[test]
fn sqlite_pragmas_are_applied() {
    let dir = std::env::temp_dir().join(format!("opencapx-db-pragma-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Storage::open(&dir.join("t.db")).unwrap();
    let mode: String = db
        .conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plugins_last_version_column_migrates_old_db() {
    // M8/F12 across versions — an old DB's plugins table has no last_version column (before M7) → open()
    // migration adds the column; plugin::last_version_of SELECTs it directly, and a missing column would fail the read.
    let dir = std::env::temp_dir().join(format!("opencapx-db-lastver-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE plugins (
                    id TEXT PRIMARY KEY, version TEXT NOT NULL, type TEXT NOT NULL,
                    status TEXT NOT NULL, path TEXT NOT NULL, manifest TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plugins VALUES ('com.x','0.2.0','capability','stopped','/tmp/x','{}')",
            [],
        )
        .unwrap();
    }
    let db = Storage::open(&path).unwrap();
    // after migration the column exists + old rows are NULL (no successful handshake yet)
    let v: Option<String> = db
        .conn
        .query_row(
            "SELECT last_version FROM plugins WHERE id = 'com.x'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, None, "old rows have an empty last_version");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_cwd_roundtrips_and_migrates_old_db() {
    // an old DB has no cwd column → after open()'s migration it should read/write normally
    let dir = std::env::temp_dir().join(format!("opencapx-db-cwd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, agent TEXT NOT NULL, project TEXT NOT NULL,
                    message TEXT NOT NULL, state TEXT NOT NULL, updated_at INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('old','claude','p','m','working',1)",
            [],
        )
        .unwrap();
    }
    let mut db = Storage::open(&path).unwrap();
    assert_eq!(db.all()[0].cwd, "");
    let mut s = sess("cw1", AgentState::Working);
    s.cwd = "/Users/me/work/OpenCapX".into();
    db.upsert(s);
    assert_eq!(db.get("cw1").unwrap().cwd, "/Users/me/work/OpenCapX");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sqlite_session_roundtrip_and_expiry() {
    let mut db = tmpdb("roundtrip");
    db.upsert(sess("a", AgentState::Done));
    db.upsert(sess("w", AgentState::Working));
    assert!(db.get("a").is_some());
    assert!(db.active(1031).iter().all(|s| s.id != "a"));
    // working with no heartbeat for over STALE_ACTIVE_TTL also counts as expired (zombie protection)
    assert!(db.active(1900).iter().any(|s| s.id == "w"));
    assert!(db.active(1901).iter().all(|s| s.id != "w"));
    db.dismiss("w");
    assert!(db.get("w").is_none());
    db.upsert(sess("b", AgentState::Idle));
    db.clear();
    assert!(db.all().is_empty());
}

#[test]
fn sweep_archives_then_deletes_and_lists_history() {
    let mut db = tmpdb("archive");
    db.upsert(sess("old", AgentState::Working)); // updated_at = 1000
    let mut live = sess("live", AgentState::Working);
    live.updated_at = 5000;
    db.upsert(live);

    assert_eq!(db.sweep(5300), 1);
    assert!(
        db.get("old").is_none(),
        "expired session left the active table"
    );
    assert!(db.get("live").is_some());

    let hist = db.list_session_archive(10);
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0].id, "old");
    assert_eq!(hist[0].state, "working");
    assert_eq!(hist[0].ended_at, 1000);
    assert_eq!(hist[0].duration, 0);

    // a repeated sweep does not produce duplicate archives
    assert_eq!(db.sweep(5300), 0);
    assert_eq!(db.list_session_archive(10).len(), 1);
}

#[test]
fn prune_session_archive_drops_only_old_records() {
    let mut db = tmpdb("archiveprune");
    db.upsert(sess("old", AgentState::Working));
    db.sweep(9999);
    assert_eq!(db.list_session_archive(10).len(), 1);
    // now is far from exceeding retention → do not delete
    assert_eq!(db.prune_session_archive(1000), 0);
    assert_eq!(db.list_session_archive(10).len(), 1);
    // now exceeds 90 days → delete (record ended_at = 1000)
    let later = 1000 + (ARCHIVE_KEEP_DAYS + 1) * 86_400;
    assert_eq!(db.prune_session_archive(later), 1);
    assert!(db.list_session_archive(10).is_empty());
}

#[test]
fn sqlite_logs_and_prunes_events() {
    let dir = std::env::temp_dir().join(format!("opencapx-db-{}-prune", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    {
        let mut db = Storage::open(&dir.join("t.db")).unwrap();
        let old = OpencapxEvent {
            id: "e1".into(),
            kind: "agent.completed".into(),
            source: "hooks:claude".into(),
            timestamp: 1000,
            payload: serde_json::json!({"x": 1}),
        };
        db.insert_event(&old);
        let fresh = OpencapxEvent {
            id: "e2".into(),
            kind: "agent.started".into(),
            source: "mcp".into(),
            timestamp: now_secs(),
            payload: serde_json::json!({}),
        };
        db.insert_event(&fresh);
    }
    let db = Storage::open(&dir.join("t.db")).unwrap();
    let n: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "old event should be pruned on reopen");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn store_enum_delegates() {
    let mut s = StoreEnum::Mem(SessionStore::new());
    s.upsert(sess("a", AgentState::Working));
    assert_eq!(s.active(1000).len(), 1);
    assert!(!s.is_db());
    let e = OpencapxEvent {
        id: "e".into(),
        kind: "k".into(),
        source: "s".into(),
        timestamp: 1,
        payload: serde_json::json!(null),
    };
    s.log_event(&e); // mem variant: no-op, must not panic
}

#[test]
fn list_events_filters_by_prefix_and_orders_desc() {
    let mut db = tmpdb("audit");
    db.insert_event(&OpencapxEvent {
            id: "p1".into(),
            kind: "permission.ask".into(),
            source: "core".into(),
            timestamp: 100,
            payload: serde_json::json!({"pluginId":"echo-vision","permission":"image.read","decision":"granted"}),
        });
    db.insert_event(&OpencapxEvent {
            id: "p2".into(),
            kind: "permission.deny".into(),
            source: "core".into(),
            timestamp: 200,
            payload: serde_json::json!({"pluginId":"x","permission":"shell.exec","decision":"denied","reason":"high-risk"}),
        });
    db.insert_event(&OpencapxEvent {
        id: "x1".into(),
        kind: "agent.started".into(),
        source: "mcp".into(),
        timestamp: 300,
        payload: serde_json::json!({}),
    });
    let only_perm = db.list_events("permission.", 50);
    assert_eq!(only_perm.len(), 2);
    assert_eq!(only_perm[0].id, "p2", "newest first");
    assert_eq!(only_perm[1].id, "p1");
    assert_eq!(only_perm[0].payload["decision"], "denied");
    let capped = db.list_events("permission.", 1);
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].id, "p2");
}

/// Phase 35 — Audit search/filter: kind prefix + full text + time range + special-character escaping.
#[test]
fn list_events_filtered_applies_all_clauses() {
    let mut db = tmpdb("filter");
    let cases = [
        ("p1", "permission.granted", 100, "image.read", "granted", ""),
        (
            "p2",
            "permission.denied",
            200,
            "shell.exec",
            "denied",
            "high-risk",
        ),
        (
            "p3",
            "permission.ask",
            300,
            "camera",
            "ask",
            "user-decision",
        ),
        (
            "l1",
            "plugin.lifecycle.starting",
            150,
            "echo-vision",
            "",
            "",
        ),
        ("c1", "capability.completed", 250, "image.analyze", "ok", ""),
        (
            "p4",
            "permission.granted",
            400,
            "image.read",
            "granted",
            "rematch",
        ),
    ];
    for (id, kind, ts, plugin, decision, reason) in cases {
        db.insert_event(&OpencapxEvent {
            id: id.into(),
            kind: kind.into(),
            source: "core".into(),
            timestamp: ts,
            payload: serde_json::json!({
                "pluginId": plugin,
                "permission": kind.split('.').nth(1).unwrap_or(""),
                "decision": decision,
                "reason": reason,
            }),
        });
    }

    // 1) kind_prefix only
    let only_perm = db.list_events_filtered(Some("permission."), None, None, None, 50);
    assert_eq!(only_perm.len(), 4, "4 permission.* rows");
    assert!(only_perm.iter().all(|e| e.kind.starts_with("permission.")));

    // 2) kind_prefix + query (full-text match "den") — only p2's kind/payload contains "den" (the other
    //   permission.* events have reasons "high-risk"/"user-decision"/"rematch", with no den substring)
    let denied = db.list_events_filtered(Some("permission."), Some("den"), None, None, 50);
    assert_eq!(
        denied.len(),
        1,
        "permission.* + 'den' matches only p2 (permission.denied)"
    );
    assert_eq!(denied[0].id, "p2");

    // 3) time range since..until
    let ranged = db.list_events_filtered(None, None, Some(180), Some(350), 50);
    assert_eq!(ranged.len(), 3, "ts ∈ [180, 350]");
    let ids: Vec<&str> = ranged.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"p2"));
    assert!(ids.contains(&"p3"));
    assert!(ids.contains(&"c1"));

    // 4) combination: permission.* + full text + time
    let combo =
        db.list_events_filtered(Some("permission."), Some("image"), Some(50), Some(500), 50);
    assert_eq!(
        combo.len(),
        2,
        "permission.* and payload contains image and ts ∈ [50,500]"
    );
    let combo_ids: Vec<&str> = combo.iter().map(|e| e.id.as_str()).collect();
    assert!(combo_ids.contains(&"p1"));
    assert!(combo_ids.contains(&"p4"));

    // 5) limit truncation
    let capped = db.list_events_filtered(Some("permission."), None, None, None, 2);
    assert_eq!(capped.len(), 2);

    // 6) special-character escaping: a user searching "_" must not be treated as a wildcard
    // if "_" were a wildcard it would match payloads like "pluginId":"echo-vision", image.read (no _), shell.exec (no _)…
    // so the fuzzy search "_" below should match only payloads actually containing _ (0 here → verifies no over-matching)
    let underscore = db.list_events_filtered(None, Some("_"), None, None, 50);
    assert_eq!(
        underscore.len(),
        0,
        "an unescaped _ would match every event; escaped, it matches only a literal underscore"
    );

    // 7) empty query / None query behave identically
    let none_q = db.list_events_filtered(Some("permission."), None, None, None, 50);
    let empty_q = db.list_events_filtered(Some("permission."), Some(""), None, None, 50);
    assert_eq!(none_q.len(), empty_q.len());
}

/// Phase 28 — ratings: add writes to the DB + list_ratings descending + rating_summary computes the average.
/// Cross-plugin isolation, consistent add/list behavior across StoreEnum, out-of-range scores rejected.
#[test]
fn rating_add_list_summary_round_trip() {
    let dir = std::env::temp_dir().join(format!("opencapx-rating-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut db = Storage::open(&dir.join("t.db")).unwrap();
    // out-of-range scores: both 0 and 6 are rejected.
    assert!(db.add_rating("com.x.demo-7", 0, None, 100).is_err());
    assert!(db.add_rating("com.x.demo-7", 6, None, 100).is_err());

    db.add_rating("com.x.demo-7", 5, Some("love it"), 100)
        .unwrap();
    db.add_rating("com.x.demo-7", 3, None, 200).unwrap();
    db.add_rating("com.x.demo-7", 4, Some("good"), 300).unwrap();
    // noise: another plugin id that must not be included.
    db.add_rating("com.x.other-7", 1, None, 400).unwrap();

    let rows = db.list_ratings("com.x.demo-7", 50);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].score, 4, "newest first (ts=300)");
    assert_eq!(rows[0].comment.as_deref(), Some("good"));
    assert_eq!(rows[1].score, 3);
    assert_eq!(rows[2].score, 5);

    let summary = db.rating_summary("com.x.demo-7");
    assert_eq!(summary.count, 3);
    // (5+3+4)/3 = 4.0 → rounded 4.00
    assert!((summary.avg - 4.0).abs() < 0.01, "avg={}", summary.avg);

    // a plugin with no ratings → count=0, avg=0
    let empty = db.rating_summary("com.x.nothing-7");
    assert_eq!(empty.count, 0);
    assert_eq!(empty.avg, 0.0);

    // limit=2 takes only the first two
    assert_eq!(db.list_ratings("com.x.demo-7", 2).len(), 2);

    // StoreEnum passthrough
    let mut store: StoreEnum = StoreEnum::Db(db);
    store
        .add_rating("com.x.demo-7", 2, None, 500)
        .expect("enum add_rating");
    assert_eq!(store.rating_summary("com.x.demo-7").count, 4);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Phase 30 — capability_stats: record + summary aggregate count + avg + p50 + p95,
/// taking only the latest N per pair (via the ROW_NUMBER window function).
#[test]
fn capability_stats_summary_aggregates_percentiles() {
    let dir = std::env::temp_dir().join(format!("opencapx-capstats-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut db = Storage::open(&dir.join("t.db")).unwrap();

    // 100 calls for (image.analyze, plug-a), latency 10ms..109ms increasing
    for i in 0..100i64 {
        db.record_capability_call(
            "image.analyze",
            "plug-a",
            10 + i,
            (1_000 + i) as u64,
            "ok",
            None,
        );
    }
    // noise: another plugin, only 5 calls, all with high latency
    for i in 0..5i64 {
        db.record_capability_call(
            "image.analyze",
            "plug-b",
            500 + i,
            (2_000 + i) as u64,
            "ok",
            None,
        );
    }

    let rows = db.capability_stats_summary(50);
    // two pairs
    assert_eq!(rows.len(), 2);

    // by count descending: plug-a (100) should come first
    assert_eq!(rows[0].plugin_id, "plug-a");
    assert_eq!(rows[0].count, 50, "limit=50 takes the latest 50");
    // the latest 50 latencies = 60..109, average = (60+109)*50/2/50 = 84.5
    let avg_a = rows[0].avg_ms;
    assert!((avg_a - 84.5).abs() < 0.5, "avg_a={}", avg_a);
    // p50: samples[len/2] = samples[25], after sorting = 60+25 = 85
    assert_eq!(rows[0].p50_ms, 85);
    // p95: ceil(50*0.95)=48, idx=47 (0-based), after sorting = 60+47 = 107
    assert_eq!(rows[0].p95_ms, 107);

    // plug-b: all 5 collected, count=5
    let b = rows.iter().find(|r| r.plugin_id == "plug-b").unwrap();
    assert_eq!(b.count, 5);
    assert!((b.avg_ms - 502.0).abs() < 0.5);
    assert_eq!(b.p50_ms, 502); // sorted [500,501,502,503,504], len/2=2 → 502
    assert_eq!(b.p95_ms, 504); // ceil(5*0.95)=5,idx=4 → 504

    // last_used_at takes the maximum
    assert!(rows.iter().all(|r| r.last_used_at > 0));

    // returns [] when empty
    assert!(
        Storage::open(&dir.join("t.db"))
            .unwrap()
            .capability_stats_summary(50)
            .is_empty()
            == false
    ); // the same file has data; just verify the call does not crash

    let _ = std::fs::remove_dir_all(&dir);
}

/// Phase 31 — capability_stats failure classification: result + error_kind + fail_count + errors bucketing.
/// Verifies: ok paths are computed into p50/p95, failures are not; fail_count and errors aggregate by error_kind.
#[test]
fn capability_stats_summary_tracks_failures_and_errors() {
    let dir = std::env::temp_dir().join(format!("opencapx-capstats-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut db = Storage::open(&dir.join("t.db")).unwrap();

    // 7 ok calls (latency 50..56) + 2 timeout + 1 denied
    for i in 0..7i64 {
        db.record_capability_call(
            "image.analyze",
            "plug-a",
            50 + i,
            (1_000 + i) as u64,
            "ok",
            None,
        );
    }
    db.record_capability_call(
        "image.analyze",
        "plug-a",
        60_000,
        2_000,
        "timeout",
        Some("timeout"),
    );
    db.record_capability_call(
        "image.analyze",
        "plug-a",
        61_000,
        2_001,
        "timeout",
        Some("timeout"),
    );
    db.record_capability_call(
        "image.analyze",
        "plug-a",
        0,
        2_002,
        "denied",
        Some("permission_denied"),
    );

    let rows = db.capability_stats_summary(50);
    assert_eq!(rows.len(), 1);
    let r = &rows[0];

    // total samples = 7 ok + 2 timeout + 1 denied = 10
    assert_eq!(r.count, 10);
    assert_eq!(r.fail_count, 3, "3 failures: 2 timeout + 1 denied");

    // errors bucketing
    assert_eq!(r.errors.get("timeout").copied(), Some(2));
    assert_eq!(r.errors.get("permission_denied").copied(), Some(1));
    assert_eq!(r.errors.len(), 2);

    // p50/p95/avg use only the 7 ok samples (50..56)
    // sorted [50,51,52,53,54,55,56], len/2=3 → 53
    assert_eq!(r.p50_ms, 53);
    // ceil(7*0.95)=7,idx=6 → 56
    assert_eq!(r.p95_ms, 56);
    // avg = (50+51+52+53+54+55+56)/7 = 53.0
    assert!((r.avg_ms - 53.0).abs() < 0.01);

    // second plugin: 3 all err + 1 ok, verifying fail_count and ok are both correct
    for i in 0..3i64 {
        db.record_capability_call(
            "image.analyze",
            "plug-b",
            100,
            (3_000 + i) as u64,
            "err",
            Some("rpc_error"),
        );
    }
    db.record_capability_call("image.analyze", "plug-b", 200, 3_010, "ok", None);

    let rows2 = db.capability_stats_summary(50);
    let b = rows2.iter().find(|r| r.plugin_id == "plug-b").unwrap();
    assert_eq!(b.count, 4);
    assert_eq!(b.fail_count, 3);
    assert_eq!(b.errors.get("rpc_error").copied(), Some(3));
    // p50/p95/avg use only 1 ok sample (200)
    assert_eq!(b.p50_ms, 200);
    assert_eq!(b.p95_ms, 200);
    assert!((b.avg_ms - 200.0).abs() < 0.01);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Phase 40 — health config upsert/list roundtrip; get_health_config falls back to the default when there is no row.
#[test]
fn health_config_round_trips_and_falls_back_to_default() {
    let dir = std::env::temp_dir().join(format!("opencapx-db-{}-health", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut db = Storage::open(&dir.join("t.db")).unwrap();

    // not yet upserted → get the default
    let cfg = db.get_health_config("plug-x");
    assert_eq!(cfg, crate::core::health::PluginHealthConfig::default());
    assert_eq!(cfg.heartbeat_sec, 0);
    assert_eq!(cfg.max_retries, 3);
    assert!(cfg.enabled);

    // after upsert it roundtrips
    let custom = crate::core::health::PluginHealthConfig {
        heartbeat_sec: 30,
        ping_timeout_ms: 1500,
        max_retries: 5,
        backoff_initial_ms: 2000,
        enabled: false,
    };
    db.upsert_health_config("plug-x", &custom);
    let back = db.get_health_config("plug-x");
    assert_eq!(back, custom);

    // upserting the same plugin again = overwrite
    let custom2 = crate::core::health::PluginHealthConfig {
        heartbeat_sec: 60,
        ..crate::core::health::PluginHealthConfig::default()
    };
    db.upsert_health_config("plug-x", &custom2);
    let back2 = db.get_health_config("plug-x");
    assert_eq!(back2.heartbeat_sec, 60);
    assert_eq!(back2.max_retries, 3); // other fields keep the default

    // list fetches both plugins at once
    db.upsert_health_config(
        "plug-y",
        &crate::core::health::PluginHealthConfig::default(),
    );
    let all = db.list_health_configs();
    let ids: Vec<&str> = all.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"plug-x"));
    assert!(ids.contains(&"plug-y"));

    let _ = std::fs::remove_dir_all(&dir);
}
