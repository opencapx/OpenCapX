use super::*;

// Use this repo's Python echo plugin for process-level integration tests; skip when python3 is absent
fn echo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("plugins")
        .join("echo-vision")
}

fn python3() -> Option<String> {
    // Require a *successful* --version: on Windows `python3.exe` is a Microsoft Store
    // alias that spawns fine and then exits nonzero without running any Python.
    for c in ["python3", "python"] {
        if Command::new(c)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(c.to_string());
        }
    }
    None
}

#[test]
fn echo_plugin_handshake_and_call() {
    let Some(py) = python3() else {
        eprintln!("skip: no python3");
        return;
    };
    let spec = RuntimeSpec {
        command: py,
        args: vec!["bin/echo_vision.py".into()],
        env: HashMap::new(),
    };
    let on_reverse: OnReverse = Arc::new(|_v: Value, _reply: Reply| {});
    let p = PluginProcess::spawn(
        "com.opencapx.echo-vision",
        &echo_root(),
        &spec,
        "test",
        on_reverse,
        &EnvPolicy::default(),
        None,
    )
    .expect("spawn");
    let init = p
        .call(
            "plugin.initialize",
            json!({"coreVersion":"0.1.0","apiVersion":"1","pluginId":"com.opencapx.echo-vision"}),
            Duration::from_secs(10),
        )
        .expect("handshake");
    assert_eq!(init["pluginId"], "com.opencapx.echo-vision");
    let caps = init["capabilities"].as_array().unwrap().clone();
    assert!(caps.iter().any(|c| c["id"] == "image.analyze"));

    let ping = p
        .call("plugin.ping", json!({}), Duration::from_secs(5))
        .expect("ping");
    assert_eq!(ping["ok"], json!(true));

    let out = p
        .call(
            "image.analyze",
            json!({"image":"/tmp/test.png"}),
            Duration::from_secs(10),
        )
        .expect("analyze");
    assert!(out["description"]
        .as_str()
        .unwrap()
        .contains("/tmp/test.png"));

    p.shutdown();
}

#[test]
fn unknown_method_returns_32601() {
    let Some(py) = python3() else {
        eprintln!("skip: no python3");
        return;
    };
    let spec = RuntimeSpec {
        command: py,
        args: vec!["bin/echo_vision.py".into()],
        env: HashMap::new(),
    };
    let on_reverse: OnReverse = Arc::new(|_v: Value, _reply: Reply| {});
    let p = PluginProcess::spawn(
        "com.opencapx.echo-vision",
        &echo_root(),
        &spec,
        "test",
        on_reverse,
        &EnvPolicy::default(),
        None,
    )
    .expect("spawn");
    let err = p
        .call("no.such.method", json!({}), Duration::from_secs(10))
        .expect_err("must error");
    assert!(err.contains("32601"), "got: {}", err);
    p.shutdown();
}

/// stderr line → publish_stderr_log → EventBus "plugin.log". Verifies the plugin.log event fires.
/// Launch a Python child that immediately print()s to stderr.
#[test]
fn stderr_publishes_plugin_log_event() {
    let Some(py) = python3() else {
        eprintln!("skip: no python3");
        return;
    };
    let bus = crate::core::event::EventBus::shared();
    let mut rx = bus.subscribe();
    let spec = RuntimeSpec {
            command: py,
            args: vec![
                "-c".into(),
                "import sys, time; sys.stderr.write('hello-from-stderr\\n'); sys.stderr.flush(); time.sleep(0.3)".into(),
            ],
            env: HashMap::new(),
        };
    let on_reverse: OnReverse = Arc::new(|_v: Value, _reply: Reply| {});
    let _ = PluginProcess::spawn(
        "com.opencapx.stderr-test",
        &echo_root(),
        &spec,
        "test",
        on_reverse,
        &EnvPolicy::default(),
        None,
    )
    .expect("spawn");
    let mut got = false;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(3) {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(ev) => {
                if ev.kind == "plugin.log"
                    && ev.payload.get("pluginId").and_then(|v| v.as_str())
                        == Some("com.opencapx.stderr-test")
                    && ev.payload.get("source").and_then(|v| v.as_str()) == Some("stderr")
                    && ev.payload.get("message").and_then(|v| v.as_str())
                        == Some("hello-from-stderr")
                {
                    got = true;
                    break;
                }
            }
            Err(_) => {}
        }
    }
    assert!(got, "stderr line should publish plugin.log event");
}

#[test]
fn env_whitelist_rules() {
    let extra = vec!["OPENAI_API_KEY".to_string()];
    assert!(env_is_allowed("PATH", &extra));
    assert!(env_is_allowed("LC_ALL", &extra));
    assert!(env_is_allowed("XDG_DATA_HOME", &extra));
    assert!(env_is_allowed("OPENAI_API_KEY", &extra));
    assert!(!env_is_allowed("AWS_SECRET_ACCESS_KEY", &extra));
    assert!(!env_is_allowed("OPENCAPX_TEST_SECRET", &extra));
}

/// P3 — bounded line read: a normal line passes as-is; an over-long line is dropped entirely without affecting the next line; EOF wraps up.
#[test]
fn read_line_capped_drops_oversized_without_losing_next() {
    let input = format!("ok-1\n{}\nok-2\n", "x".repeat(64));
    let mut r = std::io::BufReader::new(input.as_bytes());
    assert_eq!(
        read_line_capped(&mut r, 16),
        LineOutcome::Line(b"ok-1".to_vec())
    );
    assert_eq!(read_line_capped(&mut r, 16), LineOutcome::Dropped);
    assert_eq!(
        read_line_capped(&mut r, 16),
        LineOutcome::Line(b"ok-2".to_vec())
    );
    assert_eq!(read_line_capped(&mut r, 16), LineOutcome::Eof);
}

/// CRLF: Windows plugin stderr is `\r\n`-terminated; the trailing `\r` must be stripped
/// (it used to ride into every published plugin.log message and break exact matching).
#[test]
fn read_line_capped_strips_crlf_carriage_return() {
    let mut r = std::io::BufReader::new(&b"a\r\nb\nno-eol\r"[..]);
    assert_eq!(
        read_line_capped(&mut r, 16),
        LineOutcome::Line(b"a".to_vec())
    );
    assert_eq!(
        read_line_capped(&mut r, 16),
        LineOutcome::Line(b"b".to_vec())
    );
    assert_eq!(
        read_line_capped(&mut r, 16),
        LineOutcome::Line(b"no-eol".to_vec())
    );
}

/// P3 — the plugin first spews a 5 MiB giant line, and the handshake still succeeds (the reader neither OOMs nor blocks); the giant line is counted and dropped.
#[test]
#[cfg(unix)]
fn oversized_plugin_line_is_dropped_and_handshake_survives() {
    let Some(py) = python3() else {
        eprintln!("skip: no python3");
        return;
    };
    let dir = std::env::temp_dir().join(format!("opencapx-bigline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
            dir.join("bigline.py"),
            r#"import json, sys
sys.stdout.write('x' * (5 * 1024 * 1024) + '\n')
sys.stdout.flush()
for line in sys.stdin:
    m = json.loads(line)
    if m.get('method') == 'plugin.initialize':
        sys.stdout.write(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':{'pluginId':'com.opencapx.bigline','apiVersion':'1','capabilities':[]}}) + '\n')
        sys.stdout.flush()
    if m.get('method') == 'plugin.shutdown':
        break
"#,
        )
        .unwrap();
    let before = DROPPED_FRAMES.load(std::sync::atomic::Ordering::Relaxed);
    let spec = RuntimeSpec {
        command: py,
        args: vec!["bigline.py".into()],
        env: HashMap::new(),
    };
    let on_reverse: OnReverse = Arc::new(|_v: Value, _reply: Reply| {});
    let p = PluginProcess::spawn(
        "com.opencapx.bigline",
        &dir,
        &spec,
        "test",
        on_reverse,
        &EnvPolicy::default(),
        None,
    )
    .expect("spawn");
    let init = p
        .call(
            "plugin.initialize",
            json!({"coreVersion":"t","apiVersion":"1","pluginId":"com.opencapx.bigline"}),
            Duration::from_secs(15),
        )
        .expect("handshake survives giant line");
    assert_eq!(init["pluginId"], json!("com.opencapx.bigline"));
    assert!(
        DROPPED_FRAMES.load(std::sync::atomic::Ordering::Relaxed) > before,
        "a giant line should be counted and dropped"
    );
    p.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// S1 — grandchild kill-through: the plugin spawns `sleep 30`, and the shutdown group kill reaps the grandchild too.
#[test]
#[cfg(unix)]
fn shutdown_kills_grandchildren() {
    let Some(py) = python3() else {
        eprintln!("skip: no python3");
        return;
    };
    let dir = std::env::temp_dir().join(format!("opencapx-gc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
            dir.join("sleeper.py"),
            "import subprocess, time\np = subprocess.Popen(['sleep', '30'])\nopen('grandchild.pid','w').write(str(p.pid))\ntime.sleep(60)\n",
        )
        .unwrap();
    let spec = RuntimeSpec {
        command: py,
        args: vec!["sleeper.py".into()],
        env: HashMap::new(),
    };
    let on_reverse: OnReverse = Arc::new(|_v: Value, _reply: Reply| {});
    let p = PluginProcess::spawn(
        "com.opencapx.gc-test",
        &dir,
        &spec,
        "test",
        on_reverse,
        &EnvPolicy::default(),
        None,
    )
    .expect("spawn");
    let pid_path = dir.join("grandchild.pid");
    let mut pid: i32 = 0;
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(5) {
        if let Ok(text) = std::fs::read_to_string(&pid_path) {
            if let Ok(n) = text.trim().parse() {
                pid = n;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(pid > 0, "grandchild pid not captured");
    assert!(
        unsafe { libc::kill(pid, 0) } == 0,
        "grandchild alive before shutdown"
    );
    p.shutdown();
    let t1 = std::time::Instant::now();
    let mut dead = false;
    while t1.elapsed() < std::time::Duration::from_secs(5) {
        if unsafe { libc::kill(pid, 0) } != 0 {
            dead = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(dead, "grandchild must be dead after shutdown (pid {})", pid);
}

/// S1 — environment isolation: the host secret does not leak, PATH is preserved; after the whitelist addendum it becomes visible.
#[test]
#[cfg(unix)]
fn env_isolation_filters_host_and_honors_allowlist() {
    let Some(py) = python3() else {
        eprintln!("skip: no python3");
        return;
    };
    std::env::set_var("OPENCAPX_TEST_SECRET", "leak-me");
    let dir = std::env::temp_dir().join(format!("opencapx-env-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dump = "import os, json, sys, time\njson.dump(dict(os.environ), open(sys.argv[1], 'w'))\ntime.sleep(2)\n";
    let wait_env = |file: &str| -> Value {
        let path = dir.join(file);
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_secs(5) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(v) = serde_json::from_str(&text) {
                    return v;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("env dump {} not captured", file);
    };

    // ① Default isolation: the secret is invisible; PATH is preserved
    let spec = RuntimeSpec {
        command: py.clone(),
        args: vec!["-c".into(), dump.into(), "env1.json".into()],
        env: HashMap::new(),
    };
    let on_reverse: OnReverse = Arc::new(|_v: Value, _reply: Reply| {});
    let p1 = PluginProcess::spawn(
        "com.opencapx.env1",
        &dir,
        &spec,
        "test",
        on_reverse,
        &EnvPolicy::default(),
        None,
    )
    .expect("spawn 1");
    let env1 = wait_env("env1.json");
    assert!(
        env1.get("OPENCAPX_TEST_SECRET").is_none(),
        "host secret must not leak"
    );
    assert!(env1.get("PATH").is_some(), "PATH must be preserved");
    p1.shutdown();

    // ② Whitelist addendum: the secret is visible
    let spec2 = RuntimeSpec {
        command: py,
        args: vec!["-c".into(), dump.into(), "env2.json".into()],
        env: HashMap::new(),
    };
    let on_reverse2: OnReverse = Arc::new(|_v: Value, _reply: Reply| {});
    let policy = EnvPolicy {
        isolate: true,
        allow: vec!["OPENCAPX_TEST_SECRET".to_string()],
    };
    let p2 = PluginProcess::spawn(
        "com.opencapx.env2",
        &dir,
        &spec2,
        "test",
        on_reverse2,
        &policy,
        None,
    )
    .expect("spawn 2");
    let env2 = wait_env("env2.json");
    assert_eq!(
        env2.get("OPENCAPX_TEST_SECRET").and_then(|v| v.as_str()),
        Some("leak-me"),
        "allowlisted var must pass"
    );
    p2.shutdown();

    std::env::remove_var("OPENCAPX_TEST_SECRET");
    let _ = std::fs::remove_dir_all(&dir);
}

/// O2 — a chatty stderr flood is throttled by the token bucket: only the burst passes, the rest is dropped (log + event share one gate).
#[test]
#[cfg(unix)]
fn stderr_flood_is_throttled() {
    let Some(py) = python3() else {
        eprintln!("skip: no python3");
        return;
    };
    let pid = format!("com.opencapx.stderr-flood-{}", std::process::id());
    let rx = crate::core::event::EventBus::shared().subscribe();
    let spec = RuntimeSpec {
            command: py,
            args: vec![
                "-c".into(),
                "import sys, time\nfor i in range(300):\n    sys.stderr.write('flood-%d\\n' % i)\nsys.stderr.flush()\ntime.sleep(1)".into(),
            ],
            env: HashMap::new(),
        };
    let on_reverse: OnReverse = Arc::new(|_v: Value, _reply: Reply| {});
    let _p = PluginProcess::spawn(
        &pid,
        &echo_root(),
        &spec,
        "test",
        on_reverse,
        &EnvPolicy::default(),
        None,
    )
    .expect("spawn flood");
    // Collect until quiet (the reader thread is async; 300 lines should be processed in <1s)
    let mut delivered = 0usize;
    let start = std::time::Instant::now();
    let mut last = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(5) {
        match rx.recv_timeout(std::time::Duration::from_millis(300)) {
            Ok(ev) => {
                if ev.kind == "plugin.log"
                    && ev.payload.get("pluginId").and_then(|v| v.as_str()) == Some(pid.as_str())
                    && ev.payload.get("source").and_then(|v| v.as_str()) == Some("stderr")
                {
                    delivered += 1;
                    last = std::time::Instant::now();
                }
            }
            Err(_) => {
                if last.elapsed() > std::time::Duration::from_millis(700) {
                    break;
                }
            }
        }
    }
    assert!(delivered >= 1, "some stderr lines should get through");
    assert!(
        delivered <= 120,
        "the flood should be throttled (burst 100 + slack), actual {}",
        delivered
    );
}

/// S5b — macOS sandbox: escape writes are denied, plugin-data writes pass; the control group (no sandbox) can escape.
#[test]
#[cfg(target_os = "macos")]
fn sandbox_exec_blocks_escape_writes() {
    if !std::path::Path::new("/usr/bin/sandbox-exec").is_file() {
        eprintln!("skip: no sandbox-exec");
        return;
    }
    let Some(py) = python3() else {
        eprintln!("skip: no python3");
        return;
    };
    let dir = std::env::temp_dir().join(format!("opencapx-sbxenv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let data_root = dir.join("plugin-data");
    std::env::set_var("OPENCAPX_PLUGIN_DATA_DIR", &data_root);
    let data_dir = data_root.join("com.example.sbx");
    std::fs::create_dir_all(&data_dir).unwrap();
    let data_file = data_dir.join("probe.json");
    let escape_file =
        std::env::temp_dir().join(format!("opencapx-sbx-escape-{}.txt", std::process::id()));

    std::fs::write(
            dir.join("sbx.py"),
            r#"import json, os, sys
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    m = msg.get("method")
    if m == "plugin.initialize":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"pluginId":"com.example.sbx","apiVersion":"1","capabilities":[]}}) + "\n"); sys.stdout.flush()
    elif m == "plugin.ping":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"ok":True}}) + "\n"); sys.stdout.flush()
    elif m == "sbx.probe":
        r = {"escape": "?", "data": "?", "tmp": "?"}
        try:
            open(os.environ["SBX_ESCAPE"], "w").write("x"); r["escape"] = "ok"
        except Exception:
            r["escape"] = "denied"
        try:
            os.makedirs(os.path.dirname(os.environ["SBX_DATA"]), exist_ok=True)
            open(os.environ["SBX_DATA"], "w").write("x"); r["data"] = "ok"
        except Exception:
            r["data"] = "denied"
        try:
            import tempfile
            fd, p = tempfile.mkstemp()
            os.write(fd, b"x"); os.close(fd); os.unlink(p)
            r["tmp"] = "ok"
        except Exception:
            r["tmp"] = "denied"
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":r}) + "\n"); sys.stdout.flush()
    elif m == "plugin.shutdown":
        break
"#,
        )
        .unwrap();
    let spec = RuntimeSpec {
        command: py,
        args: vec!["sbx.py".into()],
        env: HashMap::from([
            ("SBX_ESCAPE".to_string(), escape_file.display().to_string()),
            ("SBX_DATA".to_string(), data_file.display().to_string()),
        ]),
    };
    let decl = crate::core::plugin::SandboxDecl {
        fs: Some(crate::core::plugin::SandboxFs {
            write: vec!["plugin-data".to_string()],
        }),
        network: Some("none".to_string()),
    };
    let profile = super::super::sandbox::sandbox_profile("com.example.sbx", &decl);
    let sbx = SandboxSpec {
        profile,
        tmp_dir: data_dir.join("tmp"),
    };
    std::fs::create_dir_all(&sbx.tmp_dir).unwrap();

    // ① Inside the sandbox: escape writes denied; plugin-data writes pass
    let on_reverse: OnReverse = Arc::new(|_v: Value, _reply: Reply| {});
    let p = PluginProcess::spawn(
        "com.example.sbx",
        &dir,
        &spec,
        "test",
        on_reverse,
        &EnvPolicy::default(),
        Some(&sbx),
    )
    .expect("spawn sandboxed");
    let r = p
        .call("sbx.probe", json!({}), Duration::from_secs(20))
        .expect("probe");
    assert_eq!(
        r["escape"], "denied",
        "writing to the temp dir must be denied by the sandbox: {:?}",
        r
    );
    assert_eq!(r["data"], "ok", "plugin-data writes must pass: {:?}", r);
    assert_eq!(
        r["tmp"], "ok",
        "TMPDIR is redirected into plugin-data, so tempfile should work: {:?}",
        r
    );
    p.shutdown();
    let _ = std::fs::remove_file(&escape_file);

    // ② Control group: no sandbox, can escape (proving the probe itself works)
    let on_reverse2: OnReverse = Arc::new(|_v: Value, _reply: Reply| {});
    let p2 = PluginProcess::spawn(
        "com.example.sbx",
        &dir,
        &spec,
        "test",
        on_reverse2,
        &EnvPolicy::default(),
        None,
    )
    .expect("spawn control");
    let r2 = p2
        .call("sbx.probe", json!({}), Duration::from_secs(20))
        .expect("probe");
    assert_eq!(
        r2["escape"], "ok",
        "the control group should be able to write: {:?}",
        r2
    );
    p2.shutdown();
    let _ = std::fs::remove_file(&escape_file);

    std::env::remove_var("OPENCAPX_PLUGIN_DATA_DIR");
    let _ = std::fs::remove_dir_all(&dir);
}
