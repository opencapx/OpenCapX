//! hook payload field extraction (message/model/cwd/project) and payload stamping.
//! Mechanical move from core/agent.rs.

use super::*;

/// Structured fields extracted from the payload. Each agent names its fields differently; they are unified here.
#[derive(Debug, Default, Clone)]
pub(crate) struct HookFields {
    pub(crate) session_id: Option<String>,
    pub(crate) project: Option<String>,
    pub(crate) event: String,
    pub(crate) text: Option<String>,
    pub(crate) model: String,
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
pub(crate) fn parse_hook_fields(v: &serde_json::Value, agent: &str) -> HookFields {
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
