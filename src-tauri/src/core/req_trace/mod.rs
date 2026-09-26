//! /rpc request-chain trace: one span tree per Agent tool call, NDJSON persisted to
//! `~/.opencapx/traces/rpc/<agent_id>/<trace_id>.ndjson`。
//!
//! Lightweight self-built span model (no OTel SDK, 0 dependencies):
//! - two-line protocol (span start/end) + event lines, append-only; in-flight requests are visible (start with no end)
//! - thread-local context: http.rs spawns a separate thread per /rpc, giving a natural scope
//!   (Rust std threads have no ALS, so thread-local carries the scope)
//! - with no trace context (tests/direct internal calls) everything is a no-op
//! - write failures are silent (same discipline as plugin_trace: debugging metadata does not affect the main path)
//!
//! Chain shape:
//! rpc (root, attrs: agent/conn/project + durMs on the end line)
//! ├── event: dispatch {tool, requestId, input preview}
//! ├── event: permission.agent {permission, granted}
//! ├── capability.<name>
//! │   ├── event: gate.denied {pluginId, permission}
//! │   └── plugin.<pluginId> {sessionId ← links to a plugin_trace dump, elapsedMs}
//! ├── event: ask.shown / ask.answered / ask.timeout / ask.cancelled
//! └── end line {status, error}

// Path-stability layer: the glob re-exports keep every crate::<...>::X path working
// after the module splits; clippy flags the ones nothing outside consumes yet.
#![allow(unused_imports)]
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

mod export;
mod line;
mod listing;
mod read;
mod span;
mod write;

pub use export::*;
pub use line::*;
pub use listing::*;
pub use read::*;
pub use span::*;
pub use write::*;

#[cfg(test)]
mod tests;
