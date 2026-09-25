//! the server half: the tiny_http serve loop, auth extraction/authorization, body limits, and the in-flight RPC slot.
//! Mechanical move from http.rs.

use super::*;

/// Server side: extract (agent_id, bearer token) from request headers. Missing either → None.
fn extract_auth(headers: &[(String, String)]) -> Option<(String, String)> {
    let mut agent_id: Option<String> = None;
    let mut token: Option<String> = None;
    for (field, value) in headers {
        if field.eq_ignore_ascii_case("x-opencapx-agent") {
            agent_id = Some(value.trim().to_string());
        } else if field.eq_ignore_ascii_case("authorization") {
            let t = value.trim();
            if let Some(rest) = t
                .strip_prefix("Bearer ")
                .or_else(|| t.strip_prefix("bearer "))
            {
                token = Some(rest.trim().to_string());
            }
        }
    }
    Some((agent_id?, token?))
}

/// Extract X-OpenCapX-Conn: the unique connection id of the CLI process, reported with /rpc and SSE /events;
/// the subscription table uses it to bind the connection lifecycle (cleaned up on disconnect, docs/capability.md "Capability Types").
pub(crate) fn extract_conn(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .find(|(f, _)| f.eq_ignore_ascii_case("x-opencapx-conn"))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

/// Extract X-OpenCapX-Project: short path of the project where the CLI process runs, reported with /rpc;
/// the viewer groups request chains into the same project by it ("" = old CLI did not report, treated as unknown).
pub(crate) fn extract_project(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .find(|(f, _)| f.eq_ignore_ascii_case("x-opencapx-project"))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

/// Auth gateway: pass → agent_id; fail → (http status code, business code, rejection cause). The cause is written to auth.rejected.
pub(crate) fn authorize(
    store: &SharedStore,
    headers: &[(String, String)],
) -> Result<String, (u16, i64, &'static str)> {
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
                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                    .unwrap(),
            ),
    );
}

/// N1 — request body limit 2 MiB (double the 1 MiB capability input gate; anonymous requests also pass this gate first).
pub(crate) const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// N1 — bounded read of the request body: actual bytes over the limit → Err (caller returns 413).
pub(crate) fn read_body_limited<R: std::io::Read>(reader: R, max: usize) -> Result<String, ()> {
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
pub(crate) fn register_allowed(now: u64) -> bool {
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
pub(crate) const MAX_RPC_IN_FLIGHT: usize = 64;

pub(crate) static RPC_IN_FLIGHT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// N4 — in-flight slot (RAII); released automatically when the thread ends.
pub(crate) struct RpcSlot;

impl RpcSlot {
    pub(crate) fn acquire() -> Option<Self> {
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
        if req
            .body_length()
            .map(|n| n > MAX_BODY_BYTES)
            .unwrap_or(false)
        {
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
                    let out =
                        serde_json::json!({ "ok": false, "error": "rate_limited", "code": 42901 })
                            .to_string();
                    respond_json(req, 429, out);
                    continue;
                }
                // TOFU registration (docs/permissions.md): the first time creates the identity and issues a token; an active re-send = rotation; revoked is rejected
                let v: serde_json::Value =
                    serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                let kind = pick_str(&v, &["kind"]).unwrap_or_else(|| "custom".into());
                let via = pick_str(&v, &["via"]).unwrap_or_else(|| "mcp".into());
                match identity::register(&store, &kind, &via) {
                    Ok((agent_id, token)) => {
                        let out =
                            serde_json::json!({ "agentId": agent_id, "token": token }).to_string();
                        respond_json(req, 200, out);
                    }
                    Err("revoked") => {
                        let out =
                            serde_json::json!({ "ok": false, "error": "revoked", "code": 40102 })
                                .to_string();
                        respond_json(req, 403, out);
                    }
                    Err(e) => {
                        let out = serde_json::json!({ "ok": false, "error": e }).to_string();
                        respond_json(req, 500, out);
                    }
                }
            }
            (tiny_http::Method::Post, "/event") => {
                match authorize(&store, &headers) {
                    Ok(agent_id) => {
                        let kind = identity::kind_for(&store, &agent_id);
                        event::ingest(
                            Some(&handle),
                            &store,
                            &bus,
                            &body,
                            kind.as_deref().unwrap_or("unknown"),
                            Some(&agent_id),
                        );
                        // SessionStart + host supports context injection → attach the capability digest,
                        // so the agent knows what OpenCapX can do without calling list_capabilities first
                        // (docs/mcp.md "Session-start capability injection"). Other events reply plain "ok".
                        let out = session_start_reply(kind.as_deref().unwrap_or(""), &body, &bus)
                            .map(|ctx| {
                                serde_json::json!({ "ok": true, "additionalContext": ctx })
                                    .to_string()
                            })
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
                            let out =
                                serde_json::json!({ "ok": false, "error": "busy", "code": 50301 })
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
                            let outcome =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    crate::core::rpc::handle(&h, &b, &st, &aid, &conn, &body)
                                }));
                            let (result, panic_err) = match outcome {
                                Ok(r) => (r, None),
                                Err(p) => (
                                    serde_json::json!({ "ok": false, "error": "internal panic" })
                                        .to_string(),
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
                            let _ = req.respond(
                                tiny_http::Response::from_string(result).with_header(
                                    tiny_http::Header::from_bytes(
                                        &b"Content-Type"[..],
                                        &b"application/json"[..],
                                    )
                                    .unwrap(),
                                ),
                            );
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
                        let v: serde_json::Value =
                            serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
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
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"text/html; charset=utf-8"[..],
                        )
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
                let _ = req
                    .respond(tiny_http::Response::from_string("not found").with_status_code(404));
            }
        }
    }
}
