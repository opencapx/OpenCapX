//! spawn specs: RuntimeSpec, EnvPolicy (+Default), SandboxSpec, and the env allowlists.
//! Mechanical move from core/process.rs.

use super::*;

pub struct RuntimeSpec {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// S1 — child-process environment policy: with `isolate=true`, clear the host environment and refill only the whitelist + manifest env.
#[derive(Debug, Clone)]
pub struct EnvPolicy {
    pub isolate: bool,
    /// Incremental whitelist (setting `plugin_env_allowlist`, parsed from a comma-separated value and passed in).
    pub allow: Vec<String>,
}

/// S5b — sandbox-exec wrapper parameters (passed in when the caller decides they are needed; macOS only).
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    pub profile: String,
    /// review F2 — TMPDIR redirect target inside the sandbox (the plugin-data subdirectory; the host tmp is not on the write whitelist).
    pub tmp_dir: std::path::PathBuf,
}

impl Default for EnvPolicy {
    fn default() -> Self {
        Self {
            isolate: true,
            allow: Vec::new(),
        }
    }
}

/// Minimal environment whitelist: what a plugin needs to run (executable lookup / home / temp dir / timezone / locale).
/// Apart from these and the incremental whitelist, plugins cannot read arbitrary host environment variables by default (S1 breaking tightening;
/// `plugin_env_isolation=false` is a one-switch rollback).
const BASE_ENV_ALLOW: &[&str] = &["PATH", "HOME", "TMPDIR", "TZ", "LANG"];

/// Windows equivalents: python.exe (and anything on the CRT) aborts without SYSTEMROOT,
/// and temp/home resolve through TEMP/USERPROFILE rather than TMPDIR/HOME. Without these,
/// every process plugin on Windows died instantly with "timeout waiting for plugin.initialize"
/// while the same plugin ran fine under the unix allowlist.
#[cfg(windows)]
const PLATFORM_ENV_ALLOW: &[&str] = &[
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "COMSPEC",
    "WINDIR",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
];

#[cfg(not(windows))]
const PLATFORM_ENV_ALLOW: &[&str] = &[];

pub(crate) fn env_is_allowed(key: &str, extra: &[String]) -> bool {
    BASE_ENV_ALLOW.contains(&key)
        || PLATFORM_ENV_ALLOW.contains(&key)
        || key.starts_with("LC_")
        || key.starts_with("XDG_")
        || extra.iter().any(|k| k == key)
}

/// Reverse-reply handle: write one line of JSON to the plugin's stdin.
pub type Reply = Arc<dyn Fn(Value) + Send + Sync>;

/// Plugin reverse request/notification callback (value is the full request; when it has an id, respond with reply).
pub type OnReverse = Arc<dyn Fn(Value, Reply) + Send + Sync>;
