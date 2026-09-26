//! Plugin child process: NDJSON JSON-RPC 2.0 over stdio. See docs/plugin-protocol.md.

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
