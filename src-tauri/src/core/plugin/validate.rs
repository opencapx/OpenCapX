//! PluginManager::validate_manifest — the 443-line manifest gate (ids, capabilities, deps, sandbox).
//! Mechanical move from core/plugin.rs.

use super::*;

impl PluginManager {
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
    pub(crate) fn valid_plugin_id(id: &str) -> bool {
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
            return Err(format!(
                "unsupported type {} (v1: pet | capability)",
                m.ptype
            ));
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
                        if !crate::core::capability::is_builtin(id) {
                            return Err(format!(
                            "unknown capability {} (not in v1 registry; new-domain capabilities must be declared in object form)",
                            id
                        ));
                        }
                    }
                    Some((perm, default)) => {
                        // Only the declaration surface is lexically constrained (§4.2): new names supplied by plugins are validated byte-exact
                        if !crate::core::permission::valid_name(id) {
                            return Err(format!(
                            "invalid capability name {:?}: ^[a-z][a-z0-9_-]*(\\.[a-z][a-z0-9_-]*)+$, <=64 chars",
                            id
                        ));
                        }
                        // Reserved IDs may only be providers in string form (§4.2 reserved-domain closure)
                        if crate::core::capability::is_builtin(id)
                            || crate::core::permission::reserved_capability(id)
                        {
                            return Err(format!(
                            "reserved capability {} cannot be declared in object form (use the string form)",
                            id
                        ));
                        }
                        if crate::core::permission::reserved_domain(
                            crate::core::permission::first_segment(id),
                        ) {
                            return Err(format!("capability {} is in a reserved domain", id));
                        }
                        if !crate::core::permission::valid_name(perm) {
                            return Err(format!("invalid permission name {:?}", perm));
                        }
                        // Declarations must not reference reserved permission names (including their granted default, to prevent self-granting via defaults)
                        if crate::core::permission::reserved_domain(
                            crate::core::permission::first_segment(perm),
                        ) {
                            return Err(format!(
                                "declaration may not reference reserved permission {}",
                                perm
                            ));
                        }
                        if crate::core::permission::first_segment(perm)
                            != crate::core::permission::first_segment(id)
                        {
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
            if crate::core::permission::known(p) {
                continue;
            }
            if !crate::core::permission::valid_name(p) {
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
            if crate::core::marketplace::parse_version_lenient(min).is_none() {
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
                return Err(format!(
                    "invalid dependency requirement {:?} for {}",
                    req, dep_id
                ));
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
                && s.key
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
                    return Err(format!(
                        "slider setting {} requires both min and max",
                        s.key
                    ));
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
                validate_localized_text(
                    description,
                    &format!("setting {} description", s.key),
                    None,
                )?;
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
                validate_cond(
                    cond,
                    &format!("setting {} visible", s.key),
                    0,
                    &setting_decls,
                )?;
            }
            if let Some(cond) = &s.disabled {
                validate_cond(
                    cond,
                    &format!("setting {} disabled", s.key),
                    0,
                    &setting_decls,
                )?;
            }
            // P1 — validation rules: type↔control table; pattern must compile with Rust `regex`.
            for rule in &s.validate {
                let Some(rtype) = rule.type_name() else {
                    return Err(format!(
                        "setting {} has an unknown validate rule type",
                        s.key
                    ));
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
}
