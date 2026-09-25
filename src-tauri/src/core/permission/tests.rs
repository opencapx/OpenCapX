use super::*;

/// session_tier's three tests share the global overlay (session_grants) and must be serialized.
static SESSION_LOCK: Mutex<()> = Mutex::new(());

fn gate_store(tag: &str) -> SharedStore {
    let dir = std::env::temp_dir().join(format!("opencapx-session-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Arc::new(Mutex::new(super::super::storage::StoreEnum::Db(
        super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )))
}

use crate::core::agent::SessionStore;
use crate::core::storage::StoreEnum;
use std::sync::{Arc, Mutex};

fn mem_store() -> SharedStore {
    Arc::new(Mutex::new(StoreEnum::Mem(SessionStore::new())))
}

/// F6 — pins the timeout semantics of waiting for an answer: no answer → Timeout; answer → Answered;
/// production ceiling constant = 60s (a short timeout only verifies the mechanism, it does not wait out the production value).
#[test]
fn await_answer_pins_timeout_semantics() {
    let (_tx, rx) = mpsc::channel::<String>();
    assert_eq!(
        await_answer(&rx, Duration::from_millis(30)),
        AskOutcome::Timeout
    );
    let (tx, rx2) = mpsc::channel::<String>();
    tx.send("once".to_string()).unwrap();
    assert_eq!(
        await_answer(&rx2, Duration::from_millis(300)),
        AskOutcome::Answered("once".to_string())
    );
    assert_eq!(ASK_TIMEOUT, Duration::from_secs(60));
}

/// §4.2 lexicon rejection table: case / homoglyphs / trailing dot / double dot / single segment / over 64 / digit-leading segment.
#[test]
fn name_lexicon_accepts_and_rejects() {
    for ok in [
        "weather.fetch",
        "things.add",
        "image.analyze",
        "a.b",
        "x2.y-z_w",
    ] {
        assert!(valid_name(ok), "{ok:?} should be valid");
    }
    for bad in [
        "",
        "weather", // single segment
        ".weather.fetch",
        "weather.fetch.",
        "weather..fetch", // leading/trailing/double dot
        "Weather.fetch",
        "weather.Fetch",  // case
        "2eather.fetch",  // digit-leading segment
        "weather.fetch ", // whitespace
        "ｗeather.fetch", // full-width homoglyph (non-ASCII on the NFKC surface)
        "wéather.fetch",  // diacritic
        "weather.fetch\n",
        &format!("{}.b", "a".repeat(63)), // over 64
    ] {
        assert!(!valid_name(bad), "{bad:?} should be invalid");
    }
}

/// §4.2 reserved sets: all built-in first segments are reserved; opencapx is reserved; new domains are not.
#[test]
fn reserved_sets_cover_builtin_domains() {
    for c in super::super::capability::CAPABILITY_IDS {
        assert!(reserved_capability(c), "{c} should be reserved capability");
        assert!(
            reserved_domain(first_segment(c)),
            "domain of {c} should be reserved"
        );
    }
    for (p, _) in PERMISSIONS {
        assert!(
            reserved_domain(first_segment(p)),
            "domain of {p} should be reserved"
        );
    }
    assert!(reserved_domain("opencapx"));
    assert!(!reserved_capability("weather.fetch"));
    assert!(!reserved_domain("weather"));
    assert!(!reserved_domain("com"));
}

#[test]
fn defaults_match_docs_table() {
    assert_eq!(default_decision("pet.animation"), Decision::Granted);
    assert_eq!(default_decision("image.read"), Decision::Ask);
    assert_eq!(default_decision("process.execute"), Decision::Denied);
    assert_eq!(default_decision("clipboard.write"), Decision::Ask);
    // v1.3 batch spot-check
    assert_eq!(default_decision("automation.control"), Decision::Ask);
    assert_eq!(default_decision("input.control"), Decision::Denied);
    assert_eq!(default_decision("photos.read"), Decision::Ask);
    assert_eq!(default_decision("location.read"), Decision::Ask);
    assert_eq!(default_decision("audio.output"), Decision::Ask);
    assert_eq!(default_decision("url.scheme.open"), Decision::Ask);
    // v1.4 batch spot-check
    assert_eq!(default_decision("media.control"), Decision::Ask);
    assert_eq!(default_decision("messages.read"), Decision::Denied);
    assert_eq!(default_decision("window.management"), Decision::Denied);
    assert_eq!(default_decision("power.control"), Decision::Denied);
    assert_eq!(default_decision("notes.read"), Decision::Ask);
    assert_eq!(default_decision("reminders.write"), Decision::Ask);
    assert_eq!(default_decision("mail.read"), Decision::Ask);
    assert_eq!(default_decision("system.settings"), Decision::Ask);
    assert_eq!(default_decision("printer.control"), Decision::Ask);
    // v1.5 Things data surface: reads and writes both ask
    assert_eq!(default_decision("things.read"), Decision::Ask);
    assert_eq!(default_decision("things.write"), Decision::Ask);
    assert!(known("plugin.install"));
    assert!(!known("not.a.permission"));
}

#[test]
fn capability_mapping() {
    assert_eq!(capability_permission("image.analyze"), Some("image.read"));
    assert_eq!(
        capability_permission("browser.read"),
        Some("browser.control")
    );
    assert_eq!(
        capability_permission("clipboard.write"),
        Some("clipboard.write")
    );
    assert_eq!(
        capability_permission("file.write"),
        Some("filesystem.write")
    );
    assert_eq!(capability_permission("file.search"), Some("file.read"));
    // v1.2: speech = human-facing channel, tightened to notification.post (ask)
    assert_eq!(
        capability_permission("speech.synthesize"),
        Some("notification.post")
    );
    // A7: the reply includes clipboard fragments, take the strictest component
    assert_eq!(
        capability_permission("context.get_current"),
        Some("clipboard.read")
    );
    // v1.2: OS permission probing is read-only, no gate needed
    assert_eq!(capability_permission("system.permission_status"), None);
    // v1.3: high/medium-value batch
    assert_eq!(
        capability_permission("automation.run"),
        Some("automation.control")
    );
    assert_eq!(capability_permission("input.send"), Some("input.control"));
    assert_eq!(capability_permission("photos.read"), Some("photos.read"));
    assert_eq!(
        capability_permission("contacts.search"),
        Some("contacts.read")
    );
    assert_eq!(
        capability_permission("calendar.events"),
        Some("calendar.read")
    );
    assert_eq!(capability_permission("location.get"), Some("location.read"));
    assert_eq!(capability_permission("audio.play"), Some("audio.output"));
    assert_eq!(
        capability_permission("url.scheme.open"),
        Some("url.scheme.open")
    );
    // v1.3: subscription screenshot diff is the same tier as screen.capture
    assert_eq!(
        capability_permission("screen.watch"),
        Some("screen.capture")
    );
    // v1.4: high/medium-value batch
    assert_eq!(
        capability_permission("media.playback"),
        Some("media.control")
    );
    assert_eq!(
        capability_permission("messages.recent"),
        Some("messages.read")
    );
    assert_eq!(
        capability_permission("window.list"),
        Some("window.management")
    );
    assert_eq!(
        capability_permission("window.focus"),
        Some("window.management")
    );
    assert_eq!(capability_permission("system.sleep"), Some("power.control"));
    assert_eq!(capability_permission("system.lock"), Some("power.control"));
    assert_eq!(capability_permission("notes.read"), Some("notes.read"));
    assert_eq!(
        capability_permission("reminders.read"),
        Some("reminders.read")
    );
    assert_eq!(
        capability_permission("reminders.write"),
        Some("reminders.write")
    );
    assert_eq!(capability_permission("mail.recent"), Some("mail.read"));
    assert_eq!(
        capability_permission("system.settings"),
        Some("system.settings")
    );
    assert_eq!(
        capability_permission("printer.print"),
        Some("printer.control")
    );
    // v1.5 Things data surface: the three reads → things.read, the three writes → things.write
    assert_eq!(capability_permission("things.list"), Some("things.read"));
    assert_eq!(capability_permission("things.show"), Some("things.read"));
    assert_eq!(capability_permission("things.search"), Some("things.read"));
    assert_eq!(capability_permission("things.add"), Some("things.write"));
    assert_eq!(capability_permission("things.update"), Some("things.write"));
    assert_eq!(capability_permission("things.delete"), Some("things.write"));
    // plugin.install is a vocabulary placeholder: no capability maps to it
    for cap in super::super::capability::CAPABILITY_IDS {
        assert_ne!(
            capability_permission(cap),
            Some("plugin.install"),
            "{} maps to plugin.install",
            cap
        );
    }
    assert_eq!(capability_permission("nope.nope"), None);
}

#[test]
fn mem_store_falls_back_to_defaults() {
    let s = mem_store();
    assert_eq!(check(&s, "p", "image.read"), Decision::Ask);
    assert!(!set_decision(&s, "p", "image.read", "granted")); // mem has no DB
    assert_eq!(
        gate(&s, "p", "image.read", "capability", None),
        Decision::Denied
    ); // ask + no UI → fast deny
    assert_eq!(
        gate(&s, "p", "pet.animation", "capability", None),
        Decision::Granted
    );
}

#[test]
// ===== v1.5 session tier =====
#[test]
fn session_tier_plugin_gate_lasts_for_process_and_db_beats_it() {
    let _guard = SESSION_LOCK.lock().unwrap();
    session_revoke_all();
    let s = gate_store("plugin");
    // first ask: answer session → Granted, not persisted
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("session".into())]);
    assert_eq!(
        gate_with(&asker, &s, "sp1", "image.read", "capability", None),
        Decision::Granted
    );
    assert_eq!(check(&s, "sp1", "image.read"), Decision::Ask);
    // second ask: passes even with UI absent (overlay hit, no prompt again)
    let no_ui = ScriptAsker::new(false, vec![]);
    assert_eq!(
        gate_with(&no_ui, &s, "sp1", "image.read", "capability", None),
        Decision::Granted
    );
    // explicit denied persisted → DB wins, overlay is void
    assert!(set_decision(&s, "sp1", "image.read", "denied"));
    assert_eq!(
        gate_with(&no_ui, &s, "sp1", "image.read", "capability", None),
        Decision::Denied
    );
    // revoke all sessions: back to the ask path (no UI → fast deny, proving the overlay is cleared)
    assert!(set_decision(&s, "sp1", "image.read", "ask"));
    session_revoke_all();
    assert_eq!(
        gate_with(&no_ui, &s, "sp1", "image.read", "capability", None),
        Decision::Denied
    );
    assert_eq!(check(&s, "sp1", "image.read"), Decision::Ask);
    session_revoke_all();
}

#[test]
fn session_tier_agent_gate_mirrors_plugin_gate() {
    let _guard = SESSION_LOCK.lock().unwrap();
    session_revoke_all();
    let s = gate_store("agent");
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("session".into())]);
    assert_eq!(
        gate_agent_with(&asker, &s, "ag_s1", "file.read", "mcp"),
        Decision::Granted
    );
    let no_ui = ScriptAsker::new(false, vec![]);
    assert_eq!(
        gate_agent_with(&no_ui, &s, "ag_s1", "file.read", "mcp"),
        Decision::Granted
    );
    // subject isolation: another agent does not receive this session grant
    assert_eq!(
        gate_agent_with(&no_ui, &s, "ag_s2", "file.read", "mcp"),
        Decision::Denied
    );
    session_revoke_all();
}

#[test]
fn session_tier_declared_permission_downgrades_to_once() {
    let _guard = SESSION_LOCK.lock().unwrap();
    session_revoke_all();
    let s = gate_store("declared");
    // seed a declaration-table row → weather.demo domain-style third-party permission (once-only semantics)
    {
        let mut g = s.lock().unwrap();
        let n = g.with_conn(|c| c.execute(
                "INSERT INTO capability_declarations (capability, plugin_id, permission, default_decision, confirmed_at) VALUES ('x.y','dp1','third.weather','ask',0)",
                [],
            ).unwrap_or(0));
        assert_eq!(n, Some(1));
    }
    assert!(crate::core::declaration::is_declared_permission(
        &s,
        "third.weather"
    ));
    // answer session → superficially Granted, but treated as once: not entered into the overlay
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("session".into())]);
    assert_eq!(
        gate_with(&asker, &s, "dp1", "third.weather", "capability", None),
        Decision::Granted
    );
    let no_ui = ScriptAsker::new(false, vec![]);
    assert_eq!(
        gate_with(&no_ui, &s, "dp1", "third.weather", "capability", None),
        Decision::Denied
    );
    session_revoke_all();
}

fn ask_registry_and_high_risk_set() {
    assert!(!resolve_ask("nope", "once"));
    assert!(asks().lock().unwrap().is_empty());
    assert!(HIGH_RISK.contains(&"process.execute"));
    assert!(!HIGH_RISK.contains(&"image.read"));
    // a built-in write goes through filesystem.write: high-risk, denied by default, only allow-once at runtime
    assert!(HIGH_RISK.contains(&capability_permission("file.write").unwrap()));
    // clipboard.write is not high-risk: paste surface, ask by default
    assert!(!HIGH_RISK.contains(&capability_permission("clipboard.write").unwrap()));
    // v1.3: driving apps / synthesizing input / installing plugins are high-risk; personal-data reads are not
    assert!(HIGH_RISK.contains(&"automation.control"));
    assert!(HIGH_RISK.contains(&"input.control"));
    assert!(HIGH_RISK.contains(&"plugin.install"));
    assert!(!HIGH_RISK.contains(&"photos.read"));
    assert!(!HIGH_RISK.contains(&"audio.output"));
    // v1.4: chat history / window manipulation / power / mail are high-risk; media, notes, reminders,
    // settings, printing are not (impact stays within this process or OS semantics, ask suffices)
    assert!(HIGH_RISK.contains(&"messages.read"));
    assert!(HIGH_RISK.contains(&"window.management"));
    assert!(HIGH_RISK.contains(&"power.control"));
    assert!(HIGH_RISK.contains(&"mail.read"));
    assert!(!HIGH_RISK.contains(&"media.control"));
    assert!(!HIGH_RISK.contains(&"reminders.write"));
    assert!(!HIGH_RISK.contains(&"system.settings"));
    assert!(!HIGH_RISK.contains(&"printer.control"));
    // v1.5: Things reads and writes are both non-high-risk (ask + Always allowed)
    assert!(!HIGH_RISK.contains(&"things.read"));
    assert!(!HIGH_RISK.contains(&"things.write"));
}

#[test]
fn resolve_ask_delivers_answer_once() {
    let (tx, rx) = mpsc::channel();
    asks().lock().unwrap().insert("perm-t".into(), tx);
    assert!(resolve_ask("perm-t", "always"));
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "always");
    assert!(!resolve_ask("perm-t", "always")); // already consumed
}

/// F12 — scripted stub: returns preset results in order; available is fixed (for fail-closed with no UI).
struct ScriptAsker {
    available: bool,
    answers: Mutex<std::collections::VecDeque<AskOutcome>>,
}

impl ScriptAsker {
    fn new(available: bool, answers: Vec<AskOutcome>) -> Self {
        Self {
            available,
            answers: Mutex::new(answers.into()),
        }
    }
}

impl Asker for ScriptAsker {
    fn is_available(&self) -> bool {
        self.available
    }
    fn ask(&self, _req: &AskRequest) -> AskOutcome {
        self.answers
            .lock()
            .ok()
            .and_then(|mut q| q.pop_front())
            .unwrap_or(AskOutcome::Timeout)
    }
}

/// F12 four branches (gate): allow once / Always persisted / Always downgraded (high-risk) / deny / timeout +
/// fail-closed with no UI retained.
#[test]
fn gate_asker_four_branches() {
    let dir = std::env::temp_dir().join(format!("opencapx-gate-asker-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
        super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )));
    // allow once → Granted, not persisted
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("once".into())]);
    assert_eq!(
        gate_with(&asker, &s, "p1", "image.read", "capability", None),
        Decision::Granted
    );
    assert_eq!(check(&s, "p1", "image.read"), Decision::Ask);
    // Always (Always allowed) → Granted and persisted as granted
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("always".into())]);
    assert_eq!(
        gate_with(&asker, &s, "p2", "image.read", "capability", None),
        Decision::Granted
    );
    assert_eq!(check(&s, "p2", "image.read"), Decision::Granted);
    // Always (high-risk, Always not allowed) → downgrade to once: Granted but not persisted
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("always".into())]);
    assert_eq!(
        gate_with(&asker, &s, "p3", "automation.control", "capability", None),
        Decision::Granted
    );
    assert_eq!(check(&s, "p3", "automation.control"), Decision::Ask);
    // deny → Denied
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("deny".into())]);
    assert_eq!(
        gate_with(&asker, &s, "p4", "image.read", "capability", None),
        Decision::Denied
    );
    // timeout → Denied
    let asker = ScriptAsker::new(true, vec![AskOutcome::Timeout]);
    assert_eq!(
        gate_with(&asker, &s, "p5", "image.read", "capability", None),
        Decision::Denied
    );
    // no UI → fail-closed (behavior unchanged)
    let asker = ScriptAsker::new(false, vec![]);
    assert_eq!(
        gate_with(&asker, &s, "p6", "image.read", "capability", None),
        Decision::Denied
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// F12 four branches (confirm_install): Always / once / declared downgrade / deny / timeout +
/// no UI goes through the non-interactive default table (behavior unchanged).
#[test]
fn confirm_install_asker_four_branches() {
    let plan = vec![ask_builtin("image.read")];
    // Always (allowed) → granted
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("always".into())]);
    let d = confirm_install_with(&asker, "com.x", &plan).unwrap();
    assert_eq!(d, vec![("image.read".to_string(), "granted".to_string())]);
    // once → ask
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("once".into())]);
    let d = confirm_install_with(&asker, "com.x", &plan).unwrap();
    assert_eq!(d, vec![("image.read".to_string(), "ask".to_string())]);
    // declared-derived + Always → downgrade to ask (do not reject the whole batch)
    let plan_decl = vec![ask_declared("x.read", "ask")];
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("always".into())]);
    let d = confirm_install_with(&asker, "com.x", &plan_decl).unwrap();
    assert_eq!(d, vec![("x.read".to_string(), "ask".to_string())]);
    // deny → Err (whole-batch failure)
    let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("deny".into())]);
    let err = confirm_install_with(&asker, "com.x", &plan).unwrap_err();
    assert!(err.contains("install denied"), "got: {}", err);
    // timeout → Err
    let asker = ScriptAsker::new(true, vec![AskOutcome::Timeout]);
    let err = confirm_install_with(&asker, "com.x", &plan).unwrap_err();
    assert!(err.contains("timed out"), "got: {}", err);
    // no UI → non-interactive default table (image.read → ask)
    let asker = ScriptAsker::new(false, vec![]);
    let d = confirm_install_with(&asker, "com.x", &plan).unwrap();
    assert_eq!(d, vec![("image.read".to_string(), "ask".to_string())]);
}

/// Test helper: confirmation item for a built-in permission.
fn ask_builtin(p: &str) -> InstallAsk {
    InstallAsk {
        permission: p.to_string(),
        declared: false,
        declared_default: "ask".into(),
    }
}

/// Test helper: confirmation item for a declared-derived permission (the default written in the manifest).
fn ask_declared(p: &str, default: &str) -> InstallAsk {
    InstallAsk {
        permission: p.to_string(),
        declared: true,
        declared_default: default.into(),
    }
}

#[test]
fn confirm_install_noninteractive_persists_defaults() {
    let dir = std::env::temp_dir().join(format!("opencapx-confirminst-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
        super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )));
    // pet.animation default granted → granted; image.read default ask → ask
    let d = confirm_install_noninteractive(
        "com.x",
        &[ask_builtin("pet.animation"), ask_builtin("image.read")],
    )
    .expect("noninteractive confirm");
    assert_eq!(
        d,
        vec![
            ("pet.animation".to_string(), "granted".to_string()),
            ("image.read".to_string(), "ask".to_string()),
        ]
    );
    // §4.4 consent-before-commit: the confirmation phase **does not write to the DB**
    assert_eq!(check(&s, "com.x", "pet.animation"), Decision::Granted); // still the default-table answer
    let rows_before = permission_row_count(&s, "com.x");
    assert_eq!(rows_before, 0, "confirm must not touch the DB");
    // only the commit phase persists
    commit_install_decisions(&s, "com.x", &d).expect("commit");
    assert_eq!(permission_row_count(&s, "com.x"), 2);
    assert_eq!(check(&s, "com.x", "image.read"), Decision::Ask);
    // unknown names are rejected outright (and not written)
    let err = confirm_install_noninteractive("com.x", &[ask_builtin("nope.perm")]).unwrap_err();
    assert!(err.contains("unknown permission"));
    assert_eq!(permission_row_count(&s, "com.x"), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// §4.3 integration point #3: a declared-derived permission's default comes from the **manifest inline declaration**, not the static table.
#[test]
fn declared_permission_uses_manifest_default() {
    let d = confirm_install_noninteractive(
        "com.weather",
        &[
            ask_declared("weather.read", "ask"),
            ask_declared("weather.admin", "denied"),
        ],
    )
    .expect("noninteractive confirm");
    assert_eq!(
        d,
        vec![
            ("weather.read".to_string(), "ask".to_string()),
            ("weather.admin".to_string(), "denied".to_string()),
        ]
    );
    // declared items are never persisted as granted
    assert!(d.iter().all(|(_, dec)| dec != "granted"));
}

/// §4.3 enforcement points 1/2 criterion: neither high-risk nor declared-derived may be Always; ordinary built-in permissions may.
#[test]
fn once_only_judgement_covers_declared_and_high_risk() {
    let dir = std::env::temp_dir().join(format!("opencapx-onceonly-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
        super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )));
    // built-in ordinary → Always allowed; built-in high-risk → not allowed
    assert!(can_always(&s, "image.read"));
    assert!(!can_always(&s, "process.execute"));
    assert!(!can_always(&s, "filesystem.write"));
    // declared-derived → not allowed (even when absent from HIGH_RISK)
    declare_for_test(&s, "com.weather", "weather.fetch", "weather.read", "ask");
    assert!(
        !can_always(&s, "weather.read"),
        "declared permission must be once-only"
    );
    // §4.3 enforcement point 3: neither settings page nor install writeback may write it as granted
    assert!(!set_decision(&s, "com.weather", "weather.read", "granted"));
    assert!(set_decision(&s, "com.weather", "weather.read", "denied"));
    assert_eq!(check(&s, "com.weather", "weather.read"), Decision::Denied);
    // integration point #2: default comes from the declaration when no DB override is persisted
    assert_eq!(default_decision_for(&s, "weather.read"), Decision::Ask);
    assert_eq!(default_decision_for(&s, "image.read"), Decision::Ask);
    assert_eq!(default_decision_for(&s, "nope.nope"), Decision::Denied);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Test helper: writes a frozen declaration directly (bypassing the install flow).
fn declare_for_test(s: &SharedStore, plugin: &str, cap: &str, perm: &str, default: &str) {
    let rows = vec![(cap.to_string(), perm.to_string(), default.to_string(), None)];
    let mut st = s.lock().unwrap();
    st.try_with_conn(|c| {
        let tx = c.unchecked_transaction().unwrap();
        super::super::declaration::write_in_tx(&tx, plugin, &rows, 1)?;
        tx.commit().unwrap();
        Ok(())
    })
    .unwrap()
    .unwrap();
}

/// §4.4 step 6 regression: an error in the confirmation phase → zero changes in the DB (consent-before-commit).
/// The interactive path's deny/timeout branches are isomorphic to this (likewise Err only, no writes).
#[test]
fn consent_before_commit_leaves_store_untouched_on_deny() {
    let dir = std::env::temp_dir().join(format!("opencapx-consent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
        super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )));
    assert!(confirm_install_noninteractive("com.x", &[ask_builtin("nope.perm")]).is_err());
    assert_eq!(permission_row_count(&s, "com.x"), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Commit-phase boundaries: empty list is a no-op; repeated commits overwrite rather than insert.
/// (whole-batch rollback is guaranteed by `unchecked_transaction`'s drop-rollback semantics; this does not fabricate
/// a failure case — the permission table only has PK / NOT NULL constraints, so a mid-way failure cannot be constructed.)
#[test]
fn commit_install_decisions_is_idempotent_and_noop_on_empty() {
    let dir = std::env::temp_dir().join(format!("opencapx-commit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
        super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )));
    let ok = vec![("image.read".to_string(), "ask".to_string())];
    commit_install_decisions(&s, "com.x", &ok).unwrap();
    assert_eq!(permission_row_count(&s, "com.x"), 1);
    commit_install_decisions(&s, "com.x", &[]).unwrap();
    assert_eq!(permission_row_count(&s, "com.x"), 1);
    // repeated commit = overwrite, not insert
    commit_install_decisions(&s, "com.x", &ok).unwrap();
    assert_eq!(permission_row_count(&s, "com.x"), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Deduplicated in-tx helper: writes two decisions in an explicit transaction and reads them back after commit.
#[test]
fn upsert_install_decisions_in_tx_writes_and_overwrites() {
    use crate::core::storage::{Storage, StoreEnum};
    let dir = std::env::temp_dir().join(format!("opencapx-upsert-tx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = StoreEnum::Db(Storage::open(&dir.join("t.db")).unwrap());
    let decisions = vec![("image.read".to_string(), "ask".to_string())];
    s.try_with_conn(|c| {
        let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
        upsert_install_decisions_in_tx(&tx, "com.x", &decisions, 1)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    })
    .unwrap()
    .unwrap();
    // overwrite write: same-key update does not insert
    s.try_with_conn(|c| {
        let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
        upsert_install_decisions_in_tx(&tx, "com.x", &[("image.read".into(), "denied".into())], 2)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    })
    .unwrap()
    .unwrap();
    let count: i64 = s
        .try_with_conn(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM plugin_permissions WHERE plugin_id = 'com.x'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
        })
        .unwrap()
        .unwrap();
    assert_eq!(count, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Test helper: count the rows a plugin has in plugin_permissions.
fn permission_row_count(s: &SharedStore, plugin_id: &str) -> usize {
    s.lock()
        .ok()
        .and_then(|st| {
            st.with_conn_ref(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM plugin_permissions WHERE plugin_id = ?1",
                    params![plugin_id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0) as usize
            })
        })
        .unwrap_or(0)
}

#[test]
fn sqlite_decision_override() {
    let dir = std::env::temp_dir().join(format!("opencapx-perm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
        super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )));
    assert_eq!(check(&s, "com.x", "image.read"), Decision::Ask);
    assert!(set_decision(&s, "com.x", "image.read", "granted"));
    assert_eq!(check(&s, "com.x", "image.read"), Decision::Granted);
    assert_eq!(
        gate(&s, "com.x", "image.read", "capability", None),
        Decision::Granted
    );
    assert!(set_decision(&s, "com.x", "image.read", "denied"));
    assert_eq!(
        gate(&s, "com.x", "image.read", "capability", None),
        Decision::Denied
    );
    assert!(!set_decision(&s, "com.x", "nope.nope", "granted"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn view_groups_overrides_defaults_and_high_risk() {
    let dir = std::env::temp_dir().join(format!("opencapx-permview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
        super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )));
    let plugins = vec![crate::core::plugin::PluginStatusDto {
        id: "com.x".into(),
        name: "X".into(),
        description: None,
        author: None,
        homepage: None,
        license: None,
        version: "0.1.0".into(),
        ptype: "capability".into(),
        status: "running".into(),
        capabilities: vec![],
        permissions: vec!["image.read".into(), "process.execute".into()],
        path: None,
        auto_reload: false,
        probe_status: None,
        probe_at: None,
        channel: None,
        sandbox_declared: false,
        health_heartbeat_sec: None,
        health_max_retries: None,
        health_enabled: None,
        missing_dependencies: vec![],
        revoked_key: None,
        revoked_at: None,
    }];
    let v = view(&s, &plugins);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].plugin_id, "com.x");
    assert_eq!(v[0].permissions[0].decision, "ask");
    assert_eq!(v[0].permissions[0].default, "ask");
    assert!(!v[0].permissions[0].high_risk);
    assert!(v[0].permissions[1].high_risk);
    assert!(set_decision(&s, "com.x", "image.read", "granted"));
    let v2 = view(&s, &plugins);
    assert_eq!(v2[0].permissions[0].decision, "granted");
    let _ = std::fs::remove_dir_all(&dir);
}

/// agent_view: full vocabulary + agent override > default table + high_risk flag.
#[test]
fn agent_view_lists_vocabulary_with_overrides() {
    let dir = std::env::temp_dir().join(format!("opencapx-agentview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
        super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )));
    let (agent_id, _) = super::super::identity::register(&s, "claude", "mcp").expect("register");
    let v = agent_view(&s, &agent_id);
    assert_eq!(v.len(), PERMISSIONS.len(), "full vocabulary");
    let pet = v.iter().find(|e| e.permission == "pet.animation").unwrap();
    assert_eq!(pet.decision, "granted");
    assert_eq!(pet.default, "granted");
    assert!(!pet.high_risk);
    let cam = v.iter().find(|e| e.permission == "camera").unwrap();
    assert_eq!(cam.decision, "denied", "default table denied");
    assert!(cam.high_risk);
    // after override the decision changes but default does not
    assert!(super::super::identity::set_agent_decision(
        &s, &agent_id, "camera", "ask"
    ));
    let v2 = agent_view(&s, &agent_id);
    let cam2 = v2.iter().find(|e| e.permission == "camera").unwrap();
    assert_eq!(cam2.decision, "ask");
    assert_eq!(cam2.default, "denied", "default keeps the table value");
    let _ = std::fs::remove_dir_all(&dir);
}

/// gate()'s Ask + no-UI path emits a permission.denied audit + carries the reason into the payload.
/// Verifies that plugin.reverse's reason field reaches the audit.
#[test]
fn gate_no_ui_emits_audit_with_reason() {
    let rx = super::super::event::EventBus::shared().subscribe();
    let s = mem_store();
    let d = gate(
        &s,
        "com.x",
        "image.read",
        "plugin.reverse",
        Some("analyze screenshot"),
    );
    assert_eq!(d, Decision::Denied);
    // the shared bus sees other events under parallel tests (including sibling tests also using com.x); filter by pluginId + caller.
    let ev = loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(e)
                if e.kind == "permission.denied"
                    && e.payload.get("pluginId").and_then(|v| v.as_str()) == Some("com.x")
                    && e.payload.get("caller").and_then(|v| v.as_str())
                        == Some("plugin.reverse") =>
            {
                break e;
            }
            Ok(_) => continue,
            Err(_) => panic!("no permission.denied audit for com.x in 500ms"),
        }
    };
    assert_eq!(
        ev.payload.get("pluginId").and_then(|v| v.as_str()),
        Some("com.x")
    );
    assert_eq!(
        ev.payload.get("permission").and_then(|v| v.as_str()),
        Some("image.read")
    );
    assert_eq!(
        ev.payload.get("requestReason").and_then(|v| v.as_str()),
        Some("analyze screenshot"),
    );
    assert_eq!(
        ev.payload.get("reason").and_then(|v| v.as_str()),
        Some("no-ui")
    );
    assert_eq!(
        ev.payload.get("caller").and_then(|v| v.as_str()),
        Some("plugin.reverse")
    );
}

/// When no reason is passed, the audit payload must not contain a requestReason field.
#[test]
fn gate_without_reason_omits_request_reason() {
    let rx = super::super::event::EventBus::shared().subscribe();
    let s = mem_store();
    let _ = gate(&s, "com.x", "image.read", "capability", None);
    let ev = loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(e)
                if e.kind == "permission.denied"
                    && e.payload.get("pluginId").and_then(|v| v.as_str()) == Some("com.x")
                    && e.payload.get("caller").and_then(|v| v.as_str()) == Some("capability") =>
            {
                break e;
            }
            Ok(_) => continue,
            Err(_) => panic!("no capability permission.denied for com.x in 500ms"),
        }
    };
    assert!(ev.payload.get("requestReason").is_none());
    assert_eq!(
        ev.payload.get("caller").and_then(|v| v.as_str()),
        Some("capability")
    );
}

/// Phase 33 — heatmap() must correctly read the whole plugin_permissions table + flag high_risk + aggregate top_denied.
/// Fills a temp DB with 7 decision rows, runs heatmap()'s SQL/aggregation, and asserts cell count + top ordering + high_risk.
#[test]
fn heatmap_reads_grid_and_aggregates_top_denied() {
    use crate::core::storage::{SharedStore, StoreEnum};
    let dir = std::env::temp_dir().join(format!("opencapx-heatmap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(StoreEnum::Db(
        crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )));

    let p_a = "com.opencapx.heat-a";
    let p_b = "com.opencapx.heat-b";
    let p_c = "com.opencapx.heat-c";
    let rows: &[(&str, &str, &str)] = &[
        (p_a, "image.read", "granted"),
        (p_a, "camera", "granted"),
        (p_a, "filesystem.write", "denied"),
        (p_b, "image.read", "denied"),
        (p_b, "camera", "ask"),
        (p_c, "browser.open", "granted"),
        (p_c, "microphone", "ask"),
    ];
    {
        let mut s = store.lock().unwrap();
        s.with_conn(|c| {
                for (pid, perm, dec) in rows {
                    c.execute(
                        "INSERT INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at)
                         VALUES (?1, ?2, NULL, ?3, 0)",
                        rusqlite::params![pid, perm, dec],
                    )
                    .unwrap_or(0);
                }
                0usize
            });
    }

    // mirror heatmap()'s read + aggregation path
    type TopRow = (String, i64, i64, i64, bool);
    let (cells_count, mut top): (usize, Vec<TopRow>) = {
        let s = store.lock().unwrap();
        s.with_conn_ref(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT plugin_id, permission, decision FROM plugin_permissions
                         ORDER BY plugin_id ASC, permission ASC",
                )
                .unwrap();
            let cells: Vec<(String, String, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .filter_map(|x| x.ok())
                .collect();
            let mut agg: std::collections::BTreeMap<String, (i64, i64, i64)> =
                std::collections::BTreeMap::new();
            for (_, perm, dec) in &cells {
                let e = agg.entry(perm.clone()).or_insert((0, 0, 0));
                match dec.as_str() {
                    "granted" => e.0 += 1,
                    "denied" => e.1 += 1,
                    "ask" => e.2 += 1,
                    _ => {}
                }
            }
            let mut t: Vec<TopRow> = agg
                .into_iter()
                .map(|(perm, (g, d, a))| {
                    let high = HIGH_RISK.contains(&perm.as_str());
                    (perm, g, d, a, high)
                })
                .collect();
            t.sort_by(|x, y| y.2.cmp(&x.2).then(y.1.cmp(&x.1)).then(x.0.cmp(&y.0)));
            (cells.len(), t)
        })
        .unwrap_or((0, vec![]))
    };

    assert_eq!(cells_count, 7, "7 decision rows");
    assert_eq!(top.len(), 5, "5 distinct permissions");

    // denied descending: filesystem.write(1) + image.read(1) tie for first, then granted descending, then name ascending
    // → image.read(granted=1) > filesystem.write(granted=0)
    assert_eq!(top[0].0, "image.read");
    assert_eq!(top[0].1, 1, "image.read granted=1");
    assert_eq!(top[0].2, 1, "image.read denied=1");
    assert!(!top[0].4, "image.read is not high_risk");

    assert_eq!(top[1].0, "filesystem.write");
    assert_eq!(top[1].2, 1);
    assert!(top[1].4, "filesystem.write is high_risk");

    // high-risk entries must be flagged
    let camera = top.iter().find(|(p, _, _, _, _)| p == "camera").unwrap();
    assert_eq!(camera.1, 1, "camera granted=1");
    assert_eq!(camera.3, 1, "camera ask=1");
    assert!(camera.4, "camera high_risk");

    let mic = top
        .iter()
        .find(|(p, _, _, _, _)| p == "microphone")
        .unwrap();
    assert_eq!(mic.3, 1, "microphone ask=1");
    assert!(mic.4, "microphone high_risk");

    let browser = top
        .iter()
        .find(|(p, _, _, _, _)| p == "browser.open")
        .unwrap();
    assert_eq!(browser.1, 1);
    assert!(!browser.4, "browser.open is not high_risk");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 2026-09-16 regression — real DB + permission rows + declaration rows, calling the real heatmap():
/// the old implementation called is_declared_permission per cell inside the closure (re-locking the store) → same-thread reentrant deadlock,
/// freezing the plugin tab and the whole app (fix: separate row reading from the declaration-set query so the lock is not reentrant).
/// Under the old code this test hangs forever; now it must return in milliseconds with correct declared flags.
#[test]
fn heatmap_with_db_rows_and_declarations_does_not_deadlock() {
    use crate::core::storage::{SharedStore, StoreEnum};
    let _g = crate::core::TEST_STORE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("opencapx-hm-deadlock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(StoreEnum::Db(
        crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )));
    crate::core::set_shared_store(store.clone());
    let pid = "com.opencapx.hm-deadlock";
    {
        let mut s = store.lock().unwrap();
        let _ = s.with_conn(|c| {
                c.execute(
                    "INSERT INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at)
                     VALUES (?1, 'demo.perm', NULL, 'granted', 0)",
                    rusqlite::params![pid],
                )
                .unwrap_or(0)
                    + c.execute(
                        "INSERT INTO capability_declarations (capability, plugin_id, permission, default_decision, confirmed_at)
                         VALUES ('demo.cap', ?1, 'demo.perm', 'ask', 0)",
                        rusqlite::params![pid],
                    )
                    .unwrap_or(0)
            });
    }
    let t0 = std::time::Instant::now();
    let dto = heatmap();
    let elapsed = t0.elapsed();
    assert_eq!(dto.cells.len(), 1, "one permission row");
    assert!(dto.cells[0].declared, "demo.perm is declared");
    assert_eq!(dto.top_denied.len(), 1, "aggregates to one row");
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "heatmap must be fast: {:?} (a hang means the reentrant-deadlock regression)",
        elapsed
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// DB store used by global policy tests (tag-isolated to avoid parallel tests colliding on the DB).
fn policy_store(tag: &str) -> SharedStore {
    let dir = std::env::temp_dir().join(format!("opencapx-policy-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Arc::new(Mutex::new(StoreEnum::Db(
        super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
    )))
}

/// Global policy setter validation: unknown permission / invalid tier / high-risk granted all rejected; valid tiers write and read back.
#[test]
fn global_policy_setter_validates_and_roundtrips() {
    let s = policy_store("setter");
    assert!(set_global_override(&s, "nope.perm", "denied").is_err());
    assert!(set_global_override(&s, "clipboard.read", "yolo").is_err());
    // high-risk cannot be globally granted (only once-at-a-time grants allowed)
    assert!(set_global_override(&s, "process.execute", "granted").is_err());
    assert!(set_global_override(&s, "input.control", "granted").is_err());
    // no override → empty string; write → read back
    assert_eq!(global_override(&s, "clipboard.read"), None);
    assert_eq!(global_override_str(&s, "clipboard.read"), "");
    assert!(set_global_override(&s, "clipboard.read", "denied").is_ok());
    assert_eq!(
        global_override(&s, "clipboard.read"),
        Some(Decision::Denied)
    );
    assert_eq!(global_override_str(&s, "clipboard.read"), "denied");
    // writing again = overwrite the same row, not insert
    assert!(set_global_override(&s, "clipboard.read", "ask").is_ok());
    assert_eq!(global_override(&s, "clipboard.read"), Some(Decision::Ask));
    // high-risk can be denied / ask
    assert!(set_global_override(&s, "process.execute", "denied").is_ok());
    assert!(set_global_override(&s, "process.execute", "ask").is_ok());
    // reset → back to no override; repeated resets are idempotent
    assert!(clear_global_override(&s, "clipboard.read").is_ok());
    assert_eq!(global_override(&s, "clipboard.read"), None);
    assert_eq!(global_override_str(&s, "clipboard.read"), "");
    assert!(clear_global_override(&s, "clipboard.read").is_ok());
}

/// Contract with the frontend settings.ts: the JSON key is override (not overrideDecision),
/// and None serializes to null (the frontend treats null as "follow the default").
#[test]
fn core_policy_dto_wire_keys_match_frontend_contract() {
    let dto = CorePermPolicyDto {
        permission: "clipboard.read".into(),
        capabilities: vec!["clipboard.read".into(), "context.get_current".into()],
        builtin_default: "ask".into(),
        override_decision: Some("denied".into()),
        effective: "denied".into(),
        high_risk: false,
    };
    let v = serde_json::to_value(&dto).unwrap();
    assert_eq!(v["permission"], "clipboard.read");
    assert_eq!(v["capabilities"][1], "context.get_current");
    assert_eq!(v["builtinDefault"], "ask");
    assert_eq!(v["override"], "denied");
    assert_eq!(v["effective"], "denied");
    assert_eq!(v["highRisk"], false);
    assert!(
        v.get("overrideDecision").is_none(),
        "the key name must be override"
    );
    let none = CorePermPolicyDto {
        override_decision: None,
        ..dto.clone()
    };
    assert!(serde_json::to_value(&none).unwrap()["override"].is_null());
    // key-set check: extra keys / missing keys / renames must all fail (a wrong name silently yields undefined on the frontend).
    // compare sets, not order — serde_json outputs in BTreeMap alphabetical order by default, so key order is not part of the contract.
    let keys: std::collections::BTreeSet<&str> =
        v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    let expected: std::collections::BTreeSet<&str> = [
        "permission",
        "capabilities",
        "builtinDefault",
        "override",
        "effective",
        "highRisk",
    ]
    .into_iter()
    .collect();
    assert_eq!(keys, expected);
}

/// Mem variant (no DB): reads back no override, writes report storage unavailable.
#[test]
fn global_policy_mem_store_is_unavailable() {
    let s = mem_store();
    assert_eq!(global_override(&s, "clipboard.read"), None);
    assert!(set_global_override(&s, "clipboard.read", "denied").is_err());
    assert!(clear_global_override(&s, "clipboard.read").is_err());
}

/// Reverse mapping: every capability is covered by exactly one permission (system.permission_status is gate-free, the sole exception).
#[test]
fn core_capabilities_for_covers_every_capability() {
    for cap in super::super::capability::CAPABILITY_IDS {
        match capability_permission(cap) {
            Some(perm) => assert!(
                core_capabilities_for(perm).contains(cap),
                "{cap} should appear in {perm}'s capability list"
            ),
            None => assert_eq!(cap, &"system.permission_status"),
        }
    }
    assert_eq!(
        core_capabilities_for("clipboard.read"),
        vec!["clipboard.read", "context.get_current"]
    );
    // plugin.install is a vocabulary placeholder with no capability mapping
    assert!(core_capabilities_for("plugin.install").is_empty());
}

/// Settings page listing: only permissions with a capability mapping; alphabetical; override/high-risk/default tier all included.
#[test]
fn core_policy_list_only_lists_mapped_permissions() {
    let s = policy_store("list");
    assert!(set_global_override(&s, "clipboard.read", "denied").is_ok());
    let list = core_policy_list(&s);
    let find = |p: &str| list.iter().find(|e| e.permission == p);
    let cb = find("clipboard.read").expect("clipboard.read should be listed");
    assert_eq!(
        cb.capabilities,
        vec![
            "clipboard.read".to_string(),
            "context.get_current".to_string()
        ]
    );
    assert_eq!(cb.builtin_default, "ask");
    assert_eq!(cb.override_decision.as_deref(), Some("denied"));
    assert_eq!(cb.effective, "denied");
    assert!(!cb.high_risk);
    // no mapping → not listed
    assert!(find("pet.animation").is_none());
    assert!(find("storage.local").is_none());
    assert!(find("plugin.install").is_none());
    // high-risk: tier included, default follows the static table, and effective = built-in default when not overridden
    let fw = find("filesystem.write").expect("filesystem.write should be listed");
    assert!(fw.high_risk);
    assert_eq!(fw.builtin_default, "denied");
    assert_eq!(fw.override_decision, None);
    assert_eq!(fw.effective, "denied");
    // permissions with no capability mapping are never listed — no capability gate triggers them,
    // so listing them is a "click and nothing happens" fake switch (process/microphone/camera etc.)
    assert!(find("process.execute").is_none());
    assert!(find("microphone").is_none());
    assert!(find("camera").is_none());
    assert!(find("network.request").is_none());
    // alphabetical order
    let got: Vec<String> = list.iter().map(|e| e.permission.clone()).collect();
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(got, sorted);
    // the listing = the full set of "permissions with a mapping": neither extra (fake switches) nor missing (omitted switches).
    // do not hardcode the count — this invariant follows automatically when capabilities are added.
    let mapped: std::collections::BTreeSet<String> = super::super::capability::CAPABILITY_IDS
        .iter()
        .filter_map(|c| capability_permission(c))
        .map(|p| p.to_string())
        .collect();
    let listed: std::collections::BTreeSet<String> =
        list.iter().map(|e| e.permission.clone()).collect();
    assert_eq!(listed, mapped);
    // reset → override returns to None, effective back to the built-in default
    assert!(clear_global_override(&s, "clipboard.read").is_ok());
    let cb2 = core_policy_list(&s)
        .into_iter()
        .find(|e| e.permission == "clipboard.read")
        .expect("still listed");
    assert_eq!(cb2.override_decision, None);
    assert_eq!(cb2.effective, "ask");
}
