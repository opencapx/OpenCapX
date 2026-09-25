//! the host catalog: AgentInfo, per-host Spec (config style, events, MCP support), the AGENTS table, and config-path resolution.
//! Mechanical move from hooks.rs.

use super::*;

#[derive(Serialize, Clone)]
pub struct AgentInfo {
    pub kind: String,
    pub display_name: String,
    pub installed: bool,
    pub note: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Style {
    ClaudeNested,
    CursorFlat,
    WindsurfFlat,
    KiroFlat,
    AntigravityNested,
    OpencodePluginModule,
    PiExtension,
    OmpExtension,
}

pub(crate) struct Spec {
    pub(crate) style: Style,
    rel_path: &'static [&'static str],
    pub(crate) events: &'static [&'static str],
}

pub(crate) fn spec(kind: &str) -> Option<Spec> {
    Some(match kind {
        "claude" => Spec {
            style: Style::ClaudeNested,
            rel_path: &[".claude", "settings.json"],
            events: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "Notification",
                "Stop",
                "SubagentStop",
                "SessionEnd",
            ],
        },
        "codex" => Spec {
            style: Style::ClaudeNested,
            rel_path: &[".codex", "hooks.json"],
            events: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PermissionRequest",
                "Stop",
                "SubagentStop",
            ],
        },
        "gemini" => Spec {
            style: Style::ClaudeNested,
            rel_path: &[".gemini", "settings.json"],
            events: &[
                "SessionStart",
                "BeforeAgent",
                "BeforeTool",
                "AfterTool",
                "Notification",
                "AfterAgent",
                "SessionEnd",
            ],
        },
        "cursor" => Spec {
            style: Style::CursorFlat,
            rel_path: &[".cursor", "hooks.json"],
            events: &[
                "sessionStart",
                "beforeSubmitPrompt",
                "preToolUse",
                "stop",
                "subagentStop",
                "sessionEnd",
            ],
        },
        "copilot" => Spec {
            style: Style::CursorFlat,
            rel_path: &[".copilot", "hooks", "opencapx.json"],
            events: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
            ],
        },
        "windsurf" => Spec {
            style: Style::WindsurfFlat,
            rel_path: &[".codeium", "windsurf", "hooks.json"],
            events: &["pre_user_prompt", "post_cascade_response"],
        },
        "antigravity" => Spec {
            style: Style::AntigravityNested,
            rel_path: &[".gemini", "config", "hooks.json"],
            events: &["PreInvocation", "PreToolUse", "PostToolUse", "Stop"],
        },
        "kiro" => Spec {
            style: Style::KiroFlat,
            rel_path: &[".kiro", "agents", "default.json"],
            events: &["agentSpawn", "userPromptSubmit", "postToolUse", "stop"],
        },
        "opencode" => Spec {
            style: Style::OpencodePluginModule,
            rel_path: &[".local", "share", "opencapx", "adapters", "opencode"],
            events: &[],
        },
        "droid" => Spec {
            style: Style::ClaudeNested,
            rel_path: &[".factory", "hooks.json"],
            events: &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "Notification",
                "Stop",
                "SubagentStop",
                "SessionEnd",
            ],
        },
        "pi" => Spec {
            style: Style::PiExtension,
            rel_path: &[".pi", "agent", "extensions", "opencapx.ts"],
            events: &[],
        },
        "omp" => Spec {
            style: Style::OmpExtension,
            rel_path: &[".omp", "agent", "extensions", "opencapx.ts"],
            events: &[],
        },
        "grok" => Spec {
            style: Style::ClaudeNested,
            rel_path: &[".grok", "hooks", "opencapx.json"],
            events: &[
                "SessionStart",
                "UserPromptSubmit",
                "PostToolUse",
                "Notification",
                "Stop",
                "SessionEnd",
            ],
        },
        _ => return None,
    })
}

/// Supported agent table: (kind, display name, note). The display name is also
/// used by the tray/notifications (claude → "Claude Code"), so maintain it only here.
pub(crate) const AGENTS: &[(&str, &str, Option<&str>)] = &[
    ("claude", "Claude Code", None),
    (
        "codex",
        "Codex",
        Some("After enabling, run /hooks in Codex and Trust the OpenCapX hook"),
    ),
    ("gemini", "Gemini CLI", None),
    ("cursor", "Cursor", None),
    ("opencode", "opencode", None),
    (
        "windsurf",
        "Windsurf",
        Some("No \"needs input\" alerts (Windsurf has no such hook)"),
    ),
    (
        "antigravity",
        "Antigravity",
        Some("No \"needs input\" alerts (Antigravity has no notification hook)"),
    ),
    (
        "copilot",
        "GitHub Copilot",
        Some("Copilot CLI only (~/.copilot/hooks)"),
    ),
    ("kiro", "Kiro CLI", Some("Hooks the default Kiro CLI agent")),
    (
        "droid",
        "Factory Droid",
        Some("Factory Droid CLI (~/.factory/hooks.json)"),
    ),
    (
        "pi",
        "Pi",
        Some("Pi extension (~/.pi/agent/extensions). No \"needs input\" alerts"),
    ),
    (
        "omp",
        "Oh My Pi",
        Some("OMP extension (~/.omp/agent/extensions). No \"needs input\" alerts"),
    ),
    (
        "grok",
        "Grok Build",
        Some("xAI Grok Build CLI (~/.grok/hooks/opencapx.json)"),
    ),
];

pub fn catalog() -> Vec<AgentInfo> {
    AGENTS
        .iter()
        .map(|(kind, name, note)| AgentInfo {
            kind: kind.to_string(),
            display_name: name.to_string(),
            installed: is_installed(kind),
            note: note.map(|s| s.to_string()),
        })
        .collect()
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

pub(crate) fn config_path(kind: &str) -> Option<PathBuf> {
    let mut p = crate::core::home_dir()?;
    for part in spec(kind)?.rel_path {
        p.push(part);
    }
    Some(p)
}
