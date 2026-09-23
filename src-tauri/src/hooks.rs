//! Writes/removes OpenCapX's hook entries in each agent's config
//! (~/.claude/settings.json, ...). Entries are identified by their command
//! string so install is idempotent and foreign hooks are never touched.

use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Serialize, Clone)]
pub struct AgentInfo {
    pub kind: String,
    pub display_name: String,
    pub installed: bool,
    pub note: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Style {
    ClaudeNested,
    CursorFlat,
    WindsurfFlat,
    KiroFlat,
    AntigravityNested,
    OpencodePluginModule,
    PiExtension,
    OmpExtension,
}

struct Spec {
    style: Style,
    rel_path: &'static [&'static str],
    events: &'static [&'static str],
}

fn spec(kind: &str) -> Option<Spec> {
    Some(match kind {
        "claude" => Spec { style: Style::ClaudeNested, rel_path: &[".claude", "settings.json"],
            events: &["SessionStart", "UserPromptSubmit", "PreToolUse", "Notification", "Stop", "SubagentStop", "SessionEnd"] },
        "codex" => Spec { style: Style::ClaudeNested, rel_path: &[".codex", "hooks.json"],
            events: &["SessionStart", "UserPromptSubmit", "PreToolUse", "PermissionRequest", "Stop", "SubagentStop"] },
        "gemini" => Spec { style: Style::ClaudeNested, rel_path: &[".gemini", "settings.json"],
            events: &["SessionStart", "BeforeAgent", "BeforeTool", "AfterTool", "Notification", "AfterAgent", "SessionEnd"] },
        "cursor" => Spec { style: Style::CursorFlat, rel_path: &[".cursor", "hooks.json"],
            events: &["sessionStart", "beforeSubmitPrompt", "preToolUse", "stop", "subagentStop", "sessionEnd"] },
        "copilot" => Spec { style: Style::CursorFlat, rel_path: &[".copilot", "hooks", "opencapx.json"],
            events: &["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"] },
        "windsurf" => Spec { style: Style::WindsurfFlat, rel_path: &[".codeium", "windsurf", "hooks.json"],
            events: &["pre_user_prompt", "post_cascade_response"] },
        "antigravity" => Spec { style: Style::AntigravityNested, rel_path: &[".gemini", "config", "hooks.json"],
            events: &["PreInvocation", "PreToolUse", "PostToolUse", "Stop"] },
        "kiro" => Spec { style: Style::KiroFlat, rel_path: &[".kiro", "agents", "default.json"],
            events: &["agentSpawn", "userPromptSubmit", "postToolUse", "stop"] },
        "opencode" => Spec { style: Style::OpencodePluginModule, rel_path: &[".local", "share", "opencapx", "adapters", "opencode"],
            events: &[] },
        "droid" => Spec { style: Style::ClaudeNested, rel_path: &[".factory", "hooks.json"],
            events: &["SessionStart", "UserPromptSubmit", "PreToolUse", "Notification", "Stop", "SubagentStop", "SessionEnd"] },
        "pi" => Spec { style: Style::PiExtension, rel_path: &[".pi", "agent", "extensions", "opencapx.ts"],
            events: &[] },
        "omp" => Spec { style: Style::OmpExtension, rel_path: &[".omp", "agent", "extensions", "opencapx.ts"],
            events: &[] },
        "grok" => Spec { style: Style::ClaudeNested, rel_path: &[".grok", "hooks", "opencapx.json"],
            events: &["SessionStart", "UserPromptSubmit", "PostToolUse", "Notification", "Stop", "SessionEnd"] },
        _ => return None,
    })
}

/// Supported agent table: (kind, display name, note). The display name is also
/// used by the tray/notifications (claude → "Claude Code"), so maintain it only here.
const AGENTS: &[(&str, &str, Option<&str>)] = &[
        ("claude", "Claude Code", None),
        ("codex", "Codex", Some("After enabling, run /hooks in Codex and Trust the OpenCapX hook")),
        ("gemini", "Gemini CLI", None),
        ("cursor", "Cursor", None),
        ("opencode", "opencode", None),
        ("windsurf", "Windsurf", Some("No \"needs input\" alerts (Windsurf has no such hook)")),
        ("antigravity", "Antigravity", Some("No \"needs input\" alerts (Antigravity has no notification hook)")),
        ("copilot", "GitHub Copilot", Some("Copilot CLI only (~/.copilot/hooks)")),
        ("kiro", "Kiro CLI", Some("Hooks the default Kiro CLI agent")),
        ("droid", "Factory Droid", Some("Factory Droid CLI (~/.factory/hooks.json)")),
        ("pi", "Pi", Some("Pi extension (~/.pi/agent/extensions). No \"needs input\" alerts")),
        ("omp", "Oh My Pi", Some("OMP extension (~/.omp/agent/extensions). No \"needs input\" alerts")),
        ("grok", "Grok Build", Some("xAI Grok Build CLI (~/.grok/hooks/opencapx.json)")),
];

pub fn catalog() -> Vec<AgentInfo> {
    AGENTS.iter().map(|(kind, name, note)| AgentInfo {
        kind: kind.to_string(),
        display_name: name.to_string(),
        installed: is_installed(kind),
        note: note.map(|s| s.to_string()),
    }).collect()
}

/// kind → display name. Unknown kinds fall back to the raw kind with its first
/// letter capitalized (a custom agent should not show up in the tray as an id-style "my-agent").
pub fn display_name(kind: &str) -> String {
    if let Some((_, name, _)) = AGENTS.iter().find(|(k, _, _)| *k == kind) {
        return (*name).to_string();
    }
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => kind.to_string(),
    }
}

fn config_path(kind: &str) -> Option<PathBuf> {
    let mut p = crate::core::home_dir()?;
    for part in spec(kind)?.rel_path { p.push(part); }
    Some(p)
}

// ===== Stable CLI path (the shim) =====
// Hook and MCP configs must never bake current_exe's absolute path: a dev checkout's
// target/debug path dies on `cargo clean` or a moved checkout (observed in the wild:
// release installs kept firing a dead debug path, and `is_ours` substring matching made
// `connect` short-circuit without ever repairing it). Every config instead points at a
// stable copy under ~/.opencapx/bin/opencapx, refreshed at app start / connect / hook.

/// The stable CLI path that every hook and MCP config points at.
pub fn shim_path() -> PathBuf {
    crate::core::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".opencapx")
        .join("bin")
        .join("opencapx")
}

/// Materialize the shim. Test builds skip the copy itself (the test binary is ~100 MB;
/// every install()-path test would churn it through a temp HOME) — tests pin paths, and
/// one dedicated test exercises the real copy via ensure_shim_for_real().
pub fn ensure_shim() -> std::io::Result<PathBuf> {
    if cfg!(test) {
        return Ok(shim_path());
    }
    ensure_shim_for_real()
}

/// The actual copy: atomic tmp+rename, so it also works while the shim itself is executing
/// (hook processes are spawned from this very path). Skipped when the shim already matches
/// the running binary (same size and not older than the source).
fn ensure_shim_for_real() -> std::io::Result<PathBuf> {
    let shim = shim_path();
    let exe = std::env::current_exe()?;
    if same_file(&exe, &shim) {
        return Ok(shim);
    }
    let fresh = std::fs::metadata(&exe)
        .ok()
        .zip(std::fs::metadata(&shim).ok())
        .map(|(src, dst)| src.len() == dst.len() && modified_secs(&dst) >= modified_secs(&src))
        .unwrap_or(false);
    if !fresh {
        if let Some(dir) = shim.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = shim.with_file_name(format!("opencapx.tmp-{}", std::process::id()));
        std::fs::copy(&exe, &tmp)?;
        std::fs::rename(&tmp, &shim)?;
    }
    Ok(shim)
}

fn modified_secs(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (std::fs::canonicalize(a), std::fs::canonicalize(b)),
        (Ok(x), Ok(y)) if x == y
    )
}

fn hook_command() -> String {
    // The written path must exist by construction: materialize the shim first, and only
    // fall back to current_exe when the shim cannot be written (read-only home).
    let exe = ensure_shim()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "opencapx".into())
        });
    format!("\"{}\" hook --agent", exe)
}
fn full_command(kind: &str) -> String { format!("{} {}", hook_command(), kind) }

fn is_ours(cmd: &str) -> bool {
    let l = cmd.to_lowercase();
    l.contains("opencapx") && l.contains("hook")
}

fn read_json(path: &PathBuf) -> Value {
    std::fs::read_to_string(path).ok()
        .and_then(|s| if s.trim().is_empty() { None } else { serde_json::from_str(&s).ok() })
        .unwrap_or_else(|| json!({}))
}
fn write_json(path: &PathBuf, v: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    std::fs::write(path, serde_json::to_string_pretty(v).unwrap_or_default())
}

fn container_key(style: Style) -> &'static str {
    if style == Style::AntigravityNested { "opencapx" } else { "hooks" }
}
fn antigravity_matcher(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PostToolUse")
}
fn group_is_ours(entry: &Value) -> bool {
    entry.get("hooks").and_then(|h| h.as_array())
        .map(|a| a.iter().any(|h| h.get("command").and_then(|c| c.as_str()).map(is_ours).unwrap_or(false)))
        .unwrap_or(false)
}
fn flat_is_ours(entry: &Value) -> bool {
    entry.get("command").and_then(|c| c.as_str()).map(is_ours).unwrap_or(false)
}
fn entry_is_ours(style: Style, event: &str, entry: &Value) -> bool {
    match style {
        Style::ClaudeNested => group_is_ours(entry),
        Style::AntigravityNested => if antigravity_matcher(event) { group_is_ours(entry) } else { flat_is_ours(entry) },
        _ => flat_is_ours(entry),
    }
}
fn make_entry(style: Style, event: &str, cmd: &str) -> Value {
    match style {
        Style::ClaudeNested => json!({ "hooks": [{ "type": "command", "command": cmd }] }),
        Style::CursorFlat => json!({ "command": cmd, "type": "command" }),
        Style::WindsurfFlat => json!({ "command": cmd, "show_output": false }),
        Style::KiroFlat => json!({ "command": cmd }),
        Style::AntigravityNested => if antigravity_matcher(event) {
            json!({ "matcher": "*", "hooks": [{ "type": "command", "command": cmd }] })
        } else {
            json!({ "type": "command", "command": cmd })
        },
        Style::OpencodePluginModule | Style::PiExtension | Style::OmpExtension => Value::Null,
    }
}

pub fn is_installed(kind: &str) -> bool {
    let (Some(path), Some(s)) = (config_path(kind), spec(kind)) else { return false };
    if s.style == Style::OpencodePluginModule {
        return plugin_module_file(kind)
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|c| is_ours(&c))
            .unwrap_or(false);
    }
    if s.style == Style::PiExtension || s.style == Style::OmpExtension {
        return std::fs::read_to_string(&path).map(|c| is_ours(&c)).unwrap_or(false);
    }
    let v = read_json(&path);
    let Some(map) = v.get(container_key(s.style)).and_then(|h| h.as_object()) else { return false };
    s.events.iter().any(|event| {
        map.get(*event).and_then(|a| a.as_array())
            .map(|arr| arr.iter().any(|e| entry_is_ours(s.style, event, e)))
            .unwrap_or(false)
    })
}


/// connect's hook side: ensure it is installed (idempotent; a second call does not
/// uninstall, unlike toggle's semantics).
///
/// File-based plugins (opencode / pi) additionally do a **content refresh**: if the
/// on-disk template is stale, rewrite it. Otherwise `is_installed` short-circuits and
/// template upgrades can never ship (observed: a template adding
/// `tool.execute.before` only takes effect after manually deleting the file).
/// settings.json-style plugins are unaffected — `install` there is already idempotent
/// and the command string does not contain version-varying content.
pub fn ensure_installed(kind: &str) -> Result<(), String> {
    if is_installed(kind) && !stale_plugin_file(kind) {
        return Ok(());
    }
    install(kind).map_err(|e| e.to_string())
}

/// Whether a file-based plugin's on-disk content lags the current template. Always `false` for non-file types.
fn stale_plugin_file(kind: &str) -> bool {
    let (Some(path), Some(s)) = (config_path(kind), spec(kind)) else {
        return false;
    };
    let binary = binary_from(&full_command(kind));
    let (target, want) = match s.style {
        Style::OpencodePluginModule => (plugin_module_file(kind), opencode_plugin(&binary)),
        Style::PiExtension => (Some(path.clone()), pi_extension(&binary)),
        Style::OmpExtension => (Some(path.clone()), omp_extension(&binary)),
        _ => return false,
    };
    let Some(target) = target else { return false };
    std::fs::read_to_string(target)
        .map(|current| current != want)
        .unwrap_or(false)
}

// ===== connect's MCP side: write `opencapx mcp` into each host's MCP config (idempotent) =====
// Auth is handled by mcp::run's TOFU registration at startup (token lands in
// ~/.opencapx/agent-tokens/, 0600); each host's config only points at this binary
// without credentials — same principle as hooks.

fn mcp_config_target(kind: &str) -> Option<(PathBuf, &'static str)> {
    let home = crate::core::home_dir()?;
    Some(match kind {
        // Claude Code: the global MCP table lives in ~/.claude.json (separate from the hooks settings.json)
        "claude" => (home.join(".claude.json"), "json"),
        "codex" => (home.join(".codex").join("config.toml"), "toml"),
        "opencode" => (home.join(".config").join("opencode").join("opencode.json"), "json"),
        "omp" => (home.join(".omp").join("agent").join("mcp.json"), "json"),
        _ => return None,
    })
}

/// Whether this host has an MCP config target. Hooks-only hosts (pi, omp) legitimately don't;
/// `connect` treats that as a note, not a failure.
pub fn supports_mcp(kind: &str) -> bool {
    mcp_config_target(kind).is_some()
}

fn mcp_entry_is_ours(kind: &str, v: &Value) -> bool {
    let entry = match kind {
        "claude" | "omp" => v.get("mcpServers").and_then(|m| m.get("opencapx")),
        "opencode" => v.get("mcp").and_then(|m| m.get("opencapx")),
        _ => return false,
    };
    let cmd_hit = |c: &Value| {
        c.as_str().map(|s| s.to_lowercase().contains("opencapx")).unwrap_or(false)
            || c.as_array().map(|a| a.iter().any(|x| x.as_str().map(|s| s.to_lowercase().contains("opencapx")).unwrap_or(false))).unwrap_or(false)
    };
    match kind {
        "claude" | "omp" => entry.and_then(|e| e.get("command")).map(cmd_hit).unwrap_or(false),
        "opencode" => entry.and_then(|e| e.get("command")).map(cmd_hit).unwrap_or(false),
        _ => false,
    }
}

/// omp (like codex) exports no host-signature env var to stdio MCP children, so an entry written
/// before the identity pin existed must be upgraded — otherwise the MCP child registers as `custom`
/// while the hooks register as `omp`: one session split across two agents, the exact incident
/// codex's backfill addresses.
fn mcp_entry_needs_identity_backfill(kind: &str, v: &Value) -> bool {
    kind == "omp"
        && v.get("mcpServers")
            .and_then(|m| m.get("opencapx"))
            .and_then(|e| e.get("env"))
            .and_then(|e| e.get("OPEN_CAPX_AGENT"))
            .and_then(|x| x.as_str())
            != Some("omp")
}

/// The identity pin every codex MCP entry carries. Codex exports no host-signature env
/// var (unlike CLAUDECODE=1 / OPENCODE=1), so without this explicit mapping
/// `identity::detect_kind()` falls back to "custom" in the MCP child: hooks record the
/// session as codex, the MCP side registers as custom — one session split across two agents.
const CODEX_IDENTITY_ENV: &str = "OPEN_CAPX_AGENT = \"codex\"";

/// Returns (config file path, whether it was newly written this time). Already written and pointing at this repo's binary → (path, false).
pub fn ensure_mcp(kind: &str) -> Result<(String, bool), String> {
    let Some((path, fmt)) = mcp_config_target(kind) else {
        return Err(format!("unknown agent: {}", kind));
    };
    // Same stable-path rule as hook entries: point at the shim, never at current_exe.
    let exe = ensure_shim()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "opencapx".into())
        });

    if fmt == "toml" {
        // codex: append [mcp_servers.opencapx] to config.toml; an existing block counts as in
        // place here — refresh_installations backfills the identity env into older blocks.
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing.contains("[mcp_servers.opencapx]") {
            return Ok((path.display().to_string(), false));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let esc = exe.replace('\\', "\\\\").replace('"', "\\\"");
        let block = format!(
            "\n[mcp_servers.opencapx]\ncommand = \"{}\"\nargs = [\"mcp\"]\nenv = {{ {} }}\n",
            esc, CODEX_IDENTITY_ENV
        );
        let mut out = existing;
        if !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&block);
        std::fs::write(&path, out).map_err(|e| e.to_string())?;
        return Ok((path.display().to_string(), true));
    }

    let v = read_json(&path);
    if mcp_entry_is_ours(kind, &v) && !mcp_entry_needs_identity_backfill(kind, &v) {
        return Ok((path.display().to_string(), false));
    }
    let mut v = v;
    let obj = v.as_object_mut().ok_or("mcp config is not a JSON object")?;
    match kind {
        "claude" => {
            let servers = obj
                .entry("mcpServers")
                .or_insert_with(|| json!({}));
            servers
                .as_object_mut()
                .ok_or("mcpServers is not an object")?
                .insert("opencapx".into(), json!({ "command": exe, "args": ["mcp"] }));
        }
        "opencode" => {
            let mcp = obj.entry("mcp").or_insert_with(|| json!({}));
            mcp.as_object_mut()
                .ok_or("mcp is not an object")?
                .insert("opencapx".into(), json!({ "type": "local", "command": [exe, "mcp"] }));
        }
        "omp" => {
            // omp spawns MCP children without a host-signature env var (same gap as codex), so the
            // identity pin rides in the entry; an existing entry is merged, not clobbered, so
            // user-set env keys survive.
            let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
            let entry = servers
                .as_object_mut()
                .ok_or("mcpServers is not an object")?
                .entry("opencapx")
                .or_insert_with(|| json!({}));
            let e = entry.as_object_mut().ok_or("opencapx entry is not an object")?;
            e.insert("type".into(), json!("stdio"));
            e.insert("command".into(), json!(exe));
            e.insert("args".into(), json!(["mcp"]));
            e.entry("env")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or("env is not an object")?
                .insert("OPEN_CAPX_AGENT".into(), json!("omp"));
        }
        _ => unreachable!(),
    }
    write_json(&path, &v).map_err(|e| e.to_string())?;
    Ok((path.display().to_string(), true))
}

pub fn toggle(kind: &str) -> Result<bool, String> {
    if is_installed(kind) {
        uninstall(kind).map_err(|e| e.to_string())?;
        Ok(false)
    } else {
        install(kind).map_err(|e| e.to_string())?;
        Ok(true)
    }
}

/// Repair pass for configs written by older builds (or a dev checkout that has since moved):
/// rewrite every OpenCapX hook/MCP entry still pointing at a binary path other than the shim,
/// backfill the codex identity env into older MCP blocks, and refresh the file-based plugins
/// (opencode / pi) whose embedded BIN went stale.
///
/// Called at app start and after `connect`. Idempotent; returns the number of files rewritten.
/// A shim that cannot be written short-circuits the whole pass — on such a machine install()
/// already fell back to current_exe, and rewriting those entries to a dead path would break them.
pub fn refresh_installations() -> usize {
    let shim = match ensure_shim_for_real() {
        Ok(p) => p,
        Err(_) => return 0,
    };
    let want = shim.to_string_lossy().into_owned();
    let mut fixed = 0;
    for (kind, _, _) in AGENTS {
        let Some(s) = spec(kind) else { continue };
        if let Some(path) = config_path(kind) {
            match s.style {
                // file plugins: rewrite wholesale from the current template when the embedded BIN differs
                Style::OpencodePluginModule => {
                    if let Some(f) = plugin_module_file(kind) {
                        if let Ok(cur) = std::fs::read_to_string(&f) {
                            let want_content = opencode_plugin(&want);
                            if cur.contains("opencapx") && cur != want_content && std::fs::write(&f, want_content).is_ok() {
                                fixed += 1;
                            }
                        }
                    }
                }
                Style::PiExtension => {
                    if let Ok(cur) = std::fs::read_to_string(&path) {
                        let want_content = pi_extension(&want);
                        if cur.contains("opencapx") && cur != want_content {
                            if let Some(dir) = path.parent() {
                                let _ = std::fs::create_dir_all(dir);
                            }
                            if std::fs::write(&path, want_content).is_ok() {
                                fixed += 1;
                            }
                        }
                    }
                }
                Style::OmpExtension => {
                    if let Ok(cur) = std::fs::read_to_string(&path) {
                        let want_content = omp_extension(&want);
                        if cur.contains("opencapx") && cur != want_content {
                            if let Some(dir) = path.parent() {
                                let _ = std::fs::create_dir_all(dir);
                            }
                            if std::fs::write(&path, want_content).is_ok() {
                                fixed += 1;
                            }
                        }
                    }
                }
                _ => {
                    let mut v = read_json(&path);
                    if v.is_object() && fix_stale_hook_commands(&mut v, &s, kind, &want) && write_json(&path, &v).is_ok() {
                        fixed += 1;
                    }
                }
            }
        }
        if reconcile_mcp_entry(kind, &want) {
            fixed += 1;
        }
    }
    fixed
}

/// Walk one agent's hook config; rewrite our entries whose baked binary ≠ the shim.
fn fix_stale_hook_commands(v: &mut Value, s: &Spec, kind: &str, want_bin: &str) -> bool {
    let cmd = full_command(kind);
    let key = container_key(s.style);
    let Some(map) = v.get_mut(key).and_then(|h| h.as_object_mut()) else { return false };
    let mut changed = false;
    for event in s.events {
        if let Some(arr) = map.get_mut(*event).and_then(|a| a.as_array_mut()) {
            for entry in arr.iter_mut() {
                if entry_is_ours(s.style, event, entry) && fix_entry_command(entry, &cmd, want_bin) {
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Replace the command string(s) inside one of our entries when the baked binary is stale.
/// Handles both shapes we write: flat `{command}` and nested `{hooks:[{command}]}`.
fn fix_entry_command(entry: &mut Value, new_cmd: &str, want_bin: &str) -> bool {
    let mut changed = false;
    let slots: Vec<&mut Value> = match entry.get_mut("hooks").and_then(|h| h.as_array_mut()) {
        Some(arr) => arr.iter_mut().filter_map(|h| h.get_mut("command")).collect(),
        None => entry.get_mut("command").into_iter().collect(),
    };
    for c in slots {
        if let Some(s) = c.as_str() {
            if is_ours(s) && binary_from(s) != want_bin {
                *c = json!(new_cmd);
                changed = true;
            }
        }
    }
    changed
}

/// Rewrite our MCP entry's binary when it no longer points at the shim.
/// claude: ~/.claude.json mcpServers.opencapx.command; omp: ~/.omp/agent/mcp.json mcpServers.opencapx.command;
/// opencode: mcp.opencapx.command[0]; codex: the command + identity env inside our [mcp_servers.opencapx] config.toml block.
fn reconcile_mcp_entry(kind: &str, want: &str) -> bool {
    let Some((path, fmt)) = mcp_config_target(kind) else { return false };
    if fmt == "toml" {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let Some(updated) = reconcile_toml_block(&text, want) else { return false };
        return std::fs::write(&path, updated).is_ok();
    }
    let mut v = read_json(&path);
    let Some(obj) = v.as_object_mut() else { return false };
    let stale = |s: &str| s.to_lowercase().contains("opencapx") && s != want;
    let mut changed = false;
    if kind == "claude" || kind == "omp" {
        if let Some(c) = obj
            .get_mut("mcpServers")
            .and_then(|m| m.get_mut("opencapx"))
            .and_then(|e| e.get_mut("command"))
        {
            if c.as_str().is_some_and(stale) {
                *c = json!(want);
                changed = true;
            }
        }
    } else if kind == "opencode" {
        if let Some(first) = obj
            .get_mut("mcp")
            .and_then(|m| m.get_mut("opencapx"))
            .and_then(|e| e.get_mut("command"))
            .and_then(|c| c.as_array_mut())
            .and_then(|a| a.first_mut())
        {
            if first.as_str().is_some_and(stale) {
                *first = json!(want);
                changed = true;
            }
        }
    }
    if changed {
        let _ = write_json(&path, &v);
    }
    changed
}

/// codex config.toml: keep our `[mcp_servers.opencapx]` block current. Two repairs:
/// - swap a stale `command` line for the current shim path;
/// - backfill the identity env (`CODEX_IDENTITY_ENV`) into blocks written before it existed.
///
/// An `env` form we cannot safely merge into (nested `[mcp_servers.opencapx.env]` table,
/// or a line without an inline table) is left alone rather than risk a TOML duplicate-key
/// error the host would refuse to parse — a missing backfill beats a broken config file.
/// None = no block, or nothing to change.
fn reconcile_toml_block(text: &str, want: &str) -> Option<String> {
    const HEADER: &str = "[mcp_servers.opencapx]";
    const NESTED_ENV: &str = "[mcp_servers.opencapx.env]";
    let start = text.find(HEADER)?;
    // the block runs to the next top-level table header (or EOF)
    let block_end = text[start + HEADER.len()..]
        .find("\n[")
        .map(|i| start + HEADER.len() + i)
        .unwrap_or(text.len());
    let block = &text[start..block_end];
    let esc = want.replace('\\', "\\\\").replace('"', "\\\"");
    let new_cmd = format!("command = \"{}\"", esc);
    let key_of = |l: &str| l.trim().split('=').next().unwrap_or("").trim().to_string();
    let indent_of = |l: &str| l.chars().take_while(|c| c.is_whitespace()).collect::<String>();

    let mut lines: Vec<String> = block.lines().map(|l| l.to_string()).collect();
    let mut changed = false;

    for l in lines.iter_mut() {
        if key_of(l) == "command" && l.trim() != new_cmd {
            *l = format!("{}{}", indent_of(l), new_cmd);
            changed = true;
        }
    }

    let env_already_pinned = block.contains("OPEN_CAPX_AGENT") || text.contains(NESTED_ENV);
    if !env_already_pinned {
        if let Some(i) = lines.iter().position(|l| key_of(l) == "env") {
            let core = lines[i].trim().to_string();
            let indent = indent_of(&lines[i]);
            if let (Some(open), Some(close)) = (core.find('{'), core.rfind('}')) {
                if close > open {
                    let body = core[open + 1..close].trim();
                    let mid = if body.is_empty() {
                        format!(" {} ", CODEX_IDENTITY_ENV)
                    } else {
                        format!(" {}, {} ", CODEX_IDENTITY_ENV, body)
                    };
                    lines[i] = format!("{}{}{}{}", indent, &core[..open + 1], mid, &core[close..]);
                    changed = true;
                }
            }
        } else {
            // Insert only after an anchor line whose brackets balance. A multi-line value
            // (`args = [\n  "mcp"\n]`) would otherwise get the env line inside the array
            // literal — invalid TOML, codex refuses the whole config: the exact breakage
            // this repair must never cause. A missing backfill beats a broken config.
            let balanced = |l: &str| {
                l.matches('[').count() == l.matches(']').count()
                    && l.matches('{').count() == l.matches('}').count()
            };
            let anchor = lines
                .iter()
                .rposition(|l| key_of(l) == "args" && balanced(l))
                .or_else(|| lines.iter().rposition(|l| key_of(l) == "command" && balanced(l)));
            if let Some(anchor) = anchor {
                let indent = indent_of(&lines[anchor]);
                lines.insert(anchor + 1, format!("{}env = {{ {} }}", indent, CODEX_IDENTITY_ENV));
                changed = true;
            }
            // No balanced anchor: skip the env backfill but keep any command repair already
            // made above — a partial repair beats discarding it.
        }
    }

    if !changed {
        return None;
    }
    Some(format!("{}{}{}", &text[..start], lines.join("\n"), &text[block_end..]))
}

fn install(kind: &str) -> std::io::Result<()> {
    let (Some(path), Some(s)) = (config_path(kind), spec(kind)) else {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, "unknown agent"));
    };
    let cmd = full_command(kind);

    if s.style == Style::OpencodePluginModule {
        std::fs::create_dir_all(&path)?;
        std::fs::write(path.join("index.js"), opencode_plugin(&binary_from(&cmd)))?;
        std::fs::write(path.join("package.json"), opencode_plugin_package_json())?;
        register_opencode_plugin(&path)?;
        return Ok(());
    }
    if s.style == Style::PiExtension || s.style == Style::OmpExtension {
        if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
        let body = if s.style == Style::OmpExtension {
            omp_extension(&binary_from(&cmd))
        } else {
            pi_extension(&binary_from(&cmd))
        };
        return std::fs::write(&path, body);
    }

    let mut v = read_json(&path);
    let Some(obj) = v.as_object_mut() else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
            format!("{} is not a JSON object; fix or remove it and try again", path.display())));
    };
    if s.style == Style::CursorFlat { obj.entry("version").or_insert(json!(1)); }
    if s.style == Style::KiroFlat && obj.get("name").is_none() {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "default".into());
        obj.insert("name".to_string(), json!(name));
    }
    let key = container_key(s.style);
    if !obj.get(key).map_or(false, |h| h.is_object()) { obj.insert(key.to_string(), json!({})); }
    let map = obj.get_mut(key).and_then(|h| h.as_object_mut()).unwrap();
    for event in s.events {
        let mut kept: Vec<Value> = map.get(*event).and_then(|a| a.as_array())
            .map(|a| a.iter().filter(|e| !entry_is_ours(s.style, event, e)).cloned().collect())
            .unwrap_or_default();
        kept.push(make_entry(s.style, event, &cmd));
        map.insert((*event).to_string(), Value::Array(kept));
    }
    write_json(&path, &v)?;
    if kind == "codex" { enable_codex_hooks(); }
    Ok(())
}

fn uninstall(kind: &str) -> std::io::Result<()> {
    let (Some(path), Some(s)) = (config_path(kind), spec(kind)) else { return Ok(()) };
    if s.style == Style::OpencodePluginModule {
        unregister_opencode_plugin(&path);
        let _ = std::fs::remove_dir_all(&path);
        return Ok(());
    }
    if kind == "copilot" || s.style == Style::PiExtension || s.style == Style::OmpExtension {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let mut v = read_json(&path);
    let Some(obj) = v.as_object_mut() else { return Ok(()) };
    let key = container_key(s.style);
    if let Some(map) = obj.get_mut(key).and_then(|h| h.as_object_mut()) {
        for event in s.events {
            if let Some(arr) = map.get(*event).and_then(|a| a.as_array()) {
                let kept: Vec<Value> = arr.iter().filter(|e| !entry_is_ours(s.style, event, e)).cloned().collect();
                if kept.is_empty() { map.remove(*event); } else { map.insert((*event).to_string(), Value::Array(kept)); }
            }
        }
        if map.is_empty() { obj.remove(key); }
    }
    write_json(&path, &v)
}

fn binary_from(cmd: &str) -> String {
    if let Some(start) = cmd.find('"') {
        if let Some(end) = cmd[start + 1..].find('"') {
            return cmd[start + 1..start + 1 + end].to_string();
        }
    }
    cmd.split(' ').next().unwrap_or(cmd).to_string()
}

/// opencode plugin source file — real JS, see adapters/README.md.
/// `"__OPENCAPX_BIN__"` (including quotes) is replaced wholesale with a JSON string literal, safe even when the path contains backslashes.
const OPENCODE_PLUGIN_JS: &str = include_str!("../../adapters/opencode/plugin.js");

fn opencode_plugin(binary: &str) -> String {
    let bin = serde_json::to_string(binary).unwrap_or_else(|_| format!("\"{}\"", binary));
    OPENCODE_PLUGIN_JS.replace("\"__OPENCAPX_BIN__\"", &bin)
}

/// OMP (oh-my-pi) extension source file — real TS, see adapters/README.md.
/// Same placeholder contract as opencode's: `"__OPENCAPX_BIN__"` (including quotes) is replaced
/// wholesale with a JSON string literal, safe even when the path contains backslashes.
const OMP_EXTENSION_TS: &str = include_str!("../../adapters/omp/extension.ts");

fn omp_extension(binary: &str) -> String {
    let bin = serde_json::to_string(binary).unwrap_or_else(|_| format!("\"{}\"", binary));
    OMP_EXTENSION_TS.replace("\"__OPENCAPX_BIN__\"", &bin)
}

/// The plugin module's entry file (`<dir>/index.js`).
fn plugin_module_file(kind: &str) -> Option<PathBuf> {
    config_path(kind).map(|d| d.join("index.js"))
}

fn opencode_plugin_package_json() -> String {
    format!(
        "{{\n  \"name\": \"opencapx-opencode\",\n  \"version\": \"{}\",\n  \"type\": \"module\",\n  \"main\": \"index.js\"\n}}\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// **Prepend** the plugin directory to the front of the `plugin` array in
/// `~/.config/opencode/opencode.json`.
///
/// It must come first: opencode runs config plugins before directory plugins, and a
/// plugin that **replaces wholesale** `output.args` (swapping in a new object) is
/// silently ignored by the host — only running first and mutating properties in place
/// reaches the object that actually gets executed. See adapters/README.md.
fn register_opencode_plugin(dir: &std::path::Path) -> std::io::Result<()> {
    let Some((path, _)) = mcp_config_target("opencode") else { return Ok(()) };
    let mut v = read_json(&path);
    let obj = v.as_object_mut().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "opencode.json is not a JSON object")
    })?;
    let me = dir.to_string_lossy().into_owned();
    let arr = obj
        .entry("plugin")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "plugin is not an array")
        })?;
    arr.retain(|x| x.as_str() != Some(me.as_str()));
    arr.insert(0, json!(me));
    write_json(&path, &v)
}

/// Remove our entry from the plugin array (used on uninstall).
fn unregister_opencode_plugin(dir: &std::path::Path) {
    let Some((path, _)) = mcp_config_target("opencode") else { return };
    let mut v = read_json(&path);
    let Some(arr) = v.get_mut("plugin").and_then(|a| a.as_array_mut()) else { return };
    let me = dir.to_string_lossy().into_owned();
    let before = arr.len();
    arr.retain(|x| x.as_str() != Some(me.as_str()));
    if arr.len() != before {
        let _ = write_json(&path, &v);
    }
}

fn pi_extension(binary: &str) -> String {
    let bin = serde_json::to_string(binary).unwrap_or_else(|_| format!("\"{}\"", binary));
    format!(
        "// OpenCapX integration (auto-generated, safe to delete to uninstall).\n\
         // Reports Pi session lifecycle to OpenCapX.\n\
         import {{ spawn }} from \"node:child_process\"\n\
         const OPENCAPX_BIN = {bin}\n\
         export default function (pi) {{\n\
         \x20 const send = (state, ctx) => {{\n\
         \x20   try {{\n\
         \x20     const cwd = (ctx && ctx.cwd) || process.cwd()\n\
         \x20     const file = ctx && ctx.sessionManager && ctx.sessionManager.getSessionFile ? ctx.sessionManager.getSessionFile() : null\n\
         \x20     const sid = \"pi:\" + (file || cwd)\n\
         \x20     const p = spawn(OPENCAPX_BIN, [\"hook\", \"--agent\", \"pi\", \"--event\", state, \"--session\", sid, \"--project\", cwd], {{ stdio: \"ignore\" }})\n\
         \x20     if (p && p.unref) p.unref()\n\
         \x20   }} catch (e) {{}}\n\
         }}\n\
         \x20 pi.on(\"session_start\", async (_e, ctx) => send(\"registered\", ctx))\n\
         \x20 pi.on(\"agent_start\", async (_e, ctx) => send(\"working\", ctx))\n\
         \x20 pi.on(\"agent_end\", async (_e, ctx) => send(\"done\", ctx))\n\
         \x20 pi.on(\"session_shutdown\", async (_e, ctx) => send(\"done\", ctx))\n\
         }}\n"
    )
}

fn enable_codex_hooks() {
    let Some(home) = crate::core::home_dir() else { return };
    let path = home.join(".codex").join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let already = text.lines().any(|l| {
        let c = l.trim().replace(' ', "");
        !c.starts_with('#') && c.starts_with("hooks=true")
    });
    if already { return; }
    let updated = if let Some(idx) = text.lines().position(|l| l.trim() == "[features]") {
        let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
        lines.insert(idx + 1, "hooks = true".into());
        lines.join("\n")
    } else {
        let mut t = text;
        if !t.is_empty() && !t.ends_with('\n') { t.push('\n'); }
        t.push_str("\n[features]\nhooks = true\n");
        t
    };
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    let _ = std::fs::write(&path, updated);
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    static HOME_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_home(tag: &str, f: impl FnOnce()) {
        // Proceed even when poisoned: otherwise one panicking test leaves the rest of
        // the group stuck on lock() (observed: 1 flake amplified into 4 reds).
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-home-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // OPENCAPX_HOME is honored by core::home_dir() on every platform; HOME alone cannot
        // isolate on Windows (dirs::home_dir() there is the Known Folder API, env-blind).
        let old_home = std::env::var_os("HOME");
        let old_override = std::env::var_os("OPENCAPX_HOME");
        std::env::set_var("OPENCAPX_HOME", &dir);
        std::env::set_var("HOME", &dir);
        f();
        match old_override {
            Some(h) => std::env::set_var("OPENCAPX_HOME", h),
            None => std::env::remove_var("OPENCAPX_HOME"),
        }
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn display_name_uses_catalog_then_capitalizes_unknown() {
        assert_eq!(display_name("claude"), "Claude Code");
        assert_eq!(display_name("gemini"), "Gemini CLI");
        assert_eq!(display_name("codex"), "Codex");
        // Custom agent: capitalize the first letter; don't hand the raw id to the user
        assert_eq!(display_name("my-agent"), "My-agent");
        assert_eq!(display_name(""), "");
    }

    #[test]
    fn hooks_toggle_roundtrip_in_temp_home() {
        with_temp_home("toggle", || {
            assert!(!is_installed("claude"));
            assert_eq!(toggle("claude"), Ok(true));
            assert!(is_installed("claude"));
            assert_eq!(toggle("claude"), Ok(false));
            assert!(!is_installed("claude"));
            assert!(toggle("nope").is_err());
        });
    }

    #[test]
    fn hooks_plugin_files_roundtrip_in_temp_home() {
        with_temp_home("plugin", || {
            assert_eq!(toggle("opencode"), Ok(true));
            assert!(is_installed("opencode"));
            let dir = config_path("opencode").unwrap().to_string_lossy().into_owned();
            assert!(plugin_array_contains(&dir), "install must write the plugin directory into the plugin array");

            assert_eq!(toggle("opencode"), Ok(false));
            assert!(!is_installed("opencode"));
            assert!(!plugin_array_contains(&dir), "uninstall must remove it from the plugin array");
        });
    }

    /// The Settings page's "Agents" button goes through toggle → install/uninstall; this pins the two places it touches.
    fn plugin_array_contains(needle: &str) -> bool {
        let cfg = mcp_config_target("opencode").unwrap().0;
        read_json(&cfg)["plugin"]
            .as_array()
            .map(|a| a.iter().any(|x| x.as_str() == Some(needle)))
            .unwrap_or(false)
    }

    #[test]
    fn hooks_install_preserves_foreign_entries() {
        with_temp_home("foreign", || {
            let path = config_path("claude").unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"other-tool"}]}]}}"#).unwrap();
            assert_eq!(toggle("claude"), Ok(true));
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let stop = v["hooks"]["Stop"].as_array().unwrap();
            assert_eq!(stop.len(), 2);
            assert_eq!(toggle("claude"), Ok(false));
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1);
        });
    }

    #[test]
    fn connect_ensure_installed_is_idempotent_not_toggle() {
        with_temp_home("ensure", || {
            assert!(!is_installed("claude"));
            ensure_installed("claude").unwrap();
            assert!(is_installed("claude"));
            // Run again: still in place (unlike toggle, it does not uninstall)
            ensure_installed("claude").unwrap();
            assert!(is_installed("claude"));
        });
    }

    #[test]
    fn connect_ensure_mcp_writes_each_shape_idempotently() {
        with_temp_home("mcp", || {
            // claude: ~/.claude.json mcpServers
            let (p1, w1) = ensure_mcp("claude").unwrap();
            assert!(w1 && p1.ends_with(".claude.json"));
            let v = read_json(&std::path::PathBuf::from(&p1));
            assert!(v["mcpServers"]["opencapx"]["command"].is_string());
            assert_eq!(v["mcpServers"]["opencapx"]["args"][0], "mcp");
            assert!(mcp_entry_is_ours("claude", &v));
            assert_eq!(ensure_mcp("claude").unwrap().1, false);

            // codex: append [mcp_servers.opencapx] to config.toml
            let (p2, w2) = ensure_mcp("codex").unwrap();
            assert!(w2 && p2.ends_with("config.toml"));
            let t = std::fs::read_to_string(&p2).unwrap();
            assert!(t.contains("[mcp_servers.opencapx]"));
            assert!(t.contains("args = [\"mcp\"]"));
            assert!(
                t.contains("env = { OPEN_CAPX_AGENT = \"codex\" }"),
                "the block must pin the host identity: codex exports no signature env var to MCP children"
            );
            assert_eq!(ensure_mcp("codex").unwrap().1, false);

            // opencode: opencode.json mcp local command array
            let (p3, w3) = ensure_mcp("opencode").unwrap();
            assert!(w3 && p3.ends_with("opencode.json"));
            let v3 = read_json(&std::path::PathBuf::from(&p3));
            assert_eq!(v3["mcp"]["opencapx"]["type"], "local");
            assert!(mcp_entry_is_ours("opencode", &v3));
            assert_eq!(ensure_mcp("opencode").unwrap().1, false);

            // omp: ~/.omp/agent/mcp.json stdio entry with the identity pin
            let (p4, w4) = ensure_mcp("omp").unwrap();
            assert!(w4 && p4.ends_with("mcp.json"));
            let v4 = read_json(&std::path::PathBuf::from(&p4));
            assert_eq!(v4["mcpServers"]["opencapx"]["type"], "stdio");
            assert_eq!(v4["mcpServers"]["opencapx"]["args"][0], "mcp");
            assert_eq!(v4["mcpServers"]["opencapx"]["env"]["OPEN_CAPX_AGENT"], "omp");
            assert!(mcp_entry_is_ours("omp", &v4));
            assert_eq!(ensure_mcp("omp").unwrap().1, false);

            assert!(ensure_mcp("nope").is_err());
        });
    }

    #[test]
    fn ensure_mcp_omp_preserves_existing_servers() {
        with_temp_home("omp-mcp-keep", || {
            let path = crate::core::home_dir().unwrap().join(".omp").join("agent").join("mcp.json");
            write_json(&path, &json!({"mcpServers": {"other": {"command": "/x/other"}}})).unwrap();
            let (_, w) = ensure_mcp("omp").unwrap();
            assert!(w);
            let v = read_json(&path);
            assert_eq!(v["mcpServers"]["other"]["command"], "/x/other");
            assert_eq!(v["mcpServers"]["opencapx"]["env"]["OPEN_CAPX_AGENT"], "omp");
        });
    }

    /// The observed split: omp hooks record the session as `omp` while the MCP child had no identity
    /// env to sniff and registered as `custom`. A current command must still be repaired when the pin
    /// is missing, and user env keys must survive the merge.
    #[test]
    fn ensure_mcp_omp_backfills_the_identity_env() {
        with_temp_home("omp-mcp-env", || {
            let path = crate::core::home_dir().unwrap().join(".omp").join("agent").join("mcp.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let shim = shim_path().to_string_lossy().to_string();
            write_json(&path, &json!({"mcpServers": {"opencapx": {
                "command": shim, "args": ["mcp"], "env": {"FOO": "bar"}}}}))
            .unwrap();

            let (_, w) = ensure_mcp("omp").unwrap();
            assert!(w, "env-less entry must be repaired even when the command is current");
            let v = read_json(&path);
            assert_eq!(v["mcpServers"]["opencapx"]["env"]["OPEN_CAPX_AGENT"], "omp");
            assert_eq!(v["mcpServers"]["opencapx"]["env"]["FOO"], "bar", "user env keys must survive");
            assert_eq!(ensure_mcp("omp").unwrap().1, false, "idempotent");
        });
    }

    #[test]
    fn refresh_repoints_a_stale_omp_mcp_command() {
        with_temp_home("omp-mcp-repoint", || {
            let path = crate::core::home_dir().unwrap().join(".omp").join("agent").join("mcp.json");
            write_json(&path, &json!({"mcpServers": {"opencapx": {"command": "/gone/debug/opencapx", "args": ["mcp"]}}})).unwrap();
            assert!(refresh_installations() >= 1);
            let v = read_json(&path);
            assert!(v["mcpServers"]["opencapx"]["command"]
                .as_str()
                .unwrap()
                .contains(&shim_path().to_string_lossy().to_string()));
            assert_eq!(refresh_installations(), 0, "idempotent");
        });
    }

    #[test]
    fn cursor_config_includes_pretooluse_for_writeback() {
        with_temp_home("cursor-pretool", || {
            ensure_installed("cursor").unwrap();
            let path = config_path("cursor").unwrap();
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(v["version"], 1, "cursor requires a config version");
            let arr = v["hooks"]["preToolUse"].as_array().expect("preToolUse entry must exist");
            assert!(arr.iter().any(|e| e["command"].as_str().map(is_ours).unwrap_or(false)));
        });
    }

    #[test]
    fn copilot_config_includes_pretooluse_for_writeback() {
        with_temp_home("copilot-pretool", || {
            ensure_installed("copilot").unwrap();
            let path = config_path("copilot").unwrap();
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(v["version"], 1, "copilot rejects a hook config without a version");
            let arr = v["hooks"]["PreToolUse"].as_array().expect("PreToolUse entry must exist");
            assert!(arr.iter().any(|e| e["command"].as_str().map(is_ours).unwrap_or(false)));
        });
    }

    #[test]
    fn connect_ensure_mcp_preserves_existing_config() {
        with_temp_home("mcp-keep", || {
            let path = crate::core::home_dir().unwrap().join(".claude.json");
            write_json(&path, &json!({
                "other": { "keep": 1 },
                "mcpServers": { "fs": { "command": "/x/fs" } }
            })).unwrap();
            let (_, w) = ensure_mcp("claude").unwrap();
            assert!(w);
            let v = read_json(&path);
            assert_eq!(v["other"]["keep"], 1);
            assert_eq!(v["mcpServers"]["fs"]["command"], "/x/fs");
            assert!(v["mcpServers"]["opencapx"]["command"].is_string());
        });
    }

    #[test]
    fn mcp_support_is_declared_per_host() {
        assert!(supports_mcp("claude") && supports_mcp("codex") && supports_mcp("opencode"));
        assert!(supports_mcp("omp"), "omp gets an MCP entry in ~/.omp/agent/mcp.json");
        assert!(!supports_mcp("pi"), "pi ships hooks-only");
        assert!(!supports_mcp("nope"));
    }

/// opencode's command rule mutates args in place via `tool.execute.before` — it does
/// not write back through stdout. It must also call itself opencode (a previous
/// forwarder hardcoded --agent claude, mis-recording opencode traffic as claude).
///
/// This only pins that "the generated artifact has the right shape"; the host really
/// does take the **in-place** mutation (observed on 1.18.31: after removing the plugin
/// that replaces args wholesale, the rewritten command is what gets executed). See the
/// plugin source for the pitfalls.
    #[test]
    fn opencode_plugin_intercepts_tool_calls() {
        let js = opencode_plugin("/usr/local/bin/opencapx");
        assert!(js.contains("tool.execute.before"), "must hook tool pre-execution");
        assert!(js.contains("\"rewrite\", cmd"), "must delegate to opencapx rewrite");
        assert!(js.contains("output.args.command = rewritten"), "must mutate the property in place");
        assert!(js.contains("\"opencode\""), "must call itself opencode");
        assert!(!js.contains("claude"), "must not be mislabeled as claude");
        assert!(!js.contains("__OPENCAPX_BIN__"), "placeholder must be substituted");
        assert!(js.contains("\"/usr/local/bin/opencapx\""), "binary path must be injected");
    }

    /// Re-running connect after a template upgrade must update the on-disk plugin file — otherwise the old template lingers forever.
    #[test]
    fn ensure_installed_refreshes_a_stale_plugin_file() {
        with_temp_home("opencode-refresh", || {
            let dir = config_path("opencode").unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            // Old template: must still contain opencapx + hook, otherwise is_ours says "not ours" and is_installed is false.
            std::fs::write(dir.join("index.js"), "// OpenCapX integration (old)\n// hook\n").unwrap();
            assert!(is_installed("opencode"), "old content still counts as installed");
            assert!(stale_plugin_file("opencode"), "old content must be judged as needing refresh");

            ensure_installed("opencode").unwrap();
            let now = std::fs::read_to_string(dir.join("index.js")).unwrap();
            assert!(now.contains("tool.execute.before"), "should refresh to the current template");
            assert!(!stale_plugin_file("opencode"), "should no longer be judged stale after refresh");
            assert!(dir.join("package.json").exists(), "must write package.json (declaring type: module)");

            // settings.json-style plugins are unaffected by the content refresh logic.
            assert!(!stale_plugin_file("claude"), "always false for non-file types");
        });
    }

    #[test]
    fn hooks_omp_extension_roundtrip_in_temp_home() {
        with_temp_home("omp", || {
            assert!(!is_installed("omp"));
            assert_eq!(toggle("omp"), Ok(true));
            assert!(is_installed("omp"));
            let path = config_path("omp").unwrap();
            assert_eq!(
                path,
                crate::core::home_dir().unwrap().join(".omp").join("agent").join("extensions").join("opencapx.ts")
            );
            assert!(!stale_plugin_file("omp"), "freshly installed content is current");
            assert_eq!(toggle("omp"), Ok(false));
            assert!(!is_installed("omp"));
            assert!(!path.exists());
        });
    }

    #[test]
    fn omp_extension_reports_lifecycle_and_carries_the_binary() {
        let ts = omp_extension("/usr/local/bin/opencapx");
        assert!(ts.contains("pi.on(\"session_start\""), "must report session lifecycle");
        assert!(ts.contains("\"hook\", \"--agent\", AGENT"), "must call the hook as omp");
        assert!(ts.contains("const AGENT = \"omp\""), "must call itself omp");
        assert!(!ts.contains("__OPENCAPX_BIN__"), "placeholder must be substituted");
        assert!(ts.contains("\"/usr/local/bin/opencapx\""), "binary path must be injected");
    }

    #[test]
    fn omp_extension_rewrites_tool_calls() {
        let ts = omp_extension("/usr/local/bin/opencapx");
        assert!(ts.contains("pi.on(\"tool_call\""), "must hook tool pre-execution");
        assert!(ts.contains("hook_event_name: \"PreToolUse\""), "must ask as a PreToolUse event");
        assert!(ts.contains("return { input:"), "must return the replacement input, not mutate");
        assert!(ts.contains("spawnSync"), "the reply must be read synchronously");
    }

    #[test]
    fn ensure_installed_refreshes_a_stale_omp_extension() {
        with_temp_home("omp-refresh", || {
            let path = config_path("omp").unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            // Old template must still contain opencapx + hook, otherwise is_ours says "not ours".
            std::fs::write(&path, "// OpenCapX integration (old)\n// hook\n").unwrap();
            assert!(is_installed("omp"), "old content still counts as installed");
            assert!(stale_plugin_file("omp"), "old content must be judged as needing refresh");
            ensure_installed("omp").unwrap();
            assert!(!stale_plugin_file("omp"), "must refresh to the current template");
            assert!(std::fs::read_to_string(&path).unwrap().contains("session_start"));
        });
    }

    #[test]
    fn refresh_repoints_a_stale_omp_extension_binary() {
        with_temp_home("omp-refresh-bin", || {
            let path = config_path("omp").unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, omp_extension("/gone/debug/opencapx")).unwrap();
            assert!(refresh_installations() >= 1);
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains(&shim_path().to_string_lossy().to_string()));
            assert_eq!(refresh_installations(), 0, "idempotent");
        });
    }

    /// The opencode plugin must be **prepended** to the front of the plugin array —
    /// placed after a plugin that replaces args wholesale it always fails (that one
    /// swaps output.args for a new object, and the host only executes the original args
    /// in the closure). It must also be idempotent and preserve foreign entries and other keys.
    #[test]
    fn opencode_plugin_registration_prepends_and_preserves_foreign() {
        with_temp_home("opencode-register", || {
            let cfg = mcp_config_target("opencode").unwrap().0;
            std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
            std::fs::write(&cfg, r#"{"$schema":"x","plugin":["some-foreign-plugin@1.0.0"]}"#).unwrap();

            ensure_installed("opencode").unwrap();
            let dir = config_path("opencode").unwrap().to_string_lossy().into_owned();
            let v = read_json(&cfg);
            let arr: Vec<String> = v["plugin"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect();
            assert_eq!(arr.first().map(String::as_str), Some(dir.as_str()), "must come first");
            assert!(arr.iter().any(|x| x == "some-foreign-plugin@1.0.0"), "must preserve foreign entries");
            assert_eq!(v["$schema"], "x", "must preserve other keys");

            // Idempotent: running again must not insert a duplicate.
            ensure_installed("opencode").unwrap();
            assert_eq!(
                read_json(&cfg)["plugin"].as_array().unwrap().len(),
                2,
                "repeated calls must not append"
            );
        });
    }

    #[test]
    fn shim_materializes_and_selfheals() {
        with_temp_home("shim", || {
            let shim = shim_path();
            assert!(!shim.exists());
            ensure_shim_for_real().unwrap();
            assert!(shim.is_file());
            // up-to-date shim: a second call is a no-op that keeps the file
            ensure_shim_for_real().unwrap();
            assert!(shim.is_file());
            // deleted shim comes back on the next pass
            let _ = std::fs::remove_file(&shim);
            assert!(!shim.exists());
            ensure_shim_for_real().unwrap();
            assert!(shim.is_file());
        });
    }

    /// The observed bug: a dev-checkout path baked into settings.json keeps matching
    /// `is_ours`, so connect short-circuits forever. refresh_installations must repoint it.
    #[test]
    fn refresh_rewrites_stale_hook_paths() {
        with_temp_home("stale", || {
            let stale = "/gone/checkout/OpenCapX/src-tauri/target/debug/opencapx";
            let path = config_path("claude").unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let v0 = json!({"hooks": {"Stop": [
                {"hooks": [{"type": "command", "command": format!("\"{}\" hook --agent claude", stale)}]},
                {"hooks": [{"type": "command", "command": "other-tool"}]}
            ]}});
            std::fs::write(&path, v0.to_string()).unwrap();
            assert!(is_installed("claude"), "stale entry still counts as ours (the bug)");

            assert!(refresh_installations() >= 1);
            let v = read_json(&path);
            let cmd = v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(binary_from(cmd), shim_path().to_string_lossy());
            assert_eq!(v["hooks"]["Stop"][1]["hooks"][0]["command"], "other-tool", "foreign entries untouched");

            // idempotent: nothing left to fix
            assert_eq!(refresh_installations(), 0);
        });
    }

    #[test]
    fn refresh_rewrites_stale_claude_mcp_command() {
        with_temp_home("mcp-stale", || {
            let path = mcp_config_target("claude").unwrap().0;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                json!({"mcpServers": {"opencapx": {"command": "/gone/debug/opencapx", "args": ["mcp"]}}}).to_string(),
            )
            .unwrap();
            assert!(refresh_installations() >= 1);
            let v = read_json(&path);
            assert_eq!(
                v["mcpServers"]["opencapx"]["command"],
                json!(shim_path().to_string_lossy().to_string())
            );
            assert_eq!(v["mcpServers"]["opencapx"]["args"][0], "mcp", "args untouched");
            assert_eq!(refresh_installations(), 0);
        });
    }

    #[test]
    fn refresh_rewrites_stale_codex_toml_command() {
        with_temp_home("toml-stale", || {
            let path = mcp_config_target("codex").unwrap().0;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                "[mcp_servers.other]\ncommand = \"keep-me\"\n\n[mcp_servers.opencapx]\ncommand = \"/gone/debug/opencapx\"\nargs = [\"mcp\"]\n",
            )
            .unwrap();

            assert!(refresh_installations() >= 1);
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains(&format!("command = \"{}\"", shim_path().to_string_lossy())));
            assert!(text.contains("args = [\"mcp\"]"), "block body preserved");
            assert!(
                text.contains("env = { OPEN_CAPX_AGENT = \"codex\" }"),
                "repair must backfill the identity env into blocks written before it existed"
            );
            assert!(text.contains("command = \"keep-me\""), "foreign block untouched");
            assert_eq!(refresh_installations(), 0);
        });
    }

    /// The observed bug: hooks record the session as `codex` (explicit `--agent codex`),
    /// while the MCP child had no CODEX env to sniff, registered as `custom`, and the one
    /// session split across two agents (audit / grouping / authorization). A block whose
    /// command is current must still be repaired when the identity env is missing.
    #[test]
    fn refresh_backfills_codex_identity_env() {
        with_temp_home("toml-env", || {
            let path = mcp_config_target("codex").unwrap().0;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let current = shim_path().to_string_lossy().to_string();
            std::fs::write(
                &path,
                format!("[mcp_servers.opencapx]\ncommand = \"{}\"\nargs = [\"mcp\"]\n", current),
            )
            .unwrap();

            assert!(refresh_installations() >= 1, "env-less block must be repaired even when the command is current");
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("env = { OPEN_CAPX_AGENT = \"codex\" }"), "identity env must be backfilled");
            assert!(text.contains(&format!("command = \"{}\"", current)), "current command untouched");
            assert_eq!(refresh_installations(), 0, "idempotent");
        });
    }

    /// Backfilling into a block a user already extended: merge our key, keep theirs, and
    /// leave the nested `[mcp_servers.opencapx.env]` table form alone — writing a second
    /// `env` key into the same table would be a TOML duplicate-key error, and a malformed
    /// edit is worse than a missing backfill.
    #[test]
    fn refresh_merges_codex_identity_env_without_clobbering_user_keys() {
        with_temp_home("toml-env-merge", || {
            let path = mcp_config_target("codex").unwrap().0;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let current = shim_path().to_string_lossy().to_string();
            std::fs::write(
                &path,
                format!(
                    "[mcp_servers.opencapx]\ncommand = \"{}\"\nargs = [\"mcp\"]\nenv = {{ FOO = \"bar\" }}\n",
                    current
                ),
            )
            .unwrap();

            assert!(refresh_installations() >= 1);
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("OPEN_CAPX_AGENT = \"codex\""), "identity env must be merged in");
            assert!(text.contains("FOO = \"bar\""), "user's env keys must survive");
            assert_eq!(refresh_installations(), 0);

            let nested = format!(
                "[mcp_servers.opencapx]\ncommand = \"{}\"\n[mcp_servers.opencapx.env]\nFOO = \"bar\"\n",
                current
            );
            std::fs::write(&path, &nested).unwrap();
            assert_eq!(refresh_installations(), 0, "nested env table must be left untouched");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), nested);
        });
    }
}
