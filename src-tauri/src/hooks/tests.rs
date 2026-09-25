use super::*;

use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Escape a filesystem path the way the TOML/JSON config writers do. On Windows the
/// shim path is full of backslashes; raw comparisons against written (correctly
/// escaped) config text used to fail, and raw fixtures made valid repairs look stale.
fn esc_path(p: &str) -> String {
    p.replace('\\', "\\\\").replace('"', "\\\"")
}

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
        let dir = config_path("opencode")
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            plugin_array_contains(&dir),
            "install must write the plugin directory into the plugin array"
        );

        assert_eq!(toggle("opencode"), Ok(false));
        assert!(!is_installed("opencode"));
        assert!(
            !plugin_array_contains(&dir),
            "uninstall must remove it from the plugin array"
        );
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
        std::fs::write(
            &path,
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"other-tool"}]}]}}"#,
        )
        .unwrap();
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
        assert_eq!(
            v4["mcpServers"]["opencapx"]["env"]["OPEN_CAPX_AGENT"],
            "omp"
        );
        assert!(mcp_entry_is_ours("omp", &v4));
        assert_eq!(ensure_mcp("omp").unwrap().1, false);

        assert!(ensure_mcp("nope").is_err());
    });
}

#[test]
fn ensure_mcp_omp_preserves_existing_servers() {
    with_temp_home("omp-mcp-keep", || {
        let path = crate::core::home_dir()
            .unwrap()
            .join(".omp")
            .join("agent")
            .join("mcp.json");
        write_json(
            &path,
            &json!({"mcpServers": {"other": {"command": "/x/other"}}}),
        )
        .unwrap();
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
        let path = crate::core::home_dir()
            .unwrap()
            .join(".omp")
            .join("agent")
            .join("mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let shim = shim_path().to_string_lossy().to_string();
        write_json(
            &path,
            &json!({"mcpServers": {"opencapx": {
                "command": shim, "args": ["mcp"], "env": {"FOO": "bar"}}}}),
        )
        .unwrap();

        let (_, w) = ensure_mcp("omp").unwrap();
        assert!(
            w,
            "env-less entry must be repaired even when the command is current"
        );
        let v = read_json(&path);
        assert_eq!(v["mcpServers"]["opencapx"]["env"]["OPEN_CAPX_AGENT"], "omp");
        assert_eq!(
            v["mcpServers"]["opencapx"]["env"]["FOO"], "bar",
            "user env keys must survive"
        );
        assert_eq!(ensure_mcp("omp").unwrap().1, false, "idempotent");
    });
}

#[test]
fn refresh_repoints_a_stale_omp_mcp_command() {
    with_temp_home("omp-mcp-repoint", || {
        let path = crate::core::home_dir()
            .unwrap()
            .join(".omp")
            .join("agent")
            .join("mcp.json");
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
        let arr = v["hooks"]["preToolUse"]
            .as_array()
            .expect("preToolUse entry must exist");
        assert!(arr
            .iter()
            .any(|e| e["command"].as_str().map(is_ours).unwrap_or(false)));
    });
}

#[test]
fn copilot_config_includes_pretooluse_for_writeback() {
    with_temp_home("copilot-pretool", || {
        ensure_installed("copilot").unwrap();
        let path = config_path("copilot").unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            v["version"], 1,
            "copilot rejects a hook config without a version"
        );
        let arr = v["hooks"]["PreToolUse"]
            .as_array()
            .expect("PreToolUse entry must exist");
        assert!(arr
            .iter()
            .any(|e| e["command"].as_str().map(is_ours).unwrap_or(false)));
    });
}

#[test]
fn connect_ensure_mcp_preserves_existing_config() {
    with_temp_home("mcp-keep", || {
        let path = crate::core::home_dir().unwrap().join(".claude.json");
        write_json(
            &path,
            &json!({
                "other": { "keep": 1 },
                "mcpServers": { "fs": { "command": "/x/fs" } }
            }),
        )
        .unwrap();
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
    assert!(
        supports_mcp("omp"),
        "omp gets an MCP entry in ~/.omp/agent/mcp.json"
    );
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
    assert!(
        js.contains("tool.execute.before"),
        "must hook tool pre-execution"
    );
    assert!(
        js.contains("\"rewrite\", cmd"),
        "must delegate to opencapx rewrite"
    );
    assert!(
        js.contains("output.args.command = rewritten"),
        "must mutate the property in place"
    );
    assert!(js.contains("\"opencode\""), "must call itself opencode");
    assert!(!js.contains("claude"), "must not be mislabeled as claude");
    assert!(
        !js.contains("__OPENCAPX_BIN__"),
        "placeholder must be substituted"
    );
    assert!(
        js.contains("\"/usr/local/bin/opencapx\""),
        "binary path must be injected"
    );
}

/// Re-running connect after a template upgrade must update the on-disk plugin file — otherwise the old template lingers forever.
#[test]
fn ensure_installed_refreshes_a_stale_plugin_file() {
    with_temp_home("opencode-refresh", || {
        let dir = config_path("opencode").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        // Old template: must still contain opencapx + hook, otherwise is_ours says "not ours" and is_installed is false.
        std::fs::write(
            dir.join("index.js"),
            "// OpenCapX integration (old)\n// hook\n",
        )
        .unwrap();
        assert!(
            is_installed("opencode"),
            "old content still counts as installed"
        );
        assert!(
            stale_plugin_file("opencode"),
            "old content must be judged as needing refresh"
        );

        ensure_installed("opencode").unwrap();
        let now = std::fs::read_to_string(dir.join("index.js")).unwrap();
        assert!(
            now.contains("tool.execute.before"),
            "should refresh to the current template"
        );
        assert!(
            !stale_plugin_file("opencode"),
            "should no longer be judged stale after refresh"
        );
        assert!(
            dir.join("package.json").exists(),
            "must write package.json (declaring type: module)"
        );

        // settings.json-style plugins are unaffected by the content refresh logic.
        assert!(
            !stale_plugin_file("claude"),
            "always false for non-file types"
        );
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
            crate::core::home_dir()
                .unwrap()
                .join(".omp")
                .join("agent")
                .join("extensions")
                .join("opencapx.ts")
        );
        assert!(
            !stale_plugin_file("omp"),
            "freshly installed content is current"
        );
        assert_eq!(toggle("omp"), Ok(false));
        assert!(!is_installed("omp"));
        assert!(!path.exists());
    });
}

#[test]
fn omp_extension_reports_lifecycle_and_carries_the_binary() {
    let ts = omp_extension("/usr/local/bin/opencapx");
    assert!(
        ts.contains("pi.on(\"session_start\""),
        "must report session lifecycle"
    );
    assert!(
        ts.contains("\"hook\", \"--agent\", AGENT"),
        "must call the hook as omp"
    );
    assert!(ts.contains("const AGENT = \"omp\""), "must call itself omp");
    assert!(
        !ts.contains("__OPENCAPX_BIN__"),
        "placeholder must be substituted"
    );
    assert!(
        ts.contains("\"/usr/local/bin/opencapx\""),
        "binary path must be injected"
    );
}

#[test]
fn omp_extension_rewrites_tool_calls() {
    let ts = omp_extension("/usr/local/bin/opencapx");
    assert!(
        ts.contains("pi.on(\"tool_call\""),
        "must hook tool pre-execution"
    );
    assert!(
        ts.contains("hook_event_name: \"PreToolUse\""),
        "must ask as a PreToolUse event"
    );
    assert!(
        ts.contains("return { input:"),
        "must return the replacement input, not mutate"
    );
    assert!(
        ts.contains("spawnSync"),
        "the reply must be read synchronously"
    );
}

#[test]
fn ensure_installed_refreshes_a_stale_omp_extension() {
    with_temp_home("omp-refresh", || {
        let path = config_path("omp").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Old template must still contain opencapx + hook, otherwise is_ours says "not ours".
        std::fs::write(&path, "// OpenCapX integration (old)\n// hook\n").unwrap();
        assert!(is_installed("omp"), "old content still counts as installed");
        assert!(
            stale_plugin_file("omp"),
            "old content must be judged as needing refresh"
        );
        ensure_installed("omp").unwrap();
        assert!(
            !stale_plugin_file("omp"),
            "must refresh to the current template"
        );
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("session_start"));
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
        assert!(text.contains(&esc_path(&shim_path().to_string_lossy())));
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
        std::fs::write(
            &cfg,
            r#"{"$schema":"x","plugin":["some-foreign-plugin@1.0.0"]}"#,
        )
        .unwrap();

        ensure_installed("opencode").unwrap();
        let dir = config_path("opencode")
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let v = read_json(&cfg);
        let arr: Vec<String> = v["plugin"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        assert_eq!(
            arr.first().map(String::as_str),
            Some(dir.as_str()),
            "must come first"
        );
        assert!(
            arr.iter().any(|x| x == "some-foreign-plugin@1.0.0"),
            "must preserve foreign entries"
        );
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
        assert!(
            is_installed("claude"),
            "stale entry still counts as ours (the bug)"
        );

        assert!(refresh_installations() >= 1);
        let v = read_json(&path);
        let cmd = v["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(binary_from(cmd), shim_path().to_string_lossy());
        assert_eq!(
            v["hooks"]["Stop"][1]["hooks"][0]["command"], "other-tool",
            "foreign entries untouched"
        );

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
        assert_eq!(
            v["mcpServers"]["opencapx"]["args"][0], "mcp",
            "args untouched"
        );
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
        assert!(text.contains(&format!(
            "command = \"{}\"",
            esc_path(&shim_path().to_string_lossy())
        )));
        assert!(text.contains("args = [\"mcp\"]"), "block body preserved");
        assert!(
            text.contains("env = { OPEN_CAPX_AGENT = \"codex\" }"),
            "repair must backfill the identity env into blocks written before it existed"
        );
        assert!(
            text.contains("command = \"keep-me\""),
            "foreign block untouched"
        );
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
        let current = esc_path(&shim_path().to_string_lossy());
        std::fs::write(
            &path,
            format!(
                "[mcp_servers.opencapx]\ncommand = \"{}\"\nargs = [\"mcp\"]\n",
                current
            ),
        )
        .unwrap();

        assert!(
            refresh_installations() >= 1,
            "env-less block must be repaired even when the command is current"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("env = { OPEN_CAPX_AGENT = \"codex\" }"),
            "identity env must be backfilled"
        );
        assert!(
            text.contains(&format!("command = \"{}\"", current)),
            "current command untouched"
        );
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
        let current = esc_path(&shim_path().to_string_lossy());
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
        assert!(
            text.contains("OPEN_CAPX_AGENT = \"codex\""),
            "identity env must be merged in"
        );
        assert!(
            text.contains("FOO = \"bar\""),
            "user's env keys must survive"
        );
        assert_eq!(refresh_installations(), 0);

        let nested = format!(
            "[mcp_servers.opencapx]\ncommand = \"{}\"\n[mcp_servers.opencapx.env]\nFOO = \"bar\"\n",
            current
        );
        std::fs::write(&path, &nested).unwrap();
        assert_eq!(
            refresh_installations(),
            0,
            "nested env table must be left untouched"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), nested);
    });
}
