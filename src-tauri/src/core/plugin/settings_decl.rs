//! Declarative settings schema (P1/P2/M7/F8): SettingDecl, Cond, ValidateRule and their validation.
//! Mechanical move from core/plugin.rs.

use super::*;

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
    Item {
        value: String,
        label: Option<LocalizedText>,
    },
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
pub(crate) fn num_f64(n: &serde_json::Number) -> f64 {
    n.as_f64().unwrap_or(f64::NAN)
}

/// color default value: ^#[0-9a-fA-F]{3}([0-9a-fA-F]{3})?$.
pub(crate) fn is_hex_color(s: &str) -> bool {
    match s.strip_prefix('#') {
        Some(hex) => matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit()),
        None => false,
    }
}

/// Constructs Rust `regex` accepts but JS `new RegExp` cannot compile (inline flags / Python named groups).
/// pattern is evaluated in two places — Rust (disk write + default self-consistency) and the UI (`new RegExp`) — so an installable
/// manifest must not carry a pattern the UI cannot evaluate (otherwise the UI silently passes and only the disk write errors).
/// `(?:…)` and `(?<name>…)` are accepted by both engines and are not on the list.
pub(crate) const JS_INCOMPATIBLE_REGEX: &[&str] =
    &["(?i", "(?m", "(?s", "(?x", "(?U", "(?-", "(?P<"];

impl ValidateRule {
    /// The rule type name (used in error messages; Unknown → None).
    pub(crate) fn type_name(&self) -> Option<&'static str> {
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
    pub(crate) fn message(&self) -> Option<&LocalizedText> {
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
    pub(crate) fn legal_on(&self, stype: &str) -> bool {
        match self.type_name() {
            Some("required") => {
                matches!(
                    stype,
                    "text" | "textarea" | "secret" | "path" | "number" | "color"
                )
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
pub(crate) fn validate_localized_text(
    text: &LocalizedText,
    owner: &str,
    max_chars: Option<usize>,
) -> Result<(), String> {
    fn check_length(
        value: &str,
        owner: &str,
        locale: Option<&str>,
        max: Option<usize>,
    ) -> Result<(), String> {
        let Some(max) = max else {
            return Ok(());
        };
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
    value.is_null() || value.as_str() == Some("") || value.as_array().is_some_and(|a| a.is_empty())
}

/// P1 — single-rule evaluation (same semantics as TS `rulePasses`: a type mismatch always passes, empty values are left to
/// `required`). Used only at the two Rust enforcement points, not as full type validation.
pub(crate) fn rule_passes(rule: &ValidateRule, value: &serde_json::Value) -> bool {
    if !matches!(rule, ValidateRule::Required { .. }) && is_unset(value) {
        return true;
    }
    match rule {
        ValidateRule::Required { .. } => !is_unset(value),
        ValidateRule::MinLength { value: min, .. } => value
            .as_str()
            .is_none_or(|s| s.chars().count() >= num_f64(min) as usize),
        ValidateRule::MaxLength { value: max, .. } => value
            .as_str()
            .is_none_or(|s| s.chars().count() <= num_f64(max) as usize),
        ValidateRule::Min { value: min, .. } => value.as_f64().is_none_or(|n| n >= num_f64(min)),
        ValidateRule::Max { value: max, .. } => value.as_f64().is_none_or(|n| n <= num_f64(max)),
        ValidateRule::Pattern { regex: re, .. } => value
            .as_str()
            .is_none_or(|s| regex::Regex::new(re).map_or(true, |r| r.is_match(s))),
        ValidateRule::Unknown => true,
    }
}

/// P1 — the first rule that fails (all pass → None).
pub(crate) fn first_failing_rule<'a>(
    rules: &'a [ValidateRule],
    value: &serde_json::Value,
) -> Option<&'a ValidateRule> {
    rules.iter().find(|r| !rule_passes(r, value))
}

/// P1 — `set_setting_value` enforcement: failure → `invalid: <message>` (when a message exists) or
/// `invalid: <rule-type>`。
pub(crate) fn enforce_validate_rules(
    decl: &SettingDecl,
    value: &serde_json::Value,
) -> Result<(), String> {
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
pub(crate) fn validate_cond(
    cond: &Cond,
    owner: &str,
    depth: usize,
    decls: &BTreeMap<&str, &SettingDecl>,
) -> Result<(), String> {
    match cond {
        Cond::Unknown => Err(format!("{}: unknown predicate op", owner)),
        Cond::Equals { key, value } => validate_cond_key(
            owner,
            key,
            "equals",
            Some(std::slice::from_ref(value)),
            decls,
        ),
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
