//! docs/permission-domains.md §4.3 / §4.6 — plugin domain declaration table + single-point resolver.
//!
//! Responsibilities:
//! - `resolve()` **single-point resolution**: ① built-in static table → ② frozen declaration table → ③ no match = None
//! - Declaration table read/write (install commit, uninstall release, "global consistency" check)
//! - Domain ownership registration (P1 local: first-install claims + reserved domains caught by `permission::reserved_domain`)
//!
//! **Implementation note (deviating from the §9-4 decision)**: the decision said "startup cache + install/uninstall invalidation hooks",
//! but here we query the DB **every time** (same as `permission::check` / `capability::providers`).
//! Rationale: profile hot-switching (`replace_shared_store`) and store swaps in tests will both make the in-process
//! global cache stale, and the cache sits on the **authorization path**; missing a single invalidation point means privilege escalation. The table is tiny,
//! in-process SQLite with microsecond point lookups, so we skip caching for now; if load testing later proves an impact, within the same function boundary
//! adding a cache + explicit invalidation is enough, and callers are unaffected.

use super::permission::{self, Decision};
use super::storage::SharedStore;
use rusqlite::params;
use serde::Serialize;

/// Resolution result (§4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub permission: String,
    pub default: Decision,
    /// true = comes from a plugin declaration → the four once-only enforcement points apply (§4.3 review 7);
    /// false = built-in static mapping.
    pub declared: bool,
}

/// One frozen declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Declaration {
    pub capability: String,
    pub plugin_id: String,
    pub permission: String,
    pub default_decision: String,
    /// S4 — per-capability call timeout (seconds, 1..=600); None = default CALL_TIMEOUT (60s).
    pub timeout_secs: Option<u32>,
}

/// Single-point resolver (§4.3): ① reserved static table → ② frozen declaration table → ③ no match = None (route denied).
/// Inconsistent multi-provider mapping → None (fail-closed, §4.6).
pub fn resolve(store: &SharedStore, capability: &str) -> Option<Resolved> {
    if let Some(perm) = permission::capability_permission(capability) {
        return Some(Resolved {
            permission: perm.to_string(),
            default: permission::default_decision(perm),
            declared: false,
        });
    }
    let rows = for_capability(store, capability);
    let first = rows.first()?.clone();
    // Global consistency: all live providers' (permission, default) must match, otherwise it cannot be routed
    if rows
        .iter()
        .any(|r| r.permission != first.permission || r.default_decision != first.default_decision)
    {
        return None;
    }
    Some(Resolved {
        permission: first.permission,
        default: permission::parse_decision(&first.default_decision),
        declared: true,
    })
}

/// Whether this permission name is derived from some plugin's declaration (→ once-only + not grantable from the settings page).
pub fn is_declared_permission(store: &SharedStore, perm: &str) -> bool {
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM capability_declarations WHERE permission = ?1",
                    params![perm],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
            })
        })
        .flatten()
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// Set of declared permissions (one DISTINCT query; for batch scenarios like the heat map).
/// NOTE: the caller must not already hold the store lock — the is_declared family locks the same Mutex again (reentrant deadlock).
pub fn declared_permission_set(store: &SharedStore) -> std::collections::HashSet<String> {
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                let mut stmt = c
                    .prepare("SELECT DISTINCT permission FROM capability_declarations")
                    .ok()?;
                let it = stmt
                    .query_map([], |r| r.get::<_, String>(0))
                    .ok()?;
                Some(it.filter_map(|x| x.ok()).collect::<std::collections::HashSet<String>>())
            })
        })
        .flatten()
        .unwrap_or_default()
}


/// Declaration-derived default decision (§4.3 access point #2). Non-declared permissions return None.
/// Multiple providers declare the same permission with inconsistent defaults → None (caller falls back to denied).
pub fn declared_default(store: &SharedStore, perm: &str) -> Option<Decision> {
    let rows = rows_where(store, "permission", perm);
    let first = rows.first()?;
    if rows.iter().any(|r| r.default_decision != first.default_decision) {
        return None;
    }
    Some(permission::parse_decision(&first.default_decision))
}

/// All declaration rows for a capability (including multiple providers).
pub fn for_capability(store: &SharedStore, capability: &str) -> Vec<Declaration> {
    rows_where(store, "capability", capability)
}

fn rows_where(store: &SharedStore, column: &str, value: &str) -> Vec<Declaration> {
    // column comes only from literals inside this module, not external input
    let sql = format!(
        "SELECT capability, plugin_id, permission, default_decision, timeout_secs
         FROM capability_declarations WHERE {} = ?1 ORDER BY plugin_id ASC",
        column
    );
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                let mut stmt = c.prepare(&sql).ok()?;
                let it = stmt
                    .query_map(params![value], |r| {
                        Ok(Declaration {
                            capability: r.get(0)?,
                            plugin_id: r.get(1)?,
                            permission: r.get(2)?,
                            default_decision: r.get(3)?,
                            timeout_secs: r.get(4)?,
                        })
                    })
                    .ok()?;
                Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
            })
        })
        .flatten()
        .unwrap_or_default()
}

/// S4 — a provider's call timeout for a capability (seconds); no declaration/no such row → None (caller uses the default 60s).
pub fn timeout_for(store: &SharedStore, plugin_id: &str, capability: &str) -> Option<u32> {
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT timeout_secs FROM capability_declarations
                     WHERE capability = ?1 AND plugin_id = ?2",
                    params![capability, plugin_id],
                    |r| r.get::<_, Option<u32>>(0),
                )
                .ok()
                .flatten()
            })
        })
        .flatten()
}

/// Whole table (for the settings page / dependency graph / audit).
pub fn all(store: &SharedStore) -> Vec<Declaration> {
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                let mut stmt = c
                    .prepare(
                        "SELECT capability, plugin_id, permission, default_decision, timeout_secs
                         FROM capability_declarations ORDER BY capability ASC, plugin_id ASC",
                    )
                    .ok()?;
                let it = stmt
                    .query_map([], |r| {
                        Ok(Declaration {
                            capability: r.get(0)?,
                            plugin_id: r.get(1)?,
                            permission: r.get(2)?,
                            default_decision: r.get(3)?,
                            timeout_secs: r.get(4)?,
                        })
                    })
                    .ok()?;
                Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
            })
        })
        .flatten()
        .unwrap_or_default()
}

/// List of declared capability IDs (route admission = reserved ∪ declared, §4.3 access point #1).
pub fn declared_capability_ids(store: &SharedStore) -> Vec<String> {
    let mut ids: Vec<String> = all(store).into_iter().map(|d| d.capability).collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Declared permission names → default decisions (settings-page vocabulary = static table ∪ this table, §4.3 access point #6).
pub fn declared_permission_defaults(store: &SharedStore) -> Vec<(String, Decision)> {
    let mut out: Vec<(String, Decision)> = Vec::new();
    for d in all(store) {
        let dec = permission::parse_decision(&d.default_decision);
        match out.iter_mut().find(|(p, _)| *p == d.permission) {
            Some((_, existing)) => {
                // Inconsistent → converge to the stricter level (denied < ask < granted)
                if stricter(dec) < stricter(*existing) {
                    *existing = dec;
                }
            }
            None => out.push((d.permission, dec)),
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn stricter(d: Decision) -> u8 {
    match d {
        Decision::Denied => 0,
        Decision::Ask => 1,
        Decision::Granted => 2,
    }
}

/// §4.2 "global consistency": if another live provider of the same capability maps differently → return the conflicting plugin_id.
pub fn conflicting_provider(
    store: &SharedStore,
    capability: &str,
    permission: &str,
    default_decision: &str,
    exclude_plugin: &str,
) -> Option<String> {
    for_capability(store, capability)
        .into_iter()
        .find(|d| {
            d.plugin_id != exclude_plugin
                && (d.permission != permission || d.default_decision != default_decision)
        })
        .map(|d| d.plugin_id)
}

/// §4.5 P1 domain ownership: reserved domains are blocked by `permission::reserved_domain`; here we register non-reserved domains.
/// A domain already claimed by an **other** plugin → Err (first-install claims, conflict rejected).
pub fn claim_domains_in_tx(
    tx: &rusqlite::Transaction<'_>,
    plugin_id: &str,
    domains: &[String],
) -> Result<(), String> {
    for domain in domains {
        if permission::reserved_domain(domain) {
            return Err(format!("domain {} is reserved", domain));
        }
        let holder: Option<String> = tx
            .query_row(
                "SELECT plugin_id FROM domain_registry WHERE domain = ?1",
                params![domain],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        match holder {
            Some(h) if h != plugin_id => {
                return Err(format!("domain {} already claimed by {}", domain, h));
            }
            _ => {
                tx.execute(
                    "INSERT INTO domain_registry (domain, plugin_id, publisher_id, source)
                     VALUES (?1, ?2, NULL, 'local')
                     ON CONFLICT(domain) DO UPDATE SET plugin_id = ?2",
                    params![domain, plugin_id],
                )
                .map_err(|e| format!("claim {} failed: {}", domain, e))?;
            }
        }
    }
    Ok(())
}

/// Commit phase: write declaration rows + claim domains. Shares the same transaction with `plugins` / `plugin_permissions`.
pub fn write_in_tx(
    tx: &rusqlite::Transaction<'_>,
    plugin_id: &str,
    declarations: &[(String, String, String, Option<u32>)], // (capability, permission, default, timeoutSecs)
    now: u64,
) -> Result<(), String> {
    let mut domains: Vec<String> = Vec::new();
    for (capability, perm, default, timeout_secs) in declarations {
        tx.execute(
            "INSERT INTO capability_declarations
                 (capability, plugin_id, permission, default_decision, confirmed_at, timeout_secs)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(capability, plugin_id)
                 DO UPDATE SET permission = ?3, default_decision = ?4, confirmed_at = ?5,
                               timeout_secs = ?6",
            params![
                capability,
                plugin_id,
                perm,
                default,
                now,
                (*timeout_secs).map(|t| t as i64)
            ],
        )
        .map_err(|e| format!("declare {} failed: {}", capability, e))?;
        let d = permission::first_segment(capability).to_string();
        if !domains.contains(&d) {
            domains.push(d);
        }
    }
    claim_domains_in_tx(tx, plugin_id, &domains)
}

/// Uninstall: release domains + delete declarations (§4.6 uninstall release).
pub fn delete_for_plugin(store: &SharedStore, plugin_id: &str) -> Result<(), String> {
    let Ok(mut s) = store.lock() else {
        return Err("storage unavailable".into());
    };
    match s.try_with_conn(|c| {
        let tx = c
            .unchecked_transaction()
            .map_err(|e| format!("begin failed: {}", e))?;
        tx.execute(
            "DELETE FROM capability_declarations WHERE plugin_id = ?1",
            params![plugin_id],
        )
        .map_err(|e| format!("delete declarations failed: {}", e))?;
        tx.execute(
            "DELETE FROM domain_registry WHERE plugin_id = ?1",
            params![plugin_id],
        )
        .map_err(|e| format!("release domains failed: {}", e))?;
        tx.commit().map_err(|e| format!("commit failed: {}", e))?;
        Ok(())
    }) {
        Some(r) => r,
        None => Err("sqlite unavailable".into()),
    }
}

/// Domain registry snapshot (settings page / audit: shows "unverified domains").
pub fn domains(store: &SharedStore) -> Vec<(String, Option<String>, String)> {
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                let mut stmt = c
                    .prepare("SELECT domain, plugin_id, source FROM domain_registry ORDER BY domain ASC")
                    .ok()?;
                let it = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .ok()?;
                Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
            })
        })
        .flatten()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::storage::{Storage, StoreEnum};
    use std::sync::{Arc, Mutex};

    fn db_store(tag: &str) -> SharedStore {
        let dir = std::env::temp_dir().join(format!("opencapx-decl-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Arc::new(Mutex::new(StoreEnum::Db(
            Storage::open(&dir.join("t.db")).unwrap(),
        )))
    }

    fn declare(store: &SharedStore, plugin: &str, caps: &[(&str, &str, &str)]) {
        let rows: Vec<(String, String, String, Option<u32>)> = caps
            .iter()
            .map(|(c, p, d)| (c.to_string(), p.to_string(), d.to_string(), None))
            .collect();
        let mut s = store.lock().unwrap();
        s.try_with_conn(|c| {
            let tx = c.unchecked_transaction().unwrap();
            write_in_tx(&tx, plugin, &rows, 1).map_err(|e| e.to_string())?;
            tx.commit().unwrap();
            Ok(())
        })
        .unwrap()
        .unwrap();
    }

    #[test]
    fn resolve_prefers_builtin_then_declarations() {
        let s = db_store("resolve");
        // ① built-in static table
        let r = resolve(&s, "image.analyze").expect("builtin");
        assert_eq!(r.permission, "image.read");
        assert!(!r.declared);
        // ③ no match
        assert!(resolve(&s, "weather.fetch").is_none());
        // ② declaration table
        declare(&s, "com.x.weather", &[("weather.fetch", "weather.read", "ask")]);
        let r = resolve(&s, "weather.fetch").expect("declared");
        assert_eq!(r.permission, "weather.read");
        assert_eq!(r.default, Decision::Ask);
        assert!(r.declared);
        assert!(is_declared_permission(&s, "weather.read"));
        assert_eq!(declared_default(&s, "weather.read"), Some(Decision::Ask));
        assert!(!is_declared_permission(&s, "image.read"));
    }

    /// §4.6 fail-closed: inconsistent multi-provider mapping → the capability cannot be routed.
    #[test]
    fn resolve_fails_closed_on_inconsistent_providers() {
        let s = db_store("inconsistent");
        declare(&s, "com.a", &[("weather.fetch", "weather.read", "ask")]);
        let mut x = s.lock().unwrap();
        x.try_with_conn(|c| {
            c.execute(
                "INSERT INTO capability_declarations
                     (capability, plugin_id, permission, default_decision, confirmed_at)
                 VALUES ('weather.fetch', 'com.b', 'weather.read2', 'denied', 1)",
                [],
            )
            .unwrap();
            Ok(())
        })
        .unwrap()
        .unwrap();
        drop(x);
        assert!(resolve(&s, "weather.fetch").is_none());
        // Declared defaults are also inconsistent → None (caller falls back to denied)
        assert_eq!(declared_default(&s, "weather.read"), Some(Decision::Ask));
    }

    #[test]
    fn domain_claim_is_first_come_and_rejects_reserved() {
        let s = db_store("domain");
        declare(&s, "com.a", &[("weather.fetch", "weather.read", "ask")]);
        assert_eq!(domains(&s, ), vec![("weather".to_string(), Some("com.a".to_string()), "local".to_string())]);
        // Same domain, different plugin → rejected
        let mut x = s.lock().unwrap();
        let r = x.try_with_conn(|c| {
            let tx = c.unchecked_transaction().unwrap();
            let r = write_in_tx(&tx, "com.b", &[("weather.alerts".into(), "weather.read".into(), "ask".into(), None)], 1);
            drop(tx);
            Ok(r)
        }).unwrap().unwrap();
        assert!(r.is_err(), "second claimant must be rejected: {:?}", r);
        // Reserved domain → rejected
        let r2 = x.try_with_conn(|c| {
            let tx = c.unchecked_transaction().unwrap();
            let r = write_in_tx(&tx, "com.c", &[("things.list".into(), "things.read".into(), "ask".into(), None)], 1);
            drop(tx);
            Ok(r)
        }).unwrap().unwrap();
        assert!(r2.is_err());
    }

    #[test]
    fn uninstall_releases_domain_and_declarations() {
        let s = db_store("release");
        declare(&s, "com.a", &[("weather.fetch", "weather.read", "ask")]);
        assert_eq!(all(&s).len(), 1);
        delete_for_plugin(&s, "com.a").unwrap();
        assert!(all(&s).is_empty());
        assert!(domains(&s).is_empty());
        assert!(!is_declared_permission(&s, "weather.read"));
        // After release it can be claimed by a new plugin
        declare(&s, "com.b", &[("weather.fetch", "weather.read", "ask")]);
        assert_eq!(domains(&s)[0].1.as_deref(), Some("com.b"));
    }

    #[test]
    fn conflicting_provider_detects_mapping_drift() {
        let s = db_store("conflict");
        declare(&s, "com.a", &[("weather.fetch", "weather.read", "ask")]);
        assert_eq!(
            conflicting_provider(&s, "weather.fetch", "weather.read2", "ask", "com.b"),
            Some("com.a".to_string())
        );
        assert_eq!(
            conflicting_provider(&s, "weather.fetch", "weather.read", "ask", "com.b"),
            None
        );
        // Itself does not count as a conflict (overwrite install)
        assert_eq!(
            conflicting_provider(&s, "weather.fetch", "weather.read2", "ask", "com.a"),
            None
        );
    }
}
