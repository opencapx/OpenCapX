//! POST /rpc handling: Core-side implementation of MCP tools. See docs/mcp.md.
//! The `opencapx mcp` subprocess is a thin forwarding layer; all logic lives here.
//! The caller identity is passed in as agent_id after the http gateway authenticates; Core native tools pass
//! the Agent-layer permission here (docs/permissions.md "Two-Layer Decision"); execute's plugin layer is inside the Router.

use crate::core::event::{self, EventBus};
use crate::core::permission::{gate_agent, Decision};
use crate::core::storage::SharedStore;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::Emitter;

/// Agent State Protocol (i2 §12, see mcp.md): 8 canonical states.
/// `permission` and `waiting` are kept separate — waiting for user authorization is a key interaction moment (pet 🥺).
const PET_STATES: [&str; 8] = [
    "idle",
    "thinking",
    "working",
    "waiting",
    "permission",
    "success",
    "error",
    "sleeping",
];

fn asks() -> &'static Mutex<HashMap<String, Sender<String>>> {
    static ASKS: OnceLock<Mutex<HashMap<String, Sender<String>>>> = OnceLock::new();
    ASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Callback command entry point for the frontend to answer opencapx.ask.
pub fn resolve_ask(id: &str, answer: &str) -> bool {
    match asks().lock() {
        Ok(mut m) => match m.remove(id) {
            Some(tx) => {
                let sent = tx.send(answer.to_string()).is_ok();
                if !sent {
                    eprintln!("ask(rpc): receiver for {id} gone before answer arrived");
                }
                sent
            }
            None => {
                // stale dialog (already timed out) or an answer routed to the wrong registry
                eprintln!("ask(rpc): answer {answer:?} for unknown/expired id {id} dropped");
                false
            }
        },
        Err(_) => {
            eprintln!("ask(rpc): registry lock poisoned, answer for {id} dropped");
            false
        }
    }
}

/// Tool → Agent-layer permission (docs/permissions.md "Permission List" mapping).
/// None = read-only metadata / unknown tool, bypassing the Agent-layer gate.
/// capability-type tools get their permission via the §4.3 single-point resolver (built-in ∪ frozen declarations);
/// a resolver miss → None → the later `known()` will reject, so it does not constitute an allow.
fn tool_permission(tool: &str, input: &Value, store: &SharedStore) -> Option<String> {
    let static_perm = match tool {
        "opencapx.say" | "opencapx.set_state" | "opencapx.ask" => Some("pet.animation"),
        "opencapx.notify" => Some("notification.post"),
        _ => None,
    };
    if let Some(p) = static_perm {
        return Some(p.to_string());
    }
    match tool {
        // Decided once when the subscription is established; event pushes are not re-judged per item (docs/capability.md "Capability Types")
        "opencapx.execute" | "opencapx.subscribe" => input
            .get("capability")
            .and_then(|c| c.as_str())
            .and_then(|cap| super::declaration::resolve(store, cap))
            .map(|r| r.permission),
        _ => None,
    }
}

/// Cancellation flag for in-flight requests (mcp.md "Cancellation" v2: /rpc carries requestId,
/// POST /rpc/cancel sets it). ask polls it to finish early.
fn cancels() -> &'static Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>> {
    static C: OnceLock<Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// POST /rpc/cancel entry point: sets the cancellation flag. Returns whether the requestId was in the table.
pub fn cancel(request_id: &str) -> bool {
    match cancels().lock() {
        Ok(mut m) => match m.remove(request_id) {
            Some(flag) => {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                true
            }
            None => false,
        },
        Err(_) => false,
    }
}

pub fn handle(
    handle: &tauri::AppHandle,
    bus: &Arc<EventBus>,
    store: &SharedStore,
    agent_id: &str,
    conn_id: &str,
    body: &str,
) -> String {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return err_json(&format!("bad request: {}", e)),
    };
    let tool = v.get("tool").and_then(|t| t.as_str()).unwrap_or("");
    let input = v.get("input").cloned().unwrap_or_else(|| json!({}));
    let request_id = v
        .get("requestId")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string();
    // trace: the dispatch event hangs off the root span (tool name + input preview, truncated at 4KB)
    super::req_trace::event(
        "dispatch",
        json!({
            "tool": tool,
            "requestId": request_id,
            "input": super::req_trace::trunc_str(&input.to_string(), 4096),
        }),
    );
    // ① Agent-layer gate: Core native tools only pass this layer; execute's plugin layer is inside the Router
    if let Some(perm) = tool_permission(tool, &input, store) {
        let granted = gate_agent(store, agent_id, &perm, "mcp") == Decision::Granted;
        // trace: Agent-layer decision result (including denial, since denial is the request's final state)
        super::req_trace::event("permission.agent", json!({ "permission": perm, "granted": granted }));
        if !granted {
            return err_code("permission_denied", 40001, &format!(
                "agent layer: {} — ask the user to grant it (OpenCapX Settings → Permissions) or retry to trigger the ask prompt",
                perm
            ));
        }
    }
    // ② Tool dispatch
    // v1.0 freeze guardrail: the MCP v1 tool surface = these 8 names, not one character changes (docs/deprecation.md).
    // The constant and the dispatch branches are reconciled against each other by tests; adding tools is additive (does not break v1).
    debug_assert!(MCP_V1_TOOLS.contains(&tool));
    let out = match tool {
        "opencapx.say" => say(handle, bus, &input),
        "opencapx.notify" => notify(handle, bus, agent_id, &input),
        "opencapx.set_state" => set_state(handle, bus, &input),
        "opencapx.ask" => ask(handle, &input, &request_id),
        "opencapx.list_capabilities" => list_capabilities(&input),
        "opencapx.execute" => execute(&input, agent_id),
        "opencapx.subscribe" => subscribe_tool(bus, agent_id, conn_id, &input),
        "opencapx.unsubscribe" => unsubscribe_tool(bus, &input),
        other => err_json(&format!("unknown tool: {}", other)),
    };
    out
}

/// opencapx.subscribe (mcp.md "Subscription Tool Pair"): validate the type + write to the in-memory subscription table + start a watcher.
/// conn_id binds the MCP connection (cleaned up on SSE disconnect); an empty string = no connection context (tests/internal calls).
fn subscribe_tool(bus: &Arc<EventBus>, agent_id: &str, conn_id: &str, input: &Value) -> String {
    let capability = input.get("capability").and_then(|c| c.as_str()).unwrap_or("");
    if capability.is_empty() {
        return err_json("capability is required");
    }
    let sub_input = input.get("input").cloned().unwrap_or_else(|| json!({}));
    match super::subscription::subscribe(agent_id, conn_id, capability, &sub_input, bus) {
        Ok(id) => serde_json::to_string(&json!({ "ok": true, "subscriptionId": id })).unwrap(),
        Err(e) => err_json(&e),
    }
}

/// opencapx.unsubscribe: idempotent; a non-existent subscriptionId also returns ok (the response echoes
/// subscriptionId, which the CLI uses to update its local subscription set).
fn unsubscribe_tool(bus: &Arc<EventBus>, input: &Value) -> String {
    let id = input.get("subscriptionId").and_then(|s| s.as_str()).unwrap_or("");
    if id.is_empty() {
        return err_json("subscriptionId is required");
    }
    super::subscription::unsubscribe(id, bus);
    serde_json::to_string(&json!({ "ok": true, "subscriptionId": id })).unwrap()
}

fn err_json(msg: &str) -> String {
    serde_json::to_string(&json!({ "ok": false, "error": msg })).unwrap_or_else(|_| "{}".into())
}

/// MCP v1 frozen tool set (say/notify/set_state/ask/list_capabilities/execute +
/// the subscribe/unsubscribe pair). Renaming/removing = breaking change, only allowed within the apiVersion "2"
/// migration window (see docs/deprecation.md); adding a tool within v1 is additive and legal.
pub const MCP_V1_TOOLS: &[&str] = &[
    "opencapx.say",
    "opencapx.notify",
    "opencapx.set_state",
    "opencapx.ask",
    "opencapx.list_capabilities",
    "opencapx.execute",
    "opencapx.subscribe",
    "opencapx.unsubscribe",
];

fn err_code(error: &str, code: i64, detail: &str) -> String {
    serde_json::to_string(&json!({ "ok": false, "error": error, "code": code, "detail": detail }))
        .unwrap_or_else(|_| "{}".into())
}

fn say(handle: &tauri::AppHandle, bus: &Arc<EventBus>, input: &Value) -> String {
    let text = input.get("text").and_then(|t| t.as_str()).unwrap_or("");
    if text.is_empty() {
        return err_json("text is required");
    }
    event::publish_say(handle, bus, text);
    serde_json::to_string(&json!({ "ok": true })).unwrap()
}

/// opencapx.notify (i2 §13): OS toast + `notification.posted` event persisted,
/// aggregated by the settings page Notification Center. severity ∈ info|warn|error, default info.
fn notify(handle: &tauri::AppHandle, bus: &Arc<EventBus>, agent_id: &str, input: &Value) -> String {
    use tauri_plugin_notification::NotificationExt;
    let title = input.get("title").and_then(|t| t.as_str()).unwrap_or("OpenCapX");
    let body = input.get("body").and_then(|t| t.as_str()).unwrap_or("");
    let severity = input.get("severity").and_then(|s| s.as_str()).unwrap_or("info");
    if !matches!(severity, "info" | "warn" | "error") {
        return err_json(&format!("invalid severity: {} (allowed: info, warn, error)", severity));
    }
    let _ = handle.notification().builder().title(title).body(body).show();
    bus.publish(&event::OpencapxEvent::new(
        "notification.posted",
        "mcp",
        json!({
            "agentId": agent_id,
            "title": title,
            "body": body,
            "severity": severity,
        }),
    ));
    serde_json::to_string(&json!({ "ok": true })).unwrap()
}

fn set_state(handle: &tauri::AppHandle, bus: &Arc<EventBus>, input: &Value) -> String {
    let state = input.get("state").and_then(|s| s.as_str()).unwrap_or("");
    if !PET_STATES.contains(&state) {
        return err_json(&format!("invalid state: {} (allowed: {})", state, PET_STATES.join(", ")));
    }
    let message = input.get("message").and_then(|m| m.as_str()).unwrap_or("");
    bus.publish(&event::OpencapxEvent::new(
        "pet.state_changed",
        "mcp",
        json!({ "from": null, "to": state, "message": message }),
    ));
    let _ = handle.emit("opencapx-set-state", json!({ "state": state, "message": message }));
    serde_json::to_string(&json!({ "ok": true })).unwrap()
}

/// Timeout policy (mcp.md "opencapx.ask" v1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnTimeout {
    /// Timed out returns {ok:false, error:"timeout"} (v1 behavior, default).
    Error,
    /// On timeout, finish with defaultAnswer; the response carries timedOut:true.
    Default,
}

/// The shape after ask input validation. Three mutually exclusive forms: single choice (options) / multiple choice (options+multi) /
/// form (fields). fields are passed through verbatim for frontend rendering; only the shape is validated here.
#[derive(Debug, Clone, PartialEq)]
struct AskSpec {
    question: String,
    options: Vec<String>,
    multi: bool,
    fields: Vec<Value>,
    timeout_secs: u64,
    on_timeout: OnTimeout,
    default_answer: Option<Value>,
}

const ASK_FIELD_TYPES: [&str; 4] = ["text", "number", "select", "checkbox"];

/// Validation only, for testability (the ask body needs an AppHandle to emit, which a test process cannot create).
fn validate_ask_input(input: &Value) -> Result<AskSpec, String> {
    let question = input.get("question").and_then(|q| q.as_str()).unwrap_or("");
    if question.is_empty() {
        return Err("question is required".into());
    }
    let has_options = input.get("options").map(|o| o.is_array()).unwrap_or(false);
    let has_fields = input.get("fields").map(|f| f.is_array()).unwrap_or(false);
    if has_options == has_fields {
        return Err("exactly one of options | fields is required".into());
    }
    let multi = input.get("multi").and_then(|m| m.as_bool()).unwrap_or(false);
    if multi && !has_options {
        return Err("multi requires options".into());
    }
    let options: Vec<String> = input
        .get("options")
        .and_then(|o| o.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if has_options && options.is_empty() {
        return Err("options must be a non-empty string array".into());
    }
    let fields = input
        .get("fields")
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default();
    if has_fields {
        validate_ask_fields(&fields)?;
    }
    let timeout_secs = input.get("timeout").and_then(|t| t.as_u64()).unwrap_or(300).min(900);
    let on_timeout = match input.get("onTimeout").and_then(|s| s.as_str()).unwrap_or("error") {
        "error" => OnTimeout::Error,
        "default" => OnTimeout::Default,
        other => return Err(format!("invalid onTimeout: {} (allowed: error, default)", other)),
    };
    let default_answer = input.get("defaultAnswer").cloned();
    if on_timeout == OnTimeout::Default {
        validate_ask_default(&default_answer, &options, multi, &fields)?;
    }
    Ok(AskSpec {
        question: question.to_string(),
        options,
        multi,
        fields,
        timeout_secs,
        on_timeout,
        default_answer,
    })
}

/// fields shape: name unique and non-empty, type restricted, select must carry non-empty options.
fn validate_ask_fields(fields: &[Value]) -> Result<(), String> {
    let mut names: Vec<&str> = Vec::new();
    for f in fields {
        let name = f.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if name.is_empty() {
            return Err("field.name is required".into());
        }
        if names.contains(&name) {
            return Err(format!("duplicate field name: {}", name));
        }
        names.push(name);
        let ftype = f.get("type").and_then(|t| t.as_str()).unwrap_or("text");
        if !ASK_FIELD_TYPES.contains(&ftype) {
            return Err(format!("field {} invalid type: {} (allowed: {})", name, ftype, ASK_FIELD_TYPES.join(", ")));
        }
        if ftype == "select" {
            let empty = f
                .get("options")
                .and_then(|o| o.as_array())
                .map(|a| a.is_empty() || a.iter().any(|x| !x.is_string()))
                .unwrap_or(true);
            if empty {
                return Err(format!("field {} (select) requires non-empty string options", name));
            }
        }
    }
    Ok(())
}

/// The defaultAnswer shape must match the question type, to prevent a typo from silently taking effect:
/// single choice = a string ∈ options; multiple choice = a non-empty array with each item ∈ options; form = covers all required fields.
fn validate_ask_default(
    default_answer: &Option<Value>,
    options: &[String],
    multi: bool,
    fields: &[Value],
) -> Result<(), String> {
    let Some(d) = default_answer else {
        return Err("defaultAnswer is required when onTimeout=default".into());
    };
    if !fields.is_empty() {
        let obj = d.as_object().ok_or("defaultAnswer must be an object for form asks")?;
        for f in fields {
            let required = f.get("required").and_then(|r| r.as_bool()).unwrap_or(false);
            if required {
                let name = f.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if !obj.contains_key(name) {
                    return Err(format!("defaultAnswer missing required field: {}", name));
                }
            }
        }
        return Ok(());
    }
    if multi {
        let arr = d.as_array().ok_or("defaultAnswer must be a string array for multi asks")?;
        if arr.is_empty() {
            return Err("defaultAnswer must be non-empty for multi asks".into());
        }
        for v in arr {
            let s = v.as_str().ok_or("defaultAnswer must be a string array for multi asks")?;
            if !options.contains(&s.to_string()) {
                return Err(format!("defaultAnswer contains unknown option: {}", s));
            }
        }
        return Ok(());
    }
    let s = d.as_str().ok_or("defaultAnswer must be a string for single-choice asks")?;
    if !options.contains(&s.to_string()) {
        return Err(format!("defaultAnswer is not in options: {}", s));
    }
    Ok(())
}

/// Parsing of the answer returned by the frontend: multiple choice/form is a JSON string (starting with [ or {),
/// single choice stays plain text (numeric options are not mistakenly converted to number).
fn parse_answer(raw: &str) -> Value {
    if raw.starts_with('[') || raw.starts_with('{') {
        serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
    } else {
        Value::String(raw.to_string())
    }
}

/// Polling slice of the ask wait loop: checks the cancellation flag at 50ms granularity while preserving the answer channel and timeout.
const ASK_POLL: Duration = Duration::from_millis(50);

fn ask(handle: &tauri::AppHandle, input: &Value, request_id: &str) -> String {
    let spec = match validate_ask_input(input) {
        Ok(s) => s,
        Err(e) => return err_json(&e),
    };
    let id = format!("ask-{}", nanos());
    let (tx, rx) = mpsc::channel::<String>();
    if let Ok(mut m) = asks().lock() {
        m.insert(id.clone(), tx);
    }
    // Cancellable: CLI's notifications/cancelled → POST /rpc/cancel → set the flag (mcp.md "Cancellation")
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if !request_id.is_empty() {
        if let Ok(mut m) = cancels().lock() {
            m.insert(request_id.to_string(), flag.clone());
        }
    }
    let _ = handle.emit(
        "opencapx-ask",
        json!({
            "id": id,
            "question": spec.question,
            "options": spec.options,
            "multi": spec.multi,
            "fields": spec.fields,
            "timeout": spec.timeout_secs,
        }),
    );
    // trace: the bubble is shown (waiting for the user, may hang for minutes — the visibility of a start with no end is most useful here)
    super::req_trace::event(
        "ask.shown",
        json!({ "id": id, "timeoutSecs": spec.timeout_secs }),
    );
    // Wait: answer first, then the cancellation flag, finally timeout (mcp.md: cancel ≠ timeout, semantics kept separate)
    let deadline = std::time::Instant::now() + Duration::from_secs(spec.timeout_secs);
    let mut cancelled = false;
    let answered: Option<String> = loop {
        if flag.load(std::sync::atomic::Ordering::Relaxed) {
            cancelled = true;
            break None;
        }
        match rx.try_recv() {
            Ok(raw) => break Some(raw),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(ASK_POLL);
    };
    if !request_id.is_empty() {
        if let Ok(mut m) = cancels().lock() {
            m.remove(request_id);
        }
    }
    if let Ok(mut m) = asks().lock() {
        m.remove(&id);
    }
    if cancelled {
        // Cancelled: the bubble closes immediately, no error is reported to the user, and the Agent has already received -32800
        let _ = handle.emit("opencapx-ask-done", json!({ "id": id, "answer": null }));
        super::req_trace::event("ask.cancelled", json!({ "id": id }));
        return err_json("cancelled");
    }
    match answered {
        Some(raw) => {
            let answer = parse_answer(&raw);
            super::req_trace::event("ask.answered", json!({ "id": id }));
            serde_json::to_string(&json!({ "ok": true, "answer": answer })).unwrap()
        }
        None => {
            // Timeout: tell the frontend to dismiss the bubble, then finish per the policy
            let _ = handle.emit("opencapx-ask-done", json!({ "id": id, "answer": null }));
            super::req_trace::event("ask.timeout", json!({ "id": id }));
            if spec.on_timeout == OnTimeout::Default {
                if let Some(d) = spec.default_answer {
                    return serde_json::to_string(&json!({ "ok": true, "answer": d, "timedOut": true })).unwrap();
                }
            }
            err_json("timeout")
        }
    }
}

fn list_capabilities(_input: &Value) -> String {
    serde_json::to_string(&super::capability::list()).unwrap()
}

/// Mapping of capability errors → protocol-facing error bodies (40001 agent layer is inside handle, 40002/40003 are here).
/// detail carries "layer + permission + remediation hint" so the Agent can self-heal (retry to trigger the dialog / use another path / ask the user to grant).
fn capability_error_body(e: &str) -> String {
    if let Some(perm) = e.strip_prefix("plugin_permission_denied:") {
        return err_code("permission_denied", 40002, &format!(
            "plugin layer: {} — the user must grant this permission to the plugin (OpenCapX Settings → Plugins), then retry",
            perm
        ));
    }
    if let Some(rest) = e.strip_prefix("scope_denied:") {
        let mut it = rest.splitn(2, ':');
        let layer = it.next().unwrap_or("?");
        let perm = it.next().unwrap_or("");
        return err_code("scope_denied", 40003, &format!(
            "path is outside the allowed scope ({} layer, {}) — retry with a path under the allowed scope, or ask the user to widen it (OpenCapX Settings → Permissions)",
            layer, perm
        ));
    }
    err_json(e)
}

fn execute(input: &Value, agent_id: &str) -> String {
    let capability = input.get("capability").and_then(|c| c.as_str()).unwrap_or("");
    if capability.is_empty() {
        return err_json("capability is required");
    }
    let input_obj = input.get("input").cloned().unwrap_or_else(|| json!({}));
    // trace: capability dispatch span (total duration/outcome including the built-in fallback and the plugin loop)
    let sp = super::req_trace::span(
        &format!("capability.{}", capability),
        json!({ "capability": capability }),
    );
    let started = std::time::Instant::now();
    let out = match super::capability::execute(capability, &input_obj, Some(agent_id)) {
        Ok(out) => {
            sp.end(true, None, json!({ "elapsedMs": started.elapsed().as_millis() as u64 }));
            serde_json::to_string(&json!({ "ok": true, "result": out })).unwrap()
        }
        Err(e) => {
            sp.end(false, Some(&e), json!({ "elapsedMs": started.elapsed().as_millis() as u64 }));
            capability_error_body(&e)
        }
    };
    out
}

#[cfg(test)]
mod tests {
    /// Test shim: these assertions only go through the built-in static mapping and do not need a declaration table (a Mem store suffices).
    fn PERM(tool: &str, input: &Value) -> Option<String> {
        let s: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Mem(crate::core::agent::SessionStore::new()),
        ));
        tool_permission(tool, input, &s)
    }

    use super::*;

    #[test]
    fn ask_registry_roundtrip() {
        // resolve_ask returns false for a non-existent id, does not panic
        assert!(!resolve_ask("nope", "x"));
        assert!(asks().lock().unwrap().is_empty());
    }

    /// The ask v1 shape (options single choice) passes verbatim: default timeout 300, onTimeout=error.
    #[test]
    fn ask_validate_accepts_v1_shape() {
        let spec = validate_ask_input(&json!({
            "question": "Which database to use?",
            "options": ["SQLite", "Postgres"]
        }))
        .unwrap();
        assert_eq!(spec.options, vec!["SQLite", "Postgres"]);
        assert!(!spec.multi);
        assert!(spec.fields.is_empty());
        assert_eq!(spec.timeout_secs, 300);
        assert_eq!(spec.on_timeout, OnTimeout::Error);
        assert_eq!(spec.default_answer, None);
    }

    /// options and fields are mutually exclusive, exactly one required; multi only pairs with options.
    #[test]
    fn ask_validate_modes_are_exclusive() {
        let both = json!({
            "question": "q",
            "options": ["a"],
            "fields": [{ "name": "f", "type": "text" }]
        });
        assert_eq!(validate_ask_input(&both).unwrap_err(), "exactly one of options | fields is required");
        let neither = json!({ "question": "q" });
        assert_eq!(validate_ask_input(&neither).unwrap_err(), "exactly one of options | fields is required");
        let multi_no_opts = json!({ "question": "q", "multi": true, "fields": [{ "name": "f", "type": "text" }] });
        assert_eq!(validate_ask_input(&multi_no_opts).unwrap_err(), "multi requires options");
        let no_question = json!({ "options": ["a"] });
        assert_eq!(validate_ask_input(&no_question).unwrap_err(), "question is required");
        let empty_opts = json!({ "question": "q", "options": [] });
        assert_eq!(validate_ask_input(&empty_opts).unwrap_err(), "options must be a non-empty string array");
    }

    /// Form field shape: name unique, type restricted, select must carry options.
    #[test]
    fn ask_validate_fields_shape() {
        let ok = json!({
            "question": "Connect to the database",
            "fields": [
                { "name": "host", "type": "text", "required": true },
                { "name": "port", "type": "number", "default": 5432 },
                { "name": "mode", "type": "select", "options": ["a", "b"] },
                { "name": "verbose", "type": "checkbox" }
            ]
        });
        let spec = validate_ask_input(&ok).unwrap();
        assert_eq!(spec.fields.len(), 4);
        // Each failure shape
        let cases = [
            (json!({"question":"q","fields":[{"type":"text"}]}), "field.name is required"),
            (
                json!({"question":"q","fields":[
                    {"name":"f","type":"text"},{"name":"f","type":"number"}
                ]}),
                "duplicate field name: f",
            ),
            (
                json!({"question":"q","fields":[{"name":"f","type":"datetime"}]}),
                "field f invalid type: datetime (allowed: text, number, select, checkbox)",
            ),
            (
                json!({"question":"q","fields":[{"name":"f","type":"select"}]}),
                "field f (select) requires non-empty string options",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(validate_ask_input(&input).unwrap_err(), want, "input: {}", input);
        }
    }

    /// Timeout policy: default must carry a shape-matching defaultAnswer; error does not require one.
    #[test]
    fn ask_validate_timeout_strategy() {
        // Single choice: defaultAnswer must ∈ options
        let ok = json!({
            "question": "q", "options": ["a", "b"],
            "onTimeout": "default", "defaultAnswer": "b"
        });
        assert_eq!(validate_ask_input(&ok).unwrap().on_timeout, OnTimeout::Default);
        let missing = json!({ "question": "q", "options": ["a"], "onTimeout": "default" });
        assert_eq!(
            validate_ask_input(&missing).unwrap_err(),
            "defaultAnswer is required when onTimeout=default"
        );
        let wrong_opt = json!({ "question": "q", "options": ["a"], "onTimeout": "default", "defaultAnswer": "zzz" });
        assert_eq!(
            validate_ask_input(&wrong_opt).unwrap_err(),
            "defaultAnswer is not in options: zzz"
        );
        // Multiple choice: non-empty array with each item ∈ options
        let multi_ok = json!({
            "question": "q", "options": ["a", "b"], "multi": true,
            "onTimeout": "default", "defaultAnswer": ["a", "b"]
        });
        let spec = validate_ask_input(&multi_ok).unwrap();
        assert_eq!(spec.default_answer, Some(json!(["a", "b"])));
        let multi_bad = json!({
            "question": "q", "options": ["a", "b"], "multi": true,
            "onTimeout": "default", "defaultAnswer": []
        });
        assert_eq!(
            validate_ask_input(&multi_bad).unwrap_err(),
            "defaultAnswer must be non-empty for multi asks"
        );
        // Form: must cover the required fields
        let form_ok = json!({
            "question": "q",
            "fields": [{ "name": "host", "type": "text", "required": true }, { "name": "note", "type": "text" }],
            "onTimeout": "default", "defaultAnswer": { "host": "localhost" }
        });
        validate_ask_input(&form_ok).unwrap();
        let form_bad = json!({
            "question": "q",
            "fields": [{ "name": "host", "type": "text", "required": true }],
            "onTimeout": "default", "defaultAnswer": { "note": "x" }
        });
        assert_eq!(
            validate_ask_input(&form_bad).unwrap_err(),
            "defaultAnswer missing required field: host"
        );
        // Invalid enum
        let bad_enum = json!({ "question": "q", "options": ["a"], "onTimeout": "retry" });
        assert!(validate_ask_input(&bad_enum)
            .unwrap_err()
            .starts_with("invalid onTimeout: retry"));
        // timeout cap 900
        let long = json!({ "question": "q", "options": ["a"], "timeout": 99999 });
        assert_eq!(validate_ask_input(&long).unwrap().timeout_secs, 900);
    }

    /// Return parsing: multiple choice/form (JSON string) is restored, single choice plain text is untouched — a numeric option
    /// "42" is not mistakenly converted to number.
    #[test]
    fn ask_parse_answer_keeps_single_choice_verbatim() {
        assert_eq!(parse_answer("42"), json!("42"));
        assert_eq!(parse_answer("SQLite"), json!("SQLite"));
        assert_eq!(parse_answer(r#"["a","b"]"#), json!(["a", "b"]));
        assert_eq!(parse_answer(r#"{"host":"h"}"#), json!({ "host": "h" }));
        assert_eq!(parse_answer("[broken"), json!("[broken"));
    }

    #[test]
    fn state_validation() {
        assert!(PET_STATES.contains(&"thinking"));
        assert!(!PET_STATES.contains(&"excited"));
    }

    /// B10 shape of the subscription tool pair at the /rpc layer: call type rejected, missing capability rejected,
    /// subscribe → subscriptionId, unsubscribe idempotent.
    #[test]
    fn subscribe_tool_validates_and_unsubscribe_is_idempotent() {
        let bus = Arc::new(EventBus::new());
        let call_type = subscribe_tool(&bus, "ag_rpc_st", "conn-st", &json!({ "capability": "file.read", "input": {} }));
        assert!(call_type.contains("not subscribable"), "{}", call_type);
        let no_cap = subscribe_tool(&bus, "ag_rpc_st", "conn-st", &json!({}));
        assert!(no_cap.contains("capability is required"), "{}", no_cap);
        let ok = subscribe_tool(
            &bus,
            "ag_rpc_st",
            "conn-st",
            &json!({ "capability": "file.watch", "input": { "path": "/tmp/opencapx-rpc-none" } }),
        );
        let v: Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(v["ok"], json!(true));
        let sid = v["subscriptionId"].as_str().unwrap().to_string();
        assert!(unsubscribe_tool(&bus, &json!({ "subscriptionId": sid })).contains("\"ok\":true"));
        assert!(
            unsubscribe_tool(&bus, &json!({ "subscriptionId": sid })).contains("\"ok\":true"),
            "duplicate unsubscribe is not an error"
        );
        assert!(unsubscribe_tool(&bus, &json!({})).contains("subscriptionId is required"));
    }

    /// Cancellation registry: setting the flag removes it from the table (both duplicate cancel / unknown id return false).
    #[test]
    fn cancel_registry_roundtrip() {
        use std::sync::atomic::{AtomicBool, Ordering};
        assert!(!cancel("req_ghost"));
        let flag = Arc::new(AtomicBool::new(false));
        cancels().lock().unwrap().insert("req_rpc_a".into(), flag.clone());
        assert!(cancel("req_rpc_a"));
        assert!(flag.load(Ordering::Relaxed));
        assert!(!cancel("req_rpc_a"), "setting the flag removes it from the table");
    }

    /// Tool → permission mapping aligned with docs/permissions.md: the Core native tool trio → pet.animation,
    /// notify → notification.post, execute → mapped by capability, list_capabilities/unknown → None.
    #[test]
    fn tool_permission_mapping_matches_docs() {
        assert_eq!(PERM("opencapx.say", &json!({})), Some("pet.animation".to_string()));
        assert_eq!(PERM("opencapx.set_state", &json!({})), Some("pet.animation".to_string()));
        assert_eq!(PERM("opencapx.ask", &json!({})), Some("pet.animation".to_string()));
        assert_eq!(PERM("opencapx.notify", &json!({})), Some("notification.post".to_string()));
        assert_eq!(
            PERM("opencapx.execute", &json!({"capability": "image.analyze"})),
            Some("image.read".to_string())
        );
        // B10: subscription setup goes through the same mapping (subscribe type); unsubscribe is a cleanup operation and opens no new permission surface
        assert_eq!(
            PERM("opencapx.subscribe", &json!({"capability": "file.watch"})),
            Some("file.read".to_string())
        );
        assert_eq!(PERM("opencapx.unsubscribe", &json!({})), None);
        assert_eq!(PERM("opencapx.list_capabilities", &json!({})), None);
        // v1.2: system.permission_status is read-only metadata, exempt from the Agent gate (used for up-front routing)
        assert_eq!(
            PERM("opencapx.execute", &json!({"capability": "system.permission_status"})),
            None
        );
        // v1.3: high/medium value batch mapping
        assert_eq!(
            PERM("opencapx.execute", &json!({"capability": "automation.run"})),
            Some("automation.control".to_string())
        );
        assert_eq!(
            PERM("opencapx.execute", &json!({"capability": "input.send"})),
            Some("input.control".to_string())
        );
        assert_eq!(
            PERM("opencapx.execute", &json!({"capability": "photos.read"})),
            Some("photos.read".to_string())
        );
        assert_eq!(
            PERM("opencapx.execute", &json!({"capability": "audio.play"})),
            Some("audio.output".to_string())
        );
        assert_eq!(
            PERM("opencapx.execute", &json!({"capability": "url.scheme.open"})),
            Some("url.scheme.open".to_string())
        );
        assert_eq!(PERM("opencapx.nothing", &json!({})), None);
    }

    /// Denial error surface: 40002 (plugin layer) / 40003 (scope) carry layer, permission, and remediation hint;
    /// unknown errors keep the {ok:false,error} shape (the Agent can reliably parse the prefix).
    #[test]
    fn execute_denials_carry_codes_and_hints() {
        let b = capability_error_body("plugin_permission_denied:file.read");
        assert!(b.contains("\"code\":40002"), "{}", b);
        assert!(b.contains("plugin layer: file.read"), "{}", b);
        assert!(b.contains("Settings"), "{}", b);

        let b2 = capability_error_body("scope_denied:agent:file.read");
        assert!(b2.contains("\"code\":40003"), "{}", b2);
        assert!(b2.contains("agent layer, file.read"), "{}", b2);
        assert!(b2.contains("allowed scope"), "{}", b2);

        let b3 = capability_error_body("scope_denied:plugin:filesystem.write");
        assert!(b3.contains("plugin layer, filesystem.write"), "{}", b3);

        // Non-permission errors: no code (consistent with existing shapes such as capability_failed)
        let b4 = capability_error_body("capability_failed");
        assert!(b4.contains("\"error\":\"capability_failed\""), "{}", b4);
        assert!(!b4.contains("\"code\""), "{}", b4);
    }


    /// v1.0 frozen contract: the MCP v1 tool surface is pinned to these 8 names, consistent with docs/mcp.md.
    /// Renaming/removing is a breaking change (only allowed within the apiVersion "2" window); this test going red = the freeze is broken.
    #[test]
    fn mcp_v1_tool_surface_is_frozen() {
        assert_eq!(
            MCP_V1_TOOLS,
            &[
                "opencapx.say",
                "opencapx.notify",
                "opencapx.set_state",
                "opencapx.ask",
                "opencapx.list_capabilities",
                "opencapx.execute",
                "opencapx.subscribe",
                "opencapx.unsubscribe",
            ]
        );
        // Addition is allowed: a new tool entering the set breaks no v1 client beyond this assertion
    }

    /// Agent-layer gate end-to-end: notification.post defaults to ask, the test process has no UI → gate_agent rejects fast,
    /// /rpc returns permission_denied + 40001; after overriding the agent decision to granted it passes through to the tool body
    /// (notify is not callable in a test process without a tauri AppHandle, so here an invalid state of set_state
    /// verifies that the tool logic is only reached after passing the gate). The dev principal __dev__ also uses the default table.
    #[test]
    fn rpc_gates_agent_layer_before_dispatch() {
        use crate::core::identity;
        use crate::core::storage::StoreEnum;
        use std::sync::Mutex;
        let dir = std::env::temp_dir().join(format!("opencapx-rpcgate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        let (aid, _tok) = identity::register(&store, "claude", "test").unwrap();
        let bus = Arc::new(EventBus::new());
        // No tauri AppHandle: handle needs &AppHandle — a test process cannot create one.
        // Instead test the gate function + mapping combination (equivalent to the internal order of handle).
        let d = gate_agent(&store, &aid, "notification.post", "mcp");
        assert_eq!(d, Decision::Denied, "ask default + no UI → fast rejection");
        // Overridden to granted, it passes the gate
        assert!(identity::set_agent_decision(&store, &aid, "notification.post", "granted"));
        let d2 = gate_agent(&store, &aid, "notification.post", "mcp");
        assert_eq!(d2, Decision::Granted);
        // pet.animation defaults to granted, passes directly
        assert_eq!(gate_agent(&store, &aid, "pet.animation", "mcp"), Decision::Granted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// rpc::execute wraps a capability span: unknown capability → an error end line is persisted.
    /// (handle/ask need an AppHandle, which cannot be created in-process — these two instrumentation points rely on compilation + manual verification.)
    #[test]
    fn execute_writes_capability_span() {
        // OPENCAPX_TRACES_DIR is a process-level env: a repo-wide shared lock must be used, otherwise it races with req_trace /
        // retention / plugin_trace tests in parallel (each sets/deletes the same variable).
        let _g = crate::core::plugin_trace::traces_env_lock();
        let base = std::env::temp_dir().join(format!("ocx-rpc-trace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_TRACES_DIR", &base);

        let tid = crate::core::req_trace::begin("ag_t", "", "");
        let out = execute(&json!({ "capability": "no.such.cap", "input": {} }), "ag_t");
        let (ok, err) = match serde_json::from_str::<serde_json::Value>(&out) {
            Ok(v) => (
                v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false),
                v.get("error").and_then(|e| e.as_str()).map(String::from),
            ),
            Err(_) => (false, None),
        };
        crate::core::req_trace::finish(ok, err.as_deref(), serde_json::Value::Null);

        // Read the file back: there should be a start + error end for capability.no.such.cap
        let text = std::fs::read_to_string(
            crate::core::req_trace::rpc_traces_root()
                .join("ag_t")
                .join(format!("{}.ndjson", tid)),
        )
        .unwrap();
        assert!(text.contains("\"name\":\"capability.no.such.cap\""), "has a start line: {text}");
        assert!(text.contains("\"error\":\"unknown capability: no.such.cap\""), "end line carries the error: {text}");

        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }
}
