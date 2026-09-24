//! Phase 43 — Plugin log grep / regex / tail filtering
//!
//! Reuses `storage::list_events("plugin.log", big_limit)` to pull a window, then filters in memory by level /
//! pluginId / substring / since_ts. The SQL layer only queries the plugin.log type; all filtering happens in Rust,
//! and O(n) is entirely sufficient at a scale of 200~2000 entries.
//!
//! The return shape aligns with Phase 15's `LogEntry`: `{ id, kind, plugin_id, level, source, message, timestamp }`.

use serde::Serialize;

use super::event::OpencapxEvent;
use super::storage::{SharedStore, StoreEnum};

#[derive(Debug, Clone, Serialize)]
pub struct LogEntryDto {
    pub id: String,
    pub kind: String,
    pub plugin_id: String,
    pub level: String,
    pub source: String,
    pub message: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct LogFilter {
    /// Case-insensitive substring query (empty = no filtering). Phase 43 does not support true regex yet (RegExp in the JS layer is more flexible).
    pub query: String,
    /// "info" / "warn" / "error" / "debug" (any case); empty = no filtering.
    pub level: String,
    /// Exact plugin id match (empty = no filtering).
    pub plugin_id: String,
    /// tail mode: only return events with ts > since_ts; a tail client fills the previous round's max_ts when polling.
    pub since_ts: Option<u64>,
    /// Cap, default 500, clamped to [1, 5000].
    pub limit: usize,
}

impl Default for LogFilter {
    fn default() -> Self {
        Self {
            query: String::new(),
            level: String::new(),
            plugin_id: String::new(),
            since_ts: None,
            limit: 500,
        }
    }
}

fn event_to_log(e: OpencapxEvent) -> LogEntryDto {
    let p = &e.payload;
    let get = |k: &str| -> String { p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string() };
    LogEntryDto {
        id: e.id,
        kind: e.kind,
        plugin_id: get("pluginId"),
        level: {
            let l = get("level");
            if l.is_empty() {
                "info".to_string()
            } else {
                l
            }
        },
        source: get("source"),
        message: get("message"),
        timestamp: e.timestamp,
    }
}

/// Pull a plugin.log window (>3x filter.limit, since filtering still lies ahead). Mem store returns empty.
fn raw_logs(store: &SharedStore, prefix: &str, limit: usize) -> Vec<OpencapxEvent> {
    let s = match store.lock() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match &*s {
        StoreEnum::Db(storage) => storage.list_events(prefix, limit),
        StoreEnum::Mem(_) => Vec::new(),
    }
}

/// Pull a window from the store + filter by LogFilter + return (non-tail by ts DESC, tail by ts ASC).
pub fn search_logs(store: &SharedStore, filter: LogFilter) -> Vec<LogEntryDto> {
    let limit = filter.limit.clamp(1, 5000);
    let window = (limit.saturating_mul(3)).clamp(200, 5000);
    let raw = raw_logs(store, "plugin.log", window);

    let q = filter.query.to_lowercase();
    let lvl = filter.level.to_lowercase();
    let since = filter.since_ts.unwrap_or(0);

    let mut out: Vec<LogEntryDto> = raw
        .into_iter()
        .map(event_to_log)
        .filter(|e| {
            if since > 0 && e.timestamp <= since {
                return false;
            }
            if !lvl.is_empty() && e.level.to_lowercase() != lvl {
                return false;
            }
            if !filter.plugin_id.is_empty() && e.plugin_id != filter.plugin_id {
                return false;
            }
            if !q.is_empty()
                && !e.message.to_lowercase().contains(&q)
                && !e.plugin_id.to_lowercase().contains(&q)
                && !e.source.to_lowercase().contains(&q)
            {
                return false;
            }
            true
        })
        .collect();

    if filter.since_ts.is_some() {
        out.sort_by_key(|e| e.timestamp);
    } else {
        out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    }
    if out.len() > limit {
        out.truncate(limit);
    }
    out
}

/// For the frontend tail mode: compute the max timestamp among search results (0 means empty).
pub fn max_timestamp(entries: &[LogEntryDto]) -> u64 {
    entries.iter().map(|e| e.timestamp).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event::OpencapxEvent;
    use crate::core::storage::{Storage, StoreEnum};
    use serde_json::json;

    fn fresh_store(label: &str) -> SharedStore {
        let dir = std::env::temp_dir().join(format!(
            "opencapx-logsearch-{}-{}",
            std::process::id(),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = std::sync::Arc::new(std::sync::Mutex::new(StoreEnum::Db(
            Storage::open(&dir.join("t.db")).unwrap(),
        )));
        {
            let mut s = store.lock().unwrap();
            for i in 0..8u64 {
                let mut ev = OpencapxEvent::new(
                    "plugin.log",
                    "core",
                    json!({
                        "pluginId": if i % 2 == 0 { "plug-a" } else { "plug-b" },
                        "level": match i % 3 {
                            0 => "info",
                            1 => "warn",
                            _ => "error",
                        },
                        "source": if i < 4 { "stderr" } else { "reverse" },
                        "message": format!("hello world line {i}"),
                    }),
                );
                ev.timestamp = 1000 + i;
                s.log_event(&ev);
            }
            s.log_event(&OpencapxEvent::new(
                "permission.granted",
                "core",
                json!({"pluginId": "plug-a"}),
            ));
            s.log_event(&OpencapxEvent::new(
                "plugin.lifecycle.running",
                "core",
                json!({"pluginId": "plug-a"}),
            ));
        }
        store
    }

    #[test]
    fn filters_by_level() {
        let store = fresh_store("filters_by_level");
        let f = LogFilter {
            level: "error".into(),
            ..Default::default()
        };
        let out = search_logs(&store, f);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|e| e.level == "error"));
    }

    #[test]
    fn filters_by_plugin_id() {
        let store = fresh_store("filters_by_plugin_id");
        let f = LogFilter {
            plugin_id: "plug-b".into(),
            ..Default::default()
        };
        let out = search_logs(&store, f);
        assert!(out.iter().all(|e| e.plugin_id == "plug-b"));
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn filters_by_query_substring_case_insensitive() {
        let store = fresh_store("filters_by_query");
        let f = LogFilter {
            query: "HELLO".into(),
            ..Default::default()
        };
        let out = search_logs(&store, f);
        assert_eq!(out.len(), 8);
    }

    #[test]
    fn since_ts_returns_only_newer_ascending() {
        let store = fresh_store("since_ts");
        let f = LogFilter {
            since_ts: Some(1004),
            ..Default::default()
        };
        let out = search_logs(&store, f);
        assert_eq!(out.len(), 3);
        let ts: Vec<u64> = out.iter().map(|e| e.timestamp).collect();
        assert_eq!(ts, vec![1005, 1006, 1007]);
    }

    #[test]
    fn excludes_non_plugin_log_kinds() {
        let store = fresh_store("excludes_non_plugin_log_kinds");
        let f = LogFilter::default();
        let out = search_logs(&store, f);
        assert_eq!(out.len(), 8);
        assert!(out.iter().all(|e| e.kind == "plugin.log"));
    }

    #[test]
    fn default_level_falls_back_to_info() {
        let dir = std::env::temp_dir().join(format!(
            "opencapx-logsearch-default-level-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = std::sync::Arc::new(std::sync::Mutex::new(StoreEnum::Db(
            Storage::open(&dir.join("t.db")).unwrap(),
        )));
        let mut s = store.lock().unwrap();
        s.log_event(&OpencapxEvent::new(
            "plugin.log",
            "core",
            json!({"pluginId": "p", "message": "x", "timestamp": 1u64}),
        ));
        drop(s);
        let out = search_logs(&store, LogFilter::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].level, "info");
    }

    #[test]
    fn limit_caps_results() {
        let store = fresh_store("limit_caps");
        let f = LogFilter {
            limit: 3,
            ..Default::default()
        };
        let out = search_logs(&store, f);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn max_timestamp_handles_empty() {
        assert_eq!(max_timestamp(&[]), 0);
    }
}
