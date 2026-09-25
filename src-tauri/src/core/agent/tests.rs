use super::*;

#[test]
fn claude_stop_with_question_maps_waiting() {
    assert_eq!(map_state("claude", "stop:question"), AgentState::Waiting);
}

#[test]
fn stop_without_question_maps_done() {
    assert_eq!(map_state("claude", "stop"), AgentState::Done);
}

#[test]
fn tool_use_maps_working() {
    assert_eq!(
        map_state("codex", "running tool: Edit src/main.rs"),
        AgentState::Working
    );
}

#[test]
fn unknown_maps_idle() {
    assert_eq!(map_state("pi", "hello"), AgentState::Idle);
}

#[test]
fn done_sessions_expire_from_active_after_30s() {
    let mut s = SessionStore::new();
    s.upsert(Session {
        id: "a".into(),
        agent: "claude".into(),
        project: "p".into(),
        cwd: String::new(),
        message: "".into(),
        state: AgentState::Done,
        started_at: 1000,
        updated_at: 1000,
        model: String::new(),
        speech: String::new(),
        choices: None,
        answered: None,
    });
    assert!(s.active(1031).is_empty());
    assert_eq!(s.active(1029).len(), 1);
}

#[test]
fn retention_is_state_machine_not_flat_ttl() {
    let mut s = SessionStore::new();
    for (id, state) in [
        ("d", AgentState::Done),
        ("i", AgentState::Idle),
        ("w", AgentState::Working),
        ("q", AgentState::Waiting),
    ] {
        s.upsert(Session {
            id: id.into(),
            agent: "claude".into(),
            project: "p".into(),
            cwd: String::new(),
            message: "".into(),
            state,
            started_at: 1000,
            updated_at: 1000,
            model: String::new(),
            speech: String::new(),
            choices: None,
            answered: None,
        });
    }
    let has = |now: u64, id: &str| s.active(now).iter().any(|x| x.id == id);
    // done: 30s (so the user can see it), not kept forever
    assert!(has(1029, "d"));
    assert!(!has(1031, "d"));
    // idle: 600s
    assert!(has(1600, "i"));
    assert!(!has(1601, "i"));
    // working / waiting: 900s with no heartbeat means the agent is dead (it sent no Stop)
    assert!(has(1900, "w"));
    assert!(has(1900, "q"));
    assert!(!has(1901, "w"));
    assert!(!has(1901, "q"));
}

#[test]
fn sweep_removes_only_expired() {
    let mut s = SessionStore::new();
    for (id, state, ts) in [
        ("live", AgentState::Working, 5000u64),
        ("dead", AgentState::Working, 1000),
        ("oldDone", AgentState::Done, 1000),
    ] {
        s.upsert(Session {
            id: id.into(),
            agent: "claude".into(),
            project: "p".into(),
            cwd: String::new(),
            message: "".into(),
            state,
            started_at: ts,
            updated_at: ts,
            model: String::new(),
            speech: String::new(),
            choices: None,
            answered: None,
        });
    }
    assert_eq!(s.sweep(5300), 2);
    let left: Vec<String> = s.all().into_iter().map(|x| x.id).collect();
    assert_eq!(left, vec!["live".to_string()]);
    // A second sweep finds no new expired items
    assert_eq!(s.sweep(5300), 0);
}

#[test]
fn upsert_keeps_original_started_at() {
    let mut s = SessionStore::new();
    let mk = |ts: u64| Session {
        id: "x".into(),
        agent: "claude".into(),
        project: "p".into(),
        cwd: String::new(),
        message: "".into(),
        state: AgentState::Working,
        started_at: ts,
        updated_at: ts,
        model: String::new(),
        speech: String::new(),
        choices: None,
        answered: None,
    };
    s.upsert(mk(1000));
    s.upsert(mk(2000));
    assert_eq!(s.get("x").unwrap().started_at, 1000);
}

#[test]
fn queue_replay_preserves_state_and_timestamp() {
    // The offline queue stores the DTO itself; replay must trust state / updatedAt,
    // otherwise "working at the time" becomes "idle now" and the timestamp is refreshed to the replay moment.
    let dto = process_body(
        r#"{"agent":"claude","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"npm test"},"session_id":"q1"}"#,
        "unknown",
    );
    assert_eq!(dto.state, "working");
    let body = serde_json::to_string(&dto).unwrap();
    let back = process_body(&body, "unknown");
    assert_eq!(back.state, "working");
    assert_eq!(back.updated_at, dto.updated_at);
    assert_eq!(back.started_at, dto.started_at);
    assert_eq!(back.message, dto.message);
    assert_eq!(back.id, "q1");
}

#[test]
fn stamp_payload_injects_agent_and_sent_at_without_losing_fields() {
    let raw = r#"{"hook_event_name":"Stop","session_id":"s","transcript_path":"/tmp/t.jsonl","tool_name":"Edit"}"#;
    let stamped = stamp_payload(raw, "claude", 1700);
    let v: serde_json::Value = serde_json::from_str(&stamped).unwrap();
    assert_eq!(v["agent"], "claude");
    assert_eq!(v["__sent_at"], 1700);
    // Not a single raw field may be lost (otherwise Core cannot read the transcript)
    assert_eq!(v["transcript_path"], "/tmp/t.jsonl");
    assert_eq!(v["hook_event_name"], "Stop");
    assert_eq!(v["session_id"], "s");
    assert_eq!(v["tool_name"], "Edit");
}

#[test]
fn stamp_payload_annotated_injects_rule_id() {
    let stamped = stamp_payload_annotated(
        r#"{"hook_event_name":"PreToolUse"}"#,
        "claude",
        7,
        Some("sandbox-curl"),
    );
    let v: serde_json::Value = serde_json::from_str(&stamped).unwrap();
    assert_eq!(v["__rule"], "sandbox-curl");
    assert_eq!(v["__sent_at"], 7);
    assert_eq!(v["agent"], "claude");
}

#[test]
fn stamp_payload_annotated_omits_rule_when_none() {
    let stamped = stamp_payload_annotated("{}", "claude", 7, None);
    let v: serde_json::Value = serde_json::from_str(&stamped).unwrap();
    assert!(v.get("__rule").is_none());
}

#[test]
fn stamp_payload_wraps_non_json_and_skips_auto_agent() {
    let stamped = stamp_payload("plain text", "auto", 5);
    let v: serde_json::Value = serde_json::from_str(&stamped).unwrap();
    assert_eq!(v["text"], "plain text");
    assert_eq!(v["__sent_at"], 5);
    assert!(
        v.get("agent").is_none(),
        "auto should not be hardcoded as an agent name"
    );
}

#[test]
fn sent_at_is_honoured_as_event_time() {
    let body = stamp_payload(
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"npm test"},"session_id":"st"}"#,
        "claude",
        1700,
    );
    let dto = process_body(&body, "unknown");
    assert_eq!(dto.state, "working");
    assert_eq!(dto.updated_at, 1700);
    assert_eq!(dto.started_at, 1700);
}

#[test]
fn stale_queue_replay_is_not_resurrected_as_active() {
    // Sessions that finished while the app was closed: after replay they are judged by the original timestamp and should count as expired directly,
    // rather than popping up as a "just finished" session.
    let body = r#"{"id":"old","agent":"claude","project":"p","message":"m","state":"done","startedAt":1000,"updatedAt":1000}"#;
    let dto = process_body(body, "unknown");
    assert_eq!(dto.state, "done");
    assert_eq!(dto.updated_at, 1000);
    let s = dto_to_session(&dto);
    assert!(is_active(&s, 1005));
    assert!(!is_active(&s, 10_000));
}

#[test]
fn dismiss_and_clear_work() {
    let mut s = SessionStore::new();
    s.upsert(Session {
        id: "a".into(),
        agent: "claude".into(),
        project: "p".into(),
        cwd: String::new(),
        message: "".into(),
        state: AgentState::Working,
        started_at: 1000,
        updated_at: 1000,
        model: String::new(),
        speech: String::new(),
        choices: None,
        answered: None,
    });
    s.dismiss("a");
    assert!(s.active(1000).is_empty());
    s.upsert(Session {
        id: "b".into(),
        agent: "codex".into(),
        project: "q".into(),
        cwd: String::new(),
        message: "".into(),
        state: AgentState::Working,
        started_at: 1000,
        updated_at: 1000,
        model: String::new(),
        speech: String::new(),
        choices: None,
        answered: None,
    });
    s.clear();
    assert!(s.active(1000).is_empty());
}

#[test]
fn process_body_builds_dto() {
    let d = process_body(
        r#"{"agent":"claude","text":"Should I proceed?","project":"/x/demo"}"#,
        "unknown",
    );
    assert_eq!(d.agent, "claude");
    assert_eq!(d.state, "waiting");
    assert_eq!(d.project, "demo");
    assert_eq!(d.message, "Should I proceed?");
}

#[test]
fn process_body_event_name_drives_waiting() {
    let d = process_body(
        r#"{"agent":"codex","event":"PermissionRequest","text":"rm -rf /"}"#,
        "unknown",
    );
    assert_eq!(d.state, "waiting");
}

#[test]
fn dto_session_roundtrip() {
    let dto = process_body(
        r#"{"agent":"codex","text":"running tool","project":"/x/demo"}"#,
        "unknown",
    );
    let s = dto_to_session(&dto);
    let back = session_to_dto(&s);
    assert_eq!(back.id, dto.id);
    assert_eq!(back.state, dto.state);
}

#[test]
fn process_body_parses_choices() {
    let dto = process_body(
        r#"{"agent":"codex","text":"Which one?","id":"q1","choices":[{"id":"A","label":"Approve"},{"id":"B","label":"Deny"}]}"#,
        "unknown",
    );
    let choices = dto.choices.expect("choices parsed");
    assert_eq!(choices.len(), 2);
    assert_eq!(choices[0].id, "A");
    assert_eq!(choices[0].label, "Approve");
    assert_eq!(choices[1].id, "B");
    assert!(dto.answered.is_none());
}

#[test]
fn process_body_parses_answered() {
    let dto = process_body(
        r#"{"agent":"codex","id":"q1","answered":"A","choices":[{"id":"A","label":"Approve"}]}"#,
        "unknown",
    );
    assert_eq!(dto.answered.as_deref(), Some("A"));
}

#[test]
fn process_body_choices_drop_invalid_entries() {
    let dto = process_body(
        r#"{"agent":"codex","id":"q1","choices":[{"id":"A","label":"Approve"},{"label":"NoId"},{"id":"C"}]}"#,
        "unknown",
    );
    let choices = dto.choices.expect("at least one valid");
    assert_eq!(choices.len(), 2);
    assert_eq!(choices[0].id, "A");
    // Fall back to the id when there is no label
    assert_eq!(choices[1].label, "C");
}

#[test]
fn process_body_no_choices_is_none() {
    let dto = process_body(r#"{"agent":"codex","text":"running tool"}"#, "unknown");
    assert!(dto.choices.is_none());
}

#[test]
fn session_to_dto_roundtrip_preserves_choices() {
    let dto = process_body(
        r#"{"agent":"codex","id":"q1","choices":[{"id":"A","label":"X"}]}"#,
        "unknown",
    );
    let s = dto_to_session(&dto);
    let back = session_to_dto(&s);
    assert_eq!(back.choices.as_ref().unwrap()[0].id, "A");
    assert_eq!(back.choices.as_ref().unwrap()[0].label, "X");
}

#[test]
fn process_body_extracts_tool_file_from_claude_payload() {
    let d = process_body(
        r#"{"agent":"claude","hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"src/overlay.ts","old_string":"a","new_string":"b"}}"#,
        "unknown",
    );
    assert_eq!(d.state, "working");
    assert_eq!(d.message, "Edit src/overlay.ts");
}

#[test]
fn process_body_tool_activity_uses_first_command_line() {
    let d = process_body(
        r#"{"agent":"claude","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"npm test\nsecond","description":"run tests"}}"#,
        "unknown",
    );
    assert_eq!(d.message, "Bash npm test");
}

#[test]
fn process_body_explicit_text_wins_over_tool() {
    let d = process_body(
        r#"{"agent":"claude","text":"manual note","tool_name":"Edit","tool_input":{"file_path":"x.ts"}}"#,
        "unknown",
    );
    assert_eq!(d.message, "manual note");
}

#[test]
fn process_body_without_text_or_tool_keeps_message_empty() {
    let d = process_body(r#"{"agent":"claude","hook_event_name":"Stop"}"#, "unknown");
    assert_eq!(d.state, "done");
    assert_eq!(d.message, "");
}

// Keep the full path (grouping key + branch reading), but the display name is still the last segment —
// the text length and existing behavior of the tray/notifications must not change.
#[test]
fn cwd_is_kept_while_project_stays_last_segment() {
    let d = process_body(
        r#"{"agent":"claude","hook_event_name":"PreToolUse","session_id":"c1","cwd":"/Users/me/work/OpenCapX"}"#,
        "unknown",
    );
    assert_eq!(d.project, "OpenCapX");
    assert_eq!(d.cwd, "/Users/me/work/OpenCapX");
}

#[test]
fn missing_cwd_keeps_project_unknown() {
    let d = process_body(r#"{"agent":"claude","hook_event_name":"Stop"}"#, "unknown");
    assert_eq!(d.project, "unknown");
    assert_eq!(d.cwd, "");
}

fn tmp_transcript(tag: &str, body: &str) -> String {
    use std::io::Write;
    let p = std::env::temp_dir().join(format!("opencapx-agent-{}-{}", std::process::id(), tag));
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    p.to_string_lossy().into_owned()
}

/// JSON-escape a filesystem path for embedding in a raw-string hook payload fixture.
/// On Windows the temp path is full of backslashes; unescaped, `\U`/`\A` are invalid
/// JSON escapes and the whole payload silently fails to parse.
fn json_path(p: &str) -> String {
    p.replace('\\', "\\\\")
}

fn stop_body(path: &str) -> String {
    format!(
        r#"{{"agent":"claude","hook_event_name":"Stop","session_id":"s1","transcript_path":"{}"}}"#,
        json_path(path)
    )
}

#[test]
fn refine_turns_done_into_waiting_when_assistant_asked_a_question() {
    let path = tmp_transcript(
            "q",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Refactored the parser. Should I also add tests?\"}]}}\n",
        );
    let body = stop_body(&path);
    let mut dto = process_body(&body, "unknown");
    assert_eq!(dto.state, "done");
    enrich_from_transcript(&mut dto, &body);
    assert_eq!(dto.state, "waiting");
    // Fill in an assistant summary when the message is empty
    assert_eq!(
        dto.message,
        "Refactored the parser. Should I also add tests?"
    );
    std::fs::remove_file(path).ok();
}

#[test]
fn refine_keeps_done_for_a_plain_summary() {
    let path = tmp_transcript(
            "d",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"All tests pass. Let me know if you need more.\"}]}}\n",
        );
    let body = stop_body(&path);
    let mut dto = process_body(&body, "unknown");
    enrich_from_transcript(&mut dto, &body);
    assert_eq!(dto.state, "done");
    std::fs::remove_file(path).ok();
}

// The raw text of a user question: Claude UserPromptSubmit / Gemini BeforeAgent go through `prompt`.
// Not reading it leaves that line with only the theme preset phrase (half the reason the bubble looks "all commands").
#[test]
fn user_prompt_is_used_as_message() {
    let d = process_body(
        r#"{"agent":"claude","hook_event_name":"UserPromptSubmit","session_id":"p1","prompt":"also build the shape of the theme"}"#,
        "unknown",
    );
    assert_eq!(d.state, "working");
    assert_eq!(d.message, "also build the shape of the theme");
}

// The agent's body must also be brought into the bubble while working (previously it was read only once at the Stop moment).
#[test]
fn speech_filled_from_transcript_while_working() {
    let path = tmp_transcript(
            "speech-work",
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-sonnet-4-5\",\"content\":[{\"type\":\"text\",\"text\":\"Let me first look at the bubble render path.\"}]}}\n",
        );
    let body = format!(
        r#"{{"agent":"claude","hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{{"file_path":"src/bubble.ts"}},"session_id":"speech-w1","transcript_path":"{}"}}"#,
        json_path(&path)
    );
    let mut dto = process_body(&body, "unknown");
    enrich_from_transcript(&mut dto, &body);
    // Two separate things: message is "what it's doing", speech is "what it said"
    assert_eq!(dto.message, "Read src/bubble.ts");
    assert_eq!(dto.speech, "Let me first look at the bubble render path.");
    assert_eq!(dto.model, "claude-sonnet-4-5");
    std::fs::remove_file(path).ok();
}

// The message of a done line gets replaced by the celebration text in the bubble, so the body must be kept separately.
#[test]
fn done_keeps_speech_even_when_message_is_the_summary() {
    let path = tmp_transcript(
            "speech-done",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"The shape system is done.\"}]}}\n",
        );
    let body = stop_body(&path);
    let mut dto = process_body(&body, "unknown");
    enrich_from_transcript(&mut dto, &body);
    assert_eq!(dto.state, "done");
    assert_eq!(dto.message, "The shape system is done.");
    assert_eq!(dto.speech, "The shape system is done.");
    std::fs::remove_file(path).ok();
}

#[test]
fn refine_ignores_non_stop_states_and_other_agents() {
    // working does not do transcript judgment
    let body = r#"{"agent":"claude","hook_event_name":"PreToolUse","tool_name":"Bash","transcript_path":"/nope.jsonl"}"#;
    let mut dto = process_body(body, "unknown");
    let before = dto.state.clone();
    enrich_from_transcript(&mut dto, body);
    assert_eq!(dto.state, before);
    // non-claude/droid does not read the transcript even when done
    let mut other = process_body(
        r#"{"agent":"codex","hook_event_name":"Stop","transcript_path":"/nope.jsonl"}"#,
        "unknown",
    );
    enrich_from_transcript(
        &mut other,
        r#"{"agent":"codex","hook_event_name":"Stop","transcript_path":"/nope.jsonl"}"#,
    );
    assert_eq!(other.state, "done");
}

#[test]
fn refine_is_a_noop_when_transcript_is_unreadable() {
    let body = stop_body("/nope/opencapx/missing.jsonl");
    let mut dto = process_body(&body, "unknown");
    enrich_from_transcript(&mut dto, &body);
    assert_eq!(dto.state, "done");
    assert_eq!(dto.message, "");
}

// ---- per-agent payload parsing ----

#[test]
fn claude_events_map_by_name_not_by_substring() {
    let cases = [
        ("SessionStart", "idle"),
        ("UserPromptSubmit", "working"),
        ("PreToolUse", "working"),
        ("PostToolUse", "working"),
        ("Notification", "waiting"),
        ("PermissionRequest", "waiting"),
        ("Stop", "done"),
        ("SessionEnd", "idle"),
    ];
    for (event, want) in cases {
        let body = format!(r#"{{"agent":"claude","hook_event_name":"{event}","session_id":"s"}}"#);
        assert_eq!(process_body(&body, "unknown").state, want, "event {event}");
    }
}

#[test]
fn subagent_stop_is_ignored_entirely() {
    let body = r#"{"agent":"claude","hook_event_name":"SubagentStop","session_id":"s"}"#;
    assert!(should_ignore_event(body));
    // The same applies to other agents' subagentStop (Cursor camelCase)
    assert!(should_ignore_event(
        r#"{"agent":"cursor","hookEventName":"subagentStop","conversation_id":"c"}"#
    ));
    // Ordinary events must not be misjudged
    assert!(!should_ignore_event(
        r#"{"agent":"claude","hook_event_name":"Stop","session_id":"s"}"#
    ));
    assert!(!should_ignore_event("not json"));
}

#[test]
fn cursor_uses_conversation_id_and_workspace_roots() {
    let body = r#"{"agent":"cursor","hook_event_name":"preToolUse","conversation_id":"conv-9","workspace_roots":["/Users/x/proj"],"tool_name":"run_terminal_cmd","tool_input":{"command":"npm test"}}"#;
    let d = process_body(body, "unknown");
    assert_eq!(d.id, "conv-9");
    assert_eq!(d.project, "proj");
    assert_eq!(d.state, "working");
    assert_eq!(d.message, "run_terminal_cmd npm test");
}

#[test]
fn windsurf_uses_trajectory_id() {
    let body = r#"{"agent":"windsurf","agent_action_name":"pre_user_prompt","trajectory_id":"tray-1","workspacePaths":["/w/app"]}"#;
    let d = process_body(body, "unknown");
    assert_eq!(d.id, "tray-1");
    assert_eq!(d.project, "app");
    assert_eq!(d.state, "working");
}

#[test]
fn grok_camel_case_keys() {
    let body = r#"{"agent":"grok","hookEventName":"Notification","sessionId":"g-1","workspaceRoot":"/g/repo","message":"needs permission"}"#;
    let d = process_body(body, "unknown");
    assert_eq!(d.id, "g-1");
    assert_eq!(d.project, "repo");
    assert_eq!(d.state, "waiting");
    assert_eq!(d.message, "needs permission");
}

#[test]
fn antigravity_infers_event_from_present_fields() {
    // No event name: with toolCall → treat as working
    let working = r#"{"agent":"antigravity","conversationId":"a-1","workspacePaths":["/a/x"],"toolCall":{"name":"Edit"}}"#;
    let d = process_body(working, "unknown");
    assert_eq!(d.state, "working");
    // With terminationReason → treat as done
    let done = r#"{"agent":"antigravity","conversationId":"a-1","terminationReason":"completed"}"#;
    assert_eq!(process_body(done, "unknown").state, "done");
}

#[test]
fn model_is_read_from_payload_in_three_shapes() {
    for body in [
        r#"{"agent":"claude","hook_event_name":"Stop","session_id":"s","model":"claude-opus-4-1"}"#,
        r#"{"agent":"claude","hook_event_name":"Stop","session_id":"s","model":{"id":"claude-opus-4-1"}}"#,
        r#"{"agent":"claude","hook_event_name":"Stop","session_id":"s","model":{"display_name":"Opus"}}"#,
    ] {
        let d = process_body(body, "unknown");
        assert!(!d.model.is_empty(), "no model parsed from {body}");
    }
    // Empty when there is no model field, do not guess
    assert_eq!(
        process_body(
            r#"{"agent":"claude","hook_event_name":"Stop","session_id":"s"}"#,
            "u"
        )
        .model,
        ""
    );
}

#[test]
fn enrich_fills_model_from_transcript_on_working() {
    let path = tmp_transcript(
            "model",
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-haiku-4-5\",\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}\n",
        );
    let body = format!(
        r#"{{"agent":"claude","hook_event_name":"PreToolUse","tool_name":"Read","session_id":"m-sess-1","transcript_path":"{}"}}"#,
        json_path(&path)
    );
    let mut dto = process_body(&body, "unknown");
    assert_eq!(dto.state, "working");
    enrich_from_transcript(&mut dto, &body);
    assert_eq!(dto.model, "claude-haiku-4-5");
    // State is not changed while in working
    assert_eq!(dto.state, "working");
    std::fs::remove_file(path).ok();
}

#[test]
fn unknown_events_still_fall_back_to_heuristics() {
    // An event name not in the table → falls back to the old map_state keyword heuristic
    let body = r#"{"agent":"claude","event":"totally-unknown-event","text":"working on it","session_id":"s"}"#;
    assert_eq!(process_body(body, "unknown").state, "working");
    // A custom agent goes entirely through the fallback
    let custom = r#"{"agent":"my-agent","text":"running tool","session_id":"s"}"#;
    assert_eq!(process_body(custom, "unknown").state, "working");
}

fn sortable(id: &str, state: AgentState, updated_at: u64) -> Session {
    Session {
        id: id.into(),
        agent: "claude".into(),
        project: "p".into(),
        cwd: String::new(),
        message: String::new(),
        state,
        started_at: 1,
        updated_at,
        model: String::new(),
        speech: String::new(),
        choices: None,
        answered: None,
    }
}

#[test]
fn sort_puts_waiting_first_then_recency() {
    let mut v = vec![
        sortable("d", AgentState::Done, 100),
        sortable("w1", AgentState::Working, 100),
        sortable("n", AgentState::Waiting, 50),
        sortable("w2", AgentState::Working, 200),
        sortable("i", AgentState::Idle, 999),
    ];
    sort_sessions(&mut v);
    let ids: Vec<&str> = v.iter().map(|s| s.id.as_str()).collect();
    // waiting first; within working, most recently active first; done before idle
    assert_eq!(ids, vec!["n", "w2", "w1", "d", "i"]);
}

#[test]
fn sort_is_deterministic_when_timestamps_tie() {
    let mut a = vec![
        sortable("b", AgentState::Working, 100),
        sortable("a", AgentState::Working, 100),
    ];
    let mut b = a.clone();
    sort_sessions(&mut a);
    sort_sessions(&mut b);
    let ids = |v: &Vec<Session>| v.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&a), ids(&b));
    assert_eq!(ids(&a), vec!["a", "b"]); // same second falls back to ascending id
}

/// The ordering policy exists in only one place: the key in the DTO must give the same order as sort_sessions,
/// otherwise rows merged locally by the frontend will be misordered.
#[test]
fn order_key_matches_sort_sessions() {
    let mut v = vec![
        sortable("x", AgentState::Idle, 10),
        sortable("y", AgentState::Waiting, 5),
        sortable("z", AgentState::Working, 900),
    ];
    let mut by_key = v.clone();
    by_key.sort_by_key(order_key);
    sort_sessions(&mut v);
    let ids = |rows: &Vec<Session>| rows.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&by_key), ids(&v));
}
