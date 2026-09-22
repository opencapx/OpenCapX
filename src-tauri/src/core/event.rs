//! Event Bus: unified event structure, synchronous broadcast, absorbing the existing ingest path.
//! See docs/events.md.

use super::agent::{dto_to_session, process_body, session_to_dto, Session, SessionDto, SessionSink};
use super::storage::SharedStore;
use serde::{Deserialize, Serialize};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpencapxEvent {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub source: String,
    pub timestamp: u64,
    pub payload: serde_json::Value,
}

impl OpencapxEvent {
    pub fn new(kind: &str, source: &str, payload: serde_json::Value) -> Self {
        // Q2 — concurrent generation in the same nanosecond collides on id (INSERT OR REPLACE silently overwrites and drops events):
        // a monotonic sequence is folded into the id to guarantee in-process uniqueness.
        static EVENT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = EVENT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!(
            "evt-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            seq
        );
        Self {
            id,
            kind: kind.to_string(),
            source: source.to_string(),
            timestamp: super::agent::now_secs(),
            payload,
        }
    }
}

/// hook state → event type (idle is treated as session start).
pub fn agent_event_type(state: &str) -> &'static str {
    match state {
        "working" => "agent.working",
        "waiting" => "agent.waiting",
        "done" => "agent.completed",
        _ => "agent.started",
    }
}

/// Synchronous broadcast bus: each subscriber holds its own channel, publish delivers non-blocking.
pub struct EventBus {
    subs: Mutex<Vec<Sender<OpencapxEvent>>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            subs: Mutex::new(Vec::new()),
        }
    }

    pub fn shared() -> Arc<Self> {
        static BUS: OnceLock<Arc<EventBus>> = OnceLock::new();
        BUS.get_or_init(|| Arc::new(EventBus::new())).clone()
    }

    pub fn subscribe(&self) -> Receiver<OpencapxEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        if let Ok(mut subs) = self.subs.lock() {
            subs.push(tx);
        }
        rx
    }

    pub fn publish(&self, e: &OpencapxEvent) {
        if let Ok(mut subs) = self.subs.lock() {
            subs.retain(|tx| tx.send(e.clone()).is_ok());
        }
    }
}

/// Start bus subscriber threads: events land in SQLite, emit to the `opencapx-event` channel.
pub fn spawn_subscribers(handle: tauri::AppHandle, store: SharedStore, bus: Arc<EventBus>) {
    // Automation(§15): Event → Rule → Action, built on the same Bus
    super::automation::start(handle.clone(), bus.clone());

    let w_store = store;
    let w_bus = bus.clone();
    std::thread::spawn(move || {
        for e in w_bus.subscribe() {
            if let Ok(mut s) = w_store.lock() {
                s.log_event(&e);
            }
        }
    });

    let bus_for_sse = bus.clone();
    std::thread::spawn(move || {
        for e in bus_for_sse.subscribe() {
            use tauri::Emitter;
            let _ = handle.emit("opencapx-event", &e);
        }
    });

    // Phase 34 — every event lands as NDJSON in ~/.opencapx/replay/<session>.ndjson (replayable).
    std::thread::spawn(move || {
        for e in bus.subscribe() {
            super::event_replay::record(&e);
        }
    });
}

/// After CLI rewriting, the matched rule id is injected into `__rule` (same mechanism as `__sent_at`). It is split out here to publish
/// `rule.applied` into the Activity Timeline — so the user can see "which command was changed by which rule".
fn publish_rule_audit(bus: &EventBus, body: &str, default_agent: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };
    let Some(rule_id) = v.get("__rule").and_then(|x| x.as_str()) else {
        return;
    };
    let agent = v
        .get("agent")
        .and_then(|x| x.as_str())
        .unwrap_or(default_agent);
    let command = v
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let mut payload = serde_json::json!({ "ruleId": rule_id, "agent": agent, "command": command });
    // Optional context (e.g. why a run executed unguarded — "sandbox.unguarded" carries the
    // backend failure reason); absent for ordinary rule hits.
    if let Some(r) = v.get("reason").and_then(|x| x.as_str()) {
        payload["reason"] = serde_json::json!(r);
    }
    bus.publish(&OpencapxEvent::new("rule.applied", "core", payload));
}

/// Handle one event body: store + bus + notify + tray tooltip.
/// Formerly http::ingest, absorbed as the Bus entry point.
/// caller_agent_id: the agent identity after /event authentication (passed in by http.rs); hook traces are archived by
/// it — the viewer's query key is agent_id; None = anonymous/test, falling back to dto.agent kind.
pub fn ingest(
    handle: Option<&tauri::AppHandle>,
    store: &SharedStore,
    bus: &EventBus,
    body: &str,
    default_agent: &str,
    caller_agent_id: Option<&str>,
) {
    // Events that must be ignored explicitly (e.g. SubagentStop: a subagent finishing does not mean the main session changed),
    // dropped outright: no DB write, no notification, no state change.
    if super::agent::should_ignore_event(body) {
        return;
    }
    publish_rule_audit(bus, body, default_agent);
    let mut dto = process_body(body, default_agent);
    // Fill in the model name, and for Stop re-check "done / asking" (read the transcript tail).
    // If unreadable, skip silently; event handling is unaffected.
    super::agent::enrich_from_transcript(&mut dto, body);
    eprintln!("[ingest] id={} agent={} state={}", dto.id, dto.agent, dto.state);
    let prev_session = store.lock().ok().and_then(|s| s.get(&dto.id));
    let prev: Option<String> = prev_session
        .as_ref()
        .map(|old| super::agent::state_str(old.state).to_string());
    // Sticky fields: hook payloads mostly carry no model / body, so do not overwrite known content with empty values.
    // (The body only exists at the moment the transcript is read; later tool events must not wipe it.)
    if dto.model.is_empty() {
        if let Some(old) = prev_session.as_ref() {
            dto.model = old.model.clone();
        }
    }
    if dto.speech.is_empty() {
        if let Some(old) = prev_session.as_ref() {
            dto.speech = old.speech.clone();
        }
    }
    if dto.cwd.is_empty() {
        if let Some(old) = prev_session.as_ref() {
            dto.cwd = old.cwd.clone();
        }
    }
    // The sort key travels with the event to the frontend: the frontend only compares keys, it does not reimplement the sort policy.
    dto.order = super::agent::order_key_for_dto(&dto);
    if let Ok(mut s) = store.lock() {
        s.upsert(dto_to_session(&dto));
    }
    // trace: hook events land in per-session trace files (alongside /rpc traces, aligned in time for the viewer).
    // Archive key = authenticated agent_id (the viewer queries by agent_id); anonymous falls back to kind — both keys
    // are queryable, but the viewer only hits the former; anonymous streams are only for humans browsing directories.
    super::req_trace::hook_event(
        caller_agent_id.unwrap_or(&dto.agent),
        &dto.id,
        agent_event_type(&dto.state),
        serde_json::json!({
            "state": dto.state,
            "message": dto.message,
            "project": super::project::short_path(&dto.cwd),
        }),
    );
    let source = format!("hooks:{}", dto.agent);
    bus.publish(&OpencapxEvent::new(
        agent_event_type(&dto.state),
        &source,
        serde_json::to_value(&dto).unwrap_or(serde_json::Value::Null),
    ));
    if let Some(h) = handle {
        let transitioned = prev.as_deref() != Some(dto.state.as_str());
        if transitioned && (dto.state == "waiting" || dto.state == "done") {
            use tauri_plugin_notification::NotificationExt;
            // Notification copy follows the user language (the native copy tables live in core::i18n); the title uses the display name
            let locale = crate::read_settings_file()
                .get("locale")
                .and_then(|v| v.as_str())
                .unwrap_or("en")
                .to_string();
            let strings = super::i18n::strings(super::i18n::from_locale(&locale));
            let text = crate::notify::notify_copy(strings, &dto.agent, &dto.state, &dto.message);
            let _ = h
                .notification()
                .builder()
                .title(crate::hooks::display_name(&dto.agent))
                .body(text)
                .show();
        }
    }
    // The tray (menu list / icon status dot / tooltip) is refreshed uniformly by main::refresh_tray_menu
    // through the event bus: one source for all three, avoiding a second copy here that would overwrite it.
}

/// A say event for internal entry points like /rpc to reuse (the pet bubble speaks).
pub fn publish_say(handle: &tauri::AppHandle, bus: &EventBus, text: &str) {
    use tauri::Emitter;
    bus.publish(&OpencapxEvent::new(
        "pet.say",
        "mcp",
        serde_json::json!({ "text": text }),
    ));
    let _ = handle.emit("opencapx-say", serde_json::json!({ "text": text }));
}

/// Dead-code placeholder: Session is already persisted through SessionSink.
#[allow(dead_code)]
fn _keep_session_import_alive(s: &Session) -> SessionDto {
    session_to_dto(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Q2 — uniqueness must hold even under same-nanosecond concurrency: the sequence is folded into the id (previously nano collisions + REPLACE silently overwrote).
    #[test]
    fn event_ids_are_unique_in_tight_loop() {
        use std::collections::HashSet;
        let ids: HashSet<String> = (0..500)
            .map(|_| OpencapxEvent::new("t", "s", serde_json::json!({})).id)
            .collect();
        assert_eq!(ids.len(), 500, "ids must be unique within the process");
        // Design contract: id = evt-<nanos>-<seq> (three parts, covering same-nanosecond collisions)
        assert_eq!(ids.iter().next().unwrap().split('-').count(), 3);
    }

    #[test]
    fn bus_delivers_to_all_subscribers() {
        let bus = EventBus::new();
        let rx1 = bus.subscribe();
        let rx2 = bus.subscribe();
        bus.publish(&OpencapxEvent::new("agent.started", "test", serde_json::json!({})));
        assert!(rx1.recv_timeout(std::time::Duration::from_secs(1)).is_ok());
        assert!(rx2.recv_timeout(std::time::Duration::from_millis(100)).is_ok());
    }

    #[test]
    fn bus_drops_dead_subscribers() {
        let bus = EventBus::new();
        {
            let _rx = bus.subscribe();
        }
        let rx = bus.subscribe();
        bus.publish(&OpencapxEvent::new("k", "s", serde_json::json!(null)));
        assert!(rx.recv_timeout(std::time::Duration::from_secs(1)).is_ok());
        assert_eq!(bus.subs.lock().unwrap().len(), 1);
    }

    #[test]
    fn agent_event_type_mapping() {
        assert_eq!(agent_event_type("working"), "agent.working");
        assert_eq!(agent_event_type("waiting"), "agent.waiting");
        assert_eq!(agent_event_type("done"), "agent.completed");
        assert_eq!(agent_event_type("idle"), "agent.started");
    }

    #[test]
    fn ingest_stores_without_handle() {
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Mem(
            crate::core::agent::SessionStore::new(),
        )));
        let bus = EventBus::new();
        let rx = bus.subscribe();
        ingest(None, &store, &bus, r#"{"agent":"codex","text":"running tool"}"#, "unknown", None);
        // The event must land in the DB and be active at "now" (retention policy in is_active)
        assert_eq!(store.lock().unwrap().all().len(), 1);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .active(crate::core::agent::now_secs())
                .len(),
            1
        );
        let e = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(e.kind, "agent.working");
        assert_eq!(e.source, "hooks:codex");
    }

    /// Wow 4: ingest one Claude stop hook, the bus receives `agent.completed`, which is the source of the
    /// 🎉 celebration bubble + bounce animation in the frontend. Lock this mapping down so that renaming
    /// "done" some day does not silently lose the celebration.
    #[test]
    fn ingest_done_state_publishes_agent_completed() {
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Mem(
            crate::core::agent::SessionStore::new(),
        )));
        let bus = EventBus::new();
        let rx = bus.subscribe();
        // The shape a Claude stop hook carries (stop + trailing text).
        ingest(
            None,
            &store,
            &bus,
            r#"{"agent":"claude","event":"stop","text":"All set."}"#,
            "unknown",
            None,
        );
        let e = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(e.kind, "agent.completed");
        assert_eq!(e.source, "hooks:claude");
        // The session must also actually reach the Done state. `active(now)` filters out Done states
        // older than 30s — so use now_secs() to get a "just now" that is guaranteed inside the window.
        let now = super::super::agent::now_secs();
        let active = store.lock().unwrap().active(now);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].state, super::super::agent::AgentState::Done);
    }

    /// The body (speech) is a sticky field: it is produced only at the moment the transcript is read, and later tool events
    /// do not carry it — a following PreToolUse must not wipe it (the bubble's second line would flicker away).
    #[test]
    fn ingest_keeps_speech_sticky_across_events() {
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Mem(
            crate::core::agent::SessionStore::new(),
        )));
        let bus = EventBus::new();
        ingest(
            None,
            &store,
            &bus,
            r#"{"agent":"claude","session_id":"sticky","state":"working","message":"Read a.ts"}"#,
            "unknown",
            None,
        );
        if let Ok(mut s) = store.lock() {
            if let Some(cur) = s.get("sticky") {
                let mut with_speech = cur;
                with_speech.speech = "Let me check the render path first.".into();
                s.upsert(with_speech);
            }
        }
        // Then another event without a body
        ingest(
            None,
            &store,
            &bus,
            r#"{"agent":"claude","session_id":"sticky","state":"working","message":"Edit b.ts"}"#,
            "unknown",
            None,
        );
        let after = store.lock().unwrap().get("sticky").unwrap();
        assert_eq!(after.message, "Edit b.ts");
        assert_eq!(after.speech, "Let me check the render path first.", "the body must not be wiped by the following event");
    }

    /// cwd is a sticky field too: when later events carry no cwd, it must not wipe the grouping key to an empty string,
    /// otherwise the row falls out of the "project group" into an unknown group (the group header suddenly becomes unknown).
    #[test]
    fn ingest_keeps_cwd_sticky_across_events() {
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Mem(
            crate::core::agent::SessionStore::new(),
        )));
        let bus = EventBus::new();
        ingest(
            None,
            &store,
            &bus,
            r#"{"agent":"claude","session_id":"cwd-sticky","state":"working","message":"Read a.ts","cwd":"/Users/me/work/OpenCapX"}"#,
            "unknown",
            None,
        );
        // The second carries no cwd
        ingest(
            None,
            &store,
            &bus,
            r#"{"agent":"claude","session_id":"cwd-sticky","state":"working","message":"Edit b.ts"}"#,
            "unknown",
            None,
        );
        let after = store.lock().unwrap().get("cwd-sticky").unwrap();
        assert_eq!(after.cwd, "/Users/me/work/OpenCapX");
    }

    /// hook trace archive key = authenticated caller_agent_id (the viewer query key); anonymous falls back to kind.
    /// Regression: it used to archive by dto.agent (kind), so viewer queries by agent_id were always empty.
    #[test]
    fn hook_trace_archives_under_caller_agent_id() {
        let _g = super::super::plugin_trace::traces_env_lock();
        let base = std::env::temp_dir().join(format!("ocx-hookkey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            super::super::storage::StoreEnum::Mem(super::super::agent::SessionStore::new()),
        ));
        let bus = EventBus::new();

        // Authenticated path: kind=claude but caller is ag_x_01 → lands in hooks/ag_x_01/
        ingest(
            None,
            &store,
            &bus,
            r#"{"agent":"claude","session_id":"s-caller","state":"working","message":"hi"}"#,
            "unknown",
            Some("ag_x_01"),
        );
        // Anonymous path: falls back to kind → lands in hooks/aider/. kind must be exclusive: other ingest tests
        // in this module (anonymous) also write hook files, and sharing a kind gets extra sessions stuffed in by parallel runs.
        ingest(
            None,
            &store,
            &bus,
            r#"{"agent":"aider","session_id":"s-anon","state":"done","message":"bye"}"#,
            "unknown",
            None,
        );

        assert_eq!(super::super::req_trace::list_hook_traces("ag_x_01").len(), 1, "viewer key hits");
        assert_eq!(super::super::req_trace::list_hook_traces("aider").len(), 1, "anonymous falls back to kind");
        // The authenticated stream must not leak into the kind directory
        let caller_lines = super::super::req_trace::read_hook_trace("ag_x_01", "s-caller", 10);
        assert!(matches!(
            caller_lines.last(),
            Some(super::super::req_trace::RpcTraceLine::Event { name, .. }) if name == "agent.working"
        ));

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rule_audit_publishes_applied_event() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        publish_rule_audit(
            &bus,
            r#"{"agent":"claude","__rule":"sandbox-curl","tool_input":{"command":"curl x"}}"#,
            "unknown",
        );
        let ev = rx.try_recv().expect("rule.applied");
        assert_eq!(ev.kind, "rule.applied");
        assert_eq!(ev.payload["ruleId"], "sandbox-curl");
        assert_eq!(ev.payload["agent"], "claude");
        assert_eq!(ev.payload["command"], "curl x");
    }

    #[test]
    fn rule_audit_silent_without_rule_field() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        publish_rule_audit(&bus, r#"{"agent":"claude"}"#, "unknown");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn rule_audit_falls_back_to_default_agent() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        publish_rule_audit(&bus, r#"{"__rule":"r1"}"#, "fallback");
        let ev = rx.try_recv().expect("rule.applied");
        assert_eq!(ev.payload["agent"], "fallback");
    }

    #[test]
    fn rule_audit_carries_reason_when_present() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        publish_rule_audit(
            &bus,
            r#"{"agent":"opencapx","__rule":"sandbox.unguarded","reason":"no backend","tool_input":{"command":"sh x.sh"}}"#,
            "unknown",
        );
        let ev = rx.try_recv().expect("rule.applied");
        assert_eq!(ev.payload["reason"], "no backend");
    }
}
