//! host event → AgentState mapping: per-agent event tables, payload picking, ignore rules.
//! Mechanical move from core/agent.rs.

use super::*;

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

pub(crate) fn pick_num(v: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    for k in keys {
        if let Some(n) = v.get(*k).and_then(|x| x.as_u64()) {
            return Some(n);
        }
    }
    None
}

pub(crate) fn pick_str(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

pub(crate) fn last_path_component(p: &str) -> String {
    p.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("unknown")
        .to_string()
}

/// The result of mapping an event name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventMapping {
    State(AgentState),
    /// Events that explicitly "do not touch the state". SubagentStop is typical: a child agent finishing does not mean
    /// the main session state changed; treating it as done would immediately be pushed back by the next working, causing a flicker.
    Ignore,
    /// Not in the table → handed to the generic heuristic `map_state`.
    Unknown,
}

/// Event name → state table for each agent (names are lowercased before comparison, compatible with PascalCase /
/// camelCase / snake_case naming conventions).
pub(crate) fn event_mapping(agent: &str, event: &str) -> EventMapping {
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
