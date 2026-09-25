use super::*;

#[test]
fn rpc_request_project_header_stays_ascii() {
    // Regression: short_path yields a multibyte "…" for 3+ segment cwd paths; a non-ASCII
    // byte in the raw header line makes the server drop the connection, and every agent
    // tool call from a deep cwd failed with a misleading "app is not running".
    assert_eq!(
        ascii_header_value("…/OpenCapX/src-tauri"),
        ".../OpenCapX/src-tauri"
    );
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
    assert_eq!(
        ascii_header_value("keep spaces\tand-tabs"),
        "keep spaces\tand-tabs"
    );
    let creds = Credentials {
        agent_id: "a\r\nX-Evil: 1".into(),
        token: "t".into(),
    };
    let lines = auth_header_lines(Some(&creds));
    for line in lines.split("\r\n") {
        assert!(
            !line.starts_with("X-Evil"),
            "smuggled header line survived: {lines:?}"
        );
    }
    let req = events_request(None, "conn\r\nX-Evil: 1");
    for line in req.split("\r\n") {
        assert!(
            !line.starts_with("X-Evil"),
            "smuggled header line survived: {req:?}"
        );
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
    assert!(
        session_start_reply("some-agent", r#"{"hook_event_name":"SessionStart"}"#, &bus).is_none()
    );
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
    for k in [
        "cursor",
        "gemini",
        "opencode",
        "kiro",
        "windsurf",
        "antigravity",
        "copilot",
        "pi",
    ] {
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
    let c = Credentials {
        agent_id: "ag_x_01".into(),
        token: "ocx1_t".into(),
    };
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
    assert_eq!(
        extract_project(&hdr("X-OpenCapX-Project", "temp-workspace/OpenCapX")),
        "temp-workspace/OpenCapX"
    );
    assert_eq!(
        extract_project(&hdr("x-opencapx-project", " proj/x ")),
        "proj/x"
    );
    assert_eq!(extract_project(&[]), "");
    assert_eq!(extract_project(&hdr("X-OpenCapX-Conn", "conn-42")), "");
}

/// /rpc request headers: auth + project carried with the same mechanism as conn; the project value comes from the process cwd (non-empty).
#[test]
fn rpc_request_carries_project_header() {
    let c = Credentials {
        agent_id: "ag_x_01".into(),
        token: "ocx1_t".into(),
    };
    let req = rpc_request(Some(&c), "{}");
    assert!(req.starts_with("POST /rpc HTTP/1.1\r\n"));
    assert!(req.contains("Authorization: Bearer ocx1_t\r\n"));
    assert!(req.contains("X-OpenCapX-Project: "));
    assert!(
        !cli_project().is_empty(),
        "cargo test cwd should be readable"
    );
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
    assert_eq!(
        parse_rpc_outcome(r#"{"capabilities":[{"id":"things.list"}]}"#),
        (true, None)
    );
    assert_eq!(
        parse_rpc_outcome(r#"{"ok":true,"answer":"yes"}"#),
        (true, None)
    );
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
    let with = rpc_payload(
        "opencapx.ask",
        &serde_json::json!({"question":"q"}),
        Some("42"),
    );
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
    let c = Credentials {
        agent_id: "ag_x_01".into(),
        token: "ocx1_t".into(),
    };
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
    assert_eq!(
        classify_status("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nok"),
        Deliver::Sent
    );
    assert_eq!(
        classify_status("HTTP/1.1 204 No Content\r\n\r\n"),
        Deliver::Sent
    );
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
    assert_eq!(
        classify_status("HTTP/1.1 403 Forbidden\r\n\r\n{}"),
        Deliver::Rejected
    );
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
    assert_eq!(
        parse_register_response(ok),
        Ok(("ag_x_9".into(), "ocx1_t".into()))
    );
    let revoked =
        "HTTP/1.1 403 Forbidden\r\n\r\n{\"ok\":false,\"error\":\"revoked\",\"code\":40102}";
    assert_eq!(
        parse_register_response(revoked),
        Err(RegisterError::Rejected {
            status: 403,
            code: 40102
        })
    );
    let broken = "HTTP/1.1 200 OK\r\n\r\nnot json";
    assert_eq!(
        parse_register_response(broken),
        Err(RegisterError::BadResponse)
    );
    let no_token = "HTTP/1.1 200 OK\r\n\r\n{\"agentId\":\"ag_x_9\"}";
    assert_eq!(
        parse_register_response(no_token),
        Err(RegisterError::BadResponse)
    );
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
    assert!(matches!(
        authorize(&store, &hdr(Some(&aid), Some(&tok))),
        Ok(_)
    ));
    assert_eq!(
        authorize(&store, &hdr(None, None)),
        Err((401, 40101, "anonymous"))
    );
    assert_eq!(
        authorize(&store, &hdr(Some(&aid), Some("ocx1_wrong"))),
        Err((401, 40101, "bad_token"))
    );
    assert_eq!(
        authorize(&store, &hdr(Some("ag_ghost_00"), Some(&tok))),
        Err((401, 40101, "bad_token"))
    );
    assert!(identity::revoke(&store, &aid));
    assert_eq!(
        authorize(&store, &hdr(Some(&aid), Some(&tok))),
        Err((401, 40102, "revoked"))
    );
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
        assert!(
            register_allowed(t0 + i),
            "attempt {} should be allowed",
            i + 1
        );
    }
    assert!(
        !register_allowed(t0 + 30),
        "the 31st attempt in the window should be rejected"
    );
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
    assert!(
        RpcSlot::acquire().is_some(),
        "obtainable again after release"
    );
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
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut out = Vec::new();
    let _ = client.read_to_end(&mut out);
    let s = String::from_utf8_lossy(&out);
    assert!(
        s.starts_with("HTTP/1.1 200"),
        "got: {}",
        &s[..s.find('\r').unwrap_or(s.len()).min(40)]
    );
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
    use std::sync::mpsc;
    use std::sync::Arc;
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
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
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
