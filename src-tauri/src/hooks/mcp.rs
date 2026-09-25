//! the MCP side of connect: per-host config targets, idempotent entry writing, identity backfill, toggle.
//! Mechanical move from hooks.rs.

use super::*;

pub(crate) fn mcp_config_target(kind: &str) -> Option<(PathBuf, &'static str)> {
    let home = crate::core::home_dir()?;
    Some(match kind {
        // Claude Code: the global MCP table lives in ~/.claude.json (separate from the hooks settings.json)
        "claude" => (home.join(".claude.json"), "json"),
        "codex" => (home.join(".codex").join("config.toml"), "toml"),
        "opencode" => (
            home.join(".config").join("opencode").join("opencode.json"),
            "json",
        ),
        "omp" => (home.join(".omp").join("agent").join("mcp.json"), "json"),
        _ => return None,
    })
}

/// Whether this host has an MCP config target. Hooks-only hosts (pi, omp) legitimately don't;
/// `connect` treats that as a note, not a failure.
pub fn supports_mcp(kind: &str) -> bool {
    mcp_config_target(kind).is_some()
}

pub(crate) fn mcp_entry_is_ours(kind: &str, v: &Value) -> bool {
    let entry = match kind {
        "claude" | "omp" => v.get("mcpServers").and_then(|m| m.get("opencapx")),
        "opencode" => v.get("mcp").and_then(|m| m.get("opencapx")),
        _ => return false,
    };
    let cmd_hit = |c: &Value| {
        c.as_str()
            .map(|s| s.to_lowercase().contains("opencapx"))
            .unwrap_or(false)
            || c.as_array()
                .map(|a| {
                    a.iter().any(|x| {
                        x.as_str()
                            .map(|s| s.to_lowercase().contains("opencapx"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
    };
    match kind {
        "claude" | "omp" => entry
            .and_then(|e| e.get("command"))
            .map(cmd_hit)
            .unwrap_or(false),
        "opencode" => entry
            .and_then(|e| e.get("command"))
            .map(cmd_hit)
            .unwrap_or(false),
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
pub(crate) const CODEX_IDENTITY_ENV: &str = "OPEN_CAPX_AGENT = \"codex\"";

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
            let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
            servers
                .as_object_mut()
                .ok_or("mcpServers is not an object")?
                .insert(
                    "opencapx".into(),
                    json!({ "command": exe, "args": ["mcp"] }),
                );
        }
        "opencode" => {
            let mcp = obj.entry("mcp").or_insert_with(|| json!({}));
            mcp.as_object_mut().ok_or("mcp is not an object")?.insert(
                "opencapx".into(),
                json!({ "type": "local", "command": [exe, "mcp"] }),
            );
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
            let e = entry
                .as_object_mut()
                .ok_or("opencapx entry is not an object")?;
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
