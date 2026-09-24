//! Command transform operators (declarative, no shell). See `docs/rules.md`.
//!
//! P0 conservative policy: **only handle a single simple command**. Input containing `&&` / `||` / `|` / `;` / newline /
//! or command substitution is never handled (better to miss a match than to match wrongly). Leading wrappers
//! (`sudo` / `env` / `VAR=VAL`) are peeled first, then the transform lands back after the lead — a lesson from real testing:
//! the prepend must land **after** `sudo`. `sudo docker ps` → `sudo <wrapper> docker ps` works;
//! if wrapped as `<wrapper> sudo docker ps`, the wrapper is often not on root's PATH and blows up outright.

use super::{CommandMatch, Rule};
use regex::Regex;

/// Peelable leading wrapper words (not forms with arguments, e.g. `sudo -u bob`; on encountering one, P0 abandons the match).
const WRAPPERS: &[&str] = &["sudo", "doas", "env", "time", "nohup", "nice", "command"];

/// Compound-command / command-substitution markers: on hit, abandon (P0).
const SEPARATORS: &[&str] = &["&&", "||", "|", ";", "\n", "\r", "`", "$("];

/// Split a command into (lead, body). Lead = consecutive leading wrapper words and `KEY=VALUE` assignments.
///
/// ```text
/// "sudo curl x"      -> ("sudo ", "curl x")
/// "env FOO=1 curl x" -> ("env FOO=1 ", "curl x")
/// "curl x"           -> ("", "curl x")
/// ```
pub fn split_lead(cmd: &str) -> (&str, &str) {
    let mut idx = 0usize;
    loop {
        let rest = &cmd[idx..];
        let trimmed = rest.trim_start();
        idx += rest.len() - trimmed.len();
        let tok_end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        let tok = &trimmed[..tok_end];
        if tok.is_empty() {
            break;
        }
        if WRAPPERS.contains(&tok) || is_env_assignment(tok) {
            idx += tok_end;
        } else {
            break;
        }
    }
    (&cmd[..idx], &cmd[idx..])
}

/// `FOO=bar` (identifier = value). Used to peel environment assignments as lead.
/// Shared with the danger guard (`core::sandbox`), which peels the same forms.
pub fn is_env_assignment(tok: &str) -> bool {
    let Some((name, _)) = tok.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The transform result of one rule on one command; `None` = unchanged.
pub fn apply(rule: &Rule, cmd: &str) -> Option<String> {
    if cmd_has_separator(cmd) {
        return None;
    }
    let (lead, body) = split_lead(cmd);
    if body.is_empty() {
        return None;
    }
    let m = rule.when.command.as_ref()?;
    if !matches_command(m, body) {
        return None;
    }
    let unless_matches = rule
        .unless
        .as_ref()
        .and_then(|u| u.command.as_ref())
        .map(|um| matches_command(um, body))
        .unwrap_or(false);
    if unless_matches {
        return None;
    }

    let transformed = transform_body(rule, body, cmd)?;
    if transformed == body {
        return None;
    }
    let out = format!("{lead}{transformed}");
    if out == cmd {
        return None;
    }
    Some(out)
}

fn cmd_has_separator(cmd: &str) -> bool {
    SEPARATORS.iter().any(|s| cmd.contains(s))
}

fn matches_command(m: &CommandMatch, cmd: &str) -> bool {
    // Every field that is present must match (AND).
    let mut any = false;
    if let Some(p) = &m.prefix {
        any = true;
        if !cmd.starts_with(p.as_str()) {
            return false;
        }
    }
    if let Some(b) = &m.binary {
        any = true;
        if first_token(cmd) != b.as_str() {
            return false;
        }
    }
    if let Some(r) = &m.regex {
        any = true;
        match Regex::new(r) {
            // Anchored: the match must start at 0 (safe even if the user writes `curl` without `^curl`).
            Ok(re) => match re.find(cmd) {
                Some(mat) if mat.start() == 0 => {}
                _ => return false,
            },
            Err(_) => return false,
        }
    }
    any
}

fn first_token(s: &str) -> &str {
    let t = s.trim_start();
    let end = t.find(char::is_whitespace).unwrap_or(t.len());
    &t[..end]
}

fn transform_body(rule: &Rule, body: &str, cmd: &str) -> Option<String> {
    let t = &rule.then;
    if let Some(p) = &t.prepend {
        if p.trim().is_empty() {
            return None;
        }
        // Idempotent: if it is already `<p> ...`, do not wrap again.
        if body == p.as_str() || body.starts_with(&format!("{p} ")) {
            return None;
        }
        return Some(format!("{p} {body}"));
    }
    if let Some(rb) = &t.replace_binary {
        let tok = first_token(body);
        if tok != rb.from {
            return None;
        }
        let start = body.len() - body.trim_start().len();
        return Some(format!(
            "{}{}{}",
            &body[..start],
            rb.to,
            &body[start + tok.len()..]
        ));
    }
    if let Some(env) = &t.env {
        if env.is_empty() {
            return None;
        }
        let prefix: String = env.iter().map(|(k, v)| format!("{k}={v} ")).collect();
        // Idempotent: if the whole command already carries the same assignment prefix, do not add it (the assignment is peeled into lead by split_lead,
        // so the comparison here must be against the full cmd, not body).
        if cmd.starts_with(&prefix) {
            return None;
        }
        return Some(format!("{prefix}{body}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rules::RulesFile;

    fn rule(json: &str) -> Rule {
        RulesFile::parse(json)
            .unwrap()
            .rules
            .into_iter()
            .next()
            .unwrap()
    }

    fn prepend_rule() -> Rule {
        rule(
            r#"{"version":1,"rules":[
              {"id":"sandbox-curl","when":{"stage":"tool_pre","command":{"prefix":"curl "}},
               "then":{"action":"rewrite","prepend":"sandbox"}}]}"#,
        )
    }

    #[test]
    fn split_lead_peels_wrappers_and_assignments() {
        assert_eq!(split_lead("curl x"), ("", "curl x"));
        assert_eq!(split_lead("sudo curl x"), ("sudo ", "curl x"));
        assert_eq!(split_lead("env FOO=1 curl x"), ("env FOO=1 ", "curl x"));
        assert_eq!(split_lead("FOO=1 curl x"), ("FOO=1 ", "curl x"));
        assert_eq!(split_lead(""), ("", ""));
    }

    #[test]
    fn prepends_simple() {
        let r = prepend_rule();
        assert_eq!(
            apply(&r, "curl https://x"),
            Some("sandbox curl https://x".into())
        );
    }

    #[test]
    fn preserves_sudo_lead() {
        let r = prepend_rule();
        // Critical: it must be "sudo sandbox curl ...", never "sandbox sudo curl ..."
        assert_eq!(
            apply(&r, "sudo curl https://x"),
            Some("sudo sandbox curl https://x".into())
        );
    }

    #[test]
    fn idempotent_when_already_wrapped() {
        let r = prepend_rule();
        assert_eq!(apply(&r, "sandbox curl https://x"), None);
    }

    #[test]
    fn refuses_compound_with_separators() {
        let r = prepend_rule();
        assert_eq!(apply(&r, "curl x && git status"), None);
        assert_eq!(apply(&r, "curl x | grep y"), None);
        assert_eq!(apply(&r, "curl x; ls"), None);
    }

    #[test]
    fn refuses_multiline_and_cr() {
        let r = prepend_rule();
        assert_eq!(apply(&r, "curl x\ngit status"), None);
        assert_eq!(apply(&r, "curl x\rgit status"), None);
    }

    #[test]
    fn no_match_returns_none() {
        let r = prepend_rule();
        assert_eq!(apply(&r, "ls /tmp"), None);
    }

    #[test]
    fn replaces_binary_and_keeps_lead() {
        let r = rule(
            r#"{"version":1,"rules":[
              {"id":"scurl","when":{"stage":"tool_pre","command":{"binary":"curl"}},
               "then":{"action":"rewrite","replace_binary":{"from":"curl","to":"scurl"}}}]}"#,
        );
        assert_eq!(apply(&r, "curl https://x"), Some("scurl https://x".into()));
        assert_eq!(
            apply(&r, "sudo curl https://x"),
            Some("sudo scurl https://x".into())
        );
        assert_eq!(apply(&r, "wget https://x"), None);
    }

    #[test]
    fn env_prefix_is_added_once() {
        let r = rule(
            r#"{"version":1,"rules":[
              {"id":"proxy","when":{"stage":"tool_pre","command":{"prefix":"curl "}},
               "then":{"action":"rewrite","env":{"HTTPS_PROXY":"http://127.0.0.1:9"}}}]}"#,
        );
        assert_eq!(
            apply(&r, "curl https://x"),
            Some("HTTPS_PROXY=http://127.0.0.1:9 curl https://x".into())
        );
        assert_eq!(
            apply(&r, "HTTPS_PROXY=http://127.0.0.1:9 curl https://x"),
            None
        );
    }

    #[test]
    fn unless_suppresses_match() {
        let r = rule(
            r#"{"version":1,"rules":[
              {"id":"sandbox-curl","when":{"stage":"tool_pre","command":{"prefix":"curl "}},
               "unless":{"stage":"tool_pre","command":{"prefix":"sandbox "}},
               "then":{"action":"rewrite","prepend":"sandbox"}}]}"#,
        );
        assert_eq!(apply(&r, "sandbox curl x"), None);
        assert_eq!(apply(&r, "curl x"), Some("sandbox curl x".into()));
    }

    #[test]
    fn regex_must_be_anchored() {
        let r = rule(
            r#"{"version":1,"rules":[
              {"id":"re","when":{"stage":"tool_pre","command":{"regex":"curl"}},
               "then":{"action":"rewrite","prepend":"sandbox"}}]}"#,
        );
        assert_eq!(apply(&r, "curl x"), Some("sandbox curl x".into()));
        // Unanchored -> does not match a curl appearing in the middle
        assert_eq!(apply(&r, "echo curl"), None);
    }
}
