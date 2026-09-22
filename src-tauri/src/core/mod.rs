//! OpenCapX Core: agent state, event bus, storage, capability routing, permissions, plugin runtime.
//! See the protocol docs under docs/.

pub mod agent;
pub mod alerting;
pub mod backup;
pub mod browser;
pub mod capability;
pub mod clipboard;
pub mod config;
pub mod declaration;
pub mod event;
pub mod event_replay;
pub mod hotkey;
pub mod health;
pub mod i18n;
pub mod identity;
pub mod kill_switch;
pub mod safe_mode;
pub mod lifecycle_order;
pub mod log_search;
pub mod marketplace;
pub mod notification;
pub mod osperm;
pub mod permission;
pub mod pet;
pub mod petpack;
pub mod pack;
pub mod plugin;
pub mod plugin_deps;
pub mod plugin_metrics;
pub mod plugin_sig;
pub mod plugin_trace;
pub mod process;
pub mod probe;
pub mod profile;
pub mod project;
pub mod rpc;
pub mod req_trace;
pub mod registry;
pub mod retention;
pub mod rules;
pub mod revocation;
pub mod sandbox;
pub mod verify;
pub mod scope;
pub mod sla;
pub mod speech;
pub mod storage;
pub mod subscription;
pub mod vision;
pub mod context;
pub mod automation;
pub mod appctl;
pub mod audio;
pub mod inputctl;
pub mod pim;
pub mod media;
pub mod power;
pub mod settings;
pub mod signing;
pub mod printer;
pub mod messages;
pub mod windowctl;
pub mod subscriber;
pub mod throttle;
pub mod tray;
pub mod transcript;

use storage::SharedStore;
use std::sync::{Mutex, OnceLock, RwLock};

pub use config::PluginConfigEntry;

/// Phase 46 — switched to RwLock<Option<...>> to support profile hot-switching.
static SHARED_STORE: RwLock<Option<SharedStore>> = RwLock::new(None);

/// Phase 46 — a global write lock for tests, preventing multiple parallel tests from calling set_shared_store simultaneously and overwriting each other.
/// Production code does not take this lock (set_shared_store is called once by main at startup).
#[cfg(test)]
pub static TEST_STORE_LOCK: Mutex<()> = Mutex::new(());

/// Registered during main setup, for the /rpc thread and the plugin system to use.
pub fn set_shared_store(s: SharedStore) {
    let mut w = SHARED_STORE.write().expect("store poisoned");
    *w = Some(s);
}

pub fn shared_store() -> Option<SharedStore> {
    SHARED_STORE.read().expect("store poisoned").as_ref().cloned()
}

/// Phase 46 — called on a profile switch: replace the current store, returning the old one.
/// The caller must hold the new store so the old one can drop (it drops after the lock is released).
pub fn replace_shared_store(new: SharedStore) -> Option<SharedStore> {
    let mut w = SHARED_STORE.write().expect("store poisoned");
    let old = w.take();
    *w = Some(new);
    old
}

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Registered during main setup, for paths without an AppHandle context (such as the permission dialog) to emit events to the frontend.
pub fn set_app_handle(h: tauri::AppHandle) {
    let _ = APP_HANDLE.set(h);
}

pub fn app_handle() -> Option<tauri::AppHandle> {
    APP_HANDLE.get().cloned()
}
