//! PluginProcess: the child handle, command resolution, and the spawn/request lifecycle.
//! Mechanical move from core/process.rs.

use super::*;

pub struct PluginProcess {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<i64, SyncSender<Value>>>>,
    next_id: Mutex<i64>,
    /// Wow 9 — one session id per launch (a start_secs value), corresponding to ~/.opencapx/traces/<id>/<session>.ndjson.
    /// Passed in by the caller at spawn time; every JSON-RPC frame is traced during the run.
    session_id: String,
    #[allow(dead_code)]
    plugin_id: String,
}

/// Windows python3 trap: `python3.exe` there is a Microsoft Store alias that exits without
/// running any Python. Plugin manifests declare `"command": "python3"` (the portable unix
/// name), so when the literal command cannot actually run, fall back to `python` before
/// giving up. Unix keeps the exact command the manifest declared.
fn resolve_command(cmd: &str) -> String {
    if cmd != "python3" || cfg!(not(target_os = "windows")) {
        return cmd.to_string();
    }
    let runs = |c: &str| {
        std::process::Command::new(c)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if runs("python3") {
        cmd.to_string()
    } else if runs("python") {
        "python".to_string()
    } else {
        cmd.to_string()
    }
}

impl PluginProcess {
    pub fn spawn(
        plugin_id: &str,
        plugin_root: &std::path::Path,
        spec: &RuntimeSpec,
        session_id: &str,
        on_reverse: OnReverse,
        env_policy: &EnvPolicy,
        sandbox: Option<&SandboxSpec>,
    ) -> std::io::Result<Self> {
        // S5b — macOS: when a sandbox is declared, wrap via sandbox-exec (process_group stays on the outer layer, group-kill semantics unchanged)
        #[cfg(target_os = "macos")]
        let (spawn_cmd, spawn_args) = match sandbox {
            Some(sb) => {
                let mut args: Vec<String> = vec![
                    "-p".into(),
                    sb.profile.clone(),
                    "--".into(),
                    spec.command.clone(),
                ];
                args.extend(spec.args.iter().cloned());
                ("sandbox-exec".to_string(), args)
            }
            None => (spec.command.clone(), spec.args.clone()),
        };
        #[cfg(not(target_os = "macos"))]
        let (spawn_cmd, spawn_args) = {
            let _ = sandbox; // enforcement layer out of scope (Linux bubblewrap / Windows AppContainer = roadmap)
            (resolve_command(&spec.command), spec.args.clone())
        };
        let mut cmd = Command::new(&spawn_cmd);
        cmd.args(&spawn_args)
            .current_dir(plugin_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // S1 — process group: the plugin forms its own group (pgid == leader pid); when shutdown drags on, grandchildren are reaped by group.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        // S1 — environment isolation (on by default): env_clear, then refill only the whitelist, then layer manifest env (author's static declaration).
        if env_policy.isolate {
            cmd.env_clear();
            for (k, v) in std::env::vars() {
                if env_is_allowed(&k, &env_policy.allow) {
                    cmd.env(k, v);
                }
            }
        }
        cmd.envs(&spec.env);
        // review F2 — under the sandbox, TMPDIR is redirected into plugin-data: the host tmp is not on the write whitelist,
        // so without the redirect every tempfile hits EPERM (a soak test is bound to hit it); overrides the manifest-declared value (sandbox semantics win).
        #[cfg(target_os = "macos")]
        if let Some(sb) = sandbox {
            cmd.env("TMPDIR", &sb.tmp_dir);
        }
        // Python plugins: `opencapx_sdk` lives in this repo at packages/plugin-sdk/.
        // In production with `pip install -e` this hack is unnecessary; in dev/test we directly
        // stuff the SDK path into PYTHONPATH, so plugin scripts need not care where they themselves are installed
        // (tests often unpack the plugin into /tmp/.../plugins/<id>/).
        let cmd_name = spec.command.rsplit(['/', '\\']).next().unwrap_or("");
        if cmd_name.starts_with("python") || cmd_name == "python3" || cmd_name == "py" {
            let sdk_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("packages")
                .join("plugin-sdk");
            if sdk_path.join("opencapx_sdk").join("__init__.py").is_file() {
                let existing = spec.env.get("PYTHONPATH").cloned().unwrap_or_else(|| {
                    // Without isolation, inherit the host PYTHONPATH; with isolation it has been cleared and is not revived.
                    if env_policy.isolate {
                        String::new()
                    } else {
                        std::env::var("PYTHONPATH").unwrap_or_default()
                    }
                });
                let sep = if existing.is_empty() { "" } else { ":" };
                cmd.env(
                    "PYTHONPATH",
                    format!("{}{}{}", sdk_path.display(), sep, existing),
                );
            }
        }
        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no stdout"))?;
        let stdin: Arc<Mutex<ChildStdin>> = Arc::new(Mutex::new(stdin));

        // stderr → plugin log file + EventBus (consumed live by SSE/admin/settings audit)
        if let Some(stderr) = child.stderr.take() {
            let pid = plugin_id.to_string();
            std::thread::spawn(move || {
                let mut r = BufReader::new(stderr);
                loop {
                    match read_line_capped(&mut r, MAX_PLUGIN_LINE_BYTES) {
                        LineOutcome::Eof => break,
                        LineOutcome::Dropped => {
                            let n = DROPPED_FRAMES
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                + 1;
                            if n == 1 || n % 100 == 0 {
                                log_line(
                                    &pid,
                                    &format!("[core] dropped oversized stderr line (#{})", n),
                                );
                            }
                        }
                        LineOutcome::Line(bytes) => {
                            // O2 — stderr and reverse share a per-plugin token bucket: a chatty plugin must not
                            // bypass S3 to flood the EventBus/DB/log; over-limit lines are dropped directly (summarized via plugin.throttled).
                            if !crate::core::plugin::reverse_allow(&pid) {
                                continue;
                            }
                            let line = String::from_utf8_lossy(&bytes);
                            log_line(&pid, &line);
                            publish_stderr_log(&pid, &line);
                        }
                    }
                }
            });
        }

        let pending: Arc<Mutex<HashMap<i64, SyncSender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let reply_stdin = stdin.clone();
        let reply: Reply = Arc::new(move |v: Value| {
            let _ = write_raw(&reply_stdin, &v);
        });
        let trace_pid = plugin_id.to_string();
        let trace_sid = session_id.to_string();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let bytes = match read_line_capped(&mut r, MAX_PLUGIN_LINE_BYTES) {
                    LineOutcome::Eof => break,
                    LineOutcome::Dropped => {
                        let n =
                            DROPPED_FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if n == 1 || n % 100 == 0 {
                            log_line(
                                &trace_pid,
                                &format!("[core] dropped oversized plugin frame (#{})", n),
                            );
                        }
                        continue;
                    }
                    LineOutcome::Line(b) => b,
                };
                let line = String::from_utf8_lossy(&bytes);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // Wow 9: trace every incoming JSON-RPC frame (in).
                crate::core::plugin_trace::record(
                    &trace_pid,
                    &trace_sid,
                    crate::core::plugin_trace::Direction::In,
                    &v,
                );
                if let Some(id) = v.get("id").and_then(|i| i.as_i64()) {
                    // Response to a request initiated by the Core (id is numeric)
                    if v.get("result").is_some() || v.get("error").is_some() {
                        if let Ok(mut m) = reader_pending.lock() {
                            if let Some(tx) = m.remove(&id) {
                                let _ = tx.send(v);
                            }
                        }
                        continue;
                    }
                }
                if v.get("method").is_some() {
                    on_reverse(v, reply.clone());
                }
            }
            // stdout closed: clear pending so waiters fail immediately
            if let Ok(mut m) = reader_pending.lock() {
                m.clear();
            }
        });

        Ok(Self {
            child: Mutex::new(child),
            stdin,
            pending,
            next_id: Mutex::new(1),
            session_id: session_id.to_string(),
            plugin_id: plugin_id.to_string(),
        })
    }

    fn write_line(&self, v: &Value) -> Result<(), String> {
        write_raw(&self.stdin, v)
    }

    /// Send a request and wait for the response.
    /// trace correlation key: req_trace's plugin span uses it to point at plugin_trace's frame dump.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let (tx, rx): (SyncSender<Value>, Receiver<Value>) = std::sync::mpsc::sync_channel(1);
        let id = {
            let mut n = self.next_id.lock().map_err(|_| "poisoned".to_string())?;
            let v = *n;
            *n += 1;
            v
        };
        {
            let mut m = self.pending.lock().map_err(|_| "poisoned".to_string())?;
            m.insert(id, tx);
        }
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        // Wow 9: trace the core->plugin request frame (out).
        crate::core::plugin_trace::record(
            &self.plugin_id,
            &self.session_id,
            crate::core::plugin_trace::Direction::Out,
            &frame,
        );
        self.write_line(&frame)?;
        let resp = match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(_) => {
                // Timeout or process exit: clear the pending slot
                if let Ok(mut m) = self.pending.lock() {
                    m.remove(&id);
                }
                return Err(format!("timeout waiting for {}", method));
            }
        };
        if let Some(err) = resp.get("error") {
            return Err(format!(
                "plugin error {}: {}",
                err.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
                err.get("message").and_then(|m| m.as_str()).unwrap_or("")
            ));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Send a notification (no wait).
    pub fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let frame = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        crate::core::plugin_trace::record(
            &self.plugin_id,
            &self.session_id,
            crate::core::plugin_trace::Direction::Out,
            &frame,
        );
        self.write_line(&frame)
    }

    /// plugin.shutdown notification + 3s grace + group-kill wrap-up.
    /// Takes `&self`: the caller may only hold a clone of `Arc<PluginProcess>` (an in-flight capability
    /// call / watchdog) and does not need exclusive ownership — the earlier `shutdown(self)` forced
    /// `PluginManager::stop` to use `Arc::try_unwrap(..).unwrap()`, assuming exclusivity,
    /// which panicked when concurrent references were held.
    pub fn shutdown(&self) {
        let _ = self.notify("plugin.shutdown", json!({}));
        let mut child = match self.child.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        for _ in 0..30 {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => return,
            }
        }
        // S1 — group-kill wrap-up: whether the exit is normal or timed out, clean up remaining process-group members (grandchildren).
        // spawn already did process_group(0) (pgid == leader pid); with the leader already exited, the group kill
        // still applies to living group members (ESRCH is harmless). Defense: never target our own process group.
        Self::kill_process_group(child.id());
        let _ = child.kill(); // fallback
        let _ = child.wait();
    }

    /// Kill the whole process group (SIGKILL; ESRCH is idempotent). Windows uses taskkill /T.
    fn kill_process_group(leader_pid: u32) {
        #[cfg(unix)]
        {
            let pgid = leader_pid as i32;
            if pgid > 1 && pgid != unsafe { libc::getpgrp() } {
                // review F3 — first probe whether the group exists: if the leader has been reaped with no remaining members → the group id is released,
                // and the pid may be reused by the OS for a new process (collision with a new group = wrongful kill). ESRCH → skip the kill.
                // While the group exists (has remaining members), that pgid value is held by the kernel and cannot be reused → the kill is guaranteed safe.
                if unsafe { libc::kill(-pgid, 0) } == 0 {
                    unsafe {
                        libc::kill(-pgid, libc::SIGKILL);
                    }
                }
            }
        }
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &leader_pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = leader_pid;
        }
    }

    pub fn is_alive(&self) -> bool {
        match self.child.lock() {
            Ok(mut c) => matches!(c.try_wait(), Ok(None)),
            Err(_) => false,
        }
    }

    /// Phase 45 — return the child's OS pid (for the metrics module to read /proc or ps). None if the process has exited.
    pub fn pid(&self) -> Option<u32> {
        match self.child.lock() {
            Ok(mut c) => {
                if c.try_wait().ok().flatten().is_some() {
                    None
                } else {
                    Some(c.id())
                }
            }
            Err(_) => None,
        }
    }

    /// Phase 40 — lightweight ping for the watchdog (via `plugin.ping` JSON-RPC; returns false on timeout).
    /// The SDK-side Plugin base class's default `on_ping` always replies `{"ok": true}`, so a plugin
    /// always pings through even without a custom implementation; a custom one can emit deeper health signals.
    pub fn ping(&self, timeout: std::time::Duration) -> bool {
        match self.call("plugin.ping", serde_json::json!({}), timeout) {
            Ok(v) => v.get("ok").and_then(|x| x.as_bool()).unwrap_or(true),
            Err(_) => false,
        }
    }
}
