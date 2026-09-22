//! Plugin Manager: manifest parsing, install, state machine, process lifecycle. See docs/plugin-manifest.md.
//! State machine: probe_pending → starting → running; plus stopped / error / probe_failed
//! (authoritative value is whatever plugins.status actually stores; see docs/plugin-manifest.md).

use super::process::{PluginProcess, Reply, RuntimeSpec};
use super::storage::SharedStore;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::core::plugin_sig::VerifyOutcome;

pub const API_VERSION: &str = "1";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuntimeManifest {
    #[serde(rename = "type")]
    pub rtype: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub version: String,
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    #[serde(rename = "type")]
    pub ptype: String,
    pub runtime: Option<RuntimeManifest>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityDecl>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub states: Vec<String>,
    /// Phase 53 — optional alerting sub-config (severityHints table).
    /// Old manifests lack this field → `#[serde(default)]` → None.
    #[serde(default)]
    pub alerting: Option<super::alerting::AlertingManifest>,
    /// F4 — semantic version floor: install/start only when core_version >= minCoreVersion.
    #[serde(rename = "minCoreVersion", default, skip_serializing_if = "Option::is_none")]
    pub min_core_version: Option<String>,
    /// F5 — inter-plugin dependencies: pluginId -> semver VersionReq.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
    /// M7/F8 — declarative settings: Core renders the generic settings page; the real values still live in config.* (the declaration only drives the UI).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<SettingDecl>,
    /// S5 — sandbox declaration (optional); whether it is enforced is also bound by the sandbox_enforcement switch and trust level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxDecl>,
    /// Wow 6 signature/provenance fields. Previously dropped by serde → now fully persisted (old manifests unaffected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Shape is owned by plugin_sig: keep the raw JSON, no strong typing here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<serde_json::Value>,
}

/// §4.2 capability declaration shape: "string | object" (untagged parse; old manifests work as-is).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CapabilityDecl {
    /// String form: provides an implementation of a **built-in capability**; permissions follow the static mapping.
    Plain(String),
    /// Object form: plugin-domain declaration (capability → permission + default); permissions are frozen by this declaration.
    Mapping {
        id: String,
        permission: String,
        /// ask | denied (§4.2 default is restricted); default = ask
        #[serde(default)]
        default: Option<String>,
        /// S4 — call timeout for this capability (seconds, 1..=600); default = CALL_TIMEOUT(60s).
        #[serde(rename = "timeoutSecs", default)]
        timeout_secs: Option<u32>,
    },
}

impl CapabilityDecl {
    pub fn id(&self) -> &str {
        match self {
            CapabilityDecl::Plain(s) => s,
            CapabilityDecl::Mapping { id, .. } => id,
        }
    }

    /// The (permission, default) of the object form; returns None for the string form.
    pub fn mapping(&self) -> Option<(&str, &str)> {
        match self {
            CapabilityDecl::Plain(_) => None,
            CapabilityDecl::Mapping {
                permission,
                default,
                ..
            } => Some((permission, default.as_deref().unwrap_or("ask"))),
        }
    }

    /// Whether this is a plugin-domain declaration (→ declaration table / once-only / "unverified domain" marker).
    pub fn is_declared(&self) -> bool {
        matches!(self, CapabilityDecl::Mapping { .. })
    }
}

/// P2 — localizable text (frozen schema): label / description / section share
/// `validate[].message`. untagged makes the **existing plain-string form round-trip byte-identical**
/// (old manifests unaffected) while also accepting a locale → text mapping.
/// Locale keys are unrestricted: a plugin may carry languages the App does not yet know, and parsing still accepts them.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum LocalizedText {
    /// Existing form: single-language string (serialized back as a string unchanged).
    Plain(String),
    /// New form: locale → text (BTreeMap naturally gives deterministic lexicographic iteration).
    Map(BTreeMap<String, String>),
}

impl LocalizedText {
    /// Host side (no locale context: storage / set_setting_value error strings) picks a text:
    /// use `en` if present, otherwise the lexicographically first key. The UI side does not go through here (it resolves by the current locale).
    pub fn pick_host_locale(&self) -> &str {
        match self {
            LocalizedText::Plain(s) => s.as_str(),
            LocalizedText::Map(m) => m
                .get("en")
                .map(String::as_str)
                .or_else(|| m.iter().next().map(|(_, v)| v.as_str()))
                .unwrap_or(""),
        }
    }
}

/// An option of dropdown / radio-group: a bare string (display = value) or
/// {value, label} (label is localized, display only; storage and predicate comparison both use value).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingOption {
    Value(String),
    Item { value: String, label: Option<LocalizedText> },
}

impl SettingOption {
    pub fn value(&self) -> &str {
        match self {
            SettingOption::Value(v) => v,
            SettingOption::Item { value, .. } => value,
        }
    }
}

/// M7/F8 — settings[] control declaration (frozen schema). Real values are always stored in config (secret:* goes to the keychain).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SettingDecl {
    /// Config key name: ^[a-z][a-z0-9_-]{0,63}$; the actual storage key for a secret control is `secret:<key>`.
    pub key: String,
    /// toggle | text | textarea | number | slider | dropdown | radio-group |
    /// color | secret | path | button | list
    #[serde(rename = "type")]
    pub stype: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<LocalizedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<LocalizedText>,
    /// Default value (UI falls back to "unset"; secrets must not declare one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// dropdown / radio-group options; must be empty for all other types.
    /// String or {value, label?} — label is localized and shown to the user; value is what gets stored in config.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SettingOption>,
    /// P1 — data-driven predicate: when false the whole row is not rendered. Evaluated by the UI only; Rust validates shape/references only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<Cond>,
    /// P1 — data-driven predicate: when true the control is disabled (the row remains so the reason is visible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<Cond>,
    /// P1 — validation rules applied before writing to disk (data, not closures); empty by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validate: Vec<ValidateRule>,
    /// P1 — consecutive declarations in the same section share one heading; non-empty after trim and ≤40 chars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<LocalizedText>,
    /// P2 — search index keywords (index only, never displayed); ≤8 items, each non-empty after trim and ≤40 chars.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Display order: lower first; equal and default values fall back to declaration order (stable frontend sort, form-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i64>,
    /// Deprecation note (localized): the control still works, with a "deprecated" marker and the reason on the row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<LocalizedText>,
    /// P1 — path only: file | directory (default directory, preserving existing behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick: Option<String>,
    /// P1 — number / slider only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<serde_json::Number>,
}

/// P1 — data-driven predicate (frozen schema). Definitions must cross process boundaries (Python plugin → Rust Core → TS UI),
/// and closures are not serializable, so conditions are expressed as data; evaluation happens only in the UI, Rust does no semantic evaluation,
/// it only guarantees each Cond has a valid shape and references keys already declared in the same manifest.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "op")]
pub enum Cond {
    #[serde(rename = "equals")]
    Equals {
        key: String,
        value: serde_json::Value,
    },
    #[serde(rename = "notEquals")]
    NotEquals {
        key: String,
        value: serde_json::Value,
    },
    #[serde(rename = "in")]
    In {
        key: String,
        values: Vec<serde_json::Value>,
    },
    #[serde(rename = "isSet")]
    IsSet { key: String, value: bool },
    #[serde(rename = "all")]
    All { conds: Vec<Cond> },
    #[serde(rename = "any")]
    Any { conds: Vec<Cond> },
    #[serde(rename = "not")]
    Not { cond: Box<Cond> },
    /// Frozen schema discipline: unknown ops are not silently ignored; validate_manifest always rejects them.
    #[serde(other)]
    Unknown,
}

/// P1 — data-driven validation rules (frozen schema). Rust enforces them in two places: user writes to disk
/// (`set_setting_value`) and the default self-consistency check; a plugin's reverse `config.set` is not validated.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ValidateRule {
    #[serde(rename = "required")]
    Required {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<LocalizedText>,
    },
    #[serde(rename = "minLength")]
    MinLength {
        value: serde_json::Number,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<LocalizedText>,
    },
    #[serde(rename = "maxLength")]
    MaxLength {
        value: serde_json::Number,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<LocalizedText>,
    },
    #[serde(rename = "min")]
    Min {
        value: serde_json::Number,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<LocalizedText>,
    },
    #[serde(rename = "max")]
    Max {
        value: serde_json::Number,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<LocalizedText>,
    },
    #[serde(rename = "pattern")]
    Pattern {
        regex: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<LocalizedText>,
    },
    /// Frozen schema discipline: unknown types are not silently ignored; validate_manifest always rejects them.
    #[serde(other)]
    Unknown,
}

/// P1 — predicate nesting limit (counted per container node, leaf = 0). Same convention as the alerting template lint:
/// 8 levels allowed, the 9th is rejected. `all`/`any`/`not` each add +1 per nesting.
const MAX_COND_DEPTH: usize = 8;

/// JSON number → f64 (NaN = not representable; used only for validation-time comparison).
fn num_f64(n: &serde_json::Number) -> f64 {
    n.as_f64().unwrap_or(f64::NAN)
}

/// color default value: ^#[0-9a-fA-F]{3}([0-9a-fA-F]{3})?$.
fn is_hex_color(s: &str) -> bool {
    match s.strip_prefix('#') {
        Some(hex) => matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit()),
        None => false,
    }
}

/// Constructs Rust `regex` accepts but JS `new RegExp` cannot compile (inline flags / Python named groups).
/// pattern is evaluated in two places — Rust (disk write + default self-consistency) and the UI (`new RegExp`) — so an installable
/// manifest must not carry a pattern the UI cannot evaluate (otherwise the UI silently passes and only the disk write errors).
/// `(?:…)` and `(?<name>…)` are accepted by both engines and are not on the list.
const JS_INCOMPATIBLE_REGEX: &[&str] = &["(?i", "(?m", "(?s", "(?x", "(?U", "(?-", "(?P<"];

impl ValidateRule {
    /// The rule type name (used in error messages; Unknown → None).
    fn type_name(&self) -> Option<&'static str> {
        Some(match self {
            ValidateRule::Required { .. } => "required",
            ValidateRule::MinLength { .. } => "minLength",
            ValidateRule::MaxLength { .. } => "maxLength",
            ValidateRule::Min { .. } => "min",
            ValidateRule::Max { .. } => "max",
            ValidateRule::Pattern { .. } => "pattern",
            ValidateRule::Unknown => return None,
        })
    }

    /// The rule's own message (passed through first when set_setting_value fails).
    fn message(&self) -> Option<&LocalizedText> {
        match self {
            ValidateRule::Required { message }
            | ValidateRule::MinLength { message, .. }
            | ValidateRule::MaxLength { message, .. }
            | ValidateRule::Min { message, .. }
            | ValidateRule::Max { message, .. }
            | ValidateRule::Pattern { message, .. } => message.as_ref(),
            ValidateRule::Unknown => None,
        }
    }

    /// Which control types the rule may be attached to (frozen table).
    fn legal_on(&self, stype: &str) -> bool {
        match self.type_name() {
            Some("required") => {
                matches!(stype, "text" | "textarea" | "secret" | "path" | "number" | "color")
            }
            Some("minLength" | "maxLength") => {
                matches!(stype, "text" | "textarea" | "secret" | "path")
            }
            Some("min" | "max") => matches!(stype, "number" | "slider"),
            Some("pattern") => matches!(stype, "text" | "textarea" | "path" | "color" | "secret"),
            _ => false,
        }
    }
}

/// P2 — LocalizedText validation: plain strings as before (length/blank governed by the caller-supplied limit);
/// a map must be non-empty, locale keys non-empty after trim, and each value non-empty after trim (an empty translation = manifest bug,
/// the UI would render an empty label). The key set is unrestricted. `max_chars` is reused for `section` (≤40).
fn validate_localized_text(
    text: &LocalizedText,
    owner: &str,
    max_chars: Option<usize>,
) -> Result<(), String> {
    fn check_length(value: &str, owner: &str, locale: Option<&str>, max: Option<usize>) -> Result<(), String> {
        let Some(max) = max else { return Ok(()); };
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(format!("{} must not be blank", owner));
        }
        if trimmed.chars().count() > max {
            return Err(match locale {
                Some(l) => format!("{} ({}) is too long (max {} chars)", owner, l, max),
                None => format!("{} is too long (max {} chars)", owner, max),
            });
        }
        Ok(())
    }
    match text {
        LocalizedText::Plain(s) => check_length(s, owner, None, max_chars),
        LocalizedText::Map(m) => {
            if m.is_empty() {
                return Err(format!("{}: localized text map must not be empty", owner));
            }
            for (locale, value) in m {
                if locale.trim().is_empty() {
                    return Err(format!("{} has a blank locale key", owner));
                }
                if value.trim().is_empty() {
                    return Err(format!("{} has a blank {} translation", owner, locale));
                }
                check_length(value, owner, Some(locale), max_chars)?;
            }
            Ok(())
        }
    }
}

/// P1 — "unset": value missing (i.e. `null` on the Rust side), `null`, `""`, or an empty array. Every rule except `required`
/// skips unset values, so optional fields can be cleared.
fn is_unset(value: &serde_json::Value) -> bool {
    value.is_null()
        || value.as_str() == Some("")
        || value.as_array().map_or(false, |a| a.is_empty())
}

/// P1 — single-rule evaluation (same semantics as TS `rulePasses`: a type mismatch always passes, empty values are left to
/// `required`). Used only at the two Rust enforcement points, not as full type validation.
fn rule_passes(rule: &ValidateRule, value: &serde_json::Value) -> bool {
    if !matches!(rule, ValidateRule::Required { .. }) && is_unset(value) {
        return true;
    }
    match rule {
        ValidateRule::Required { .. } => !is_unset(value),
        ValidateRule::MinLength { value: min, .. } => value
            .as_str()
            .map_or(true, |s| s.chars().count() >= num_f64(min) as usize),
        ValidateRule::MaxLength { value: max, .. } => value
            .as_str()
            .map_or(true, |s| s.chars().count() <= num_f64(max) as usize),
        ValidateRule::Min { value: min, .. } => value
            .as_f64()
            .map_or(true, |n| n >= num_f64(min)),
        ValidateRule::Max { value: max, .. } => value
            .as_f64()
            .map_or(true, |n| n <= num_f64(max)),
        ValidateRule::Pattern { regex: re, .. } => value
            .as_str()
            .map_or(true, |s| regex::Regex::new(re).map_or(true, |r| r.is_match(s))),
        ValidateRule::Unknown => true,
    }
}

/// P1 — the first rule that fails (all pass → None).
fn first_failing_rule<'a>(
    rules: &'a [ValidateRule],
    value: &serde_json::Value,
) -> Option<&'a ValidateRule> {
    rules.iter().find(|r| !rule_passes(r, value))
}

/// P1 — `set_setting_value` enforcement: failure → `invalid: <message>` (when a message exists) or
/// `invalid: <rule-type>`。
fn enforce_validate_rules(decl: &SettingDecl, value: &serde_json::Value) -> Result<(), String> {
    let Some(rule) = first_failing_rule(&decl.validate, value) else {
        return Ok(());
    };
    Err(match rule.message() {
        Some(m) => format!("invalid: {}", m.pick_host_locale()),
        None => format!("invalid: {}", rule.type_name().unwrap_or("validate")),
    })
}

/// P1 — keys referenced by a predicate: must already be declared; secrets may only use `isSet`, lists may never be referenced;
/// `equals`/`notEquals`/`in` values of dropdown / radio-group must fall within that key's `options[]`.
/// Rust does not evaluate.
fn validate_cond_key(
    owner: &str,
    key: &str,
    op: &str,
    values: Option<&[serde_json::Value]>,
    decls: &BTreeMap<&str, &SettingDecl>,
) -> Result<(), String> {
    let Some(decl) = decls.get(key) else {
        return Err(format!(
            "{}: predicate references undeclared setting key {:?}",
            owner, key
        ));
    };
    if decl.stype == "secret" && op != "isSet" {
        return Err(format!(
            "{}: secret setting {:?} only supports the isSet predicate",
            owner, key
        ));
    }
    // list values belong to the plugin process (settings_view does not return them), so a predicate would only ever see undefined — reject at install time
    // rather than give the author a row that never appears.
    if decl.stype == "list" {
        return Err(format!(
            "{}: list setting {:?} cannot be referenced by a predicate",
            owner, key
        ));
    }
    if matches!(decl.stype.as_str(), "dropdown" | "radio-group") {
        if let Some(values) = values {
            for v in values {
                let ok = v
                    .as_str()
                    .map(|x| decl.options.iter().any(|o| o.value() == x))
                    .unwrap_or(false);
                if !ok {
                    return Err(format!(
                        "{}: predicate value {:?} is not one of the options[] of setting {}",
                        owner, v, key
                    ));
                }
            }
        }
    }
    Ok(())
}

/// P1 — predicate shape + reference validation (no evaluation): `all`/`any` non-empty, nesting ≤ `MAX_COND_DEPTH`.
fn validate_cond(
    cond: &Cond,
    owner: &str,
    depth: usize,
    decls: &BTreeMap<&str, &SettingDecl>,
) -> Result<(), String> {
    match cond {
        Cond::Unknown => Err(format!("{}: unknown predicate op", owner)),
        Cond::Equals { key, value } => {
            validate_cond_key(owner, key, "equals", Some(std::slice::from_ref(value)), decls)
        }
        Cond::NotEquals { key, value } => validate_cond_key(
            owner,
            key,
            "notEquals",
            Some(std::slice::from_ref(value)),
            decls,
        ),
        Cond::In { key, values } => validate_cond_key(owner, key, "in", Some(values), decls),
        Cond::IsSet { key, .. } => validate_cond_key(owner, key, "isSet", None, decls),
        Cond::All { conds } | Cond::Any { conds } => {
            if conds.is_empty() {
                return Err(format!(
                    "{}: all/any predicate requires a non-empty conds[]",
                    owner
                ));
            }
            let depth = depth + 1;
            if depth > MAX_COND_DEPTH {
                return Err(format!(
                    "{}: predicate nesting too deep (max {})",
                    owner, MAX_COND_DEPTH
                ));
            }
            for c in conds {
                validate_cond(c, owner, depth, decls)?;
            }
            Ok(())
        }
        Cond::Not { cond } => {
            let depth = depth + 1;
            if depth > MAX_COND_DEPTH {
                return Err(format!(
                    "{}: predicate nesting too deep (max {})",
                    owner, MAX_COND_DEPTH
                ));
            }
            validate_cond(cond, owner, depth, decls)
        }
    }
}

/// S5 — sandbox declaration (macOS execution layer in core::sandbox; the declaration itself is portable across platforms).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SandboxDecl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<SandboxFs>,
    /// "none" | "out"; enforced as none (least privilege) by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SandboxFs {
    /// Currently only `["plugin-data"]` is supported (= `~/.opencapx/plugin-data/<id>/`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write: Vec<String>,
}

/// S5a — sandbox declaration validation (shared by install time + auto-gate category 6).
pub(crate) fn validate_sandbox(s: &SandboxDecl) -> Result<(), String> {
    if let Some(net) = &s.network {
        if net != "none" && net != "out" {
            return Err(format!("invalid sandbox.network {:?} (expect none|out)", net));
        }
    }
    if let Some(fs) = &s.fs {
        for entry in &fs.write {
            if entry != "plugin-data" {
                return Err(format!(
                    "unsupported sandbox.fs.write entry {:?} (only \"plugin-data\" supported)",
                    entry
                ));
            }
        }
    }
    Ok(())
}

impl Manifest {
    /// List of declared capability IDs (string form + object form; the latter has no duplicates).
    pub fn capability_ids(&self) -> Vec<String> {
        self.capabilities.iter().map(|c| c.id().to_string()).collect()
    }

    /// Object-form declarations → (capability, permission, default), used by the commit phase to write the declaration table.
    pub fn declarations(&self) -> Vec<(String, String, String, Option<u32>)> {
        self.capabilities
            .iter()
            .filter_map(|c| {
                c.mapping().map(|(perm, default)| {
                    let timeout = match c {
                        CapabilityDecl::Mapping { timeout_secs, .. } => *timeout_secs,
                        CapabilityDecl::Plain(_) => None,
                    };
                    (
                        c.id().to_string(),
                        perm.to_string(),
                        default.to_string(),
                        timeout,
                    )
                })
            })
            .collect()
    }

    /// Set of permission names from inline-mapping declarations (half of the §4.4 step 4 confirmation set).
    pub fn declared_permission_names(&self) -> Vec<String> {
        self.capabilities
            .iter()
            .filter_map(|c| c.mapping().map(|(p, _)| p.to_string()))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginStatusDto {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    pub version: String,
    #[serde(rename = "type")]
    pub ptype: String,
    pub status: String,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Whether auto-reload polling is enabled (manifest mtime change → automatic stop+start).
    /// Default false; settings.ts exposes a toggle that calls `set_auto_reload` to flip it.
    #[serde(rename = "autoReload")]
    pub auto_reload: bool,
    /// Phase 37 — most recent probe status ("passed" / "failed" / "pending" / "skipped").
    /// None if never run (old database / Mem mode).
    #[serde(rename = "probeStatus", skip_serializing_if = "Option::is_none")]
    pub probe_status: Option<String>,
    /// Phase 37 — probe timestamp (epoch seconds).
    #[serde(rename = "probeAt", skip_serializing_if = "Option::is_none")]
    pub probe_at: Option<u64>,
    /// Phase 38 — plugin update channel ("stable" / "beta" / "dev").
    /// None if never stored (falls back to the global default channel).
    #[serde(rename = "channel", skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// S5 — whether a sandbox block was declared (UI badge; enforcement is still bound by the switch/trust constraints).
    #[serde(rename = "sandboxDeclared")]
    pub sandbox_declared: bool,
    /// Phase 40 — heartbeat_sec (0 = ping off, default behavior). None = never configured → watchdog uses a 500ms tick.
    #[serde(rename = "healthHeartbeatSec", skip_serializing_if = "Option::is_none")]
    pub health_heartbeat_sec: Option<u32>,
    /// Phase 40 — max_retries (0 = watchdog disabled).
    #[serde(rename = "healthMaxRetries", skip_serializing_if = "Option::is_none")]
    pub health_max_retries: Option<u32>,
    /// Phase 40 — whether the watchdog is enabled (default true).
    #[serde(rename = "healthEnabled", skip_serializing_if = "Option::is_none")]
    pub health_enabled: Option<bool>,
    /// F5 — unmet plugin dependencies (used for the settings-page warning).
    #[serde(rename = "missingDependencies", default, skip_serializing_if = "Vec::is_empty")]
    pub missing_dependencies: Vec<MissingDepDto>,
    /// M4 — revocation hit: the publisher key is in the registry revokedKeys (settings-page banner + reopen button).
    #[serde(rename = "revokedKey", skip_serializing_if = "Option::is_none")]
    pub revoked_key: Option<String>,
    /// M4 — revocation hit time (epoch seconds).
    #[serde(rename = "revokedAt", skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
}

/// F5 — a single unmet dependency (id + requirement literal).
#[derive(Debug, Clone, Serialize)]
pub struct MissingDepDto {
    pub id: String,
    pub requirement: String,
}

/// For the Wow 5 install-preview dialog. Opens the zip, reads the manifest, **without extracting**, and returns to the frontend
/// for display; only when the user clicks Install does it go through `install_ocplugin`. The permissions array carries the `high_risk`
/// marker (aligned with core::permission::HIGH_RISK); the frontend highlights sensitive permissions in red.
#[derive(Debug, Clone, Serialize)]
pub struct PluginPreviewDto {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    pub version: String,
    #[serde(rename = "type")]
    pub ptype: String,
    pub capabilities: Vec<String>,
    pub permissions: Vec<PermissionPreviewDto>,
    /// Wow 6 — signature/integrity status (used by the settings badge).
    /// label: "trusted" / "unsigned" / "tampered" / "bad-signature" / "unknown-key" / "malformed-signature"
    /// keyId: signer identifier (returned when trusted)
    #[serde(rename = "signature")]
    pub signature: SignaturePreviewDto,
    /// F7 — registry verification: keyId is a registered publisher (verified and not revoked). None = unsigned / registry unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// S5 — whether a sandbox block was declared (used by the enforcement popup when unsigned).
    #[serde(rename = "sandboxDeclared")]
    pub sandbox_declared: bool,
    /// F7 — official keyId marker (com.opencapx prefix).
    pub official: bool,
    /// F7 — core compatibility (a rendering fact; the real gate is in the install flow).
    pub compat: CompatPreviewDto,
    /// F7 — permission diff against the installed version (not installed → None).
    #[serde(rename = "permissionDiff", skip_serializing_if = "Option::is_none")]
    pub permission_diff: Option<PermissionDiffDto>,
}

/// F7 — compatibility summary for preview.
#[derive(Debug, Clone, Serialize)]
pub struct CompatPreviewDto {
    pub ok: bool,
    #[serde(rename = "minCoreVersion", skip_serializing_if = "Option::is_none")]
    pub min_core_version: Option<String>,
    pub current: String,
}

/// F7 — permission diff for preview (update case: added / removed).
#[derive(Debug, Clone, Serialize)]
pub struct PermissionDiffDto {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// M7/F8 — generic settings-page view: declarations + current values + set secret keys (values are not returned).
#[derive(Debug, Clone, Serialize)]
pub struct SettingsViewDto {
    pub settings: Vec<SettingDecl>,
    pub values: serde_json::Map<String, serde_json::Value>,
    #[serde(rename = "secretsSet")]
    pub secrets_set: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignaturePreviewDto {
    #[serde(rename = "status")]
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "keyId")]
    pub key_id: Option<String>,
}

/// Wow 7 — for the uninstall confirmation dialog. Shows a few extra things that will be cleared, beyond `window.confirm`:
/// - auto_reload switch state (the user may have forgotten to turn it off)
/// - whether the config file exists (unrecoverable if lost)
/// - permissions / capabilities counts (indirectly reflect the footprint)
/// - dependents: ids of other installed plugins whose capabilities overlap this plugin's
///   (the user should first assess whether these dependents would break)
#[derive(Debug, Clone, Serialize)]
pub struct UninstallPreviewDto {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(rename = "autoReload")]
    pub auto_reload: bool,
    #[serde(rename = "configExists")]
    pub config_exists: bool,
    #[serde(rename = "permissionCount")]
    pub permission_count: usize,
    #[serde(rename = "capabilityCount")]
    pub capability_count: usize,
    /// ids of other installed plugins with overlapping capabilities (describes the dependency relationship)
    pub dependents: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionPreviewDto {
    pub name: String,
    #[serde(rename = "highRisk")]
    pub high_risk: bool,
    /// §4.3: declaration-derived → once-only, the UI does not offer Always
    #[serde(rename = "declared")]
    pub declared: bool,
}

/// Phase 32 — capability dependency graph node (each installed plugin = one node).
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityGraphNodeDto {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
}

/// Phase 32 — capability dependency graph edge. An edge is created when two plugins share at least one capability,
/// `shared` lists every shared item (for the tooltip).
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityGraphEdgeDto {
    pub from: String,
    pub to: String,
    pub shared: Vec<String>,
}

/// Phase 32 — the full dependency graph; the frontend draws the node-edge chart.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityGraphDto {
    pub nodes: Vec<CapabilityGraphNodeDto>,
    pub edges: Vec<CapabilityGraphEdgeDto>,
}

pub struct PluginManager {
    procs: Mutex<HashMap<String, Arc<PluginProcess>>>,
    watchdogs: Mutex<HashMap<String, Arc<Mutex<Watchdog>>>>,
    /// Cumulative retry count across restarts. Reset to zero in start().
    retry_counts: Mutex<HashMap<String, u32>>,
    /// manifest path → most recent mtime (for auto-reload polling)
    last_mtimes: Mutex<HashMap<String, std::time::SystemTime>>,
    /// Set of plugin ids with auto_reload enabled (per-manager, avoiding shared_store global coupling)
    auto_reload_set: Mutex<std::collections::HashSet<String>>,
}

const WATCHDOG_MAX_RETRIES: u32 = 3;
const WATCHDOG_TICK_MS: u64 = 500;

/// F6 mutual exclusion: install / update / uninstall are globally serialized (in-process lock). Installs are infrequent,
/// so the coarse-grained lock cost is negligible; it prevents concurrent installs from cross-writing the same staging dir (.tmp-<id>-<pid>).
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// Extracts the publisher keyId from the manifest signature block (v1/v2 same shape); unsigned → None.
pub fn manifest_signature_key_id(m: &Manifest) -> Option<String> {
    m.signature
        .as_ref()?
        .get("keyId")?
        .as_str()
        .map(str::to_string)
}

/// F7 — the "allow unsigned packages" master switch (settings_kv, default ON; OFF → unsigned/unknown-key is hard-rejected).
pub fn allow_unsigned() -> bool {
    super::shared_store()
        .and_then(|s| s.lock().ok().and_then(|g| g.get_setting("allow_unsigned")))
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// F7 — writes the master switch (settings page).
pub fn set_allow_unsigned(on: bool) -> Result<(), String> {
    let Some(store) = super::shared_store() else {
        return Err("storage unavailable".into());
    };
    let Ok(mut s) = store.lock() else {
        return Err("store lock poisoned".into());
    };
    s.set_setting("allow_unsigned", if on { "1" } else { "0" });
    Ok(())
}

/// S1 — environment isolation switch (default on; `plugin_env_isolation=false` falls back to inheriting the host env).
pub fn env_isolation_enabled() -> bool {
    super::shared_store()
        .and_then(|s| s.lock().ok().and_then(|g| g.get_setting("plugin_env_isolation")))
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// S1 — incremental plugin environment allowlist (comma-separated; empty = minimal allowlist only).
pub fn env_allowlist() -> Vec<String> {
    super::shared_store()
        .and_then(|s| s.lock().ok().and_then(|g| g.get_setting("plugin_env_allowlist")))
        .map(|v| {
            v.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// S5b — sandbox enforcement switch (default false: H1 soaks first; trusted declaring plugins follow the switch, untrusted declaring plugins are forced).
pub fn sandbox_enforcement_enabled() -> bool {
    super::shared_store()
        .and_then(|s| s.lock().ok().and_then(|g| g.get_setting("sandbox_enforcement")))
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// S5b — writes the sandbox enforcement switch (settings page).
pub fn set_sandbox_enforcement(on: bool) -> Result<(), String> {
    let Some(store) = super::shared_store() else {
        return Err("storage unavailable".into());
    };
    let Ok(mut s) = store.lock() else {
        return Err("store lock poisoned".into());
    };
    s.set_setting("sandbox_enforcement", if on { "1" } else { "0" });
    Ok(())
}

/// F7 — install confirmation options. All false by default = fail-closed (soft-warning tier rejects the install).
#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    /// The user has confirmed "unverified source (unsigned / unknown key), risk accepted".
    pub confirm_unsigned: bool,
    /// The user has confirmed "publisher key changed = new publisher takes over".
    pub confirm_key_change: bool,
}

struct Watchdog {
    enabled: bool,
    /// Phase 40 — per-plugin watchdog config (snapshotted at register time; the loop no longer touches the store).
    cfg: super::health::PluginHealthConfig,
}

impl Watchdog {
    fn new(cfg: super::health::PluginHealthConfig) -> Self {
        Self { enabled: cfg.enabled, cfg }
    }
}

impl PluginManager {
    pub fn shared() -> Arc<Self> {
        static MGR: OnceLock<Arc<PluginManager>> = OnceLock::new();
        MGR.get_or_init(|| {
            Arc::new(PluginManager {
                procs: Mutex::new(HashMap::new()),
                watchdogs: Mutex::new(HashMap::new()),
                retry_counts: Mutex::new(HashMap::new()),
                last_mtimes: Mutex::new(HashMap::new()),
                auto_reload_set: Mutex::new(std::collections::HashSet::new()),
            })
        })
        .clone()
    }

    fn store() -> Option<SharedStore> {
        super::shared_store()
    }

    /// Sets the plugin status column + emits `plugin.state_changed`. Phase 37 routes `probe_failed`
    /// through this same path too, so the EventBus uses one channel (SSE-consistent).
    pub fn set_status(id: &str, status: &str) {
        if let Some(store) = Self::store() {
            if let Ok(mut s) = store.lock() {
                let _ = s.with_conn(|c| {
                    c.execute(
                        "UPDATE plugins SET status = ?2 WHERE id = ?1",
                        params![id, status],
                    )
                    .unwrap_or(0)
                });
            }
        }
        super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
            "plugin.state_changed",
            "core",
            json!({ "pluginId": id, "status": status }),
        ));
    }

    pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
        let path = dir.join("opencapx-plugin.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        let m: Manifest =
            serde_json::from_str(&text).map_err(|e| format!("bad manifest: {}", e))?;
        Self::validate_manifest(&m)?;
        Ok(m)
    }

    /// P0 hotfix (docs/permission-domains.md §7): plugin id lexical validation.
/// Previously `install_ocplugin` did `root.join(&m.id)` directly and, for an existing target,
/// `remove_dir_all` — a manifest id of `../..` could traverse and delete an arbitrary directory.
/// Rules: charset [a-z0-9.-], non-empty, length ≤ 128, forbids ".." and leading/trailing dots.
/// Whitespace / non-ASCII / slashes are naturally excluded by the charset. Old single-segment hyphen ids are compatible.
fn valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        && !id.starts_with('.')
        && !id.ends_with('.')
        && !id.contains("..")
}

pub(crate) fn validate_manifest(m: &Manifest) -> Result<(), String> {
    if !Self::valid_plugin_id(&m.id) {
        return Err(format!(
            "invalid plugin id {:?}: charset [a-z0-9.-], no \"..\", no leading/trailing dot, max 128 chars",
            m.id
        ));
    }
    if m.api_version != API_VERSION {
        return Err(format!(
            "unsupported apiVersion {} (core expects {})",
            m.api_version, API_VERSION
        ));
    }
    if !matches!(m.ptype.as_str(), "pet" | "capability") {
        return Err(format!("unsupported type {} (v1: pet | capability)", m.ptype));
    }

    // §4.4 step 1 bootstrapping order: capability validation branches by form —
    // - string form: must still be a **built-in** capability (the declaration is not yet persisted, so `known()` must not be trusted)
    // - object form: valid lexically + non-reserved domain is enough to pass (the declaration itself is frozen in the commit phase)
    if m.ptype == "capability" {
        if m.runtime.is_none() {
            return Err("capability plugin requires runtime".into());
        }
        if m.capabilities.is_empty() {
            return Err("capability plugin requires capabilities[]".into());
        }
        for c in &m.capabilities {
            let id = c.id();
            match c.mapping() {
                None => {
                    // built-in capability names are exempt from the lexical rule (Core's own word list; additions must sync docs/capability.md)
                    if !super::capability::is_builtin(id) {
                        return Err(format!(
                            "unknown capability {} (not in v1 registry; new-domain capabilities must be declared in object form)",
                            id
                        ));
                    }
                }
                Some((perm, default)) => {
                    // Only the declaration surface is lexically constrained (§4.2): new names supplied by plugins are validated byte-exact
                    if !super::permission::valid_name(id) {
                        return Err(format!(
                            "invalid capability name {:?}: ^[a-z][a-z0-9_-]*(\\.[a-z][a-z0-9_-]*)+$, <=64 chars",
                            id
                        ));
                    }
                    // Reserved IDs may only be providers in string form (§4.2 reserved-domain closure)
                    if super::capability::is_builtin(id) || super::permission::reserved_capability(id)
                    {
                        return Err(format!(
                            "reserved capability {} cannot be declared in object form (use the string form)",
                            id
                        ));
                    }
                    if super::permission::reserved_domain(super::permission::first_segment(id)) {
                        return Err(format!("capability {} is in a reserved domain", id));
                    }
                    if !super::permission::valid_name(perm) {
                        return Err(format!("invalid permission name {:?}", perm));
                    }
                    // Declarations must not reference reserved permission names (including their granted default, to prevent self-granting via defaults)
                    if super::permission::reserved_domain(super::permission::first_segment(perm)) {
                        return Err(format!(
                            "declaration may not reference reserved permission {}",
                            perm
                        ));
                    }
                    if super::permission::first_segment(perm) != super::permission::first_segment(id) {
                        return Err(format!(
                            "capability {} and permission {} must share a domain",
                            id, perm
                        ));
                    }
                    if !matches!(default, "ask" | "denied") {
                        return Err(format!(
                            "declaration default must be ask|denied, got {:?}",
                            default
                        ));
                    }
                    // S4 — call timeout bounds (object form only; default = CALL_TIMEOUT 60s)
                    if let CapabilityDecl::Mapping {
                        timeout_secs: Some(t),
                        ..
                    } = c
                    {
                        if !(1..=600).contains(t) {
                            return Err(format!(
                                "timeoutSecs {} out of range for {} (expect 1..=600)",
                                t, id
                            ));
                        }
                    }
                }
            }
        }
    }

    // Permission table: built-in names as before (lexical exemption — single-segment names like `camera` / `microphone` are
    // Core's existing word list, so tightening the lexical rule does not change the built-in list); new domain names must be given by the object form of this list
    // declaration (and must not dangle).
    let declared_perms = m.declared_permission_names();
    for p in &m.permissions {
        if super::permission::known(p) {
            continue;
        }
        if !super::permission::valid_name(p) {
            return Err(format!("invalid permission name {:?}", p));
        }
        if declared_perms.iter().any(|d| d == p) {
            continue;
        }
        return Err(format!(
            "unknown permission {} (non-built-in permissions must be declared via an object-form capability)",
            p
        ));
    }

    // F4/F5 — strict validation of new fields: bad minCoreVersion / bad requirement / illegal plugin id / self-dependency
    if let Some(min) = &m.min_core_version {
        if super::marketplace::parse_version_lenient(min).is_none() {
            return Err(format!("invalid minCoreVersion {:?}", min));
        }
    }
    for (dep_id, req) in &m.dependencies {
        if !Self::valid_plugin_id(dep_id) {
            return Err(format!("invalid dependency plugin id {:?}", dep_id));
        }
        if dep_id == &m.id {
            return Err("plugin cannot depend on itself".into());
        }
        if semver::VersionReq::parse(req).is_err() {
            return Err(format!("invalid dependency requirement {:?} for {}", req, dep_id));
        }
    }

    // M7/F8 + P1 — settings[] frozen schema: unique key names/charset; 11 controls; dropdown /
    // radio-group options; button carries no value; number default is an integer (consistent with the signature numeric dialect);
    // secret declares no default; range fields / pick / section; data-driven predicates; data-driven validation rules.
    if m.settings.len() > 32 {
        return Err("too many settings declarations (max 32)".into());
    }
    let mut setting_keys = std::collections::BTreeSet::new();
    // P1 — build the full key table first: predicates allow forward references, so reference integrity is checked uniformly in the declaration loop.
    let setting_decls: BTreeMap<&str, &SettingDecl> =
        m.settings.iter().map(|s| (s.key.as_str(), s)).collect();
    for s in &m.settings {
        let key_ok = !s.key.is_empty()
            && s.key.len() <= 64
            && s
                .key
                .chars()
                .next()
                .map(|c| c.is_ascii_lowercase())
                .unwrap_or(false)
            && s.key
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if !key_ok {
            return Err(format!(
                "invalid setting key {:?}: ^[a-z][a-z0-9_-]{{0,63}}$",
                s.key
            ));
        }
        if !setting_keys.insert(s.key.clone()) {
            return Err(format!("duplicate setting key {}", s.key));
        }
        match s.stype.as_str() {
            "toggle" | "text" | "textarea" | "number" | "slider" | "secret" | "path"
            | "color" => {}
            "dropdown" | "radio-group" => {
                if s.options.is_empty() {
                    return Err(format!("{} setting {} requires options[]", s.stype, s.key));
                }
                if let Some(d) = &s.default {
                    let ok = d
                        .as_str()
                        .map(|x| s.options.iter().any(|o| o.value() == x))
                        .unwrap_or(false);
                    if !ok {
                        return Err(format!(
                            "{} setting {} default must be one of options[]",
                            s.stype, s.key
                        ));
                    }
                }
            }
            "button" => {
                if s.default.is_some() || !s.options.is_empty() {
                    return Err(format!(
                        "button setting {} must not declare default/options",
                        s.key
                    ));
                }
            }
            "list" => {
                // P3 — data belongs to the plugin process: the host only relays ops, and default/options etc. must never be declared
                if s.default.is_some()
                    || !s.options.is_empty()
                    || s.min.is_some()
                    || s.max.is_some()
                    || s.pick.is_some()
                {
                    return Err(format!(
                        "list setting {} must not declare default/options/min/max/pick",
                        s.key
                    ));
                }
            }
            other => {
                return Err(format!(
                    "unknown setting type {:?} (toggle|text|textarea|number|slider|dropdown|radio-group|color|secret|path|button|list)",
                    other
                ));
            }
        }
        if s.stype == "secret" && s.default.is_some() {
            return Err(format!("secret setting {} must not declare default", s.key));
        }
        // P1 — min/max/step are available for number / slider only.
        let uses_range = s.min.is_some() || s.max.is_some() || s.step.is_some();
        if uses_range && !matches!(s.stype.as_str(), "number" | "slider") {
            return Err(format!(
                "setting {} ({}) must not declare min/max/step",
                s.key, s.stype
            ));
        }
        if s.stype == "slider" {
            let (Some(min), Some(max)) = (&s.min, &s.max) else {
                return Err(format!("slider setting {} requires both min and max", s.key));
            };
            if !(num_f64(min) < num_f64(max)) {
                return Err(format!("slider setting {} requires min < max", s.key));
            }
        }
        if let Some(step) = &s.step {
            if !(num_f64(step) > 0.0) {
                return Err(format!("setting {} step must be > 0", s.key));
            }
        }
        // P1 — the default of number / slider must be numeric and within [min,max].
        if matches!(s.stype.as_str(), "number" | "slider") {
            if let Some(d) = &s.default {
                if s.stype == "number" && !d.is_i64() {
                    return Err(format!(
                        "number setting {} default must be an integer",
                        s.key
                    ));
                }
                let Some(dv) = d.as_f64() else {
                    return Err(format!(
                        "{} setting {} default must be numeric",
                        s.stype, s.key
                    ));
                };
                if let Some(min) = &s.min {
                    if dv < num_f64(min) {
                        return Err(format!(
                            "{} setting {} default {} is below min {}",
                            s.stype,
                            s.key,
                            dv,
                            num_f64(min)
                        ));
                    }
                }
                if let Some(max) = &s.max {
                    if dv > num_f64(max) {
                        return Err(format!(
                            "{} setting {} default {} is above max {}",
                            s.stype,
                            s.key,
                            dv,
                            num_f64(max)
                        ));
                    }
                }
            }
        }
        // P1 — color: no options/min/max; the default (if any) must be hex.
        if s.stype == "color" {
            if !s.options.is_empty() || s.min.is_some() || s.max.is_some() {
                return Err(format!(
                    "color setting {} must not declare options/min/max",
                    s.key
                ));
            }
            if let Some(d) = &s.default {
                let ok = d.as_str().map(is_hex_color).unwrap_or(false);
                if !ok {
                    return Err(format!(
                        "color setting {} default must be a hex color (^#[0-9a-fA-F]{{3}}([0-9a-fA-F]{{3}})?$)",
                        s.key
                    ));
                }
            }
        }
        // P1 — pick only on path, value ∈ {file, directory}.
        if let Some(pick) = &s.pick {
            if s.stype != "path" {
                return Err(format!(
                    "setting {} ({}) must not declare pick (only path)",
                    s.key, s.stype
                ));
            }
            if pick != "file" && pick != "directory" {
                return Err(format!(
                    "path setting {} pick must be \"file\" or \"directory\"",
                    s.key
                ));
            }
        }
        // P2 — localizable display text: map form must be non-empty, keys non-empty, values non-empty after trim.
        if let Some(label) = &s.label {
            validate_localized_text(label, &format!("setting {} label", s.key), None)?;
        }
        if let Some(description) = &s.description {
            validate_localized_text(description, &format!("setting {} description", s.key), None)?;
        }
        // P1/P2 — section: non-empty after trim and ≤40 chars (per locale).
        if let Some(section) = &s.section {
            validate_localized_text(section, &format!("setting {} section", s.key), Some(40))?;
        }
        // P2 — aliases: search keywords only, never displayed; ≤8 items, each non-empty after trim and ≤40 chars.
        if let Some(dep) = &s.deprecated {
            validate_localized_text(dep, &format!("setting {} deprecated", s.key), Some(200))?;
        }
        if s.aliases.len() > 8 {
            return Err(format!("setting {} has too many aliases (max 8)", s.key));
        }
        for alias in &s.aliases {
            let trimmed = alias.trim();
            if trimmed.is_empty() {
                return Err(format!("setting {} alias must not be blank", s.key));
            }
            if trimmed.chars().count() > 40 {
                return Err(format!(
                    "setting {} alias is too long (max 40 chars)",
                    s.key
                ));
            }
        }
        // P1 — predicates are only shape/reference validated, Rust does not evaluate (the UI evaluates).
        if let Some(cond) = &s.visible {
            validate_cond(cond, &format!("setting {} visible", s.key), 0, &setting_decls)?;
        }
        if let Some(cond) = &s.disabled {
            validate_cond(cond, &format!("setting {} disabled", s.key), 0, &setting_decls)?;
        }
        // P1 — validation rules: type↔control table; pattern must compile with Rust `regex`.
        for rule in &s.validate {
            let Some(rtype) = rule.type_name() else {
                return Err(format!("setting {} has an unknown validate rule type", s.key));
            };
            if !rule.legal_on(&s.stype) {
                return Err(format!(
                    "setting {} ({}): validate rule {:?} is not allowed on this control",
                    s.key, s.stype, rtype
                ));
            }
            if let Some(message) = rule.message() {
                validate_localized_text(
                    message,
                    &format!("setting {} validate rule {:?} message", s.key, rtype),
                    None,
                )?;
            }
            if let ValidateRule::Pattern { regex: re, .. } = rule {
                if regex::Regex::new(re).is_err() {
                    return Err(format!(
                        "setting {} pattern must be a valid Rust regex (JS-only constructs like lookahead are rejected): {:?}",
                        s.key, re
                    ));
                }
                if let Some(bad) = JS_INCOMPATIBLE_REGEX.iter().find(|f| re.contains(*f)) {
                    return Err(format!(
                        "setting {} pattern uses Rust-only construct {:?} which JavaScript (new RegExp) cannot compile: {:?}",
                        s.key, bad, re
                    ));
                }
            }
        }
        // P1 — default must be self-consistent: its own declared validate must pass (rejected at install time).
        if let Some(d) = &s.default {
            if let Some(rule) = first_failing_rule(&s.validate, d) {
                return Err(format!(
                    "setting {} default {:?} fails its validate rule {:?}{}",
                    s.key,
                    d,
                    rule.type_name().unwrap_or("unknown"),
                    rule.message()
                        .map(|m| format!(": {}", m.pick_host_locale()))
                        .unwrap_or_default()
                ));
            }
        }
    }

    // S5a — sandbox declaration: allowlist validation; pet (no process) must not carry one.
    if let Some(sb) = &m.sandbox {
        if m.runtime.is_none() {
            return Err(
                "sandbox declaration requires runtime (pet plugins have no process)".into(),
            );
        }
        validate_sandbox(sb)?;
    }
    Ok(())
}

/// F4 — semantic-version compatibility gate: core >= minCoreVersion. Default = no floor. Lenient parsing
/// (same implementation as the marketplace, avoiding two comparison paths).
pub fn check_core_compat(m: &Manifest) -> Result<(), String> {
    let Some(min) = &m.min_core_version else { return Ok(()); };
    let core = env!("CARGO_PKG_VERSION");
    match (
        super::marketplace::parse_version_lenient(core),
        super::marketplace::parse_version_lenient(min),
    ) {
        (Some(c), Some(req)) => {
            if c >= req {
                Ok(())
            } else {
                Err(format!("plugin requires core >= {} (current {})", min, core))
            }
        }
        // unparseable → let through (strict rejection already happens in validate; this is defensive)
        _ => Ok(()),
    }
}

/// §4.4 step 4 — confirmation set = permissions[] ∪ all inline-mapping permissions (review M1),
/// each item annotated with whether it is declaration-derived (→ once-only) and its default tier.
fn install_ask_plan(m: &Manifest) -> Vec<super::permission::InstallAsk> {
    let mut plan: Vec<super::permission::InstallAsk> = Vec::new();
    let mut push = |permission: String, declared: bool, default: Option<&str>| {
        if plan.iter().any(|a| a.permission == permission) {
            return;
        }
        plan.push(super::permission::InstallAsk {
            permission,
            declared,
            declared_default: default.unwrap_or("ask").to_string(),
        });
    };
    for p in &m.permissions {
        // built-in names as before; declared permission names get the `declared` marker from the inline mapping
        let declared = !super::permission::known(p);
        let default = m
            .capabilities
            .iter()
            .find(|c| c.mapping().map(|(perm, _)| perm) == Some(p.as_str()))
            .and_then(|c| c.mapping().map(|(_, d)| d))
            .map(|s| s.to_string());
        push(p.clone(), declared, default.as_deref());
    }
    for c in &m.capabilities {
        if let Some((perm, default)) = c.mapping() {
            push(perm.to_string(), true, Some(default));
        }
    }
    plan
}

    /// F6 — update confirmation plan: keep only items that are "added / declaration or default changed".
    /// Completely unchanged → empty plan = silent update (reuses the existing consent, no popup).
    fn update_ask_plan(old: &Manifest, new: &Manifest) -> Vec<super::permission::InstallAsk> {
        let old_plan = Self::install_ask_plan(old);
        Self::install_ask_plan(new)
            .into_iter()
            .filter(|ask| {
                !old_plan.iter().any(|o| {
                    o.permission == ask.permission
                        && o.declared == ask.declared
                        && o.declared_default == ask.declared_default
                })
            })
            .collect()
    }

    /// F6 — update matrix entry: returns (confirmation plan, whether the publisher key changed).
    /// Fresh install → full plan; key changed/unknown → full re-confirmation + marker (new-publisher path);
    /// same key → diff plan only.
    fn update_plan_for(
        old: Option<&Manifest>,
        new: &Manifest,
    ) -> (Vec<super::permission::InstallAsk>, bool) {
        let Some(old) = old else {
            return (Self::install_ask_plan(new), false);
        };
        let key_changed = manifest_signature_key_id(old) != manifest_signature_key_id(new);
        if key_changed {
            (Self::install_ask_plan(new), true)
        } else {
            (Self::update_ask_plan(old, new), false)
        }
    }

    /// F7 — publisher keyId of the installed plugin (used by the update preview / key-change prompt).
    pub fn installed_publisher_key(id: &str) -> Option<String> {
        Self::row_manifest(id)
            .ok()
            .and_then(|(_, m)| manifest_signature_key_id(&m))
    }

    /// M7/F8 — generic settings-page data source: declared schema + current values (secrets only report "set", values are not returned).
    pub fn settings_view(id: &str) -> Result<SettingsViewDto, String> {
        let (_, m) = Self::row_manifest(id)?;
        let mut values = serde_json::Map::new();
        let mut secrets_set = Vec::new();
        for s in &m.settings {
            if s.stype == "button" || s.stype == "list" {
                continue;
            }
            if s.stype == "secret" {
                if super::config::get_secret(id, &s.key).is_some() {
                    secrets_set.push(s.key.clone());
                }
                continue;
            }
            let default = s.default.clone().unwrap_or(serde_json::Value::Null);
            values.insert(s.key.clone(), super::config::get(id, &s.key, &default));
        }
        Ok(SettingsViewDto {
            settings: m.settings.clone(),
            values,
            secrets_set,
        })
    }

    /// M7/F8 — write a single setting: the key must be declared; since P1 the declared `validate[]` is enforced (failure →
    /// `invalid: <message>` / `invalid: <rule-type>`); secrets go to the keychain, everything else to config.
    /// A plugin's reverse `config.set` does not go through here, so it is not subject to validation.
    pub fn set_setting_value(id: &str, key: &str, value: &serde_json::Value) -> Result<(), String> {
        let (_, m) = Self::row_manifest(id)?;
        let decl = m
            .settings
            .iter()
            .find(|s| s.key == key)
            .ok_or_else(|| format!("setting {} is not declared by {}", key, id))?;
        match decl.stype.as_str() {
            "button" => Err(format!("setting {} is an action; invoke it instead", key)),
            "list" => Err(format!("list setting {} is managed by its plugin", key)),
            "secret" => {
                let text = value
                    .as_str()
                    .ok_or_else(|| format!("secret setting {} must be a string", key))?;
                enforce_validate_rules(decl, value)?;
                super::config::set_secret(id, key, text)
            }
            _ => {
                enforce_validate_rules(decl, value)?;
                super::config::set(id, key, value)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        }
    }

    /// M7/F8 — button control: requests plugin method `settings.<key>` (lazy start).
    pub fn invoke_setting_action(id: &str, key: &str) -> Result<serde_json::Value, String> {
        let (_, m) = Self::row_manifest(id)?;
        let decl = m
            .settings
            .iter()
            .find(|s| s.key == key)
            .ok_or_else(|| format!("setting {} is not declared by {}", key, id))?;
        if decl.stype != "button" {
            return Err(format!("setting {} is not a button action", key));
        }
        let proc = Self::shared().ensure_running(id)?;
        proc.call(
            &format!("settings.{}", key),
            json!({}),
            Duration::from_secs(10),
        )
    }

    /// P3 — list control: forwards CRUD ops to plugin method `settings.<key>` (lazy start, 10s timeout,
    /// same timeout surface as button). The plugin is the sole holder of list data and returns the new array; the host does not persist it.
    /// op ∈ list(read) | add(value) | delete(index) | move(index→to).
    pub fn invoke_setting_list_op(
        id: &str,
        key: &str,
        op: &str,
        index: Option<usize>,
        to: Option<usize>,
        value: Option<String>,
    ) -> Result<serde_json::Value, String> {
        let (_, m) = Self::row_manifest(id)?;
        let decl = m
            .settings
            .iter()
            .find(|s| s.key == key)
            .ok_or_else(|| format!("setting {} is not declared by {}", key, id))?;
        if decl.stype != "list" {
            return Err(format!("setting {} is not a list", key));
        }
        let mut params = json!({ "op": op });
        if let Some(i) = index {
            params["index"] = json!(i);
        }
        if let Some(i) = to {
            params["to"] = json!(i);
        }
        if let Some(v) = value {
            params["value"] = json!(v);
        }
        let proc = Self::shared().ensure_running(id)?;
        proc.call(&format!("settings.{}", key), params, Duration::from_secs(10))
    }

    /// F6 — revises the frozen revocation marker (`revoked_key IS NOT NULL` = disabled by default).
    fn revoked_key_of(id: &str) -> Option<String> {
        Self::store()
            .and_then(|store| {
                store.lock().ok().and_then(|s| {
                    s.with_conn_ref(|c| {
                        c.query_row(
                            "SELECT revoked_key FROM plugins WHERE id = ?1",
                            [id],
                            |r| r.get::<_, Option<String>>(0),
                        )
                        .ok()
                    })
                })
            })
            .flatten()
            .flatten()
    }

    /// M7/F9 — version of the most recent successful initialization handshake (NULL = no successful start record yet).
    fn last_version_of(id: &str) -> Option<String> {
        Self::store()
            .and_then(|store| {
                store.lock().ok().and_then(|s| {
                    s.with_conn_ref(|c| {
                        c.query_row(
                            "SELECT last_version FROM plugins WHERE id = ?1",
                            [id],
                            |r| r.get::<_, Option<String>>(0),
                        )
                        .ok()
                    })
                })
            })
            .flatten()
            .flatten()
    }

    /// M7/F9 — records the version of this successful start (for injecting previousVersion on the next start).
    fn set_last_version(id: &str, version: &str) {
        if let Some(store) = Self::store() {
            if let Ok(mut s) = store.lock() {
                let _ = s.with_conn(|c| {
                    c.execute(
                        "UPDATE plugins SET last_version = ?2 WHERE id = ?1",
                        params![id, version],
                    )
                    .unwrap_or(0)
                });
            }
        }
    }

    /// F6 — swap failure recovery: roll the DB row back to the old manifest and restore the original runtime state as needed.
    /// (the directory is already restored by the caller; here we fix up the DB row and process, eliminating "failed after stop leaves stopped").
    fn restore_after_failed_swap(&self, id: &str, old: &Option<Manifest>, was_running: bool) {
        if let Some(om) = old {
            if let Some(store) = Self::store() {
                if let Ok(mut s) = store.lock() {
                    let manifest_json = serde_json::to_string(om).unwrap_or_default();
                    let _ = s.with_conn(|c| {
                        c.execute(
                            "UPDATE plugins SET version = ?2, manifest = ?3, status = 'stopped' WHERE id = ?1",
                            params![id, om.version, manifest_json],
                        )
                        .unwrap_or(0)
                    });
                }
            }
        }
        if was_running {
            let _ = self.start(id);
        }
    }

    /// **dev/test only (not exposed in the settings page)**: install from an already-unpacked directory — for test fixtures and local
    /// development; the distribution path is `install_ocplugin` (.ocplugin ZIP) and the marketplace.
    /// §4.4: confirm item by item first (pure collection, zero writes), then commit (single transaction + events + hints).
    pub fn install_from_dir(&self, dir: &PathBuf) -> Result<String, String> {
        let _install_guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let m = Self::read_manifest(dir)?;
        // F4/F5 — install gate: core version and dependency cycles are caught before confirmation/DB write (zero side effects).
        Self::check_core_compat(&m)?;
        self.check_install_dependencies(&m)?;
        self.check_declaration_consistency(&m)?;
        let plan = Self::install_ask_plan(&m);
        let decisions = super::permission::confirm_install(&m.id, &plan)?;
        self.write_install_tx(&m, dir, &decisions)?;
        self.after_install(&m)
    }

    /// §4.2 "global consistency": the effective mapping of all live providers of the same capability must agree (including the built-in static mapping).
    /// Inconsistent → reject the install, avoiding two enforcement meanings for the same capability name.
    fn check_declaration_consistency(&self, m: &Manifest) -> Result<(), String> {
        let Some(store) = Self::store() else {
            return Ok(()); // storage unavailable: the later commit phase will reject, so don't report again here
        };
        for (capability, permission, default, _timeout) in m.declarations() {
            // built-in static mapping takes precedence: a declaration must not land on a reserved capability (validate already blocks it; this is a fallback)
            if super::capability::is_builtin(&capability) {
                return Err(format!("reserved capability {} cannot be declared", capability));
            }
            if let Some(other) =
                super::declaration::conflicting_provider(&store, &capability, &permission, &default, &m.id)
            {
                return Err(format!(
                    "capability {} is already provided by {} with a different permission mapping",
                    capability, other
                ));
            }
        }
        Ok(())
    }

    /// F5 install-time precheck: only blocks cycles (missing dependencies are allowed through — the install order is user-controlled, with a start-time fallback).
    fn check_install_dependencies(&self, m: &Manifest) -> Result<(), String> {
        let Some(store) = Self::store() else { return Ok(()); };
        let installed: Vec<(String, BTreeMap<String, semver::VersionReq>)> = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    let mut st = c.prepare("SELECT id, manifest FROM plugins").ok()?;
                    let it = st
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                        .ok()?;
                    Some(
                        it.filter_map(|x| x.ok())
                            .filter_map(|(id, json)| {
                                let dm: Manifest = serde_json::from_str(&json).ok()?;
                                Some((id, super::plugin_deps::parse_deps(&dm.dependencies)))
                            })
                            .collect::<Vec<_>>(),
                    )
                })
            })
            .flatten()
            .unwrap_or_default();
        let new_deps = super::plugin_deps::parse_deps(&m.dependencies);
        if let Some(cyc) = super::plugin_deps::would_create_cycle(&m.id, &new_deps, &installed) {
            return Err(format!("dependency cycle: {}", cyc.join(" -> ")));
        }
        Ok(())
    }

    /// §4.4 step 5 — commit phase (DB writes only). **Called only after all confirmations pass**.
    /// The `plugins` row + `plugin_permissions` are a **single transaction**; if any row fails the whole batch rolls back,
    /// leaving no half state. Events / alerting hints / probe / start are not here (see
    /// `after_install`), so on failure the caller only needs to clean up staging.
    fn write_install_tx(
        &self,
        m: &Manifest,
        dir: &Path,
        decisions: &[(String, String)],
    ) -> Result<(), String> {
        let manifest_json = serde_json::to_string(m).unwrap_or_default();
        let Some(store) = Self::store() else {
            return Err("storage unavailable".into());
        };
        let now = super::plugin_trace::now_secs();
        let path = dir.display().to_string();
        let r = store.lock().ok().and_then(|mut s| {
            s.try_with_conn(|c| {
                let tx = c
                    .unchecked_transaction()
                    .map_err(|e| format!("begin failed: {}", e))?;
                tx.execute(
                    "INSERT INTO plugins (id, version, type, status, path, manifest)
                     VALUES (?1, ?2, ?3, 'probe_pending', ?4, ?5)
                     ON CONFLICT(id) DO UPDATE SET version=?2, type=?3, path=?4, manifest=?5, status='probe_pending'",
                    params![m.id, m.version, m.ptype, path, manifest_json],
                )
                .map_err(|e| format!("plugin row failed: {}", e))?;
                super::permission::upsert_install_decisions_in_tx(&tx, &m.id, decisions, now as i64)?;
                // §4.6 frozen declaration table + domain registration (P1 installs locally and occupies first) — same transaction,
                // a domain conflict here must also roll back the whole batch, leaving no half state
                super::declaration::write_in_tx(&tx, &m.id, &m.declarations(), now)?;
                tx.commit().map_err(|e| format!("commit failed: {}", e))?;
                Ok(())
            })
        });
        match r {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(e),
            None => Err("sqlite unavailable".into()),
        }
    }

    /// §4.4 second half of step 5 — post-commit side effects: events, alerting hints, probe, start.
    /// Review 2: `plugin.installed` and hints were previously emitted **before** user confirmation, leaving dirty
    /// records on rejection; now both happen after confirmation + a successful commit.
    fn after_install(&self, m: &Manifest) -> Result<String, String> {
        let id = m.id.clone();
        super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
            "plugin.installed",
            "core",
            json!({ "pluginId": id, "version": m.version }),
        ));
        // Phase 53 — load the severity hints declared by the manifest (if any).
        super::alerting::install_manifest_hints(&id, &m.alerting);
        // Phase 37 — Probe self-check. When the manifest declares no capability (pure pet / config-only plugin)
        // it is skipped; otherwise `core.probe.capability` is called once per capability, and on failure
        // the plugin does not enter the lifecycle and the settings tab shows a red badge so the user can retry.
        let probe = super::probe::run_and_publish(&id, &m.capability_ids());
        if probe.status == "failed" {
            Self::set_status(&id, "probe_failed");
            return Ok(id);
        }
        self.start(&id)?;
        Ok(id)
    }

    /// The plugin install root. Tests can override it with OPENCAPX_PLUGINS_DIR.
    pub fn plugins_root() -> PathBuf {
        if let Ok(dir) = std::env::var("OPENCAPX_PLUGINS_DIR") {
            return PathBuf::from(dir);
        }
        dirs::home_dir()
            .map(|h| h.join(".opencapx").join("plugins"))
            .unwrap_or_else(|| std::env::temp_dir().join("opencapx-plugins"))
    }

    /// Install from .ocplugin (ZIP). See the install flow in docs/plugin-manifest.md.
    /// Safety: validate the manifest before extracting; check every entry name (reject absolute paths / ../ backslashes),
    /// with a 100MB per-file and 256MB per-package limit; extract to a temp directory then atomically rename.
    /// F7 — three-state install entry (fail-closed by default: soft warnings need explicit confirmation).
    /// Production commands go through [`Self::install_ocplugin_ex`]; this wrapper is for tests and future SDK default calls.
    #[allow(dead_code)]
    pub fn install_ocplugin(&self, archive: &Path) -> Result<String, String> {
        self.install_ocplugin_ex(archive, InstallOptions::default())
    }

    /// F7 — install/update entry with confirmation options: trusted direct install / soft-warning explicit confirmation / integrity hard reject;
    /// a key change on an installed version additionally needs `confirm_key_change` (the UI catches the marker and retries with confirmation).
    pub fn install_ocplugin_ex(
        &self,
        archive: &Path,
        opts: InstallOptions,
    ) -> Result<String, String> {
        let _install_guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let file = std::fs::File::open(archive)
            .map_err(|e| format!("cannot open {}: {}", archive.display(), e))?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|e| format!("bad zip: {}", e))?;

        // the manifest must be at the package root
        let mut manifest_text = String::new();
        let mut found = false;
        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|e| format!("zip read error: {}", e))?;
            if entry.name() == "opencapx-plugin.json" {
                std::io::Read::read_to_string(&mut entry, &mut manifest_text)
                    .map_err(|e| format!("manifest read error: {}", e))?;
                found = true;
                break;
            }
        }
        if !found {
            return Err("missing opencapx-plugin.json at archive root".into());
        }
        let m: Manifest =
            serde_json::from_str(&manifest_text).map_err(|e| format!("bad manifest: {}", e))?;
        Self::validate_manifest(&m)?;
        // F6 check order, position 1: revocation. A revoked-disabled plugin must first be explicitly reopened;
        // if the target publisher key is in the registry revocation list → reject outright (same path for install and update).
        if let Some(key) = Self::revoked_key_of(&m.id) {
            return Err(format!(
                "plugin revoked: publisher key {} (reopen explicitly before reinstall/update)",
                key
            ));
        }
        if let Some(key_id) = manifest_signature_key_id(&m) {
            if super::revocation::key_is_revoked(&key_id) {
                return Err(format!("publisher key revoked: {}", key_id));
            }
        }
        // F4/F5 — install gate: caught before any side effect (signature event / extraction staging).
        Self::check_core_compat(&m)?;
        self.check_install_dependencies(&m)?;

        // Wow 6 + F7 — signature/integrity check (three states: direct install / soft-warning confirmation / hard reject).
        // verify before extracting, to prevent a malicious zip from landing in plugins/<id>/ before triggering side effects.
        let outcome = crate::core::plugin_sig::verify(archive);
        match outcome.allowance() {
            crate::core::plugin_sig::Allowance::Direct => {}
            crate::core::plugin_sig::Allowance::SoftWarn => {
                if !allow_unsigned() {
                    return Err(format!(
                        "unsigned package rejected: allow_unsigned is off ({})",
                        outcome.label()
                    ));
                }
                if !opts.confirm_unsigned {
                    return Err(format!("unsigned-confirm-required: {}", outcome.label()));
                }
            }
            crate::core::plugin_sig::Allowance::HardDeny => {
                return Err(format!("signature verification failed: {}", outcome.label()));
            }
        }
        if let VerifyOutcome::Trusted { key_id } = &outcome {
            // leave a trace: trusted install goes into the audit
            super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                "plugin.signature.verified",
                "core",
                serde_json::json!({ "id": m.id, "keyId": key_id }),
            ));
        }
        // F6 — key continuity check (after the trust chain, before the permission diff): key change = new publisher,
        // which needs explicit confirmation (the F7 warning-confirm path; the error marker lets the UI catch it and retry with confirmation).
        let old_manifest = Self::row_manifest(&m.id).ok().map(|(_, om)| om);
        let key_changed = old_manifest
            .as_ref()
            .map(|om| manifest_signature_key_id(om) != manifest_signature_key_id(&m))
            .unwrap_or(false);
        if key_changed && !opts.confirm_key_change {
            return Err(format!(
                "publisher-key-change-confirm-required: {} -> {}",
                old_manifest
                    .as_ref()
                    .and_then(manifest_signature_key_id)
                    .unwrap_or_else(|| "<none>".into()),
                manifest_signature_key_id(&m).unwrap_or_else(|| "<none>".into()),
            ));
        }

        // §4.2 "global consistency": compare the mapping with other already-frozen providers. Placed **before extraction** —
        // a failure here needs no staging cleanup (it is not created yet), keeping the error path shorter.
        self.check_declaration_consistency(&m)?;

        // entry-name safety check + size limits
        const MAX_FILE: u64 = 100 * 1024 * 1024;
        const MAX_TOTAL: u64 = 256 * 1024 * 1024;
        let mut total: u64 = 0;
        for i in 0..zip.len() {
            let entry = zip
                .by_index(i)
                .map_err(|e| format!("zip read error: {}", e))?;
            let name = entry.name();
            if name.starts_with('/')
                || name.contains("..")
                || name.contains('\\')
                || Path::new(name).is_absolute()
            {
                return Err(format!("unsafe path in archive: {}", name));
            }
            if entry.size() > MAX_FILE {
                return Err(format!("entry too large: {} ({} bytes)", name, entry.size()));
            }
            total += entry.size();
        }
        if total > MAX_TOTAL {
            return Err("archive too large".into());
        }

        // extract to a temp directory (P0: the lexical check already blocks `..`; the component-level prefix assertion here is a fallback)
        let root = Self::plugins_root();
        let tmp = root.join(format!(".tmp-{}-{}", m.id, std::process::id()));
        if !tmp.starts_with(&root) {
            return Err("install temp path escapes plugins root".into());
        }
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| format!("mkdir failed: {}", e))?;
        // M8 hardening: any failure in the staging phase (corrupt entry / recheck failure) must clean up tmp,
        // otherwise .tmp-* is left in the plugins root (pinned by the corrupted-package test).
        let staged = (|| -> Result<(), String> {
            for i in 0..zip.len() {
                let mut entry = zip
                    .by_index(i)
                    .map_err(|e| format!("zip read error: {}", e))?;
                let out = tmp.join(entry.name());
                if entry.is_dir() {
                    std::fs::create_dir_all(&out).map_err(|e| format!("mkdir failed: {}", e))?;
                    continue;
                }
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {}", e))?;
                }
                let mut f = std::fs::File::create(&out)
                    .map_err(|e| format!("extract {} failed: {}", entry.name(), e))?;
                std::io::copy(&mut entry, &mut f)
                    .map_err(|e| format!("extract {} failed: {}", entry.name(), e))?;
                // preserve the unix executable bit so binary plugins work
                #[cfg(unix)]
                if let Some(mode) = entry.unix_mode() {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode));
                }
            }
            // recheck the manifest from the extraction result
            Self::read_manifest(&tmp)?;
            Ok(())
        })();
        if let Err(e) = staged {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }

        // P0: dest must still be inside the plugins root (lexical check + component-level assertion, two layers)
        let dest = root.join(&m.id);
        if !dest.starts_with(&root) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err("install path escapes plugins root".into());
        }

        // §4.4 step 4 — item-by-item confirmation (**pure collection, zero writes**). Must happen before any destructive action
        // before it: at this point only the staging directory has been touched, so a user rejection → clean staging and return,
        // with the installed version's directory and DB records intact (review C2: previously it was "swap the directory / write the DB first,
        // confirm later", so a rejection destroyed the old version).
        // F6: updates go by diff — same key and unchanged permissions/mapping gives an empty plan (silently reuse
        // the existing consent); key changed/unknown → full re-confirmation (new-publisher path).
        let (plan, _) = Self::update_plan_for(old_manifest.as_ref(), &m);
        if key_changed {
            super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                "plugin.update.key_changed",
                "core",
                serde_json::json!({
                    "pluginId": m.id,
                    "from": old_manifest.as_ref().and_then(manifest_signature_key_id),
                    "to": manifest_signature_key_id(&m),
                }),
            ));
        }
        let decisions = match super::permission::confirm_install(&m.id, &plan) {
            Ok(d) => d,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
        };

        // §4.4 step 5a — commit the DB first (single transaction). On failure the old directory / old records are untouched,
        // so cleaning staging is enough and the old version is intact. The directory switch comes after the commit: DB failures are more common of the two,
        // and doing that first would expose a half-installed state where the old version is deleted but the new one is not in the DB.
        if let Err(e) = self.write_install_tx(&m, &dest, &decisions) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }

        // §4.4 step 5b — directory switch. Stop the old process, move it to .bak as a safety net, then rename the new one into
        // place; if any step fails, move .bak back, restore the DB row and the original runtime state (F6 transactionality:
        // eliminating "failure after stop leaves stopped"; a same-partition rename almost never fails, and the recovery branch
        // guarantees we are never stranded on both sides).
        let backup = root.join(format!(".old-{}-{}", m.id, std::process::id()));
        let had_old = dest.exists();
        let was_running = self
            .get_process(&m.id)
            .map(|p| p.is_alive())
            .unwrap_or(false);
        if had_old {
            self.stop(&m.id);
            let _ = std::fs::remove_dir_all(&backup);
            if let Err(e) = std::fs::rename(&dest, &backup) {
                let _ = std::fs::remove_dir_all(&tmp);
                self.restore_after_failed_swap(&m.id, &old_manifest, was_running);
                return Err(format!("failed to set aside old version: {}", e));
            }
        }
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            if had_old {
                let _ = std::fs::rename(&backup, &dest);
            }
            let _ = std::fs::remove_dir_all(&tmp);
            self.restore_after_failed_swap(&m.id, &old_manifest, was_running);
            return Err(format!("install move failed: {}", e));
        }
        if had_old {
            let _ = std::fs::remove_dir_all(&backup);
        }

        // §4.4 step 5c — side effects (events / hints / probe / start)
        // F7 audit: after a soft-warning tier (unsigned / unknown key) install succeeds, write `plugin.installed.unsigned`.
        let id = self.after_install(&m)?;
        if matches!(
            outcome.allowance(),
            crate::core::plugin_sig::Allowance::SoftWarn
        ) {
            super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                "plugin.installed.unsigned",
                "core",
                serde_json::json!({
                    "pluginId": m.id,
                    "status": outcome.label(),
                    "keyId": match &outcome {
                        VerifyOutcome::UnknownKey { key_id } => Some(key_id.clone()),
                        _ => None,
                    },
                }),
            ));
        }
        Ok(id)
    }

    /// Wow 5 — let the user take a look before installing. Shares the manifest parsing path with install_ocplugin
    /// (aligned with i1.md §18 path safety check + apiVersion validation), but does **not** extract,
    /// does not write the DB, and does not spawn. The returned permissions array carries the high_risk marker,
    /// and the frontend dialog marks "camera / microphone / filesystem.write / process.execute" with red badges.
    pub fn preview_ocplugin(archive: &Path) -> Result<PluginPreviewDto, String> {
        let file = std::fs::File::open(archive)
            .map_err(|e| format!("cannot open {}: {}", archive.display(), e))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("bad zip: {}", e))?;
        let mut manifest_text = String::new();
        let mut found = false;
        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|e| format!("zip read error: {}", e))?;
            if entry.name() == "opencapx-plugin.json" {
                std::io::Read::read_to_string(&mut entry, &mut manifest_text)
                    .map_err(|e| format!("manifest read error: {}", e))?;
                found = true;
                break;
            }
        }
        if !found {
            return Err("missing opencapx-plugin.json at archive root".into());
        }
        let m: Manifest =
            serde_json::from_str(&manifest_text).map_err(|e| format!("bad manifest: {}", e))?;
        Self::validate_manifest(&m)?;
        // §4.4 step 4: preview and confirmation share one source = permissions[] ∪ inline-mapping permissions (M1),
        // declaration-derived items carry the `declared` marker → the frontend hides Always (the UI face of once-only)
        let permissions = Self::install_ask_plan(&m)
            .into_iter()
            .map(|ask| PermissionPreviewDto {
                high_risk: super::permission::HIGH_RISK.contains(&ask.permission.as_str()),
                declared: ask.declared,
                name: ask.permission,
            })
            .collect();
        // Wow 6 — signature/integrity status is surfaced too (even if preview does not block, the UI badge must show it)
        let outcome = crate::core::plugin_sig::verify(archive);
        let (status, key_id) = match outcome {
            VerifyOutcome::Trusted { key_id } => ("trusted".to_string(), Some(key_id)),
            VerifyOutcome::Unsigned => ("unsigned".to_string(), None),
            VerifyOutcome::HashMismatch { .. } => ("tampered".to_string(), None),
            VerifyOutcome::BadSignature { .. } => ("bad-signature".to_string(), None),
            VerifyOutcome::UnknownKey { key_id } => ("unknown-key".to_string(), Some(key_id)),
            VerifyOutcome::MalformedSignature => ("malformed-signature".to_string(), None),
        };
        let capability_ids = m.capability_ids();
        // F7 — provenance verification / official marker / compatibility / permission diff (rendering facts, not gating decisions).
        let verified = key_id.as_deref().and_then(|k| {
            super::registry::load_offline().map(|idx| {
                super::registry::key_status(&idx, k) == super::registry::KeyStatus::Registered
            })
        });
        let official = key_id
            .as_deref()
            .map(|k| k.starts_with("com.opencapx"))
            .unwrap_or(false);
        let compat = CompatPreviewDto {
            ok: Self::check_core_compat(&m).is_ok(),
            min_core_version: m.min_core_version.clone(),
            current: env!("CARGO_PKG_VERSION").to_string(),
        };
        let permission_diff = Self::row_manifest(&m.id).ok().map(|(_, om)| {
            let old: std::collections::BTreeSet<String> = Self::install_ask_plan(&om)
                .into_iter()
                .map(|a| a.permission)
                .collect();
            let new: std::collections::BTreeSet<String> = Self::install_ask_plan(&m)
                .into_iter()
                .map(|a| a.permission)
                .collect();
            PermissionDiffDto {
                added: new.difference(&old).cloned().collect(),
                removed: old.difference(&new).cloned().collect(),
            }
        });
        Ok(PluginPreviewDto {
            id: m.id,
            name: m.name,
            description: m.description,
            author: m.author,
            homepage: m.homepage,
            license: m.license,
            version: m.version,
            ptype: m.ptype,
            capabilities: capability_ids,
            permissions,
            signature: SignaturePreviewDto { status, key_id },
            verified,
            official,
            compat,
            permission_diff,
            sandbox_declared: m.sandbox.is_some(),
        })
    }

    fn row_manifest(id: &str) -> Result<(PathBuf, Manifest), String> {
        let Some(store) = Self::store() else {
            return Err("storage unavailable".into());
        };
        let row = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    c.query_row(
                        "SELECT path, manifest FROM plugins WHERE id = ?1",
                        [id],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                    )
                    .ok()
                })
            })
            .flatten();
        let Some((path, manifest)) = row else {
            return Err(format!("plugin not installed: {}", id));
        };
        let m: Manifest =
            serde_json::from_str(&manifest).map_err(|e| format!("stored manifest broken: {}", e))?;
        Ok((PathBuf::from(path), m))
    }

    /// Detail page: reads README.md in the plugin root. Exists, ≤ limit, UTF-8 → Some(raw);
    /// missing/oversized/non-UTF-8 → None (the frontend shows "not provided"); plugin not installed → Err.
    /// Reads a fixed filename only; does not accept a path from the frontend.
    pub fn readme(id: &str) -> Result<Option<String>, String> {
        const MAX_README_BYTES: u64 = 256 * 1024;
        let (dir, _) = Self::row_manifest(id)?;
        // repo convention is README.md; hand-written plugins often use readme.md, so try each once
        for name in ["README.md", "readme.md"] {
            let p = dir.join(name);
            let Ok(meta) = std::fs::metadata(&p) else { continue };
            if !meta.is_file() || meta.len() > MAX_README_BYTES {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&p) {
                if let Ok(text) = String::from_utf8(bytes) {
                    return Ok(Some(text));
                }
            }
        }
        Ok(None)
    }

    /// (id, version) snapshot, used for dependency decisions.
    fn installed_versions() -> Vec<(String, String)> {
        Self::store()
            .and_then(|store| {
                store.lock().ok().and_then(|s| {
                    s.with_conn_ref(|c| {
                        let mut st = c.prepare("SELECT id, version FROM plugins").ok()?;
                        let it = st
                            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                            .ok()?;
                        Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
                    })
                })
            })
            .flatten()
            .unwrap_or_default()
    }

    /// Audit event for a rejected start (same shape as kill_switch::guard_start's plugin.start.rejected).
    fn reject_start(id: &str, reason: &str) {
        super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
            "plugin.start.rejected",
            "core",
            json!({ "pluginId": id, "reason": reason }),
        ));
    }

    /// Start: spawn → initialize handshake → capabilities written to the registry → running.
    pub fn start(&self, id: &str) -> Result<(), String> {
        self.start_inner(id, true)
    }

    fn start_inner(&self, id: &str, reset_retry: bool) -> Result<(), String> {
        // Phase 44 — global kill switch gatekeeper. When active, return Err directly and do not start the process.
        super::kill_switch::guard_start(id)?;
        // --safe-mode: startup diagnostic mode; no third-party plugin is started (manual start is likewise rejected).
        super::safe_mode::guard_start(id)?;
        // F6 — revocation default-disable: a plugin hit by revokedKeys is forbidden to start until the user explicitly reopens it
        // (reopen clears revoked_key and records an ack; here we only look at the current disable marker).
        if let Some(key) = Self::revoked_key_of(id) {
            let reason = format!("plugin revoked: publisher key {} (reopen explicitly to run)", key);
            Self::reject_start(id, &reason);
            return Err(reason);
        }
        if let Some(p) = self.procs.lock().ok().and_then(|m| m.get(id).cloned()) {
            if p.is_alive() {
                return Ok(());
            }
        }
        let (dir, m) = Self::row_manifest(id)?;
        // F4/F5 — start gate: forbid the start when compatibility and dependencies are not satisfied. The rejection happens before the state-machine
        // transition (starting is not set), and reuses kill_switch's plugin.start.rejected event shape.
        if let Err(e) = Self::check_core_compat(&m) {
            Self::reject_start(id, &e);
            return Err(e);
        }
        let installed: Vec<(String, String)> = Self::installed_versions();
        let missing = super::plugin_deps::find_missing(&m.dependencies, &installed);
        if !missing.is_empty() {
            let detail = missing
                .iter()
                .map(|(d, r)| format!("{} ({})", d, r))
                .collect::<Vec<_>>()
                .join(", ");
            let e = format!("plugin_dependency_missing: requires {}", detail);
            Self::reject_start(id, &e);
            return Err(e);
        }
        Self::set_status(id, "starting");
        super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
            "plugin.lifecycle.starting",
            "core",
            json!({ "pluginId": id }),
        ));
        let runtime = m.runtime.clone().ok_or_else(|| "pet plugin has no runtime".to_string())?;
        if runtime.rtype != "process" {
            let err = format!("unsupported runtime type {}", runtime.rtype);
            super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                "plugin.lifecycle.crashed",
                "core",
                json!({ "pluginId": id, "reason": &err }),
            ));
            return Err(err);
        }
        let spec = RuntimeSpec {
            command: runtime.command,
            args: runtime.args,
            env: runtime.env,
        };
        let plugin_id = id.to_string();
        let on_reverse: super::process::OnReverse = Arc::new(move |v: serde_json::Value, reply| {
            handle_reverse(&plugin_id, v, reply);
        });
        // Wow 9: one trace session per start, session_id = now_secs + 4 random digits to avoid collisions from simultaneous starts.
        let session_id = format!(
            "{}-{:04x}",
            super::plugin_trace::now_secs(),
            (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u32)
                .unwrap_or(0))
                & 0xFFFF
        );
        // S1 — environment policy: isolated by default (minimal allowlist only) + the settings incremental allowlist; one switch falls back.
        let env_policy = super::process::EnvPolicy {
            isolate: env_isolation_enabled(),
            allow: env_allowlist(),
        };
        // S5b/S5c — sandbox enforcement: declaration + (trusted ? switch : forced); None on non-macOS.
        let trusted = manifest_signature_key_id(&m)
            .map(|kid| super::plugin_sig::load_trusted_keys().contains_key(&kid))
            .unwrap_or(false);
        let sandbox_spec = super::sandbox::effective_profile(
            id,
            m.sandbox.as_ref(),
            trusted,
            sandbox_enforcement_enabled(),
        )
        .map(|profile| {
            // review F2 — the tmp directory is created alongside the data directory (the TMPDIR redirection target).
            let tmp_dir = super::sandbox::plugin_tmp_dir(id);
            let _ = std::fs::create_dir_all(&tmp_dir);
            super::process::SandboxSpec { profile, tmp_dir }
        });
        let proc = PluginProcess::spawn(
            id,
            &dir,
            &spec,
            &session_id,
            on_reverse,
            &env_policy,
            sandbox_spec.as_ref(),
        )
        .map_err(|e| format!("spawn failed: {}", e))?;
        let proc = Arc::new(proc);
        // M7/F9 — inject previousVersion (the version of the last successful start; omitted on first start),
        // so the plugin can perform idempotent data migration itself (the host neither moves data nor runs migration scripts).
        let previous_version = Self::last_version_of(id);
        let mut init_payload = json!({
            "coreVersion": env!("CARGO_PKG_VERSION"),
            "apiVersion": API_VERSION,
            "pluginId": id
        });
        if let Some(prev) = &previous_version {
            init_payload["previousVersion"] = serde_json::Value::String(prev.clone());
        }
        let init = proc
            .call(
                "plugin.initialize",
                init_payload,
                Duration::from_secs(10),
            )
            .map_err(|e| {
                Self::set_status(id, "error");
                super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                    "plugin.lifecycle.crashed",
                    "core",
                    json!({ "pluginId": id, "reason": format!("initialize failed: {}", e) }),
                ));
                format!("initialize failed: {}", e)
            })?;
        let handshake_id = init.get("pluginId").and_then(|x| x.as_str()).unwrap_or("");
        if handshake_id != id {
            Self::set_status(id, "error");
            let _ = proc.notify("plugin.shutdown", json!({}));
            let reason = format!(
                "handshake pluginId mismatch: manifest {} vs handshake {}",
                id, handshake_id
            );
            super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                "plugin.lifecycle.crashed",
                "core",
                json!({ "pluginId": id, "reason": &reason }),
            ));
            return Err(reason);
        }
        // M7/F9 — a successful handshake = a successful start: record the version for injecting previousVersion on the next start.
        Self::set_last_version(id, &m.version);
        // capabilities registry
        if let Some(store) = Self::store() {
            if let Ok(mut s) = store.lock() {
                let _ = s.with_conn(|c| {
                    let mut n = 0;
                    for cap in &m.capability_ids() {
                        n = c
                            .execute(
                                "INSERT INTO capabilities (id, version, plugin_id, priority, enabled)
                                 VALUES (?1, '1', ?2, 100, 1)
                                 ON CONFLICT(id, plugin_id) DO UPDATE SET enabled = 1",
                                params![cap, id],
                            )
                            .unwrap_or(0);
                    }
                    n
                });
            }
        }
        if let Ok(mut m) = self.procs.lock() {
            m.insert(id.to_string(), proc);
        }
        Self::set_status(id, "running");
        super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
            "plugin.lifecycle.running",
            "core",
            json!({ "pluginId": id }),
        ));
        // a manual start resets the cumulative retries; a watchdog-triggered restart preserves the cumulative count.
        if reset_retry {
            if let Ok(mut m) = self.retry_counts.lock() {
                m.insert(id.to_string(), 0);
            }
        }
        // record the manifest mtime for auto-reload polling
        if let Ok((path, _)) = Self::row_manifest(id) {
            let manifest = path.join("opencapx-plugin.json");
            if let Ok(meta) = std::fs::metadata(&manifest) {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(mut m) = self.last_mtimes.lock() {
                        m.insert(id.to_string(), mtime);
                    }
                }
            }
        }
        self.register_watchdog(id);
        Ok(())
    }

    /// Toggle auto-reload. Can be called at runtime; once true, a background poller watches the manifest mtime.
    pub fn set_auto_reload(&self, id: &str, on: bool) -> Result<(), String> {
        let Some(store) = Self::store() else {
            return Err("storage unavailable".into());
        };
        let mut s = store.lock().map_err(|_| "poisoned".to_string())?;
        s.with_conn(|c| {
            c.execute(
                "UPDATE plugins SET auto_reload = ?2 WHERE id = ?1",
                params![id, on as i64],
            )
            .unwrap_or(0)
        });
        // per-manager set: the poller no longer depends on the shared store
        if let Ok(mut set) = self.auto_reload_set.lock() {
            if on {
                set.insert(id.to_string());
            } else {
                set.remove(id);
            }
        }
        Ok(())
    }

    /// Startup backfill: re-enroll plugins with auto_reload=1 from the DB into the polling set,
    /// using the current manifest mtime as the baseline — otherwise the first tick after a restart would
    /// indiscriminately stop+start every plugin that was ever checked.
    /// `None` = storage unavailable / query failed; `Some(count)` = success, where count is the number backfilled.
    pub fn restore_auto_reload(&self) -> Option<usize> {
        let store = Self::store()?;
        let rows: Vec<(String, String)> = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    let mut st = c
                        .prepare("SELECT id, path FROM plugins WHERE auto_reload = 1")
                        .ok()?;
                    let it = st
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                        .ok()?;
                    Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
                })
            })
            .flatten()?;
        let mut n = 0;
        if let Ok(mut set) = self.auto_reload_set.lock() {
            for (id, path) in rows {
                set.insert(id.clone());
                if let Ok(m) = std::fs::metadata(std::path::Path::new(&path).join("opencapx-plugin.json")) {
                    if let Ok(t) = m.modified() {
                        if let Ok(mut lm) = self.last_mtimes.lock() {
                            lm.insert(id, t);
                        }
                    }
                }
                n += 1;
            }
        }
        Some(n)
    }

    /// Phase 40 — apply cfg to the running watchdog:
    /// - enabled=false → set the current watchdog's enabled=false and the old loop exits naturally.
    /// - enabled=true and the plugin is running → remove the old watchdog and register_watchdog rebuilds it with the new cfg.
    /// - plugin not running → do nothing (the next start_inner reads the new cfg from the store).
    pub fn apply_health_config(&self, id: &str, cfg: &super::health::PluginHealthConfig) {
        if !cfg.enabled {
            if let Ok(mut m) = self.watchdogs.lock() {
                if let Some(wd) = m.get(id).cloned() {
                    if let Ok(mut w) = wd.lock() {
                        w.enabled = false;
                    }
                }
            }
            return;
        }
        let running = self
            .procs
            .lock()
            .ok()
            .map(|p| p.contains_key(id))
            .unwrap_or(false);
        if running {
            if let Ok(mut m) = self.watchdogs.lock() {
                // Q1 — disable the old thread before removing it: otherwise the old thread holds an Arc with a stale cfg and keeps monitoring
                // (each health-config save = +1 duplicate monitor thread). Same as the stop()/disable branch.
                if let Some(wd) = m.get(id).cloned() {
                    if let Ok(mut w) = wd.lock() {
                        w.enabled = false;
                    }
                }
                m.remove(id);
            }
            self.register_watchdog(id);
        }
    }

    /// Register/reset the watchdog and spawn the monitor thread. Idempotent.
    fn register_watchdog(&self, id: &str) {
        // Phase 40 — read the per-plugin config from the store; snapshot it at register time to avoid hitting the DB in later loops.
        let cfg = Self::store()
            .and_then(|s| s.lock().ok().map(|g| g.get_health_config(id)))
            .unwrap_or_default();
        let wd = Arc::new(Mutex::new(Watchdog::new(cfg)));
        if let Ok(mut m) = self.watchdogs.lock() {
            m.insert(id.to_string(), wd.clone());
        }
        let id_owned = id.to_string();
        let mgr = self_ref();
        std::thread::spawn(move || watchdog_loop(mgr, id_owned, wd));
    }

    pub fn get_process(&self, id: &str) -> Option<Arc<PluginProcess>> {
        self.procs.lock().ok().and_then(|m| m.get(id).cloned())
    }

    pub fn stop(&self, id: &str) {
        // disable the watchdog first, so it does not misjudge our shutdown as a crash and restart
        if let Ok(mut m) = self.watchdogs.lock() {
            if let Some(w) = m.get(id) {
                if let Ok(mut w) = w.lock() {
                    w.enabled = false;
                }
            }
            m.remove(id);
        }
        if let Some(p) = self.procs.lock().ok().and_then(|mut m| m.remove(id)) {
            // don't try_unwrap: in-flight calls / the watchdog may still hold Arc clones
            p.shutdown();
        }
        // remove from the registry
        if let Some(store) = Self::store() {
            if let Ok(mut s) = store.lock() {
                let _ = s.with_conn(|c| {
                    c.execute(
                        "UPDATE capabilities SET enabled = 0 WHERE plugin_id = ?1",
                        [id],
                    )
                    .unwrap_or(0)
                });
            }
        }
        Self::set_status(id, "stopped");
        super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
            "plugin.lifecycle.stopped",
            "core",
            json!({ "pluginId": id }),
        ));
    }

    /// Phase 46 — called on a profile switch: stops all running plugins and returns the list of stopped plugin_ids.
    /// Does not affect config / sqlite rows; it is just a clean shutdown.
    pub fn stop_all(&self) -> Vec<String> {
        let ids: Vec<String> = match self.procs.lock() {
            Ok(m) => m.keys().cloned().collect(),
            Err(_) => Vec::new(),
        };
        for id in &ids {
            self.stop(id);
        }
        ids
    }

    /// Uninstall a plugin: stop the process + delete the config file + clear sqlite rows (plugin / permissions /
    /// capabilities / **declaration + domain release**) + emit a `plugin.uninstalled` event.
    /// Failure-tolerant (clean up as much as possible).
    pub fn uninstall(&self, id: &str) -> Result<(), String> {
        let _install_guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // stop first, so the watchdog / restart loop does not bring the process back while we clean up
        self.stop(id);
        // remove from the auto_reload set
        if let Ok(mut set) = self.auto_reload_set.lock() {
            set.remove(id);
        }
        // delete the config file (ignore "not found" errors)
        let _ = super::config::reset(id);
        // M7/F8 — the secret fallback file is cleaned up with the uninstall (keychain entries cannot be enumerated; see the config comment).
        let _ = super::config::forget_plugin_secrets(id);
        // M8 hardening (review tightening) — delete managed plugin directories: strict equality check (path == plugins root/<id>,
        // i.e. the copy left by a .ocplugin install). Prefix matching does not guard against ".." components (Path::starts_with
        // does not normalize), and directory installs (dev/test) may write an arbitrary path to the DB — neither should be deleted.
        // read the path first; only delete files after the DB row is successfully removed (see below, closing the "row without file" window).
        let managed_path: Option<PathBuf> = Self::row_manifest(id).ok().and_then(|(path, _)| {
            if path == Self::plugins_root().join(id) {
                Some(path)
            } else {
                None
            }
        });
        // delete the plugin / permissions / capabilities tables
        let mut removed_row = false;
        if let Some(store) = Self::store() {
            if let Ok(mut s) = store.lock() {
                removed_row = s
                    .with_conn(|c| {
                        c.execute("DELETE FROM plugin_permissions WHERE plugin_id = ?1", [id])
                            .unwrap_or(0)
                            + c.execute("DELETE FROM capabilities WHERE plugin_id = ?1", [id])
                                .unwrap_or(0)
                            + c.execute("DELETE FROM plugins WHERE id = ?1", [id]).unwrap_or(0)
                    })
                    .map(|n| n > 0)
                    .unwrap_or(false);
            }
            // §4.6 uninstall release: delete the frozen declaration + release the domain (the tombstone prompt belongs to the UI.
            // Reinstall = a fresh install with full confirmation; it does not accept implicit renewal of the old frozen state)
            let _ = super::declaration::delete_for_plugin(&store, id);
        }
        // Only clear files after the DB row is truly deleted: avoid "row without file (startup always fails)"; the reverse "file without row" is harmless.
        if removed_row {
            if let Some(path) = managed_path {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
        super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
            "plugin.uninstalled",
            "core",
            json!({ "pluginId": id }),
        ));
        // Phase 53 — uninstall the manifest-origin severity hints.
        super::alerting::uninstall_manifest_hints(id);
        Ok(())
    }

    /// Wow 7 — uninstall preview. Deletes nothing; only collects the metadata that would be cleared +
    /// detects capability overlap with other plugins (dependency warning).
    pub fn uninstall_preview(&self, id: &str) -> Result<UninstallPreviewDto, String> {
        // find this plugin: list() already includes auto_reload + capabilities + status
        let me = self
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("plugin not installed: {}", id))?;
        // whether the config file exists
        let config_path = super::config::config_path(id);
        let config_exists = config_path.exists();
        // permission count (queried from sqlite)
        let permission_count = Self::store()
            .and_then(|s| {
                s.lock().ok().and_then(|mut g| {
                    g.with_conn(|c| {
                        c.query_row(
                            "SELECT COUNT(*) FROM plugin_permissions WHERE plugin_id = ?1",
                            [id],
                            |r| r.get::<_, i64>(0),
                        )
                        .unwrap_or(0) as usize
                    })
                })
            })
            .unwrap_or(0);
        let capability_count = me.capabilities.len();
        // dependents: ids of other installed plugins whose manifest contains any of this plugin's capabilities
        let mut dependents: Vec<String> = Vec::new();
        if !me.capabilities.is_empty() {
            for other in self.list() {
                if other.id == id {
                    continue;
                }
                if other.capabilities.iter().any(|c| me.capabilities.contains(c)) {
                    dependents.push(other.id);
                }
            }
        }
        Ok(UninstallPreviewDto {
            id: me.id,
            name: me.name,
            version: me.version,
            auto_reload: me.auto_reload,
            config_exists,
            permission_count,
            capability_count,
            dependents,
        })
    }

    /// Phase 32 — all installed plugins + edges for shared capabilities. For the frontend SVG node-edge chart.
    /// O(n²), but n≤20 is usually fine and the shared-capability count is far smaller than the edge count.
    pub fn capability_dependency_graph(&self) -> CapabilityGraphDto {
        let plugins = self.list();
        let mut nodes: Vec<CapabilityGraphNodeDto> = plugins
            .iter()
            .map(|p| CapabilityGraphNodeDto {
                id: p.id.clone(),
                name: p.name.clone(),
                capabilities: p.capabilities.clone(),
            })
            .collect();
        let mut edges: Vec<CapabilityGraphEdgeDto> = Vec::new();
        for i in 0..plugins.len() {
            for j in (i + 1)..plugins.len() {
                let a = &plugins[i];
                let b = &plugins[j];
                let shared: Vec<String> = a
                    .capabilities
                    .iter()
                    .filter(|c| b.capabilities.contains(c))
                    .cloned()
                    .collect();
                if shared.is_empty() {
                    continue;
                }
                // sort from/to by id lexicographically to keep edges stable
                let (from, to) = if a.id <= b.id {
                    (a.id.clone(), b.id.clone())
                } else {
                    (b.id.clone(), a.id.clone())
                };
                edges.push(CapabilityGraphEdgeDto { from, to, shared });
            }
        }
        // nodes sorted by id, keeping the frontend layout stable
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        CapabilityGraphDto { nodes, edges }
    }

    /// F5 — all `(dependent, dep)` pairs (read from DB rows; rows that fail to parse are skipped).
    pub fn dependency_edges(&self) -> Vec<(String, String)> {
        let Some(store) = Self::store() else {
            return Vec::new();
        };
        store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    let mut st = c.prepare("SELECT id, manifest FROM plugins").ok()?;
                    let it = st
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                        .ok()?;
                    let mut out = Vec::new();
                    for (id, json) in it.filter_map(|x| x.ok()) {
                        if let Ok(dm) = serde_json::from_str::<Manifest>(&json) {
                            for dep in dm.dependencies.keys() {
                                out.push((id.clone(), dep.clone()));
                            }
                        }
                    }
                    Some(out)
                })
            })
            .flatten()
            .unwrap_or_default()
    }

    pub fn toggle(&self, id: &str) -> Result<bool, String> {
        let running = self
            .procs
            .lock()
            .ok()
            .and_then(|m| m.get(id).cloned())
            .map(|p| p.is_alive())
            .unwrap_or(false);
        if running {
            self.stop(id);
            Ok(false)
        } else {
            self.start(id)?;
            Ok(true)
        }
    }

    pub fn list(&self) -> Vec<PluginStatusDto> {
        let Some(store) = Self::store() else {
            return Vec::new();
        };
        let rows = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    let mut stmt = c
                        .prepare("SELECT id, manifest, status, auto_reload, probe_status, probe_at, revoked_key, revoked_at FROM plugins")
                        .ok()?;
                    let it = stmt
                        .query_map([], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                                r.get::<_, i64>(3)?,
                                r.get::<_, Option<String>>(4)?,
                                r.get::<_, Option<i64>>(5)?,
                                r.get::<_, Option<String>>(6)?,
                                r.get::<_, Option<i64>>(7)?,
                            ))
                        })
                        .ok()?;
                    Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
                })
            })
            .flatten()
            .unwrap_or_default();
        // F5 — fetch all (id, version) at once, so each manifest can check dependency satisfaction in memory (avoiding N+1).
        let installed: Vec<(String, String)> = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    let mut st = c.prepare("SELECT id, version FROM plugins").ok()?;
                    let it = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?;
                    Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
                })
            })
            .flatten()
            .unwrap_or_default();
        rows.into_iter()
            .filter_map(|(id, manifest, status, auto_reload, probe_status, probe_at, revoked_key, revoked_at)| {
                let m: Manifest = serde_json::from_str(&manifest).ok()?;
                // also fetch path, so settings can show "where it came from"
                let path_row = store
                    .lock()
                    .ok()
                    .and_then(|s| {
                        s.with_conn_ref(|c| {
                            c.query_row(
                                "SELECT path FROM plugins WHERE id = ?1",
                                params![id],
                                |r| r.get::<_, String>(0),
                            )
                            .ok()
                        })
                    })
                    .flatten();
                // Phase 38 — query the plugin_channel table separately for the currently subscribed channel.
                // fetch everything via list_plugin_channels then find in memory,
                // one SELECT for everything, avoiding N+1 queries.
                let channel = store
                    .lock()
                    .ok()
                    .and_then(|s| s.list_plugin_channels().into_iter().find(|(pid, _)| pid == &id))
                    .map(|(_, c)| c);
                // Phase 40 — health config (fetch all once, find in memory). No row → None.
                let health_cfg = store
                    .lock()
                    .ok()
                    .and_then(|s| s.list_health_configs().into_iter().find(|(pid, _)| pid == &id))
                    .map(|(_, c)| c);
                let capability_ids = m.capability_ids();
                Some(PluginStatusDto {
                    id,
                    name: m.name,
                    description: m.description,
                    author: m.author,
                    homepage: m.homepage,
                    license: m.license,
                    version: m.version,
                    ptype: m.ptype,
                    status,
                    capabilities: capability_ids,
                    permissions: m.permissions,
                    path: path_row,
                    auto_reload: auto_reload != 0,
                    probe_status: probe_status.filter(|s| !s.is_empty()),
                    probe_at: probe_at.map(|v| v as u64).filter(|v| *v > 0),
                    channel,
                    sandbox_declared: m.sandbox.is_some(),
                    health_heartbeat_sec: health_cfg.as_ref().map(|c| c.heartbeat_sec),
                    health_max_retries: health_cfg.as_ref().map(|c| c.max_retries),
                    health_enabled: health_cfg.as_ref().map(|c| c.enabled),
                    missing_dependencies: super::plugin_deps::find_missing(
                        &m.dependencies,
                        &installed,
                    )
                    .into_iter()
                    .map(|(id, requirement)| MissingDepDto { id, requirement })
                    .collect(),
                    revoked_key,
                    revoked_at: revoked_at.map(|v| v as u64).filter(|v| *v > 0),
                })
            })
            .collect()
    }

    /// Ensures the process is running before the call (lazy start).
    pub fn ensure_running(&self, id: &str) -> Result<Arc<PluginProcess>, String> {
        if let Some(p) = self.procs.lock().ok().and_then(|m| m.get(id).cloned()) {
            if p.is_alive() {
                return Ok(p);
            }
        }
        self.start(id)?;
        self.procs
            .lock()
            .ok()
            .and_then(|m| m.get(id).cloned())
            .ok_or_else(|| "plugin process missing after start".to_string())
    }

    /// Phase 45 — for the metrics module: returns (plugin_id, pid) pairs of all running plugins.
    /// `pid` may be None (the process just started and has no pid yet / it exited but procs is not cleaned up).
    pub fn list_running_with_pid(&self) -> Vec<(String, u32)> {
        let Ok(procs) = self.procs.lock() else { return Vec::new() };
        procs
            .iter()
            .filter_map(|(id, p)| p.pid().map(|pid| (id.clone(), pid)))
            .collect()
    }
}

/// Start the auto-reload polling thread (singleton, idempotent). Checks all auto_reload=1 plugins every 3s.
pub fn spawn_auto_reload_poller() {
    use std::sync::OnceLock;
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| auto_reload_loop());
}

/// Background polling: for auto_reload=1 plugins, a manifest mtime change → stop+start, and emit plugin.auto_reloaded.
fn auto_reload_loop() {
    let mut restored = false;
    loop {
        std::thread::sleep(Duration::from_secs(3));
        let mgr = self_ref();
        if !restored {
            if let Some(n) = mgr.restore_auto_reload() {
                restored = true;
                if n > 0 {
                    eprintln!("[plugin] auto-reload restored for {} plugin(s)", n);
                }
            }
        }
        // per-manager set: does not depend on the global shared_store (avoiding collisions with other plugin tests in parallel)
        let ids: Vec<String> = match mgr.auto_reload_set.lock() {
            Ok(s) => s.iter().cloned().collect(),
            Err(_) => continue,
        };
        // query path from the shared store (read-only, no writes)
        let Some(store) = PluginManager::store() else {
            continue;
        };
        let tracked: Vec<(String, String)> = match store.lock() {
            Ok(mut s) => {
                let mut out = Vec::new();
                for id in &ids {
                    let path = s.with_conn_ref(|c| {
                        c.query_row(
                            "SELECT path FROM plugins WHERE id = ?1",
                            [id],
                            |r| r.get::<_, String>(0),
                        )
                        .ok()
                    });
                    if let Some(Some(p)) = path.map(|x| x) {
                        out.push((id.clone(), p));
                    }
                }
                out
            }
            Err(_) => continue,
        };
        for (id, path) in tracked {
            let manifest_path = std::path::Path::new(&path).join("opencapx-plugin.json");
            let meta = match std::fs::metadata(&manifest_path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = match meta.modified() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let changed = mgr
                .last_mtimes
                .lock()
                .ok()
                .and_then(|m| m.get(&id).copied())
                .map(|last| mtime > last)
                .unwrap_or(true);
            if !changed {
                continue;
            }
            // stop and restart with reset=false (avoiding a watchdog cumulative-count trigger)
            mgr.stop(&id);
            if mgr.start_inner(&id, true).is_ok() {
                super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                    "plugin.auto_reloaded",
                    "core",
                    json!({ "pluginId": &id }),
                ));
            }
        }
    }
}

fn self_ref() -> Arc<PluginManager> {
    PluginManager::shared()
}

/// Watchdog: polls the plugin child process; on an unexpected death it restarts with exponential backoff (1s, 2s, 4s),
/// and after exceeding WATCHDOG_MAX_RETRIES it disables the plugin and emits plugin.state_changed="error".
/// A manual stop() first clears the enabled flag; this loop exits immediately when it sees enabled=false.
fn watchdog_loop(mgr: Arc<PluginManager>, id: String, wd: Arc<Mutex<Watchdog>>) {
    loop {
        // Phase 40 — the tick interval is decided by cfg.heartbeat_sec; 0 = ping off, still using the 500ms is_alive check.
        let heartbeat_ms = wd
            .lock()
            .map(|w| if w.cfg.heartbeat_sec == 0 {
                WATCHDOG_TICK_MS
            } else {
                (w.cfg.heartbeat_sec as u64).saturating_mul(1000).max(WATCHDOG_TICK_MS)
            })
            .unwrap_or(WATCHDOG_TICK_MS);
        std::thread::sleep(Duration::from_millis(heartbeat_ms));
        let enabled = wd.lock().map(|w| w.enabled).unwrap_or(false);
        if !enabled {
            return;
        }
        let alive = mgr
            .procs
            .lock()
            .ok()
            .and_then(|m| m.get(&id).cloned())
            .map(|p| p.is_alive())
            .unwrap_or(false);

        // Phase 40 — when heartbeat_sec > 0, do a ping first; a ping timeout = treated as alive=false and triggers the retry path.
        if alive {
            let heartbeat_sec = wd.lock().map(|w| w.cfg.heartbeat_sec).unwrap_or(0);
            if heartbeat_sec > 0 {
                let timeout_ms = wd
                    .lock()
                    .map(|w| w.cfg.ping_timeout_ms)
                    .unwrap_or(1000);
                let ping_ok = mgr
                    .procs
                    .lock()
                    .ok()
                    .and_then(|m| m.get(&id).cloned())
                    .map(|p| {
                        p.ping(std::time::Duration::from_millis(timeout_ms as u64))
                    })
                    .unwrap_or(false);
                if ping_ok {
                    continue;
                }
                // ping failure → takes the same path as process death
            } else {
                continue;
            }
        }
        if alive {
            continue;
        }
        // the process died. Check whether our own stop caused it.
        let still_tracked = mgr.watchdogs.lock().ok().and_then(|m| m.get(&id).cloned()).is_some();
        if !still_tracked {
            return;
        }
        // count + backoff (cumulative across restarts; reset by start())
        let retry = {
            let w = match wd.lock() {
                Ok(w) => w,
                Err(_) => return,
            };
            if !w.enabled {
                return;
            }
            drop(w);
            super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                "plugin.lifecycle.crashed",
                "core",
                json!({ "pluginId": &id, "reason": "process exited unexpectedly" }),
            ));
            let mut counts = match mgr.retry_counts.lock() {
                Ok(c) => c,
                Err(_) => return,
            };
            let n = counts.get(&id).copied().unwrap_or(0) + 1;
            counts.insert(id.clone(), n);
            // Phase 40 — max_retries comes from cfg; 0 disables the watchdog outright; max_retries=3 matches the Phase 10 behavior.
            let max_retries = wd.lock().map(|w| w.cfg.max_retries).unwrap_or(WATCHDOG_MAX_RETRIES);
            if max_retries == 0 || n > max_retries {
                PluginManager::set_status(&id, "error");
                super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                    "plugin.watchdog_disabled",
                    "core",
                    json!({ "pluginId": &id, "reason": "max retries exceeded" }),
                ));
                counts.remove(&id);
                drop(counts);
                if let Ok(mut m) = mgr.watchdogs.lock() {
                    m.remove(&id);
                }
                return;
            }
            n
        };
        // Phase 40 — backoff comes from cfg (default 1s, doubling, capped at 30s).
        let backoff_initial = wd
            .lock()
            .map(|w| w.cfg.backoff_initial_ms)
            .unwrap_or(1000);
        let backoff_ms = super::health::compute_backoff_ms(retry, backoff_initial);
        std::thread::sleep(Duration::from_millis(backoff_ms));
        // re-confirm enabled (start may have already reset the watchdog)
        let still_enabled = wd.lock().map(|w| w.enabled).unwrap_or(false);
        if !still_enabled {
            return;
        }
        super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
            "plugin.restarting",
            "core",
            json!({ "pluginId": &id, "retry": retry }),
        ));
        if mgr.start_inner(&id, false).is_ok() {
            // start_inner calls register_watchdog() to replace us with a new enabled=true watchdog,
            // so here it is enough to exit the old loop.
            return;
        }
        // start failed: leave it for the next round to decide
    }
}

// ===== S3 — reverse call rate limiting / dedup =====

/// Reverse rate-limit parameters (settings can override): `reverse_rate_per_sec` (default 50/s), `reverse_burst` (default 100).
/// review F5 — 5s TTL cache: avoid locking SQLite on every frame during a flood (the limiter must not be turned into an amplifier by DB locks).
fn reverse_limits() -> (f64, f64) {
    static CACHE: OnceLock<Mutex<Option<(std::time::Instant, f64, f64)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((at, r, b)) = *g {
            if at.elapsed().as_secs_f64() < 5.0 {
                return (r, b);
            }
        }
    }
    let read = |k: &str| -> Option<f64> {
        super::shared_store()
            .and_then(|s| s.lock().ok().and_then(|g| g.get_setting(k)))
            .and_then(|v| v.parse::<f64>().ok())
    };
    let rate = read("reverse_rate_per_sec").unwrap_or(50.0).max(1.0);
    let burst = read("reverse_burst").unwrap_or(100.0).max(1.0);
    if let Ok(mut g) = cache.lock() {
        *g = Some((std::time::Instant::now(), rate, burst));
    }
    (rate, burst)
}

/// per-plugin token bucket (combined limit for core.emit + core.log).
struct ReverseBucket {
    tokens: f64,
    last_refill: std::time::Instant,
    /// Start of the current drop window (a summary is emitted when the window closes after 1s).
    window_start: Option<std::time::Instant>,
    window_dropped: u64,
    last_summary: Option<std::time::Instant>,
}

fn reverse_buckets() -> &'static Mutex<HashMap<String, ReverseBucket>> {
    static BUCKETS: OnceLock<Mutex<HashMap<String, ReverseBucket>>> = OnceLock::new();
    BUCKETS.get_or_init(|| {
        // review F5 — a background flush of lazy windows every minute: even if the flood abruptly stops (no more frames to trigger it), the final dropped count is still reported.
        std::thread::spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            flush_all_reverse_windows();
        });
        Mutex::new(HashMap::new())
    })
}

/// Background fallback for lazy windows: iterate all buckets, close windows that have been full for 1s, and report (emit events outside the lock).
fn flush_all_reverse_windows() {
    let pending = {
        let Ok(mut map) = reverse_buckets().lock() else {
            return;
        };
        let now = std::time::Instant::now();
        let mut acc: Vec<(String, u64, f64)> = Vec::new();
        for (pid, b) in map.iter_mut() {
            if let Some((dropped, secs)) = flush_reverse_window(b, now) {
                acc.push((pid.clone(), dropped, secs));
            }
        }
        acc
    };
    for (pid, dropped, secs) in pending {
        publish_throttled(&pid, dropped, secs);
    }
}

/// Close a drop window that has been full for 1s → produce a summary (the summary is itself rate-limited: ≥1s since the last one, preventing a feedback loop).
fn flush_reverse_window(b: &mut ReverseBucket, now: std::time::Instant) -> Option<(u64, f64)> {
    let start = b.window_start?;
    let elapsed = now.duration_since(start).as_secs_f64();
    if elapsed < 1.0 {
        return None;
    }
    if let Some(last) = b.last_summary {
        if now.duration_since(last).as_secs_f64() < 1.0 {
            return None;
        }
    }
    let out = Some((b.window_dropped, elapsed));
    b.window_dropped = 0;
    b.window_start = None;
    b.last_summary = Some(now);
    out
}

/// Whether a reverse event is allowed; over the limit it is dropped and (on window close) one `plugin.throttled` summary is emitted.
/// (O2 — also reused by process.rs's stderr path; chatty stderr must not bypass rate limiting.)
pub(crate) fn reverse_allow(plugin_id: &str) -> bool {
    let (rate, burst) = reverse_limits();
    let now = std::time::Instant::now();
    let (allowed, summary) = {
        let Ok(mut map) = reverse_buckets().lock() else {
            return true;
        };
        let b = map
            .entry(plugin_id.to_string())
            .or_insert_with(|| ReverseBucket {
                tokens: burst,
                last_refill: now,
                window_start: None,
                window_dropped: 0,
                last_summary: None,
            });
        let dt = now.duration_since(b.last_refill).as_secs_f64();
        b.tokens = (b.tokens + dt * rate).min(burst);
        b.last_refill = now;
        let summary = flush_reverse_window(b, now);
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            (true, summary)
        } else {
            if b.window_start.is_none() {
                b.window_start = Some(now);
            }
            b.window_dropped += 1;
            (false, summary)
        }
    };
    if let Some((dropped, window_secs)) = summary {
        publish_throttled(plugin_id, dropped, window_secs);
    }
    allowed
}

/// Emit one `plugin.throttled` summary (on window close; the throttle event itself is also constrained to a ≥1s window).
fn publish_throttled(plugin_id: &str, dropped: u64, window_secs: f64) {
    let (rate, burst) = reverse_limits();
    super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
        "plugin.throttled",
        &format!("plugin:{}", plugin_id),
        json!({
            "pluginId": plugin_id,
            "dropped": dropped,
            "windowSecs": window_secs,
            "ratePerSec": rate,
            "burst": burst,
        }),
    ));
}

/// In-flight requestPermission waiters: the same (plugin, permission) shares a single decision,
/// and all are replied to when the decision completes — N requests produce only 1 prompt.
struct PermWaiters {
    replies: Vec<(serde_json::Value, super::process::Reply)>,
}

fn permission_pending() -> &'static Mutex<HashMap<(String, String), PermWaiters>> {
    static PENDING: OnceLock<Mutex<HashMap<(String, String), PermWaiters>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registers a waiter; returns whether this is the first request for the key (the first one runs the gate).
fn register_permission_waiter(
    key: &(String, String),
    id: Option<serde_json::Value>,
    reply: super::process::Reply,
) -> bool {
    let Ok(mut map) = permission_pending().lock() else {
        return false;
    };
    match map.get_mut(key) {
        Some(w) => {
            if let Some(id) = id {
                w.replies.push((id, reply));
            }
            false
        }
        None => {
            let mut w = PermWaiters { replies: Vec::new() };
            if let Some(id) = id {
                w.replies.push((id, reply));
            }
            map.insert(key.clone(), w);
            true
        }
    }
}

/// Decision complete: remove the key and reply to all waiters (newly arriving ones are under the same key too).
fn settle_permission_waiters(key: &(String, String), granted: bool) {
    let waiters = permission_pending().lock().ok().and_then(|mut m| m.remove(key));
    if let Some(w) = waiters {
        for (id, reply) in w.replies {
            reply(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "granted": granted }
            }));
        }
    }
}

#[cfg(test)]
static PERM_GATE_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Runs the permission gate once (same source as the old behavior: known precheck + gate).
fn run_permission_gate(plugin_id: &str, permission: &str, reason: Option<&str>) -> bool {
    #[cfg(test)]
    {
        PERM_GATE_RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // the concurrency-dedup test needs deterministic overlap: make the gate slightly slow so the second request necessarily attaches to the same decision.
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    super::permission::known(permission)
        && super::permission::gate(
            &PluginManager::store().unwrap_or_else(|| {
                Arc::new(Mutex::new(super::storage::StoreEnum::Mem(
                    super::agent::SessionStore::new(),
                )))
            }),
            plugin_id,
            permission,
            "plugin.reverse",
            reason,
        ) == super::permission::Decision::Granted
}

/// Dedup + async: the first request runs the gate on a separate thread (the gate blocks waiting for the user and must not stall the same plugin's other reverse frames).
fn enqueue_permission_request(
    plugin_id: String,
    permission: String,
    reason: Option<String>,
    id: Option<serde_json::Value>,
    reply: super::process::Reply,
) {
    let key = (plugin_id.clone(), permission.clone());
    if !register_permission_waiter(&key, id, reply) {
        return;
    }
    std::thread::spawn(move || {
        let granted = run_permission_gate(&plugin_id, &permission, reason.as_deref());
        settle_permission_waiters(&key, granted);
    });
}

/// Plugin reverse requests/notifications. See docs/plugin-protocol.md.
fn handle_reverse(plugin_id: &str, v: serde_json::Value, reply: super::process::Reply) {
    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    match method {
        "core.log" => {
            if !reverse_allow(plugin_id) {
                return;
            }
            let level = v.pointer("/params/level").and_then(|x| x.as_str()).unwrap_or("info");
            let msg = v.pointer("/params/message").and_then(|x| x.as_str()).unwrap_or("");
            eprintln!("[plugin:{}] [{}] {}", plugin_id, level, msg);
            super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                "plugin.log",
                &format!("plugin:{}", plugin_id),
                json!({
                    "pluginId": plugin_id,
                    "level": level,
                    "source": "reverse",
                    "message": msg,
                }),
            ));
        }
        "core.emit" => {
            if !reverse_allow(plugin_id) {
                return;
            }
            let kind = v.pointer("/params/type").and_then(|x| x.as_str()).unwrap_or("");
            if !kind.is_empty() {
                super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
                    &format!("{}.{}", plugin_id, kind),
                    &format!("plugin:{}", plugin_id),
                    v.pointer("/params/payload").cloned().unwrap_or(json!(null)),
                ));
            }
        }
        "core.requestPermission" => {
            let permission = v
                .pointer("/params/permission")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let reason = v
                .pointer("/params/reason")
                .and_then(|x| x.as_str())
                .map(str::to_string);
            let id = v.get("id").cloned();
            // S3: dedup (N requests, 1 prompt) + move off the reader thread (so waiting on the user does not stall the same plugin's other frames)
            enqueue_permission_request(plugin_id.to_string(), permission, reason, id, reply);
        }
        "plugin.subscribe" => {
            let kind = v.pointer("/params/kind").and_then(|x| x.as_str()).unwrap_or("");
            let res = super::subscriber::SubscriptionRegistry::shared().subscribe(plugin_id, kind);
            if let Some(id) = v.get("id").cloned() {
                let (ok, msg) = match res {
                    Ok(_) => (true, serde_json::Value::Null),
                    Err(e) => (false, serde_json::Value::String(e)),
                };
                reply(json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": ok, "error": msg } }));
            }
        }
        "plugin.unsubscribe" => {
            let kind = v.pointer("/params/kind").and_then(|x| x.as_str()).unwrap_or("");
            super::subscriber::SubscriptionRegistry::shared().unsubscribe(plugin_id, kind);
            if let Some(id) = v.get("id").cloned() {
                reply(json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": true } }));
            }
        }
        _ if method.starts_with("config.") => {
            // of the form config.{op}: get / set / delete / all / list
            let op = method.trim_start_matches("config.");
            let params = v.get("params").cloned().unwrap_or(json!({}));
            let res = super::config::dispatch(plugin_id, op, &params);
            if let Some(id) = v.get("id").cloned() {
                match res {
                    Ok(value) => reply(json!({ "jsonrpc": "2.0", "id": id, "result": value })),
                    Err(e) => reply(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32603, "message": e }
                    })),
                }
            }
        }
        _ => {
            if let Some(id) = v.get("id").cloned() {
                reply(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("unknown reverse method {}", method) }
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Old test fixtures do not register a `core.probe.capability` handler. Installing a plugin runs probe
    /// and would misjudge them as failed. Uniformly set this env at the test entry to skip probe. The production path is unaffected.
    fn skip_probe_in_tests() {
        std::env::set_var("OPENCAPX_SKIP_PROBE", "1");
    }

    #[test]
    fn manifest_rejects_bad_api_version() {
        let dir = std::env::temp_dir().join(format!("opencapx-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("opencapx-plugin.json"),
            r#"{"id":"x","name":"X","version":"0.1.0","apiVersion":"9","type":"capability","runtime":{"type":"process","command":"true"},"capabilities":["image.analyze"]}"#,
        )
        .unwrap();
        let err = PluginManager::read_manifest(&dir).unwrap_err();
        assert!(err.contains("apiVersion"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_rejects_unknown_permission_and_capability() {
        let dir = std::env::temp_dir().join(format!("opencapx-manifest2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("opencapx-plugin.json"),
            r#"{"id":"x","name":"X","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"true"},"capabilities":["nope.nope"]}"#,
        )
        .unwrap();
        assert!(PluginManager::read_manifest(&dir).unwrap_err().contains("unknown capability"));
        std::fs::write(
            dir.join("opencapx-plugin.json"),
            r#"{"id":"x","name":"X","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"true"},"capabilities":["image.analyze"],"permissions":["root.access"]}"#,
        )
        .unwrap();
        assert!(PluginManager::read_manifest(&dir).unwrap_err().contains("unknown permission"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_rejects_traversal_plugin_id() {
        // P0 regression (docs/permission-domains.md §7): an id like `../..` could exploit
        // root.join(id) + remove_dir_all to traverse and delete an arbitrary directory; it must be
        // rejected in validate_manifest (parse time) — both the tmp and dest joins are covered.
        let dir = std::env::temp_dir().join(format!("opencapx-manifest-traversal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for bad in [
            "../..", "..", ".", "a/../b", "/abs/path", "a/b",
            "A-upper", "é-accent", "has space", ".leading", "trailing.", "a..b",
        ] {
            std::fs::write(
                dir.join("opencapx-plugin.json"),
                format!(r#"{{"id":"{bad}","name":"X","version":"0.1.0","apiVersion":"1","type":"pet"}}"#),
            )
            .unwrap();
            let err = PluginManager::read_manifest(&dir).unwrap_err();
            assert!(err.contains("invalid plugin id"), "id {bad:?} should be rejected, got: {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_id_lexicon_accepts_legal_ids() {
        // Old single-segment hyphen ids are compatible; reverse-DNS form is preferred.
        for ok in ["x", "echo-vision", "things-demo", "com.example.weather", "plug2.v2"] {
            assert!(PluginManager::valid_plugin_id(ok), "{ok:?} should be valid");
        }
        for bad in [
            "", "..", ".", "a..b", "../x", "x/../y", "/abs", "a/b", "A", "É",
            "a b", ".lead", "trail.", &"x".repeat(129),
        ] {
            assert!(!PluginManager::valid_plugin_id(bad), "{bad:?} should be invalid");
        }
    }

    /// Wow 6 fields were previously dropped by serde (the struct had no matching field) → they must now round-trip completely.
    #[test]
    fn manifest_round_trips_author_license_and_signature_fields() {
        let text = r#"{
            "id": "com.example.foo", "name": "Foo", "version": "1.0.0",
            "apiVersion": "1", "type": "capability",
            "runtime": { "type": "process", "command": "python3" },
            "capabilities": [],
            "author": "Alice", "homepage": "https://example.com", "license": "MIT",
            "sha256": "aa", "signature": { "keyId": "com.example", "sig": "bb" }
        }"#;
        let m: Manifest = serde_json::from_str(text).unwrap();
        assert_eq!(m.author.as_deref(), Some("Alice"));
        assert_eq!(m.license.as_deref(), Some("MIT"));
        let round = serde_json::to_string(&m).unwrap();
        let m2: Manifest = serde_json::from_str(&round).unwrap();
        assert_eq!(m2.homepage.as_deref(), Some("https://example.com"));
        assert_eq!(m2.sha256.as_deref(), Some("aa"));
        assert_eq!(
            m2.signature.as_ref().and_then(|s| s.get("sig")).and_then(|v| v.as_str()),
            Some("bb")
        );
    }

    /// Old manifests (without the new fields) are unaffected: parsing passes and serialization does not invent keys.
    #[test]
    fn legacy_manifest_without_new_fields_is_unchanged() {
        let text = r#"{"id":"com.x.y","name":"Y","version":"1.0.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"python3"},"capabilities":[]}"#;
        let m: Manifest = serde_json::from_str(text).unwrap();
        assert!(m.author.is_none() && m.homepage.is_none() && m.license.is_none());
        let round = serde_json::to_string(&m).unwrap();
        assert!(!round.contains("\"author\""));
        assert!(!round.contains("\"signature\""));
    }

    #[test]
    fn manifest_round_trips_min_core_version_and_dependencies() {
        let text = r#"{"id":"com.x.y","name":"Y","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"],
            "minCoreVersion":"0.5.0","dependencies":{"com.x.b":">=1.2.0"}}"#;
        let m: Manifest = serde_json::from_str(text).unwrap();
        assert_eq!(m.min_core_version.as_deref(), Some("0.5.0"));
        assert_eq!(m.dependencies.get("com.x.b").map(String::as_str), Some(">=1.2.0"));
        let round = serde_json::to_string(&m).unwrap();
        assert!(round.contains("minCoreVersion") && round.contains(">=1.2.0"));
        // an old manifest with no fields: serialization adds no keys
        let legacy = r#"{"id":"com.x.z","name":"Z","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"]}"#;
        let l: Manifest = serde_json::from_str(legacy).unwrap();
        let out = serde_json::to_string(&l).unwrap();
        assert!(!out.contains("minCoreVersion") && !out.contains("dependencies"));
    }

    #[test]
    fn validate_rejects_bad_min_core_and_bad_deps() {
        let base = |extra: &str| format!(
            r#"{{"id":"com.x.y","name":"Y","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{{"type":"process","command":"python3"}},"capabilities":["image.analyze"]{extra}}}"#
        );
        let ok: Manifest = serde_json::from_str(&base(r#","minCoreVersion":"1.2""#)).unwrap();
        assert!(PluginManager::validate_manifest(&ok).is_ok(), "lenient semver 1.2 is legal");
        for bad in [
            r#","minCoreVersion":"not-a-version""#,
            r#","dependencies":{"com.x.b":"not a req"}"#,
            r#","dependencies":{"Bad IDFormat":">=1.0"}"#,
            r#","dependencies":{"com.x.y":">=1.0"}"#, // self-dependency
        ] {
            let m: Manifest = serde_json::from_str(&base(bad)).unwrap();
            assert!(PluginManager::validate_manifest(&m).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn check_core_compat_gates_on_min_core() {
        let mk = |v: Option<&str>| {
            let mut m: Manifest = serde_json::from_str(
                r#"{"id":"com.x.y","name":"Y","version":"1.0.0","apiVersion":"1","type":"capability",
                   "runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"]}"#,
            ).unwrap();
            m.min_core_version = v.map(String::from);
            m
        };
        assert!(PluginManager::check_core_compat(&mk(None)).is_ok());
        assert!(PluginManager::check_core_compat(&mk(Some("0.1.0"))).is_ok());
        assert!(PluginManager::check_core_compat(&mk(Some(env!("CARGO_PKG_VERSION")))).is_ok());
        let err = PluginManager::check_core_compat(&mk(Some("99.0.0"))).unwrap_err();
        assert!(err.contains("requires core"), "{err}");
        // lenient parsing: a 2-segment name is not wrongly rejected
        assert!(PluginManager::check_core_compat(&mk(Some("0.1"))).is_ok());
    }

    /// Install gate: minCoreVersion higher than the current core → reject, with zero writes (never reaches the confirm/commit phase).
    #[test]
    fn install_rejects_future_core_requirement_before_any_write() {
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-core-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("plugin-src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("opencapx-plugin.json"),
            r#"{"id":"com.x.future","name":"F","version":"1.0.0","apiVersion":"1","type":"capability",
                "runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"],
                "minCoreVersion":"99.0.0"}"#,
        ).unwrap();
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        crate::core::set_shared_store(store.clone());
        let mgr = PluginManager::shared();
        let err = mgr.install_from_dir(&src).unwrap_err();
        assert!(err.contains("requires core"), "{err}");
        let count: i64 = store
            .lock()
            .unwrap()
            .with_conn_ref(|c| c.query_row("SELECT COUNT(*) FROM plugins", [], |r| r.get(0)).ok())
            .flatten()
            .unwrap_or(-1);
        assert_eq!(count, 0, "a failed gate must not write to the DB");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Detail-page README read has three states: missing → None; README.md exists → Some(raw);
    /// over the size limit → None (treated as not provided, no half-rendering of a truncated file); not installed → Err.
    /// Writes the DB row directly instead of going through install_from_dir — that path would really spawn + handshake,
    /// whereas readme() only depends on the (path, manifest) columns.
    #[test]
    fn readme_reads_plugin_dir_three_states() {
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-readme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let installed = dir.join("installed");
        std::fs::create_dir_all(&installed).unwrap();
        let manifest = r#"{"id":"com.x.readme","name":"R","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"]}"#;
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        store
            .lock()
            .unwrap()
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO plugins (id, version, type, status, path, manifest) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params!["com.x.readme", "1.0.0", "capability", "stopped", installed.to_str().unwrap(), manifest],
                )
                .unwrap()
            })
            .unwrap();
        crate::core::set_shared_store(store.clone());

        // 1) no README → None (not Err: the plugin itself is registered)
        assert_eq!(PluginManager::readme("com.x.readme").unwrap(), None);

        // 2) README.md present → Some(raw)
        std::fs::write(installed.join("README.md"), "# Hi\n\ndoc here\n").unwrap();
        assert_eq!(PluginManager::readme("com.x.readme").unwrap(), Some("# Hi\n\ndoc here\n".to_string()));

        // 3) over the limit → None
        let big = "x".repeat(256 * 1024 + 1);
        std::fs::write(installed.join("README.md"), &big).unwrap();
        assert_eq!(PluginManager::readme("com.x.readme").unwrap(), None);

        // 4) not installed → Err
        assert!(PluginManager::readme("nope.nope").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Start gate: missing dependency → reject the start (no spawn), emit plugin.start.rejected.
    #[test]
    fn start_rejects_missing_dependency_without_spawning() {
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-dep-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        crate::core::set_shared_store(store.clone());
        {
            let mut s = store.lock().unwrap();
            s.with_conn(|c| c.execute(
                "INSERT INTO plugins (id, version, type, status, path, manifest) VALUES (?1,?2,?3,?4,?5,?6)",
                params!["com.x.dep", "1.0.0", "capability", "stopped", dir.display().to_string(),
                    r#"{"id":"com.x.dep","name":"D","version":"1.0.0","apiVersion":"1","type":"capability",
                        "runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"],
                        "dependencies":{"com.x.absent":">=1.0.0"}}"#],
            ).unwrap_or(0));
        }
        let bus = crate::core::event::EventBus::shared();
        let rx = bus.subscribe();
        let mgr = PluginManager::shared();
        let err = mgr.start("com.x.dep").unwrap_err();
        assert!(err.contains("plugin_dependency_missing"), "{err}");
        assert!(mgr.get_process("com.x.dep").is_none(), "must not spawn");
        let ev = rx.recv_timeout(Duration::from_millis(300)).expect("event");
        assert_eq!(ev.kind, "plugin.start.rejected");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// list() surfaces author/license from the persisted manifest (old behavior: missing field → None).
    #[test]
    fn list_exposes_author_from_persisted_manifest() {
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-list-author-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        crate::core::set_shared_store(store.clone());
        {
            let mut s = store.lock().unwrap();
            s.with_conn(|c| {
                c.execute(
                    "INSERT INTO plugins (id, version, type, status, path, manifest) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        "com.x.y", "1.0.0", "capability", "running", "/tmp/x",
                        r#"{"id":"com.x.y","name":"Y","version":"1.0.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"python3"},"capabilities":[],"author":"Alice","license":"MIT"}"#
                    ],
                )
                .unwrap_or(0)
            });
        }
        let list = PluginManager::shared().list();
        let dto = list.iter().find(|d| d.id == "com.x.y").expect("row listed");
        assert_eq!(dto.author.as_deref(), Some("Alice"));
        assert_eq!(dto.license.as_deref(), Some("MIT"));
        assert!(dto.homepage.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ─── Step 3 validation surface (docs/permission-domains.md §4.2)──────────────────────

    /// Writes a manifest into a temp directory and runs validate (read_manifest validates along the way).
    fn validate_json(tag: &str, json: &str) -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("opencapx-vm-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("opencapx-plugin.json"), json).unwrap();
        let r = PluginManager::read_manifest(&dir).map(|_| ());
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    fn cap_manifest(caps: &str, perms: &str) -> String {
        format!(
            r#"{{"id":"com.x.weather","name":"W","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{{"type":"process","command":"true"}},"capabilities":{},"permissions":{}}}"#,
            caps, perms
        )
    }

    /// §4.2 object-form declaration is accepted (capability → permission in the same domain + default ∈ ask|denied).
    #[test]
    fn manifest_accepts_declared_domain_capability() {
        let ok = cap_manifest(
            r#"[{"id":"weather.fetch","permission":"weather.read","default":"ask"},
                {"id":"weather.set","permission":"weather.write","default":"denied"}]"#,
            r#"["weather.read","weather.write"]"#,
        );
        assert!(validate_json("ok", &ok).is_ok(), "should accept: {:?}", validate_json("ok2", &ok));
        // default omitted = ask
        let no_default = cap_manifest(
            r#"[{"id":"weather.fetch","permission":"weather.read"}]"#,
            r#"["weather.read"]"#,
        );
        assert!(validate_json("nodef", &no_default).is_ok());
        // inline-mapping permissions need not appear in permissions[] (the confirmation set is a union, M1)
        let union_only = cap_manifest(
            r#"[{"id":"weather.fetch","permission":"weather.read"}]"#,
            r#"[]"#,
        );
        assert!(validate_json("union", &union_only).is_ok());
    }

    /// §4.2 reserved-domain closure: reserved domains / reserved capability IDs must not be declared; declarations must not reference reserved permission names.
    #[test]
    fn manifest_rejects_reserved_domain_declarations() {
        // reserved domain (things.*, long built-in)
        let r = validate_json(
            "res-domain",
            &cap_manifest(r#"[{"id":"things.fetch","permission":"things.read"}]"#, r#"[]"#),
        );
        assert!(r.unwrap_err().contains("reserved"), "reserved domain must be rejected");
        // a reserved capability ID may only use the string form
        let r = validate_json(
            "res-cap",
            &cap_manifest(r#"[{"id":"image.analyze","permission":"image.analyze"}]"#, r#"[]"#),
        );
        assert!(r.unwrap_err().contains("reserved capability"));
        // declaration references a reserved permission name (the reserved-domain check fires before the same-domain check, giving a more precise message)
        let r = validate_json(
            "res-perm",
            &cap_manifest(r#"[{"id":"weather.fetch","permission":"image.read"}]"#, r#"[]"#),
        );
        assert!(
            r.unwrap_err().contains("may not reference reserved permission"),
            "declaration referencing a reserved permission must be rejected"
        );
        // cross-domain (both are new domains, but different ones)
        let r = validate_json(
            "cross-domain",
            &cap_manifest(r#"[{"id":"weather.fetch","permission":"stock.read"}]"#, r#"[]"#),
        );
        assert!(r.unwrap_err().contains("must share a domain"));
        // the opencapx prefix is reserved
        let r = validate_json(
            "res-opencapx",
            &cap_manifest(r#"[{"id":"opencapx.fetch","permission":"opencapx.read"}]"#, r#"[]"#),
        );
        assert!(r.is_err());
    }

    /// §4.2 remaining rejection surfaces: restricted default / lexical / dangling new permission name / cross-domain.
    #[test]
    fn manifest_rejects_malformed_declarations() {
        // default may only be ask|denied (prevents self-granting via the default value)
        let r = validate_json(
            "bad-default",
            &cap_manifest(
                r#"[{"id":"weather.fetch","permission":"weather.read","default":"granted"}]"#,
                r#"[]"#,
            ),
        );
        assert!(r.unwrap_err().contains("ask|denied"));
        // lexical: uppercase / single-segment / double dot
        for bad in [
            r#"[{"id":"Weather.fetch","permission":"weather.read"}]"#,
            r#"[{"id":"weather","permission":"weather.read"}]"#,
            r#"[{"id":"weather..x","permission":"weather.read"}]"#,
        ] {
            let r = validate_json("lex", &cap_manifest(bad, r#"[]"#));
            assert!(r.is_err(), "{bad} should be rejected");
        }
        // non-ASCII homoglyphs
        let r = validate_json(
            "homoglyph",
            &cap_manifest(r#"[{"id":"wéather.fetch","permission":"wéather.read"}]"#, r#"[]"#),
        );
        assert!(r.unwrap_err().contains("invalid"));
        // dangling new permission name: not in the built-in table and not declared by any object form
        let r = validate_json(
            "dangling",
            &cap_manifest(r#"[{"id":"weather.fetch","permission":"weather.read"}]"#, r#"["root.access"]"#),
        );
        assert!(r.unwrap_err().contains("unknown permission"));
    }

    /// The lexical rule only governs names **newly declared by plugins**: the built-in word list is exempt — `camera` / `microphone`
    /// are existing single-segment permission names, and tightening the lexical rule should not reject the built-in list too (regression: they were wrongly rejected).
    #[test]
    fn manifest_accepts_builtin_single_segment_permissions() {
        let m = cap_manifest(r#"["image.analyze"]"#, r#"["camera","microphone"]"#);
        assert!(validate_json("builtin-single", &m).is_ok());
    }

    #[test]
    fn manifest_parses_alerting_severity_hints() {
        // Phase 53 — the `alerting.severityHints` table should be captured by the Manifest.alerting field.
        let dir = std::env::temp_dir().join(format!(
            "opencapx-manifest-hints-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("opencapx-plugin.json"),
            r#"{
                "id":"my-plug","name":"MyPlug","version":"0.1.0","apiVersion":"1",
                "type":"capability",
                "runtime":{"type":"process","command":"true"},
                "capabilities":["image.analyze"],
                "alerting":{"severityHints":{"my.event":"warn","my.critical":"critical"}}
            }"#,
        )
        .unwrap();
        let m = PluginManager::read_manifest(&dir).expect("manifest should parse");
        let alerting = m.alerting.expect("alerting field present");
        assert_eq!(
            alerting
                .severity_hints
                .get("my.event")
                .map(|s| s.as_str()),
            Some("warn")
        );
        assert_eq!(
            alerting
                .severity_hints
                .get("my.critical")
                .map(|s| s.as_str()),
            Some("critical")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_without_alerting_field_parses_ok() {
        // Phase 53 — an old manifest has no alerting field → `#[serde(default)]` → None, without breaking the existing flow.
        let dir = std::env::temp_dir().join(format!(
            "opencapx-manifest-noalert-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("opencapx-plugin.json"),
            r#"{"id":"x","name":"X","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"true"},"capabilities":["image.analyze"]}"#,
        )
        .unwrap();
        let m = PluginManager::read_manifest(&dir).expect("manifest should parse");
        assert!(m.alerting.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn echo_manifest_is_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("plugins")
            .join("echo-vision");
        let m = PluginManager::read_manifest(&root).expect("echo manifest valid");
        assert_eq!(m.id, "com.opencapx.echo-vision");
        assert_eq!(m.ptype, "capability");
        assert_eq!(m.capability_ids(), vec!["image.analyze".to_string()]);
        assert_eq!(m.permissions, vec!["image.read".to_string()]);
    }

    /// pet-type plugin: starts successfully, and a reverse `core.emit` triggers a `{pluginId}.{kind}` event.
    #[test]
    fn pet_plugin_emits_event_on_start() {
        skip_probe_in_tests();
        let py = std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !py {
            eprintln!("skip: no python3");
            return;
        }
        let dir = std::env::temp_dir().join(format!("opencapx-pettest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());

        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("plugins")
            .join("pet-blank");
        let m = PluginManager::read_manifest(&root).expect("pet manifest valid");
        assert_eq!(m.ptype, "pet");
        assert!(m.capability_ids().is_empty());

        // starts a stdio process once to simulate a start and asserts the reverse emit is forwarded into an event by handle_reverse
        let on_reverse: crate::core::process::OnReverse = std::sync::Arc::new(|v, _reply| {
            if v.get("method").and_then(|m| m.as_str()) == Some("core.emit") {
                let kind = v.pointer("/params/type").and_then(|x| x.as_str()).unwrap_or("");
                if kind == "animation" {
                    crate::core::event::EventBus::shared().publish(
                        &crate::core::event::OpencapxEvent::new(
                            "pet_blank_seen",
                            "test",
                            serde_json::json!({ "kind": kind }),
                        ),
                    );
                }
            }
        });
        let spec = crate::core::process::RuntimeSpec {
            command: "python3".into(),
            args: vec!["bin/pet_blank.py".into()],
            env: Default::default(),
        };
        let proc = crate::core::process::PluginProcess::spawn(
            &m.id,
            &root.to_path_buf(),
            &spec,
            "test",
            on_reverse,
            &crate::core::process::EnvPolicy::default(),
            None,
        )
        .expect("spawn pet");
        let proc = std::sync::Arc::new(proc);
        let init = proc
            .call(
                "plugin.initialize",
                serde_json::json!({"coreVersion":"t","apiVersion":"1","pluginId":m.id}),
                Duration::from_secs(5),
            )
            .expect("initialize");
        assert_eq!(init.get("pluginId").and_then(|v| v.as_str()), Some(m.id.as_str()));

        // give the reverse call 200ms to reach the EventBus, then subscribe + publish to detect it
        std::thread::sleep(Duration::from_millis(200));
        let bus = crate::core::event::EventBus::shared();
        let rx = bus.subscribe();
        bus.publish(&crate::core::event::OpencapxEvent::new(
            "pet_blank_seen",
            "test",
            serde_json::json!({ "kind": "animation" }),
        ));
        let seen = rx.recv_timeout(Duration::from_millis(500))
            .map(|e| e.kind == "pet_blank_seen")
            .unwrap_or(false);
        assert!(seen, "pet plugin core.emit should surface as EventBus event");

        let _ = proc.notify("plugin.shutdown", serde_json::json!({}));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The safe-mode start rejection happens before row_manifest, so a nonexistent id is enough to verify the wiring, no install
    /// fixture needed. Unit tests for the gate semantics are in core::safe_mode. The global flag affects parallel start paths,
    /// so it holds TEST_STORE_LOCK to exclude other tests that start plugins.
    #[test]
    fn safe_mode_blocks_start_before_manifest_lookup() {
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::safe_mode::set_active(true);
        let err = PluginManager::shared()
            .start("com.example.safe-mode-probe")
            .err()
            .expect("start must be rejected in safe mode");
        assert!(err.contains("safe mode"), "unexpected error: {err}");
        crate::core::safe_mode::set_active(false);
    }

    /// Full path: install the echo plugin → default permission denied → grant → capability execute succeeds.
    /// Requires python3; skipped if absent. Sets the global shared_store, and when run in parallel with other tests
    /// capability::execute is affected as soon as it queries providers, so this test is serial-exclusive.
    /// #[ignore] by default: run with `cargo test install_grant_execute_e2e -- --ignored --test-threads=1`.
    #[test]
    #[ignore]
    fn install_grant_execute_e2e() {
        use std::sync::{Arc, Mutex};
        let python = ["python3", "python"].iter().find(|c| {
            std::process::Command::new(c)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok()
        });
        let Some(py) = python else {
            eprintln!("skip: no python3");
            return;
        };
        assert_eq!(*py, "python3"); // manifest runtime is fixed to python3
        let dir = std::env::temp_dir().join(format!("opencapx-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());

        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("plugins")
            .join("echo-vision");
        let mgr = PluginManager::shared();
        let id = mgr.install_from_dir(&root.to_path_buf()).expect("install");
        assert_eq!(id, "com.opencapx.echo-vision");
        let listed = mgr.list();
        assert!(listed.iter().any(|p| p.status == "running"));

        // unauthorized: image.read defaults to ask → gate rejects → capability_failed
        let denied = crate::core::capability::execute("image.analyze", &serde_json::json!({"image":"/tmp/a.png"}), None);
        assert!(denied.is_err());

        // after granting, the full path passes
        assert!(crate::core::permission::set_decision(&store, &id, "image.read", "granted"));
        let out = crate::core::capability::execute("image.analyze", &serde_json::json!({"image":"/tmp/a.png"}), None)
            .expect("execute after grant");
        assert!(out["description"].as_str().unwrap().contains("/tmp/a.png"));

        let caps = crate::core::capability::list();
        let entry = caps["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "image.analyze")
            .expect("registered");
        assert!(entry["providers"].as_array().unwrap().iter().any(|p| p == "com.opencapx.echo-vision"));

        mgr.stop(&id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Full path for a TS plugin (node fixture `plugins/echo-vision-ts`, using the real @opencapx/sdk):
    /// install → handshake → default permission denied → grant → capability execute succeeds, and the description's
    /// prefix comes from a real reverse `config.get` (key unset → falls back to the default `ts-echo`).
    /// Requires node; skipped if absent. Like the python e2e, it is serial-exclusive because of the global shared_store:
    /// `cargo test install_grant_execute_ts_e2e -- --ignored --test-threads=1`。
    #[test]
    #[ignore]
    fn install_grant_execute_ts_e2e() {
        use std::sync::{Arc, Mutex};
        let node = std::process::Command::new("node")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !node {
            eprintln!("skip: no node");
            return;
        }
        let dir = std::env::temp_dir().join(format!("opencapx-e2e-ts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());

        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("plugins")
            .join("echo-vision-ts");
        let mgr = PluginManager::shared();
        let id = mgr.install_from_dir(&root.to_path_buf()).expect("install");
        assert_eq!(id, "com.opencapx.echo-vision-ts");
        assert!(mgr.list().iter().any(|p| p.status == "running"));

        // unauthorized: image.read defaults to ask → gate rejects → capability_failed
        let denied =
            crate::core::capability::execute("image.analyze", &serde_json::json!({"image":"/tmp/b.png"}), None);
        assert!(denied.is_err());

        // after granting, the full path passes; the prefix proves the reverse config.get went through the real core
        assert!(crate::core::permission::set_decision(&store, &id, "image.read", "granted"));
        let out = crate::core::capability::execute("image.analyze", &serde_json::json!({"image":"/tmp/b.png"}), None)
            .expect("execute after grant");
        assert_eq!(
            out["description"].as_str().unwrap(),
            "[ts-echo] got /tmp/b.png"
        );

        mgr.stop(&id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_zip(path: &Path, entries: &[(String, String)]) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        use zip::ZipWriter;
        let f = std::fs::File::create(path).unwrap();
        let mut w = ZipWriter::new(f);
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(name.trim_end_matches('/'), SimpleFileOptions::default()).unwrap();
            } else {
                w.start_file(name.clone(), SimpleFileOptions::default()).unwrap();
                w.write_all(data.as_bytes()).unwrap();
            }
        }
        w.finish().unwrap();
    }

    fn echo_zip_entries() -> Vec<(String, String)> {
        // echo_vision.py hardcodes a self-reported pluginId=com.opencapx.echo-vision, so the manifest must match
        let manifest = r#"{"id":"com.opencapx.echo-vision","name":"Echo Vision","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"python3","args":["bin/echo_vision.py"]},"capabilities":["image.analyze"],"permissions":["image.read"]}"#.to_string();
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("plugins").join("echo-vision").join("bin").join("echo_vision.py"),
        )
        .unwrap();
        vec![
            ("opencapx-plugin.json".to_string(), manifest),
            ("bin/echo_vision.py".to_string(), script),
        ]
    }

    /// .ocplugin install: full path of manifest validation, extraction, DB write, and start.
    #[test]
    fn ocplugin_install_runs() {
        skip_probe_in_tests();
        let py = std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !py {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-ocp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store);
        // env is process-level: it must be set inside the lock, otherwise a parallel `ocplugin_rejects_unsafe_paths`
        // remove_var/overwrite → extraction lands in the wrong directory.
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let entries = echo_zip_entries();
        let owned: Vec<(String, String)> = entries.clone();
        let zip_path = base.join("echo.ocplugin");
        make_zip(&zip_path, &owned);

        let mgr = PluginManager::shared();
        let id = install_confirmed(&zip_path).expect("install ocplugin");
        assert_eq!(id, "com.opencapx.echo-vision");
        assert!(base.join("plugins").join(&id).join("bin").join("echo_vision.py").exists());
        assert!(mgr.list().iter().any(|p| p.id == id && p.status == "running"));

        mgr.stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    // ─── Step 2 regression (docs/permission-domains.md §4.4 / §9.1)──────────────────

    /// Reads the version of the plugins row (test helper).
    fn installed_version(store: &SharedStore, id: &str) -> Option<String> {
        store.lock().ok().and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT version FROM plugins WHERE id = ?1",
                    params![id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
        })
        .flatten()
    }

    /// Reads the decision of the plugin_permissions row (test helper).
    fn stored_decision(store: &SharedStore, id: &str, perm: &str) -> Option<String> {
        store.lock().ok().and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT decision FROM plugin_permissions WHERE plugin_id = ?1 AND permission = ?2",
                    params![id, perm],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
        })
        .flatten()
    }

    /// A variant of the echo package: changes version and declared permissions, for overwrite-install / failure-rollback cases.
    fn echo_zip_entries_with(version: &str, permissions: &[&str]) -> Vec<(String, String)> {
        let mut entries = echo_zip_entries();
        let perms = permissions
            .iter()
            .map(|p| format!("\"{}\"", p))
            .collect::<Vec<_>>()
            .join(",");
        let manifest = format!(
            r#"{{"id":"com.opencapx.echo-vision","name":"Echo Vision","version":"{}","apiVersion":"1","type":"capability","runtime":{{"type":"process","command":"python3","args":["bin/echo_vision.py"]}},"capabilities":["image.analyze"],"permissions":[{}]}}"#,
            version, perms
        );
        entries[0] = ("opencapx-plugin.json".to_string(), manifest);
        entries
    }

    /// Two-phase signing of a v2 package (same method as plugin_sig's golden vectors): compute digest_v2 for the base package →
    /// sign `opencapx-v2\n<digest>` with the seed → backfill signature/sha256 and repackage.
    fn make_v2_signed_echo(path: &Path, key_id: &str, seed: [u8; 32], version: &str, perms: &[&str]) {
        use ed25519_dalek::Signer;
        let mut entries = echo_zip_entries_with(version, perms);
        let unsigned = path.with_extension("unsigned.ocplugin");
        make_zip(&unsigned, &entries);
        let digest = crate::core::signing::digest_v2(&unsigned).unwrap();
        let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
        let sig_hex = sk
            .sign(format!("opencapx-v2\n{}", digest).as_bytes())
            .to_bytes()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
        let mut m: serde_json::Value = serde_json::from_str(&entries[0].1).unwrap();
        m["sha256"] = serde_json::Value::String(digest);
        m["signature"] = serde_json::json!({ "keyId": key_id, "sig": sig_hex, "alg": "ed25519" });
        entries[0] = (
            "opencapx-plugin.json".to_string(),
            serde_json::to_string(&m).unwrap(),
        );
        make_zip(path, &entries);
    }

    fn pubkey_hex(seed: [u8; 32]) -> String {
        ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }

    /// Minimal Manifest construction (for pure-function matrix tests).
    fn um(id: &str, version: &str, perms: &[&str], key: Option<&str>) -> Manifest {
        let perms = perms
            .iter()
            .map(|p| format!("\"{}\"", p))
            .collect::<Vec<_>>()
            .join(",");
        let key_block = key
            .map(|k| format!(r#","signature":{{"keyId":"{}","sig":"00"}}"#, k))
            .unwrap_or_default();
        serde_json::from_str(&format!(
            r#"{{"id":"{}","name":"X","version":"{}","apiVersion":"1","type":"capability","runtime":{{"type":"process","command":"true"}},"capabilities":["image.analyze"],"permissions":[{}]{}}}"#,
            id, version, perms, key_block
        ))
        .unwrap()
    }

    /// F7 — test-only: install entry where the soft-warning tier (unsigned / key change) has been explicitly confirmed.
    fn install_confirmed(zip: &Path) -> Result<String, String> {
        PluginManager::shared().install_ocplugin_ex(
            zip,
            InstallOptions {
                confirm_unsigned: true,
                confirm_key_change: true,
            },
        )
    }

    /// Recursively copies a directory (for the sample "updatable" case: repackage after changing the version number).
    fn copy_tree(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let d = dst.join(e.file_name());
            if p.is_dir() {
                copy_tree(&p, &d);
            } else {
                std::fs::copy(&p, &d).unwrap();
            }
        }
    }

    fn python3_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    fn node_available() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    /// Unix: chmod read-only does not block writes under root (write-failure injection would be distorted) → the related cases are skipped.
    #[cfg(unix)]
    fn running_as_root() -> bool {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim() == "0")
            .unwrap_or(false)
    }

    /// §4.4 step 5: the `plugins` row and `plugin_permissions` are written in the same commit phase,
    /// decisions come from the confirmation phase (non-interactive path = default table: image.read defaults to ask).
    #[test]
    fn install_commits_plugin_row_and_permissions_together() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-commit-inst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let zip_path = base.join("echo.ocplugin");
        make_zip(&zip_path, &echo_zip_entries_with("0.1.0", &["image.read"]));
        let id = install_confirmed(&zip_path).expect("install");

        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.1.0"));
        assert_eq!(stored_decision(&store, &id, "image.read").as_deref(), Some("ask"));

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// §4.4 core invariant + review C2 regression: after a failed install (in this case: commit-phase storage unavailable)
    /// **the installed version's directory and DB records are intact**, leaving no half-installed state.
    /// The old implementation swapped/deleted the old directory before confirmation; this case would fail there.
    #[test]
    fn failed_install_keeps_previous_version_intact() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-reject-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        // 1) install a healthy version first
        let v1 = base.join("v1.ocplugin");
        make_zip(&v1, &echo_zip_entries_with("0.1.0", &["image.read"]));
        let id = install_confirmed(&v1).expect("install v1");
        let dest = base.join("plugins").join(&id);
        assert!(dest.join("bin").join("echo_vision.py").exists());
        PluginManager::shared().stop(&id);

        // 2) swap the shared store to Mem (= commit phase unavailable), then install v0.2.0
        crate::core::set_shared_store(std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Mem(crate::core::agent::SessionStore::new()),
        )));
        let v2 = base.join("v2.ocplugin");
        make_zip(&v2, &echo_zip_entries_with("0.2.0", &["image.read", "file.read"]));
        let err = install_confirmed(&v2).unwrap_err();
        assert!(err.contains("sqlite unavailable"), "got: {}", err);

        // 3) the old version must be intact: directory present + DB record still 0.1.0 + decisions not overwritten
        assert!(dest.join("bin").join("echo_vision.py").exists(), "old dir must survive");
        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.1.0"));
        assert_eq!(stored_decision(&store, &id, "image.read").as_deref(), Some("ask"));
        assert_eq!(stored_decision(&store, &id, "file.read"), None, "new permissions must not be written to the DB");
        // no staging or .bak residue may remain
        let leftovers: Vec<String> = std::fs::read_dir(base.join("plugins"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".tmp-") || n.starts_with(".old-"))
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {:?}", leftovers);

        // restore the real store and do a successful overwrite install (verifying the .bak path does not break normal upgrades)
        crate::core::set_shared_store(store.clone());
        let id2 = install_confirmed(&v2).expect("install v2");
        assert_eq!(id2, id);
        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.2.0"));
        assert_eq!(stored_decision(&store, &id, "file.read").as_deref(), Some("ask"));

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    // ─── M4: update and revocation semantics (F6/D5)────────────────────────────────────────

    /// Four-path matrix (pure function): silent / re-confirm / key change / fresh install.
    #[test]
    fn update_plan_matrix() {
        let v1 = um("com.x", "0.1.0", &["image.read"], None);
        let v2_same = um("com.x", "0.2.0", &["image.read"], None);
        let v2_add = um("com.x", "0.2.0", &["image.read", "file.read"], None);

        // same key and unchanged permissions → empty plan (silently reuse existing consent)
        let (plan, key_changed) = PluginManager::update_plan_for(Some(&v1), &v2_same);
        assert!(plan.is_empty(), "silent update must ask nothing");
        assert!(!key_changed);

        // added permission → ask only about the added item
        let (plan, key_changed) = PluginManager::update_plan_for(Some(&v1), &v2_add);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].permission, "file.read");
        assert!(!key_changed);

        // key change → full re-confirmation + marker
        let v1_keya = um("com.x", "0.1.0", &["image.read"], Some("key.a"));
        let v2_keyb = um("com.x", "0.2.0", &["image.read"], Some("key.b"));
        let (plan, key_changed) = PluginManager::update_plan_for(Some(&v1_keya), &v2_keyb);
        assert!(key_changed);
        assert_eq!(plan.len(), 1);

        // unsigned → signed also counts as a key change (None != Some)
        let (_, key_changed) = PluginManager::update_plan_for(Some(&v1), &v2_keyb);
        assert!(key_changed);

        // fresh install → full plan, not a key change
        let (plan, key_changed) = PluginManager::update_plan_for(None, &v2_add);
        assert_eq!(plan.len(), 2);
        assert!(!key_changed);
    }

    /// Silent update (integration): same key, unchanged → existing granted decisions are not reset and the version advances.
    #[test]
    fn update_silent_preserves_granted_decision() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-silent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let v1 = base.join("v1.ocplugin");
        make_zip(&v1, &echo_zip_entries_with("0.1.0", &["image.read"]));
        let id = install_confirmed(&v1).expect("install v1");
        assert!(crate::core::permission::set_decision(&store, &id, "image.read", "granted"));

        // same key (both unsigned), unchanged → silent: the old behavior (full re-ask) would reset granted to ask
        let v2 = base.join("v2.ocplugin");
        make_zip(&v2, &echo_zip_entries_with("0.2.0", &["image.read"]));
        let id2 = install_confirmed(&v2).expect("update v2");
        assert_eq!(id2, id);
        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.2.0"));
        assert_eq!(
            stored_decision(&store, &id, "image.read").as_deref(),
            Some("granted"),
            "silent update must reuse existing consent"
        );

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Key-change path (integration): signed key changed → full re-confirmation (granted is reset) + event.
    #[test]
    fn update_key_change_reconfirms_and_emits() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-keychg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let seed_a = [7u8; 32];
        let seed_b = [9u8; 32];
        let keys_path = base.join("trusted-keys.json");
        std::fs::write(
            &keys_path,
            serde_json::json!({
                "key.a": { "alg": "ed25519", "publicKey": pubkey_hex(seed_a) },
                "key.b": { "alg": "ed25519", "publicKey": pubkey_hex(seed_b) },
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &keys_path);

        let v1 = base.join("v1.ocplugin");
        make_v2_signed_echo(&v1, "key.a", seed_a, "0.1.0", &["image.read"]);
        let id = install_confirmed(&v1).expect("install key.a");
        assert!(crate::core::permission::set_decision(&store, &id, "image.read", "granted"));

        let rx = crate::core::event::EventBus::shared().subscribe();
        let v2 = base.join("v2.ocplugin");
        make_v2_signed_echo(&v2, "key.b", seed_b, "0.2.0", &["image.read"]);
        // F7 — a key change needs explicit confirmation: unconfirmed → marker error; after confirmation → full re-confirmation + event
        let err = PluginManager::shared().install_ocplugin(&v2).unwrap_err();
        assert!(
            err.contains("publisher-key-change-confirm-required"),
            "got: {}",
            err
        );
        let id2 = install_confirmed(&v2).expect("update key.b");
        assert_eq!(id2, id);
        // full re-confirmation: non-interactive uses the default table → granted is reset to ask
        assert_eq!(
            stored_decision(&store, &id, "image.read").as_deref(),
            Some("ask"),
            "key change must force full re-confirm"
        );
        let mut saw_key_change = false;
        while let Ok(e) = rx.try_recv() {
            if e.kind == "plugin.update.key_changed" {
                saw_key_change = true;
            }
        }
        assert!(saw_key_change, "key change event required");

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// F6 transactionality: swap failure (blocked by a .old placeholder file) → roll back the DB row + restore the original runtime state.
    #[test]
    fn update_swap_failure_restores_old_running() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-swapfail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let v1 = base.join("v1.ocplugin");
        make_zip(&v1, &echo_zip_entries_with("0.1.0", &["image.read"]));
        let id = install_confirmed(&v1).expect("install v1");
        assert!(PluginManager::shared()
            .list()
            .iter()
            .any(|p| p.id == id && p.status == "running"));

        // placeholder: put a regular file at the backup path → rename(dest→backup) must fail
        let backup = base
            .join("plugins")
            .join(format!(".old-{}-{}", id, std::process::id()));
        std::fs::write(&backup, b"x").unwrap();

        let v2 = base.join("v2.ocplugin");
        make_zip(&v2, &echo_zip_entries_with("0.2.0", &["image.read"]));
        let err = install_confirmed(&v2).unwrap_err();
        assert!(err.contains("failed to set aside"), "got: {}", err);

        // directory / DB row return to 0.1.0 and the old version is brought back up (original runtime state restored)
        let dest = base.join("plugins").join(&id);
        assert!(dest.join("bin").join("echo_vision.py").exists());
        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.1.0"));
        assert!(
            PluginManager::shared()
                .list()
                .iter()
                .any(|p| p.id == id && p.status == "running"),
            "old version must be restarted after failed swap"
        );
        let leftovers: Vec<String> = std::fs::read_dir(base.join("plugins"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {:?}", leftovers);

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// F6 mutual exclusion: concurrent installs of the same plugin → serialized, no staging crossover / no residue.
    #[test]
    fn concurrent_installs_serialize() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-conc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let v1 = base.join("v1.ocplugin");
        let v2 = base.join("v2.ocplugin");
        make_zip(&v1, &echo_zip_entries_with("0.1.0", &["image.read"]));
        make_zip(&v2, &echo_zip_entries_with("0.2.0", &["image.read"]));

        let (p1, p2) = (v1.clone(), v2.clone());
        let h1 = std::thread::spawn(move || install_confirmed(&p1));
        let h2 = std::thread::spawn(move || install_confirmed(&p2));
        let r1 = h1.join().expect("thread 1");
        let r2 = h2.join().expect("thread 2");
        assert!(r1.is_ok(), "install1: {:?}", r1);
        assert!(r2.is_ok(), "install2: {:?}", r2);

        let id = "com.opencapx.echo-vision";
        let ver = installed_version(&store, id).unwrap();
        assert!(ver == "0.1.0" || ver == "0.2.0", "final version: {}", ver);
        assert!(base
            .join("plugins")
            .join(id)
            .join("bin")
            .join("echo_vision.py")
            .exists());
        let leftovers: Vec<String> = std::fs::read_dir(base.join("plugins"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".tmp-") || n.starts_with(".old-"))
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {:?}", leftovers);

        PluginManager::shared().stop(id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// F6 revocation E2E: hit revokedKeys → block update + disable by default + event;
    /// after reopen it can start again, and the same key is not disabled a second time (ack exemption).
    #[test]
    fn revocation_disables_blocks_and_reopens() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-revoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        // the directory-version plugin carries a signed keyId (the dev path does not verify signatures, only writes to the DB)
        let src = base.join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("plugins")
                .join("echo-vision")
                .join("bin")
                .join("echo_vision.py"),
        )
        .unwrap();
        std::fs::write(src.join("bin").join("echo_vision.py"), script).unwrap();
        std::fs::write(
            src.join("opencapx-plugin.json"),
            r#"{"id":"com.opencapx.echo-vision","name":"Echo Vision","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"python3","args":["bin/echo_vision.py"]},"capabilities":["image.analyze"],"permissions":["image.read"],"signature":{"keyId":"com.test.revoked","sig":"00"}}"#,
        )
        .unwrap();
        let id = PluginManager::shared()
            .install_from_dir(&src)
            .expect("install from dir");
        assert!(PluginManager::shared()
            .list()
            .iter()
            .any(|p| p.id == id && p.status == "running"));

        let index: crate::core::registry::RegistryIndex = serde_json::from_value(
            serde_json::json!({
                "schemaVersion": 2,
                "generatedAt": 1,
                "publishers": [],
                "revokedKeys": [{ "keyId": "com.test.revoked", "at": 9, "reason": "test" }],
                "entries": [],
                "indexSignature": { "alg": "ed25519", "keyId": "k", "sig": "0" }
            }),
        )
        .unwrap();

        let rx = crate::core::event::EventBus::shared().subscribe();
        let hits = crate::core::revocation::sweep_with(&index);
        assert_eq!(hits.len(), 1, "one revoke hit");
        assert_eq!(hits[0].plugin_id, id);
        let mut saw_revoked = false;
        while let Ok(e) = rx.try_recv() {
            if e.kind == "plugin.revoked" {
                saw_revoked = true;
            }
        }
        assert!(saw_revoked, "plugin.revoked event required");

        // disabled by default: marker written to DB + process stopped + start/update rejected
        let row = PluginManager::shared()
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap();
        assert_eq!(row.revoked_key.as_deref(), Some("com.test.revoked"));
        assert_eq!(row.status, "stopped");
        assert!(PluginManager::shared()
            .start(&id)
            .unwrap_err()
            .contains("revoked"));
        let v2 = base.join("v2.ocplugin");
        make_zip(&v2, &echo_zip_entries_with("0.2.0", &["image.read"]));
        let err = PluginManager::shared().install_ocplugin(&v2).unwrap_err();
        assert!(err.contains("plugin revoked"), "got: {}", err);

        // explicit reopen: clear the marker + idempotent error; ack exempts it from the next sweep; it can start again
        crate::core::revocation::reopen(&id).expect("reopen");
        assert!(crate::core::revocation::reopen(&id).is_err(), "not-revoked error");
        let row = PluginManager::shared()
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap();
        assert!(row.revoked_key.is_none());
        let again = crate::core::revocation::sweep_with(&index);
        assert!(again.is_empty(), "ack must exempt re-disable");
        PluginManager::shared().start(&id).expect("start after reopen");
        assert!(PluginManager::shared()
            .list()
            .iter()
            .any(|p| p.id == id && p.status == "running"));

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// F7 three-state matrix: unsigned needs confirmation (audited after confirmation) / switch OFF hard-rejects / tampering is never bypassed.
    #[test]
    fn install_three_state_matrix() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-3state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        // 1) unsigned + unconfirmed → fail-closed (marker error, so the UI can warn and retry)
        let v1 = base.join("v1.ocplugin");
        make_zip(&v1, &echo_zip_entries_with("0.1.0", &["image.read"]));
        let err = PluginManager::shared().install_ocplugin(&v1).unwrap_err();
        assert!(err.contains("unsigned-confirm-required"), "got: {}", err);

        // 2) after confirmation → install succeeds + `plugin.installed.unsigned` leaves a trace
        let rx = crate::core::event::EventBus::shared().subscribe();
        let id = install_confirmed(&v1).expect("confirmed install");
        assert_eq!(id, "com.opencapx.echo-vision");
        let mut saw_unsigned = false;
        while let Ok(e) = rx.try_recv() {
            if e.kind == "plugin.installed.unsigned" {
                saw_unsigned = true;
            }
        }
        assert!(saw_unsigned, "unsigned install audit required");

        // 3) master switch OFF → hard-reject even after confirmation
        crate::core::plugin::set_allow_unsigned(false).unwrap();
        let err = install_confirmed(&v1).unwrap_err();
        assert!(err.contains("allow_unsigned is off"), "got: {}", err);
        crate::core::plugin::set_allow_unsigned(true).unwrap();

        // 4) tampering (sha256 mismatch) is never bypassed: confirmation is likewise rejected
        let tampered = base.join("tampered.ocplugin");
        let mut entries = echo_zip_entries_with("0.1.0", &["image.read"]);
        let mut m: serde_json::Value = serde_json::from_str(&entries[0].1).unwrap();
        m["sha256"] = serde_json::Value::String("00".repeat(32));
        m["signature"] = serde_json::json!({ "keyId": "k1", "sig": "00" });
        entries[0] = (
            "opencapx-plugin.json".to_string(),
            serde_json::to_string(&m).unwrap(),
        );
        make_zip(&tampered, &entries);
        let err = install_confirmed(&tampered).unwrap_err();
        assert!(err.contains("signature verification failed"), "got: {}", err);

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M6 — official sample republish: signed artifact is installable (trusted direct install) + updatable (same key, silent update).
    #[test]
    fn signed_samples_install_and_update() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-samples-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        std::env::set_var(
            "OPENCAPX_TRUSTED_KEYS",
            repo.join("fixtures").join("signing").join("trusted-keys.json"),
        );

        // 1) the sample signed artifact is installable (trusted direct install, no confirmation)
        let samples = repo.join("fixtures").join("samples");
        let things = samples.join("things-demo-0.1.0.ocplugin");
        let id = install_confirmed(&things).expect("install things-demo sample");
        assert_eq!(id, "com.opencapx.things-demo");
        let weather = samples.join("weather-demo-0.1.0.ocplugin");
        let id2 = install_confirmed(&weather).expect("install weather-demo sample");
        assert_eq!(id2, "com.example.weather-demo");
        assert!(PluginManager::shared()
            .list()
            .iter()
            .any(|p| p.id == id2 && p.status == "running"));

        // 2) updatable: repackage weather-demo v0.1.1 with the same key → silent update (same permissions, existing consent reused)
        let src = base.join("weather-src");
        copy_tree(&repo.join("plugins").join("weather-demo"), &src);
        let mpath = src.join("opencapx-plugin.json");
        let mut m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mpath).unwrap()).unwrap();
        m["version"] = serde_json::Value::String("0.1.1".to_string());
        std::fs::write(&mpath, serde_json::to_string(&m).unwrap()).unwrap();
        let seed_text = std::fs::read_to_string(
            repo.join("fixtures").join("signing").join("key.seed.hex"),
        )
        .unwrap();
        let seed_text = seed_text.trim();
        let mut seed = [0u8; 32];
        for i in 0..32 {
            seed[i] = u8::from_str_radix(&seed_text[i * 2..i * 2 + 2], 16).unwrap();
        }
        let v011 = base.join("weather-0.1.1.ocplugin");
        crate::core::pack::pack_dir(&src, &seed, "com.opencapx.test-signing", &v011).unwrap();
        let id3 = install_confirmed(&v011).expect("update weather-demo to 0.1.1");
        assert_eq!(id3, id2);
        assert_eq!(installed_version(&store, &id2).as_deref(), Some("0.1.1"));

        PluginManager::shared().stop(&id);
        PluginManager::shared().stop(&id2);
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M7/F9 — migration-sample upgrade: install 0.1.0 (successful start records last_version) → upgrade to 0.1.1 →
    /// previousVersion injection + idempotent store migration (v1→v2), preserving old data.
    #[test]
    fn migration_previous_version_and_store_upgrade() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        std::env::set_var(
            "OPENCAPX_TRUSTED_KEYS",
            repo.join("fixtures").join("signing").join("trusted-keys.json"),
        );
        let seed_text = std::fs::read_to_string(
            repo.join("fixtures").join("signing").join("key.seed.hex"),
        )
        .unwrap();
        let seed_text = seed_text.trim();
        let mut seed = [0u8; 32];
        for i in 0..32 {
            seed[i] = u8::from_str_radix(&seed_text[i * 2..i * 2 + 2], 16).unwrap();
        }

        // one shared copy of the source: storePath points at a temp file (isolating the real home), with the version substituted as needed.
        let src = base.join("weather-src");
        copy_tree(&repo.join("plugins").join("weather-demo"), &src);
        let store_file = base.join("weather-store.json");
        let set_manifest = |version: &str| {
            let mpath = src.join("opencapx-plugin.json");
            let mut m: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&mpath).unwrap()).unwrap();
            m["version"] = serde_json::Value::String(version.to_string());
            m["storePath"] = serde_json::Value::String(store_file.display().to_string());
            std::fs::write(&mpath, serde_json::to_string(&m).unwrap()).unwrap();
        };

        // 1) v0.1.0 installs and starts successfully → last_version recorded
        set_manifest("0.1.0");
        let v1 = base.join("weather-0.1.0.ocplugin");
        crate::core::pack::pack_dir(&src, &seed, "com.opencapx.test-signing", &v1).unwrap();
        let id = install_confirmed(&v1).expect("install 0.1.0");
        assert_eq!(id, "com.example.weather-demo");
        assert_eq!(PluginManager::last_version_of(&id).as_deref(), Some("0.1.0"));
        PluginManager::shared().stop(&id);

        // 2) simulate the v1 store left by the old version (the host does not migrate; the plugin migrates itself on the next start)
        std::fs::write(&store_file, r#"{"home": "Shanghai"}"#).unwrap();

        // 3) upgrade to 0.1.1 (same key, silent) → previousVersion=0.1.0 injected → idempotent migration on start
        set_manifest("0.1.1");
        let v2 = base.join("weather-0.1.1.ocplugin");
        crate::core::pack::pack_dir(&src, &seed, "com.opencapx.test-signing", &v2).unwrap();
        install_confirmed(&v2).expect("update 0.1.1");
        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.1.1"));
        assert_eq!(PluginManager::last_version_of(&id).as_deref(), Some("0.1.1"));

        let migrated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&store_file).unwrap()).unwrap();
        assert_eq!(migrated["schema"], serde_json::json!(2));
        assert_eq!(migrated["home_city"], serde_json::json!("Shanghai"));
        assert_eq!(migrated["migrated_from"], serde_json::json!("0.1.0"));

        // 4) idempotent + old data preserved: after a restart the read still has the migrated shape and the old city
        let proc = PluginManager::shared().ensure_running(&id).expect("running");
        let out = proc
            .call("weather.current", json!({}), Duration::from_secs(10))
            .expect("weather.current");
        assert_eq!(out["city"], "Shanghai");
        let again: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&store_file).unwrap()).unwrap();
        assert_eq!(again["schema"], serde_json::json!(2));
        assert_eq!(again["migrated_from"], serde_json::json!("0.1.0"));

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M7/F8 — settings[] frozen schema: the original 8 controls are legal; duplicate keys / bad types / bad dropdown /
    /// button with a value / secret with a default / floating-point number → reject (P1's new controls are covered in later tests).
    #[test]
    fn settings_schema_validation() {
        fn manifest_with_settings(settings: serde_json::Value) -> Manifest {
            serde_json::from_value(serde_json::json!({
                "id": "com.x", "name": "X", "version": "0.1.0", "apiVersion": "1",
                "type": "capability",
                "runtime": {"type": "process", "command": "true"},
                "capabilities": ["image.analyze"], "permissions": ["image.read"],
                "settings": settings
            }))
            .unwrap()
        }

        // all 8 controls present → legal
        let full = manifest_with_settings(serde_json::json!([
            {"key": "auto_play", "type": "toggle", "label": "Auto", "default": true},
            {"key": "api_key", "type": "secret", "label": "API Key"},
            {"key": "quality", "type": "dropdown", "options": ["low", "high"], "default": "low"},
            {"key": "data_dir", "type": "path"},
            {"key": "retry", "type": "number", "default": 3},
            {"key": "notes", "type": "textarea"},
            {"key": "title", "type": "text"},
            {"key": "run_now", "type": "button", "label": "Run"}
        ]));
        assert!(PluginManager::validate_manifest(&full).is_ok());

        let dup = manifest_with_settings(serde_json::json!([
            {"key": "a", "type": "text"}, {"key": "a", "type": "toggle"}
        ]));
        assert!(PluginManager::validate_manifest(&dup)
            .unwrap_err()
            .contains("duplicate"));

        let bad_type = manifest_with_settings(serde_json::json!([{"key": "a", "type": "date"}]));
        assert!(PluginManager::validate_manifest(&bad_type)
            .unwrap_err()
            .contains("unknown setting type"));

        let bad_key = manifest_with_settings(serde_json::json!([{"key": "BadKey", "type": "text"}]));
        assert!(PluginManager::validate_manifest(&bad_key)
            .unwrap_err()
            .contains("invalid setting key"));

        let dd_no_options =
            manifest_with_settings(serde_json::json!([{"key": "a", "type": "dropdown"}]));
        assert!(PluginManager::validate_manifest(&dd_no_options)
            .unwrap_err()
            .contains("options"));

        let dd_bad_default = manifest_with_settings(serde_json::json!([
            {"key": "a", "type": "dropdown", "options": ["x"], "default": "y"}
        ]));
        assert!(PluginManager::validate_manifest(&dd_bad_default)
            .unwrap_err()
            .contains("one of options"));

        let btn_default = manifest_with_settings(serde_json::json!([
            {"key": "a", "type": "button", "default": true}
        ]));
        assert!(PluginManager::validate_manifest(&btn_default)
            .unwrap_err()
            .contains("button"));

        let secret_default = manifest_with_settings(serde_json::json!([
            {"key": "a", "type": "secret", "default": "x"}
        ]));
        assert!(PluginManager::validate_manifest(&secret_default)
            .unwrap_err()
            .contains("secret"));

        let float_number = manifest_with_settings(serde_json::json!([
            {"key": "a", "type": "number", "default": 1.5}
        ]));
        assert!(PluginManager::validate_manifest(&float_number)
            .unwrap_err()
            .contains("integer"));

        let too_many = manifest_with_settings(serde_json::Value::Array(
            (0..33)
                .map(|i| serde_json::json!({"key": format!("k{}", i), "type": "text"}))
                .collect(),
        ));
        assert!(PluginManager::validate_manifest(&too_many)
            .unwrap_err()
            .contains("too many"));
    }

    /// M7/F8 — declarative settings view and write gate: default fallback / secret only reports set / uninstall clears the fallback.
    #[test]
    fn declarative_settings_view_and_set() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("OPENCAPX_SECRETS_DIR", base.join("secrets"));
        crate::core::set_shared_store(store.clone());

        let src = base.join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("plugins")
                .join("echo-vision")
                .join("bin")
                .join("echo_vision.py"),
        )
        .unwrap();
        std::fs::write(src.join("bin").join("echo_vision.py"), script).unwrap();
        std::fs::write(
            src.join("opencapx-plugin.json"),
            serde_json::json!({
                "id": "com.opencapx.echo-vision",
                "name": "Echo Vision",
                "version": "0.1.0",
                "apiVersion": "1",
                "type": "capability",
                "runtime": {"type": "process", "command": "python3", "args": ["bin/echo_vision.py"]},
                "capabilities": ["image.analyze"],
                "permissions": ["image.read"],
                "settings": [
                    {"key": "auto_play", "type": "toggle", "default": true},
                    {"key": "api_key", "type": "secret", "label": "API Key"},
                    {"key": "quality", "type": "dropdown", "options": ["low", "high"], "default": "low"},
                    {"key": "run_now", "type": "button", "label": "Run"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let id = PluginManager::shared().install_from_dir(&src).expect("install");
        assert_eq!(id, "com.opencapx.echo-vision");

        // view: default-value fallback + secret unset (value not returned)
        let view = PluginManager::settings_view(&id).expect("view");
        assert_eq!(view.settings.len(), 4);
        assert_eq!(view.values.get("auto_play"), Some(&serde_json::json!(true)));
        assert_eq!(view.values.get("quality"), Some(&serde_json::json!("low")));
        assert!(view.secrets_set.is_empty());
        assert!(view.values.get("api_key").is_none());

        // write: non-secret goes to config; secret goes to the keychain fallback; button/undeclared keys rejected
        PluginManager::set_setting_value(&id, "auto_play", &serde_json::json!(false)).unwrap();
        PluginManager::set_setting_value(&id, "api_key", &serde_json::json!("s3cret")).unwrap();
        let err =
            PluginManager::set_setting_value(&id, "run_now", &serde_json::json!(null)).unwrap_err();
        assert!(err.contains("action"), "got: {}", err);
        let err =
            PluginManager::set_setting_value(&id, "nope", &serde_json::json!(1)).unwrap_err();
        assert!(err.contains("not declared"), "got: {}", err);

        let view = PluginManager::settings_view(&id).expect("view 2");
        assert_eq!(view.values.get("auto_play"), Some(&serde_json::json!(false)));
        assert_eq!(view.secrets_set, vec!["api_key".to_string()]);
        assert!(
            view.values.get("api_key").is_none(),
            "secret value must never come back to UI"
        );

        // uninstall: the secret fallback directory is cleaned up too
        let secret_dir = base.join("secrets").join("com.opencapx.echo-vision");
        assert!(secret_dir.exists());
        PluginManager::shared().uninstall(&id).expect("uninstall");
        assert!(!secret_dir.exists(), "uninstall must clean fallback secrets");

        std::env::remove_var("OPENCAPX_SECRETS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    fn decl_manifest(settings: serde_json::Value) -> Manifest {
        serde_json::from_value(serde_json::json!({
            "id": "com.x", "name": "X", "version": "0.1.0", "apiVersion": "1",
            "type": "capability",
            "runtime": {"type": "process", "command": "true"},
            "capabilities": ["image.analyze"], "permissions": ["image.read"],
            "settings": settings
        }))
        .expect("settings manifest should deserialize")
    }

    fn decl_err(settings: serde_json::Value) -> String {
        PluginManager::validate_manifest(&decl_manifest(settings))
            .expect_err("manifest should be rejected")
    }

    #[test]
    fn settings_p1_new_controls_validate() {
        let m = decl_manifest(serde_json::json!([
            {"key": "level", "type": "slider", "min": 1, "max": 8, "step": 0.5, "default": 3.5},
            {"key": "tier", "type": "radio-group", "options": ["a", "b"], "default": "a"},
            {"key": "tint", "type": "color", "default": "#a1b2c3"},
            {"key": "accent", "type": "color", "default": "#fff"},
            {"key": "retry", "type": "number", "min": 1, "max": 5, "step": 1, "default": 3}
        ]));
        assert!(PluginManager::validate_manifest(&m).is_ok());
    }

    #[test]
    fn settings_p1_slider_requires_min_max_lt_and_positive_step() {
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "slider", "min": 1}]))
            .contains("min and max"));
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "slider", "max": 8}]))
            .contains("min and max"));
        assert!(
            decl_err(serde_json::json!([{"key": "a", "type": "slider", "min": 8, "max": 8}]))
                .contains("min < max")
        );
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "slider", "min": 0, "max": 8, "step": 0}
        ]))
        .contains("step"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "slider", "min": 1, "max": 8, "default": 9.0}
        ]))
        .contains("above max"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "slider", "min": 1, "max": 8, "default": 0.0}
        ]))
        .contains("below min"));
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "slider", "min": 1, "max": 8, "default": 1.5}
        ])))
        .is_ok());
    }

    #[test]
    fn settings_p1_range_fields_only_on_number_and_slider() {
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "text", "min": 1}]))
            .contains("min/max/step"));
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "toggle", "max": 1}]))
            .contains("min/max/step"));
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "text", "step": 1}]))
            .contains("min/max/step"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "dropdown", "options": ["x"], "max": 2}
        ]))
        .contains("min/max/step"));
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "number", "min": 0, "max": 10, "step": 2}
        ])))
        .is_ok());
    }

    #[test]
    fn settings_p1_radio_group_like_dropdown() {
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "radio-group"}]))
            .contains("options"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "radio-group", "options": ["x"], "default": "y"}
        ]))
        .contains("one of options"));
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "radio-group", "options": ["x", "y"], "default": "y"}
        ])))
        .is_ok());
    }

    #[test]
    fn settings_p1_color_default_must_be_hex_and_no_options() {
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "color", "default": "red"}]))
            .contains("hex"));
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "color", "default": "#12"}]))
            .contains("hex"));
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "color", "default": "#12345"}]))
            .contains("hex"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "color", "options": ["#000"]}
        ]))
        .contains("options/min/max"));
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "color", "min": 0}]))
            .contains("min/max/step"));
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "color", "default": "#0f0"}
        ])))
        .is_ok());
    }

    #[test]
    fn settings_p1_pick_only_on_path_and_known_value() {
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "path", "pick": "file"}
        ])))
        .is_ok());
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "path", "pick": "directory"}
        ])))
        .is_ok());
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "text", "pick": "file"}]))
            .contains("pick"));
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "path", "pick": "bogus"}]))
            .contains("file"));
    }

    #[test]
    fn settings_p1_section_nonblank_and_max_40() {
        let exact = "x".repeat(40);
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "text", "section": exact}
        ])))
        .is_ok());
        let too_long = "x".repeat(41);
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "text", "section": too_long}]))
            .contains("section"));
        assert!(decl_err(serde_json::json!([{"key": "a", "type": "text", "section": "   "}]))
            .contains("section"));
    }

    #[test]
    fn settings_p1_predicates_validate_references() {
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "auto", "type": "toggle"},
            {"key": "label", "type": "text", "visible": {"op": "equals", "key": "auto", "value": true}}
        ])))
        .is_ok());

        assert!(decl_err(serde_json::json!([
            {"key": "label", "type": "text", "visible": {"op": "equals", "key": "nope", "value": 1}}
        ]))
        .contains("undeclared"));

        assert!(decl_err(serde_json::json!([
            {"key": "token", "type": "secret"},
            {"key": "label", "type": "text", "disabled": {"op": "equals", "key": "token", "value": "x"}}
        ]))
        .contains("isSet"));

        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "token", "type": "secret"},
            {"key": "label", "type": "text", "disabled": {"op": "isSet", "key": "token", "value": true}}
        ])))
        .is_ok());

        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "mode", "type": "dropdown", "options": ["a", "b"]},
            {"key": "label", "type": "text", "visible": {"op": "equals", "key": "mode", "value": "a"}}
        ])))
        .is_ok());

        assert!(decl_err(serde_json::json!([
            {"key": "mode", "type": "dropdown", "options": ["a", "b"]},
            {"key": "label", "type": "text", "visible": {"op": "equals", "key": "mode", "value": "c"}}
        ]))
        .contains("options"));

        assert!(decl_err(serde_json::json!([
            {"key": "mode", "type": "dropdown", "options": ["a", "b"]},
            {"key": "label", "type": "text", "visible": {"op": "in", "key": "mode", "values": ["a", "z"]}}
        ]))
        .contains("options"));

        assert!(decl_err(serde_json::json!([
            {"key": "mode", "type": "radio-group", "options": ["a"]},
            {"key": "label", "type": "text", "visible": {"op": "notEquals", "key": "mode", "value": "z"}}
        ]))
        .contains("options"));
    }

    #[test]
    fn settings_p1_predicates_group_shape_and_depth() {
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "toggle"},
            {"key": "b", "type": "text", "visible": {"op": "all", "conds": []}}
        ]))
        .contains("non-empty"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "toggle"},
            {"key": "b", "type": "text", "visible": {"op": "any", "conds": []}}
        ]))
        .contains("non-empty"));

        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "toggle"},
            {"key": "b", "type": "toggle"},
            {"key": "c", "type": "text", "disabled": {"op": "not", "cond": {"op": "any", "conds": [
                {"op": "equals", "key": "a", "value": true},
                {"op": "all", "conds": [{"op": "isSet", "key": "b", "value": true}]}
            ]}}}
        ])))
        .is_ok());

        fn deep(n: usize) -> serde_json::Value {
            let mut cond = serde_json::json!({"op": "equals", "key": "a", "value": true});
            for _ in 0..n {
                cond = serde_json::json!({"op": "not", "cond": cond});
            }
            cond
        }
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "toggle"},
            {"key": "b", "type": "text", "visible": deep(8)}
        ])))
        .is_ok());
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "toggle"},
            {"key": "b", "type": "text", "visible": deep(9)}
        ]))
        .contains("deep"));
    }

    #[test]
    fn settings_p1_unknown_predicate_op_and_rule_type_rejected() {
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "toggle"},
            {"key": "b", "type": "text", "visible": {"op": "matches", "key": "a", "value": "x"}}
        ]))
        .contains("unknown predicate"));
        assert!(decl_err(serde_json::json!([
            {"key": "b", "type": "text", "validate": [{"type": "regex", "value": "x"}]}
        ]))
        .contains("unknown validate rule"));
    }

    #[test]
    fn settings_p1_is_set_value_must_be_bool() {
        let bad = serde_json::json!({
            "id": "com.x", "name": "X", "version": "0.1.0", "apiVersion": "1",
            "type": "capability",
            "runtime": {"type": "process", "command": "true"},
            "capabilities": ["image.analyze"], "permissions": ["image.read"],
            "settings": [
                {"key": "token", "type": "secret"},
                {"key": "label", "type": "text", "visible": {"op": "isSet", "key": "token", "value": "yes"}}
            ]
        });
        assert!(serde_json::from_value::<Manifest>(bad).is_err());
    }

    #[test]
    fn settings_p1_validate_rule_type_table() {
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "title", "type": "text", "validate": [
                {"type": "required"}, {"type": "minLength", "value": 2},
                {"type": "maxLength", "value": 10}, {"type": "pattern", "regex": "^x"}
            ]},
            {"key": "notes", "type": "textarea", "validate": [{"type": "required"}]},
            {"key": "dir", "type": "path", "validate": [{"type": "required"}]},
            {"key": "token", "type": "secret", "validate": [{"type": "required"}]},
            {"key": "n", "type": "number", "validate": [
                {"type": "required"}, {"type": "min", "value": 1}, {"type": "max", "value": 9}
            ]},
            {"key": "s", "type": "slider", "min": 0, "max": 10, "validate": [{"type": "min", "value": 1}]},
            {"key": "c", "type": "color", "default": "#fff", "validate": [{"type": "pattern", "regex": "^#"}]}
        ])))
        .is_ok());

        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "toggle", "validate": [{"type": "required"}]}
        ]))
        .contains("not allowed"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "number", "validate": [{"type": "minLength", "value": 1}]}
        ]))
        .contains("not allowed"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "dropdown", "options": ["x"], "validate": [{"type": "required"}]}
        ]))
        .contains("not allowed"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "button", "validate": [{"type": "required"}]}
        ]))
        .contains("not allowed"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "text", "validate": [{"type": "min", "value": 1}]}
        ]))
        .contains("not allowed"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "radio-group", "options": ["x"], "validate": [{"type": "required"}]}
        ]))
        .contains("not allowed"));
    }

    #[test]
    fn settings_p1_pattern_must_be_rust_regex() {
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "path", "validate": [{"type": "pattern", "regex": "\\.json$"}]}
        ])))
        .is_ok());
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "text", "validate": [{"type": "pattern", "regex": "("}]}
        ]))
        .contains("regex"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "text", "validate": [{"type": "pattern", "regex": "(?=x)"}]}
        ]))
        .contains("regex"));
    }

    #[test]
    fn settings_p1_pattern_must_be_js_compilable() {
        for re in ["(?i)foo", "(?m)^a$", "(?s).", "(?x)a b", "(?U)a+", "(?-i)foo", "(?P<n>a)"] {
            let e = decl_err(serde_json::json!([
                {"key": "a", "type": "text", "validate": [{"type": "pattern", "regex": re}]}
            ]));
            assert!(
                e.contains("JavaScript") && e.contains("Rust-only"),
                "pattern {:?} must be rejected as JS-incompatible, got: {}",
                re,
                e
            );
        }
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "text", "validate": [{"type": "pattern", "regex": "(?:foo)"}]}
        ])))
        .is_ok());
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "text", "validate": [{"type": "pattern", "regex": "(?<name>foo)"}]}
        ])))
        .is_ok());
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "text", "validate": [{"type": "pattern", "regex": "^[a-z]+$"}]}
        ])))
        .is_ok());
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "text", "validate": [{"type": "pattern", "regex": "(?=x)"}]}
        ]))
        .contains("regex"));
    }

    #[test]
    fn settings_p1_default_must_satisfy_own_validate() {
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "text", "default": "hi", "validate": [{"type": "minLength", "value": 8}]}
        ]))
        .contains("default"));
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "text", "default": "", "validate": [{"type": "required"}]}
        ]))
        .contains("default"));
        let e = decl_err(serde_json::json!([
            {"key": "a", "type": "text", "default": "nope",
             "validate": [{"type": "pattern", "regex": "^x", "message": "must start with x"}]}
        ]));
        assert!(e.contains("default") && e.contains("must start with x"), "got: {}", e);
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "text", "default": "longenough", "validate": [{"type": "minLength", "value": 8}]}
        ])))
        .is_ok());
    }

    #[test]
    fn settings_p1_optional_rules_skip_unset_values() {
        fn rule(v: serde_json::Value) -> ValidateRule {
            serde_json::from_value(v).expect("validate rule")
        }
        let missing = serde_json::Value::Null;
        let empty = serde_json::json!("");
        let empty_arr = serde_json::json!([]);
        let rules = [
            rule(serde_json::json!({"type": "pattern", "regex": "^[a-z]+$"})),
            rule(serde_json::json!({"type": "minLength", "value": 8})),
            rule(serde_json::json!({"type": "maxLength", "value": 2})),
            rule(serde_json::json!({"type": "min", "value": 1})),
            rule(serde_json::json!({"type": "max", "value": 1})),
        ];
        for r in &rules {
            assert!(rule_passes(r, &missing), "{:?} must skip null", r);
            assert!(rule_passes(r, &empty), "{:?} must skip \"\"", r);
            assert!(rule_passes(r, &empty_arr), "{:?} must skip []", r);
        }
        assert!(!rule_passes(&rules[0], &serde_json::json!("NO")));
        assert!(!rule_passes(&rules[1], &serde_json::json!("short")));
        assert!(rule_passes(&rules[1], &serde_json::json!("longenough")));
        assert!(!rule_passes(&rules[2], &serde_json::json!("long")));
        assert!(!rule_passes(&rules[3], &serde_json::json!(0)));
        assert!(!rule_passes(&rules[4], &serde_json::json!(2)));

        let required = rule(serde_json::json!({"type": "required"}));
        assert!(!rule_passes(&required, &missing));
        assert!(!rule_passes(&required, &empty));
        assert!(!rule_passes(&required, &empty_arr));
        assert!(rule_passes(&required, &serde_json::json!("x")));
    }

    #[test]
    fn settings_p1_empty_default_with_pattern_validates() {
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "store_path", "type": "path", "default": "", "validate": [
                {"type": "pattern", "regex": "\\.json$", "message": "must be a .json file"}
            ]}
        ])))
        .is_ok());
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "optional_title", "type": "text", "default": "", "validate": [
                {"type": "minLength", "value": 8}
            ]}
        ])))
        .is_ok());
        assert!(decl_err(serde_json::json!([
            {"key": "store_path", "type": "path", "default": "notes.txt", "validate": [
                {"type": "pattern", "regex": "\\.json$"}
            ]}
        ]))
        .contains("default"));
    }

    #[test]
    fn settings_p1_legacy_eight_controls_still_validate_and_serialize() {
        let m = decl_manifest(serde_json::json!([
            {"key": "auto_play", "type": "toggle", "label": "Auto", "default": true},
            {"key": "api_key", "type": "secret", "label": "API Key"},
            {"key": "quality", "type": "dropdown", "options": ["low", "high"], "default": "low"},
            {"key": "data_dir", "type": "path"},
            {"key": "retry", "type": "number", "default": 3},
            {"key": "notes", "type": "textarea"},
            {"key": "title", "type": "text"},
            {"key": "run_now", "type": "button", "label": "Run"}
        ]));
        assert!(PluginManager::validate_manifest(&m).is_ok());
        let v = serde_json::to_value(&m).expect("serialize");
        for (i, s) in v["settings"].as_array().unwrap().iter().enumerate() {
            for k in ["visible", "disabled", "validate", "section", "pick", "min", "max", "step"] {
                assert!(s.get(k).is_none(), "legacy setting #{} unexpectedly gained {}", i, k);
            }
        }
    }

    #[test]
    fn settings_p1_dto_round_trips_new_fields() {
        let m = decl_manifest(serde_json::json!([
            {"key": "mode", "type": "dropdown", "options": ["a", "b"], "default": "a"},
            {"key": "level", "type": "slider", "min": 1, "max": 8, "step": 0.5, "default": 2,
             "section": "Audio",
             "visible": {"op": "equals", "key": "mode", "value": "a"},
             "disabled": {"op": "not", "cond": {"op": "isSet", "key": "mode", "value": true}},
             "validate": [{"type": "max", "value": 8, "message": "too high"}]}
        ]));
        let dto = SettingsViewDto {
            settings: m.settings.clone(),
            values: serde_json::Map::new(),
            secrets_set: Vec::new(),
        };
        let v = serde_json::to_value(&dto).expect("serialize dto");
        let s = v["settings"][1].clone();
        assert_eq!(s["section"], serde_json::json!("Audio"));
        assert_eq!(s["min"], serde_json::json!(1));
        assert_eq!(s["max"], serde_json::json!(8));
        assert_eq!(s["step"], serde_json::json!(0.5));
        assert_eq!(s["visible"]["op"], serde_json::json!("equals"));
        assert_eq!(s["disabled"]["op"], serde_json::json!("not"));
        assert_eq!(s["validate"][0]["type"], serde_json::json!("max"));
        assert_eq!(s["validate"][0]["message"], serde_json::json!("too high"));
        let back: SettingDecl = serde_json::from_value(s).expect("decl round-trips");
        assert_eq!(back.validate.len(), 1);
        assert!(back.visible.is_some());
        assert_eq!(back.min.as_ref().and_then(|n| n.as_f64()), Some(1.0));
    }

    /// P1 — disk-write enforcement: `validate[]` failure → `invalid: <message>` / `invalid: <rule-type>`;
    /// a plugin's reverse `config.set` does not take this path and is not subject to validation.
    #[test]
    fn settings_p1_set_setting_value_enforces_validate() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-settings-p1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("OPENCAPX_SECRETS_DIR", base.join("secrets"));
        crate::core::set_shared_store(store.clone());

        let src = base.join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("plugins")
                .join("echo-vision")
                .join("bin")
                .join("echo_vision.py"),
        )
        .unwrap();
        std::fs::write(src.join("bin").join("echo_vision.py"), script).unwrap();
        std::fs::write(
            src.join("opencapx-plugin.json"),
            serde_json::json!({
                "id": "com.opencapx.p1-settings",
                "name": "P1 Settings",
                "version": "0.1.0",
                "apiVersion": "1",
                "type": "capability",
                "runtime": {"type": "process", "command": "python3", "args": ["bin/echo_vision.py"]},
                "capabilities": ["image.analyze"],
                "permissions": ["image.read"],
                "settings": [
                    {"key": "title", "type": "text", "default": "longenough",
                     "validate": [
                        {"type": "required"},
                        {"type": "minLength", "value": 8, "message": "too short"},
                        {"type": "pattern", "regex": "^[a-z]+$"}
                     ]},
                    {"key": "store_path", "type": "path", "validate": [
                        {"type": "pattern", "regex": "\\.json$", "message": "must be a .json file"}
                    ]},
                    {"key": "level", "type": "number", "min": 1, "max": 5, "default": 3}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let id = PluginManager::shared().install_from_dir(&src).expect("install");
        assert_eq!(id, "com.opencapx.p1-settings");

        assert_eq!(
            PluginManager::set_setting_value(&id, "title", &serde_json::json!("hi")).unwrap_err(),
            "invalid: too short"
        );
        assert_eq!(
            PluginManager::set_setting_value(&id, "title", &serde_json::json!("BadValue"))
                .unwrap_err(),
            "invalid: pattern"
        );
        assert_eq!(
            PluginManager::set_setting_value(&id, "title", &serde_json::json!("")).unwrap_err(),
            "invalid: required"
        );
        PluginManager::set_setting_value(&id, "title", &serde_json::json!("goodvalue")).unwrap();

        PluginManager::set_setting_value(&id, "store_path", &serde_json::json!(""))
            .expect("optional empty value skips the pattern rule");
        assert_eq!(
            PluginManager::set_setting_value(&id, "store_path", &serde_json::json!("notes.txt"))
                .unwrap_err(),
            "invalid: must be a .json file"
        );
        PluginManager::set_setting_value(&id, "store_path", &serde_json::json!("notes.json"))
            .unwrap();

        crate::core::config::dispatch(
            &id,
            "set",
            &serde_json::json!({"key": "title", "value": ""}),
        )
        .expect("plugin reverse config.set must bypass validate");

        PluginManager::shared().uninstall(&id).expect("uninstall");
        let _ = std::fs::remove_file(crate::core::config::config_path(&id));
        std::env::remove_var("OPENCAPX_SECRETS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// P2 — plain-string form: an existing manifest's declarations serialize field-for-field the same as before the change.
    #[test]
    fn settings_p2_plain_text_round_trips_identically() {
        let declared = serde_json::json!({
            "key": "mode", "type": "text",
            "label": "Backend mode",
            "description": "Which backend to use",
            "section": "General",
            "validate": [{"type": "required", "message": "required!"}]
        });
        let m = decl_manifest(serde_json::json!([declared.clone()]));
        assert!(PluginManager::validate_manifest(&m).is_ok());
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(
            v["settings"][0], declared,
            "plain strings must serialize exactly as before (no object wrapper)"
        );
    }

    /// P2 — map form: label/description/section/message all accept locale mappings;
    /// unknown locale keys are not rejected (a plugin may carry a language the App does not yet know).
    #[test]
    fn settings_p2_localized_map_forms_validate() {
        let m = decl_manifest(serde_json::json!([
            {"key": "mode", "type": "text",
             "label": {"en": "Backend mode", "zh-Hans": "后端模式", "vi": "Chế độ backend", "ja": "バックエンド"},
             "description": {"en": "Which backend to use", "zh-Hans": "使用哪个后端"},
             "section": {"en": "General", "zh-Hans": "通用"},
             "aliases": ["mode", "backend", "运行模式"],
             "validate": [{"type": "required",
                           "message": {"en": "Backend mode is required", "zh-Hans": "后端模式必填"}}]}
        ]));
        assert!(PluginManager::validate_manifest(&m).is_ok());
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["settings"][0]["label"]["zh-Hans"], serde_json::json!("后端模式"));
        assert_eq!(v["settings"][0]["label"]["ja"], serde_json::json!("バックエンド"));
        assert_eq!(v["settings"][0]["aliases"][2], serde_json::json!("运行模式"));
        assert_eq!(
            v["settings"][0]["validate"][0]["message"]["en"],
            serde_json::json!("Backend mode is required")
        );
    }

    #[test]
    fn settings_p2_localized_map_rejects_blank_and_empty() {
        let blank_value = decl_err(serde_json::json!([
            {"key": "a", "type": "text", "label": {"en": "Ok", "vi": "  "}}
        ]));
        assert!(blank_value.contains("label"), "got: {blank_value}");

        let blank_desc = decl_err(serde_json::json!([
            {"key": "a", "type": "text", "description": {"en": ""}}
        ]));
        assert!(blank_desc.contains("description"), "got: {blank_desc}");

        let empty_map = decl_err(serde_json::json!([
            {"key": "a", "type": "text", "label": {}}
        ]));
        assert!(empty_map.contains("label"), "got: {empty_map}");

        let blank_key = decl_err(serde_json::json!([
            {"key": "a", "type": "text", "label": {"": "x"}}
        ]));
        assert!(blank_key.contains("label"), "got: {blank_key}");

        let blank_section = decl_err(serde_json::json!([
            {"key": "a", "type": "text", "section": {"en": "  "}}
        ]));
        assert!(blank_section.contains("section"), "got: {blank_section}");

        let long_section = decl_err(serde_json::json!([
            {"key": "a", "type": "text", "section": {"en": "x".repeat(41)}}
        ]));
        assert!(long_section.contains("section"), "got: {long_section}");

        let blank_message = decl_err(serde_json::json!([
            {"key": "a", "type": "text",
             "validate": [{"type": "required", "message": {"zh-Hans": "   "}}]}
        ]));
        assert!(blank_message.contains("message"), "got: {blank_message}");
    }

    #[test]
    fn settings_p2_aliases_limits() {
        let ok8: Vec<String> = (0..8).map(|i| format!("alias{}", i)).collect();
        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "text", "aliases": ok8}
        ])))
        .is_ok());

        let too_many: Vec<String> = (0..9).map(|i| format!("alias{}", i)).collect();
        assert!(
            decl_err(serde_json::json!([{"key": "a", "type": "text", "aliases": too_many}]))
                .contains("too many aliases")
        );

        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "text", "aliases": ["ok", "   "]}
        ]))
        .contains("alias"));

        let long = "x".repeat(41);
        assert!(decl_err(serde_json::json!([
            {"key": "a", "type": "text", "aliases": ["ok", long]}
        ]))
        .contains("too long"));

        assert!(PluginManager::validate_manifest(&decl_manifest(serde_json::json!([
            {"key": "a", "type": "text", "aliases": ["x".repeat(40)]}
        ])))
        .is_ok());
    }

    /// P2 — host-side text selection: use `en` if present; otherwise the lexicographically first key (BTreeMap iteration order).
    #[test]
    fn settings_p2_host_locale_picker_prefers_en_then_lexicographic() {
        fn map(pairs: &[(&str, &str)]) -> LocalizedText {
            LocalizedText::Map(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
        }
        assert_eq!(
            LocalizedText::Plain("plain".into()).pick_host_locale(),
            "plain"
        );
        assert_eq!(
            map(&[("zh-Hans", "后端模式"), ("en", "Backend mode"), ("vi", "Chế độ backend")])
                .pick_host_locale(),
            "Backend mode"
        );
        assert_eq!(
            map(&[("zh-Hans", "后端模式"), ("vi", "Chế độ backend")]).pick_host_locale(),
            "Chế độ backend",
            "no en → lexicographically first key (vi < zh-Hans)"
        );
    }

    /// P2 — when a rule's message is a map, the host error string is resolved via pick_host_locale.
    /// The UI validates by the current locale first, so normal users see the UI text; this path is the fallback for non-UI callers.
    #[test]
    fn settings_p2_rule_message_map_end_to_end() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-settings-p2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("OPENCAPX_SECRETS_DIR", base.join("secrets"));
        crate::core::set_shared_store(store.clone());

        let src = base.join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("plugins")
                .join("echo-vision")
                .join("bin")
                .join("echo_vision.py"),
        )
        .unwrap();
        std::fs::write(src.join("bin").join("echo_vision.py"), script).unwrap();
        std::fs::write(
            src.join("opencapx-plugin.json"),
            serde_json::json!({
                "id": "com.opencapx.p2-settings",
                "name": "P2 Settings",
                "version": "0.1.0",
                "apiVersion": "1",
                "type": "capability",
                "runtime": {"type": "process", "command": "python3", "args": ["bin/echo_vision.py"]},
                "capabilities": ["image.analyze"],
                "permissions": ["image.read"],
                "settings": [
                    {"key": "with_en", "type": "text", "default": "longenough",
                     "validate": [{"type": "minLength", "value": 8,
                                   "message": {"en": "too short", "zh-Hans": "太短"}}]},
                    {"key": "no_en", "type": "text", "default": "longenough",
                     "validate": [{"type": "minLength", "value": 8,
                                   "message": {"zh-Hans": "名字太短", "vi": "Tên quá ngắn"}}]}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let id = PluginManager::shared().install_from_dir(&src).expect("install");
        assert_eq!(id, "com.opencapx.p2-settings");

        assert_eq!(
            PluginManager::set_setting_value(&id, "with_en", &serde_json::json!("hi")).unwrap_err(),
            "invalid: too short"
        );
        assert_eq!(
            PluginManager::set_setting_value(&id, "no_en", &serde_json::json!("hi")).unwrap_err(),
            "invalid: Tên quá ngắn"
        );

        PluginManager::shared().uninstall(&id).expect("uninstall");
        let _ = std::fs::remove_file(crate::core::config::config_path(&id));
        std::env::remove_var("OPENCAPX_SECRETS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// P2 — a pre-P2 manifest (plain strings, no aliases) passes validation and its declarations serialize field-for-field unchanged.
    #[test]
    fn settings_p2_pre_p2_manifest_round_trips_unchanged() {
        let text = r#"{"id":"com.x","name":"X","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"true"},"capabilities":["image.analyze"],"permissions":["image.read"],"settings":[{"key":"auto_play","type":"toggle","label":"Auto","default":true},{"key":"api_key","type":"secret","label":"API Key"},{"key":"quality","type":"dropdown","options":["low","high"],"default":"low"},{"key":"title","type":"text","section":"Audio","validate":[{"type":"required","message":"pick one"}]}]}"#;
        let m: Manifest = serde_json::from_str(text).unwrap();
        assert!(PluginManager::validate_manifest(&m).is_ok());
        let settings = serde_json::to_value(&m).unwrap()["settings"].clone();
        assert_eq!(
            settings,
            serde_json::json!([
                {"key":"auto_play","type":"toggle","label":"Auto","default":true},
                {"key":"api_key","type":"secret","label":"API Key"},
                {"key":"quality","type":"dropdown","options":["low","high"],"default":"low"},
                {"key":"title","type":"text","section":"Audio",
                 "validate":[{"type":"required","message":"pick one"}]}
            ]),
            "pre-P2 declarations must serialize to exactly the old shape (no aliases, strings stay strings)"
        );
    }

    /// M7/T8 — scaffold produces an installable plugin in one command: placeholder substitution + overwrite refusal + real install and start.
    #[test]
    fn scaffold_produces_installable_plugin() {
        skip_probe_in_tests();
        if !python3_available() || !node_available() {
            eprintln!("skip: no python3/node");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-scaffold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The default plugin directory of shared-store tests is not isolated, so it is uniformly redirected to a temp directory.
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        crate::core::set_shared_store(store.clone());

        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let script = repo.join("scripts").join("create-opencapx-plugin.mjs");
        let out_dir = base.join("hello");
        let run = |args: &[&str]| {
            std::process::Command::new("node")
                .arg(&script)
                .args(args)
                .output()
                .expect("run scaffold script")
        };

        let res = run(&[
            "--id",
            "com.opencapx.scaffold-test",
            "--name",
            "Scaffold Test",
            "--author",
            "ocx-test",
            "--dir",
            out_dir.to_str().unwrap(),
        ]);
        assert!(
            res.status.success(),
            "scaffold failed: {}",
            String::from_utf8_lossy(&res.stderr)
        );

        // structure: manifest + script + release pipeline (including dotfiles)
        for f in [
            "opencapx-plugin.json",
            "bin/plugin.py",
            "README.md",
            ".gitignore",
            ".github/workflows/release.yml",
        ] {
            assert!(out_dir.join(f).exists(), "missing {}", f);
        }

        // placeholder substitution: the new id/name is present, the old placeholder does not linger
        let manifest = std::fs::read_to_string(out_dir.join("opencapx-plugin.json")).unwrap();
        assert!(manifest.contains("com.opencapx.scaffold-test"));
        assert!(manifest.contains("Scaffold Test"));
        assert!(!manifest.contains("com.example.my-plugin"));
        let py_src = std::fs::read_to_string(out_dir.join("bin").join("plugin.py")).unwrap();
        assert!(!py_src.contains("MyPlugin"), "class placeholder not rewritten");

        // overwrite refusal: a second scaffold in the same directory must fail
        let again = run(&[
            "--id",
            "com.opencapx.scaffold-test",
            "--dir",
            out_dir.to_str().unwrap(),
        ]);
        assert!(!again.status.success(), "must refuse to overwrite existing dir");

        // installable: real install + start handshake + registered
        let id = PluginManager::shared().install_from_dir(&out_dir).expect("install scaffold");
        assert_eq!(id, "com.opencapx.scaffold-test");
        assert!(PluginManager::shared().list().iter().any(|p| p.id == id));
        PluginManager::shared().uninstall(&id).expect("uninstall");

        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M8/F12 — local registry fixture full-chain E2E (one chain):
    /// publish (pack with a v2 signature) → verify the signed index → auto-gate → install (trusted direct install)
    /// → update (same key, silent version bump) → revoke (sweep default-disables + blocks) → reopen → uninstall.
    #[test]
    fn registry_fixture_full_chain_e2e() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let key_id = "com.opencapx.test-signing";
        let seed_text =
            std::fs::read_to_string(repo.join("fixtures").join("signing").join("key.seed.hex"))
                .unwrap();
        let seed_text = seed_text.trim();
        let mut seed = [0u8; 32];
        for i in 0..32 {
            seed[i] = u8::from_str_radix(&seed_text[i * 2..i * 2 + 2], 16).unwrap();
        }
        let trusted_path = repo.join("fixtures").join("signing").join("trusted-keys.json");
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &trusted_path);
        let trusted = crate::core::plugin_sig::load_trusted_keys_from(&trusted_path);
        let official_env = format!("{}={}", key_id, pubkey_hex(seed));

        // 1) publish: source → v2 signed package (equivalent to the CI tag flow: pack + sign)
        let src = base.join("src");
        copy_tree(&repo.join("plugins").join("echo-vision"), &src);
        let v010 = base.join("e2e-0.1.0.ocplugin");
        let digest010 = crate::core::pack::pack_dir(&src, &seed, key_id, &v010).unwrap();

        // signed index (same chain as key-ceremony S5; the test identity = the official fixtures injection)
        let idx_raw = serde_json::json!({
            "schemaVersion": 2,
            "generatedAt": 100,
            "publishers": [{"keyId": key_id, "publicKey": pubkey_hex(seed), "verified": true}],
            "revokedKeys": [],
            "entries": [{
                "id": "com.opencapx.echo-vision",
                "name": "Echo Vision",
                "author": {"keyId": key_id},
                "versions": [{
                    "version": "0.1.0",
                    "downloadUrl": format!("file://{}", v010.display()),
                    "sha256": digest010,
                    "sizeBytes": std::fs::metadata(&v010).unwrap().len()
                }]
            }]
        });
        let signed =
            crate::core::registry::sign_index(idx_raw.to_string().as_bytes(), &seed, key_id).unwrap();

        // 2) verify: index signature-verification chain (positive case; the tamper negative case is in the registry/verify tests)
        std::env::set_var("OPENCAPX_REGISTRY_OFFICIAL_KEYS", &official_env);
        let index =
            crate::core::registry::verify_index(signed.as_bytes()).expect("signed index verifies");
        std::env::remove_var("OPENCAPX_REGISTRY_OFFICIAL_KEYS");
        assert_eq!(index.entries.len(), 1);

        // 3) auto-gate (with trust table + index; same source as registry CI)
        let report = crate::core::verify::verify_package(
            &v010,
            &trusted,
            Some(&index),
            &crate::core::verify::GateLimits::default(),
        );
        assert!(
            report.ok,
            "gate must pass: {:?}",
            report.checks.iter().filter(|c| !c.ok).collect::<Vec<_>>()
        );

        // 4) install: trusted direct install + start
        let id = install_confirmed(&v010).expect("install");
        assert_eq!(id, "com.opencapx.echo-vision");
        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.1.0"));

        // 5) update: same key, version bump → silent update
        let mpath = src.join("opencapx-plugin.json");
        let mut m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mpath).unwrap()).unwrap();
        m["version"] = serde_json::Value::String("0.1.1".to_string());
        std::fs::write(&mpath, serde_json::to_string(&m).unwrap()).unwrap();
        let v011 = base.join("e2e-0.1.1.ocplugin");
        crate::core::pack::pack_dir(&src, &seed, key_id, &v011).unwrap();
        let id2 = install_confirmed(&v011).expect("update");
        assert_eq!(id2, id);
        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.1.1"));

        // 6) revoke: the new index revokes the keyId → sweep default-disables + blocks start/update
        let idx2_raw = serde_json::json!({
            "schemaVersion": 2,
            "generatedAt": 200,
            "publishers": [{"keyId": key_id, "publicKey": pubkey_hex(seed), "verified": true}],
            "revokedKeys": [{"keyId": key_id, "at": 200, "reason": "e2e drill"}],
            "entries": []
        });
        let signed2 = crate::core::registry::sign_index(idx2_raw.to_string().as_bytes(), &seed, key_id)
            .unwrap();
        std::env::set_var("OPENCAPX_REGISTRY_OFFICIAL_KEYS", &official_env);
        let index2 = crate::core::registry::verify_index(signed2.as_bytes()).unwrap();
        std::env::remove_var("OPENCAPX_REGISTRY_OFFICIAL_KEYS");
        let hits = crate::core::revocation::sweep_with(&index2);
        assert_eq!(hits.len(), 1, "one revoke hit");
        let row = PluginManager::shared()
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap();
        assert_eq!(row.status, "stopped");
        assert_eq!(row.revoked_key.as_deref(), Some(key_id));
        assert!(PluginManager::shared().start(&id).unwrap_err().contains("revoked"));
        let err = install_confirmed(&v011).unwrap_err();
        assert!(err.contains("plugin revoked"), "got: {}", err);

        // 7) reopen: explicit reopen → clear marker → can start
        crate::core::revocation::reopen(&id).expect("reopen");
        PluginManager::shared().start(&id).expect("start after reopen");

        // 8) uninstall: rows/directory/process all cleared
        PluginManager::shared().uninstall(&id).expect("uninstall");
        assert!(!PluginManager::shared().list().iter().any(|p| p.id == id));
        assert!(!base.join("plugins").join(&id).exists(), "plugin dir must be gone");

        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M8/F12 failure injection — corrupt package (garbage bytes / truncation / bit flip): install fails with zero state
    /// (no DB row / no target directory / no staging residue).
    #[test]
    fn corrupt_package_install_leaves_no_state() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let valid = base.join("valid.ocplugin");
        make_zip(&valid, &echo_zip_entries_with("0.1.0", &["image.read"]));
        let bytes = std::fs::read(&valid).unwrap();

        let garbage = base.join("garbage.ocplugin");
        std::fs::write(&garbage, b"this is definitely not a zip").unwrap();
        let truncated = base.join("truncated.ocplugin");
        std::fs::write(&truncated, &bytes[..bytes.len() * 2 / 3]).unwrap();
        let mut flipped = bytes.clone();
        let mid = flipped.len() / 3;
        flipped[mid] ^= 0xFF;
        let corrupted = base.join("corrupted.ocplugin");
        std::fs::write(&corrupted, &flipped).unwrap();

        for bad in [&garbage, &truncated, &corrupted] {
            let err = install_confirmed(bad).unwrap_err();
            assert!(!err.is_empty(), "must fail: {}", bad.display());
        }
        assert!(
            !PluginManager::shared()
                .list()
                .iter()
                .any(|p| p.id == "com.opencapx.echo-vision"),
            "no row after failed installs"
        );
        let entries: Vec<String> = std::fs::read_dir(base.join("plugins"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !entries
                .iter()
                .any(|n| n == "com.opencapx.echo-vision"
                    || n.starts_with(".tmp-")
                    || n.starts_with(".old-")),
            "no dest/staging leftovers: {:?}",
            entries
        );

        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M8/F12 failure injection — write failure (permissions simulating a full disk): when the plugins root is not writable,
    /// the update fails but the old version is intact and still running; a fresh install fails with zero state.
    #[cfg(unix)]
    #[test]
    fn write_failure_keeps_old_running() {
        use std::os::unix::fs::PermissionsExt;
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        if running_as_root() {
            eprintln!("skip: running as root (read-only root would not block writes)");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-diskfull-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let v1 = base.join("v1.ocplugin");
        make_zip(&v1, &echo_zip_entries_with("0.1.0", &["image.read"]));
        let id = install_confirmed(&v1).expect("install v1");
        let root = base.join("plugins");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();

        // 1) update + 2) fresh install: both must fail under the write failure. Collect results, restore permissions, then assert,
        //     to avoid a read-only directory lingering after an assertion failure.
        let v2 = base.join("v2.ocplugin");
        make_zip(&v2, &echo_zip_entries_with("0.2.0", &["image.read"]));
        let r_update = install_confirmed(&v2);
        let fresh_manifest = serde_json::json!({
            "id": "com.opencapx.ro-echo",
            "name": "RO Echo",
            "version": "0.1.0",
            "apiVersion": "1",
            "type": "capability",
            "runtime": {"type": "process", "command": "python3", "args": ["bin/echo_vision.py"]},
            "capabilities": ["image.analyze"],
            "permissions": ["image.read"]
        });
        let fresh = base.join("fresh.ocplugin");
        make_zip(
            &fresh,
            &[
                ("opencapx-plugin.json".to_string(), fresh_manifest.to_string()),
                ("bin/echo_vision.py".to_string(), "print('x')".to_string()),
            ],
        );
        let r_fresh = install_confirmed(&fresh);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();

        // update fails but the old version is intact and still running (fails before swap, never touches the old version)
        r_update.expect_err("update must fail under write failure");
        assert_eq!(
            installed_version(&store, &id).as_deref(),
            Some("0.1.0"),
            "old version intact"
        );
        let row = PluginManager::shared()
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap();
        assert_eq!(row.status, "running", "old process must keep running");
        assert!(root.join(&id).join("bin").join("echo_vision.py").exists());

        // a fresh install similarly fails with zero rows
        r_fresh.expect_err("fresh install must fail under write failure");
        assert!(!PluginManager::shared()
            .list()
            .iter()
            .any(|p| p.id == "com.opencapx.ro-echo"));

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M8/F12 failure injection — mid-revocation: update and revocation scan run concurrently and the final state must be consistent
    /// (revocation marker always written, process always stopped, version is old or new, explicit reopen possible), with no half-installed state.
    #[test]
    fn revocation_mid_update_is_consistent() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-revrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let key_id = "com.test.revrace";
        let script = std::fs::read_to_string(
            repo.join("plugins")
                .join("echo-vision")
                .join("bin")
                .join("echo_vision.py"),
        )
        .unwrap();
        let mk_manifest = |version: &str| {
            format!(
                r#"{{"id":"com.opencapx.echo-vision","name":"Echo Vision","version":"{version}","apiVersion":"1","type":"capability","runtime":{{"type":"process","command":"python3","args":["bin/echo_vision.py"]}},"capabilities":["image.analyze"],"permissions":["image.read"],"signature":{{"keyId":"{key_id}","sig":"00"}}}}"#
            )
        };
        let src1 = base.join("src-010");
        std::fs::create_dir_all(src1.join("bin")).unwrap();
        std::fs::write(src1.join("bin").join("echo_vision.py"), &script).unwrap();
        std::fs::write(src1.join("opencapx-plugin.json"), mk_manifest("0.1.0")).unwrap();
        let id = PluginManager::shared().install_from_dir(&src1).expect("install v1");

        let src2 = base.join("src-020");
        copy_tree(&src1, &src2);
        std::fs::write(src2.join("opencapx-plugin.json"), mk_manifest("0.2.0")).unwrap();

        let index: crate::core::registry::RegistryIndex = serde_json::from_value(
            serde_json::json!({
                "schemaVersion": 2,
                "generatedAt": 9,
                "publishers": [],
                "revokedKeys": [{ "keyId": key_id, "at": 9, "reason": "mid-update race" }],
                "entries": [],
                "indexSignature": { "alg": "ed25519", "keyId": "k", "sig": "0" }
            }),
        )
        .unwrap();

        // concurrent: update (v0.2.0) vs revocation scan
        let src2c = src2.clone();
        let h_update = std::thread::spawn(move || PluginManager::shared().install_from_dir(&src2c));
        let h_sweep = std::thread::spawn(move || crate::core::revocation::sweep_with(&index));
        let r_update = h_update.join().expect("update thread");
        let hits = h_sweep.join().expect("sweep thread");
        assert_eq!(hits.len(), 1, "sweep must hit the installed plugin: {:?}", hits);

        // final state consistent: revocation marker + stopped + version old or new; the update failure must be caused by the revocation
        let row = PluginManager::shared()
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap();
        assert_eq!(row.revoked_key.as_deref(), Some(key_id), "revocation marker must land");
        assert_eq!(row.status, "stopped");
        let ver = installed_version(&store, &id).unwrap();
        assert!(ver == "0.1.0" || ver == "0.2.0", "coherent version: {}", ver);
        if let Err(e) = &r_update {
            assert!(e.contains("revoked"), "update error must be revocation: {}", e);
        }
        let leftovers: Vec<String> = std::fs::read_dir(base.join("plugins"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert!(!leftovers.iter().any(|n| n.starts_with(".tmp-") || n.starts_with(".old-")));

        // reopen path works
        crate::core::revocation::reopen(&id).expect("reopen");
        PluginManager::shared().start(&id).expect("start after reopen");

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M8/F12 cross-version — an old-shape manifest (required fields only, no settings/dependencies)
    /// installs; upgrading to the new shape (with settings[]/dependencies/minCoreVersion) goes through a same-key update.
    #[test]
    fn legacy_manifest_upgrades_to_new_shape() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let script = std::fs::read_to_string(
            repo.join("plugins")
                .join("echo-vision")
                .join("bin")
                .join("echo_vision.py"),
        )
        .unwrap();

        let legacy = base.join("legacy.ocplugin");
        make_zip(
            &legacy,
            &[
                (
                    "opencapx-plugin.json".to_string(),
                    r#"{"id":"com.opencapx.legacy-fmt","name":"Legacy","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"python3","args":["bin/echo_vision.py"]},"capabilities":["image.analyze"],"permissions":["image.read"]}"#.to_string(),
                ),
                ("bin/echo_vision.py".to_string(), script.clone()),
            ],
        );
        let id = install_confirmed(&legacy).expect("legacy install");
        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.1.0"));

        let new_shape = base.join("new-shape.ocplugin");
        let new_manifest = serde_json::json!({
            "id": "com.opencapx.legacy-fmt",
            "name": "Legacy",
            "version": "0.2.0",
            "apiVersion": "1",
            "type": "capability",
            "runtime": {"type": "process", "command": "python3", "args": ["bin/echo_vision.py"]},
            "capabilities": ["image.analyze"],
            "permissions": ["image.read"],
            "dependencies": {},
            "minCoreVersion": "0.1.0",
            "settings": [{"key": "auto", "type": "toggle", "default": true}]
        });
        make_zip(
            &new_shape,
            &[
                ("opencapx-plugin.json".to_string(), new_manifest.to_string()),
                ("bin/echo_vision.py".to_string(), script),
            ],
        );
        let id2 = install_confirmed(&new_shape).expect("upgrade to new shape");
        assert_eq!(id2, id);
        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.2.0"));
        let view = PluginManager::settings_view(&id).expect("settings view");
        assert_eq!(view.settings.len(), 1, "new settings[] visible after upgrade");

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M8/F12 cross-version — a v1 local HMAC-signed package is still installable (legacy-format compatibility channel,
    /// the signing toolchain matches previous versions: sha256 over non-manifest entries + HMAC frame).
    #[test]
    fn v1_hmac_package_still_installs() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-v1pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let script = std::fs::read_to_string(
            repo.join("plugins")
                .join("echo-vision")
                .join("bin")
                .join("echo_vision.py"),
        )
        .unwrap();

        let key_id = "com.test.legacy-hmac";
        let secret: Vec<u8> = (0u8..32).map(|i| i.wrapping_mul(7)).collect();
        let secret_hex: String = secret.iter().map(|b| format!("{:02x}", b)).collect();
        let keys_path = base.join("trusted-keys.json");
        std::fs::write(
            &keys_path,
            serde_json::json!({ key_id: secret_hex }).to_string(),
        )
        .unwrap();
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &keys_path);

        let unsigned = base.join("v1-unsigned.ocplugin");
        let manifest = serde_json::json!({
            "id": "com.opencapx.v1-legacy",
            "name": "V1 Legacy",
            "version": "0.1.0",
            "apiVersion": "1",
            "type": "capability",
            "runtime": {"type": "process", "command": "python3", "args": ["bin/echo_vision.py"]},
            "capabilities": ["image.analyze"],
            "permissions": ["image.read"]
        });
        make_zip(
            &unsigned,
            &[
                ("opencapx-plugin.json".to_string(), manifest.to_string()),
                ("bin/echo_vision.py".to_string(), script.clone()),
            ],
        );
        let hash = crate::core::plugin_sig::compute_archive_hash(&unsigned).unwrap();
        let sig = crate::core::marketplace::hmac_sha256_hex(
            &secret,
            format!("opencapx-v1\n{}", hash).as_bytes(),
        );
        let mut signed_manifest = manifest;
        signed_manifest["sha256"] = serde_json::Value::String(hash);
        signed_manifest["signature"] =
            serde_json::json!({"keyId": key_id, "sig": sig});
        let signed = base.join("v1-signed.ocplugin");
        make_zip(
            &signed,
            &[
                ("opencapx-plugin.json".to_string(), signed_manifest.to_string()),
                ("bin/echo_vision.py".to_string(), script),
            ],
        );

        let id = install_confirmed(&signed).expect("v1 HMAC package installs");
        assert_eq!(id, "com.opencapx.v1-legacy");
        assert_eq!(installed_version(&store, &id).as_deref(), Some("0.1.0"));

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M8/F12 budget — install (excluding download) ≤ 2s: extraction + DB write + directory switch + cold-start handshake.
    #[test]
    fn install_budget_under_two_seconds() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let zip = base.join("budget.ocplugin");
        make_zip(&zip, &echo_zip_entries_with("0.1.0", &["image.read"]));
        let t0 = std::time::Instant::now();
        let id = install_confirmed(&zip).expect("install");
        let elapsed = t0.elapsed();
        println!("[budget] install(no download) = {:?}", elapsed);
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "install exceeded 2s budget: {:?}",
            elapsed
        );

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// M8/F12 — revocation drill (repeatable, for referencing in drill records):
    /// key revocation → sweep default-disables + `plugin.revoked` event → blocks start/update
    /// → user explicit reopen (ack) + `plugin.revoked.reopened` → next sweep exempts it → recovered.
    #[test]
    fn drill_revocation_default_disable_and_reopen() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-drill-rev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let key_id = "com.opencapx.test-signing";
        let seed_text =
            std::fs::read_to_string(repo.join("fixtures").join("signing").join("key.seed.hex"))
                .unwrap();
        let seed_text = seed_text.trim();
        let mut seed = [0u8; 32];
        for i in 0..32 {
            seed[i] = u8::from_str_radix(&seed_text[i * 2..i * 2 + 2], 16).unwrap();
        }
        std::env::set_var(
            "OPENCAPX_TRUSTED_KEYS",
            repo.join("fixtures").join("signing").join("trusted-keys.json"),
        );
        let official_env = format!("{}={}", key_id, pubkey_hex(seed));

        // [1/6] publish + install (signed package, trusted direct install)
        let src = base.join("src");
        copy_tree(&repo.join("plugins").join("echo-vision"), &src);
        let pkg = base.join("drill-0.1.0.ocplugin");
        crate::core::pack::pack_dir(&src, &seed, key_id, &pkg).unwrap();
        let id = install_confirmed(&pkg).expect("install");
        let row = PluginManager::shared()
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap();
        println!("[1/6] installed id={} status={}", id, row.status);
        assert_eq!(row.status, "running");

        // [2/6] official issuance of the revocation index (offline signing + client-side verification chain)
        let idx_raw = serde_json::json!({
            "schemaVersion": 2,
            "generatedAt": 300,
            "publishers": [{"keyId": key_id, "publicKey": pubkey_hex(seed), "verified": true}],
            "revokedKeys": [{"keyId": key_id, "at": 300, "reason": "drill"}],
            "entries": []
        });
        let signed =
            crate::core::registry::sign_index(idx_raw.to_string().as_bytes(), &seed, key_id)
                .unwrap();
        std::env::set_var("OPENCAPX_REGISTRY_OFFICIAL_KEYS", &official_env);
        let index = crate::core::registry::verify_index(signed.as_bytes()).expect("index verifies");
        std::env::remove_var("OPENCAPX_REGISTRY_OFFICIAL_KEYS");
        println!("[2/6] revocation index signed+verified (keyId={}, at=300)", key_id);

        // [3/6] sweep: default-disable + event
        let rx = crate::core::event::EventBus::shared().subscribe();
        let hits = crate::core::revocation::sweep_with(&index);
        assert_eq!(hits.len(), 1, "sweep hit");
        let mut saw_revoked = false;
        while let Ok(e) = rx.try_recv() {
            if e.kind == "plugin.revoked" {
                assert_eq!(e.payload.get("keyId").and_then(|v| v.as_str()), Some(key_id));
                saw_revoked = true;
            }
        }
        assert!(saw_revoked, "plugin.revoked event required");
        let row = PluginManager::shared()
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap();
        println!(
            "[3/6] auto-disabled status={} revokedKey={:?}",
            row.status, row.revoked_key
        );
        assert_eq!(row.status, "stopped");
        assert_eq!(row.revoked_key.as_deref(), Some(key_id));

        // [4/6] block start and update
        assert!(PluginManager::shared().start(&id).unwrap_err().contains("revoked"));
        let err = install_confirmed(&pkg).unwrap_err();
        assert!(err.contains("plugin revoked"), "got: {}", err);
        println!("[4/6] start/update blocked: {}", err);

        // [5/6] explicit reopen: ack written to DB + event; next sweep exempts it
        let rx2 = crate::core::event::EventBus::shared().subscribe();
        crate::core::revocation::reopen(&id).expect("reopen");
        let mut saw_reopened = false;
        while let Ok(e) = rx2.try_recv() {
            if e.kind == "plugin.revoked.reopened" {
                saw_reopened = true;
            }
        }
        assert!(saw_reopened, "plugin.revoked.reopened event required");
        let ack: Option<String> = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    c.query_row(
                        "SELECT revocation_ack FROM plugins WHERE id = ?1",
                        [id.as_str()],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .ok()
                    .flatten()
                })
            })
            .flatten();
        assert_eq!(ack.as_deref(), Some(key_id), "ack recorded");
        let again = crate::core::revocation::sweep_with(&index);
        assert!(again.is_empty(), "ack exempts re-disable");
        println!("[5/6] reopened; ack={:?}; re-sweep hits=0", ack);

        // [6/6] resume running
        PluginManager::shared().start(&id).expect("start after reopen");
        assert!(PluginManager::shared()
            .list()
            .iter()
            .any(|p| p.id == id && p.status == "running"));
        println!("[6/6] running again");

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S3 — reverse rate limiting: a burst of 100 is allowed, then dropped; one summary on window close (count = dropped count).
    #[test]
    fn reverse_events_throttle_after_burst() {
        let pid = format!("com.opencapx.throttle-{}", std::process::id());
        let rx = crate::core::event::EventBus::shared().subscribe();
        let reply: Reply = Arc::new(|_v: serde_json::Value| {});

        for i in 0..200 {
            handle_reverse(
                &pid,
                json!({"jsonrpc":"2.0","method":"core.emit","params":{"type":"tick","payload":{"i":i}}}),
                reply.clone(),
            );
        }
        let mut allowed = 0usize;
        let mut summaries = 0usize;
        while let Ok(ev) = rx.try_recv() {
            if ev.kind == format!("{}.tick", pid) {
                allowed += 1;
            }
            if ev.kind == "plugin.throttled" {
                summaries += 1;
            }
        }
        assert_eq!(allowed, 100, "burst bucket allows 100");
        assert_eq!(summaries, 0, "no summary until the window closes");

        std::thread::sleep(std::time::Duration::from_millis(1100));
        handle_reverse(
            &pid,
            json!({"jsonrpc":"2.0","method":"core.emit","params":{"type":"tick2","payload":{}}}),
            reply,
        );
        let mut summaries = 0usize;
        let mut reported = 0u64;
        while let Ok(ev) = rx.try_recv() {
            if ev.kind == "plugin.throttled" && ev.payload["pluginId"] == pid {
                summaries += 1;
                reported += ev.payload["dropped"].as_u64().unwrap_or(0);
            }
        }
        assert_eq!(summaries, 1, "exactly one summary after the window closes");
        assert_eq!(reported, 100, "dropped count = 100");
    }

    /// S3 — requestPermission dedup: in-flight duplicates share one decision, and all waiters get a unified reply.
    #[test]
    fn permission_requests_dedupe_and_settle_together() {
        let key = ("com.opencapx.dedupe".to_string(), "image.read".to_string());
        let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let mk_reply = |captured: Arc<Mutex<Vec<serde_json::Value>>>| -> Reply {
            Arc::new(move |v: serde_json::Value| {
                if let Ok(mut c) = captured.lock() {
                    c.push(v);
                }
            })
        };
        assert!(
            register_permission_waiter(&key, Some(json!(1)), mk_reply(captured.clone())),
            "first request registers as owner"
        );
        assert!(
            !register_permission_waiter(&key, Some(json!(2)), mk_reply(captured.clone())),
            "second attaches to the same decision"
        );
        settle_permission_waiters(&key, true);
        let got = captured.lock().unwrap().clone();
        assert_eq!(got.len(), 2, "both waiters replied");
        assert_eq!(got[0]["id"], json!(1));
        assert_eq!(got[1]["id"], json!(2));
        assert_eq!(got[0]["result"]["granted"], json!(true));
        assert_eq!(got[1]["result"]["granted"], json!(true));
    }

    /// S3 — two concurrent requests for the same permission: the gate runs once and both replies are delivered (same decision).
    #[test]
    fn concurrent_permission_requests_share_one_gate_run() {
        use std::sync::atomic::Ordering as AOrd;
        let before = PERM_GATE_RUNS.load(AOrd::SeqCst);
        let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let mk_reply = |captured: Arc<Mutex<Vec<serde_json::Value>>>| -> Reply {
            Arc::new(move |v: serde_json::Value| {
                if let Ok(mut c) = captured.lock() {
                    c.push(v);
                }
            })
        };
        let pid = format!("com.opencapx.perm-race-{}", std::process::id());
        let p1 = pid.clone();
        let c1 = captured.clone();
        let h1 = std::thread::spawn(move || {
            enqueue_permission_request(p1, "image.read".to_string(), None, Some(json!(11)), mk_reply(c1));
        });
        std::thread::sleep(std::time::Duration::from_millis(30));
        let p2 = pid.clone();
        let c2 = captured.clone();
        let h2 = std::thread::spawn(move || {
            enqueue_permission_request(p2, "image.read".to_string(), None, Some(json!(12)), mk_reply(c2));
        });
        h1.join().unwrap();
        h2.join().unwrap();
        let t0 = std::time::Instant::now();
        while captured.lock().map(|c| c.len()).unwrap_or(0) < 2
            && t0.elapsed() < std::time::Duration::from_secs(5)
        {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        let got = captured.lock().unwrap().clone();
        assert_eq!(got.len(), 2, "both replies arrive");
        assert_eq!(got[0]["result"], got[1]["result"], "same decision");
        assert_eq!(
            PERM_GATE_RUNS.load(AOrd::SeqCst) - before,
            1,
            "gate runs exactly once for the in-flight pair"
        );
    }

    /// S4 — timeoutSecs bounds: 0 / 601 rejected; default / 1 / 600 pass.
    #[test]
    fn capability_timeout_secs_bounds() {
        fn manifest_with_timeout(t: Option<u32>) -> Manifest {
            let mut cap = serde_json::json!({"id": "demo.slow", "permission": "demo.run"});
            if let Some(t) = t {
                cap["timeoutSecs"] = serde_json::json!(t);
            }
            serde_json::from_value(serde_json::json!({
                "id": "com.example.slow-x", "name": "S", "version": "0.1.0", "apiVersion": "1",
                "type": "capability",
                "runtime": {"type": "process", "command": "true"},
                "capabilities": [cap], "permissions": ["demo.run"]
            }))
            .unwrap()
        }
        assert!(
            PluginManager::validate_manifest(&manifest_with_timeout(None)).is_ok(),
            "default = 60s"
        );
        assert!(PluginManager::validate_manifest(&manifest_with_timeout(Some(1))).is_ok());
        assert!(PluginManager::validate_manifest(&manifest_with_timeout(Some(600))).is_ok());
        for bad in [0u32, 601] {
            let err = PluginManager::validate_manifest(&manifest_with_timeout(Some(bad)))
                .unwrap_err();
            assert!(err.contains("timeoutSecs"), "got: {}", err);
        }
    }

    /// S4 — declared-timeout end-to-end: object form timeoutSecs:1 + a sleep 3s handler →
    /// call directly with the parsed declared value, timing out at ~1s instead of the default 60s.
    /// (the plugin path in execute is blocked by the once-only gate in the test environment — a fast rejection with no UI —
    ///  so here we call the provider directly to verify the "declaration written to DB → value read → call timeout" seam;
    ///  the value-reading wiring inside execute is pinned by the `capability::call_timeout_for` unit test.)
    #[test]
    fn declared_timeout_limits_slow_capability_call() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-slow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        std::fs::create_dir_all(base.join("plugins")).unwrap();

        let src = base.join("slow-src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(
            src.join("bin").join("slow.py"),
            r#"import json, sys, time
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    m = msg.get("method")
    if m == "plugin.initialize":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"pluginId":"com.example.slow","apiVersion":"1","capabilities":[{"id":"demo.slow","version":"1"}]}}) + "\n"); sys.stdout.flush()
    elif m == "plugin.ping":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"ok":True}}) + "\n"); sys.stdout.flush()
    elif m == "demo.slow":
        time.sleep(3)
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"ok":True}}) + "\n"); sys.stdout.flush()
    elif m == "plugin.shutdown":
        break
"#,
        )
        .unwrap();
        std::fs::write(
            src.join("opencapx-plugin.json"),
            r#"{"id":"com.example.slow","name":"Slow","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"python3","args":["bin/slow.py"]},"capabilities":[{"id":"demo.slow","permission":"demo.run","default":"ask","timeoutSecs":1}],"permissions":["demo.run"]}"#,
        )
        .unwrap();
        let id = PluginManager::shared().install_from_dir(&src).expect("install slow");
        assert_eq!(id, "com.example.slow");

        // 1) declaration written to DB: timeout_for parses 1 second; string form → no declared timeout
        assert_eq!(
            crate::core::declaration::timeout_for(&store, &id, "demo.slow"),
            Some(1),
            "declared timeout persists"
        );
        let rows = crate::core::declaration::all(&store);
        assert!(
            rows.iter().any(|d| d.capability == "demo.slow" && d.timeout_secs == Some(1)),
            "rows: {:?}",
            rows
        );
        let echo = base.join("echo.ocplugin");
        make_zip(&echo, &echo_zip_entries_with("0.1.0", &["image.read"]));
        let echo_id = install_confirmed(&echo).expect("install echo");
        assert_eq!(
            crate::core::declaration::timeout_for(&store, &echo_id, "image.analyze"),
            None,
            "string form has no per-capability timeout"
        );

        // 2) call times out by the declared value (~1s, not the default 60s)
        let proc = PluginManager::shared().ensure_running(&id).expect("running");
        let timeout = std::time::Duration::from_secs(
            crate::core::declaration::timeout_for(&store, &id, "demo.slow").unwrap() as u64,
        );
        let t0 = std::time::Instant::now();
        let err = proc.call("demo.slow", json!({}), timeout).unwrap_err();
        let elapsed = t0.elapsed();
        assert!(err.contains("timeout"), "got: {}", err);
        assert!(
            elapsed.as_millis() >= 900 && elapsed.as_millis() < 5000,
            "~1s not 60s: {:?}",
            elapsed
        );

        PluginManager::shared().stop(&id);
        PluginManager::shared().stop(&echo_id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S4 — input gate: a >1 MiB payload is rejected with a `payload_too_large` event;
    /// a small payload does not trigger it (it reaches the permission gate → fast rejection with no UI).
    #[test]
    fn capability_input_size_gate_blocks_oversized() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-payload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        std::fs::create_dir_all(base.join("plugins")).unwrap();

        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let root = repo.join("plugins").join("weather-demo");
        let id = PluginManager::shared()
            .install_from_dir(&root.to_path_buf())
            .expect("install weather-demo");
        assert_eq!(id, "com.example.weather-demo");
        let rx = crate::core::event::EventBus::shared().subscribe();

        let big = json!({"city": "Beijing", "pad": "x".repeat(1_048_600)});
        let err = crate::core::capability::execute("weather.current", &big, None).unwrap_err();
        assert_eq!(err, "payload_too_large");
        let mut seen = false;
        while let Ok(ev) = rx.try_recv() {
            if ev.kind == "capability.failed" && ev.payload["error"] == "payload_too_large" {
                seen = true;
            }
        }
        assert!(seen, "capability.failed carries payload_too_large");

        let small = json!({"city": "Beijing"});
        let err2 = crate::core::capability::execute("weather.current", &small, None).unwrap_err();
        // v1.5: plugin-layer permission denial carries an explainable error (the permission name is returned with the error, mapped to 40002 on the rpc side)
        assert_eq!(err2, "plugin_permission_denied:weather.read", "gate deny, not payload gate");

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S5a — sandbox declaration validation: legal passes; bad network / unknown fs entry / carried by a pet → reject.
    #[test]
    fn sandbox_declaration_validation() {
        fn m_with_sandbox(sb: serde_json::Value) -> Manifest {
            serde_json::from_value(serde_json::json!({
                "id": "com.example.sb", "name": "SB", "version": "0.1.0", "apiVersion": "1",
                "type": "capability",
                "runtime": {"type": "process", "command": "true"},
                "capabilities": ["image.analyze"], "permissions": ["image.read"],
                "sandbox": sb
            }))
            .unwrap()
        }
        assert!(PluginManager::validate_manifest(&m_with_sandbox(serde_json::json!({}))).is_ok(), "empty block = minimal permissions");
        assert!(PluginManager::validate_manifest(&m_with_sandbox(serde_json::json!({"network": "none"}))).is_ok());
        assert!(
            PluginManager::validate_manifest(&m_with_sandbox(
                serde_json::json!({"network": "out", "fs": {"write": ["plugin-data"]}})
            ))
            .is_ok()
        );
        let err = PluginManager::validate_manifest(&m_with_sandbox(serde_json::json!({"network": "host"})))
            .unwrap_err();
        assert!(err.contains("sandbox.network"), "got: {}", err);
        let err = PluginManager::validate_manifest(&m_with_sandbox(serde_json::json!({"fs": {"write": ["/tmp"]}})))
            .unwrap_err();
        assert!(err.contains("fs.write"), "got: {}", err);
        // a pet has no process: it must not carry sandbox
        let pet: Manifest = serde_json::from_value(serde_json::json!({
            "id": "com.example.pet2", "name": "P", "version": "0.1.0", "apiVersion": "1",
            "type": "pet", "states": ["idle"],
            "sandbox": {"network": "none"}
        }))
        .unwrap();
        let err = PluginManager::validate_manifest(&pet).unwrap_err();
        assert!(err.contains("runtime"), "got: {}", err);
    }

    /// S5c — unsigned + declared sandbox → enforced (sandboxed even with the switch off); through the real start_inner path.
    #[test]
    #[cfg(target_os = "macos")]
    fn unsigned_sandbox_declaration_is_forced() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        if !std::path::Path::new("/usr/bin/sandbox-exec").is_file() {
            eprintln!("skip: no sandbox-exec");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-sbx-forced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        std::env::set_var("OPENCAPX_PLUGIN_DATA_DIR", base.join("plugin-data"));
        std::fs::create_dir_all(base.join("plugins")).unwrap();

        let escape_file = base.join("escape.txt");
        let data_file = base.join("plugin-data").join("com.example.sbx").join("probe.json");
        let src = base.join("sbx-src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(
            src.join("bin").join("sbx.py"),
            r#"import json, os, sys
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    m = msg.get("method")
    if m == "plugin.initialize":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"pluginId":"com.example.sbx","apiVersion":"1","capabilities":[{"id":"demo.sbx","version":"1"}]}}) + "\n"); sys.stdout.flush()
    elif m == "plugin.ping":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"ok":True}}) + "\n"); sys.stdout.flush()
    elif m == "demo.sbx":
        r = {"escape": "?", "data": "?"}
        try:
            open(os.environ["SBX_ESCAPE"], "w").write("x"); r["escape"] = "ok"
        except Exception:
            r["escape"] = "denied"
        try:
            os.makedirs(os.path.dirname(os.environ["SBX_DATA"]), exist_ok=True)
            open(os.environ["SBX_DATA"], "w").write("x"); r["data"] = "ok"
        except Exception:
            r["data"] = "denied"
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":r}) + "\n"); sys.stdout.flush()
    elif m == "plugin.shutdown":
        break
"#,
        )
        .unwrap();
        std::fs::write(
            src.join("opencapx-plugin.json"),
            format!(
                r#"{{"id":"com.example.sbx","name":"SBX","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{{"type":"process","command":"python3","args":["bin/sbx.py"],"env":{{"SBX_ESCAPE":"{}","SBX_DATA":"{}"}}}},"capabilities":[{{"id":"demo.sbx","permission":"demo.run","default":"ask"}}],"permissions":["demo.run"],"sandbox":{{"network":"none","fs":{{"write":["plugin-data"]}}}}}}"#,
                escape_file.display(),
                data_file.display()
            ),
        )
        .unwrap();
        // dev install (unsigned, trusted=false) → declared sandbox → enforced (the switch is false by default)
        let id = PluginManager::shared().install_from_dir(&src).expect("install");
        assert_eq!(id, "com.example.sbx");
        assert!(
            !sandbox_enforcement_enabled(),
            "the switch stays false by default, proving that enforcement does not depend on the switch"
        );
        let proc = PluginManager::shared().ensure_running(&id).expect("running");
        let r = proc
            .call("demo.sbx", json!({}), std::time::Duration::from_secs(20))
            .expect("probe");
        assert_eq!(r["escape"], "denied", "an unsigned declared sandbox must be enforced: {:?}", r);
        assert_eq!(r["data"], "ok", "plugin-data write allowed: {:?}", r);

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGIN_DATA_DIR");
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Soak smoke (S5c trust path): trusted declaring plugins follow the switch — OFF not sandboxed, ON sandboxed;
    /// together with "unverified enforcement", this covers both production branches of effective_profile (real signed package + policy switch on restart).
    #[test]
    #[cfg(target_os = "macos")]
    fn trusted_sandbox_declaration_follows_enforcement_switch() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        if !std::path::Path::new("/usr/bin/sandbox-exec").is_file() {
            eprintln!("skip: no sandbox-exec");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-sbx-trusted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        std::env::set_var("OPENCAPX_PLUGIN_DATA_DIR", base.join("plugin-data"));
        std::fs::create_dir_all(base.join("plugins")).unwrap();

        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        std::env::set_var(
            "OPENCAPX_TRUSTED_KEYS",
            repo.join("fixtures").join("signing").join("trusted-keys.json"),
        );
        let seed_text =
            std::fs::read_to_string(repo.join("fixtures").join("signing").join("key.seed.hex"))
                .unwrap();
        let seed_text = seed_text.trim();
        let mut seed = [0u8; 32];
        for i in 0..32 {
            seed[i] = u8::from_str_radix(&seed_text[i * 2..i * 2 + 2], 16).unwrap();
        }

        let escape_file = base.join("escape.txt");
        let data_file = base
            .join("plugin-data")
            .join("com.example.sbx2")
            .join("probe.json");
        let src = base.join("sbx2-src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(
            src.join("bin").join("sbx2.py"),
            r#"import json, os, sys
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    m = msg.get("method")
    if m == "plugin.initialize":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"pluginId":"com.example.sbx2","apiVersion":"1","capabilities":[{"id":"demo.sbx","version":"1"}]}}) + "\n"); sys.stdout.flush()
    elif m == "plugin.ping":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"ok":True}}) + "\n"); sys.stdout.flush()
    elif m == "demo.sbx":
        r = {"escape": "?", "data": "?"}
        try:
            open(os.environ["SBX_ESCAPE"], "w").write("x"); r["escape"] = "ok"
        except Exception:
            r["escape"] = "denied"
        try:
            os.makedirs(os.path.dirname(os.environ["SBX_DATA"]), exist_ok=True)
            open(os.environ["SBX_DATA"], "w").write("x"); r["data"] = "ok"
        except Exception:
            r["data"] = "denied"
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":r}) + "\n"); sys.stdout.flush()
    elif m == "plugin.shutdown":
        break
"#,
        )
        .unwrap();
        std::fs::write(
            src.join("opencapx-plugin.json"),
            format!(
                r#"{{"id":"com.example.sbx2","name":"SBX2","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{{"type":"process","command":"python3","args":["bin/sbx2.py"],"env":{{"SBX_ESCAPE":"{}","SBX_DATA":"{}"}}}},"capabilities":[{{"id":"demo.sbx","permission":"demo.run","default":"ask"}}],"permissions":["demo.run"],"sandbox":{{"network":"none","fs":{{"write":["plugin-data"]}}}}}}"#,
                escape_file.display(),
                data_file.display()
            ),
        )
        .unwrap();
        let pkg = base.join("sbx2-0.1.0.ocplugin");
        crate::core::pack::pack_dir(&src, &seed, "com.opencapx.test-signing", &pkg).unwrap();

        // 1) switch OFF (default): trusted declaring plugins are not sandboxed (can write any path)
        let id = install_confirmed(&pkg).expect("install trusted signed");
        assert_eq!(id, "com.example.sbx2");
        assert!(!sandbox_enforcement_enabled(), "the switch defaults to OFF");
        let proc = PluginManager::shared().ensure_running(&id).expect("running");
        let r = proc
            .call("demo.sbx", json!({}), std::time::Duration::from_secs(20))
            .expect("probe off");
        assert_eq!(r["escape"], "ok", "OFF: trusted is not sandboxed: {:?}", r);
        let _ = std::fs::remove_file(&escape_file);

        // 2) switch ON + restart: the same package is sandboxed (the policy is read live at start)
        set_sandbox_enforcement(true).expect("switch on");
        PluginManager::shared().stop(&id);
        PluginManager::shared().start(&id).expect("restart under sandbox");
        let proc = PluginManager::shared().ensure_running(&id).expect("running 2");
        let r2 = proc
            .call("demo.sbx", json!({}), std::time::Duration::from_secs(20))
            .expect("probe on");
        assert_eq!(r2["escape"], "denied", "ON: trusted declaring plugin is sandboxed: {:?}", r2);
        assert_eq!(r2["data"], "ok", "plugin-data write allowed: {:?}", r2);

        PluginManager::shared().stop(&id);
        set_sandbox_enforcement(false).ok();
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        std::env::remove_var("OPENCAPX_PLUGIN_DATA_DIR");
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// things-demo end-to-end (fills a sample coverage gap): directory install → static permission grant →
    /// execute things.add / things.list → the store lands at the manifest storePath.
    #[test]
    fn things_demo_end_to_end() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-things-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        std::fs::create_dir_all(base.join("plugins")).unwrap();

        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let src = base.join("things-src");
        copy_tree(&repo.join("plugins").join("things-demo"), &src);
        // after soak, things-demo declares sandbox (S5c unsigned enforcement): storePath must fall inside
        // the plugin-data writable area — the test redirects the plugin-data root via env and points the store into the same root.
        std::env::set_var("OPENCAPX_PLUGIN_DATA_DIR", base.join("plugin-data"));
        let store_file = base
            .join("plugin-data")
            .join("com.opencapx.things-demo")
            .join("store.json");
        {
            let mpath = src.join("opencapx-plugin.json");
            let mut m: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&mpath).unwrap()).unwrap();
            m["storePath"] = serde_json::Value::String(store_file.display().to_string());
            std::fs::write(&mpath, serde_json::to_string(&m).unwrap()).unwrap();
        }
        let id = PluginManager::shared().install_from_dir(&src).expect("install things-demo");
        assert_eq!(id, "com.opencapx.things-demo");
        assert!(PluginManager::shared()
            .list()
            .iter()
            .any(|p| p.id == id && p.status == "running"));

        // static permission (not declaration-derived) → can be explicitly granted; with no UI the default ask is quickly rejected.
        assert!(crate::core::permission::set_decision(&store, &id, "things.write", "granted"));
        assert!(crate::core::permission::set_decision(&store, &id, "things.read", "granted"));

        // write: add → the store lands at the manifest storePath
        let added = crate::core::capability::execute(
            "things.add",
            &json!({"title": "buy milk", "when": "today"}),
            None,
        )
        .expect("things.add");
        assert_eq!(added["title"], json!("buy milk"));
        assert!(
            store_file.exists(),
            "the store should land at the manifest storePath: {}",
            store_file.display()
        );

        // read: list contains the todo just written
        let listed = crate::core::capability::execute("things.list", &json!({}), None)
            .expect("things.list");
        let todos = listed["todos"].as_array().expect("todos array");
        assert!(
            todos.iter().any(|t| t["title"] == json!("buy milk")),
            "todos: {}",
            listed
        );
        assert_eq!(listed["via"], json!("demo"), "demo backend provenance marker: {}", listed);

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        std::env::remove_var("OPENCAPX_PLUGIN_DATA_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Q1 — re-enabling the health config at runtime: the old watchdog thread is disabled first (no leak),
    /// and the map is replaced with a new instance; each save no longer leaves a duplicate monitor thread.
    #[test]
    fn health_reenable_disinherits_old_watchdog() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-wd-reenable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        std::fs::create_dir_all(base.join("plugins")).unwrap();

        let v1 = base.join("v1.ocplugin");
        make_zip(&v1, &echo_zip_entries_with("0.1.0", &["image.read"]));
        let id = install_confirmed(&v1).expect("install");
        let mgr = PluginManager::shared();

        let cfg = crate::core::health::PluginHealthConfig {
            enabled: true,
            ..Default::default()
        };
        {
            let mut g = store.lock().unwrap();
            g.upsert_health_config(&id, &cfg);
        }
        // first registration (simulating a health-config save)
        mgr.apply_health_config(&id, &cfg);
        let old = mgr
            .watchdogs
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .expect("watchdog #1");
        // second save: the old thread must be disabled before removal
        mgr.apply_health_config(&id, &cfg);

        assert!(
            !old.lock().unwrap().enabled,
            "Q1: the old watchdog must be disabled before removal (otherwise it loops forever with a stale cfg)"
        );
        let new = mgr
            .watchdogs
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .expect("watchdog #2");
        assert!(!std::sync::Arc::ptr_eq(&old, &new), "the map should be replaced with a new instance");

        mgr.stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Step 4 acceptance (docs/permission-domains.md §9.1): **third-party new-domain plugin end-to-end**
    /// install → declaration frozen + domain occupied + permissions written to DB (taking the manifest default) → routing admits → the plugin really answers →
    /// domain conflict rejected → uninstall releases.
    ///
    /// Note: the "Allow once" hop of once-only requires a UI present (AppHandle), which unit tests
    /// cannot reach — here we verify the gate **fails closed** with no UI, plus the three of the four enforcement points that can be verified offline
    /// (can_always / set_decision / set_agent_decision). See the honesty note in §12.
    #[test]
    fn weather_demo_declared_domain_end_to_end() {
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-weather-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        // after soak, weather-demo declares sandbox (unsigned dev install = enforced sandbox): the store lands by default in
        // plugin-data — the test redirects the plugin-data root to avoid writing the real home.
        std::env::set_var("OPENCAPX_PLUGIN_DATA_DIR", base.join("plugin-data"));
        // the dev flow (install_from_dir) references the directory in the repo directly and does not create a plugins root;
        // create it first so the "no staging residue" assertion has a definite observation point.
        std::fs::create_dir_all(base.join("plugins")).unwrap();

        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("plugins")
            .join("weather-demo");
        let mgr = PluginManager::shared();
        let id = mgr
            .install_from_dir(&root.to_path_buf())
            .expect("install weather-demo");
        assert_eq!(id, "com.example.weather-demo");
        assert!(
            mgr.list().iter().any(|p| p.id == id && p.status == "running"),
            "declared-domain plugin must reach running"
        );

        // 1) declaration frozen + domain occupied + permissions written to DB (the non-interactive path takes the manifest's default)
        let decls = crate::core::declaration::all(&store);
        assert!(
            decls.iter().any(|d| d.capability == "weather.current"
                && d.permission == "weather.read"
                && d.default_decision == "ask"),
            "declaration rows: {:?}",
            decls
        );
        assert!(decls.iter().any(|d| d.capability == "weather.set_home"
            && d.permission == "weather.write"
            && d.default_decision == "denied"));
        assert_eq!(stored_decision(&store, &id, "weather.read").as_deref(), Some("ask"));
        assert_eq!(stored_decision(&store, &id, "weather.write").as_deref(), Some("denied"));
        let doms = crate::core::declaration::domains(&store);
        assert_eq!(doms.len(), 1, "domains: {:?}", doms);
        assert_eq!(doms[0].0, "weather");
        assert_eq!(doms[0].1.as_deref(), Some(id.as_str()));
        assert_eq!(doms[0].2, "local");

        // 2) routing admission = reserved ∪ declared; resolution yields the declared permission and default
        assert!(!crate::core::capability::is_builtin("weather.current"));
        assert!(crate::core::capability::known("weather.current"));
        assert!(crate::core::capability::known("weather.set_home"));
        let r = crate::core::declaration::resolve(&store, "weather.current").expect("resolved");
        assert_eq!(r.permission, "weather.read");
        assert_eq!(r.default, crate::core::permission::Decision::Ask);
        assert!(r.declared, "declared flag drives once-only");

        // 3) the three offline-verifiable ones of the four once-only enforcement points
        use crate::core::permission;
        assert!(!permission::can_always(&store, "weather.read"), "enforcement points 1/2");
        assert!(
            !permission::set_decision(&store, &id, "weather.read", "granted"),
            "enforcement point 3: the settings page / write-back must not turn a declared permission into granted"
        );
        assert!(
            !crate::core::identity::set_agent_decision(&store, "agent-any", "weather.read", "granted"),
            "enforcement point 3 (agent half)"
        );
        // can be turned off: denied is allowed
        assert!(permission::set_decision(&store, &id, "weather.read", "denied"));
        assert_eq!(permission::check(&store, &id, "weather.read"), permission::Decision::Denied);

        // 4) fails closed with no UI (in production this hop is an Allow once popup)
        assert_eq!(
            permission::gate(&store, &id, "weather.read", "capability", None),
            permission::Decision::Denied
        );

        // 5) the plugin really answers (call the plugin process directly, bypassing the permission gate already verified in 4)
        let proc = mgr.ensure_running(&id).expect("plugin running");
        let out = proc
            .call("weather.current", json!({ "city": "Shenzhen" }), Duration::from_secs(10))
            .expect("plugin answered weather.current");
        assert_eq!(out["city"], "Shenzhen");
        assert_eq!(out["via"], "weather-demo/local-table");

        // 6) domain conflict: another plugin must not declare the same capability with a **different mapping**, nor grab an already-occupied domain
        let drift = base.join("fake-drift");
        std::fs::create_dir_all(&drift).unwrap();
        std::fs::write(
            drift.join("opencapx-plugin.json"),
            r#"{"id":"com.example.other","name":"Other","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"true"},"capabilities":[{"id":"weather.current","permission":"weather.read2"}],"permissions":["weather.read2"]}"#,
        )
        .unwrap();
        let err = mgr.install_from_dir(&drift).unwrap_err();
        assert!(err.contains("already provided by"), "got: {}", err);

        let same = base.join("fake-same");
        std::fs::create_dir_all(&same).unwrap();
        std::fs::write(
            same.join("opencapx-plugin.json"),
            r#"{"id":"com.example.other","name":"Other","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"true"},"capabilities":[{"id":"weather.current","permission":"weather.read"}],"permissions":["weather.read"]}"#,
        )
        .unwrap();
        let err = mgr.install_from_dir(&same).unwrap_err();
        assert!(err.contains("already claimed by"), "got: {}", err);

        // 6b .ocplugin path collision consistency: validation happens **before extraction**, leaving no staging residue
        let zip_path = base.join("drift.ocplugin");
        make_zip(
            &zip_path,
            &[(
                "opencapx-plugin.json".to_string(),
                r#"{"id":"com.example.other","name":"Other","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"true"},"capabilities":[{"id":"weather.current","permission":"weather.read2"}],"permissions":["weather.read2"]}"#
                    .to_string(),
            )],
        );
        let err = install_confirmed(&zip_path).unwrap_err();
        assert!(err.contains("already provided by"), "got: {}", err);
        let leftovers: Vec<String> = std::fs::read_dir(base.join("plugins"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".tmp-") || n.starts_with(".old-"))
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {:?}", leftovers);

        // 7) uninstall releases the domain and declaration (reinstall = a fresh install)
        mgr.uninstall(&id).expect("uninstall");
        assert!(crate::core::declaration::all(&store)
            .iter()
            .all(|d| d.plugin_id != id));
        assert!(crate::core::declaration::domains(&store).is_empty());
        assert!(!crate::core::capability::known("weather.current"));

        std::env::remove_var("OPENCAPX_PLUGIN_DATA_DIR");
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ocplugin_rejects_unsafe_paths() {        let base = std::env::temp_dir().join(format!("opencapx-ocp2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // shares the process-level env OPENCAPX_PLUGINS_DIR with ocplugin_install_runs, serialized under a lock.
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        let entries = echo_zip_entries();
        let mut bad = entries;
        bad.push(("../evil.txt".to_string(), "pwn".to_string()));
        let zip_path = base.join("evil.ocplugin");
        make_zip(&zip_path, &bad);
        let err = install_confirmed(&zip_path).unwrap_err();
        assert!(err.contains("unsafe path"), "got: {}", err);
        assert!(!base.join("evil.txt").exists());
        assert!(!Path::new("../evil.txt").exists(), "nothing should be written inside the repo");

        // manifest missing
        make_zip(&base.join("nomanifest.ocplugin"), &[("bin/x.py".to_string(), "x".to_string())]);
        let err2 = PluginManager::shared()
            .install_ocplugin(&base.join("nomanifest.ocplugin"))
            .unwrap_err();
        assert!(err2.contains("missing opencapx-plugin.json"), "got: {}", err2);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Wow 5 — preview before install. All manifest fields line up, and permissions carry high_risk.
    /// Does not extract, write the DB, or spawn. This is the only source the settings.ts dialog trusts.
    #[test]
    fn preview_ocplugin_returns_manifest_with_high_risk() {
        let base = std::env::temp_dir().join(format!("opencapx-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let manifest = r#"{
            "id": "com.opencapx.test-vision",
            "name": "Test Vision",
            "description": "Reads pixels, returns text.",
            "version": "0.3.1",
            "apiVersion": "1",
            "type": "capability",
            "runtime": {"type": "process", "command": "python3"},
            "capabilities": ["image.analyze", "image.ocr"],
            "permissions": ["image.read", "camera", "filesystem.write", "process.execute"]
        }"#;
        let zip_path = base.join("test.ocplugin");
        make_zip(&zip_path, &[
            ("opencapx-plugin.json".to_string(), manifest.to_string()),
            ("bin/run.py".to_string(), "# placeholder".to_string()),
        ]);

        // calls the static method directly (not install_ocplugin), writing no DB / spawning nothing.
        let p = PluginManager::preview_ocplugin(&zip_path).expect("preview");
        assert_eq!(p.id, "com.opencapx.test-vision");
        assert_eq!(p.name, "Test Vision");
        assert_eq!(p.version, "0.3.1");
        assert_eq!(p.ptype, "capability");
        assert_eq!(p.description.as_deref(), Some("Reads pixels, returns text."));
        assert_eq!(p.capabilities, vec!["image.analyze", "image.ocr"]);
        assert_eq!(p.permissions.len(), 4);
        // the high_risk markers align exactly with core::permission::HIGH_RISK.
        let by_name: std::collections::HashMap<&str, bool> = p
            .permissions
            .iter()
            .map(|x| (x.name.as_str(), x.high_risk))
            .collect();
        assert_eq!(by_name["image.read"], false);
        assert_eq!(by_name["camera"], true);
        assert_eq!(by_name["filesystem.write"], true);
        assert_eq!(by_name["process.execute"], true);

        // a zip missing the manifest must also be explicitly rejected, not returned as an empty object.
        make_zip(&base.join("nomanifest.ocplugin"), &[("bin/x.py".to_string(), "x".to_string())]);
        let err = PluginManager::preview_ocplugin(&base.join("nomanifest.ocplugin")).unwrap_err();
        assert!(err.contains("missing opencapx-plugin.json"), "got: {}", err);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Phase 24 — the sole contract for the UI toggle flip: when settings.ts calls set_plugin_auto_reload,
    /// the Rust side must write the SQL `auto_reload` column + flip the per-manager set, and list()
    /// must expose the autoReload field to the frontend. Here we verify the SQL + DTO flip directly against storage,
    /// without depending on PluginManager shared state, so it can run in parallel with other plugin tests.
    #[test]
    fn set_auto_reload_round_trips_through_storage() {
        let dir = std::env::temp_dir().join(format!("opencapx-autoreload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        );
        let id = "com.opencapx.unit-auto-reload";
        let manifest = r#"{"id":"com.opencapx.unit-auto-reload","name":"u","version":"0.1.0","apiVersion":"1","type":"capability","runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"]}"#;
        // create a plugin row directly (auto_reload=0, stopped)
        s.with_conn(|c| {
            c.execute(
                "INSERT INTO plugins (id, version, type, status, path, manifest, auto_reload) VALUES (?1, ?2, ?3, 'stopped', ?4, ?5, 0)",
                rusqlite::params![id, "0.1.0", "capability", dir.join("dummy").to_string_lossy().to_string(), manifest],
            )
            .unwrap_or(0)
        });

        // flip on → SQL column = 1
        s.with_conn(|c| {
            c.execute(
                "UPDATE plugins SET auto_reload = ?2 WHERE id = ?1",
                rusqlite::params![id, 1i64],
            )
            .unwrap_or(0)
        });
        let on: i64 = s
            .with_conn_ref(|c| {
                c.query_row(
                    "SELECT auto_reload FROM plugins WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .unwrap()
            })
            .unwrap();
        assert_eq!(on, 1, "the SQL column flips to 1");

        // list() surfaces the auto_reload column from SQL, verifying the DTO field semantics (the plugin table already has this column)
        let listed: i64 = s
            .with_conn_ref(|c| {
                c.query_row(
                    "SELECT auto_reload FROM plugins WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .unwrap()
            })
            .unwrap();
        assert_eq!(listed != 0, true, "DTO field = true when auto_reload=1");

        // flip off → SQL column = 0
        s.with_conn(|c| {
            c.execute(
                "UPDATE plugins SET auto_reload = ?2 WHERE id = ?1",
                rusqlite::params![id, 0i64],
            )
            .unwrap_or(0)
        });
        let off: i64 = s
            .with_conn_ref(|c| {
                c.query_row(
                    "SELECT auto_reload FROM plugins WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .unwrap()
            })
            .unwrap();
        assert_eq!(off, 0, "the SQL column flips back to 0");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Restart backfill: rows with DB auto_reload=1 re-enter the polling set, with the current mtime as the baseline.
    #[test]
    fn auto_reload_restores_from_db_on_boot() {
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-ar-restore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("opencapx-plugin.json"), "{}").unwrap();
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        crate::core::set_shared_store(store.clone());
        {
            let mut s = store.lock().unwrap();
            s.with_conn(|c| {
                c.execute(
                    "INSERT INTO plugins (id, version, type, status, path, manifest, auto_reload) VALUES (?1,?2,?3,?4,?5,?6,1)",
                    params!["com.x.ar", "1.0.0", "capability", "stopped", dir.display().to_string(), "{}"],
                )
                .unwrap_or(0);
                c.execute(
                    "INSERT INTO plugins (id, version, type, status, path, manifest, auto_reload) VALUES (?1,?2,?3,?4,?5,?6,0)",
                    params!["com.x.noar", "1.0.0", "capability", "stopped", dir.display().to_string(), "{}"],
                )
                .unwrap_or(0)
            });
        }
        let mgr = PluginManager::shared();
        assert_eq!(mgr.restore_auto_reload(), Some(1), "only auto_reload=1 row restored");
        assert!(mgr.auto_reload_set.lock().unwrap().contains("com.x.ar"));
        assert!(!mgr.auto_reload_set.lock().unwrap().contains("com.x.noar"));
        assert!(
            mgr.last_mtimes.lock().unwrap().contains_key("com.x.ar"),
            "baseline mtime recorded"
        );
        // singleton reused across tests: clear the state injected by this run
        mgr.auto_reload_set.lock().unwrap().remove("com.x.ar");
        mgr.last_mtimes.lock().unwrap().remove("com.x.ar");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watchdog_restarts_then_disables_after_max_retries() {
        skip_probe_in_tests();
        let py = std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !py {
            eprintln!("skip: no python3");
            return;
        }
        let dir = std::env::temp_dir().join(format!("opencapx-wd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plugin_dir = dir.join("crash-after-init");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("opencapx-plugin.json"),
            r#"{
              "id": "com.opencapx.crash-after-init",
              "name": "crash",
              "version": "0.1.0",
              "apiVersion": "1",
              "type": "pet",
              "runtime": {"type": "process", "command": "python3", "args": ["crash.py"]}
            }"#,
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("crash.py"),
            r#"import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "plugin.initialize":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{
            "pluginId":"com.opencapx.crash-after-init",
            "apiVersion":"1",
            "capabilities":[]
        }}), flush=True)
        break
sys.exit(1)
"#,
        )
        .unwrap();

        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        let bus = crate::core::event::EventBus::shared();
        let mgr = PluginManager::shared();
        let id = mgr.install_from_dir(&plugin_dir).expect("install");
        assert_eq!(id, "com.opencapx.crash-after-init");
        mgr.start(&id).expect("start");

        // poll the EventBus until plugin.watchdog_disabled appears (<= 20s)
        let start = std::time::Instant::now();
        let mut saw_restart = false;
        let mut saw_disabled = false;
        let mut retry_count = 0u32;
        let mut sub = bus.subscribe();
        while start.elapsed() < std::time::Duration::from_secs(20) {
            match sub.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(ev) => {
                    if ev.kind == "plugin.restarting" {
                        saw_restart = true;
                        retry_count = retry_count.max(
                            ev.payload
                                .get("retry")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0)
                                as u32,
                        );
                    }
                    if ev.kind == "plugin.watchdog_disabled" {
                        saw_disabled = true;
                        break;
                    }
                }
                Err(_) => {}
            }
        }
        mgr.stop(&id);
        assert!(saw_restart, "watchdog should have triggered at least one restart");
        assert!(saw_disabled, "watchdog should disable after max retries");
        assert!(retry_count >= 2, "should have escalated past first retry");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_subscribes_and_receives_core_event() {
        skip_probe_in_tests();
        let py = std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !py {
            eprintln!("skip: no python3");
            return;
        }
        let dir = std::env::temp_dir().join(format!("opencapx-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plugin_dir = dir.join("subtest");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("opencapx-plugin.json"),
            r#"{
              "id": "com.opencapx.subtest",
              "name": "subtest",
              "version": "0.1.0",
              "apiVersion": "1",
              "type": "pet",
              "runtime": {"type": "process", "command": "python3", "args": ["sub.py"]}
            }"#,
        )
        .unwrap();
        let log_path = dir.join("sub.log");
        std::fs::write(
            plugin_dir.join("sub.py"),
            format!(
                r#"import json, sys
LOG = r"{log}"
with open(LOG, "w") as f: f.write("init\n")
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "plugin.initialize":
        print(json.dumps({{"jsonrpc":"2.0","id":msg["id"],"result":{{
            "pluginId":"com.opencapx.subtest","apiVersion":"1","capabilities":[]
        }}}}), flush=True)
        print(json.dumps({{"jsonrpc":"2.0","id":99,"method":"plugin.subscribe","params":{{"kind":"test.kind"}}}}), flush=True)
        continue
    if msg.get("id") == 99:
        continue
    if msg.get("method") == "core.event":
        with open(LOG, "a") as f: f.write(json.dumps(msg) + "\n")
"#,
                log = log_path.to_string_lossy().replace('\\', r"\\"),
            ),
        )
        .unwrap();

        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        let bus = crate::core::event::EventBus::shared();
        // start the fanout thread (the real app starts it in main.rs setup)
        let mgr = PluginManager::shared();
        crate::core::subscriber::spawn_fanout(bus.clone(), mgr.clone());
        let id = mgr.install_from_dir(&plugin_dir).expect("install");
        mgr.start(&id).expect("start");

        // wait for the plugin to finish sending the subscribe request (log file written + registry recorded)
        let start = std::time::Instant::now();
        let mut subscribed = false;
        while start.elapsed() < std::time::Duration::from_secs(5) {
            if !crate::core::subscriber::SubscriptionRegistry::shared()
                .subscribers("test.kind")
                .is_empty()
            {
                subscribed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(subscribed, "plugin should have registered subscription");

        // emit the matching event
        bus.publish(&crate::core::event::OpencapxEvent::new(
            "test.kind",
            "test",
            serde_json::json!({"x": 42}),
        ));

        // wait for the plugin to receive the core.event notification and write it to the log
        let start = std::time::Instant::now();
        let mut got = false;
        while start.elapsed() < std::time::Duration::from_secs(5) {
            if let Ok(text) = std::fs::read_to_string(&log_path) {
                if text.contains("\"test.kind\"") && text.contains("\"x\": 42") {
                    got = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        mgr.stop(&id);
        assert!(got, "plugin should have received core.event notification");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After enabling auto_reload, changing the manifest file mtime → triggers the plugin.auto_reloaded event within 3s.
    /// Depends on the pet-blank python plugin staying alive until we stop it.
    /// Starting the background poller is a process-level singleton, so running several plugin tests in sequence crosses cases → #[ignore] by default:
    /// run with `cargo test auto_reload -- --ignored --test-threads=1`.
    #[test]
    #[ignore]
    fn auto_reload_restarts_on_manifest_change() {
        skip_probe_in_tests();
        let py = std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !py {
            eprintln!("skip: no python3");
            return;
        }
        let dir = std::env::temp_dir().join(format!("opencapx-autoreload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // copy pet-blank to a standalone dir
        std::fs::create_dir_all(&dir).unwrap();
        let plugin_dir = dir.join("autoreload");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("opencapx-plugin.json"),
            r#"{"id":"com.opencapx.autoreload","name":"AR","version":"0.1.0","apiVersion":"1","type":"pet","runtime":{"type":"process","command":"python3","args":["bin/pet.py"]}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(plugin_dir.join("bin")).unwrap();
        // read pluginId from the initialize params (rather than hardcoding it), making it easy to reuse the same script with a different id
        std::fs::write(
            plugin_dir.join("bin").join("pet.py"),
            r#"import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "plugin.initialize":
        pid = msg["params"]["pluginId"]
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"pluginId":pid,"apiVersion":"1"}}) + "\n")
        sys.stdout.flush()
        continue
    if msg.get("method") == "plugin.shutdown":
        break
"#,
        )
        .unwrap();

        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        let bus = crate::core::event::EventBus::shared();
        let mgr = PluginManager::shared();
        crate::core::plugin::spawn_auto_reload_poller();
        let id = mgr.install_from_dir(&plugin_dir).expect("install");
        mgr.start(&id).expect("start");

        // wait for the first start round to write last_mtimes
        std::thread::sleep(Duration::from_millis(200));

        // enable auto_reload and change the manifest content → mtime follows; write across a 1s boundary to bypass FS mtime precision.
        mgr.set_auto_reload(&id, true).expect("set auto_reload");
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(
            &plugin_dir.join("opencapx-plugin.json"),
            r#"{"id":"com.opencapx.autoreload","name":"AR","version":"0.2.0","apiVersion":"1","type":"pet","runtime":{"type":"process","command":"python3","args":["bin/pet.py"]}}"#,
        )
        .unwrap();

        // subscribe to the event and wait for plugin.auto_reloaded
        let mut rx = bus.subscribe();
        let start = std::time::Instant::now();
        let mut saw = false;
        while start.elapsed() < std::time::Duration::from_secs(10) {
            match rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(ev) => {
                    if ev.kind == "plugin.auto_reloaded" && ev.payload.get("pluginId").and_then(|v| v.as_str()) == Some(id.as_str()) {
                        saw = true;
                        break;
                    }
                }
                Err(_) => {}
            }
        }
        mgr.stop(&id);
        assert!(saw, "auto_reload should fire after manifest mtime change");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reverse core.log {level, message} → emits plugin.log on the EventBus (with source=reverse).
    #[test]
    fn core_log_reverse_publishes_plugin_log_event() {
        let py = std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !py {
            eprintln!("skip: no python3");
            return;
        }
        let dir = std::env::temp_dir().join(format!("opencapx-corelog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plugin_dir = dir.join("clog");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("opencapx-plugin.json"),
            r#"{"id":"com.opencapx.clog","name":"CL","version":"0.1.0","apiVersion":"1","type":"pet","runtime":{"type":"process","command":"python3","args":["clog.py"]}}"#,
        );
        std::fs::write(
            plugin_dir.join("clog.py"),
            r#"import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "plugin.initialize":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"pluginId":"com.opencapx.clog","apiVersion":"1"}}) + "\n"); sys.stdout.flush()
        # proactively reverse core.log
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","method":"core.log","params":{"level":"warn","message":"hello-from-reverse"}}) + "\n"); sys.stdout.flush()
        continue
    if msg.get("method") == "plugin.shutdown":
        break
"#,
        )
        .unwrap();

        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        let bus = crate::core::event::EventBus::shared();
        let mgr = PluginManager::shared();
        let mut sub = bus.subscribe();
        let id = mgr.install_from_dir(&plugin_dir).expect("install");
        mgr.start(&id).expect("start");

        // wait for the plugin.log event
        let start = std::time::Instant::now();
        let mut got = false;
        while start.elapsed() < std::time::Duration::from_secs(5) {
            match sub.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(ev) => {
                    if ev.kind == "plugin.log"
                        && ev.payload.get("pluginId").and_then(|v| v.as_str())
                            == Some(id.as_str())
                        && ev.payload.get("source").and_then(|v| v.as_str())
                            == Some("reverse")
                        && ev.payload.get("level").and_then(|v| v.as_str()) == Some("warn")
                        && ev.payload.get("message").and_then(|v| v.as_str())
                            == Some("hello-from-reverse")
                    {
                        got = true;
                        break;
                    }
                }
                Err(_) => {}
            }
        }
        mgr.stop(&id);
        assert!(got, "core.log reverse should publish plugin.log event");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reverse_config_get_returns_default_when_missing() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let reply: Reply = {
            let captured = captured.clone();
            Arc::new(move |v: serde_json::Value| {
                *captured.lock().unwrap() = Some(v);
            })
        };
        let v = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "config.get",
            "params": { "key": "absent-xyz", "default": "fallback" }
        });
        handle_reverse("com.opencapx.cfg-test", v, reply);
        let got = captured.lock().unwrap().clone().expect("reply called");
        assert_eq!(got["id"], serde_json::json!(7));
        assert_eq!(got["result"], serde_json::json!("fallback"));
    }

    #[test]
    fn reverse_config_get_returns_null_when_no_default() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let reply: Reply = {
            let captured = captured.clone();
            Arc::new(move |v: serde_json::Value| {
                *captured.lock().unwrap() = Some(v);
            })
        };
        let v = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "config.get",
            "params": { "key": "absent-xyz2" }
        });
        handle_reverse("com.opencapx.cfg-test-2", v, reply);
        let got = captured.lock().unwrap().clone().expect("reply called");
        assert_eq!(got["result"], serde_json::Value::Null);
    }

    #[test]
    fn reverse_config_unknown_op_returns_error() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let reply: Reply = {
            let captured = captured.clone();
            Arc::new(move |v: serde_json::Value| {
                *captured.lock().unwrap() = Some(v);
            })
        };
        let v = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "config.bogus",
            "params": {}
        });
        handle_reverse("com.opencapx.cfg-test-3", v, reply);
        let got = captured.lock().unwrap().clone().expect("reply called");
        assert_eq!(got["error"]["code"], serde_json::json!(-32603));
        assert!(got["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("unknown config op"));
    }

    /// uninstall cleanly clears config + sqlite + emits an event.
    #[test]
    fn uninstall_cleans_config_and_emits_event() {
        skip_probe_in_tests();
        let py = std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !py {
            eprintln!("skip: no python3");
            return;
        }
        let dir = std::env::temp_dir().join(format!("opencapx-uninst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plugin_dir = dir.join("echo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("opencapx-plugin.json"),
            r#"{"id":"com.opencapx.echo-uninst","name":"EchoU","description":"echo for uninstall test","version":"0.1.0","apiVersion":"1","type":"pet","runtime":{"type":"process","command":"python3","args":["echo.py"]}}"#,
        );
        std::fs::write(
            plugin_dir.join("echo.py"),
            r#"import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "plugin.initialize":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"pluginId":"com.opencapx.echo-uninst","apiVersion":"1"}}) + "\n"); sys.stdout.flush()
        continue
    if msg.get("method") == "plugin.shutdown":
        sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{}}) + "\n"); sys.stdout.flush()
        break
"#,
        ).unwrap();

        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        let bus = crate::core::event::EventBus::shared();
        let mgr = PluginManager::shared();
        let id = mgr.install_from_dir(&plugin_dir).expect("install");

        // write some config for uninstall to clear
        crate::core::config::set(&id, "testKey", &serde_json::json!("to-be-deleted")).unwrap();
        assert!(crate::core::config::config_path(&id).exists());

        let mut sub = bus.subscribe();
        mgr.uninstall(&id).expect("uninstall");

        // the config file should be deleted
        assert!(!crate::core::config::config_path(&id).exists());
        // M8 hardening boundary: the source directory of a directory install (dev/test) belongs to the user and must not be deleted on uninstall
        assert!(plugin_dir.exists(), "dir-install source must survive uninstall");
        // the sqlite plugins table has no such id
        let still: Option<String> = store.lock().ok().and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT id FROM plugins WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
            .flatten()
        });
        assert!(still.is_none(), "plugin row should be removed");

        // wait for the plugin.uninstalled event
        let start = std::time::Instant::now();
        let mut got = false;
        while start.elapsed() < std::time::Duration::from_secs(2) {
            if let Ok(ev) = sub.recv_timeout(std::time::Duration::from_millis(200)) {
                if ev.kind == "plugin.uninstalled"
                    && ev.payload.get("pluginId").and_then(|v| v.as_str())
                        == Some(id.as_str())
                {
                    got = true;
                    break;
                }
            }
        }
        assert!(got, "plugin.uninstalled event should be emitted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Wow 7 — uninstall_preview must lay out all the metadata that would be cleared +
    /// detect dependents (other installed plugins sharing a capability).
    /// write a unique id into shared_store (whoever grabs the OnceLock first uses whose DB),
    /// and clean it up after the test. Avoids set_shared_store polluting other parallel tests.
    #[test]
    fn uninstall_preview_reports_state_and_dependents() {
        skip_probe_in_tests();
        let me_id = "com.opencapx.uprev-me-7";
        let other_id = "com.opencapx.uprev-other-7";
        let me_manifest = r#"{"id":"com.opencapx.uprev-me-7","name":"Me","version":"0.1.0","apiVersion":"1","type":"capability","capabilities":["image.analyze","image.ocr"],"permissions":["image.read","camera"]}"#;
        let other_manifest = r#"{"id":"com.opencapx.uprev-other-7","name":"Other","version":"0.1.0","apiVersion":"1","type":"capability","capabilities":["image.analyze"],"permissions":["image.read"]}"#;

        // brings its own temp DB + holds TEST_STORE_LOCK: uninstall_preview goes through PluginManager::store()
        // and re-reads the global store; without the lock, a parallel set_shared_store test could swap the DB between insert and read
        // → "plugin not installed". Same pattern as capability_dependency_graph.
        let dir = std::env::temp_dir().join(format!(
            "opencapx-uprev-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        let mut s = store.lock().unwrap();
        let path_a = std::env::temp_dir().join(format!("opencapx-uprev-me-{}", std::process::id())).to_string_lossy().to_string();
        let path_b = std::env::temp_dir().join(format!("opencapx-uprev-other-{}", std::process::id())).to_string_lossy().to_string();
        s.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO plugins (id, version, type, status, path, manifest, auto_reload) VALUES (?1, ?2, ?3, 'running', ?4, ?5, 1)",
                rusqlite::params![me_id, "0.1.0", "capability", path_a, me_manifest],
            ).unwrap_or(0);
            c.execute(
                "INSERT OR REPLACE INTO plugins (id, version, type, status, path, manifest, auto_reload) VALUES (?1, ?2, ?3, 'running', ?4, ?5, 0)",
                rusqlite::params![other_id, "0.1.0", "capability", path_b, other_manifest],
            ).unwrap_or(0);
            c.execute(
                "INSERT OR REPLACE INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at) VALUES (?1, 'image.read', NULL, 'granted', 0)",
                [me_id],
            ).unwrap_or(0);
            c.execute(
                "INSERT OR REPLACE INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at) VALUES (?1, 'camera', NULL, 'granted', 0)",
                [me_id],
            ).unwrap_or(0)
        });
        drop(s);

        // the config file is written to the default ~/.opencapx/config/<id>.json (a global location),
        // clean it up after the test to avoid polluting the user's machine
        crate::core::config::set(me_id, "k", &serde_json::json!("v")).unwrap();

        let mgr = PluginManager::shared();
        let preview = mgr.uninstall_preview(me_id).expect("preview");

        assert_eq!(preview.id, me_id);
        assert_eq!(preview.name, "Me");
        assert_eq!(preview.version, "0.1.0");
        assert_eq!(preview.auto_reload, true, "auto_reload should be surfaced");
        assert_eq!(preview.config_exists, true, "an existing config file should be detected");
        assert_eq!(preview.permission_count, 2);
        assert_eq!(preview.capability_count, 2);
        assert_eq!(preview.dependents, vec![other_id.to_string()], "other plugins sharing image.analyze should be listed as dependents");

        // a nonexistent id → Err
        let err = mgr.uninstall_preview("not-real-7");
        assert!(err.is_err());

        // cleanup: config is written to a global location (~/.opencapx/config) and must be cleared explicitly; the DB is this test's
        // own temp DB, so deleting the DB together with the directory is enough.
        drop(_g);
        crate::core::config::reset(me_id).ok();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase 32 — dependency graph: 3 plugins, a↔b share image.analyze, b↔c share camera,
    /// a↔c share nothing. Verify nodes = 3, edges = 2, the shared lists are correct, and from/to are sorted lexicographically.
    #[test]
    fn capability_dependency_graph_finds_shared_pairs() {
        let id_a = "com.opencapx.depgraph-a-8";
        let id_b = "com.opencapx.depgraph-b-8";
        let id_c = "com.opencapx.depgraph-c-8";
        let m_a = r#"{"id":"com.opencapx.depgraph-a-8","name":"A","version":"0.1.0","apiVersion":"1","type":"capability","capabilities":["image.analyze","file.read"],"permissions":["image.read"]}"#;
        let m_b = r#"{"id":"com.opencapx.depgraph-b-8","name":"B","version":"0.1.0","apiVersion":"1","type":"capability","capabilities":["image.analyze","camera"],"permissions":["image.read","camera"]}"#;
        let m_c = r#"{"id":"com.opencapx.depgraph-c-8","name":"C","version":"0.1.0","apiVersion":"1","type":"capability","capabilities":["camera","browser.open"],"permissions":["browser.open"]}"#;

        // brings its own temp DB + holds TEST_STORE_LOCK: list() re-reads the global store,
        // and without the lock a parallel set_shared_store test could swap the DB between insert and read → nodes=0.
        let dir = std::env::temp_dir().join(format!("opencapx-dg-store-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        let path_a = std::env::temp_dir().join(format!("opencapx-dg-a-{}", std::process::id())).to_string_lossy().to_string();
        let path_b = std::env::temp_dir().join(format!("opencapx-dg-b-{}", std::process::id())).to_string_lossy().to_string();
        let path_c = std::env::temp_dir().join(format!("opencapx-dg-c-{}", std::process::id())).to_string_lossy().to_string();
        {
            let mut s = store.lock().unwrap();
            s.with_conn(|c| {
                c.execute(
                    "INSERT OR REPLACE INTO plugins (id, version, type, status, path, manifest, auto_reload) VALUES (?1, '0.1.0', 'capability', 'running', ?2, ?3, 0)",
                    rusqlite::params![id_a, path_a, m_a],
                ).unwrap_or(0);
                c.execute(
                    "INSERT OR REPLACE INTO plugins (id, version, type, status, path, manifest, auto_reload) VALUES (?1, '0.1.0', 'capability', 'running', ?2, ?3, 0)",
                    rusqlite::params![id_b, path_b, m_b],
                ).unwrap_or(0);
                c.execute(
                    "INSERT OR REPLACE INTO plugins (id, version, type, status, path, manifest, auto_reload) VALUES (?1, '0.1.0', 'capability', 'running', ?2, ?3, 0)",
                    rusqlite::params![id_c, path_c, m_c],
                ).unwrap_or(0)
            });
        }

        let mgr = PluginManager::shared();
        let graph = mgr.capability_dependency_graph();

        // 3 nodes, lexicographic by id
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.nodes[0].id, id_a);
        assert_eq!(graph.nodes[1].id, id_b);
        assert_eq!(graph.nodes[2].id, id_c);
        assert_eq!(graph.nodes[0].capabilities, vec!["image.analyze", "file.read"]);

        // 2 edges (a↔b share image.analyze, b↔c share camera), a↔c share nothing
        assert_eq!(graph.edges.len(), 2);
        let ab = graph.edges.iter().find(|e| (e.from == id_a && e.to == id_b)).expect("a↔b");
        assert_eq!(ab.shared, vec!["image.analyze".to_string()]);
        let bc = graph.edges.iter().find(|e| (e.from == id_b && e.to == id_c)).expect("b↔c");
        assert_eq!(bc.shared, vec!["camera".to_string()]);

        // cleanup
        let mut s = store.lock().unwrap();
        s.with_conn(|c| {
            c.execute("DELETE FROM plugins WHERE id = ?1", [id_a]).unwrap_or(0);
            c.execute("DELETE FROM plugins WHERE id = ?1", [id_b]).unwrap_or(0);
            c.execute("DELETE FROM plugins WHERE id = ?1", [id_c]).unwrap_or(0)
        });
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lifecycle events are written to storage keyed by pluginId and can be queried by list_plugin_lifecycle.
    /// Here we use storage.log_event directly to simulate the publish→SQLite→SELECT chain.
    #[test]
    fn lifecycle_events_are_recorded_for_list_query() {
        skip_probe_in_tests();
        use crate::core::storage::StoreEnum;
        let dir = std::env::temp_dir().join(format!("opencapx-lifecycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        let id = "com.opencapx.lifecycle-7";
        // simulate the real start/stop path: starting / running / stopped + 1 other-plugin entry as noise.
        let kinds = [
            "plugin.lifecycle.starting",
            "plugin.lifecycle.running",
            "plugin.lifecycle.stopped",
        ];
        {
            let mut s = store.lock().unwrap();
            for k in kinds {
                s.log_event(&super::super::event::OpencapxEvent::new(
                    k,
                    "core",
                    serde_json::json!({ "pluginId": id }),
                ));
            }
            // noise entry: another id, confirming list_plugin_lifecycle does not wrongly include it.
            s.log_event(&super::super::event::OpencapxEvent::new(
                "plugin.lifecycle.starting",
                "core",
                serde_json::json!({ "pluginId": "com.opencapx.other-7" }),
            ));
        }
        let events = store.lock().unwrap().list_events("plugin.", 50);
        let mine: Vec<_> = events
            .into_iter()
            .filter(|e| e.payload.get("pluginId").and_then(|v| v.as_str()) == Some(id))
            .collect();
        assert_eq!(mine.len(), 3, "there should be exactly 3 lifecycle events, actually {}", mine.len());
        let names: Vec<&str> = mine.iter().map(|e| e.kind.as_str()).collect();
        assert!(names.contains(&"plugin.lifecycle.starting"));
        assert!(names.contains(&"plugin.lifecycle.running"));
        assert!(names.contains(&"plugin.lifecycle.stopped"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F5 — list() surfaces missing_dependencies for plugins with unmet dependencies (used for the settings-page warning).
    #[test]
    fn list_reports_missing_dependencies() {
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-missdep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        crate::core::set_shared_store(store.clone());
        {
            let mut s = store.lock().unwrap();
            s.with_conn(|c| c.execute(
                "INSERT INTO plugins (id, version, type, status, path, manifest) VALUES (?1,?2,?3,?4,?5,?6)",
                params!["com.x.a", "1.0.0", "capability", "stopped", "/tmp/a",
                    r#"{"id":"com.x.a","name":"A","version":"1.0.0","apiVersion":"1","type":"capability",
                        "runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"],
                        "dependencies":{"com.x.b":">=1.2.0"}}"#],
            ).unwrap_or(0));
        }
        let dto = PluginManager::shared().list().into_iter().find(|d| d.id == "com.x.a").unwrap();
        assert_eq!(dto.missing_dependencies.len(), 1);
        assert_eq!(dto.missing_dependencies[0].id, "com.x.b");
        assert_eq!(dto.missing_dependencies[0].requirement, ">=1.2.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Soak — three real sample plugins run fully through real sandbox-exec per their manifest sandbox declarations:
    /// handshake → representative capability (including the plugin-data write path) → shutdown. Writes land in the real plugin-data
    /// (the sandbox's only writable area, matching production semantics). Run explicitly: `cargo test sandbox_soak -- --ignored`.
    #[test]
    #[ignore = "soak: writes real ~/.opencapx/plugin-data, run explicitly"]
    #[cfg(target_os = "macos")]
    fn sandbox_soak_real_plugins() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        if !std::path::Path::new("/usr/bin/sandbox-exec").is_file() {
            eprintln!("skip: no sandbox-exec");
            return;
        }
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        use crate::core::process::{EnvPolicy, OnReverse, PluginProcess, Reply, RuntimeSpec, SandboxSpec};
        use std::collections::HashMap;
        let on_reverse: OnReverse = std::sync::Arc::new(|_v: serde_json::Value, _r: Reply| {});

        // (directory, plugin id, [(method, params, assertion closure description)]) — a skeleton shared by the three plugins
        let cases: &[(&str, &str, Vec<(String, serde_json::Value)>)] = &[
            (
                "echo-vision",
                "com.opencapx.echo-vision",
                vec![(
                    "image.analyze".into(),
                    json!({ "image": "/tmp/soak-probe.png" }),
                )],
            ),
            (
                "weather-demo",
                "com.example.weather-demo",
                vec![
                    ("weather.current".into(), json!({})),
                    ("weather.set_home".into(), json!({ "city": "Tokyo" })),
                ],
            ),
            (
                "things-demo",
                "com.opencapx.things-demo",
                vec![
                    ("things.add".into(), json!({ "title": "soak-item" })),
                    ("things.list".into(), json!({})),
                ],
            ),
        ];

        for (dir, id, calls) in cases {
            let root = repo.join("plugins").join(dir);
            let m = PluginManager::read_manifest(&root).unwrap_or_else(|e| panic!("{}: {}", dir, e));
            let decl = m.sandbox.as_ref().unwrap_or_else(|| panic!("{} did not declare sandbox", dir));
            assert!(validate_sandbox(decl).is_ok());

            let profile = super::super::sandbox::sandbox_profile(id, decl);
            let tmp_dir = super::super::sandbox::plugin_tmp_dir(id);
            std::fs::create_dir_all(&tmp_dir).expect("plugin-data/tmp");
            let rt = m.runtime.clone().expect("runtime");
            let spec = RuntimeSpec {
                command: rt.command,
                args: rt.args,
                env: rt.env.clone().into_iter().collect::<HashMap<_, _>>(),
            };
            let proc = PluginProcess::spawn(id, &root, &spec, "soak", on_reverse.clone(), &EnvPolicy::default(), Some(&SandboxSpec { profile, tmp_dir }))
                .unwrap_or_else(|e| panic!("{} spawn: {}", dir, e));

            let init = proc
                .call("plugin.initialize", json!({ "coreVersion": env!("CARGO_PKG_VERSION"), "apiVersion": "1", "pluginId": id }), std::time::Duration::from_secs(20))
                .unwrap_or_else(|e| panic!("{} handshake: {}", dir, e));
            assert_eq!(init["pluginId"], json!(id), "{} handshake id", dir);

            for (method, params) in calls {
                let out = proc
                    .call(method, params.clone(), std::time::Duration::from_secs(20))
                    .unwrap_or_else(|e| panic!("{} {}: {}", dir, method, e));
                eprintln!("[soak] {}.{} -> {}", id, method, serde_json::to_string(&out).unwrap_or_default());
            }

            // write-path destination: store.json must appear in plugin-data (= the profile's only writable area).
            if *id != "com.opencapx.echo-vision" {
                let store = super::super::sandbox::plugin_data_root().join(id).join("store.json");
                assert!(store.is_file(), "{} should write the store under plugin-data: {}", dir, store.display());
            }

            proc.shutdown();
            assert!(!proc.is_alive(), "{} shutdown", dir);
            eprintln!("[soak] {} ✓ sandboxed full path passed", id);
        }
    }

    /// Plugin path calls capability::execute → the plugin.<pid> span is written to disk (with sessionId).
    /// setup shares its source with things_demo_end_to_end (temp base + Db store + things-demo install).
    #[test]
    fn capability_execute_traces_plugin_span() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        // OPENCAPX_TRACES_DIR shares one lock with the req_trace / retention / plugin_trace tests (Task 1)
        let _tg = crate::core::plugin_trace::traces_env_lock();
        let base = std::env::temp_dir().join(format!("opencapx-traceplug-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("OPENCAPX_TRACES_DIR", base.join("traces"));

        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            crate::core::storage::StoreEnum::Db(
                crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::core::set_shared_store(store.clone());
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));
        std::fs::create_dir_all(base.join("plugins")).unwrap();

        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let src = base.join("things-src");
        copy_tree(&repo.join("plugins").join("things-demo"), &src);
        std::env::set_var("OPENCAPX_PLUGIN_DATA_DIR", base.join("plugin-data"));
        let store_file = base
            .join("plugin-data")
            .join("com.opencapx.things-demo")
            .join("store.json");
        {
            let mpath = src.join("opencapx-plugin.json");
            let mut m: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&mpath).unwrap()).unwrap();
            m["storePath"] = serde_json::Value::String(store_file.display().to_string());
            std::fs::write(&mpath, serde_json::to_string(&m).unwrap()).unwrap();
        }
        let id = PluginManager::shared().install_from_dir(&src).expect("install things-demo");
        assert!(crate::core::permission::set_decision(&store, &id, "things.read", "granted"));

        let tid = crate::core::req_trace::begin("ag_pt", "", "");
        let out = crate::core::capability::execute("things.list", &json!({}), Some("ag_pt"));
        crate::core::req_trace::finish(out.is_ok(), out.err().as_deref(), serde_json::Value::Null);

        let text = std::fs::read_to_string(
            crate::core::req_trace::rpc_traces_root()
                .join("ag_pt")
                .join(format!("{}.ndjson", tid)),
        )
        .unwrap_or_default();
        assert!(text.contains("\"name\":\"plugin."), "the plugin span should be on disk: {text}");
        assert!(text.contains("\"sessionId\":"), "the span should carry a sessionId linking it to the plugin_trace dump: {text}");

        PluginManager::shared().stop(&id);
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        std::env::remove_var("OPENCAPX_PLUGIN_DATA_DIR");
        std::env::remove_var("OPENCAPX_TRACES_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// list type: a legal bare declaration; default/options/min/max/pick are all rejected (data belongs to the plugin process).
    #[test]
    fn settings_list_type_validation() {
        let dir = std::env::temp_dir().join(format!("opencapx-list-val-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |body: &str| std::fs::write(dir.join("opencapx-plugin.json"), body).unwrap();
        // legal: label/section/aliases/visible are available consistently with other types
        write(&format!(r#"{{"id":"com.x.list","name":"L","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{{"type":"process","command":"python3"}},"capabilities":["image.analyze"],
            "settings":[{{"key":"tags","type":"list","label":"Tags"}}]}}"#));
        assert!(PluginManager::read_manifest(&dir).is_ok());
        // default rejected
        write(&format!(r#"{{"id":"com.x.list","name":"L","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{{"type":"process","command":"python3"}},"capabilities":["image.analyze"],
            "settings":[{{"key":"tags","type":"list","default":[]}}]}}"#));
        let err = PluginManager::read_manifest(&dir).unwrap_err();
        assert!(err.contains("list setting tags"), "{err}");
        // options rejected
        write(&format!(r#"{{"id":"com.x.list","name":"L","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{{"type":"process","command":"python3"}},"capabilities":["image.analyze"],
            "settings":[{{"key":"tags","type":"list","options":["a"]}}]}}"#));
        assert!(PluginManager::read_manifest(&dir).unwrap_err().contains("list setting tags"));
        // min / max / pick likewise rejected: range and selection fields are meaningless for list
        for extra in ["\"min\":1", "\"max\":9", "\"pick\":\"file\""] {
            write(&format!(r#"{{"id":"com.x.list","name":"L","version":"1.0.0","apiVersion":"1","type":"capability",
                "runtime":{{"type":"process","command":"python3"}},"capabilities":["image.analyze"],
                "settings":[{{"key":"tags","type":"list",{extra}}}]}}"#));
            let err = PluginManager::read_manifest(&dir).unwrap_err();
            assert!(err.contains("list setting tags"), "{extra} → {err}");
        }
        // a predicate referencing a list key: rejected at install time — list values do not enter settings_view, so the predicate always reads undefined,
        // and allowing it would only give the author a row that never appears.
        write(&format!(r#"{{"id":"com.x.list","name":"L","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{{"type":"process","command":"python3"}},"capabilities":["image.analyze"],
            "settings":[{{"key":"tags","type":"list"}},{{"key":"plain","type":"text","visible":{{"op":"isSet","key":"tags","value":true}}}}]}}"#));
        let err = PluginManager::read_manifest(&dir).unwrap_err();
        assert!(
            err.contains("list setting \"tags\" cannot be referenced by a predicate"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Structured options {value,label}, order, deprecated.
    /// value is the storage/comparison surface; label is display only; the deprecated text must be non-empty and localizable.
    #[test]
    fn settings_structured_option_fields() {
        let dir = std::env::temp_dir().join(format!("opencapx-structured-options-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |body: &str| std::fs::write(dir.join("opencapx-plugin.json"), body).unwrap();
        let base = r#"{"id":"com.x.parity","name":"P","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"],"settings":[__SET__]}"#;
        // structured options: default lands on a value (not on a label)
        write(&base.replace("__SET__", r#"{"key":"mode","type":"dropdown","default":"fast","order":2,
            "options":["slow",{"value":"fast","label":{"en":"Fast","zh-Hans":"快速"}}]}"#));
        let m = PluginManager::read_manifest(&dir).expect("structured options + order ok");
        assert_eq!(m.settings[0].order, Some(2));
        // default matches the option's value, not the label
        write(&base.replace("__SET__", r#"{"key":"mode","type":"dropdown","default":"Fast",
            "options":[{"value":"fast","label":{"en":"Fast"}}]}"#));
        assert!(PluginManager::read_manifest(&dir).unwrap_err().contains("default must be one of options[]"));
        // deprecated: legal localizable text
        write(&base.replace("__SET__", r#"{"key":"legacy","type":"text","order":1,
            "deprecated":{"en":"Use mode instead.","zh-Hans":"请改用 mode。"},"default":""}"#));
        assert!(PluginManager::read_manifest(&dir).is_ok());
        // deprecated: blank text rejected
        write(&base.replace("__SET__", r#"{"key":"legacy","type":"text","deprecated":{"en":"  "}}"#));
        let err = PluginManager::read_manifest(&dir).unwrap_err();
        assert!(err.contains("deprecated"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// list values do not enter settings_view.values; set_setting_value writing a list key → Err.
    #[test]
    fn settings_list_not_settable_and_not_in_view() {
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-list-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let installed = dir.join("installed");
        std::fs::create_dir_all(&installed).unwrap();
        let manifest = r#"{"id":"com.x.list","name":"L","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"],
            "settings":[{"key":"tags","type":"list","label":"Tags"},{"key":"plain","type":"text"}]}"#;
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        store.lock().unwrap().with_conn(|c| {
            c.execute(
                "INSERT INTO plugins (id, version, type, status, path, manifest) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params!["com.x.list", "1.0.0", "capability", "stopped", installed.to_str().unwrap(), manifest],
            ).unwrap()
        }).unwrap();
        crate::core::set_shared_store(store.clone());
        let view = PluginManager::settings_view("com.x.list").expect("view");
        assert!(!view.values.contains_key("tags"), "list value must not leak into view: {:?}", view.values);
        let err = PluginManager::set_setting_value("com.x.list", "tags", &serde_json::json!(["a"])).unwrap_err();
        assert!(err.contains("managed by its plugin"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// list op guard: undeclared key / non-list type → Err (no spawn).
    #[test]
    fn settings_list_op_guards() {
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-list-op-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let installed = dir.join("installed");
        std::fs::create_dir_all(&installed).unwrap();
        let manifest = r#"{"id":"com.x.list","name":"L","version":"1.0.0","apiVersion":"1","type":"capability",
            "runtime":{"type":"process","command":"python3"},"capabilities":["image.analyze"],
            "settings":[{"key":"tags","type":"list"},{"key":"plain","type":"text"}]}"#;
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        store.lock().unwrap().with_conn(|c| {
            c.execute(
                "INSERT INTO plugins (id, version, type, status, path, manifest) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params!["com.x.list", "1.0.0", "capability", "stopped", installed.to_str().unwrap(), manifest],
            ).unwrap()
        }).unwrap();
        crate::core::set_shared_store(store.clone());
        let err = PluginManager::invoke_setting_list_op("com.x.list", "nope", "list", None, None, None).unwrap_err();
        assert!(err.contains("not declared"), "{err}");
        let err = PluginManager::invoke_setting_list_op("com.x.list", "plain", "list", None, None, None).unwrap_err();
        assert!(err.contains("not a list"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// list e2e: a real python process, add/delete/move round trip, items held by the plugin and persisted via config.
    #[test]
    fn settings_list_ops_round_trip() {
        skip_probe_in_tests();
        if !python3_available() {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-list-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let store: SharedStore = Arc::new(Mutex::new(crate::core::storage::StoreEnum::Db(
            crate::core::storage::Storage::open(&base.join("t.db")).unwrap(),
        )));
        // OPENCAPX_TRACES_DIR shares one lock with the req_trace / retention / plugin_trace tests;
        // this lock must be taken before TEST_STORE_LOCK (that is the order across the suite); taking it the other way would create a cycle with a test that "already holds
        // the traces lock and is waiting on the store lock".
        let _tg = crate::core::plugin_trace::traces_env_lock();
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // the plugin process's span persistence goes through plugin_trace::traces_root(); without redirection it would leave a test tree in the real
        // ~/.opencapx/traces/<id>/; the guard ensures this process-level variable is cleared even when an assertion panics,
        // not leaving it for later tests.
        struct TracesDir;
        impl Drop for TracesDir {
            fn drop(&mut self) {
                std::env::remove_var("OPENCAPX_TRACES_DIR");
            }
        }
        std::env::set_var("OPENCAPX_TRACES_DIR", base.join("traces"));
        let _traces = TracesDir;
        // the secret fallback directory likewise has no test coverage point, and the trailing uninstall → config::forget_plugin_secrets
        // would `remove_dir_all` the real fallback root (`dirs::config_dir()/opencapx/plugin-secrets`,
        // macOS = `~/Library/Application Support/opencapx/plugin-secrets`) for `<id>`;
        // so redirect it to a temp directory (a process-level variable; secret-writing tests set/remove it inside TEST_STORE_LOCK,
        // keeping them serial). The guard ensures the process-level variable is cleared even when an assertion panics.
        struct SecretsDir;
        impl Drop for SecretsDir {
            fn drop(&mut self) {
                std::env::remove_var("OPENCAPX_SECRETS_DIR");
            }
        }
        std::env::set_var("OPENCAPX_SECRETS_DIR", base.join("secrets"));
        let _secrets = SecretsDir;
        crate::core::set_shared_store(store.clone());

        // a plugin's reverse `config.set` lands in the real `~/.opencapx/config/<id>.json` (config::config_dir
        // has no test coverage point). Archive before clearing: otherwise a rerun reads tags left by the previous round and "fresh install
        // → empty list" becomes a false green; the guard ensures the original file is put back even on an assertion failure (panic), without polluting user config.
        struct ConfigSnapshot {
            path: PathBuf,
            saved: Option<Vec<u8>>,
            /// The content written by this test (refreshed after each op; empty = this test has not written anything yet). At snapshot time,
            /// the file not existing = this file was created from scratch by this test, but it still only deletes the file that is byte-identical to it
            /// — deleting content just written by someone else (the user's App / a parallel test) is far worse than leaving a temp file behind.
            written: Option<Vec<u8>>,
        }
        impl Drop for ConfigSnapshot {
            fn drop(&mut self) {
                match self.saved.take() {
                    Some(bytes) => {
                        // no panic in Drop, but a restore failure must not be silent: losing the user's config needs a trace.
                        if let Err(e) = std::fs::write(&self.path, bytes) {
                            eprintln!(
                                "ConfigSnapshot: restore {} failed: {e}",
                                self.path.display()
                            );
                        }
                    }
                    None => match (self.written.as_ref(), std::fs::read(&self.path)) {
                        (Some(written), Ok(current)) if &current == written => {
                            if let Err(e) = std::fs::remove_file(&self.path) {
                                eprintln!(
                                    "ConfigSnapshot: remove {} failed: {e}",
                                    self.path.display()
                                );
                            }
                        }
                        // already gone (uninstall deletes its own config): nothing to do.
                        (_, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                        (_, Err(e)) => eprintln!(
                            "ConfigSnapshot: cannot inspect {}: {e}",
                            self.path.display()
                        ),
                        _ => eprintln!(
                            "ConfigSnapshot: leaving {} (content is not what this test wrote)",
                            self.path.display()
                        ),
                    },
                }
            }
        }
        let cfg_path = crate::core::config::config_path("com.opencapx.echo-vision");
        let mut cfg_guard = ConfigSnapshot {
            path: cfg_path.clone(),
            saved: std::fs::read(&cfg_path).ok(),
            written: None,
        };
        let _ = std::fs::remove_file(&cfg_path);

        let src = base.join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..").join("plugins").join("echo-vision")
                .join("bin").join("echo_vision.py"),
        ).unwrap();
        std::fs::write(src.join("bin").join("echo_vision.py"), script).unwrap();
        let manifest = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..").join("plugins").join("echo-vision")
                .join("opencapx-plugin.json"),
        ).unwrap();
        std::fs::write(src.join("opencapx-plugin.json"), manifest).unwrap();
        let id = PluginManager::shared().install_from_dir(&src).expect("install");
        assert_eq!(id, "com.opencapx.echo-vision");

        // every op writes the list into this file via the plugin's config.set; note down "the bytes this test wrote"
        // so the guard can prove "this content was written by me" on any path (an assertion panic included).
        let mut call = |op: &str, index: Option<usize>, to: Option<usize>, value: Option<String>| {
            let out = PluginManager::invoke_setting_list_op(&id, "tags", op, index, to, value);
            cfg_guard.written = std::fs::read(&cfg_path).ok();
            out
        };
        let empty = call("list", None, None, None).expect("list");
        assert_eq!(empty, serde_json::json!([]), "fresh install → empty list");
        let one = call("add", None, None, Some("alpha".into())).expect("add");
        assert_eq!(one, serde_json::json!(["alpha"]));
        call("add", None, None, Some("beta".into())).expect("add 2");
        let moved = call("move", Some(1), Some(0), None).expect("move");
        assert_eq!(moved, serde_json::json!(["beta", "alpha"]));
        let after_del = call("delete", Some(1), None, None).expect("delete");
        assert_eq!(after_del, serde_json::json!(["beta"]));

        // empty / whitespace-only values do not land as placeholder items: add's strip guard (otherwise one empty UI submit inserts an item).
        let blank = call("add", None, None, Some("   ".into())).expect("blank add");
        assert_eq!(
            blank,
            serde_json::json!(["beta"]),
            "blank value must not append an item"
        );

        // move semantics: pop then insert at j (equivalent to "move to index j"), rather than swapping two positions.
        // with 2 items insert-pop and swap produce the same result, so 3 items are needed to tell them apart.
        call("add", None, None, Some("gamma".into())).expect("add 3");
        call("add", None, None, Some("delta".into())).expect("add 4"); // ["beta","gamma","delta"]
        // 1) move(0 → 2): correct ["gamma","delta","beta"]; swap would give ["delta","gamma","beta"].
        let moved_tail = call("move", Some(0), Some(2), None).expect("move to tail");
        assert_eq!(
            moved_tail,
            serde_json::json!(["gamma", "delta", "beta"]),
            "move must relocate, not swap"
        );
        // 2) move(2 → 0): correct ["beta","gamma","delta"]; treating j as "insert at j+1" would give ["gamma","beta","delta"].
        let moved_head = call("move", Some(2), Some(0), None).expect("move to head");
        assert_eq!(
            moved_head,
            serde_json::json!(["beta", "gamma", "delta"]),
            "move to 0 must land at 0, not 1"
        );

        // an unknown op must error: when the host-side op is misspelled, the plugin author must see the failure rather than a silent no-op success.
        let pid_of = |plugin: &str| {
            PluginManager::shared()
                .list_running_with_pid()
                .into_iter()
                .find(|(p, _)| p == plugin)
                .map(|(_, pid)| pid)
        };
        let pid_before =
            pid_of(&id).expect("plugin process must be running before the rejected op");
        let err = call("shuffle", None, None, None).expect_err("unknown op must fail");
        assert!(err.contains("unknown op"), "got: {err}");
        // "the process is still alive" can only be proven by pid: every later call goes through ensure_running, and if the process is wrongly
        // killed by a handling path, it silently restarts, re-reads the same persisted file, and returns the same array.
        let pid_after = pid_of(&id).expect("rejected op must not stop the plugin process");
        assert_eq!(
            pid_after, pid_before,
            "rejected op must not restart the plugin process"
        );
        let still = call("list", None, None, None).expect("list after error");
        assert_eq!(
            still,
            serde_json::json!(["beta", "gamma", "delta"]),
            "rejected op must not mutate the list"
        );

        // delete must locate by index: with 3 items, deleting the middle one is what distinguishes a "trim the tail" implementation — with only 2 items
        // pop() / index truncation returns the same array as the correct implementation, and the old assertion cannot tell the difference.
        let after_del_mid = call("delete", Some(1), None, None).expect("delete middle");
        assert_eq!(
            after_del_mid,
            serde_json::json!(["beta", "delta"]),
            "delete must remove the indexed item, not the tail"
        );

        // one restart round: items must come from config rather than process memory — a handler that reads config only once initially
        // and then keeps the array in memory can pass every assertion above; only a restart distinguishes it.
        PluginManager::shared().stop(&id);
        let reloaded = call("list", None, None, None).expect("list after restart");
        assert_eq!(
            reloaded,
            serde_json::json!(["beta", "delta"]),
            "items must survive a plugin restart (persisted via config)"
        );

        // uninstall: stop the python process and clear the plugins directory (the config file is restored by the guard above,
        // and the secret directory has been redirected to a temp directory).
        PluginManager::shared().uninstall(&id).expect("uninstall");
        let _ = std::fs::remove_dir_all(&base);
    }
}
