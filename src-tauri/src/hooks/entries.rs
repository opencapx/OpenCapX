//! hook-entry JSON helpers: read/write, ownership predicates, entry construction, is_installed/ensure_installed.
//! Mechanical move from hooks.rs.

use super::*;

pub(crate) fn read_json(path: &PathBuf) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| {
            if s.trim().is_empty() {
                None
            } else {
                serde_json::from_str(&s).ok()
            }
        })
        .unwrap_or_else(|| json!({}))
}

pub(crate) fn write_json(path: &PathBuf, v: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(v).unwrap_or_default())
}

pub(crate) fn container_key(style: Style) -> &'static str {
    if style == Style::AntigravityNested {
        "opencapx"
    } else {
        "hooks"
    }
}

fn antigravity_matcher(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PostToolUse")
}

fn group_is_ours(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|a| {
            a.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_ours)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn flat_is_ours(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .map(is_ours)
        .unwrap_or(false)
}

pub(crate) fn entry_is_ours(style: Style, event: &str, entry: &Value) -> bool {
    match style {
        Style::ClaudeNested => group_is_ours(entry),
        Style::AntigravityNested => {
            if antigravity_matcher(event) {
                group_is_ours(entry)
            } else {
                flat_is_ours(entry)
            }
        }
        _ => flat_is_ours(entry),
    }
}

pub(crate) fn make_entry(style: Style, event: &str, cmd: &str) -> Value {
    match style {
        Style::ClaudeNested => json!({ "hooks": [{ "type": "command", "command": cmd }] }),
        Style::CursorFlat => json!({ "command": cmd, "type": "command" }),
        Style::WindsurfFlat => json!({ "command": cmd, "show_output": false }),
        Style::KiroFlat => json!({ "command": cmd }),
        Style::AntigravityNested => {
            if antigravity_matcher(event) {
                json!({ "matcher": "*", "hooks": [{ "type": "command", "command": cmd }] })
            } else {
                json!({ "type": "command", "command": cmd })
            }
        }
        Style::OpencodePluginModule | Style::PiExtension | Style::OmpExtension => Value::Null,
    }
}

pub fn is_installed(kind: &str) -> bool {
    let (Some(path), Some(s)) = (config_path(kind), spec(kind)) else {
        return false;
    };
    if s.style == Style::OpencodePluginModule {
        return plugin_module_file(kind)
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|c| is_ours(&c))
            .unwrap_or(false);
    }
    if s.style == Style::PiExtension || s.style == Style::OmpExtension {
        return std::fs::read_to_string(&path)
            .map(|c| is_ours(&c))
            .unwrap_or(false);
    }
    let v = read_json(&path);
    let Some(map) = v.get(container_key(s.style)).and_then(|h| h.as_object()) else {
        return false;
    };
    s.events.iter().any(|event| {
        map.get(*event)
            .and_then(|a| a.as_array())
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
pub(crate) fn stale_plugin_file(kind: &str) -> bool {
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
