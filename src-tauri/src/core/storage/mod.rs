//! SQLite storage: sessions, event audit ring, plugin/permission/capability tables (used by P4).
//! See docs/plugin-manifest.md, docs/events.md.

use crate::core::agent::{state_str, AgentState, Session, SessionSink};
use crate::core::event::OpencapxEvent;
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const EVENT_RETENTION_DAYS: u64 = 14;

/// Acquire the shared_store lock, call `f(&mut StoreEnum)`, returns None when there is no store. Added in Phase 53.
/// Usage: `let row = with_store(|s| s.list_alerting_severity_hints())?;`
pub fn with_store<R>(f: impl FnOnce(&mut StoreEnum) -> R) -> Option<R> {
    let store = crate::core::shared_store()?;
    let mut guard = store.lock().ok()?;
    Some(f(&mut *guard))
}

pub fn open_default() -> StoreEnum {
    let path = data_dir().join("opencapx.db");
    match Storage::open(&path) {
        Ok(s) => StoreEnum::Db(s),
        Err(e) => {
            eprintln!(
                "[storage] sqlite unavailable ({}), falling back to memory",
                e
            );
            StoreEnum::Mem(crate::core::agent::SessionStore::new())
        }
    }
}

pub fn data_dir() -> std::path::PathBuf {
    if let Some(home) = crate::core::home_dir() {
        return home.join(".opencapx").join("data");
    }
    std::env::temp_dir().join("opencapx-data")
}

mod aggregations;
mod core_tables;
mod correlations;
mod delivery;
mod endpoints;
mod escalations;
mod hints;
mod presets;
mod ratings;
mod recipients;
mod routes;
mod rows;
mod silences;
mod stats;
mod store;

pub use aggregations::*;
pub use core_tables::*;
pub use correlations::*;
pub use delivery::*;
pub use endpoints::*;
pub use escalations::*;
pub use hints::*;
pub use presets::*;
pub use ratings::*;
pub use recipients::*;
pub use routes::*;
pub use rows::*;
pub use silences::*;
pub use stats::*;
pub use store::*;

#[cfg(test)]
mod tests;
