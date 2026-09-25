//! the client half: event posting, RPC forwarding, SSE subscription, and agent registration over the local HTTP entry.
//! Mechanical move from http.rs.

use super::*;

/// Client auth headers (two lines with a CRLF prefix). Empty string when there are no credentials.
/// Values pass through `ascii_header_value`: a CR/LF inside the on-disk token or agent_id
/// would otherwise split the header block (header injection / request smuggling).
pub(crate) fn auth_header_lines(creds: Option<&Credentials>) -> String {
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
pub fn post_event_with_body(
    payload: &str,
    creds: Option<&Credentials>,
) -> (Deliver, Option<String>) {
    post_route_with_body("/event", payload, creds)
}

pub fn post_route(route: &str, payload: &str, creds: Option<&Credentials>) -> Deliver {
    post_route_with_body(route, payload, creds).0
}

fn post_route_with_body(
    route: &str,
    payload: &str,
    creds: Option<&Credentials>,
) -> (Deliver, Option<String>) {
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
pub(crate) fn session_start_reply(kind: &str, body: &str, bus: &EventBus) -> Option<String> {
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
pub(crate) fn parse_rpc_outcome(s: &str) -> (bool, Option<String>) {
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
pub(crate) fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
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
pub(crate) fn classify_status(raw: &str) -> Deliver {
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
pub(crate) fn cli_project() -> &'static str {
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
pub(crate) fn ascii_header_value(s: &str) -> String {
    let fixed = s.replace('…', "...");
    fixed
        .chars()
        .filter(|c| *c == ' ' || *c == '\t' || c.is_ascii_graphic())
        .collect()
}

/// /rpc request headers: auth + owning project (the viewer groups request chains by project).
pub(crate) fn rpc_request(creds: Option<&Credentials>, payload: &str) -> String {
    format!(
        "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1:47628\r\nContent-Type: application/json\r\n{}X-OpenCapX-Project: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        auth_header_lines(creds),
        ascii_header_value(cli_project()),
        payload.len(),
        payload
    )
}

pub(crate) fn rpc_payload(
    tool: &str,
    input: &serde_json::Value,
    request_id: Option<&str>,
) -> String {
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
    stream
        .write_all(events_request(creds, conn_id).as_bytes())
        .ok()?;
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

pub(crate) fn events_request(creds: Option<&Credentials>, conn_id: &str) -> String {
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
    let payload =
        serde_json::json!({ "kind": kind, "displayName": display_name, "via": via }).to_string();
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
pub(crate) fn parse_register_response(buf: &str) -> Result<(String, String), RegisterError> {
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
    let body = buf
        .split("\r\n\r\n")
        .nth(1)
        .ok_or(RegisterError::BadResponse)?;
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|_| RegisterError::BadResponse)?;
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
