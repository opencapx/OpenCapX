//! `opencapx mcp`: a stdio MCP server, a thin forwarding layer.
//! Agent hosts (Claude Code / Codex / OpenCode) spawn this process,
//! and forward tool calls to the Core's POST /rpc (127.0.0.1:47628).
//! See docs/mcp.md. The protocol is NDJSON JSON-RPC 2.0, hand-written with zero extra dependencies.
//! Auth (docs/permissions.md "Agent Identity"): TOFU-register at startup to obtain a per-agent token,
//! persisted to ~/.opencapx/agent-tokens/<kind>.token (0600); every /rpc carries a Bearer header.
//!
//! v1.1 (B10 event-subscription lifecycle):
//! - tools/call runs on a worker thread + a cancellation table: `notifications/cancelled` → immediately reply -32800 +
//!   POST /rpc/cancel (the Core truly interrupts the pending ask and the bubble closes at once); the late original result is discarded
//! - Subscription channel: the first `opencapx.subscribe` opens an SSE stream (/events, with X-OpenCapX-Conn),
//!   `capability.event` is filtered by subscriptionId and pushed to the
//!   Agent host as an `opencapx.event` notification; this process exits → SSE disconnects → the Core cleans up subscriptions and watchers (subscriptions do not survive the connection)

use crate::core::identity::{self, Credentials};
use crate::http;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Process-level credentials: registered/loaded on first tool call (lazy init, avoids racing stdin).
/// Test processes (cfg test) do not register, so the real token file is not polluted.
fn creds() -> Option<&'static Credentials> {
    static C: OnceLock<Option<Credentials>> = OnceLock::new();
    C.get_or_init(|| {
        if cfg!(test) {
            return None;
        }
        identity::ensure_registered(&identity::detect_kind(), "mcp")
    })
    .as_ref()
}

/// Unique connection id for this CLI process: reported with /rpc and SSE /events,
/// so the Core's subscription table binds to the connection lifecycle (cleaned up on disconnect).
fn conn_id() -> &'static str {
    static CONN: OnceLock<String> = OnceLock::new();
    CONN.get_or_init(|| {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("conn-{}-{:x}", std::process::id(), n)
    })
}

/// Output channel: every path that writes stdout (responses / errors / notifications) funnels here,
/// flushed by a single writer thread — concurrent writes from the tool thread, SSE thread, and cancel path do not interleave.
enum Out {
    Response {
        id: Value,
        result: Value,
    },
    Error {
        id: Value,
        code: i64,
        message: String,
    },
    /// server → client notification (no id, no response needed)
    Notification(Value),
}

/// Cancellation table: in-flight tools/call, key = stringified JSON-RPC id (same source as the client's
/// notifications/cancelled requestId). Whoever removes it from the table first does the wrap-up.
fn pending() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static P: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Subscriptions held by this process (added on subscribe success, removed on unsubscribe success; cleaned up by the Core on process exit/disconnect).
fn sub_ids() -> &'static Mutex<HashSet<String>> {
    static S: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn run() -> ! {
    // Register up front: the first thing after the host spawns us is to establish identity (persist the token),
    // so every subsequent tools/call carries credentials. Core not running → None, and post_rpc naturally reports app not running.
    let _ = creds();
    let (tx, rx) = mpsc::channel::<Out>();
    // Single writer thread: one output point, concurrency-safe for worker / SSE / cancel paths
    std::thread::spawn(move || {
        let mut writer = std::io::stdout();
        for o in rx {
            match o {
                Out::Response { id, result } => write_message(&mut writer, Some(id), result),
                Out::Error { id, code, message } => {
                    write_error(&mut writer, Some(id), code, &message)
                }
                Out::Notification(v) => {
                    let _ = write_line(&mut writer, &v);
                }
            }
        }
    });
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(Out::Error {
                    id: Value::Null,
                    code: -32700,
                    message: format!("parse error: {}", e),
                });
                continue;
            }
        };
        handle_message(&msg, &tx);
    }
    // Host closes stdin = process exit; SSE disconnects accordingly and the Core cleans up subscriptions automatically
    std::process::exit(0);
}

fn write_message<W: std::io::Write>(w: &mut W, id: Option<Value>, result: Value) {
    let mut resp = json!({ "jsonrpc": "2.0", "result": result });
    if let Some(id) = id {
        resp["id"] = id;
    }
    let _ = write_line(w, &resp);
}

fn write_error<W: std::io::Write>(w: &mut W, id: Option<Value>, code: i64, message: &str) {
    let mut resp = json!({ "jsonrpc": "2.0", "error": { "code": code, "message": message } });
    if let Some(id) = id {
        resp["id"] = id;
    }
    let _ = write_line(w, &resp);
}

fn write_line<W: std::io::Write>(w: &mut W, v: &Value) -> std::io::Result<()> {
    let mut s = serde_json::to_string(v).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    w.write_all(s.as_bytes())?;
    w.flush()
}

fn handle_message(msg: &Value, out: &Sender<Out>) {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    // Notifications (no id) are never answered; the cancel notification is the only one to handle
    let Some(id) = id else {
        if method == "notifications/cancelled" {
            handle_cancel(msg.get("params").cloned().unwrap_or(Value::Null), out);
        }
        return;
    };

    match method {
        "initialize" => {
            let _ = out.send(Out::Response {
                id,
                result: json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "opencapx", "version": env!("CARGO_PKG_VERSION") }
                }),
            });
        }
        "ping" => {
            let _ = out.send(Out::Response {
                id,
                result: json!({}),
            });
        }
        "tools/list" => {
            let _ = out.send(Out::Response {
                id,
                result: json!({ "tools": tools() }),
            });
        }
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            if let Some(schema) = tool_schema(&name) {
                if let Err(reason) = validate(arguments.clone(), schema) {
                    let _ = out.send(Out::Error {
                        id,
                        code: -32602,
                        message: format!("invalid arguments: {}", reason),
                    });
                    return;
                }
            }
            // The subscription channel is established before /rpc: no event-loss window between subscription setup and the SSE connection
            if name == "opencapx.subscribe" {
                start_sse(out.clone());
            }
            spawn_call(id, name, arguments, out);
        }
        other => {
            let _ = out.send(Out::Error {
                id,
                code: -32601,
                message: format!("method not found: {}", other),
            });
        }
    }
}

/// A worker thread executes tools/call: blocking calls do not hold up stdin (ask up to 900s).
/// requestId = stringified id, so the Core-side ask can be interrupted by /rpc/cancel.
fn spawn_call(id: Value, name: String, arguments: Value, out: &Sender<Out>) {
    let key = id.to_string();
    let flag = Arc::new(AtomicBool::new(false));
    if let Ok(mut m) = pending().lock() {
        m.insert(key.clone(), flag.clone());
    }
    let out = out.clone();
    std::thread::spawn(move || {
        let result = call_tool(&name, &arguments, Some(&key));
        // Whoever removes it from the cancellation table first does the wrap-up: if cancelled → discard the late result
        let cancelled = if let Ok(mut m) = pending().lock() {
            m.remove(&key);
            flag.load(Ordering::SeqCst)
        } else {
            true
        };
        if !cancelled {
            let _ = out.send(Out::Response { id, result });
        }
    });
}

/// notifications/cancelled: immediately reply -32800 (mcp.md "Cancellation"), and best-effort notify the Core
/// to interrupt the pending ask (bubble closes at once); plugin-process calls cannot be interrupted, so the result is simply discarded.
fn handle_cancel(params: Value, out: &Sender<Out>) {
    let Some(rid) = params.get("requestId").cloned() else {
        return;
    };
    let key = rid.to_string();
    let flag = match pending().lock() {
        Ok(mut m) => m.remove(&key),
        Err(_) => return,
    };
    if let Some(flag) = flag {
        flag.store(true, Ordering::SeqCst);
        let _ = out.send(Out::Error {
            id: rid,
            code: -32800,
            message: "Request cancelled".into(),
        });
        std::thread::spawn(move || {
            http::post_rpc_cancel(&key, creds());
        });
    }
    // Not in the table = already wrapped up normally; cancel has nothing to do
}

/// Subscription channel (lazy start, once): SSE /events → filter this process's subscriptions → opencapx.event.
/// Reconnects every 5s on disconnect (survives Core not running / restarting).
fn start_sse(out: Sender<Out>) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return; // already started
    }
    std::thread::spawn(move || loop {
        if let Some(mut reader) = http::open_event_stream(creds(), conn_id()) {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break, // Core closed the stream
                    Ok(_) => {
                        if let Some(ev) = parse_sse_data(&line) {
                            forward_subscription_event(&ev, &out);
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    });
}

/// SSE data line → OpencapxEvent (JSON). `: ping` comments and `event:` lines are naturally filtered out.
fn parse_sse_data(line: &str) -> Option<Value> {
    let rest = line.strip_prefix("data: ")?;
    serde_json::from_str(rest.trim()).ok()
}

/// capability.event that belongs to this process's subscriptions → opencapx.event notification params
/// (shape per mcp.md "Subscription Tool Pair": {subscriptionId, capability, payload}).
fn forward_subscription_event(ev: &Value, out: &Sender<Out>) {
    if ev.get("type").and_then(|t| t.as_str()) != Some("capability.event") {
        return;
    }
    let Some(p) = ev.get("payload") else { return };
    let Some(sid) = p.get("subscriptionId").and_then(|s| s.as_str()) else {
        return;
    };
    let mine = sub_ids().lock().map(|m| m.contains(sid)).unwrap_or(false);
    if !mine {
        return;
    }
    let _ = out.send(Out::Notification(json!({
        "jsonrpc": "2.0",
        "method": "opencapx.event",
        "params": {
            "subscriptionId": sid,
            "capability": p.get("capability").cloned().unwrap_or(Value::Null),
            "payload": {
                "event": p.get("event").cloned().unwrap_or(Value::Null),
                "path": p.get("path").cloned().unwrap_or(Value::Null),
            },
        },
    })));
}

/// Successful subscribe reply → record in this process's subscription set (used for SSE filtering).
fn track_subscribe(body: &str) {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return;
    };
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return;
    }
    if let Some(sid) = v.get("subscriptionId").and_then(|s| s.as_str()) {
        if let Ok(mut m) = sub_ids().lock() {
            m.insert(sid.to_string());
        }
    }
}

/// Successful unsubscribe reply → remove from the subscription set.
fn track_unsubscribe(body: &str) {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return;
    };
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return;
    }
    if let Some(sid) = v.get("subscriptionId").and_then(|s| s.as_str()) {
        if let Ok(mut m) = sub_ids().lock() {
            m.remove(sid);
        }
    }
}

fn tools() -> Vec<Value> {
    vec![
        tool(
            "opencapx.say",
            "Make the desktop pet say something in a speech bubble.",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "bubble text" },
                    "duration": { "type": "number", "description": "seconds, default from settings" }
                },
                "required": ["text"]
            }),
        ),
        tool(
            "opencapx.notify",
            "Send an OS notification.",
            json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "body": { "type": "string" }
                },
                "required": ["title", "body"]
            }),
        ),
        tool(
            "opencapx.set_state",
            "Set the pet logical state.",
            json!({
                "type": "object",
                "properties": {
                    "state": { "type": "string", "enum": ["idle","thinking","working","waiting","permission","success","error","sleeping"] },
                    "message": { "type": "string" }
                },
                "required": ["state"]
            }),
        ),
        tool(
            "opencapx.ask",
            "Ask the user a question via pet bubble buttons. BLOCKS until the user clicks or timeout (default 300s).",
            json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "timeout": { "type": "number", "description": "seconds, default 300, max 900" }
                },
                "required": ["question", "options"]
            }),
        ),
        tool(
            "opencapx.list_capabilities",
            "List capabilities provided by installed plugins.",
            json!({ "type": "object", "properties": {}, "required": [] }),
        ),
        tool(
            "opencapx.execute",
            "Execute a capability (e.g. image.analyze) through the capability router.",
            json!({
                "type": "object",
                "properties": {
                    "capability": { "type": "string" },
                    "input": { "type": "object" }
                },
                "required": ["capability", "input"]
            }),
        ),
        tool(
            "opencapx.subscribe",
            "Subscribe to an event-stream capability (e.g. file.watch). Events arrive as `opencapx.event` notifications until you unsubscribe or disconnect.",
            json!({
                "type": "object",
                "properties": {
                    "capability": { "type": "string", "description": "must be a subscribe-type capability" },
                    "input": { "type": "object", "description": "e.g. { \"path\": \"~/project\", \"recursive\": true }" }
                },
                "required": ["capability"]
            }),
        ),
        tool(
            "opencapx.unsubscribe",
            "End a subscription. Idempotent: unknown subscriptionId returns ok.",
            json!({
                "type": "object",
                "properties": {
                    "subscriptionId": { "type": "string" }
                },
                "required": ["subscriptionId"]
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

fn call_tool(name: &str, arguments: &Value, request_id: Option<&str>) -> Value {
    if !name.starts_with("opencapx.") {
        return text_result(&format!("unknown tool: {}", name), true);
    }
    match http::post_rpc(name, arguments, creds(), request_id) {
        Some(body) => {
            // Subscription bookkeeping: update this process's subscription set on subscribe/unsubscribe success
            if name == "opencapx.subscribe" {
                track_subscribe(&body);
            } else if name == "opencapx.unsubscribe" {
                track_unsubscribe(&body);
            }
            let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
            let ok = v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
            text_result(&body, !ok)
        }
        None => text_result(
            "OpenCapX app is not running (start the app, then retry)",
            true,
        ),
    }
}

fn tool_schema(name: &str) -> Option<&'static Value> {
    for t in tools() {
        if t["name"].as_str() == Some(name) {
            // 8 schemas over a single process lifetime, leak < 1KB, acceptable
            return Some(Box::leak(Box::new(t["inputSchema"].clone())));
        }
    }
    None
}

/// Minimal JSON-Schema subset validation: only type + required + enum + array minItems.
/// No jsonschema dependency; enough to cover the 8 tool schemas in docs/mcp.md.
fn validate(value: Value, schema: &Value) -> Result<(), String> {
    let ty = schema.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if !ty.is_empty() {
        let ok = match ty {
            "object" => value.is_object(),
            "string" => value.is_string(),
            "number" | "integer" => value.is_number(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            _ => true,
        };
        if !ok {
            return Err(format!("expected type {}", ty));
        }
    }
    if let Some(req) = schema.get("required").and_then(|r| r.as_array()) {
        if let Some(obj) = value.as_object() {
            for k in req {
                let key = k.as_str().unwrap_or("");
                if !obj.contains_key(key) {
                    return Err(format!("missing required field {}", key));
                }
            }
        }
    }
    if let Some(enum_) = schema.get("enum").and_then(|e| e.as_array()) {
        let allowed: Vec<&Value> = enum_.iter().collect();
        if !allowed.iter().any(|v| *v == &value) {
            return Err(format!("value not in enum {:?}", allowed));
        }
    }
    if ty == "array" {
        if let Some(min) = schema.get("minItems").and_then(|m| m.as_u64()) {
            if value.as_array().map(|a| a.len() as u64).unwrap_or(0) < min {
                return Err(format!("array too short (min {})", min));
            }
        }
    }
    Ok(())
}

fn text_result(text: &str, is_error: bool) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(msg: Value) -> Value {
        let (tx, rx) = mpsc::channel();
        handle_message(&msg, &tx);
        to_wire(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("response arrives"),
        )
    }

    /// Out → wire JSON (equivalent to the writer thread's write_message/write_error output).
    fn to_wire(o: Out) -> Value {
        match o {
            Out::Response { id, result } => {
                let mut v = json!({ "jsonrpc": "2.0", "result": result });
                v["id"] = id;
                v
            }
            Out::Error { id, code, message } => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
            Out::Notification(v) => v,
        }
    }

    #[test]
    fn tools_cover_the_v1_set() {
        let names: Vec<String> = tools()
            .into_iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for expected in [
            "opencapx.say",
            "opencapx.notify",
            "opencapx.set_state",
            "opencapx.ask",
            "opencapx.list_capabilities",
            "opencapx.execute",
            "opencapx.subscribe",
            "opencapx.unsubscribe",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "missing {}",
                expected
            );
        }
    }

    /// set_state's enum matches Core PET_STATES (Agent State Protocol 8 states).
    #[test]
    fn set_state_enum_matches_protocol() {
        let schema = tool_schema("opencapx.set_state").expect("schema exists");
        let en = schema["properties"]["state"]["enum"].as_array().unwrap();
        assert_eq!(en.len(), 8, "{:?}", en);
        assert!(
            en.contains(&json!("permission")),
            "A8:permission must be in the enum"
        );
        assert!(en.contains(&json!("thinking")));
        assert!(en.contains(&json!("sleeping")));
    }

    #[test]
    fn unknown_tool_is_error_result() {
        let r = call_tool("nope.tool", &json!({}), None);
        assert_eq!(r["isError"], json!(true));
    }

    #[test]
    fn validate_covers_required_type_enum_minitems() {
        let s = json!({"type":"object","properties":{"q":{"type":"string"}},"required":["q"]});
        assert!(validate(json!({"q":"x"}), &s).is_ok());
        assert!(validate(json!({}), &s)
            .unwrap_err()
            .contains("missing required field q"));
        let s = json!({"enum":["a","b"]});
        assert!(validate(json!("c"), &s).unwrap_err().contains("enum"));
        assert!(validate(json!("a"), &s).is_ok());
        let s = json!({"type":"array","minItems":2,"items":{"type":"string"}});
        assert!(validate(json!(["a"]), &s)
            .unwrap_err()
            .contains("too short"));
        let s = json!({"type":"number"});
        assert!(validate(json!("x"), &s)
            .unwrap_err()
            .contains("expected type"));
    }

    #[test]
    fn tools_call_rejects_missing_required() {
        // tools/call without the required text → validate rejects on the synchronous path (never reaches the worker)
        let v = call(json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"opencapx.say","arguments":{}}
        }));
        assert_eq!(v["error"]["code"], json!(-32602));
        assert!(v["error"]["message"].as_str().unwrap().contains("text"));
    }

    #[test]
    fn tools_call_passes_validation() {
        // list_capabilities (no required fields) makes it to the worker: app not running → isError result, but not a JSON-RPC error
        let v = call(json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"opencapx.list_capabilities","arguments":{}}
        }));
        assert!(v.get("error").is_none(), "got error: {:?}", v);
        assert!(
            v["result"]["isError"].as_bool().unwrap(),
            "app not running should be isError"
        );
    }

    /// notifications/cancelled: unknown requestId → silent (the client already got a normal response);
    /// pending requestId → -32800 + table entry removed + flag set (worker discards the late result).
    #[test]
    fn cancel_notification_closes_pending_and_ignores_unknown() {
        let (tx, rx) = mpsc::channel();
        // Unknown id: no response
        handle_message(
            &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":99}}),
            &tx,
        );
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "unknown id gets no response"
        );
        // Pending: id=42 registered in the table (simulating insertion before the worker starts)
        let flag = Arc::new(AtomicBool::new(false));
        pending().lock().unwrap().insert("42".into(), flag.clone());
        handle_message(
            &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":42}}),
            &tx,
        );
        let v = to_wire(rx.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(v["error"]["code"], json!(-32800));
        assert_eq!(v["id"], json!(42));
        assert!(
            flag.load(Ordering::SeqCst),
            "worker discards the result based on this"
        );
        assert!(
            pending().lock().unwrap().get("42").is_none(),
            "table entry removed"
        );
        // Duplicate cancel: no longer in the table → silent
        handle_message(
            &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":42}}),
            &tx,
        );
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "duplicate cancel gets no response"
        );
    }

    /// Worker wrap-up race: cancel arrives first (flag already set) → late result is discarded;
    /// normal wrap-up (entry removed by itself) → cancel has nothing to do.
    #[test]
    fn worker_drops_result_when_cancelled() {
        let flag = Arc::new(AtomicBool::new(false));
        pending().lock().unwrap().insert("7".into(), flag.clone());
        // Cancel semantics: set flag + remove from table (same as the cancel path)
        let removed = pending().lock().unwrap().remove("7");
        assert!(removed.is_some());
        flag.store(true, Ordering::SeqCst);
        // Worker's view: remove returns None (already taken by cancel) + flag true → discard
        let cancelled =
            pending().lock().unwrap().remove("7").is_none() || flag.load(Ordering::SeqCst);
        assert!(cancelled);
        // Normal path: worker itself removes Some + flag false → send response
        let flag2 = Arc::new(AtomicBool::new(false));
        pending().lock().unwrap().insert("8".into(), flag2.clone());
        let entry = pending().lock().unwrap().remove("8");
        assert!(entry.is_some());
        assert!(!flag2.load(Ordering::SeqCst));
    }

    /// SSE data line parsing + subscription event forwarding shape (mcp.md "Subscription Tool Pair").
    #[test]
    fn sse_data_parses_and_forwards_subscribed_events() {
        assert!(parse_sse_data(": ping").is_none());
        assert!(parse_sse_data("event: capability.event").is_none());
        assert!(parse_sse_data("data: not json").is_none());
        let ev = parse_sse_data(
            r#"data: {"id":"evt-1","type":"capability.event","source":"core","timestamp":1,"payload":{"subscriptionId":"sub_1","capability":"file.watch","agentId":"ag_1","event":"created","path":"/tmp/a.txt"}}"#,
        )
        .unwrap();
        let (tx, rx) = mpsc::channel();
        // Not holding the subscription → do not forward
        forward_subscription_event(&ev, &tx);
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "someone else's subscription is not forwarded"
        );
        // Holding it → forward as an opencapx.event notification, params shape aligned with mcp.md
        sub_ids().lock().unwrap().insert("sub_1".into());
        forward_subscription_event(&ev, &tx);
        let n = to_wire(rx.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(n["jsonrpc"], json!("2.0"));
        assert_eq!(n["method"], json!("opencapx.event"));
        assert_eq!(n["params"]["subscriptionId"], json!("sub_1"));
        assert_eq!(n["params"]["capability"], json!("file.watch"));
        assert_eq!(n["params"]["payload"]["event"], json!("created"));
        assert_eq!(n["params"]["payload"]["path"], json!("/tmp/a.txt"));
        assert!(n.get("id").is_none(), "notification has no id");
        // Non capability.event is not forwarded
        let other = parse_sse_data(
            r#"data: {"id":"evt-2","type":"pet.state","source":"core","timestamp":1,"payload":{"state":"idle"}}"#,
        )
        .unwrap();
        forward_subscription_event(&other, &tx);
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
        sub_ids().lock().unwrap().remove("sub_1");
    }

    /// Subscription bookkeeping: a successful subscribe reply enters the set, a failure does not; a successful unsubscribe removes it.
    #[test]
    fn track_subscribe_and_unsubscribe_bookkeeping() {
        track_subscribe(r#"{"ok":true,"subscriptionId":"sub_a"}"#);
        track_subscribe(r#"{"ok":false,"error":"capability is not subscribable"}"#);
        assert!(sub_ids().lock().unwrap().contains("sub_a"));
        track_unsubscribe(r#"{"ok":true,"subscriptionId":"sub_a"}"#); // Core echoes subscriptionId
        assert!(!sub_ids().lock().unwrap().contains("sub_a"));
        track_unsubscribe(r#"{"ok":false,"error":"nope"}"#); // failure does not panic
        track_subscribe("not json"); // does not panic
    }
}
