//! embedded adapter assets (opencode plugin JS, OMP extension TS, pi extension) and their (un)registration.
//! Mechanical move from hooks.rs.

use super::*;

/// opencode plugin source file — real JS, see adapters/README.md.
/// `"__OPENCAPX_BIN__"` (including quotes) is replaced wholesale with a JSON string literal, safe even when the path contains backslashes.
const OPENCODE_PLUGIN_JS: &str = include_str!("../../../adapters/opencode/plugin.js");

pub(crate) fn opencode_plugin(binary: &str) -> String {
    let bin = serde_json::to_string(binary).unwrap_or_else(|_| format!("\"{}\"", binary));
    OPENCODE_PLUGIN_JS.replace("\"__OPENCAPX_BIN__\"", &bin)
}

/// OMP (oh-my-pi) extension source file — real TS, see adapters/README.md.
/// Same placeholder contract as opencode's: `"__OPENCAPX_BIN__"` (including quotes) is replaced
/// wholesale with a JSON string literal, safe even when the path contains backslashes.
const OMP_EXTENSION_TS: &str = include_str!("../../../adapters/omp/extension.ts");

pub(crate) fn omp_extension(binary: &str) -> String {
    let bin = serde_json::to_string(binary).unwrap_or_else(|_| format!("\"{}\"", binary));
    OMP_EXTENSION_TS.replace("\"__OPENCAPX_BIN__\"", &bin)
}

/// The plugin module's entry file (`<dir>/index.js`).
pub(crate) fn plugin_module_file(kind: &str) -> Option<PathBuf> {
    config_path(kind).map(|d| d.join("index.js"))
}

pub(crate) fn opencode_plugin_package_json() -> String {
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
pub(crate) fn register_opencode_plugin(dir: &std::path::Path) -> std::io::Result<()> {
    let Some((path, _)) = mcp_config_target("opencode") else {
        return Ok(());
    };
    let mut v = read_json(&path);
    let obj = v.as_object_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "opencode.json is not a JSON object",
        )
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
pub(crate) fn unregister_opencode_plugin(dir: &std::path::Path) {
    let Some((path, _)) = mcp_config_target("opencode") else {
        return;
    };
    let mut v = read_json(&path);
    let Some(arr) = v.get_mut("plugin").and_then(|a| a.as_array_mut()) else {
        return;
    };
    let me = dir.to_string_lossy().into_owned();
    let before = arr.len();
    arr.retain(|x| x.as_str() != Some(me.as_str()));
    if arr.len() != before {
        let _ = write_json(&path, &v);
    }
}

pub(crate) fn pi_extension(binary: &str) -> String {
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

pub(crate) fn enable_codex_hooks() {
    let Some(home) = crate::core::home_dir() else {
        return;
    };
    let path = home.join(".codex").join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let already = text.lines().any(|l| {
        let c = l.trim().replace(' ', "");
        !c.starts_with('#') && c.starts_with("hooks=true")
    });
    if already {
        return;
    }
    let updated = if let Some(idx) = text.lines().position(|l| l.trim() == "[features]") {
        let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
        lines.insert(idx + 1, "hooks = true".into());
        lines.join("\n")
    } else {
        let mut t = text;
        if !t.is_empty() && !t.ends_with('\n') {
            t.push('\n');
        }
        t.push_str("\n[features]\nhooks = true\n");
        t
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, updated);
}
