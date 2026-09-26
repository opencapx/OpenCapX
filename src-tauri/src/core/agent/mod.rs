//! Agent session model and session storage abstraction. The former state.rs is consolidated here.

// Path-stability layer: the glob re-exports keep every crate::<...>::X path working
// after the module splits; clippy flags the ones nothing outside consumes yet.
#![allow(unused_imports)]
use crate::detector::{detect_claude_stop, looks_like_question, title_from_transcript};
use serde::Serialize;
use std::collections::HashMap;

mod events;
mod hook_fields;
mod ingest;
mod state;
mod store;
mod types;

pub use events::*;
pub use hook_fields::*;
pub use ingest::*;
pub use state::*;
pub use store::*;
pub use types::*;

#[cfg(test)]
mod tests;
