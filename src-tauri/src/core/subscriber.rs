//! Plugin event subscription registry.
//! A plugin registers interest in a kind via the reverse `plugin.subscribe` JSON-RPC,
//! and the core event-fanout thread uses that to push matching events as `core.event` notifications
//! to the plugin's stdin. Thread-safe; subscriptions accumulate; when a plugin stops, fanout checks liveness on its own.

use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

/// S3 — cap on the number of kinds one plugin may subscribe to (overridden by the `subscribe_max_kinds` setting).
pub const PLUGIN_MAX_KINDS: usize = 32;

struct Inner {
    by_kind: HashMap<String, HashSet<String>>,
}

impl Inner {
    fn new() -> Self {
        Self { by_kind: HashMap::new() }
    }
    fn subscribe(&mut self, plugin: &str, kind: &str) {
        self.by_kind.entry(kind.to_string()).or_default().insert(plugin.to_string());
    }
    fn unsubscribe(&mut self, plugin: &str, kind: &str) {
        if let Some(set) = self.by_kind.get_mut(kind) {
            set.remove(plugin);
            if set.is_empty() {
                self.by_kind.remove(kind);
            }
        }
    }
    /// List all plugin ids interested in a given kind.
    fn subscribers(&self, kind: &str) -> Vec<String> {
        self.by_kind
            .get(kind)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// S3 — per-plugin kind cap (overridden by the `subscribe_max_kinds` setting; default 32).
fn max_kinds_per_plugin() -> usize {
    super::shared_store()
        .and_then(|s| s.lock().ok().and_then(|g| g.get_setting("subscribe_max_kinds")))
        .and_then(|v| v.parse::<usize>().ok())
        .map(|n| n.max(1))
        .unwrap_or(PLUGIN_MAX_KINDS)
}

pub struct SubscriptionRegistry {
    inner: std::sync::Mutex<Inner>,
}

impl SubscriptionRegistry {
    pub fn shared() -> Arc<Self> {
        static R: OnceLock<Arc<SubscriptionRegistry>> = OnceLock::new();
        R.get_or_init(|| {
            Arc::new(Self {
                inner: std::sync::Mutex::new(Inner::new()),
            })
        })
        .clone()
    }

    pub fn subscribe(&self, plugin: &str, kind: &str) -> Result<(), String> {
        if kind.is_empty() {
            return Err("empty kind".into());
        }
        let mut g = self.inner.lock().map_err(|_| "poisoned".to_string())?;
        // S3 — per-plugin kind cap: an already-subscribed kind passes idempotently; a new kind over the cap is rejected (prevents registry bloat).
        let already = g.by_kind.get(kind).map(|s| s.contains(plugin)).unwrap_or(false);
        if !already {
            let count = g.by_kind.values().filter(|s| s.contains(plugin)).count();
            let cap = max_kinds_per_plugin();
            if count >= cap {
                return Err(format!("subscribe limit reached for plugin ({} kinds)", cap));
            }
        }
        g.subscribe(plugin, kind);
        Ok(())
    }

    pub fn unsubscribe(&self, plugin: &str, kind: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.unsubscribe(plugin, kind);
        }
    }

    pub fn subscribers(&self, kind: &str) -> Vec<String> {
        self.inner
            .lock()
            .map(|g| g.subscribers(kind))
            .unwrap_or_default()
    }
}

/// Start the background fanout thread: subscribe to the EventBus and push matching events as
/// `core.event` JSON-RPC notifications to subscriber plugins' stdin. It only pushes, never parses responses.
pub fn spawn_fanout(bus: Arc<super::event::EventBus>, mgr: Arc<super::plugin::PluginManager>) {
    std::thread::spawn(move || {
        let mut rx = bus.subscribe();
        loop {
            match rx.recv() {
                Ok(ev) => {
                    let subs = super::subscriber::SubscriptionRegistry::shared().subscribers(&ev.kind);
                    if subs.is_empty() {
                        continue;
                    }
                    let payload = json!({
                        "kind": ev.kind,
                        "source": ev.source,
                        "timestamp": ev.timestamp,
                        "id": ev.id,
                        "payload": ev.payload,
                    });
                    for plugin_id in subs {
                        if let Some(proc) = mgr.get_process(&plugin_id) {
                            let _ = proc.notify("core.event", payload.clone());
                        }
                    }
                }
                Err(_) => {
                    // bus channel closed:should not happen unless shutdown
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_subscribe_unsubscribe_list() {
        // The registry is a process-global singleton shared by every test in the binary;
        // plain kind names ("k1"/"k2") collide with subscribe_enforces_per_plugin_kind_cap's
        // entries when thread scheduling interleaves them (seen on the Windows runner).
        let tag = std::process::id();
        let (k1, k2) = (format!("k1-{tag}"), format!("k2-{tag}"));
        let r = SubscriptionRegistry::shared();
        r.subscribe("a", &k1);
        r.subscribe("b", &k1);
        r.subscribe("a", &k2);
        let mut s1 = r.subscribers(&k1);
        s1.sort();
        assert_eq!(s1, vec!["a".to_string(), "b".to_string()]);
        let s2 = r.subscribers(&k2);
        assert_eq!(s2, vec!["a".to_string()]);
        r.unsubscribe("a", &k1);
        let s1 = r.subscribers(&k1);
        assert_eq!(s1, vec!["b".to_string()]);
        r.unsubscribe("b", &k1);
        assert!(r.subscribers(&k1).is_empty(), "empty set entry should be removed");
    }

    #[test]
    fn registry_rejects_empty_kind() {
        let r = SubscriptionRegistry::shared();
        assert!(r.subscribe("a", "").is_err());
    }

    /// S3 — per-plugin kind cap: an idempotent repeat does not consume quota; a new kind over the cap is rejected; after unsubscribing you can subscribe again.
    #[test]
    fn subscribe_enforces_per_plugin_kind_cap() {
        let r = SubscriptionRegistry::shared();
        let plugin = format!("com.opencapx.sub-cap-{}", std::process::id());
        r.subscribe(&plugin, "k0").unwrap();
        r.subscribe(&plugin, "k0").unwrap(); // idempotent
        for i in 1..PLUGIN_MAX_KINDS {
            r.subscribe(&plugin, &format!("k{}", i)).unwrap();
        }
        let err = r.subscribe(&plugin, "overflow").unwrap_err();
        assert!(err.contains("limit"), "got: {}", err);
        r.subscribe(&plugin, "k0").unwrap(); // an already-subscribed kind is still idempotent
        r.unsubscribe(&plugin, "k1");
        r.subscribe(&plugin, "overflow").unwrap();
        for i in 0..PLUGIN_MAX_KINDS {
            r.unsubscribe(&plugin, &format!("k{}", i));
        }
        r.unsubscribe(&plugin, "overflow");
    }
}