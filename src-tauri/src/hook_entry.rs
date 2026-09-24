//! the stdin hook pipeline: pre-tool rewrite host, danger guard, session-start, wrap/run entry, and the rewrite CLI.
//! Mechanical move from main.rs.

use super::*;

#[tauri::command]
pub(crate) fn ui_ping(count: usize, last_state: String) {
    eprintln!("[ui-ping] frontend sessions={} last={}", count, last_state);
}

pub(crate) fn cli_flag(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

/// /event uplink: Sent passes through (returning the response body — SessionStart carries
/// the additionalContext digest); Unreachable (app not running) silently queues locally,
/// drained in-process on next app start; Rejected (token invalid/revoked) queues and also
/// warns once about the recovery path (every hook event goes through here; only warns the first time per process).
pub(crate) fn deliver_event(
    payload: &str,
    creds: Option<&crate::core::identity::Credentials>,
    kind: &str,
) -> (http::Deliver, Option<String>) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let (mut deliver, mut body) = http::post_event_with_body(payload, creds);
    // token invalid (40101, not revoked) → void the local token, re-register via TOFU, and deliver again.
    // so rotation/invalidation no longer requires manually deleting the token file; revoked (40102) deliberately skips this — revocation is the
    // user's deliberate decision and can only be lifted by reauthorizing on the settings page.
    if matches!(deliver, http::Deliver::RejectedBadToken)
        && crate::core::identity::reset_credentials(kind)
    {
        let fresh = crate::core::identity::ensure_registered(kind, "hook");
        let (d, b) = http::post_event_with_body(payload, fresh.as_ref());
        deliver = d;
        body = b;
    }
    match deliver {
        http::Deliver::Sent => {}
        http::Deliver::RejectedBadToken | http::Deliver::Rejected => {
            let _ = WARNED.set(());
            eprintln!(
                "OpenCapX: token rejected (agent revoked, or rotation self-heal failed). \
                 Events are queued locally. To recover: OpenCapX Settings -> Agents -> \
                 Reauthorize, then update the token file in ~/.opencapx/agent-tokens/ and rerun."
            );
            let _ = queue::enqueue(&http::queue_dir(), payload);
        }
        http::Deliver::Unreachable => {
            let _ = queue::enqueue(&http::queue_dir(), payload);
        }
    }
    (deliver, body)
}

/// `opencapx connect <claude|codex|opencode>`: installs hooks + writes `opencapx mcp`
/// into that agent's MCP config (idempotent; no credentials in the config, mcp handles TOFU itself at startup).
/// Settings → General: the global `opencapx` command state.
#[tauri::command]
pub(crate) fn cli_command_status() -> cli_install::Status {
    cli_install::status()
}

/// Settings → General: install the global command (macOS: authorization dialog when needed).
/// Rebuilds the tray menu so its install item appears/disappears with the state.
#[tauri::command]
pub(crate) fn cli_command_install(app: tauri::AppHandle) -> Result<String, String> {
    let out = cli_install::install(true);
    let _ = refresh_tray_menu(&app);
    out
}

/// Settings → General: remove the global command.
#[tauri::command]
pub(crate) fn cli_command_uninstall(app: tauri::AppHandle) -> Result<String, String> {
    let out = cli_install::uninstall(true);
    let _ = refresh_tray_menu(&app);
    out
}

/// Extract the working directory from the raw payload (the agent's cwd, used to locate project-level rules).
pub(crate) fn payload_cwd(payload: &str) -> Option<std::path::PathBuf> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("cwd")?
        .as_str()
        .map(std::path::PathBuf::from)
}

/// When the payload carries a host marker, trust it as the source.
///
/// opencode's native hook payload carries `hook_source: "opencode-plugin"`; but some third-party
/// forwarders hardcode `--agent claude`, mis-recording opencode traffic as claude (both telemetry and audit get
/// polluted). The payload has more say about "who am I" than the caller, so it overrides --agent here.
/// Only an explicit opencode declaration is honored; claude/codex and other normal paths are unaffected.
pub(crate) fn payload_host(payload: &str) -> Option<&'static str> {
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    let src = v.get("hook_source")?.as_str()?;
    if src.eq_ignore_ascii_case("opencode-plugin") {
        Some("opencode")
    } else {
        None
    }
}

/// Whether this hook should write back to the host (pure function, no IO).
///
/// Returns `Some` only when the agent supports PreToolUse's `updatedInput`, the event is PreToolUse, and the command matches
/// a rule; otherwise always `None` — stay a dumb pipe (zero stdout). The accompanying
/// `permissionDecision: "allow"` is decision D1: rewriting means allowing.
pub(crate) fn hook_response(
    payload: &str,
    agent: &str,
    set: &crate::core::rules::RuleSet,
) -> Option<String> {
    hook_decision(payload, agent, set).map(|d| d.response)
}

pub(crate) struct HookDecision {
    pub(crate) rule_id: String,
    pub(crate) response: String,
}

/// Whether this host "actually applies command rewrites", and whether it writes back via stdout.
///
/// - `claude` / `codex` / `gemini`: the host honors stdout's `updatedInput` / `tool_input` → write back + record audit.
/// - `droid` / `copilot`: the Claude shape (`hookSpecificOutput.updatedInput`); copilot must be configured with the
///   PascalCase `PreToolUse` event for the VS Code-compatible payload that honors `updatedInput`.
/// - `omp`: the host itself ignores stdout, but the extension `connect` installs reads it and returns the
///   rewrite as `{ input }`, applied before the host's approval gate → write back + record audit.
/// - `cursor`: the top-level snake_case envelope (`permission` + `updated_input`), see `pre_tool_response`.
/// - `opencode`: the host does **not** honor stdout (see the opencode_plugin notes), but OpenCapX's own
///   opencode plugin does the rewrite via `rewrite` + in-place args editing → counts as applied, must be audited, but no writeback.
/// - everything else: neither rewrite nor audit (`None`).
pub(crate) fn rewrite_host(agent: &str) -> Option<bool> {
    match agent {
        "claude" | "codex" => Some(true),
        // gemini's hook **config** format shares its origin with Claude (`gemini hooks migrate` only migrates config),
        // but the **response** fields differ: it reads hookSpecificOutput.tool_input, see pre_tool_response.
        "gemini" => Some(true),
        "omp" => Some(true),
        "droid" | "copilot" => Some(true),
        "cursor" => Some(true),
        "opencode" => Some(false),
        _ => None,
    }
}

/// Each host's "pre-execution" event name (case-insensitive): Claude family uses PreToolUse, gemini uses BeforeTool.
pub(crate) fn is_pre_tool_event(name: &str) -> bool {
    name.eq_ignore_ascii_case("PreToolUse") || name.eq_ignore_ascii_case("BeforeTool")
}

/// Cursor's permission hooks answer with JSON on every invocation: with no rewrite and no audit-only
/// hit, print `{}` (valid JSON, no fields → no decision) instead of staying silent. Other hosts keep
/// the historical zero-stdout behavior.
pub(crate) fn cursor_noop_stdout(agent: &str, payload: &str) -> Option<&'static str> {
    if !agent.eq_ignore_ascii_case("cursor") {
        return None;
    }
    let event = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let name = event.get("hook_event_name")?.as_str()?;
    if is_pre_tool_event(name) {
        Some("{}")
    } else {
        None
    }
}

/// Build the "pre-execution rewrite" response body per host — the field names differ, so they cannot share one.
///
/// - `claude` / `codex` / `droid` / `copilot`:`hookSpecificOutput.updatedInput` + `permissionDecision`。
///   (copilot's rewritten commands still pass its own confirmation dialog — upstream github/copilot-cli#2643.)
/// - `gemini`: reads `hookSpecificOutput.tool_input` (**snake_case**, does not recognize `updatedInput`),
///   `hookEventName` is `BeforeTool`. See gemini-cli `docs/hooks/reference.md`.
/// - `omp`: the Claude shape; the installed OMP extension reads `updatedInput.command`
///   (it ignores `permissionDecision` — OMP's own approval gate decides).
/// - `cursor`: the top-level snake_case envelope (`continue` + `permission` + `updated_input`) — Cursor's
///   `preToolUse` does not read `hookSpecificOutput`.
pub(crate) fn pre_tool_response(
    agent: &str,
    rule_id: &str,
    tool_input: &serde_json::Value,
) -> String {
    if agent.eq_ignore_ascii_case("cursor") {
        serde_json::json!({
            "continue": true,
            "permission": "allow",
            "updated_input": tool_input,
        })
        .to_string()
    } else if agent.eq_ignore_ascii_case("gemini") {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "BeforeTool",
                "tool_input": tool_input,
            }
        })
        .to_string()
    } else {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": format!("OpenCapX rule {rule_id}"),
                "updatedInput": tool_input,
            }
        })
        .to_string()
    }
}

pub(crate) fn hook_decision(
    payload: &str,
    agent: &str,
    set: &crate::core::rules::RuleSet,
) -> Option<HookDecision> {
    let emit_stdout = rewrite_host(agent)?;
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    let event = v.get("hook_event_name")?.as_str()?;
    if !is_pre_tool_event(event) {
        return None;
    }
    let mut tool_input = v.get("tool_input")?.clone();
    let cmd = tool_input.get("command")?.as_str()?;
    let crate::core::rules::RewriteOutcome::Rewritten { rule_id, command } =
        combined_rewrite(cmd, set)
    else {
        return None;
    };
    // Audit-only hits (command unchanged): record the shape for the Timeline but write
    // nothing to stdout — emitting `permissionDecision: allow` here would auto-approve
    // exactly the shapes the guard itself could not make safe, bypassing the host's own
    // permission prompt. Trusted-passthrough is the one exception: the user explicitly
    // vouched for that host, so the allow is the point.
    if command == cmd && rule_id != "danger/trusted-passthrough" {
        return Some(HookDecision {
            rule_id,
            response: String::new(),
        });
    }
    if !emit_stdout {
        // no rewrite response, but still report rule_id for audit (opencode does the rewrite via its own plugin).
        return Some(HookDecision {
            rule_id,
            response: String::new(),
        });
    }
    tool_input["command"] = serde_json::Value::String(command);
    let response = pre_tool_response(agent, &rule_id, &tool_input);
    Some(HookDecision { rule_id, response })
}

pub(crate) fn run_hook(args: &[String]) -> ! {
    use std::io::Read;
    // Self-heal the stable CLI: when invoked from a path other than the shim (dev binary,
    // freshly installed app), refresh ~/.opencapx/bin/opencapx so configs keep working.
    // Typically one stat; a no-op when this process IS the shim.
    let _ = hooks::ensure_shim();
    let agent = cli_flag(args, "--agent").unwrap_or_else(|| "auto".into());
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    if stdin.trim().is_empty() {
        if let Some(event) = cli_flag(args, "--event") {
            let text = match event.as_str() {
                "working" => "running",
                "done" => "done",
                "registered" => "session start",
                other => other,
            };
            let session = cli_flag(args, "--session").unwrap_or_default();
            let project = cli_flag(args, "--project").unwrap_or_default();
            stdin = format!(
                "{{\"agent\":{},\"text\":{},\"session_id\":{},\"project\":{}}}",
                serde_json::to_string(&agent).unwrap_or_default(),
                serde_json::to_string(&text).unwrap_or_default(),
                serde_json::to_string(&session).unwrap_or_default(),
                serde_json::to_string(&project).unwrap_or_default(),
            );
        }
    }
    let (parsed_agent, _) = http::parse_hook_payload(&stdin);
    // when the payload carries its own source (opencode native marker) it overrides --agent: see payload_host.
    let host = payload_host(&stdin);
    let agent = match host {
        Some(h) => h.to_string(),
        None if agent.is_empty() || agent == "auto" => parsed_agent,
        None => agent,
    };
    // Agent identity (docs/permissions.md "Registration (TOFU)"): --agent first, otherwise host-sniffed env.
    // Core not running → None; /event failures are queued locally by deliver_event + warned by reason.
    let kind = match host {
        Some(h) => h.to_string(),
        None => match cli_flag(args, "--agent") {
            Some(k) if !k.is_empty() && k != "auto" => k,
            _ => crate::core::identity::detect_kind(),
        },
    };
    let creds = crate::core::identity::ensure_registered(&kind, "hook");
    // send the raw payload (injecting only agent / send time / matched rule id): parsing and enrichment
    // (transcript/model) all happen on the Core side; the CLI is a dumb pipe.
    // see crate::core::agent::stamp_payload_annotated.
    let cwd = payload_cwd(&stdin).or_else(|| std::env::current_dir().ok());
    let set = crate::core::rules::load(cwd.as_deref());
    let decision = hook_decision(&stdin, &agent, &set);
    let payload = crate::core::agent::stamp_payload_annotated(
        &stdin,
        &agent,
        crate::core::agent::now_secs(),
        decision.as_ref().map(|d| d.rule_id.as_str()),
    );
    let (deliver, resp_body) = deliver_event(&payload, creds.as_ref(), &kind);
    // Two stdout paths, at most one fires:
    // 1. PreToolUse rewrite (empty response = matched but this host does not write back via stdout: audit only, no output);
    // 2. SessionStart injection — when Core attached a capability digest and this host merges
    //    stdout additionalContext, surface it so the agent knows OpenCapX's abilities without
    //    calling list_capabilities first. Everything else stays zero-stdout (dumb pipe).
    let mut out = decision
        .filter(|d| !d.response.is_empty())
        .map(|d| d.response);
    if out.is_none() {
        out = cursor_noop_stdout(&agent, &stdin).map(str::to_string);
    }
    if out.is_none()
        && matches!(deliver, http::Deliver::Sent)
        && http::session_context_host(&agent)
        && is_session_start(&stdin)
    {
        if let Some(ctx) = resp_body
            .as_deref()
            .and_then(|b| serde_json::from_str::<serde_json::Value>(b).ok())
            .and_then(|v| {
                v.get("additionalContext")
                    .and_then(|c| c.as_str())
                    .map(String::from)
            })
        {
            out = Some(session_start_response(&agent, &ctx));
        }
    }
    if let Some(o) = out {
        println!("{}", o);
    }
    std::process::exit(0);
}

/// Claude-payload shape: the hook_event_name field says which host event fired.
/// Case-insensitive — cursor sends "sessionStart" (not injected today, but the check stays cheap).
pub(crate) fn is_session_start(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| {
            v.get("hook_event_name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .map(|n| n.eq_ignore_ascii_case("SessionStart"))
        .unwrap_or(false)
}

/// SessionStart stdout writeback. v1: the Claude-nested family (claude/codex/droid/grok — see
/// http::session_context_host) all speak the Claude shape; diverge per-host here the day one
/// differs, mirroring pre_tool_response.
pub(crate) fn session_start_response(_agent: &str, ctx: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": ctx,
        }
    })
    .to_string()
}

/// Rewrite pipeline: user rules first (they win), then the built-in danger guard — the only
/// path that can rewrite compound pipelines, since the rule engine refuses them by design.
/// Both the hook path and `opencapx rewrite` (used by the opencode plugin) go through here.
pub(crate) fn combined_rewrite(
    cmd: &str,
    set: &crate::core::rules::RuleSet,
) -> crate::core::rules::RewriteOutcome {
    use crate::core::rules::{rewrite_command, RewriteOutcome, Stage};
    match rewrite_command(cmd, Stage::ToolPre, set) {
        r @ RewriteOutcome::Rewritten { .. } => r,
        RewriteOutcome::Unchanged => {
            // Stance: `OPEN_CAPX_DANGER_GUARD` (explicit per-invocation) over the durable
            // `opencapx guard mode` field in guard.json, default installer (installers need
            // the network and $HOME; strict is the network-denied fence).
            let settings = crate::core::sandbox::resolve_guard_settings();
            // The guard's rewrite must call the stable shim (same rule as hook/MCP entries).
            let bin = hooks::ensure_shim()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "opencapx".into());
            if !settings.enabled {
                // Disabled (env var or guard.json): the idiom must not become invisible —
                // audit it as `danger/guard-disabled` with the command unchanged. The env
                // var in particular is the same injection surface that made project rules
                // trust-gated, so its effect stays on the record.
                return match crate::core::sandbox::guard(
                    cmd,
                    &bin,
                    settings.profile,
                    settings.env,
                    settings.require,
                ) {
                    Some(_) => RewriteOutcome::Rewritten {
                        rule_id: "danger/guard-disabled".to_string(),
                        command: cmd.to_string(),
                    },
                    None => RewriteOutcome::Unchanged,
                };
            }
            match crate::core::sandbox::guard(
                cmd,
                &bin,
                settings.profile,
                settings.env,
                settings.require,
            ) {
                Some(hit) => RewriteOutcome::Rewritten {
                    rule_id: hit.rule_id.to_string(),
                    command: hit.command,
                },
                None => RewriteOutcome::Unchanged,
            }
        }
    }
}

/// `opencapx rewrite <command...>`: map a single command to its rule-rewritten form.
/// Match → print to stdout + `exit 0`; no match → `exit 1` (no output).
/// **Pure computation, does not execute the command** — execution is the caller's (agent's) responsibility.
pub(crate) fn run_rewrite(cmd: &str) -> ! {
    let set = crate::core::rules::load(std::env::current_dir().ok().as_deref());
    match combined_rewrite(cmd, &set) {
        crate::core::rules::RewriteOutcome::Rewritten { command, .. } => {
            println!("{command}");
            std::process::exit(0);
        }
        crate::core::rules::RewriteOutcome::Unchanged => std::process::exit(1),
    }
}

pub(crate) fn run_wrap(args: &[String]) -> ! {
    let dash = args.iter().position(|a| a == "--");
    let cmd: Vec<String> = match dash {
        Some(i) => args[i + 1..].to_vec(),
        None => args.to_vec(),
    };
    if cmd.is_empty() {
        eprintln!("OpenCapX run: missing command after --");
        std::process::exit(2);
    }
    let start = serde_json::json!({"agent": "run", "text": format!("running {}", cmd.join(" "))});
    let run_kind = crate::core::identity::detect_kind();
    let creds = crate::core::identity::ensure_registered(&run_kind, "hook");
    deliver_event(
        &crate::core::agent::stamp_payload(
            &start.to_string(),
            "run",
            crate::core::agent::now_secs(),
        ),
        creds.as_ref(),
        &run_kind,
    );
    let mut child = match std::process::Command::new(&cmd[0]).args(&cmd[1..]).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("OpenCapX run: spawn failed: {}", e);
            std::process::exit(127);
        }
    };
    let status = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
    let done = serde_json::json!({"agent": "run", "text": "done"}).to_string();
    deliver_event(
        &crate::core::agent::stamp_payload(&done, "run", crate::core::agent::now_secs()),
        creds.as_ref(),
        &run_kind,
    );
    std::process::exit(status);
}
