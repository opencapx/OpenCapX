//! Writes/removes OpenCapX's hook entries in each agent's config
//! (~/.claude/settings.json, ...). Entries are identified by their command
//! string so install is idempotent and foreign hooks are never touched.

use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;

mod adapters;
mod entries;
mod hosts;
mod lifecycle;
mod mcp;
mod reconcile;
mod shim;

pub use adapters::*;
pub use entries::*;
pub use hosts::*;
pub use lifecycle::*;
pub use mcp::*;
pub use reconcile::*;
pub use shim::*;

#[cfg(test)]
mod tests;
