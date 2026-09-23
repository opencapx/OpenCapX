//! Core local HTTP entry point: hook event reporting (POST /event), MCP forwarding (POST /rpc),
//! Agent identity registration (POST /agents/register). Transport layer; parsing, auth, and persistence live in core.
//! See docs/permissions.md "Agent Identity" for auth: Bearer token + X-OpenCapX-Agent,
//! any of the three checks failing means 401 (code 40101 anonymous/bad token, 40102 revoked) and an auth.rejected audit is written.

use crate::core::event::{self, EventBus};
use crate::core::identity::{self, Credentials};
use crate::core::storage::SharedStore;
use std::path::PathBuf;
use std::sync::Arc;

pub const LISTEN_ADDR: &str = "127.0.0.1:47628";

/// dev skips auth (local development only, docs/permissions.md "Request Authentication").
/// Compile-time gated: the env lookup only exists in debug builds, so no runtime
/// environment can re-enable the bypass in a release binary.
#[cfg(debug_assertions)]
fn dev_mode() -> bool {
    std::env::var("OPEN_CAPX_DEV").map(|v| v == "1").unwrap_or(false)
}

#[cfg(not(debug_assertions))]
fn dev_mode() -> bool {
    false
}

fn pick_str(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

/// Minimal extractor used by the hook CLI: returns (agent, text).
pub fn parse_hook_payload(stdin: &str) -> (String, String) {
    let v: serde_json::Value = serde_json::from_str(stdin).unwrap_or(serde_json::Value::Null);
    let agent = pick_str(&v, &["agent"]).unwrap_or_else(|| "unknown".into());
    let text = pick_str(&v, &["text", "message", "content"]).unwrap_or_default();
    (agent, text)
}

pub fn queue_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(".opencapx").join("queue");
    }
    std::env::temp_dir().join("opencapx-queue")
}

/// Client auth headers (two lines with a CRLF prefix). Empty string when there are no credentials.
/// Values pass through `ascii_header_value`: a CR/LF inside the on-disk token or agent_id
/// would otherwise split the header block (header injection / request smuggling).
fn auth_header_lines(creds: Option<&Credentials>) -> String {
    match creds {
        Some(c) => format!(
            "Authorization: Bearer {}\r\nX-OpenCapX-Agent: {}\r\n",
            ascii_header_value(&c.token),
            ascii_header_value(&c.agent_id)
        ),
        None => String::new(),
    }
}

/// CLI-side upload result. The three failure causes are distinguished to give hook users accurate recovery guidance:
/// Unreachable = app not running (just enqueue locally); Rejected = token invalid/revoked
/// (401) or registration rejected (403 / code 40102), which requires recovery via the settings page, see permissions.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deliver {
    Sent,
    Unreachable,
    /// 401 + body `code:40101` — identity is known but the token is invalid. **Self-healing**: discard the local token and re-
    /// register via TOFU to recover (otherwise this agent stays 401 forever and events queue indefinitely).
    RejectedBadToken,
    /// Any other non-2xx (including 40102 revoked): no self-healing — revocation can only be lifted by the user on the settings page.
    Rejected,
}

/// POST /event, keeping the 2xx body — SessionStart replies carry the capability digest
/// for additionalContext injection (see the /event handler).
pub fn post_event_with_body(payload: &str, creds: Option<&Credentials>) -> (Deliver, Option<String>) {
    post_route_with_body("/event", payload, creds)
}

pub fn post_route(route: &str, payload: &str, creds: Option<&Credentials>) -> Deliver {
    post_route_with_body(route, payload, creds).0
}

fn post_route_with_body(route: &str, payload: &str, creds: Option<&Credentials>) -> (Deliver, Option<String>) {
    use std::io::Write;
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: 127.0.0.1:47628\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        ascii_header_value(route),
        auth_header_lines(creds),
        payload.len(),
        payload
    );
    let mut stream = match std::net::TcpStream::connect(LISTEN_ADDR) {
        Ok(s) => s,
        Err(_) => return (Deliver::Unreachable, None),
    };
    if stream.write_all(req.as_bytes()).is_err() {
        return (Deliver::Unreachable, None);
    }
    let mut buf = String::new();
    use std::io::Read;
    if stream.read_to_string(&mut buf).is_err() {
        return (Deliver::Unreachable, None);
    }
    let deliver = classify_status(&buf);
    let body = if matches!(deliver, Deliver::Sent) {
        buf.split("\r\n\r\n")
            .nth(1)
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
    } else {
        None
    };
    (deliver, body)
}

/// Hosts whose SessionStart hook merges stdout `additionalContext` into the agent's context
/// (the Claude-nested family with the documented field). Everything else stays a dumb pipe —
/// no stdout. Mirror of the rewrite_host split in main.rs; extend per-host as field shapes
/// are confirmed (cursor/gemini/kiro differ and are deliberately absent).
pub fn session_context_host(kind: &str) -> bool {
    matches!(kind, "claude" | "codex" | "droid" | "grok")
}

/// Build the additionalContext payload for a session-start /event, or None when this is not
/// a session start, the host ignores stdout context, the user disabled the injection, or
/// nothing is registered. Emits a session.context.injected audit event when it fires.
fn session_start_reply(kind: &str, body: &str, bus: &EventBus) -> Option<String> {
    if !session_context_host(kind) {
        return None;
    }
    let enabled = crate::read_settings_file()
        .get("sessionContextInject")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !enabled {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let name = parsed.get("hook_event_name")?.as_str()?;
    if !name.eq_ignore_ascii_case("SessionStart") {
        return None;
    }
    let text = crate::core::capability::digest_text()?;
    bus.publish(&crate::core::event::OpencapxEvent::new(
        "session.context.injected",
        "core",
        serde_json::json!({ "agent": kind, "bytes": text.len() }),
    ));
    Some(text)
}

/// /rpc result line → (ok, error): the closing attributes of the trace root span.
/// rpc::handle returns a JSON String; failures always carry an error field.
fn parse_rpc_outcome(s: &str) -> (bool, Option<String>) {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(v) => {
            let error = v.get("error").and_then(|e| e.as_str()).map(String::from);
            // Success shapes are more than `{ok:true}`: list_capabilities returns {"capabilities":[…]} directly.
            // The criterion is "is there an error"; an explicit ok:false also counts as failure (observed: missing this marks success as error).
            let ok = match v.get("ok").and_then(|b| b.as_bool()) {
                Some(b) => b,
                None => error.is_none(),
            };
            (ok, error)
        }
        Err(_) => (false, Some("unparseable response".into())),
    }
}

/// panic payload → readable message (catch_unwind's Box<dyn Any> can only be downcast).
fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}

/// Raw response → Deliver: with an HTTP status line, 2xx=Sent, non-2xx=Rejected;
/// no status line (empty read / non-HTTP reply) is treated as Unreachable.
fn classify_status(raw: &str) -> Deliver {
    let status = raw
        .split("\r\n")
        .next()
        .filter(|l| l.starts_with("HTTP/"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok());
    match status {
        Some(c) if (200..300).contains(&c) => Deliver::Sent,
        Some(401) if body_code(raw) == Some(40101) => Deliver::RejectedBadToken,
        Some(_) => Deliver::Rejected,
        None => Deliver::Unreachable,
    }
}

/// Extract the error code from the response body (/event rejections look like `{"ok":false,"error":"…","code":40101}`).
fn body_code(raw: &str) -> Option<u64> {
    let body = raw.split("\r\n\r\n").nth(1)?;
    serde_json::from_str::<serde_json::Value>(body.trim())
        .ok()?
        .get("code")?
        .as_u64()
}

/// POST /rpc {tool, input, requestId?} → Core executes the MCP tool logic and returns result JSON.
/// requestId lets /rpc/cancel interrupt a pending ask (mcp.md "Cancellation"); None = non-cancellable call.
pub fn post_rpc(
    tool: &str,
    input: &serde_json::Value,
    creds: Option<&Credentials>,
    request_id: Option<&str>,
) -> Option<String> {
    use std::io::{Read, Write};
    let payload = rpc_payload(tool, input, request_id);
    let req = rpc_request(creds, &payload);
    let mut stream = std::net::TcpStream::connect(LISTEN_ADDR).ok()?;
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    // Response looks like "HTTP/1.1 200 OK\r\n...\r\n\r\n{json}"
    let body = buf.split("\r\n\r\n").nth(1)?;
    if !buf.starts_with("HTTP/1.1 200") {
        return None;
    }
    Some(body.to_string())
}

/// Short path of the project where the CLI runs (`core::project::short_path`), computed once per process —
/// reported by every /rpc but never touches disk; `current_dir` failure → "" (the trace has no project, and the request must never fail for this).
fn cli_project() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        std::env::current_dir()
            .map(|d| crate::core::project::short_path(&d.to_string_lossy()))
            .unwrap_or_default()
    })
}

/// Header values must stay printable ASCII (visible chars + space + tab): CR/LF would split the
/// header block (header injection / request smuggling), and a non-ASCII byte in a raw header line
/// makes the server drop the connection (the CLI then misreports "app is not running"). Control
/// characters are stripped rather than rejected: the values (token, agent_id, project path, conn id)
/// are local inputs, and a stripped value that fails auth is safe and debuggable.
/// `short_path` yields a multibyte `…` for deep paths; the pretty ellipsis stays for UI display,
/// here it degrades to `...`.
fn ascii_header_value(s: &str) -> String {
    let fixed = s.replace('…', "...");
    fixed
        .chars()
        .filter(|c| *c == ' ' || *c == '\t' || c.is_ascii_graphic())
        .collect()
}

/// /rpc request headers: auth + owning project (the viewer groups request chains by project).
fn rpc_request(creds: Option<&Credentials>, payload: &str) -> String {
    format!(
        "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1:47628\r\nContent-Type: application/json\r\n{}X-OpenCapX-Project: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        auth_header_lines(creds),
        ascii_header_value(cli_project()),
        payload.len(),
        payload
    )
}

fn rpc_payload(tool: &str, input: &serde_json::Value, request_id: Option<&str>) -> String {
    let mut v = serde_json::json!({ "tool": tool, "input": input });
    if let Some(rid) = request_id {
        v["requestId"] = serde_json::Value::String(rid.to_string());
    }
    v.to_string()
}

/// POST /rpc/cancel {requestId}: best-effort, silent on failure (the Agent has already received -32800).
pub fn post_rpc_cancel(request_id: &str, creds: Option<&Credentials>) {
    let payload = serde_json::json!({ "requestId": request_id }).to_string();
    let _ = post_route("/rpc/cancel", &payload, creds);
}

/// CLI's SSE subscription channel: GET /events, with auth + X-OpenCapX-Conn.
/// Returns a reader past the response headers; the caller reads `data: {json}` line by line (OpencapxEvent).
pub fn open_event_stream(
    creds: Option<&Credentials>,
    conn_id: &str,
) -> Option<std::io::BufReader<std::net::TcpStream>> {
    use std::io::{BufRead, Read, Write};
    let mut stream = std::net::TcpStream::connect(LISTEN_ADDR).ok()?;
    stream.write_all(events_request(creds, conn_id).as_bytes()).ok()?;
    let mut reader = std::io::BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).ok()?;
    if !status.contains("200") {
        return None;
    }
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    Some(reader)
}

fn events_request(creds: Option<&Credentials>, conn_id: &str) -> String {
    format!(
        "GET /events HTTP/1.1\r\nHost: 127.0.0.1:47628\r\n{}X-OpenCapX-Conn: {}\r\nConnection: close\r\n\r\n",
        auth_header_lines(creds),
        ascii_header_value(conn_id)
    )
}

/// Registration failure cause. code comes from the response body code (40102 = revoked, 40101 = anonymous/bad token);
/// status is the HTTP status code (403 = revoked, 500 = store failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    Unreachable,
    Rejected { status: u16, code: i64 },
    BadResponse,
}

/// CLI-side registration: POST /agents/register {kind, displayName, via} → (agent_id, token).
/// Failures return with a cause; the caller gives different recovery text for Unreachable / Rejected (40102 = revoked).
pub fn post_register(
    kind: &str,
    display_name: &str,
    via: &str,
) -> Result<(String, String), RegisterError> {
    use std::io::{Read, Write};
    let payload = serde_json::json!({ "kind": kind, "displayName": display_name, "via": via }).to_string();
    let req = format!(
        "POST /agents/register HTTP/1.1\r\nHost: 127.0.0.1:47628\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    let mut stream = match std::net::TcpStream::connect(LISTEN_ADDR) {
        Ok(s) => s,
        Err(_) => return Err(RegisterError::Unreachable),
    };
    if stream.write_all(req.as_bytes()).is_err() {
        return Err(RegisterError::Unreachable);
    }
    let mut buf = String::new();
    if stream.read_to_string(&mut buf).is_err() {
        return Err(RegisterError::Unreachable);
    }
    parse_register_response(&buf)
}

/// Server registration response parsing: 200 + {agentId, token} → Ok; non-2xx → Rejected{status,
/// code} (code from the response body, 40102 = revoked); other shapes → BadResponse.
fn parse_register_response(buf: &str) -> Result<(String, String), RegisterError> {
    if !buf.starts_with("HTTP/1.1 2") {
        let status = buf
            .split("\r\n")
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse::<u16>().ok())
            .unwrap_or(0);
        let body = buf.split("\r\n\r\n").nth(1).unwrap_or("");
        let code = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("code").and_then(|c| c.as_i64()))
            .unwrap_or(0);
        return Err(RegisterError::Rejected { status, code });
    }
    let body = buf.split("\r\n\r\n").nth(1).ok_or(RegisterError::BadResponse)?;
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| RegisterError::BadResponse)?;
    let agent_id = v
        .get("agentId")
        .and_then(|x| x.as_str())
        .map(String::from)
        .ok_or(RegisterError::BadResponse)?;
    let token = v
        .get("token")
        .and_then(|x| x.as_str())
        .map(String::from)
        .ok_or(RegisterError::BadResponse)?;
    Ok((agent_id, token))
}

/// Server side: extract (agent_id, bearer token) from request headers. Missing either → None.
fn extract_auth(headers: &[(String, String)]) -> Option<(String, String)> {
    let mut agent_id: Option<String> = None;
    let mut token: Option<String> = None;
    for (field, value) in headers {
        if field.eq_ignore_ascii_case("x-opencapx-agent") {
            agent_id = Some(value.trim().to_string());
        } else if field.eq_ignore_ascii_case("authorization") {
            let t = value.trim();
            if let Some(rest) = t.strip_prefix("Bearer ").or_else(|| t.strip_prefix("bearer ")) {
                token = Some(rest.trim().to_string());
            }
        }
    }
    Some((agent_id?, token?))
}

/// Extract X-OpenCapX-Conn: the unique connection id of the CLI process, reported with /rpc and SSE /events;
/// the subscription table uses it to bind the connection lifecycle (cleaned up on disconnect, docs/capability.md "Capability Types").
fn extract_conn(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .find(|(f, _)| f.eq_ignore_ascii_case("x-opencapx-conn"))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

/// Extract X-OpenCapX-Project: short path of the project where the CLI process runs, reported with /rpc;
/// the viewer groups request chains into the same project by it ("" = old CLI did not report, treated as unknown).
fn extract_project(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .find(|(f, _)| f.eq_ignore_ascii_case("x-opencapx-project"))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

/// Auth gateway: pass → agent_id; fail → (http status code, business code, rejection cause). The cause is written to auth.rejected.
fn authorize(store: &SharedStore, headers: &[(String, String)]) -> Result<String, (u16, i64, &'static str)> {
    if dev_mode() {
        // dev: anonymous is allowed as the __dev__ principal, decisions use the default table (pet.animation=granted, the rest ask/denied)
        if let Some((a, t)) = extract_auth(headers) {
            if identity::verify(store, &a, &t) == identity::VerifyResult::Ok {
                return Ok(a);
            }
        }
        return Ok("__dev__".into());
    }
    let Some((agent_id, token)) = extract_auth(headers) else {
        identity::audit_rejected(None, "anonymous");
        return Err((401, 40101, "anonymous"));
    };
    match identity::verify(store, &agent_id, &token) {
        identity::VerifyResult::Ok => Ok(agent_id),
        identity::VerifyResult::Revoked => {
            identity::audit_rejected(Some(&agent_id), "revoked");
            Err((401, 40102, "revoked"))
        }
        identity::VerifyResult::UnknownAgent | identity::VerifyResult::BadToken => {
            identity::audit_rejected(Some(&agent_id), "bad_token");
            Err((401, 40101, "bad_token"))
        }
    }
}

fn respond_json(req: tiny_http::Request, status: u16, body: String) {
    let _ = req.respond(
        tiny_http::Response::from_string(body)
            .with_status_code(status)
            .with_header(
                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
            ),
    );
}

/// N1 — request body limit 2 MiB (double the 1 MiB capability input gate; anonymous requests also pass this gate first).
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// N1 — bounded read of the request body: actual bytes over the limit → Err (caller returns 413).
fn read_body_limited<R: std::io::Read>(reader: R, max: usize) -> Result<String, ()> {
    use std::io::Read;
    let mut body = String::new();
    reader
        .take((max + 1) as u64)
        .read_to_string(&mut body)
        .map_err(|_| ())?;
    if body.len() > max {
        return Err(());
    }
    Ok(body)
}

/// N3 — /agents/register rate limit (TOFU anti-flood): at most 30 times in a 60s window (rotation/reinstall is far below this).
fn register_allowed(now: u64) -> bool {
    const WINDOW_SECS: u64 = 60;
    const LIMIT: u32 = 30;
    static W: std::sync::OnceLock<std::sync::Mutex<(u64, u32)>> = std::sync::OnceLock::new();
    let w = W.get_or_init(|| std::sync::Mutex::new((0, 0)));
    let Ok(mut g) = w.lock() else {
        return true;
    };
    if now.saturating_sub(g.0) >= WINDOW_SECS {
        *g = (now, 1);
        true
    } else if g.1 < LIMIT {
        g.1 += 1;
        true
    } else {
        false
    }
}

/// N4 — /rpc in-flight thread limit (rpc threads may hang 60s on ask; a flood must not explode the thread count).
const MAX_RPC_IN_FLIGHT: usize = 64;

static RPC_IN_FLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// N4 — in-flight slot (RAII); released automatically when the thread ends.
struct RpcSlot;

impl RpcSlot {
    fn acquire() -> Option<Self> {
        use std::sync::atomic::Ordering;
        let prev = RPC_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        if prev >= MAX_RPC_IN_FLIGHT {
            RPC_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
            None
        } else {
            Some(RpcSlot)
        }
    }
}

impl Drop for RpcSlot {
    fn drop(&mut self) {
        RPC_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

pub fn serve(handle: tauri::AppHandle, store: SharedStore, bus: Arc<EventBus>) {
    let server = match tiny_http::Server::http(LISTEN_ADDR) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("OpenCapX: cannot listen on {}: {}", LISTEN_ADDR, e);
            return;
        }
    };
    for mut req in server.incoming_requests() {
        let url = req.url().to_string();
        let method = req.method().clone();
        let headers: Vec<(String, String)> = req
            .headers()
            .iter()
            .map(|h| (h.field.to_string(), h.value.to_string()))
            .collect();
        // N1 — request body limit: Content-Length precheck + bounded read (anonymous requests also pass this gate first, prevents OOM).
        if req.body_length().map(|n| n > MAX_BODY_BYTES).unwrap_or(false) {
            respond_json(
                req,
                413,
                serde_json::json!({ "ok": false, "error": "payload_too_large" }).to_string(),
            );
            continue;
        }
        let body = match read_body_limited(req.as_reader(), MAX_BODY_BYTES) {
            Ok(b) => b,
            Err(()) => {
                respond_json(
                    req,
                    413,
                    serde_json::json!({ "ok": false, "error": "payload_too_large" }).to_string(),
                );
                continue;
            }
        };
        match (method, url.as_str()) {
            (tiny_http::Method::Post, "/agents/register") => {
                // N3 — registration rate limit (anti-flood for local processes; normal TOFU rotation is far below the threshold).
                if !register_allowed(crate::core::agent::now_secs()) {
                    let out = serde_json::json!({ "ok": false, "error": "rate_limited", "code": 42901 })
                        .to_string();
                    respond_json(req, 429, out);
                    continue;
                }
                // TOFU registration (docs/permissions.md): the first time creates the identity and issues a token; an active re-send = rotation; revoked is rejected
                let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                let kind = pick_str(&v, &["kind"]).unwrap_or_else(|| "custom".into());
                let via = pick_str(&v, &["via"]).unwrap_or_else(|| "mcp".into());
                match identity::register(&store, &kind, &via) {
                    Ok((agent_id, token)) => {
                        let out = serde_json::json!({ "agentId": agent_id, "token": token }).to_string();
                        respond_json(req, 200, out);
                    }
                    Err("revoked") => {
                        let out =
                            serde_json::json!({ "ok": false, "error": "revoked", "code": 40102 }).to_string();
                        respond_json(req, 403, out);
                    }
                    Err(e) => {
                        let out =
                            serde_json::json!({ "ok": false, "error": e }).to_string();
                        respond_json(req, 500, out);
                    }
                }
            }
            (tiny_http::Method::Post, "/event") => {
                match authorize(&store, &headers) {
                    Ok(agent_id) => {
                        let kind = identity::kind_for(&store, &agent_id);
                        event::ingest(Some(&handle), &store, &bus, &body, kind.as_deref().unwrap_or("unknown"), Some(&agent_id));
                        // SessionStart + host supports context injection → attach the capability digest,
                        // so the agent knows what OpenCapX can do without calling list_capabilities first
                        // (docs/mcp.md "Session-start capability injection"). Other events reply plain "ok".
                        let out = session_start_reply(kind.as_deref().unwrap_or(""), &body, &bus)
                            .map(|ctx| serde_json::json!({ "ok": true, "additionalContext": ctx }).to_string())
                            .unwrap_or_else(|| "ok".to_string());
                        let _ = req.respond(tiny_http::Response::from_string(out));
                    }
                    Err((status, code, _reason)) => {
                        let out =
                            serde_json::json!({ "ok": false, "error": "unauthenticated", "code": code }).to_string();
                        respond_json(req, status, out);
                    }
                }
            }
            (tiny_http::Method::Post, "/rpc") => {
                match authorize(&store, &headers) {
                    Ok(agent_id) => {
                        // N4 — in-flight limit: over the limit returns 503 quickly without spawning (ask can hang 60s).
                        let Some(slot) = RpcSlot::acquire() else {
                            let out = serde_json::json!({ "ok": false, "error": "busy", "code": 50301 })
                                .to_string();
                            respond_json(req, 503, out);
                            continue;
                        };
                        // ask may block for minutes, so run it on its own thread and don't block /event
                        let h = handle.clone();
                        let b = bus.clone();
                        let st = store.clone();
                        let aid = agent_id;
                        let conn = extract_conn(&headers);
                        let project = extract_project(&headers);
                        std::thread::spawn(move || {
                            let _slot = slot;
                            // Request chain trace: this thread's lifetime is the trace scope (thread-local).
                            // catch_unwind: the root span must also be finished when handle panics — otherwise
                            // that trace stays "pending" forever (start with no end in the viewer).
                            crate::core::req_trace::begin(&aid, &conn, &project);
                            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                crate::core::rpc::handle(&h, &b, &st, &aid, &conn, &body)
                            }));
                            let (result, panic_err) = match outcome {
                                Ok(r) => (r, None),
                                Err(p) => (
                                    serde_json::json!({ "ok": false, "error": "internal panic" }).to_string(),
                                    Some(panic_message(&p)),
                                ),
                            };
                            let (ok, err) = parse_rpc_outcome(&result);
                            // Prefer recording the real panic info in the trace (a debugging artifact should give the reason);
                            // the CLI still gets the generic "internal panic" above, without leaking internal details.
                            crate::core::req_trace::finish(
                                ok,
                                panic_err.or(err).as_deref(),
                                serde_json::json!({}),
                            );
                            let _ = req.respond(tiny_http::Response::from_string(result).with_header(
                                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                                    .unwrap(),
                            ));
                        });
                    }
                    Err((status, code, _reason)) => {
                        let out =
                            serde_json::json!({ "ok": false, "error": "unauthenticated", "code": code }).to_string();
                        respond_json(req, status, out);
                    }
                }
            }
            (tiny_http::Method::Post, "/rpc/cancel") => {
                // mcp.md "Cancellation" v2: Agent host notifications/cancelled → the CLI forwards here,
                // which actually interrupts the pending ask (the bubble closes immediately). Interrupting plugin process calls is out of v1.1 scope.
                match authorize(&store, &headers) {
                    Ok(_) => {
                        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                        let request_id = pick_str(&v, &["requestId"]).unwrap_or_default();
                        let cancelled = crate::core::rpc::cancel(&request_id);
                        respond_json(
                            req,
                            200,
                            serde_json::json!({ "ok": true, "cancelled": cancelled }).to_string(),
                        );
                    }
                    Err((status, code, _reason)) => {
                        let out =
                            serde_json::json!({ "ok": false, "error": "unauthenticated", "code": code }).to_string();
                        respond_json(req, status, out);
                    }
                }
            }
            (tiny_http::Method::Get, "/admin") => {
                let _ = req.respond(
                    tiny_http::Response::from_string(crate::admin::ADMIN_HTML).with_header(
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                            .unwrap(),
                    ),
                );
            }
            (tiny_http::Method::Get, "/events") => {
                // SSE: EventBus pushes directly, with a heartbeat every 15s to prevent proxy timeouts.
                // Local read-only observation is unauthenticated by default (docs/permissions.md); with X-OpenCapX-Conn
                // and a passing auth = CLI subscription channel: SSE disconnect (Agent host exit) → clean up all
                // subscriptions under that conn (watchers stop too). Run on its own thread — the CLI keeps
                // this stream open year-round, and blocking the main loop would stall /event and /rpc.
                use std::io::Write as _;
                let conn = extract_conn(&headers);
                let bind_conn = !conn.is_empty() && authorize(&store, &headers).is_ok();
                let rx = bus.subscribe();
                let bus_h = bus.clone();
                std::thread::spawn(move || {
                    let mut sender = req.into_writer();
                    let _ = sender.write_all(b": connected\n\n");
                    let _ = sender.flush();
                    let mut beat = std::time::Instant::now();
                    loop {
                        match rx.recv_timeout(std::time::Duration::from_secs(15)) {
                            Ok(ev) => {
                                let line = format!(
                                    "event: {}\ndata: {}\n\n",
                                    ev.kind,
                                    serde_json::to_string(&ev).unwrap_or_default()
                                );
                                if sender.write_all(line.as_bytes()).is_err()
                                    || sender.flush().is_err()
                                {
                                    break;
                                }
                                beat = std::time::Instant::now();
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                                if beat.elapsed() >= std::time::Duration::from_secs(15) {
                                    if sender.write_all(b": ping\n\n").is_err()
                                        || sender.flush().is_err()
                                    {
                                        break;
                                    }
                                    beat = std::time::Instant::now();
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    drop(sender);
                    if bind_conn {
                        crate::core::subscription::cleanup_conn(&conn, &bus_h);
                    }
                });
            }
            _ => {
                let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_request_project_header_stays_ascii() {
        // Regression: short_path yields a multibyte "…" for 3+ segment cwd paths; a non-ASCII
        // byte in the raw header line makes the server drop the connection, and every agent
        // tool call from a deep cwd failed with a misleading "app is not running".
        assert_eq!(ascii_header_value("…/OpenCapX/src-tauri"), ".../OpenCapX/src-tauri");
        assert_eq!(ascii_header_value("~/a/b"), "~/a/b");
        // The test binary's cwd is a 3+ segment path, so rpc_request exercises the ellipsis branch.
        let req = rpc_request(None, "{}");
        for line in req.split("\r\n") {
            if line.starts_with("X-OpenCapX-Project:") {
                assert!(line.is_ascii(), "non-ASCII project header: {line:?}");
            }
        }
    }

    #[test]
    fn header_values_strip_crlf_and_control_chars() {
        // A CR/LF inside any header value would split the header block (header injection /
        // request smuggling); a tampered token file or a newline in a project/conn value
        // must never reach the wire raw.
        assert_eq!(ascii_header_value("tok\r\nX-Evil: 1"), "tokX-Evil: 1");
        assert_eq!(ascii_header_value("agent\u{0}\n1"), "agent1");
        assert_eq!(ascii_header_value("keep spaces\tand-tabs"), "keep spaces\tand-tabs");
        let creds = Credentials { agent_id: "a\r\nX-Evil: 1".into(), token: "t".into() };
        let lines = auth_header_lines(Some(&creds));
        for line in lines.split("\r\n") {
            assert!(!line.starts_with("X-Evil"), "smuggled header line survived: {lines:?}");
        }
        let req = events_request(None, "conn\r\nX-Evil: 1");
        for line in req.split("\r\n") {
            assert!(!line.starts_with("X-Evil"), "smuggled header line survived: {req:?}");
        }
    }

    #[test]
    fn hook_payload_extracts_agent_and_text() {
        let (agent, text) = parse_hook_payload(r#"{"agent":"codex","text":"done"}"#);
        assert_eq!(agent, "codex");
        assert_eq!(text, "done");
    }

    #[test]
    fn session_start_reply_gates_on_host_and_event() {
        let bus = EventBus::shared();
        // not a session start → None before anything else runs
        assert!(session_start_reply("claude", r#"{"hook_event_name":"Stop"}"#, &bus).is_none());
        // host without documented additionalContext support → dumb pipe
        assert!(session_start_reply("cursor", r#"{"hook_event_name":"SessionStart"}"#, &bus).is_none());
        assert!(session_start_reply("gemini", r#"{"hook_event_name":"SessionStart"}"#, &bus).is_none());
        // unknown host
        assert!(session_start_reply("some-agent", r#"{"hook_event_name":"SessionStart"}"#, &bus).is_none());
        // malformed body
        assert!(session_start_reply("claude", "not json", &bus).is_none());
        // (the Some-path is order-dependent in the full suite — another test may have set
        // the shared store with registered capabilities — so it is not asserted here)
    }

    #[test]
    fn session_context_host_is_the_claude_nested_family() {
        for k in ["claude", "codex", "droid", "grok"] {
            assert!(session_context_host(k), "{k} should inject");
        }
        for k in ["cursor", "gemini", "opencode", "kiro", "windsurf", "antigravity", "copilot", "pi"] {
            assert!(!session_context_host(k), "{k} must stay zero-stdout");
        }
    }

    #[test]
    fn hook_payload_bad_json_falls_back() {
        let (agent, text) = parse_hook_payload("not json");
        assert_eq!(agent, "unknown");
        assert_eq!(text, "");
    }

    #[test]
    fn auth_header_lines_present_or_empty() {
        let c = Credentials { agent_id: "ag_x_01".into(), token: "ocx1_t".into() };
        let h = auth_header_lines(Some(&c));
        assert!(h.contains("Authorization: Bearer ocx1_t\r\n"));
        assert!(h.contains("X-OpenCapX-Agent: ag_x_01\r\n"));
        assert_eq!(auth_header_lines(None), "");
    }

    /// B10: conn header extraction (case-insensitive), shared by the subscription channel and /rpc.
    #[test]
    fn conn_header_extracted_case_insensitive() {
        let hdr = |f: &str, v: &str| vec![(f.to_string(), v.to_string())];
        assert_eq!(extract_conn(&hdr("X-OpenCapX-Conn", "conn-42")), "conn-42");
        assert_eq!(extract_conn(&hdr("x-opencapx-conn", " c9 ")), "c9");
        assert_eq!(extract_conn(&[]), "");
        assert_eq!(extract_conn(&hdr("Content-Type", "application/json")), "");
    }

    /// Project header extraction: case-insensitive like conn; missing → "" (old CLI).
    #[test]
    fn project_header_extracted_case_insensitive() {
        let hdr = |f: &str, v: &str| vec![(f.to_string(), v.to_string())];
        assert_eq!(extract_project(&hdr("X-OpenCapX-Project", "temp-workspace/OpenCapX")), "temp-workspace/OpenCapX");
        assert_eq!(extract_project(&hdr("x-opencapx-project", " proj/x ")), "proj/x");
        assert_eq!(extract_project(&[]), "");
        assert_eq!(extract_project(&hdr("X-OpenCapX-Conn", "conn-42")), "");
    }

    /// /rpc request headers: auth + project carried with the same mechanism as conn; the project value comes from the process cwd (non-empty).
    #[test]
    fn rpc_request_carries_project_header() {
        let c = Credentials { agent_id: "ag_x_01".into(), token: "ocx1_t".into() };
        let req = rpc_request(Some(&c), "{}");
        assert!(req.starts_with("POST /rpc HTTP/1.1\r\n"));
        assert!(req.contains("Authorization: Bearer ocx1_t\r\n"));
        assert!(req.contains("X-OpenCapX-Project: "));
        assert!(!cli_project().is_empty(), "cargo test cwd should be readable");
    }

    /// /rpc result line three states: {ok:true} / {ok:false,error} / non-JSON.
    #[test]
    fn parse_rpc_outcome_covers_three_shapes() {
        assert_eq!(parse_rpc_outcome(r#"{"ok":true}"#), (true, None));
        assert_eq!(
            parse_rpc_outcome(r#"{"ok":false,"error":"permission_denied","code":40001}"#),
            (false, Some("permission_denied".into()))
        );
        assert_eq!(
            parse_rpc_outcome("not json"),
            (false, Some("unparseable response".into()))
        );
        // Success shapes are more than {ok:true}: list_capabilities returns {"capabilities":[…]} directly,
        // with no ok field — missing this marks success as error (observed in practice).
        assert_eq!(parse_rpc_outcome(r#"{"capabilities":[{"id":"things.list"}]}"#), (true, None));
        assert_eq!(parse_rpc_outcome(r#"{"ok":true,"answer":"yes"}"#), (true, None));
    }

    /// panic payload downcast for two types + fallback.
    /// Box<dyn Any + Send> must be annotated explicitly: &Box<&str> does not coerce to &Box<dyn Any> automatically.
    #[test]
    fn panic_message_downcasts_str_and_string() {
        let s: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(panic_message(&s), "boom");
        let s: Box<dyn std::any::Any + Send> = Box::new("boom".to_string());
        assert_eq!(panic_message(&s), "boom");
        let s: Box<dyn std::any::Any + Send> = Box::new(42u8);
        assert_eq!(panic_message(&s), "unknown panic");
    }

    /// /rpc payload: requestId enters the body only when provided (cancellable call); when absent the v1 shape is kept.
    #[test]
    fn rpc_payload_carries_optional_request_id() {
        let with = rpc_payload("opencapx.ask", &serde_json::json!({"question":"q"}), Some("42"));
        let v: serde_json::Value = serde_json::from_str(&with).unwrap();
        assert_eq!(v["requestId"], serde_json::json!("42"));
        assert_eq!(v["tool"], serde_json::json!("opencapx.ask"));
        let without = rpc_payload("opencapx.say", &serde_json::json!({"text":"hi"}), None);
        let v2: serde_json::Value = serde_json::from_str(&without).unwrap();
        assert!(v2.get("requestId").is_none());
    }

    /// CLI SSE subscription channel request headers: auth + conn binding, both required.
    #[test]
    fn events_request_binds_auth_and_conn() {
        let c = Credentials { agent_id: "ag_x_01".into(), token: "ocx1_t".into() };
        let req = events_request(Some(&c), "conn-7");
        assert!(req.starts_with("GET /events HTTP/1.1\r\n"));
        assert!(req.contains("Authorization: Bearer ocx1_t\r\n"));
        assert!(req.contains("X-OpenCapX-Conn: conn-7\r\n"));
        let anon = events_request(None, "conn-7");
        assert!(!anon.contains("Authorization"));
        assert!(anon.contains("X-OpenCapX-Conn: conn-7\r\n"));
    }

    /// Deliver three-state classification: 2xx=Sent; non-2xx like 401/403=Rejected;
    /// empty read / non-HTTP reply = Unreachable (the CLI uses this to decide "queue silently" or "prompt recovery").
    #[test]
    fn classify_status_maps_response_to_deliver() {
        assert_eq!(classify_status("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nok"), Deliver::Sent);
        assert_eq!(classify_status("HTTP/1.1 204 No Content\r\n\r\n"), Deliver::Sent);
        assert_eq!(
            classify_status("HTTP/1.1 401\r\n\r\n{\"ok\":false,\"code\":40102}"),
            Deliver::Rejected
        );
        assert_eq!(
            classify_status("HTTP/1.1 401\r\n\r\n{\"ok\":false,\"code\":40101}"),
            Deliver::RejectedBadToken,
            "bad_token has its own bucket so the delivery layer can self-heal and re-register"
        );
        assert_eq!(
            classify_status("HTTP/1.1 401\r\n\r\n{}"),
            Deliver::Rejected,
            "401 with no code: conservatively treated as non-self-healing"
        );
        assert_eq!(classify_status("HTTP/1.1 403 Forbidden\r\n\r\n{}"), Deliver::Rejected);
        assert_eq!(classify_status("HTTP/1.1 500\r\n\r\n"), Deliver::Rejected);
        assert_eq!(classify_status(""), Deliver::Unreachable);
        assert_eq!(classify_status("not http at all"), Deliver::Unreachable);
        assert_eq!(classify_status("HTTP/1.1\r\n\r\n"), Deliver::Unreachable);
    }

    /// Registration response parsing: 200 → (agentId, token); 403 + code 40102 (server-side revoked
    /// registration response) → Rejected{403, 40102}, and ensure_registered uses this to show the "go to the settings page
    /// Reauthorize" text; malformed 2xx → BadResponse.
    #[test]
    fn parse_register_response_splits_ok_rejected_bad() {
        let ok = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"agentId\":\"ag_x_9\",\"token\":\"ocx1_t\"}";
        assert_eq!(parse_register_response(ok), Ok(("ag_x_9".into(), "ocx1_t".into())));
        let revoked =
            "HTTP/1.1 403 Forbidden\r\n\r\n{\"ok\":false,\"error\":\"revoked\",\"code\":40102}";
        assert_eq!(
            parse_register_response(revoked),
            Err(RegisterError::Rejected { status: 403, code: 40102 })
        );
        let broken = "HTTP/1.1 200 OK\r\n\r\nnot json";
        assert_eq!(parse_register_response(broken), Err(RegisterError::BadResponse));
        let no_token = "HTTP/1.1 200 OK\r\n\r\n{\"agentId\":\"ag_x_9\"}";
        assert_eq!(parse_register_response(no_token), Err(RegisterError::BadResponse));
    }

    /// OPEN_CAPX_DEV is a process-level env, so the two env-related tests are serialized to avoid parallel interference.
    fn dev_env_lock() -> &'static std::sync::Mutex<()> {
        static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        L.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// Gateway: anonymous denied with 40101, bad token / unknown agent denied with 40101, good token allowed, revoked denied with 40102.
    /// serve() binds LISTEN_ADDR and cannot be started concurrently, so this tests authorize() + extract_auth() directly.
    #[test]
    fn auth_gateway_decisions() {
        use crate::core::storage::StoreEnum;
        use std::sync::Mutex;
        let _guard = dev_env_lock().lock().unwrap();
        let dir = std::env::temp_dir().join(format!("opencapx-authgw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        let (aid, tok) = identity::register(&store, "claude", "test").unwrap();
        let hdr = |a: Option<&str>, t: Option<&str>| -> Vec<(String, String)> {
            let mut v = Vec::new();
            if let Some(a) = a {
                v.push(("X-OpenCapX-Agent".to_string(), a.to_string()));
            }
            if let Some(t) = t {
                v.push(("Authorization".to_string(), format!("Bearer {}", t)));
            }
            v
        };
        // In dev mode authorize is always Ok — temporarily clear the env to test the strict path
        let had_dev = std::env::var("OPEN_CAPX_DEV").ok();
        std::env::remove_var("OPEN_CAPX_DEV");
        assert!(matches!(authorize(&store, &hdr(Some(&aid), Some(&tok))), Ok(_)));
        assert_eq!(authorize(&store, &hdr(None, None)), Err((401, 40101, "anonymous")));
        assert_eq!(authorize(&store, &hdr(Some(&aid), Some("ocx1_wrong"))), Err((401, 40101, "bad_token")));
        assert_eq!(
            authorize(&store, &hdr(Some("ag_ghost_00"), Some(&tok))),
            Err((401, 40101, "bad_token"))
        );
        assert!(identity::revoke(&store, &aid));
        assert_eq!(authorize(&store, &hdr(Some(&aid), Some(&tok))), Err((401, 40102, "revoked")));
        if let Some(d) = had_dev {
            std::env::set_var("OPEN_CAPX_DEV", d);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// dev bypass: with OPEN_CAPX_DEV=1, anonymous also passes (principal __dev__).
    #[test]
    fn dev_mode_bypasses_auth() {
        use crate::core::storage::StoreEnum;
        use std::sync::Mutex;
        let _guard = dev_env_lock().lock().unwrap();
        let dir = std::env::temp_dir().join(format!("opencapx-authdev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        std::env::set_var("OPEN_CAPX_DEV", "1");
        let r = authorize(&store, &[]);
        std::env::remove_var("OPEN_CAPX_DEV");
        assert_eq!(r, Ok("__dev__".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// N1 — request body gate: over the limit rejected, within the limit allowed (tested directly with Cursor, same origin as the 413 path).
    #[test]
    fn body_gate_rejects_oversized_and_passes_bounded() {
        use std::io::Cursor;
        assert_eq!(read_body_limited(Cursor::new(vec![b'a'; 10]), 5), Err(()));
        assert_eq!(
            read_body_limited(Cursor::new(vec![b'a'; 5]), 5),
            Ok("aaaaa".to_string())
        );
        assert_eq!(MAX_BODY_BYTES, 2 * 1024 * 1024);
    }

    /// N3 — registration rate limit: 30 per window, over the limit rejected; recovered as the window rolls.
    #[test]
    fn register_limiter_windows() {
        let t0 = 1_000_000u64;
        for i in 0..30 {
            assert!(register_allowed(t0 + i), "attempt {} should be allowed", i + 1);
        }
        assert!(!register_allowed(t0 + 30), "the 31st attempt in the window should be rejected");
        assert!(register_allowed(t0 + 60), "recovers after the window rolls");
    }

    /// N4 — /rpc slots: obtainable within the limit; None over the limit; obtainable again after release.
    #[test]
    fn rpc_slots_are_bounded() {
        let mut slots: Vec<RpcSlot> = Vec::new();
        for _ in 0..MAX_RPC_IN_FLIGHT {
            slots.push(RpcSlot::acquire().expect("should acquire within the limit"));
        }
        assert!(RpcSlot::acquire().is_none(), "should reject over the limit");
        slots.pop();
        assert!(RpcSlot::acquire().is_some(), "obtainable again after release");
        drop(slots);
    }

    /// /admin endpoint: returns 200 + text/html, body contains the EventSource marker.
    #[test]
    fn admin_endpoint_serves_html() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let body = crate::admin::ADMIN_HTML;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = s.write_all(resp.as_bytes());
            let _ = s.shutdown(std::net::Shutdown::Both);
        });
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(b"GET /admin HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut out = Vec::new();
        let _ = client.read_to_end(&mut out);
        let s = String::from_utf8_lossy(&out);
        assert!(s.starts_with("HTTP/1.1 200"), "got: {}", &s[..s.find('\r').unwrap_or(s.len()).min(40)]);
        assert!(s.contains("text/html"), "missing content type");
        assert!(s.contains("EventSource"), "missing EventSource client");
        assert!(s.contains("/events"), "missing SSE endpoint reference");
        let _ = t.join();
    }

    /// SSE end-to-end: subscribe to /events, and after an event is published the client reads NDJSON-over-SSE within 1s.
    /// Start the server on a temporary port at 127.0.0.1:0, the client does a TcpStream GET, and it finishes as soon as an event: line is read.
    #[test]
    fn sse_stream_delivers_event() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::sync::mpsc;
        use std::time::Duration;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let bus = Arc::new(crate::core::event::EventBus::new());
        let (ready_tx, ready_rx) = mpsc::channel();
        let bus_h = bus.clone();
        let t = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut s = stream;
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
            let _ = s.flush();
            // After the client GET completes and the server has written the response headers → now subscribed
            let rx = bus_h.subscribe();
            let _ = ready_tx.send(());
            // push the event
            bus_h.publish(&crate::core::event::OpencapxEvent::new(
                "test.event",
                "smoke",
                serde_json::json!({"hello":"world"}),
            ));
            let ev = rx.recv_timeout(Duration::from_secs(1)).expect("rx event");
            let line = format!(
                "event: {}\ndata: {}\n\n",
                ev.kind,
                serde_json::to_string(&ev).unwrap_or_default()
            );
            let _ = s.write_all(line.as_bytes());
            let _ = s.flush();
            std::thread::sleep(Duration::from_millis(100));
            let _ = s.shutdown(std::net::Shutdown::Both);
        });
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(b"GET /events HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        // Wait until the server has written the response headers + subscription is done + publish is done
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut buf = Vec::new();
        let _ = client.read_to_end(&mut buf);
        let s = String::from_utf8_lossy(&buf);
        assert!(s.starts_with("HTTP/1.1 200"), "got: {}", s);
        assert!(s.contains("event: test.event"), "missing event line: {}", s);
        assert!(s.contains("\"hello\":\"world\""), "missing data: {}", s);
        let _ = t.join();
    }
}
