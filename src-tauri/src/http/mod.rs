//! Core local HTTP entry point: hook event reporting (POST /event), MCP forwarding (POST /rpc),
//! Agent identity registration (POST /agents/register). Transport layer; parsing, auth, and persistence live in core.
//! See docs/permissions.md "Agent Identity" for auth: Bearer token + X-OpenCapX-Agent,
//! any of the three checks failing means 401 (code 40101 anonymous/bad token, 40102 revoked) and an auth.rejected audit is written.

// Path-stability layer: the glob re-exports keep every crate::<...>::X path working
// after the module splits; clippy flags the ones nothing outside consumes yet.
#![allow(unused_imports)]
use crate::core::event::{self, EventBus};
use crate::core::identity::{self, Credentials};
use crate::core::storage::SharedStore;
use std::path::PathBuf;
use std::sync::Arc;

pub const LISTEN_ADDR: &str = "127.0.0.1:47628";

/// dev skips auth (local development only, docs/permissions.md "Request Authentication").
/// Compile-time gated: the env lookup only exists in debug builds, so no runtime
/// environment can re-enable the bypass in a release binary.
#[cfg(debug_assertions)]
fn dev_mode() -> bool {
    std::env::var("OPEN_CAPX_DEV")
        .map(|v| v == "1")
        .unwrap_or(false)
}

#[cfg(not(debug_assertions))]
fn dev_mode() -> bool {
    false
}

fn pick_str(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

/// Minimal extractor used by the hook CLI: returns (agent, text).
pub fn parse_hook_payload(stdin: &str) -> (String, String) {
    let v: serde_json::Value = serde_json::from_str(stdin).unwrap_or(serde_json::Value::Null);
    let agent = pick_str(&v, &["agent"]).unwrap_or_else(|| "unknown".into());
    let text = pick_str(&v, &["text", "message", "content"]).unwrap_or_default();
    (agent, text)
}

pub fn queue_dir() -> PathBuf {
    if let Some(home) = crate::core::home_dir() {
        return home.join(".opencapx").join("queue");
    }
    std::env::temp_dir().join("opencapx-queue")
}

/// Client auth headers (two lines with a CRLF prefix). Empty string when there are no credentials.
/// Values pass through `ascii_header_value`: a CR/LF inside the on-disk token or agent_id
/// would otherwise split the header block (header injection / request smuggling).
mod client;
mod server;

pub use client::*;
pub use server::*;

#[cfg(test)]
mod tests;
