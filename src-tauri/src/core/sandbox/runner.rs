//! the `opencapx sandbox` CLI: env policy, audit emission, run dispatch, and the --require fail-closed path.
//! Mechanical move from core/sandbox.rs.

use super::*;
use clap::Parser;

/// Environment hand-over policy for the sandboxed child. The runner spawns the child itself,
/// so the environment is the one boundary seatbelt/bwrap cannot express — it is filtered here.
///
/// - `Strip` (default): drop secret-looking variables. Installers keep PATH/HOME/…, but a
///   downloaded script can no longer read `*_TOKEN`/`*KEY*`/… out of the environment — with
///   the network open in installer mode, that is the exfiltration path seatbelt cannot close.
/// - `Clear`: everything gone except a minimal whitelist (strongest; breaks scripts that
///   legitimately consume CI/registry env).
/// - `Keep`: pass through untouched.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EnvPolicy {
    Keep,
    #[default]
    Strip,
    Clear,
}

/// Substrings (case-insensitive) that mark a variable name as secret material, plus a few
/// exact names that carry credentials indirectly (the ssh agent socket lets a process sign
/// requests; GIT_ASKPASS points at a helper that prints credentials).
const SECRET_ENV_SUBSTRINGS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "API_KEY",
    "PRIVATE_KEY",
    "ACCESS_KEY",
];

const SECRET_ENV_EXACT: &[&str] = &["GITHUB_PAT", "GH_PAT", "SSH_AUTH_SOCK", "GIT_ASKPASS"];

pub fn env_is_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_ENV_EXACT.contains(&upper.as_str())
        || upper.ends_with("_KEY")
        || SECRET_ENV_SUBSTRINGS.iter().any(|s| upper.contains(s))
}

/// Variables that survive `Clear` — a shell inside the sandbox still needs to find binaries,
/// a home, and a temp dir.
const CLEAR_ENV_KEEP: &[&str] = &[
    "PATH", "HOME", "TMPDIR", "USER", "SHELL", "LANG", "LC_ALL", "TERM",
];

pub(crate) fn apply_env_policy(cmd: &mut std::process::Command, policy: EnvPolicy) {
    match policy {
        EnvPolicy::Keep => {}
        EnvPolicy::Strip => {
            let drop: Vec<std::ffi::OsString> = std::env::vars_os()
                .map(|(k, _)| k)
                .filter(|k| env_is_secret(&k.to_string_lossy()))
                .collect();
            for k in drop {
                cmd.env_remove(&k);
            }
        }
        EnvPolicy::Clear => {
            cmd.env_clear();
            for k in CLEAR_ENV_KEEP {
                if let Some(v) = std::env::var_os(k) {
                    cmd.env(k, v);
                }
            }
        }
    }
}

/// Best-effort `sandbox.unguarded` audit when the fence silently disappears (missing/broken
/// backend, un-writable scratch). The Activity Timeline must show that a guarded-shaped run
/// executed WITHOUT the guard — stderr warnings get lost in agent transcripts. Fire-and-forget
/// by design: no queueing, no failure output, the command itself is never blocked or slowed
/// by telemetry (the hook pipeline owns retry/queue semantics; this is a one-shot signal).
fn audit_unguarded(command: &[String], reason: &str) {
    let payload = serde_json::json!({
        "agent": "opencapx",
        "text": format!("sandbox unguarded ({reason})"),
        "tool_input": { "command": command.join(" ") },
        "__rule": "sandbox.unguarded",
        "reason": reason,
    })
    .to_string();
    let creds = crate::core::identity::ensure_registered("opencapx", "sandbox");
    // Queue when the core is down: this signal fires exactly when the app is not running,
    // and the Activity Timeline should still show it after the next start (same queue the
    // hook pipeline uses). Everything else stays fire-and-forget.
    if let (crate::http::Deliver::Unreachable, _) =
        crate::http::post_event_with_body(&payload, creds.as_ref())
    {
        let _ = crate::queue::enqueue(&crate::http::queue_dir(), &payload);
    }
}

/// The `--require` counterpart of [`audit_unguarded`]: the run was refused rather than
/// degraded, and the Timeline should say which side of the contract fired.
fn audit_blocked(command: &[String], reason: &str) {
    let payload = serde_json::json!({
        "agent": "opencapx",
        "text": format!("sandbox blocked ({reason})"),
        "tool_input": { "command": command.join(" ") },
        "__rule": "sandbox.blocked",
        "reason": reason,
    })
    .to_string();
    let creds = crate::core::identity::ensure_registered("opencapx", "sandbox");
    if let (crate::http::Deliver::Unreachable, _) =
        crate::http::post_event_with_body(&payload, creds.as_ref())
    {
        let _ = crate::queue::enqueue(&crate::http::queue_dir(), &payload);
    }
}

/// Knobs for one sandboxed run.
pub struct Policy {
    pub allow_net: bool,
    pub rw: Vec<PathBuf>,
    pub env: EnvPolicy,
}

pub(crate) struct Parsed {
    pub(crate) policy: Policy,
    pub(crate) profile: Mode,
    pub(crate) timeout_secs: Option<u64>,
    pub(crate) require: bool,
    check_only: bool,
    print_profile: bool,
    pub(crate) command: Vec<String>,
}

pub(crate) enum Backend {
    #[cfg(target_os = "macos")]
    Seatbelt,
    #[cfg(target_os = "linux")]
    Bwrap,
    Unavailable(&'static str),
}

#[derive(Parser)]
pub(crate) struct SandboxCli {
    /// strict (default) = network denied, writes fenced to scratch; installer = network open + $HOME writable, minus the secrets/persistence deny list
    #[arg(long, value_parser = ["strict", "installer"])]
    profile: Option<String>,
    /// Allow network access
    #[arg(long)]
    allow_net: bool,
    /// Extra writable directory (repeatable)
    #[arg(long = "rw", value_name = "DIR")]
    rw: Vec<PathBuf>,
    /// strip (default) drops secret-looking variables; clear keeps the bare minimum; keep passes everything through
    #[arg(long, value_parser = ["strip", "keep", "clear"])]
    env: Option<String>,
    /// Timeout in seconds (kills the whole process group)
    #[arg(long)]
    timeout: Option<u64>,
    /// Refuse to run when no sandbox backend is available or it fails to start (exit 99), instead of the fail-open passthrough
    #[arg(long)]
    require: bool,
    /// Check the sandbox backend and exit
    #[arg(long)]
    check: bool,
    /// Print the generated seatbelt profile (diagnostics) and exit
    #[arg(long = "print-profile")]
    print_profile: bool,
    /// The command to run
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

pub(crate) fn parsed_from(cli: SandboxCli) -> Parsed {
    Parsed {
        profile: if cli.profile.as_deref() == Some("installer") {
            Mode::Installer
        } else {
            Mode::Strict
        },
        policy: Policy {
            allow_net: cli.allow_net,
            rw: cli.rw,
            env: match cli.env.as_deref() {
                Some("keep") => EnvPolicy::Keep,
                Some("clear") => EnvPolicy::Clear,
                _ => EnvPolicy::Strip,
            },
        },
        timeout_secs: cli.timeout,
        require: cli.require,
        check_only: cli.check,
        print_profile: cli.print_profile,
        command: cli.command,
    }
}

pub fn run_cli(args: &[String]) -> i32 {
    let cli = match crate::core::cli::parse::<SandboxCli>("opencapx sandbox", args) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let parsed = parsed_from(cli);
    if parsed.check_only {
        return match backend() {
            #[cfg(target_os = "macos")]
            Backend::Seatbelt => {
                println!("backend: seatbelt (sandbox-exec)");
                0
            }
            #[cfg(target_os = "linux")]
            Backend::Bwrap => {
                println!("backend: bwrap");
                0
            }
            Backend::Unavailable(r) => {
                println!("backend: none ({r})");
                1
            }
        };
    }
    if parsed.print_profile {
        return print_profile(&parsed);
    }
    if parsed.command.is_empty() {
        eprintln!("opencapx sandbox: missing command (opencapx sandbox --help)");
        return 2;
    }
    let scratch = match make_scratch() {
        Ok(d) => d,
        Err(e) => {
            return run_unguarded(&parsed, None, &format!("cannot create scratch dir ({e})"));
        }
    };
    let code = match backend() {
        #[cfg(target_os = "macos")]
        Backend::Seatbelt => match run_seatbelt(&parsed, &scratch) {
            Ok(c) => c,
            Err(e) => run_unguarded(
                &parsed,
                Some(&scratch),
                &format!("seatbelt failed to start ({e})"),
            ),
        },
        #[cfg(target_os = "linux")]
        Backend::Bwrap => match run_bwrap(&parsed, &scratch) {
            Ok(c) => c,
            Err(e) => run_unguarded(
                &parsed,
                Some(&scratch),
                &format!("bwrap failed to start ({e})"),
            ),
        },
        Backend::Unavailable(reason) => {
            return run_unguarded(
                &parsed,
                Some(&scratch),
                &format!("no sandbox backend ({reason})"),
            );
        }
    };
    let _ = std::fs::remove_dir_all(&scratch);
    code
}

/// Exit code when `--require` refuses an unguardable run. Distinct from clap's 2 and
/// `--check`'s 1 so rules/scripts can tell "blocked" from "usage" and "probe failed".
pub const EXIT_UNGUARDED: i32 = 99;

/// Pure decision half of the fallback (unit-tested without spawning anything).
pub(crate) fn unguarded_exit(require: bool) -> Option<i32> {
    require.then_some(EXIT_UNGUARDED)
}

/// Shared fallback for every unguardable run (no backend, backend failed to start, unusable
/// scratch). Default contract is fail-open: warn + `sandbox.unguarded` audit + run as-is.
/// `--require` flips it: refuse with exit 99 and a `sandbox.blocked` audit.
fn run_unguarded(parsed: &Parsed, scratch: Option<&Path>, reason: &str) -> i32 {
    if let Some(code) = unguarded_exit(parsed.require) {
        eprintln!("opencapx sandbox: WARNING: {reason}; --require set, refusing to run");
        audit_blocked(&parsed.command, reason);
        if let Some(d) = scratch {
            let _ = std::fs::remove_dir_all(d);
        }
        return code;
    }
    eprintln!("opencapx sandbox: WARNING: {reason}; running WITHOUT sandbox");
    audit_unguarded(&parsed.command, reason);
    run_plain(&parsed.command, parsed.timeout_secs, parsed.policy.env)
}

/// `--print-profile`: show the generated seatbelt profile (diagnostics + review of the deny list).
fn print_profile(parsed: &Parsed) -> i32 {
    #[cfg(target_os = "macos")]
    {
        let Some(home) = crate::core::home_dir() else {
            eprintln!("opencapx sandbox: cannot resolve home");
            return 1;
        };
        let scratch = std::env::temp_dir().join("opencapx-sandbox-preview");
        let tmpdir = std::env::var_os("TMPDIR").map(PathBuf::from);
        println!(
            "{}",
            cli_profile(
                parsed.profile,
                &canonicalize_lossy(&home),
                &scratch,
                tmpdir.as_deref(),
                &parsed.policy.rw,
                parsed.policy.allow_net,
            )
        );
        0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = parsed;
        eprintln!("opencapx sandbox: no seatbelt profile on this platform");
        1
    }
}
