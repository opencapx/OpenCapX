//! Command lifecycle rules (P0): user-configurable command interception/transforms.
//!
//! Positioning: OpenCapX only **routes** commands to the executor the user chose (`curl x` → `sandbox curl x`);
//! the executor owns the boundary; OpenCapX makes no allow/deny decision (it only rewrites).
//!
//! ```text
//! agent initiates a Bash tool call
//!   → PreToolUse hook(opencapx hook --agent claude)
//!   → read rules ~/.opencapx/rules.json (+ trusted project-level)
//!   → match → write back updatedInput (the rewritten command)
//! ```
//!
//! - Rule file layering: builtin (empty) → global `~/.opencapx/rules.json` → project-level (requires trust)
//! - The observation channel is unchanged (dumb pipe); rewrites go through a separate stdout response channel
//! - Spec doc: `docs/rules.md`; contract: `docs/events.md`

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod transform;

/// Rules file schema version. Independent of the app version; evolution is additive only.
pub const RULES_SCHEMA_VERSION: u32 = 1;

/// Lifecycle stages. P0 only implements `ToolPre`; the rest are P1+ placeholders (parsed, no engine action yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    SessionStart,
    PromptSubmit,
    ToolPre,
    ToolPost,
    TurnStop,
    SessionEnd,
}

/// Actions. P0 only implements `Rewrite`; the rest are P1+ placeholders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Rewrite,
    Inject,
    Observe,
    Verify,
}

/// Rules file top level `~/.opencapx/rules.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesFile {
    pub version: u32,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub when: When,
    #[serde(default)]
    pub unless: Option<When>,
    pub then: Then,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    pub stage: Stage,
    /// Command matching (P0 uses it only when `stage == ToolPre`).
    #[serde(default)]
    pub command: Option<CommandMatch>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandMatch {
    /// Anchored prefix, e.g. `"curl "`.
    #[serde(default)]
    pub prefix: Option<String>,
    /// First token (executable name), e.g. `"curl"`.
    #[serde(default)]
    pub binary: Option<String>,
    /// Anchored regex (compilation failure = this rule is inert).
    #[serde(default)]
    pub regex: Option<String>,
}

/// Transform operators. **Declarative** — no shell strings may appear; `deny_unknown_fields` guarantees
/// any undeclared field (e.g. `shell`) is rejected at parse time (D3).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Then {
    pub action: Action,
    /// `curl x` → `sandbox curl x`
    #[serde(default)]
    pub prepend: Option<String>,
    #[serde(default)]
    pub replace_binary: Option<ReplaceBinary>,
    /// `HTTPS_PROXY=... curl x`
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    // ---- P1+ placeholders (parsed, no engine action yet) ----
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub on: Option<String>,
    #[serde(default)]
    pub inject_back: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceBinary {
    pub from: String,
    pub to: String,
}

fn default_true() -> bool {
    true
}

impl RulesFile {
    /// Parse + validate. Unknown version, unknown fields, and illegal action combinations all return `Err`
    /// (the load layer is fail-closed; the engine layer then fail-open — see `load`).
    pub fn parse(s: &str) -> Result<RulesFile, String> {
        let f: RulesFile =
            serde_json::from_str(s).map_err(|e| format!("rules: parse error: {e}"))?;
        f.validate()?;
        Ok(f)
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != RULES_SCHEMA_VERSION {
            return Err(format!(
                "rules: unsupported version {} (expected {})",
                self.version, RULES_SCHEMA_VERSION
            ));
        }
        for r in &self.rules {
            if r.id.trim().is_empty() {
                return Err("rules: rule with empty id".into());
            }
            if r.then.action == Action::Rewrite {
                if r.when.command.is_none() {
                    return Err(format!(
                        "rules: rule {}: rewrite requires when.command",
                        r.id
                    ));
                }
                let has_op = r.then.prepend.is_some()
                    || r.then.replace_binary.is_some()
                    || r.then.env.is_some();
                if !has_op {
                    return Err(format!(
                        "rules: rule {}: rewrite requires one of prepend/replace_binary/env",
                        r.id
                    ));
                }
            }
        }
        Ok(())
    }
}

// ===== Layered loading and project-level trust =====

/// Builtin default rules (compiled into the binary). **Currently empty** — no interception is active by default,
/// avoiding a change to the user's existing workflow on upgrade. By default it only "suggests", never "activates".
const BUILTIN: &[Rule] = &[];

/// Load result: rules + source layers + load-time errors.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    pub rules: Vec<Rule>,
    /// Layers that actually took part in the merge (so `opencapx rules list` can label the source).
    pub sources: Vec<PathBuf>,
    /// Load-time errors (bad JSON / illegal rules). fail-open: skip the layer, record it, do not block.
    pub errors: Vec<String>,
}

/// Global rules file `~/.opencapx/rules.json`.
pub fn global_path() -> PathBuf {
    crate::core::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".opencapx")
        .join("rules.json")
}

/// Project-level rules file `<project>/.opencapx/rules.json`.
pub fn project_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".opencapx").join("rules.json")
}

/// Trust registry `~/.opencapx/trusted-projects.json` (0600).
pub fn trust_registry_path() -> PathBuf {
    crate::core::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".opencapx")
        .join("trusted-projects.json")
}

/// Load and merge by layer (low → high): builtin → global → project-level (**only when trusted**).
pub fn load(project_dir: Option<&Path>) -> RuleSet {
    let global = global_path();
    let project = project_dir.map(project_path);
    let trusted = project_dir.map(is_trusted).unwrap_or(false);
    load_layers(&global, project.as_deref(), trusted)
}

fn load_layers(global: &Path, project: Option<&Path>, project_trusted: bool) -> RuleSet {
    let mut set = RuleSet::default();
    if !BUILTIN.is_empty() {
        set.rules.extend(BUILTIN.iter().cloned());
        set.sources.push(PathBuf::from("<builtin>"));
    }
    let layers = std::iter::once((global, true)).chain(project.map(|p| (p, project_trusted)));
    for (path, allowed) in layers {
        if !allowed {
            continue;
        }
        match read_layer(path) {
            Ok(Some(rules)) => {
                for r in rules {
                    // Same id: the inner layer overrides the outer.
                    if let Some(pos) = set.rules.iter().position(|x| x.id == r.id) {
                        set.rules.remove(pos);
                    }
                    set.rules.push(r);
                }
                set.sources.push(path.to_path_buf());
            }
            Ok(None) => {}
            Err(e) => set.errors.push(e),
        }
    }
    set
}

fn read_layer(path: &Path) -> Result<Option<Vec<Rule>>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    RulesFile::parse(&text)
        .map(|f| Some(f.rules))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn canonical(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Whether the project has been explicitly trusted.
pub fn is_trusted(project_dir: &Path) -> bool {
    is_trusted_in(&trust_registry_path(), project_dir)
}

fn is_trusted_in(registry: &Path, dir: &Path) -> bool {
    let target = canonical(dir);
    load_trust_registry(registry)
        .iter()
        .any(|p| canonical(Path::new(p)) == target)
}

/// Trust the project (activating its project-level rules).
pub fn trust(project_dir: &Path) -> Result<(), String> {
    set_trust_in(&trust_registry_path(), project_dir, true)
}

/// Revoke trust (project-level rules stop immediately).
pub fn untrust(project_dir: &Path) -> Result<(), String> {
    set_trust_in(&trust_registry_path(), project_dir, false)
}

fn load_trust_registry(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("projects")?
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        })
        .unwrap_or_default()
}

fn set_trust_in(registry: &Path, dir: &Path, on: bool) -> Result<(), String> {
    let target = canonical(dir);
    let mut list = load_trust_registry(registry);
    list.retain(|p| canonical(Path::new(p)) != target);
    if on {
        list.push(target.to_string_lossy().into_owned());
    }
    let v = serde_json::json!({ "projects": list });
    if let Some(parent) = registry.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(registry, serde_json::to_string_pretty(&v).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(registry, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Build a `RuleSet` directly from a rules JSON blob (the caller has decided the source; the trust registry is not consulted).
pub fn ruleset_from_json(s: &str) -> Result<RuleSet, String> {
    let f = RulesFile::parse(s)?;
    Ok(RuleSet {
        rules: f.rules,
        sources: Vec::new(),
        errors: Vec::new(),
    })
}

/// Rewrite outcome. P0 only has "changed" and "not changed"; `Skipped` auditing is left for later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewriteOutcome {
    Unchanged,
    Rewritten { rule_id: String, command: String },
}

/// Run rewrite rules for the given stage against one command; **stop at the first match** (P0).
pub fn rewrite_command(cmd: &str, stage: Stage, set: &RuleSet) -> RewriteOutcome {
    for rule in &set.rules {
        if !rule.enabled || rule.when.stage != stage || rule.then.action != Action::Rewrite {
            continue;
        }
        if let Some(out) = transform::apply(rule, cmd) {
            return RewriteOutcome::Rewritten {
                rule_id: rule.id.clone(),
                command: out,
            };
        }
    }
    RewriteOutcome::Unchanged
}

/// CLI entry for `opencapx rules <list|explain|trust|untrust>`, returns the process exit code.
/// `explain` is a **dry-run**: it only prints the match chain and the resulting command; it does not execute or output hook JSON.
/// `opencapx rules` — clap owns the parsing and the generated help.
#[derive(clap::Parser)]
#[command(name = "opencapx rules", about = "Command rules: list, explain, trust and untrust rule files")]
struct RulesCli {
    #[command(subcommand)]
    cmd: RulesCmd,
}

#[derive(clap::Subcommand)]
enum RulesCmd {
    /// List the effective rules (global + project + built-in)
    List,
    /// Print the rewritten form of a command (does not execute it)
    Explain {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Trust the project rule file at <path> (default: the current directory)
    Trust { path: Option<PathBuf> },
    /// Stop trusting the project rule file at <path> (default: the current directory)
    Untrust { path: Option<PathBuf> },
}

pub fn run_cli(args: &[String]) -> i32 {
    let cli = match super::cli::parse::<RulesCli>("opencapx rules", args) {
        Ok(c) => c,
        Err(code) => return code,
    };
    match cli.cmd {
        RulesCmd::List => {
            let set = load(std::env::current_dir().ok().as_deref());
            for e in &set.errors {
                eprintln!("rule load error: {e}");
            }
            if set.rules.is_empty() {
                println!("no rules (global: {})", global_path().display());
            }
            for r in &set.rules {
                let mark = if r.enabled { "[x]" } else { "[ ]" };
                println!("{mark} {}", describe_rule(r));
            }
            0
        }
        RulesCmd::Explain { command } => {
            let cmd = command.join(" ");
            let set = load(std::env::current_dir().ok().as_deref());
            match rewrite_command(&cmd, Stage::ToolPre, &set) {
                RewriteOutcome::Rewritten { rule_id, command } => {
                    println!("{rule_id} -> {command}");
                    0
                }
                RewriteOutcome::Unchanged => {
                    println!("(no rule matched)");
                    1
                }
            }
        }
        RulesCmd::Trust { path } => set_trust(true, path),
        RulesCmd::Untrust { path } => set_trust(false, path),
    }
}

fn set_trust(on: bool, path: Option<PathBuf>) -> i32 {
    let dir = match path {
        Some(p) => p,
        None => match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("cannot resolve cwd: {e}");
                return 1;
            }
        },
    };
    match if on { trust(&dir) } else { untrust(&dir) } {
        Ok(()) => {
            println!("{} {}", if on { "trusted" } else { "untrusted" }, dir.display());
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn matcher_desc(r: &Rule) -> String {
    match r.when.command.as_ref() {
        Some(c) => {
            let mut parts = Vec::new();
            if let Some(p) = &c.prefix {
                parts.push(format!("prefix={p:?}"));
            }
            if let Some(b) = &c.binary {
                parts.push(format!("binary={b:?}"));
            }
            if let Some(re) = &c.regex {
                parts.push(format!("regex={re:?}"));
            }
            parts.join(" ")
        }
        None => String::new(),
    }
}

fn action_desc(r: &Rule) -> String {
    match (&r.then.prepend, &r.then.replace_binary, &r.then.env) {
        (Some(p), _, _) => format!("prepend {p}"),
        (_, Some(rb), _) => format!("replace {} -> {}", rb.from, rb.to),
        (_, _, Some(_)) => "set env".to_string(),
        _ => format!("{:?}", r.then.action),
    }
}

fn describe_rule(r: &Rule) -> String {
    format!(
        "{:<18} {:?} {:<26} {}",
        r.id,
        r.when.stage,
        matcher_desc(r),
        action_desc(r)
    )
}

/// Rule summary for display in the settings page.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RuleSummary {
    pub id: String,
    pub enabled: bool,
    pub stage: String,
    pub matcher: String,
    pub action: String,
    pub source: String,
}

/// Rule summary list (with source-layer labels) for the settings page Rules tab.
pub fn list_summaries() -> Vec<RuleSummary> {
    let cwd = std::env::current_dir().ok();
    let set = load(cwd.as_deref());
    let mut origin: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Ok(Some(rs)) = read_layer(&global_path()) {
        for r in rs {
            origin.insert(r.id, "global".to_string());
        }
    }
    if let Some(p) = cwd.as_deref().map(project_path) {
        if let Ok(Some(rs)) = read_layer(&p) {
            for r in rs {
                origin.insert(r.id, format!("project: {}", p.display()));
            }
        }
    }
    set.rules
        .iter()
        .map(|r| RuleSummary {
            id: r.id.clone(),
            enabled: r.enabled,
            stage: format!("{:?}", r.when.stage),
            matcher: matcher_desc(r),
            action: action_desc(r),
            source: origin
                .get(&r.id)
                .cloned()
                .unwrap_or_else(|| "builtin".to_string()),
        })
        .collect()
}

/// Enable/disable a rule in the **global** rules file. Project-level rules are read-only (edit the file inside its repo).
pub fn set_enabled_in_global(rule_id: &str, enabled: bool) -> Result<(), String> {
    let path = global_path();
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let rules = v
        .get_mut("rules")
        .and_then(|r| r.as_array_mut())
        .ok_or_else(|| format!("{}: no rules array", path.display()))?;
    let mut found = false;
    for r in rules.iter_mut() {
        if r.get("id").and_then(|x| x.as_str()) == Some(rule_id) {
            r["enabled"] = serde_json::json!(enabled);
            found = true;
        }
    }
    if !found {
        return Err(format!("rule not found in {}: {rule_id}", path.display()));
    }
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap_or_default())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// List of trusted projects (shown in the settings page).
pub fn trusted_projects() -> Vec<String> {
    load_trust_registry(&trust_registry_path())
}

fn gen_rule_id() -> String {
    let s = uuid::Uuid::new_v4().simple().to_string();
    format!("rule-{}", &s[..8])
}

/// Append a rule to the given rules file: validate it as a `RulesFile` first, then dedupe and write.
/// If `id` is missing it is generated automatically (`rule-<8hex>`).
pub fn add_rule_to_path(path: &Path, mut rule: serde_json::Value) -> Result<(), String> {
    let obj = rule.as_object_mut().ok_or("rule must be a JSON object")?;
    let blank_id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().is_empty())
        .unwrap_or(true);
    if blank_id {
        obj.insert("id".to_string(), serde_json::json!(gen_rule_id()));
    }
    let wrapped =
        serde_json::json!({ "version": RULES_SCHEMA_VERSION, "rules": [rule.clone()] });
    RulesFile::parse(&wrapped.to_string())?;
    let id = rule
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut root = if path.exists() {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| format!("{}: {e}", path.display()))?
    } else {
        serde_json::json!({ "version": RULES_SCHEMA_VERSION, "rules": [] })
    };
    let arr = root
        .get_mut("rules")
        .and_then(|r| r.as_array_mut())
        .ok_or_else(|| format!("{}: no rules array", path.display()))?;
    if arr
        .iter()
        .any(|r| r.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
    {
        return Err(format!("rule id already exists: {id}"));
    }
    arr.push(rule);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&root).unwrap_or_default())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Append a rule to the global rules file (the settings page "Add rule").
pub fn add_rule_to_global(rule: serde_json::Value) -> Result<(), String> {
    add_rule_to_path(&global_path(), rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_rule() {
        let f = RulesFile::parse(
            r#"{"version":1,"rules":[
              {"id":"sandbox-curl","when":{"stage":"tool_pre","command":{"prefix":"curl "}},
               "then":{"action":"rewrite","prepend":"sandbox"}}]}"#,
        )
        .unwrap();
        assert_eq!(f.rules.len(), 1);
        assert_eq!(f.rules[0].id, "sandbox-curl");
        assert!(f.rules[0].enabled, "enabled defaults to true");
        assert_eq!(f.rules[0].when.stage, Stage::ToolPre);
        assert_eq!(f.rules[0].then.action, Action::Rewrite);
    }

    #[test]
    fn rejects_unknown_version() {
        assert!(RulesFile::parse(r#"{"version":99,"rules":[]}"#).is_err());
    }

    #[test]
    fn rejects_shell_action() {
        // D3: declarative transforms, no shell — undeclared fields must be rejected at parse time.
        let r = RulesFile::parse(
            r#"{"version":1,"rules":[
              {"id":"x","when":{"stage":"tool_pre","command":{"prefix":"curl "}},
               "then":{"action":"rewrite","shell":"curl | sh"}}]}"#,
        );
        assert!(r.is_err(), "shell transform must be rejected");
    }

    #[test]
    fn rejects_rewrite_without_transform() {
        let r = RulesFile::parse(
            r#"{"version":1,"rules":[
              {"id":"x","when":{"stage":"tool_pre","command":{"prefix":"curl "}},
               "then":{"action":"rewrite"}}]}"#,
        );
        assert!(r.is_err());
    }

    #[test]
    fn rejects_rewrite_without_command_match() {
        let r = RulesFile::parse(
            r#"{"version":1,"rules":[
              {"id":"x","when":{"stage":"tool_pre"},
               "then":{"action":"rewrite","prepend":"sandbox"}}]}"#,
        );
        assert!(r.is_err());
    }

    #[test]
    fn accepts_disabled_and_p1_placeholder() {
        let f = RulesFile::parse(
            r#"{"version":1,"rules":[
              {"id":"a","enabled":false,"when":{"stage":"tool_pre","command":{"binary":"curl"}},
               "then":{"action":"rewrite","replace_binary":{"from":"curl","to":"scurl"}}},
              {"id":"b","when":{"stage":"session_start"},
               "then":{"action":"inject","source":"project-context"}}]}"#,
        )
        .unwrap();
        assert_eq!(f.rules.len(), 2);
        assert!(!f.rules[0].enabled);
    }

    #[test]
    fn rejects_empty_id() {
        let r = RulesFile::parse(
            r#"{"version":1,"rules":[
              {"id":"","when":{"stage":"tool_pre","command":{"binary":"curl"}},
               "then":{"action":"rewrite","prepend":"sandbox"}}]}"#,
        );
        assert!(r.is_err());
    }

    // ---- Layered loading / trust / engine ----

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ocx-rules-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, s: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, s).unwrap();
    }

    fn rule_json(id: &str, prefix: &str, prepend: &str) -> String {
        format!(
            r#"{{"version":1,"rules":[{{"id":"{id}","when":{{"stage":"tool_pre","command":{{"prefix":"{prefix}"}}}},"then":{{"action":"rewrite","prepend":"{prepend}"}}}}]}}"#
        )
    }

    fn missing_global() -> PathBuf {
        PathBuf::from("/nonexistent-ocx-global/rules.json")
    }

    #[test]
    fn project_rules_ignored_when_untrusted() {
        let dir = tmp("proj-untrusted");
        write(&project_path(&dir), &rule_json("r1", "curl ", "sandbox"));
        let set = load_layers(&missing_global(), Some(&project_path(&dir)), false);
        assert!(set.rules.is_empty(), "untrusted project rules must not load");
    }

    #[test]
    fn project_rules_loaded_when_trusted() {
        let dir = tmp("proj-trusted");
        write(&project_path(&dir), &rule_json("r1", "curl ", "sandbox"));
        let set = load_layers(&missing_global(), Some(&project_path(&dir)), true);
        assert_eq!(set.rules.len(), 1);
        assert!(set.sources.iter().any(|p| p.ends_with("rules.json")));
    }

    #[test]
    fn bad_json_is_fail_open() {
        let dir = tmp("proj-bad");
        write(&project_path(&dir), "{ not json");
        let g = tmp("global-bad").join("rules.json");
        write(&g, "{ also bad");
        let set = load_layers(&g, Some(&project_path(&dir)), true);
        assert!(set.rules.is_empty());
        assert_eq!(set.errors.len(), 2, "both bad layers recorded, neither fatal");
    }

    #[test]
    fn later_layer_overrides_same_id() {
        let g = tmp("global-override").join("rules.json");
        write(&g, &rule_json("dup", "curl ", "sandbox"));
        let dir = tmp("proj-override");
        write(&project_path(&dir), &rule_json("dup", "wget ", "sandbox2"));
        let set = load_layers(&g, Some(&project_path(&dir)), true);
        assert_eq!(set.rules.len(), 1, "same id collapses");
        assert_eq!(
            set.rules[0].when.command.as_ref().unwrap().prefix.as_deref(),
            Some("wget "),
            "inner layer wins"
        );
    }

    #[test]
    fn trust_roundtrip() {
        let reg = tmp("trust-reg").join("trusted-projects.json");
        let dir = tmp("trust-proj");
        assert!(!is_trusted_in(&reg, &dir));
        set_trust_in(&reg, &dir, true).unwrap();
        assert!(is_trusted_in(&reg, &dir));
        set_trust_in(&reg, &dir, false).unwrap();
        assert!(!is_trusted_in(&reg, &dir));
    }

    #[test]
    fn rewrite_command_applies_first_match() {
        let dir = tmp("rw-first");
        write(&project_path(&dir), &rule_json("sandbox-curl", "curl ", "sandbox"));
        let set = load_layers(&missing_global(), Some(&project_path(&dir)), true);
        assert_eq!(
            rewrite_command("curl https://x", Stage::ToolPre, &set),
            RewriteOutcome::Rewritten {
                rule_id: "sandbox-curl".into(),
                command: "sandbox curl https://x".into(),
            }
        );
        assert_eq!(rewrite_command("ls /tmp", Stage::ToolPre, &set), RewriteOutcome::Unchanged);
    }

    // ---- add_rule_to_path ----

    fn sample_rule(id: Option<&str>) -> serde_json::Value {
        let mut v = serde_json::json!({
            "when": { "stage": "tool_pre", "command": { "prefix": "curl " } },
            "then": { "action": "rewrite", "prepend": "sandbox" }
        });
        if let Some(id) = id {
            v["id"] = serde_json::json!(id);
        }
        v
    }

    #[test]
    fn add_rule_creates_file_and_appends() {
        let f = tmp("add-rule").join("rules.json");
        add_rule_to_path(&f, sample_rule(Some("r1"))).unwrap();
        let set = load_layers(&f, None, false);
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "r1");
    }

    #[test]
    fn add_rule_generates_id_when_blank() {
        let f = tmp("add-rule-id").join("rules.json");
        add_rule_to_path(&f, sample_rule(None)).unwrap();
        let set = load_layers(&f, None, false);
        assert!(set.rules[0].id.starts_with("rule-"), "got {}", set.rules[0].id);
    }

    #[test]
    fn add_rule_rejects_shell_transform() {
        let f = tmp("add-rule-bad").join("rules.json");
        let bad = serde_json::json!({
            "id": "x",
            "when": { "stage": "tool_pre", "command": { "prefix": "curl " } },
            "then": { "action": "rewrite", "shell": "curl | sh" }
        });
        assert!(add_rule_to_path(&f, bad).is_err());
    }

    #[test]
    fn add_rule_dedupes_id() {
        let f = tmp("add-rule-dup").join("rules.json");
        add_rule_to_path(&f, sample_rule(Some("dup"))).unwrap();
        assert!(add_rule_to_path(&f, sample_rule(Some("dup"))).is_err());
    }
}
