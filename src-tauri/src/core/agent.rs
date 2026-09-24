//! Agent session model and session storage abstraction. The former state.rs is consolidated here.

use crate::detector::{detect_claude_stop, looks_like_question, title_from_transcript};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Waiting,
    Done,
    Idle,
}

#[derive(Debug, Clone, Serialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub agent: String,
    pub project: String,
    /// Full working directory (the cwd of the hook payload). Used as the bubble grouping key + to read the git branch; may be empty.
    pub cwd: String,
    pub message: String,
    pub state: AgentState,
    /// Time the session first appeared (the old value is kept on upsert, not refreshed by later events).
    pub started_at: u64,
    pub updated_at: u64,
    /// Current model name (backfilled from the transcript tail when absent from the hook payload, see enrich_from_transcript).
    pub model: String,
    /// Agent body text (the last assistant text at the transcript tail), the bubble's second line.
    pub speech: String,
    /// Choice prompt (the agent offers A/B/C for the user to pick). None = a plain text bubble.
    pub choices: Option<Vec<Choice>>,
    /// The id the user has chosen; if Some, buttons are no longer shown.
    pub answered: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionDto {
    pub id: String,
    pub agent: String,
    pub project: String,
    /// Full working directory (may be empty). Bubbles are grouped by it, and the group header reads the branch from it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    pub message: String,
    pub state: String,
    #[serde(rename = "startedAt")]
    pub started_at: u64,
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
    /// Sort key (the policy lives in Rust; the frontend only does lexicographic comparison). Not persisted.
    pub order: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// What the Agent itself said (the last assistant text at the transcript tail).
    /// `message` is "what it's doing" (tool/prompt); this is "what it said" — shown as the second line in the bubble,
    /// because the message of a done line gets replaced by the celebration text, and only this preserves the closing body.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub speech: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<Choice>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered: Option<String>,
}

/// One entry in the session history: the archived copy after a session expires from the active list.
#[derive(Debug, Clone, Serialize)]
pub struct ArchivedSession {
    pub id: String,
    pub agent: String,
    pub project: String,
    pub message: String,
    pub state: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(rename = "startedAt")]
    pub started_at: u64,
    #[serde(rename = "endedAt")]
    pub ended_at: u64,
    /// Duration in seconds (endedAt - startedAt, clamped to zero on negatives/clock rollback).
    pub duration: u64,
}

/// Session storage abstraction: the in-memory implementation (tests/degraded mode) and the SQLite implementation share the interface.
pub trait SessionSink: Send {
    fn upsert(&mut self, s: Session);
    fn get(&self, id: &str) -> Option<Session>;
    fn active(&self, now: u64) -> Vec<Session>;
    fn all(&self) -> Vec<Session>;
    fn dismiss(&mut self, id: &str);
    fn clear(&mut self);
    /// Clear expired sessions out of the active list (the SQLite implementation archives them first), returning the number processed.
    fn sweep(&mut self, now: u64) -> usize;
}

pub fn state_str(s: AgentState) -> &'static str {
    match s {
        AgentState::Working => "working",
        AgentState::Waiting => "waiting",
        AgentState::Done => "done",
        AgentState::Idle => "idle",
    }
}

/// State priority: waiting on you > working > just finished > idle.
fn state_rank(s: AgentState) -> u8 {
    match s {
        AgentState::Waiting => 0,
        AgentState::Working => 1,
        AgentState::Done => 2,
        AgentState::Idle => 3,
    }
}

fn rank_of_str(state: &str) -> u8 {
    match state {
        "waiting" => 0,
        "working" => 1,
        "done" => 2,
        _ => 3,
    }
}

/// Sort key = priority (1 digit) + reversed timestamp (20 digits, newest first) + id.
/// Fixed-width format, so **lexicographic comparison** is the ordering: the frontend also only compares this key,
/// rather than reimplementing the policy (the policy lives only here; changing it changes both ends together).
pub fn order_key(s: &Session) -> String {
    format!(
        "{}{:020}{}",
        state_rank(s.state),
        u64::MAX - s.updated_at,
        s.id
    )
}

/// Isomorphic to `order_key`, for DTOs not yet persisted (event broadcasting needs it).
pub fn order_key_for_dto(d: &SessionDto) -> String {
    format!(
        "{}{:020}{}",
        rank_of_str(&d.state),
        u64::MAX - d.updated_at,
        d.id
    )
}

/// The single entry point for session ordering (shared by the bubble and the tray).
/// The tray used to have its own copy (`tray::sort_key`, sorting same-state by agent name),
/// and two copies of the rule would inevitably drift, so they were consolidated here.
pub fn sort_sessions(rows: &mut [Session]) {
    rows.sort_by_key(order_key);
}

/// String → state; unknown values return None (dirty data is not accepted).
pub fn parse_state(s: &str) -> Option<AgentState> {
    match s.trim().to_ascii_lowercase().as_str() {
        "working" => Some(AgentState::Working),
        "waiting" => Some(AgentState::Waiting),
        "done" => Some(AgentState::Done),
        "idle" => Some(AgentState::Idle),
        _ => None,
    }
}

/// Retention duration (seconds). **State-machine style**, not a one-size-fits-all TTL:
/// done stays a while so the user can see it; idle sessions stay longer for easy review;
/// working/waiting with no heartbeat for a long time means the agent is already dead (it sent no Stop).
pub const DONE_TTL_SECS: u64 = 30;
pub const IDLE_TTL_SECS: u64 = 600;
pub const STALE_ACTIVE_TTL_SECS: u64 = 900;

/// Whether the session is still in the "active list" (the tray/bubble only show active sessions).
pub fn is_active(s: &Session, now: u64) -> bool {
    let age = now.saturating_sub(s.updated_at);
    match s.state {
        AgentState::Done => age <= DONE_TTL_SECS,
        AgentState::Idle => age <= IDLE_TTL_SECS,
        AgentState::Working | AgentState::Waiting => age <= STALE_ACTIVE_TTL_SECS,
    }
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

pub fn map_state(agent: &str, raw: &str) -> AgentState {
    let r = raw.to_lowercase();
    if r.contains("waiting")
        || r.contains("input")
        || r.contains("permission")
        || r.contains("notification")
        || r == "stop:question"
        || r.contains("needs input")
    {
        return AgentState::Waiting;
    }
    if r.contains("done") || r.contains("stop") || r.contains("exit") || r.contains("finish") {
        return AgentState::Done;
    }
    if r.contains("work") || r.contains("run") || r.contains("start") || r.contains("tool") {
        return AgentState::Working;
    }
    let _ = agent;
    AgentState::Idle
}

fn pick_num(v: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    for k in keys {
        if let Some(n) = v.get(*k).and_then(|x| x.as_u64()) {
            return Some(n);
        }
    }
    None
}

fn pick_str(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

fn last_path_component(p: &str) -> String {
    p.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("unknown")
        .to_string()
}

/// The result of mapping an event name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventMapping {
    State(AgentState),
    /// Events that explicitly "do not touch the state". SubagentStop is typical: a child agent finishing does not mean
    /// the main session state changed; treating it as done would immediately be pushed back by the next working, causing a flicker.
    Ignore,
    /// Not in the table → handed to the generic heuristic `map_state`.
    Unknown,
}

/// Event name → state table for each agent (names are lowercased before comparison, compatible with PascalCase /
/// camelCase / snake_case naming conventions).
fn event_mapping(agent: &str, event: &str) -> EventMapping {
    use AgentState::*;
    let e = event.trim();
    if e.is_empty() {
        return EventMapping::Unknown;
    }
    let lower = e.to_ascii_lowercase();
    match agent {
        // Claude family (Claude Code / Droid / Copilot / Kiro / Grok / Codex / Gemini)
        "claude" | "droid" | "copilot" | "kiro" | "grok" | "codex" | "gemini" => {
            match lower.as_str() {
                "sessionstart" | "session_start" | "agentspawn" => EventMapping::State(Idle),
                "userpromptsubmit" | "beforeagent" | "pretooluse" | "beforetool"
                | "posttooluse" | "aftertool" => EventMapping::State(Working),
                "notification" | "permissionrequest" => EventMapping::State(Waiting),
                "stop" | "afteragent" => EventMapping::State(Done),
                "subagentstop" => EventMapping::Ignore,
                "sessionend" | "session_end" => EventMapping::State(Idle),
                _ => EventMapping::Unknown,
            }
        }
        "cursor" => match lower.as_str() {
            "sessionstart" => EventMapping::State(Idle),
            "beforesubmitprompt" | "pretooluse" => EventMapping::State(Working),
            "stop" => EventMapping::State(Done),
            "subagentstop" => EventMapping::Ignore,
            "sessionend" => EventMapping::State(Idle),
            _ => EventMapping::Unknown,
        },
        "windsurf" => match lower.as_str() {
            "pre_user_prompt" => EventMapping::State(Working),
            "post_cascade_response" => EventMapping::State(Done),
            _ => EventMapping::Unknown,
        },
        "antigravity" => match lower.as_str() {
            "preinvocation" | "pretooluse" | "posttooluse" => EventMapping::State(Working),
            "stop" => EventMapping::State(Done),
            _ => EventMapping::Unknown,
        },
        _ => EventMapping::Unknown,
    }
}

/// Whether the event should be dropped entirely (no state change, no persistence, no notification).
pub fn should_ignore_event(body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let agent = pick_str(&v, &["agent"]).unwrap_or_default();
    let fields = parse_hook_fields(&v, &agent);
    matches!(event_mapping(&agent, &fields.event), EventMapping::Ignore)
}

/// Structured fields extracted from the payload. Each agent names its fields differently; they are unified here.
#[derive(Debug, Default, Clone)]
struct HookFields {
    session_id: Option<String>,
    project: Option<String>,
    event: String,
    text: Option<String>,
    model: String,
}

/// Take the first string field that exists; for arrays take the first element (workspace_roots and the like).
fn pick_first_of(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        let Some(x) = v.get(*k) else { continue };
        if let Some(s) = x.as_str() {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
        if let Some(arr) = x.as_array() {
            for item in arr {
                if let Some(s) = item.as_str() {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Model field tolerance: supports three shapes: a string, `{"display_name"}`, `{"id"}`.
fn model_from(v: &serde_json::Value, keys: &[&str]) -> String {
    for k in keys {
        let Some(m) = v.get(*k) else { continue };
        if let Some(s) = m.as_str() {
            if !s.is_empty() {
                return s.to_string();
            }
        }
        for inner in ["display_name", "displayName", "id", "name"] {
            if let Some(s) = m.get(inner).and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    String::new()
}

/// Extract fields according to each agent's field conventions (Claude is snake_case / Cursor uses conversation_id /
/// Windsurf uses trajectory_id / Antigravity has no event name at all).
fn parse_hook_fields(v: &serde_json::Value, agent: &str) -> HookFields {
    let (sess_keys, proj_keys, event_keys, model_keys): (&[&str], &[&str], &[&str], &[&str]) =
        match agent {
            "cursor" => (
                &["conversation_id", "session_id", "id"],
                &["workspace_roots", "cwd", "project"],
                &["hook_event_name", "hookEventName", "event"],
                &["model"],
            ),
            "windsurf" => (
                &["trajectory_id", "session_id", "id"],
                &["workspacePaths", "workspace_paths", "cwd"],
                &["agent_action_name", "hook_event_name", "event"],
                &["model"],
            ),
            "grok" => (
                &["sessionId", "session_id", "id"],
                &["workspaceRoot", "workspace_root", "cwd"],
                &["hookEventName", "hook_event_name", "event"],
                &["model", "modelName"],
            ),
            "antigravity" => (
                &["conversationId", "conversation_id", "session_id", "id"],
                &["workspacePaths", "workspace_paths", "cwd"],
                &["hook_event_name", "event"],
                &["model"],
            ),
            _ => (
                &["session_id", "id"],
                &["cwd", "project", "dir"],
                &["event", "hook_event", "hookEventName", "hook_event_name"],
                &["model"],
            ),
        };

    let mut event = pick_first_of(v, event_keys).unwrap_or_default();
    // Antigravity sends no event name: it is inferred from "which fields are present".
    if event.is_empty() && agent == "antigravity" {
        if v.get("terminationReason").is_some() || v.get("fullyIdle").is_some() {
            event = "Stop".into();
        } else if v.get("toolCall").is_some()
            || v.get("invocationNum").is_some()
            || v.get("stepIdx").is_some()
        {
            event = "PreToolUse".into();
        }
    }

    HookFields {
        session_id: pick_first_of(v, sess_keys),
        project: pick_first_of(v, proj_keys),
        event,
        // `prompt` is the raw text of Claude UserPromptSubmit / Gemini BeforeAgent:
        // not reading it leaves that user-question line with only the theme preset phrase.
        text: pick_str(v, &["text", "message", "content", "prompt"]),
        model: model_from(v, model_keys),
    }
}

/// Inject the agent and send time into the hook payload and hand it to Core for parsing as-is.
///
/// Why send the **raw payload** instead of a parsed DTO: the Core-side enrich needs
/// `transcript_path`, `tool_input`, and each agent's raw fields; once they are normalized
/// into a DTO at the CLI boundary, these fields are lost forever (without the transcript ⇒ cannot tell "done/asking").
/// Parsing is done once, in Core; the CLI only handles identity + delivery.
///
/// `__sent_at` is the delivery moment: the offline queue replays by it when persisting, so it won't turn "working at the time"
/// into "idle at the replay moment".
pub fn stamp_payload(body: &str, agent: &str, sent_at: u64) -> String {
    stamp_payload_annotated(body, agent, sent_at, None)
}

/// Like `stamp_payload`, additionally injecting the matched rule id (`__rule`). Called after CLI rewriting;
/// Core uses it to emit `rule.applied` (audit). Like `__sent_at`, it belongs to the "inject fields only,
/// no normalization" dumb-pipe contract.
pub fn stamp_payload_annotated(
    body: &str,
    agent: &str,
    sent_at: u64,
    rule: Option<&str>,
) -> String {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let mut v = parsed
        .filter(|x| x.is_object())
        .unwrap_or_else(|| serde_json::json!({ "text": body }));
    if let Some(o) = v.as_object_mut() {
        if !agent.is_empty() && agent != "auto" {
            o.insert("agent".to_string(), serde_json::json!(agent));
        }
        o.insert("__sent_at".to_string(), serde_json::json!(sent_at));
        if let Some(r) = rule {
            o.insert("__rule".to_string(), serde_json::json!(r));
        }
    }
    v.to_string()
}

/// Turn a raw event body into a display-ready DTO.
pub fn process_body(body: &str, default_agent: &str) -> SessionDto {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let agent = pick_str(&v, &["agent"]).unwrap_or_else(|| default_agent.to_string());
    let fields = parse_hook_fields(&v, &agent);
    let text = fields.text.clone().unwrap_or_default();

    // Offline queue replay: the queue stores the DTO itself, already carrying the decided state and the then-current
    // updatedAt. Trust it directly, otherwise replay would turn "working at the time" into "idle now".
    // __sent_at is injected by the hook CLI (see stamp_payload): the original moment is preserved when replaying the queue.
    // state/updatedAt are older DTO shapes, still supported.
    let explicit_state = pick_str(&v, &["state"]).and_then(|s| parse_state(&s));
    let explicit_ts = pick_num(&v, &["__sent_at", "updatedAt", "updated_at"]);

    let raw_state = match event_mapping(&agent, &fields.event) {
        EventMapping::State(s) => s,
        // Ignore events should not reach here (the caller blocks them first with should_ignore_event),
        // but if they do, treat it as "state unchanged" and fall back to the generic heuristic.
        EventMapping::Ignore => map_state(&agent, &format!("{} {}", fields.event, text)),
        EventMapping::Unknown => {
            let mut s = map_state(&agent, &format!("{} {}", fields.event, text));
            // Claude's Stop semantics are refined by text (short text uses the keyword table)
            if agent == "claude"
                && !text.is_empty()
                && s != AgentState::Waiting
                && s != AgentState::Working
            {
                s = detect_claude_stop(&text);
            }
            s
        }
    };
    let state = explicit_state.unwrap_or(raw_state);
    let updated_at = explicit_ts.unwrap_or_else(now_secs);

    let id = fields
        .session_id
        .clone()
        .unwrap_or_else(|| format!("evt-{}", now_nanos()));
    // Keep the full path (grouping key + branch reading), the display name is still the last segment:
    // the text length and existing behavior of the tray/notifications stay unchanged.
    let cwd = fields.project.clone().unwrap_or_default();
    let project = if cwd.is_empty() {
        "unknown".to_string()
    } else {
        last_path_component(&cwd)
    };
    // Real agents' (Claude / Codex / Cursor…) hook payloads have no text field,
    // only tool_name / tool_input; without extraction the bubble can only show the theme preset phrase.
    let message = if text.is_empty() {
        tool_activity(&v)
    } else {
        title_from_transcript(&text)
    };
    let choices = parse_choices(&v);
    let answered = pick_str(&v, &["answered", "choice"]).filter(|s| !s.is_empty());
    SessionDto {
        id,
        agent,
        project,
        cwd,
        message,
        state: state_str(state).to_string(),
        started_at: updated_at,
        updated_at,
        order: String::new(),
        model: fields.model,
        speech: String::new(),
        choices,
        answered,
    }
}

/// Throttle for model-name backfill: read the transcript only once per session within 30s.
fn model_throttle() -> &'static super::throttle::PerKeyThrottle {
    use std::sync::OnceLock;
    static T: OnceLock<super::throttle::PerKeyThrottle> = OnceLock::new();
    T.get_or_init(|| super::throttle::PerKeyThrottle::new(30))
}

/// Backfill the DTO from the transcript tail. Three things:
///
/// 1. **Model name**: hook payloads mostly carry no model, only the transcript has it (which also supports
///    a mid-session `/model` switch). Throttled to 30s per session to avoid reading disk on every event.
/// 2. **Body (speech)**: the last assistant text. While working it and the tool call are two separate things —
///    the tool answers "what it's doing", the body answers "what it said"; the message of a `done` line gets replaced by the celebration text,
///    so the body is the only field that preserves the closing sentence.
/// 3. **done vs waiting**: Claude's Stop hook fires whether the task is "done" or it "stopped to ask a question",
///    and the event name cannot tell them apart. Read the last assistant text: if it looks like a question, change it to
///    waiting, and fill in a closing summary when the message is empty.
///
/// If the transcript cannot be read, skip silently — a file read failure must not affect the event itself.
pub fn enrich_from_transcript(dto: &mut SessionDto, body: &str) {
    if dto.agent != "claude" && dto.agent != "droid" {
        return;
    }
    let Some(path) = super::transcript::path_from_payload(body) else {
        return;
    };
    let is_done = dto.state == state_str(AgentState::Done);
    // done must read (to judge a question + closing body); other states are for model-name/body backfill, throttled per session.
    if !is_done {
        let need_model = dto.model.is_empty();
        let need_speech = dto.speech.is_empty();
        if !need_model && !need_speech {
            return;
        }
        if !model_throttle().should_run(&dto.id, now_secs()) {
            return;
        }
    }
    let Some(tail) = super::transcript::read_tail(&path) else {
        return;
    };
    if dto.model.is_empty() {
        dto.model = tail.model.clone();
    }
    if tail.latest_assistant_text.is_empty() {
        return;
    }
    let text = title_from_transcript(&tail.latest_assistant_text);
    if is_done {
        // An empty message is filled with the closing summary (used by the notification body/history); the body is kept separately for the bubble.
        if dto.message.is_empty() {
            dto.message = text.clone();
        }
        dto.speech = text;
        if looks_like_question(&tail.latest_assistant_text) {
            dto.state = state_str(AgentState::Waiting).to_string();
        }
    } else {
        dto.speech = text;
    }
}

/// Extract a "what it's doing" phrase from the hook payload's tool call, such as `Edit src/overlay.ts` or
/// `Bash npm test`. When there is no tool info, return an empty string (the frontend falls back to the theme phrase).
fn tool_activity(v: &serde_json::Value) -> String {
    let Some(tool) = pick_str(v, &["tool_name", "toolName", "tool"]) else {
        return String::new();
    };
    let detail = ["tool_input", "toolInput", "arguments"]
        .iter()
        .find_map(|k| v.get(*k).and_then(tool_input_summary))
        .unwrap_or_default();
    if detail.is_empty() {
        tool
    } else {
        format!("{} {}", tool, detail)
    }
}

/// Pick the field from tool_input that best describes the operation target (file paths take priority over commands/queries).
fn tool_input_summary(input: &serde_json::Value) -> Option<String> {
    if let Some(s) = input.as_str() {
        let line = first_line(s);
        return (!line.is_empty()).then_some(line);
    }
    let obj = input.as_object()?;
    const KEYS: [&str; 9] = [
        "file_path",
        "filePath",
        "path",
        "command",
        "pattern",
        "query",
        "url",
        "notebook_path",
        "description",
    ];
    for key in KEYS {
        if let Some(s) = obj.get(key).and_then(|x| x.as_str()) {
            let line = first_line(s);
            if !line.is_empty() {
                return Some(line);
            }
        }
    }
    None
}

/// Take the first non-empty line and truncate it, to avoid stuffing the whole old_string / content into the bubble.
fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(60)
        .collect()
}

/// Parse `choices: [{id, label}, ...]` out of the JSON; invalid items are dropped.
fn parse_choices(v: &serde_json::Value) -> Option<Vec<Choice>> {
    let arr = v.get("choices").and_then(|x| x.as_array())?;
    let out: Vec<Choice> = arr
        .iter()
        .filter_map(|c| {
            let id = c.get("id").and_then(|x| x.as_str())?;
            let label = c.get("label").and_then(|x| x.as_str()).unwrap_or(id);
            Some(Choice {
                id: id.to_string(),
                label: label.to_string(),
            })
        })
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub fn session_to_dto(s: &Session) -> SessionDto {
    SessionDto {
        id: s.id.clone(),
        agent: s.agent.clone(),
        project: s.project.clone(),
        cwd: s.cwd.clone(),
        message: s.message.clone(),
        state: state_str(s.state).to_string(),
        started_at: s.started_at,
        updated_at: s.updated_at,
        order: order_key(s),
        model: s.model.clone(),
        speech: s.speech.clone(),
        choices: s.choices.clone(),
        answered: s.answered.clone(),
    }
}

pub fn dto_to_session(d: &SessionDto) -> Session {
    let state = match d.state.as_str() {
        "working" => AgentState::Working,
        "waiting" => AgentState::Waiting,
        "done" => AgentState::Done,
        _ => AgentState::Idle,
    };
    Session {
        id: d.id.clone(),
        agent: d.agent.clone(),
        project: d.project.clone(),
        cwd: d.cwd.clone(),
        message: d.message.clone(),
        state,
        started_at: d.started_at,
        updated_at: d.updated_at,
        model: d.model.clone(),
        speech: d.speech.clone(),
        choices: d.choices.clone(),
        answered: d.answered.clone(),
    }
}

#[derive(Debug, Default)]
pub struct SessionStore {
    inner: HashMap<String, Session>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }
}

impl SessionSink for SessionStore {
    fn upsert(&mut self, mut s: Session) {
        // started_at is the "time the session first appeared"; keep the old value when it already exists
        if let Some(old) = self.inner.get(&s.id) {
            if old.started_at > 0 {
                s.started_at = old.started_at;
            }
        }
        self.inner.insert(s.id.clone(), s);
    }

    fn get(&self, id: &str) -> Option<Session> {
        self.inner.get(id).cloned()
    }

    fn active(&self, now: u64) -> Vec<Session> {
        self.inner
            .values()
            .filter(|s| is_active(s, now))
            .cloned()
            .collect()
    }

    fn all(&self) -> Vec<Session> {
        self.inner.values().cloned().collect()
    }

    fn dismiss(&mut self, id: &str) {
        self.inner.remove(id);
    }

    fn clear(&mut self) {
        self.inner.clear();
    }

    fn sweep(&mut self, now: u64) -> usize {
        let expired: Vec<String> = self
            .inner
            .values()
            .filter(|s| !is_active(s, now))
            .map(|s| s.id.clone())
            .collect();
        for id in &expired {
            self.inner.remove(id);
        }
        expired.len()
    }
}

#[cfg(test)]
mod tests {
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
            let body =
                format!(r#"{{"agent":"claude","hook_event_name":"{event}","session_id":"s"}}"#);
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
        let done =
            r#"{"agent":"antigravity","conversationId":"a-1","terminationReason":"completed"}"#;
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
}
