//! the clap CLI surface (Cli/Cmd) plus connect/install-CLI and the signing toolchain subcommands.
//! Mechanical move from main.rs.

use super::*;
use clap::Parser;

/// `opencapx install-cli` — symlink the stable shim into PATH so `opencapx` resolves from any
/// terminal. The Settings row calls `cli_install::install(true)` directly.
pub(crate) fn run_install_cli(elevate: bool) -> ! {
    match cli_install::install(elevate) {
        Ok(msg) => {
            eprintln!("install-cli: {msg}");
            if !cli_install::on_path() {
                eprintln!("install-cli: note: /usr/local/bin is not on this shell's PATH — add it to use the command by name");
            }
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("install-cli: {e}");
            std::process::exit(1);
        }
    }
}

/// `opencapx uninstall-cli` — remove the symlink (only if it is ours).
pub(crate) fn run_uninstall_cli(elevate: bool) -> ! {
    match cli_install::uninstall(elevate) {
        Ok(msg) => {
            eprintln!("uninstall-cli: {msg}");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("uninstall-cli: {e}");
            std::process::exit(1);
        }
    }
}

pub(crate) fn run_connect(kind: &str) -> ! {
    let catalog = hooks::catalog();
    if !catalog.iter().any(|a| a.kind == kind) {
        eprintln!(
            "connect: unknown agent: {} (options: {})",
            kind,
            catalog
                .iter()
                .map(|a| a.kind.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        std::process::exit(2);
    }
    match hooks::ensure_installed(kind) {
        Ok(()) => eprintln!("connect: hooks in place ({})", hooks::display_name(kind)),
        Err(e) => {
            eprintln!("connect: failed to write hooks: {}", e);
            std::process::exit(1);
        }
    }
    if hooks::supports_mcp(kind) {
        match hooks::ensure_mcp(kind) {
            Ok((path, written)) => {
                if written {
                    eprintln!("connect: MCP server written to {}", path);
                } else {
                    eprintln!("connect: MCP server already in place (unchanged)");
                }
            }
            Err(e) => {
                eprintln!("connect: failed to write MCP config: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        eprintln!("connect: {} is hooks-only — no MCP target yet (session state + command-rule rewrites only)", hooks::display_name(kind));
    }
    // Repair pass: entries written by an earlier build (or a dev checkout that has since
    // moved / been cleaned) still bake a dead binary path — repoint them at the stable shim.
    // Also backfills the codex identity env into MCP blocks written before it existed.
    let fixed = hooks::refresh_installations();
    if fixed > 0 {
        let noun = if fixed == 1 { "entry" } else { "entries" };
        eprintln!(
            "connect: repaired {} config {} (stable CLI path / codex identity env)",
            fixed, noun
        );
    }
    if hooks::supports_mcp(kind) {
        eprintln!(
            "connect: done. Restart {} and have it call opencapx.list_capabilities to self-test;",
            hooks::display_name(kind)
        );
        eprintln!("connect: auth is auto-registered at opencapx mcp startup; the config file contains no credentials.");
    } else {
        eprintln!("connect: done. Restart {} — the hooks report session state and apply command-rule rewrites by rule.", hooks::display_name(kind));
    }
    if !cli_install::on_path() {
        eprintln!("connect: tip: `opencapx` is not on PATH — run `opencapx install-cli` (or install it from Settings → General) to use the command by name");
    }
    std::process::exit(0);
}

pub(crate) fn hex_encode_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Parse a 32-byte seed: 64 hex characters, or `@path` pointing to a file containing hex (after trim).
pub(crate) fn read_seed_arg(arg: &str) -> Result<[u8; 32], String> {
    let text = if let Some(path) = arg.strip_prefix('@') {
        std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read key file {}: {}", path, e))?
    } else {
        arg.to_string()
    };
    let text = text.trim();
    if text.len() != 64 {
        return Err(format!(
            "seed must be 64 hex characters, got {}",
            text.len()
        ));
    }
    let mut seed = [0u8; 32];
    for i in 0..32 {
        seed[i] = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("seed hex @{}: {}", i * 2, e))?;
    }
    Ok(seed)
}

pub(crate) fn run_keygen(out: &str) -> ! {
    let out_path = PathBuf::from(out);
    // WHY: the seed is the identity root; silently overwriting it would permanently lose the mapping between the old private key and published signed packages;
    // better to error and make the author explicitly rename/delete than to overwrite destructively.
    if out_path.exists() {
        eprintln!("keygen: {} already exists, refusing to overwrite (use a different --out or delete it first)", out);
        std::process::exit(1);
    }
    let mut seed = [0u8; 32];
    if let Err(e) = getrandom::getrandom(&mut seed) {
        eprintln!("keygen: failed to obtain randomness: {}", e);
        std::process::exit(1);
    }
    let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
    let pk_hex = hex_encode_lower(sk.verifying_key().as_bytes());
    if let Err(e) = std::fs::write(&out_path, format!("{}\n", hex_encode_lower(&seed))) {
        eprintln!("keygen: failed to write {}: {}", out, e);
        std::process::exit(1);
    }
    println!("{}", serde_json::json!({"publicKey": pk_hex, "out": out}));
    // keyId is chosen by the publisher (the CLI cannot know it); only suggest the entry shape for copying.
    println!(
        "suggested trusted-keys entry: {{\"<keyId>\":{{\"alg\":\"ed25519\",\"publicKey\":\"{}\"}}}}",
        pk_hex
    );
    std::process::exit(0);
}

pub(crate) fn run_pack(dir: &std::path::Path, key_arg: &str, key_id: &str, out: Option<&str>) -> ! {
    let manifest_path = dir.join("opencapx-plugin.json");
    let manifest_text = match std::fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("pack: failed to read {}: {}", manifest_path.display(), e);
            std::process::exit(1);
        }
    };
    let manifest: serde_json::Value = match serde_json::from_str(&manifest_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pack: manifest is invalid JSON: {}", e);
            std::process::exit(1);
        }
    };
    // WHY: Python (json) and serde_json (ryu) serialize floats/out-of-range integers differently, which would make
    // cross-language digests silently diverge; this is the pack boundary, nothing is on disk yet, so refuse and exit.
    if let Err(e) = crate::core::signing::ensure_signable_numbers(&manifest) {
        eprintln!("pack: {}", e);
        std::process::exit(1);
    }
    let Some(id) = manifest
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
    else {
        eprintln!("pack: manifest is missing id");
        std::process::exit(1);
    };
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    let out = out
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}-{}.ocplugin", id, version));

    let seed = match read_seed_arg(&key_arg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pack: {}", e);
            std::process::exit(1);
        }
    };
    match crate::core::pack::pack_dir(dir, &seed, &key_id, std::path::Path::new(&out)) {
        Ok(digest) => {
            println!(
                "{}",
                serde_json::json!({"out": out, "digest": digest, "keyId": key_id})
            );
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("pack: {}", e);
            std::process::exit(1);
        }
    }
}

pub(crate) fn run_verify(file: &str, trusted_keys: Option<&str>) -> ! {
    if let Some(tk) = trusted_keys {
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", tk);
    }
    let path = std::path::Path::new(file);
    // WHY: plugin_sig::verify conservatively returns Unsigned for unopenable files (install will block again),
    // but the CLI must distinguish "IO/format error (exit 1)" from "valid but unsigned (exit 2)", so self-check first.
    if !path.is_file() {
        eprintln!(
            "verify: file does not exist or is not a regular file: {}",
            file
        );
        std::process::exit(1);
    }
    match std::fs::File::open(path)
        .ok()
        .and_then(|f| zip::ZipArchive::new(f).ok())
    {
        Some(_) => {}
        None => {
            eprintln!(
                "verify: not a valid .ocplugin (zip) or cannot open: {}",
                file
            );
            std::process::exit(1);
        }
    }

    let outcome = crate::core::plugin_sig::verify(path);
    let key_id = match &outcome {
        crate::core::plugin_sig::VerifyOutcome::Trusted { key_id }
        | crate::core::plugin_sig::VerifyOutcome::UnknownKey { key_id }
        | crate::core::plugin_sig::VerifyOutcome::BadSignature { key_id } => Some(key_id.clone()),
        _ => None,
    };
    let mut obj = serde_json::Map::new();
    obj.insert(
        "status".into(),
        serde_json::Value::String(outcome.label().to_string()),
    );
    if let Some(kid) = &key_id {
        obj.insert("keyId".into(), serde_json::Value::String(kid.clone()));
    }
    println!("{}", serde_json::Value::Object(obj));
    eprintln!("verify: {} -> {}", file, outcome.label());

    use crate::core::plugin_sig::VerifyOutcome as O;
    let code = match outcome {
        O::Trusted { .. } => 0,
        O::Unsigned | O::UnknownKey { .. } => 2,
        O::HashMismatch { .. } | O::BadSignature { .. } | O::MalformedSignature => 1,
    };
    std::process::exit(code);
}

/// M3 — sign the registry index with the official signer: inject/overwrite `indexSignature`.
/// Default output = index.json in the same directory as the input (hosting convention).
pub(crate) fn run_sign_index(input: &str, key_arg: &str, key_id: &str, out: Option<&str>) -> ! {
    let seed = match read_seed_arg(&key_arg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sign-index: {}", e);
            std::process::exit(1);
        }
    };
    let raw = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("sign-index: failed to read {}: {}", input, e);
            std::process::exit(1);
        }
    };
    let signed = match crate::core::registry::sign_index(&raw, &seed, &key_id) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sign-index: {}", e);
            std::process::exit(1);
        }
    };
    let out = out.map(str::to_string).unwrap_or_else(|| {
        std::path::Path::new(input)
            .with_file_name("index.json")
            .display()
            .to_string()
    });
    if let Err(e) = std::fs::write(&out, &signed) {
        eprintln!("sign-index: failed to write {}: {}", out, e);
        std::process::exit(1);
    }
    println!("{}", serde_json::json!({"out": out, "keyId": key_id}));
    std::process::exit(0);
}

/// M3 — verify the registry index: official public keys = source constants ∪ `OPENCAPX_REGISTRY_OFFICIAL_KEYS`.
/// M6 — F10 automated gate (registry CI / local pre-run): full verification + JSON report + exit code.
pub(crate) fn run_verify_package(file: &str, keys: Option<&str>, index: Option<&str>) -> ! {
    let keys_path = keys
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::core::plugin_sig::trusted_keys_path);
    let keys = crate::core::plugin_sig::load_trusted_keys_from(&keys_path);
    if keys.is_empty() {
        eprintln!(
            "verify-package: warning: {} has no valid trust entries (only the registry index is available)",
            keys_path.display()
        );
    }
    let index = match index {
        Some(p) => {
            let raw = match std::fs::read(&p) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("verify-package: failed to read index {}: {}", p, e);
                    std::process::exit(1);
                }
            };
            match crate::core::registry::verify_index(&raw) {
                Ok(idx) => Some(idx),
                Err(e) => {
                    eprintln!("verify-package: index signature verification failed: {}", e);
                    println!(
                        "{}",
                        serde_json::json!({"ok": false, "checks": [{"id": "index", "ok": false, "detail": e}]})
                    );
                    std::process::exit(1);
                }
            }
        }
        None => None,
    };
    let report = crate::core::verify::verify_package(
        std::path::Path::new(file),
        &keys,
        index.as_ref(),
        &crate::core::verify::GateLimits::default(),
    );
    for c in &report.checks {
        eprintln!("{} {}: {}", if c.ok { "✓" } else { "✗" }, c.id, c.detail);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );
    std::process::exit(if report.ok { 0 } else { 1 });
}

pub(crate) fn run_verify_index(input: &str) -> ! {
    let raw = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("verify-index: failed to read {}: {}", input, e);
            std::process::exit(1);
        }
    };
    match crate::core::registry::verify_index(&raw) {
        Ok(idx) => {
            println!(
                "{}",
                serde_json::json!({
                    "status": "valid",
                    "schemaVersion": idx.schema_version,
                    "generatedAt": idx.generated_at,
                    "publishers": idx.publishers.len(),
                    "entries": idx.entries.len(),
                    "revokedKeys": idx.revoked_keys.len(),
                })
            );
            eprintln!("verify-index: {} -> valid", input);
            std::process::exit(0);
        }
        Err(e) => {
            println!("{}", serde_json::json!({"status": "invalid"}));
            eprintln!("verify-index: {} -> {}", input, e);
            std::process::exit(1);
        }
    }
}

#[derive(Parser)]
#[command(
    name = "opencapx",
    version,
    about = "OpenCapX — the desktop body for AI agents (GUI + CLI)"
)]
pub(crate) struct Cli {
    /// Start with third-party plugins disabled (see Settings → General → Safe mode)
    #[arg(long)]
    pub(crate) safe_mode: bool,
    #[command(subcommand)]
    pub(crate) command: Option<Cmd>,
}

/// One variant per user-facing subcommand. Commands whose detailed parsing still lives in their
/// `run_cli` take the remaining tokens verbatim, and `disable_help_flag` keeps their own `--help`
/// behavior unchanged until they are migrated.
#[derive(clap::Subcommand)]
pub(crate) enum Cmd {
    /// Write the agent's hook + MCP config (an unknown name lists the hosts)
    Connect { agent: String },
    /// Run a command behind the OS guard (seatbelt / bwrap)
    #[command(disable_help_flag = true)]
    Sandbox {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print the rewritten form of a command (does not execute it)
    Rewrite {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Command rules: list / explain / trust / untrust
    #[command(disable_help_flag = true)]
    Rules {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Danger-guard installer domains: trust / untrust / list / mode / env
    #[command(disable_help_flag = true)]
    Guard {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Automation rules: list / add / remove
    #[command(disable_help_flag = true)]
    Automation {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Install the `opencapx` command into PATH
    InstallCli {
        /// macOS: use the system authorization dialog when /usr/local/bin is not writable
        #[arg(long)]
        elevate: bool,
    },
    /// Remove the `opencapx` command from PATH
    UninstallCli {
        /// macOS: use the system authorization dialog when /usr/local/bin is not writable
        #[arg(long)]
        elevate: bool,
    },
    /// Generate a plugin signing key
    Keygen {
        /// Output path for the seed file
        #[arg(long, default_value = "opencapx-signing.key.hex")]
        out: String,
    },
    /// Package and sign a plugin directory
    Pack {
        /// Plugin directory to package
        dir: PathBuf,
        /// Signing seed: 64 hex chars, or @path to a file containing hex
        #[arg(long, value_name = "SEED|@FILE")]
        key: String,
        /// Publisher key id (goes into the signature)
        #[arg(long)]
        key_id: String,
        /// Output path (default: <id>-<version>.ocplugin)
        #[arg(long)]
        out: Option<String>,
    },
    /// Verify a signed plugin file
    Verify {
        /// The signed plugin file (.ocplugin)
        file: String,
        /// trusted-keys.json to verify against (default: the installed one)
        #[arg(long)]
        trusted_keys: Option<String>,
    },
    /// Verify a packed `.ocplugin` against trusted keys / the registry index
    VerifyPackage {
        /// The packed .ocplugin to verify
        file: String,
        /// trusted-keys.json (default: the installed one)
        #[arg(long)]
        keys: Option<String>,
        /// Registry index to consult
        #[arg(long)]
        index: Option<String>,
    },
    /// Sign a plugin registry index
    SignIndex {
        /// Unsigned index JSON
        input: String,
        /// Signing seed: 64 hex chars, or @path to a file containing hex
        #[arg(long, value_name = "SEED|@FILE")]
        key: String,
        /// Publisher key id (goes into the signature)
        #[arg(long)]
        key_id: String,
        /// Output path (default: index.json next to the input)
        #[arg(long)]
        out: Option<String>,
    },
    /// Verify a plugin registry index
    VerifyIndex {
        /// The index.json to verify
        input: String,
    },
}
