//! startup reconciliation: refresh_installations, stale-command repair, MCP entry and TOML block reconciliation.
//! Mechanical move from hooks.rs.

use super::*;

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
                            if cur.contains("opencapx")
                                && cur != want_content
                                && std::fs::write(&f, want_content).is_ok()
                            {
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
                    if v.is_object()
                        && fix_stale_hook_commands(&mut v, &s, kind, &want)
                        && write_json(&path, &v).is_ok()
                    {
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
    let Some(map) = v.get_mut(key).and_then(|h| h.as_object_mut()) else {
        return false;
    };
    let mut changed = false;
    for event in s.events {
        if let Some(arr) = map.get_mut(*event).and_then(|a| a.as_array_mut()) {
            for entry in arr.iter_mut() {
                if entry_is_ours(s.style, event, entry) && fix_entry_command(entry, &cmd, want_bin)
                {
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
        Some(arr) => arr
            .iter_mut()
            .filter_map(|h| h.get_mut("command"))
            .collect(),
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
    let Some((path, fmt)) = mcp_config_target(kind) else {
        return false;
    };
    if fmt == "toml" {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let Some(updated) = reconcile_toml_block(&text, want) else {
            return false;
        };
        return std::fs::write(&path, updated).is_ok();
    }
    let mut v = read_json(&path);
    let Some(obj) = v.as_object_mut() else {
        return false;
    };
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
    let indent_of = |l: &str| {
        l.chars()
            .take_while(|c| c.is_whitespace())
            .collect::<String>()
    };

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
                .or_else(|| {
                    lines
                        .iter()
                        .rposition(|l| key_of(l) == "command" && balanced(l))
                });
            if let Some(anchor) = anchor {
                let indent = indent_of(&lines[anchor]);
                lines.insert(
                    anchor + 1,
                    format!("{}env = {{ {} }}", indent, CODEX_IDENTITY_ENV),
                );
                changed = true;
            }
            // No balanced anchor: skip the env backfill but keep any command repair already
            // made above — a partial repair beats discarding it.
        }
    }

    if !changed {
        return None;
    }
    Some(format!(
        "{}{}{}",
        &text[..start],
        lines.join("\n"),
        &text[block_end..]
    ))
}
