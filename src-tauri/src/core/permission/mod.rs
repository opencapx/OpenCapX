//! Permission Manager: checked in the Rust Core, before capability execution. See docs/permissions.md.
//! ask decisions go through a runtime prompt: Allow once / Always / Deny; 60s of no action counts as Deny;
//! high-risk permissions are not offered Always. When the UI is absent (test process / frontend not ready), ask quickly resolves to Deny.

// Path-stability layer: the glob re-exports keep every crate::<...>::X path working
// after the module splits; clippy flags the ones nothing outside consumes yet.
#![allow(unused_imports)]
use super::storage::SharedStore;
use rusqlite::params;
use serde_json::json;
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::Emitter;

/// docs/permissions.md: high-risk permissions can only be granted once at a time, Always is not offered.
mod ask;
mod decisions;
mod gate;
mod install;
mod lexicon;
mod view;

pub use ask::*;
pub use decisions::*;
pub use gate::*;
pub use install::*;
pub use lexicon::*;
pub use view::*;

#[cfg(test)]
mod tests;
