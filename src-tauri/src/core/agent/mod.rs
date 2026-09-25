//! Agent session model and session storage abstraction. The former state.rs is consolidated here.

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
