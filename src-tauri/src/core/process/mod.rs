//! Plugin child process: NDJSON JSON-RPC 2.0 over stdio. See docs/plugin-protocol.md.

// Path-stability layer: the glob re-exports keep every crate::<...>::X path working
// after the module splits; clippy flags the ones nothing outside consumes yet.
#![allow(unused_imports)]
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod child;
mod io;
mod spec;

pub use child::*;
pub use io::*;
pub use spec::*;

#[cfg(test)]
mod tests;
