//! Sandbox execution layer: macOS seatbelt, Linux bubblewrap, Windows AppContainer.
//!
//! Plugins that declare `sandbox` and whose enforcement policy allows it are spawned via
//! `sandbox-exec -p '<profile>' -- <command> <args>` (process_group applies to
//! sandbox-exec, so group-kill semantics are unchanged). The *plugin* path is still macOS-only:
//! on other platforms the args are generated but the **execution layer is a no-op** for plugins.
//!
//! Known tradeoff (review F7): `(allow process*)` + global `(allow mach-lookup)` is a wide surface — plugins can
//! exec subprocesses and reach any XPC; the current boundary is **write + network** (consistent with the plan). Later, global-name enumeration could
//! tighten the mach surface; tightening exec needs a runtime allowlist (roadmap).
//!
//! The CLI runner (`opencapx sandbox [--profile strict|installer] [--allow-net] [--rw DIR]...
//! [--timeout S] [--check] [--print-profile] -- <cmd>`) has a real backend on all three desktop
//! platforms: macOS → seatbelt (`-p`, profile from `profile_with`), Linux → bubblewrap,
//! Windows → AppContainer (a lowbox profile; grants are ACLs on the AppContainer SID, and
//! `appcontainer.rs` documents how that differs from the seatbelt/bwrap calibration).
//! Contract: fail-open (a missing/broken backend never blocks the command), exit code / stdout /
//! stderr pass through, network denied by default, writes limited to a scratch dir + `--rw` dirs.
//! `--require` opts a specific run OUT of fail-open: no usable backend → refuse (exit 99) + a
//! `sandbox.blocked` audit, instead of warning and running unguarded.

use super::plugin::SandboxDecl;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-plugin data directory root (`~/.opencapx/plugin-data/<id>/`); tests can override with `OPENCAPX_PLUGIN_DATA_DIR`.
// The pure half (grant/deny path mapping, the plan text) compiles everywhere so it is testable
// off-Windows; the Win32 half and the re-exports are gated, which leaves the pure half unused
// on macOS/Linux.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod appcontainer;
mod backends;
mod guard;
mod profile;
mod runner;

pub use appcontainer::*;
pub use backends::*;
pub use guard::*;
pub use profile::*;
pub use runner::*;

#[cfg(test)]
mod tests;
