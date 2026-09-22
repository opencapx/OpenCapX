//! Runtime enforcement of scope for path-type permissions (the runtime landing of
//! permissions.md §Scope).
//!
//! Semantics:
//! - scope null / absent / empty string → **unrestricted** (compatible with existing
//!   grant rows, progressive enablement; constraints begin only after the Settings page
//!   writes a scope).
//! - Once a layer has written scope JSON, that layer enforces: denied hit → reject;
//!   allowed hit → pass; neither → reject (closed by default). A JSON parse failure is
//!   treated as reject (fail closed).
//! - Prefixes match on **path component boundaries**: `~/a` does not allow `~/ab`. `~` is
//!   expanded; symlinks are not resolved (v1, lexical normalization).
//! - Two-layer consistency: the agent layer and plugin layer each hold their own scope,
//!   and a resource must fall within both layers' allowed (isomorphic to the two-layer
//!   permission decision).
//! - Domain type (v1.5): `browser.read` (param `url`) constrains host by the scope of
//!   `browser.control`. Entry `example.com` = this domain + subdomains (dot-boundary
//!   suffix); `.example.com` / `*.example.com` = subdomains only; the URL's port and
//!   userinfo do not participate in matching.

use serde_json::Value;

use super::storage::SharedStore;

#[derive(Debug, PartialEq, Eq)]
pub enum ScopeOutcome {
    /// scope not configured: no constraint (legacy behavior).
    Unscoped,
    /// Hit allowed (and did not hit denied).
    Allowed,
    /// Hit denied, or scope is configured but nothing matched, or parsing failed.
    Denied,
}

/// Path parameter name for path-type capabilities (the input extraction point for scope
/// enforcement). Register new path-type capabilities here.
pub fn path_param(capability: &str) -> Option<&'static str> {
    match capability {
        "file.read" | "file.write" => Some("path"),
        "file.search" => Some("root"),
        _ => None,
    }
}

/// URL parameter name for domain-type capabilities. Register new network-type capabilities here.
fn url_param(capability: &str) -> Option<&'static str> {
    match capability {
        "browser.read" => Some("url"),
        _ => None,
    }
}

/// Extract host from an http(s) URL (strips userinfo and port); returns None on a bad
/// shape (leave it to the capability's own URL validation to error; scope does not claim
/// that error).
fn url_host(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None; // only http(s) is constrained; browser.read's URL validation only accepts http(s) anyway
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let host = if host.starts_with('[') {
        format!("[{}]", host.split(']').next().unwrap_or("").trim_start_matches('[')) // [IPv6]:port → canonical bracketed form
    } else {
        host.split(':').next().unwrap_or("").to_string()
    };
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

/// Whether a domain entry covers host: `example.com` = this domain + subdomains;
/// `.example.com` / `*.example.com` = subdomains only; everything else is an exact match.
fn domain_covers(entry: &str, host: &str) -> bool {
    let e = entry.trim().to_ascii_lowercase();
    let h = host.trim().to_ascii_lowercase();
    if let Some(sub) = e.strip_prefix("*.") {
        let base = sub.trim_start_matches('.');
        return h != base && h.ends_with(&format!(".{}", base));
    }
    if let Some(base) = e.strip_prefix('.') {
        return h.ends_with(&format!(".{}", base));
    }
    h == e || h.ends_with(&format!(".{}", e))
}

/// Decide host ownership for a single layer's scope JSON (same principles as decide():
/// denied first, configured means closed by default, fail closed on parse error, null
/// means unrestricted).
pub fn decide_domain(scope_json: Option<&str>, host: &str) -> ScopeOutcome {
    let Some(raw) = scope_json else {
        return ScopeOutcome::Unscoped;
    };
    let raw = raw.trim();
    if raw.is_empty() || raw == "null" {
        return ScopeOutcome::Unscoped;
    }
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return ScopeOutcome::Denied;
    };
    let Some(obj) = v.as_object() else {
        return ScopeOutcome::Denied;
    };
    let empty: Vec<Value> = Vec::new();
    let denied = obj.get("denied").and_then(|x| x.as_array()).unwrap_or(&empty);
    let allowed = obj.get("allowed").and_then(|x| x.as_array()).unwrap_or(&empty);
    for d in denied {
        if let Some(e) = d.as_str() {
            if domain_covers(e, host) {
                return ScopeOutcome::Denied;
            }
        }
    }
    for a in allowed {
        if let Some(e) = a.as_str() {
            if domain_covers(e, host) {
                return ScopeOutcome::Allowed;
            }
        }
    }
    ScopeOutcome::Denied
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Lexical normalization: expand `~`, strip trailing `/`, collapse repeated separators to one; does not touch case or resolve symlinks.
fn normalize(p: &str) -> String {
    let expanded = if p == "~" {
        home_dir()
    } else if let Some(rest) = p.strip_prefix("~/") {
        format!("{}/{}", home_dir(), rest)
    } else {
        p.to_string()
    };
    let mut out = String::with_capacity(expanded.len());
    let mut prev_sep = false;
    for ch in expanded.chars() {
        let is_sep = ch == '/';
        if is_sep && prev_sep {
            continue;
        }
        out.push(ch);
        prev_sep = is_sep;
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Component-boundary prefix: `path == prefix`, or path starts with `prefix/`.
fn starts_at_boundary(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    path == prefix
        || (path.len() > prefix.len()
            && path.starts_with(prefix)
            && path.as_bytes()[prefix.len()] == b'/')
}

/// Decide path ownership for a single layer's scope JSON.
pub fn decide(scope_json: Option<&str>, path: &str) -> ScopeOutcome {
    let Some(raw) = scope_json else {
        return ScopeOutcome::Unscoped;
    };
    let raw = raw.trim();
    if raw.is_empty() || raw == "null" {
        return ScopeOutcome::Unscoped;
    }
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return ScopeOutcome::Denied; // fail closed
    };
    let Some(obj) = v.as_object() else {
        return ScopeOutcome::Denied;
    };
    let empty: Vec<Value> = Vec::new();
    let denied = obj.get("denied").and_then(|x| x.as_array()).unwrap_or(&empty);
    let allowed = obj.get("allowed").and_then(|x| x.as_array()).unwrap_or(&empty);
    let p = normalize(path);
    for d in denied {
        if let Some(s) = d.as_str() {
            if starts_at_boundary(&p, &normalize(s)) {
                return ScopeOutcome::Denied;
            }
        }
    }
    for a in allowed {
        if let Some(s) = a.as_str() {
            if starts_at_boundary(&p, &normalize(s)) {
                return ScopeOutcome::Allowed;
            }
        }
    }
    // scope is configured but nothing matched → closed by default (consistent with permissions.md §Scope).
    ScopeOutcome::Denied
}

fn agent_scope_json(store: &SharedStore, agent_id: &str, permission: &str) -> Option<String> {
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT scope FROM agent_permissions WHERE agent_id = ?1 AND permission = ?2",
                    rusqlite::params![agent_id, permission],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
            })
        })
        .flatten()
}

fn plugin_scope_json(store: &SharedStore, plugin_id: &str, permission: &str) -> Option<String> {
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT scope FROM plugin_permissions WHERE plugin_id = ?1 AND permission = ?2",
                    rusqlite::params![plugin_id, permission],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
            })
        })
        .flatten()
}

/// Two-layer path gate. Applies only to path-type capabilities whose input carries a path;
/// an empty agent string skips the agent layer (isomorphic to "no plugin principal means
/// the plugin layer is skipped"). Returns Err(layer) when that layer's scope explicitly rejects.
pub fn enforce(
    store: &SharedStore,
    capability: &str,
    input: &Value,
    agent: &str,
    plugin: Option<&str>,
) -> Result<(), &'static str> {
    // Domain type (v1.5: browser.read): same structure, same principles, with the match target switched to host
    if let Some(param) = url_param(capability) {
        let Some(url) = input.get(param).and_then(|p| p.as_str()) else {
            return Ok(());
        };
        let Some(host) = url_host(url) else {
            return Ok(()); // Bad URL shape: leave it to the capability's own validation to error
        };
        let permission = match super::permission::capability_permission(capability) {
            Some(p) => p.to_string(),
            None => return Ok(()), // No permission mapping: scope has nothing to attach to
        };
        if !agent.is_empty() {
            if let Some(scope) = agent_scope_json(store, agent, &permission) {
                if decide_domain(Some(&scope), &host) == ScopeOutcome::Denied {
                    return Err("agent");
                }
            }
        }
        if let Some(pid) = plugin {
            if let Some(scope) = plugin_scope_json(store, pid, &permission) {
                if decide_domain(Some(&scope), &host) == ScopeOutcome::Denied {
                    return Err("plugin");
                }
            }
        }
        return Ok(());
    }

    let Some(param) = path_param(capability) else {
        return Ok(());
    };
    let Some(path) = input.get(param).and_then(|p| p.as_str()) else {
        return Ok(()); // No path input: leave it to the capability's own input validation to error
    };
    let permission = match super::permission::capability_permission(capability) {
        Some(p) => p.to_string(),
        None => return Ok(()), // No permission mapping: scope has nothing to attach to
    };
    if !agent.is_empty() {
        if let Some(scope) = agent_scope_json(store, agent, &permission) {
            if decide(Some(&scope), path) == ScopeOutcome::Denied {
                return Err("agent");
            }
        }
    }
    if let Some(pid) = plugin {
        if let Some(scope) = plugin_scope_json(store, pid, &permission) {
            if decide(Some(&scope), path) == ScopeOutcome::Denied {
                return Err("plugin");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::storage::{Storage, StoreEnum};
    use std::sync::{Arc, Mutex};

    fn tmp_store(tag: &str) -> SharedStore {
        let dir = std::env::temp_dir().join(format!("opencapx-scope-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(Mutex::new(StoreEnum::Db(Storage::open(&dir.join("t.db")).unwrap())))
    }

    #[test]
    fn null_or_missing_scope_means_unscoped() {
        assert_eq!(decide(None, "/etc/passwd"), ScopeOutcome::Unscoped);
        assert_eq!(decide(Some(""), "/etc/passwd"), ScopeOutcome::Unscoped);
        assert_eq!(decide(Some("null"), "/etc/passwd"), ScopeOutcome::Unscoped);
        assert_eq!(decide(Some("   "), "/etc/passwd"), ScopeOutcome::Unscoped);
    }

    #[test]
    fn malformed_json_fails_closed() {
        assert_eq!(decide(Some("{not json"), "/x"), ScopeOutcome::Denied);
        assert_eq!(decide(Some("[1,2]"), "/x"), ScopeOutcome::Denied);
    }

    #[test]
    fn prefix_respects_component_boundary() {
        let scope = r#"{"allowed":["~/a"],"denied":[]}"#;
        // File inside the directory: pass
        assert_eq!(decide(Some(scope), "~/a/f.txt"), ScopeOutcome::Allowed);
        // Exactly the directory itself: pass (equal after stripping the trailing slash)
        assert_eq!(decide(Some(scope), "~/a/"), ScopeOutcome::Allowed);
        // Sibling directory ~/ab: same byte prefix but different component boundary → no hit → closed by default
        assert_eq!(decide(Some(scope), "~/ab/f.txt"), ScopeOutcome::Denied);
    }

    #[test]
    fn denied_beats_allowed() {
        let scope = r#"{"allowed":["~/a"],"denied":["~/a/secret"]}"#;
        assert_eq!(decide(Some(scope), "~/a/ok.txt"), ScopeOutcome::Allowed);
        assert_eq!(decide(Some(scope), "~/a/secret/x"), ScopeOutcome::Denied);
        assert_eq!(decide(Some(scope), "~/a/secret"), ScopeOutcome::Denied);
    }

    #[test]
    fn configured_but_unmatched_is_closed() {
        let scope = r#"{"allowed":["~/Projects/x/"],"denied":[]}"#;
        assert_eq!(decide(Some(scope), "~/Library/x"), ScopeOutcome::Denied);
    }

    #[test]
    fn home_expansion_and_separator_squash() {
        let home = home_dir();
        let scope = r#"{"allowed":["~/Projects//open-capx/"],"denied":[]}"#;
        assert_eq!(
            decide(Some(scope), &format!("{}/Projects/open-capx/a/b", home)),
            ScopeOutcome::Allowed
        );
    }

    #[test]
    fn file_search_uses_root_param() {
        let store = tmp_store("root-param");
        {
            let mut g = store.lock().unwrap();
            let n = g.with_conn(|c| {
                c.execute(
                    "INSERT INTO agent_permissions (agent_id, permission, scope, decision, updated_at) VALUES (?1,?2,?3,?4,0)",
                    rusqlite::params!["ag_x", "file.read", r#"{"allowed":["~/safe"],"denied":[]}"#, "granted"],
                )
                .unwrap_or(0)
            });
            assert_eq!(n, Some(1));
        }
        let input = serde_json::json!({ "root": "~/safe/sub" });
        assert_eq!(enforce(&store, "file.search", &input, "ag_x", None), Ok(()));
        let input_bad = serde_json::json!({ "root": "~/etc" });
        assert_eq!(enforce(&store, "file.search", &input_bad, "ag_x", None), Err("agent"));
    }

    #[test]
    fn both_layers_enforced_independently() {
        let store = tmp_store("two-layer");
        {
            let mut g = store.lock().unwrap();
            let n1 = g.with_conn(|c| {
                c.execute(
                    "INSERT INTO agent_permissions (agent_id, permission, scope, decision, updated_at) VALUES ('ag_x','file.read',?1,'granted',0)",
                    rusqlite::params![r#"{"allowed":["~/pub"]}"#],
                )
                .unwrap_or(0)
            });
            let n2 = g.with_conn(|c| {
                c.execute(
                    "INSERT INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at) VALUES ('p1','file.read',?1,'granted',0)",
                    rusqlite::params![r#"{"allowed":["~/pub/inner"]}"#],
                )
                .unwrap_or(0)
            });
            assert_eq!((n1, n2), (Some(1), Some(1)));
        }
        // Falls within both layers' allowed → pass
        let input = serde_json::json!({ "path": "~/pub/inner/f.txt" });
        assert_eq!(enforce(&store, "file.read", &input, "ag_x", Some("p1")), Ok(()));
        // Inside the agent layer, outside the plugin layer → the plugin layer rejects
        let input = serde_json::json!({ "path": "~/pub/outer.txt" });
        assert_eq!(enforce(&store, "file.read", &input, "ag_x", Some("p1")), Err("plugin"));
        // Inside-agent/outside-plugin already verified; outside the agent layer (scope
        // closed by default) → the agent layer rejects, regardless of whether the plugin is
        // configured (the two layers are independent).
        let input = serde_json::json!({ "path": "~/anywhere" });
        assert_eq!(enforce(&store, "file.read", &input, "ag_x", Some("p_other")), Err("agent"));
        // Truly both unconfigured (neither agent nor plugin has a scope row) → unrestricted (legacy)
        let input = serde_json::json!({ "path": "~/anywhere" });
        assert_eq!(enforce(&store, "file.read", &input, "ag_y", Some("p_other")), Ok(()));
    }


    #[test]
    fn domain_matching_semantics() {
        let scope = r#"{"allowed":["example.com","*.internal.io"],"denied":["evil.example.com"]}"#;
        // This domain + subdomains (dot boundary, excluding sibling domains)
        assert_eq!(decide_domain(Some(scope), "example.com"), ScopeOutcome::Allowed);
        assert_eq!(decide_domain(Some(scope), "api.example.com"), ScopeOutcome::Allowed);
        assert_eq!(decide_domain(Some(scope), "notexample.com"), ScopeOutcome::Denied);
        // A malicious subdomain is not bypassed by a notexample-style prefix: evil.example.com hits denied
        assert_eq!(decide_domain(Some(scope), "evil.example.com"), ScopeOutcome::Denied);
        // *.internal.io = subdomains only; the bare domain does not hit → closed by default
        assert_eq!(decide_domain(Some(scope), "a.internal.io"), ScopeOutcome::Allowed);
        assert_eq!(decide_domain(Some(scope), "internal.io"), ScopeOutcome::Denied);
        // Unconfigured domain → closed; null → unrestricted; bad JSON → reject
        assert_eq!(decide_domain(Some(scope), "other.org"), ScopeOutcome::Denied);
        assert_eq!(decide_domain(None, "other.org"), ScopeOutcome::Unscoped);
        assert_eq!(decide_domain(Some("{"), "other.org"), ScopeOutcome::Denied);
    }

    #[test]
    fn domain_host_extraction_strips_port_userinfo_and_case() {
        assert_eq!(url_host("https://Example.com:8443/path"), Some("example.com".to_string()));
        assert_eq!(url_host("http://user:pw@api.example.com/x"), Some("api.example.com".to_string()));
        assert_eq!(url_host("http://[::1]:9000/x"), Some("[::1]".to_string()));
        assert_eq!(url_host("not-a-url"), None);
        assert_eq!(url_host("ftp://x/"), None);
    }

    #[test]
    fn browser_read_enforces_domain_scope_on_both_layers() {
        let store = tmp_store("domain");
        {
            let mut g = store.lock().unwrap();
            let n1 = g.with_conn(|c| c.execute(
                "INSERT INTO agent_permissions (agent_id, permission, scope, decision, updated_at) VALUES ('ag_x','browser.control',?1,'granted',0)",
                rusqlite::params![r#"{"allowed":["docs.rs"]}"#],
            ).unwrap_or(0));
            let n2 = g.with_conn(|c| c.execute(
                "INSERT INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at) VALUES ('p1','browser.control',?1,'granted',0)",
                rusqlite::params![r#"{"allowed":["docs.rs","crates.io"]}"#],
            ).unwrap_or(0));
            assert_eq!((n1, n2), (Some(1), Some(1)));
        }
        let ok = serde_json::json!({ "url": "https://docs.rs/opencapx/latest" });
        assert_eq!(enforce(&store, "browser.read", &ok, "ag_x", Some("p1")), Ok(()));
        let outside_agent = serde_json::json!({ "url": "https://crates.io/crates/serde" });
        assert_eq!(enforce(&store, "browser.read", &outside_agent, "ag_x", Some("p1")), Err("agent"));
        let inside_agent_outside_plugin = serde_json::json!({ "url": "https://sub.docs.rs/x" });
        assert_eq!(enforce(&store, "browser.read", &inside_agent_outside_plugin, "ag_x", Some("p_other")), Ok(()));
        // A layer with no scope row → unrestricted (legacy)
        let any = serde_json::json!({ "url": "https://anywhere.example" });
        assert_eq!(enforce(&store, "browser.read", &any, "ag_y", None), Ok(()));
        // URL missing/malformed → don't claim the error, leave it to capability validation
        assert_eq!(enforce(&store, "browser.read", &serde_json::json!({}), "ag_x", None), Ok(()));
    }

    #[test]
    fn non_path_capability_is_noop() {
        let store = tmp_store("noop");
        let input = serde_json::json!({ "text": "hi" });
        assert_eq!(enforce(&store, "opencapx.say", &input, "ag_x", Some("p1")), Ok(()));
        // Path-type capability but input lacks a path → leave it to the capability's own validation
        let input = serde_json::json!({});
        assert_eq!(enforce(&store, "file.read", &input, "ag_x", Some("p1")), Ok(()));
    }
}
