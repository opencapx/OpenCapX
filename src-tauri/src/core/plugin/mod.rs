//! Plugin Manager: manifest parsing, install, state machine, process lifecycle. See docs/plugin-manifest.md.
//! State machine: probe_pending → starting → running; plus stopped / error / probe_failed
//! (authoritative value is whatever plugins.status actually stores; see docs/plugin-manifest.md).

use super::process::{PluginProcess, RuntimeSpec};
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

mod dto;
mod install;
mod manager;
mod manifest;
mod ops;
mod reverse;
mod runtime;
mod settings_decl;
mod validate;

pub use dto::*;
pub use manager::*;
pub use manifest::*;
pub(crate) use reverse::*;
pub use runtime::*;
pub use settings_decl::*;

#[cfg(test)]
mod tests;
