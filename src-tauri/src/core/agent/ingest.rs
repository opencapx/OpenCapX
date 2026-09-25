//! the ingest pipeline: process_body, model throttle, transcript enrichment, choice parsing.
//! Mechanical move from core/agent.rs.

use super::*;

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
fn model_throttle() -> &'static crate::core::throttle::PerKeyThrottle {
    use std::sync::OnceLock;
    static T: OnceLock<crate::core::throttle::PerKeyThrottle> = OnceLock::new();
    T.get_or_init(|| crate::core::throttle::PerKeyThrottle::new(30))
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
    let Some(path) = crate::core::transcript::path_from_payload(body) else {
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
    let Some(tail) = crate::core::transcript::read_tail(&path) else {
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
