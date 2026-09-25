//! S5b — macOS sandbox execution layer (seatbelt / `sandbox-exec`).
//!
//! Plugins that declare `sandbox` and whose enforcement policy allows it are spawned via
//! `sandbox-exec -p '<profile>' -- <command> <args>` (process_group applies to
//! sandbox-exec, so group-kill semantics are unchanged). On non-macOS the args are still generated but the **execution layer is a no-op**
//! for plugins (Linux bubblewrap / Windows AppContainer are explicitly out of scope, listed in the roadmap).
//!
//! Known tradeoff (review F7): `(allow process*)` + global `(allow mach-lookup)` is a wide surface — plugins can
//! exec subprocesses and reach any XPC; the current boundary is **write + network** (consistent with the plan). Later, global-name enumeration could
//! tighten the mach surface; tightening exec needs a runtime allowlist (roadmap).
//!
//! The same calibrated profile shape now backs the CLI runner
//! (`opencapx sandbox [--allow-net] [--rw DIR]... [--timeout S] [--check] -- <cmd>`):
//! macOS → seatbelt (`-p`, profile from `profile_with`), Linux → bubblewrap, other platforms → warn + run
//! unguarded (W1; the microVM tier via microsandbox is the planned strong option for Windows).
//! Contract: fail-open (a missing/broken backend never blocks the command), exit code / stdout /
//! stderr pass through, network denied by default, writes limited to a scratch dir + `--rw` dirs.
//! `--require` opts a specific run OUT of fail-open: no usable backend → refuse (exit 99) + a
//! `sandbox.blocked` audit, instead of warning and running unguarded.

use super::plugin::SandboxDecl;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-plugin data directory root (`~/.opencapx/plugin-data/<id>/`); tests can override with `OPENCAPX_PLUGIN_DATA_DIR`.
mod backends;
mod guard;
mod profile;
mod runner;

pub use backends::*;
pub use guard::*;
pub use profile::*;
pub use runner::*;

#[cfg(test)]
mod tests;
