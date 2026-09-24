//! Plugin child process: NDJSON JSON-RPC 2.0 over stdio. See docs/plugin-protocol.md.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct RuntimeSpec {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// S1 — child-process environment policy: with `isolate=true`, clear the host environment and refill only the whitelist + manifest env.
#[derive(Debug, Clone)]
pub struct EnvPolicy {
    pub isolate: bool,
    /// Incremental whitelist (setting `plugin_env_allowlist`, parsed from a comma-separated value and passed in).
    pub allow: Vec<String>,
}

/// S5b — sandbox-exec wrapper parameters (passed in when the caller decides they are needed; macOS only).
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    pub profile: String,
    /// review F2 — TMPDIR redirect target inside the sandbox (the plugin-data subdirectory; the host tmp is not on the write whitelist).
    pub tmp_dir: std::path::PathBuf,
}

impl Default for EnvPolicy {
    fn default() -> Self {
        Self {
            isolate: true,
            allow: Vec::new(),
        }
    }
}

/// Minimal environment whitelist: what a plugin needs to run (executable lookup / home / temp dir / timezone / locale).
/// Apart from these and the incremental whitelist, plugins cannot read arbitrary host environment variables by default (S1 breaking tightening;
/// `plugin_env_isolation=false` is a one-switch rollback).
const BASE_ENV_ALLOW: &[&str] = &["PATH", "HOME", "TMPDIR", "TZ", "LANG"];

/// Windows equivalents: python.exe (and anything on the CRT) aborts without SYSTEMROOT,
/// and temp/home resolve through TEMP/USERPROFILE rather than TMPDIR/HOME. Without these,
/// every process plugin on Windows died instantly with "timeout waiting for plugin.initialize"
/// while the same plugin ran fine under the unix allowlist.
#[cfg(windows)]
const PLATFORM_ENV_ALLOW: &[&str] = &[
    "SYSTEMROOT", "SYSTEMDRIVE", "COMSPEC", "WINDIR", "PATHEXT", "TEMP", "TMP", "USERPROFILE",
    "HOMEDRIVE", "HOMEPATH", "APPDATA", "LOCALAPPDATA",
];
#[cfg(not(windows))]
const PLATFORM_ENV_ALLOW: &[&str] = &[];

fn env_is_allowed(key: &str, extra: &[String]) -> bool {
    BASE_ENV_ALLOW.contains(&key)
        || PLATFORM_ENV_ALLOW.contains(&key)
        || key.starts_with("LC_")
        || key.starts_with("XDG_")
        || extra.iter().any(|k| k == key)
}

/// Reverse-reply handle: write one line of JSON to the plugin's stdin.
pub type Reply = Arc<dyn Fn(Value) + Send + Sync>;
/// Plugin reverse request/notification callback (value is the full request; when it has an id, respond with reply).
pub type OnReverse = Arc<dyn Fn(Value, Reply) + Send + Sync>;

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

fn write_raw(stdin: &Arc<Mutex<ChildStdin>>, v: &Value) -> Result<(), String> {
    let mut s = serde_json::to_string(v).map_err(|e| e.to_string())?;
    s.push('\n');
    let mut w = stdin.lock().map_err(|_| "stdin poisoned".to_string())?;
    w.write_all(s.as_bytes()).map_err(|e| e.to_string())
}

/// review F4 — trigger a log-rotation check every N stderr lines: a 24h interval is too coarse for chatty plugins,
/// which can write well over 10MB in a day. The counter is process-global; one stat per 4096 lines is negligible.
static LOG_LINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const LOG_ROTATE_EVERY: u64 = 4096;

/// P3 — plugin→core single-line cap 4 MiB: a runaway plugin must not OOM the core with one line
/// (the S4 input gate only closed the opposite direction; trace persistence benefits at the same point).
const MAX_PLUGIN_LINE_BYTES: usize = 4 * 1024 * 1024;
static DROPPED_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
enum LineOutcome {
    Line(Vec<u8>),
    /// Line over the limit: the rest of that line's bytes have been consumed; the caller counts it and continues to the next line.
    Dropped,
    Eof,
}

/// P3 — bounded line read: a whole line exceeding `cap` (excluding the newline) is dropped, and the stream advances to the next start.
/// Line outcome with CRLF normalized: a plugin's stderr on Windows ends every line with
/// `\r\n`, and the `\r` must not ride along into plugin.log events and log files.
fn finalize_line(mut buf: Vec<u8>) -> LineOutcome {
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    LineOutcome::Line(buf)
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

fn read_line_capped<R: std::io::BufRead>(r: &mut R, cap: usize) -> LineOutcome {
    let mut buf: Vec<u8> = Vec::new();
    let mut over = false;
    loop {
        let n = match r.fill_buf() {
            Ok(chunk) if chunk.is_empty() => {
                return match (over, buf.is_empty()) {
                    (true, _) => LineOutcome::Dropped,
                    (false, true) => LineOutcome::Eof,
                    (false, false) => finalize_line(buf),
                };
            }
            Ok(chunk) => {
                if let Some(pos) = chunk.iter().position(|b| *b == b'\n') {
                    if !over {
                        if buf.len() + pos > cap {
                            over = true;
                        } else {
                            buf.extend_from_slice(&chunk[..pos]);
                        }
                    }
                    r.consume(pos + 1);
                    return if over {
                        LineOutcome::Dropped
                    } else {
                        finalize_line(buf)
                    };
                }
                if over {
                    chunk.len()
                } else if buf.len() + chunk.len() > cap {
                    over = true;
                    buf.clear();
                    chunk.len()
                } else {
                    let m = chunk.len();
                    buf.extend_from_slice(chunk);
                    m
                }
            }
            Err(_) => return LineOutcome::Eof,
        };
        r.consume(n);
    }
}

fn log_line(plugin_id: &str, line: &str) {
    let dir = super::retention::logs_root();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{}.log", plugin_id)))
    {
        let _ = writeln!(f, "{}", line);
    }
    if LOG_LINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % LOG_ROTATE_EVERY == 0 {
        let _ = super::retention::rotate_logs();
    }
}

/// Push one stderr line to the EventBus (consumed live by /events SSE, /admin, and settings audit).
/// The same line is also written to ~/.opencapx/logs/plugins/<id>.log (already done by log_line).
fn publish_stderr_log(plugin_id: &str, line: &str) {
    use serde_json::json;
    super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
        "plugin.log",
        &format!("plugin:{}", plugin_id),
        json!({ "pluginId": plugin_id, "level": "info", "source": "stderr", "message": line }),
    ));
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
        let stdin = child.stdin.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "no stdin")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "no stdout")
        })?;
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
                            if !super::plugin::reverse_allow(&pid) {
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
                        let n = DROPPED_FRAMES
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                            + 1;
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
                super::plugin_trace::record(
                    &trace_pid,
                    &trace_sid,
                    super::plugin_trace::Direction::In,
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
        let (tx, rx): (SyncSender<Value>, Receiver<Value>) =
            std::sync::mpsc::sync_channel(1);
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
        super::plugin_trace::record(
            &self.plugin_id,
            &self.session_id,
            super::plugin_trace::Direction::Out,
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
        super::plugin_trace::record(
            &self.plugin_id,
            &self.session_id,
            super::plugin_trace::Direction::Out,
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
            Ok(v) => v
                .get("ok")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
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
        let p = PluginProcess::spawn("com.opencapx.echo-vision", &echo_root(), &spec, "test", on_reverse, &EnvPolicy::default(), None)
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

        let ping = p.call("plugin.ping", json!({}), Duration::from_secs(5)).expect("ping");
        assert_eq!(ping["ok"], json!(true));

        let out = p
            .call(
                "image.analyze",
                json!({"image":"/tmp/test.png"}),
                Duration::from_secs(10),
            )
            .expect("analyze");
        assert!(out["description"].as_str().unwrap().contains("/tmp/test.png"));

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
        let p = PluginProcess::spawn("com.opencapx.echo-vision", &echo_root(), &spec, "test", on_reverse, &EnvPolicy::default(), None)
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
        let bus = super::super::event::EventBus::shared();
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
        let _ = PluginProcess::spawn("com.opencapx.stderr-test", &echo_root(), &spec, "test", on_reverse, &EnvPolicy::default(), None)
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
        assert_eq!(read_line_capped(&mut r, 16), LineOutcome::Line(b"ok-1".to_vec()));
        assert_eq!(read_line_capped(&mut r, 16), LineOutcome::Dropped);
        assert_eq!(read_line_capped(&mut r, 16), LineOutcome::Line(b"ok-2".to_vec()));
        assert_eq!(read_line_capped(&mut r, 16), LineOutcome::Eof);
    }

    /// CRLF: Windows plugin stderr is `\r\n`-terminated; the trailing `\r` must be stripped
    /// (it used to ride into every published plugin.log message and break exact matching).
    #[test]
    fn read_line_capped_strips_crlf_carriage_return() {
        let mut r = std::io::BufReader::new(&b"a\r\nb\nno-eol\r"[..]);
        assert_eq!(read_line_capped(&mut r, 16), LineOutcome::Line(b"a".to_vec()));
        assert_eq!(read_line_capped(&mut r, 16), LineOutcome::Line(b"b".to_vec()));
        assert_eq!(read_line_capped(&mut r, 16), LineOutcome::Line(b"no-eol".to_vec()));
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
        let p2 = PluginProcess::spawn("com.opencapx.env2", &dir, &spec2, "test", on_reverse2, &policy, None)
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
        let rx = super::super::event::EventBus::shared().subscribe();
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
                        && ev.payload.get("pluginId").and_then(|v| v.as_str())
                            == Some(pid.as_str())
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
        let escape_file = std::env::temp_dir().join(format!(
            "opencapx-sbx-escape-{}.txt",
            std::process::id()
        ));

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
        let decl = super::super::plugin::SandboxDecl {
            fs: Some(super::super::plugin::SandboxFs {
                write: vec!["plugin-data".to_string()],
            }),
            network: Some("none".to_string()),
        };
        let profile =
            super::super::sandbox::sandbox_profile("com.example.sbx", &decl);
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
        assert_eq!(r["escape"], "denied", "writing to the temp dir must be denied by the sandbox: {:?}", r);
        assert_eq!(r["data"], "ok", "plugin-data writes must pass: {:?}", r);
        assert_eq!(r["tmp"], "ok", "TMPDIR is redirected into plugin-data, so tempfile should work: {:?}", r);
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
        assert_eq!(r2["escape"], "ok", "the control group should be able to write: {:?}", r2);
        p2.shutdown();
        let _ = std::fs::remove_file(&escape_file);

        std::env::remove_var("OPENCAPX_PLUGIN_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
