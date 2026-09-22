//! Agent identity (AgentIdentity): the security principal for /rpc and /event callers.
//! See docs/permissions.md "Agent identity". Explicitly separate from
//! `core::agent::Session` (the observed object recording "what the Agent is doing"):
//! this module answers "who is calling".
//!
//! - Registration is TOFU: the CLI (`opencapx mcp` / `opencapx hook`) does `POST
//!   /agents/register` on first start; the Core creates an identity per kind and
//!   issues a token; the CLI writes `~/.opencapx/agent-tokens/<kind>.token` (0600)
//! - Tokens look like `ocx1_<64hex>`; the Core stores only the SHA-256, and the
//!   plaintext exists only in the registration response and the token file
//! - v1 is one identity per kind (a UNIQUE index backstops it); re-registration rotates the token
//! - revoked status: automatic re-registration is rejected (40102), and recovery only
//!   goes through the Settings page reauthorize
//!
//! Honest boundary (threat model in docs): a token cannot stop a malicious process
//! running with the same user's privileges that can read the token file; it gives
//! permissions and auditing a subject.

use super::permission::{self, Decision};
use super::storage::SharedStore;
use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};

pub const TOKEN_PREFIX: &str = "ocx1_";

/// CLI-side token file contents. display_name is not persisted (the Core already stored it at registration).
#[derive(Debug, Clone, PartialEq)]
pub struct Credentials {
    pub agent_id: String,
    pub token: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentDto {
    pub agent_id: String,
    pub kind: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub status: String,
    #[serde(rename = "firstSeen")]
    pub first_seen: u64,
    #[serde(rename = "lastSeen")]
    pub last_seen: u64,
    #[serde(rename = "registeredVia")]
    pub registered_via: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyResult {
    Ok,
    UnknownAgent,
    BadToken,
    Revoked,
}

fn now_secs() -> u64 {
    super::agent::now_secs()
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Normalize kind: lowercase, keep only [a-z0-9-], empty/all-invalid → "custom".
pub fn sanitize_kind(raw: &str) -> String {
    let k: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '-' })
        .collect();
    let k = k.trim_matches('-').to_string();
    if k.is_empty() {
        "custom".into()
    } else {
        k.chars().take(32).collect()
    }
}

pub fn display_name_for(kind: &str) -> String {
    let names: &[(&str, &str)] = &[
        ("claude", "Claude"),
        ("codex", "Codex"),
        ("gemini", "Gemini CLI"),
        ("cursor", "Cursor"),
        ("opencode", "opencode"),
        ("copilot", "GitHub Copilot"),
        ("windsurf", "Windsurf"),
        ("antigravity", "Antigravity"),
        ("kiro", "Kiro"),
        ("droid", "Factory Droid"),
        ("pi", "Pi"),
        ("omp", "Oh My Pi"),
        ("grok", "Grok Build"),
        ("cli", "OpenCapX CLI"),
        ("custom", "Custom Agent"),
    ];
    for (k, n) in names {
        if *k == kind {
            return n.to_string();
        }
    }
    // Unknown kind: capitalize the first letter
    let mut c = kind.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => "Custom Agent".into(),
    }
}

fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    format!("{:x}", h.finalize())
}

/// Constant-length comparison with no early exit (best-effort under the local threat model; avoids pulling in the subtle crate).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn rand_hex(n_bytes: usize) -> String {
    let mut buf = vec![0u8; n_bytes];
    if getrandom::getrandom(&mut buf).is_err() {
        // Entropy source failed: fall back to time entropy (not for production; getrandom does not fail on the three major platforms)
        let t = nanos();
        return format!("{:032x}{:016x}", t, t ^ std::process::id() as u128);
    }
    buf.iter().map(|b| format!("{:02x}", b)).collect()
}

fn gen_agent_id(kind: &str) -> String {
    format!("ag_{}_{}", kind, &rand_hex(2))
}

fn gen_token() -> String {
    format!("{}{}", TOKEN_PREFIX, rand_hex(32))
}

fn publish(kind: &str, payload: serde_json::Value) {
    super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(kind, "core", payload));
}

/// auth.rejected audit (called by the http gateway when it rejects a request). reason: anonymous|bad-token|revoked|unknown-agent.
pub fn audit_rejected(agent_id: Option<&str>, reason: &str) {
    let mut payload = json!({ "reason": reason });
    if let (Some(o), Some(a)) = (payload.as_object_mut(), agent_id) {
        o.insert("agentId".into(), json!(a));
    }
    publish("auth.rejected", payload);
}

/// TOFU registration. First time a kind is seen: create identity + issue token +
/// `agent.registered` event; already active: rotate the token (lost token file /
/// reinstall cases); already revoked: reject (Err).
pub fn register(store: &SharedStore, kind_raw: &str, via: &str) -> Result<(String, String), &'static str> {
    let kind = sanitize_kind(kind_raw);
    let display_name = display_name_for(&kind);
    let now = now_secs() as i64;
    let Ok(mut s) = store.lock() else { return Err("store unavailable") };
    let existing: Option<(String, String)> = s
        .with_conn_ref(|c| {
            c.query_row(
                "SELECT agent_id, status FROM agents WHERE kind = ?1",
                params![kind],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok()
        })
        .flatten();
    let (agent_id, token) = match existing {
        Some((_, ref st)) if st == "revoked" => return Err("revoked"),
        Some((id, _)) => {
            let token = gen_token();
            let n = s
                .with_conn(|c| {
                    c.execute(
                        "UPDATE agents SET token_hash = ?2, display_name = ?3, last_seen = ?4 WHERE agent_id = ?1",
                        params![id, hash_token(&token), display_name, now],
                    )
                    .unwrap_or(0)
                })
                .unwrap_or(0);
            if n == 0 {
                return Err("store unavailable");
            }
            (id, token)
        }
        None => {
            let agent_id = gen_agent_id(&kind);
            let token = gen_token();
            let n = s
                .with_conn(|c| {
                    c.execute(
                        "INSERT INTO agents (agent_id, kind, display_name, token_hash, status, first_seen, last_seen, registered_via)
                         VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?6)",
                        params![agent_id, kind, display_name, hash_token(&token), now, via],
                    )
                    .unwrap_or(0)
                })
                .unwrap_or(0);
            if n == 0 {
                return Err("store unavailable");
            }
            publish(
                "agent.registered",
                json!({ "agentId": agent_id, "kind": kind, "via": via }),
            );
            (agent_id, token)
        }
    };
    Ok((agent_id, token))
}

/// Three gates: exists → hash matches → status=active. On success also touches last_seen.
pub fn verify(store: &SharedStore, agent_id: &str, token: &str) -> VerifyResult {
    let Ok(mut s) = store.lock() else { return VerifyResult::UnknownAgent };
    let row: Option<(String, String)> = s
        .with_conn_ref(|c| {
            c.query_row(
                "SELECT token_hash, status FROM agents WHERE agent_id = ?1",
                params![agent_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok()
        })
        .flatten();
    let Some((token_hash, status)) = row else { return VerifyResult::UnknownAgent };
    if status == "revoked" {
        return VerifyResult::Revoked;
    }
    if !ct_eq(token_hash.as_bytes(), hash_token(token).as_bytes()) {
        return VerifyResult::BadToken;
    }
    let _ = s.with_conn(|c| {
        c.execute(
            "UPDATE agents SET last_seen = ?2 WHERE agent_id = ?1",
            params![agent_id, now_secs() as i64],
        )
        .unwrap_or(0)
    });
    VerifyResult::Ok
}

/// Settings page "Revoke". Emits the agent.revoked audit.
pub fn revoke(store: &SharedStore, agent_id: &str) -> bool {
    let Ok(mut s) = store.lock() else { return false };
    let n = s
        .with_conn(|c| {
            c.execute(
                "UPDATE agents SET status = 'revoked' WHERE agent_id = ?1 AND status = 'active'",
                params![agent_id],
            )
            .unwrap_or(0)
        })
        .unwrap_or(0);
    if n > 0 {
        publish("agent.revoked", json!({ "agentId": agent_id }));
        true
    } else {
        false
    }
}

/// Settings page "Reauthorize": issue a new token and return to active. Returns the token (shown once in the Settings page, not stored in frontend state).
pub fn reauthorize(store: &SharedStore, agent_id: &str) -> Option<String> {
    let token = gen_token();
    let Ok(mut s) = store.lock() else { return None };
    let n = s
        .with_conn(|c| {
            c.execute(
                "UPDATE agents SET status = 'active', token_hash = ?2, last_seen = ?3 WHERE agent_id = ?1",
                params![agent_id, hash_token(&token), now_secs() as i64],
            )
            .unwrap_or(0)
        })
        .unwrap_or(0);
    if n > 0 {
        Some(token)
    } else {
        None
    }
}

pub fn list(store: &SharedStore) -> Vec<AgentDto> {
    let Ok(s) = store.lock() else { return Vec::new() };
    s.with_conn_ref(|c| {
        let Ok(mut stmt) = c.prepare(
            "SELECT agent_id, kind, display_name, status, first_seen, last_seen, registered_via
             FROM agents ORDER BY first_seen ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok(AgentDto {
                agent_id: r.get(0)?,
                kind: r.get(1)?,
                display_name: r.get(2)?,
                status: r.get(3)?,
                first_seen: r.get::<_, i64>(4).unwrap_or(0).max(0) as u64,
                last_seen: r.get::<_, i64>(5).unwrap_or(0).max(0) as u64,
                registered_via: r.get(6)?,
            })
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    })
    .unwrap_or_default()
}

// ─── Agent-layer permissions (agent_permissions table; vocabulary shared with the plugin layer) ──────────────────

/// Agent-layer decision, including the global-policy hard gate (priority in
/// docs/permissions.md "Global policy (hard gate)"):
///   1. global denied            → Denied (hard gate, overrides any per-agent granted)
///   2. agent_permissions override → that decision (per-agent still beats global granted/ask)
///   3. global granted / ask     → that decision (acts only as the new default)
///   4. otherwise                → built-in default table
/// Lock discipline: take the lock, read the row, release the guard as the `let` ends,
/// then call global_override / default_decision_for which lock on their own
/// (re-entering on the same thread would deadlock).
pub fn check_agent(store: &SharedStore, agent_id: &str, perm: &str) -> Decision {
    let found: Option<String> = store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT decision FROM agent_permissions WHERE agent_id = ?1 AND permission = ?2",
                    params![agent_id, perm],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
        })
        .flatten();
    let global = permission::global_override(store, perm);
    if global == Some(Decision::Denied) {
        return Decision::Denied;
    }
    if let Some(d) = found.as_deref().map(permission::parse_decision) {
        return d;
    }
    global.unwrap_or_else(|| permission::default_decision_for(store, perm))
}

/// §4.3 enforcement point 3 (Agent half): derived declared permissions **refuse to
/// be written as granted** — clicking Always at the Agent layer would also leave a
/// permanent grant row, and this plugs that (review 7).
pub fn set_agent_decision(store: &SharedStore, agent_id: &str, perm: &str, decision: &str) -> bool {
    if !permission::known_or_declared(store, perm) || !["granted", "denied", "ask"].contains(&decision) {
        return false;
    }
    if decision == "granted" && permission::is_declared(store, perm) {
        publish(
            "permission.denied",
            json!({
                "agentId": agent_id,
                "permission": perm,
                "reason": "declared-permission-once-only",
                "caller": "set-agent-decision"
            }),
        );
        return false;
    }
    let now = now_secs() as i64;
    store
        .lock()
        .ok()
        .and_then(|mut s| {
            s.with_conn(|c| {
                c.execute(
                    "INSERT INTO agent_permissions (agent_id, permission, scope, decision, updated_at)
                     VALUES (?1, ?2, NULL, ?3, ?4)
                     ON CONFLICT(agent_id, permission) DO UPDATE SET decision = ?3, updated_at = ?4",
                    params![agent_id, perm, decision, now],
                )
                .unwrap_or(0)
            })
        })
        .map(|n| n > 0)
        .unwrap_or(false)
}

// ─── CLI side: token file + detection + registration orchestration ─────────────────────────────────────

/// `~/.opencapx/agent-tokens/<kind>.token`, contents JSON {agent_id, token}.
pub fn token_file(kind: &str) -> std::path::PathBuf {
    let base = dirs::home_dir()
        .unwrap_or_else(|| std::env::temp_dir())
        .join(".opencapx")
        .join("agent-tokens");
    token_file_in(&base, kind)
}

fn token_file_in(dir: &std::path::Path, kind: &str) -> std::path::PathBuf {
    dir.join(format!("{}.token", sanitize_kind(kind)))
}

/// Invalidate the local token file so the next `ensure_registered` goes through TOFU again.
///
/// Used for self-healing after `bad_token` (40101): it must **not** be used for a
/// revoked agent (40102) — revocation is the user's deliberate decision and can only be
/// lifted by reauthorizing in the Settings page. Returns whether the file was actually
/// deleted.
pub fn reset_credentials(kind: &str) -> bool {
    std::fs::remove_file(token_file(kind)).is_ok()
}

pub fn load_credentials(kind: &str) -> Option<Credentials> {
    load_credentials_from(&token_file(kind))
}

fn load_credentials_from(path: &std::path::Path) -> Option<Credentials> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let agent_id = v.get("agent_id")?.as_str()?.to_string();
    let token = v.get("token")?.as_str()?.to_string();
    if agent_id.is_empty() || token.is_empty() {
        return None;
    }
    Some(Credentials { agent_id, token })
}

fn save_credentials(kind: &str, creds: &Credentials) {
    save_credentials_to(&token_file(kind), creds);
}

fn save_credentials_to(path: &std::path::Path, creds: &Credentials) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = json!({ "agent_id": creds.agent_id, "token": creds.token }).to_string();
    if std::fs::write(path, body).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

/// For the /event gateway: look up kind by agent_id (for session attribution).
pub fn kind_for(store: &SharedStore, agent_id: &str) -> Option<String> {
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT kind FROM agents WHERE agent_id = ?1",
                    params![agent_id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
        })
        .flatten()
}

/// For the Settings page/popups: look up display_name by agent_id; falls back to agent_id itself.
pub fn display_name_for_agent(store: &SharedStore, agent_id: &str) -> String {
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT display_name FROM agents WHERE agent_id = ?1",
                    params![agent_id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
        })
        .flatten()
        .unwrap_or_else(|| agent_id.to_string())
}

/// Detect the caller's kind: explicit env > host signature env > "custom".
/// Claude Code injects CLAUDECODE=1; opencode injects OPENCODE=1. Add new hosts here.
pub fn detect_kind() -> String {
    if let Ok(k) = std::env::var("OPEN_CAPX_AGENT") {
        let k = sanitize_kind(&k);
        if k != "custom" {
            return k;
        }
    }
    for (var, kind) in [("CLAUDECODE", "claude"), ("OPENCODE", "opencode"), ("CODEX", "codex")] {
        if let Ok(v) = std::env::var(var) {
            if v == "1" || v.eq_ignore_ascii_case("true") {
                return kind.into();
            }
        }
    }
    "custom".into()
}

/// CLI startup orchestration: use the token file if present; otherwise register and
/// persist when the Core is online. On failure, give recovery text by reason
/// (docs/permissions.md "Revocation and recovery"): Core not running → events queue
/// locally and drain automatically on next start; revoked (40102) → you can only
/// Reauthorize in the Settings page, get a new token, and write it back to the token
/// file manually — the CLI does not re-register automatically.
pub fn ensure_registered(kind: &str, via: &str) -> Option<Credentials> {
    if let Some(c) = load_credentials(kind) {
        return Some(c);
    }
    match crate::http::post_register(kind, &display_name_for(kind), via) {
        Ok((agent_id, token)) => {
            let creds = Credentials { agent_id, token };
            save_credentials(kind, &creds);
            Some(creds)
        }
        Err(crate::http::RegisterError::Unreachable) => {
            eprintln!(
                "OpenCapX: app not running ({}). Events will queue locally and drain \
                 the next time you start the app. kind={}",
                crate::http::LISTEN_ADDR,
                kind
            );
            None
        }
        Err(crate::http::RegisterError::Rejected { code, .. }) if code == 40102 => {
            eprintln!(
                "OpenCapX: this agent ({}) is revoked. To recover:\n  \
                 1. Open OpenCapX Settings -> Agents -> find this agent -> Reauthorize\n     \
                 (a new token is shown ONCE — copy it)\n  \
                 2. Write it to {} as: {{\"agent_id\":\"<agentId>\",\"token\":\"<new token>\"}}\n  \
                 3. Run this command again",
                kind,
                token_file(kind).display()
            );
            None
        }
        Err(e) => {
            eprintln!("OpenCapX: agent registration failed ({:?}); kind={}", e, kind);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::storage::StoreEnum;
    use std::sync::{Arc, Mutex};

    fn db_store(tag: &str) -> SharedStore {
        let dir = std::env::temp_dir().join(format!("opencapx-identity-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        // Cleanup is left to OS temp after tests; multiple tests use different tags
        s
    }

    fn mem_store() -> SharedStore {
        Arc::new(Mutex::new(StoreEnum::Mem(crate::core::agent::SessionStore::new())))
    }

    #[test]
    fn sanitize_kind_normalizes() {
        assert_eq!(sanitize_kind("Claude"), "claude");
        assert_eq!(sanitize_kind("My Agent!"), "my-agent");
        assert_eq!(sanitize_kind("  "), "custom");
        assert_eq!(sanitize_kind("--x--"), "x");
    }

    #[test]
    fn display_name_known_and_unknown() {
        assert_eq!(display_name_for("claude"), "Claude");
        assert_eq!(display_name_for("omp"), "Oh My Pi");
        assert_eq!(display_name_for("mybot"), "Mybot");
    }

    #[test]
    fn token_format_and_hash_roundtrip() {
        let t = gen_token();
        assert!(t.starts_with(TOKEN_PREFIX));
        assert_eq!(t.len(), TOKEN_PREFIX.len() + 64);
        // Hash determinism
        assert_eq!(hash_token(&t), hash_token(&t));
        assert_ne!(hash_token(&t), hash_token("ocx1_other"));
    }

    #[test]
    fn ct_eq_behaves() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn register_verify_revoke_reauthorize_cycle() {
        let s = db_store("cycle");
        // TOFU first time
        let (id1, tok1) = register(&s, "Claude", "mcp").expect("register");
        assert!(id1.starts_with("ag_claude_"));
        assert_eq!(verify(&s, &id1, &tok1), VerifyResult::Ok);
        assert_eq!(verify(&s, &id1, "ocx1_wrong"), VerifyResult::BadToken);
        assert_eq!(verify(&s, "ag_ghost_0000", &tok1), VerifyResult::UnknownAgent);
        // Re-registration = rotation, same agent_id
        let (id2, tok2) = register(&s, "claude", "hook").expect("rotate");
        assert_eq!(id1, id2);
        assert_ne!(tok1, tok2);
        assert_eq!(verify(&s, &id1, &tok1), VerifyResult::BadToken, "old token is invalidated");
        assert_eq!(verify(&s, &id1, &tok2), VerifyResult::Ok);
        // Revoke → rejected; auto-registration rejected; reauthorize → new token works
        assert!(revoke(&s, &id1));
        assert_eq!(verify(&s, &id1, &tok2), VerifyResult::Revoked);
        assert_eq!(register(&s, "claude", "mcp"), Err("revoked"));
        let tok3 = reauthorize(&s, &id1).expect("reauthorize");
        assert_eq!(verify(&s, &id1, &tok3), VerifyResult::Ok);
        // Visible in list
        let l = list(&s);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].agent_id, id1);
        assert_eq!(l[0].status, "active");
    }

    #[test]
    fn agent_permissions_default_override_and_validation() {
        let s = db_store("perms");
        let (id, _) = register(&s, "codex", "mcp").unwrap();
        // Default table takes effect
        assert_eq!(check_agent(&s, &id, "pet.animation"), Decision::Granted);
        assert_eq!(check_agent(&s, &id, "clipboard.read"), Decision::Ask);
        // Override
        assert!(set_agent_decision(&s, &id, "clipboard.read", "granted"));
        assert_eq!(check_agent(&s, &id, "clipboard.read"), Decision::Granted);
        // Unknown permission / illegal decision refuses to write
        assert!(!set_agent_decision(&s, &id, "nope.perm", "granted"));
        assert!(!set_agent_decision(&s, &id, "clipboard.read", "yolo"));
        // No cross-contamination: another agent is unaffected by the override
        let (id2, _) = register(&s, "opencode", "mcp").unwrap();
        assert_eq!(check_agent(&s, &id2, "clipboard.read"), Decision::Ask);
    }

    /// Global-policy hard-gate priority (§ "Global policy (hard gate)"):
    /// global denied > per-agent override > global granted/ask > built-in default.
    #[test]
    fn check_agent_global_policy_priority() {
        let s = db_store("global-policy");
        let (id, _) = register(&s, "claude", "mcp").unwrap();
        // Baseline: no overrides at all goes to the built-in default
        assert_eq!(check_agent(&s, &id, "clipboard.read"), Decision::Ask);
        // ③ global granted, no per-agent row → Granted (global acts only as the new default)
        permission::set_global_override(&s, "clipboard.read", "granted").unwrap();
        assert_eq!(check_agent(&s, &id, "clipboard.read"), Decision::Granted);
        permission::set_global_override(&s, "clipboard.read", "ask").unwrap();
        assert_eq!(check_agent(&s, &id, "clipboard.read"), Decision::Ask);
        // ② per-agent override beats global granted
        permission::set_global_override(&s, "clipboard.read", "granted").unwrap();
        assert!(set_agent_decision(&s, &id, "clipboard.read", "denied"));
        assert_eq!(check_agent(&s, &id, "clipboard.read"), Decision::Denied);
        // ① global denied is a hard gate: overrides per-agent granted
        assert!(set_agent_decision(&s, &id, "clipboard.read", "granted"));
        assert_eq!(check_agent(&s, &id, "clipboard.read"), Decision::Granted);
        permission::set_global_override(&s, "clipboard.read", "denied").unwrap();
        assert_eq!(check_agent(&s, &id, "clipboard.read"), Decision::Denied);
        // The hard gate applies to another agent too (global semantics, not per-agent)
        let (id2, _) = register(&s, "codex", "mcp").unwrap();
        assert_eq!(check_agent(&s, &id2, "clipboard.read"), Decision::Denied);
        // reset → falls back to the per-agent override
        permission::clear_global_override(&s, "clipboard.read").unwrap();
        assert_eq!(check_agent(&s, &id, "clipboard.read"), Decision::Granted);
        // High-risk cannot be globally granted, but can be globally denied (and the hard gate applies)
        assert!(permission::set_global_override(&s, "process.execute", "granted").is_err());
        assert!(permission::set_global_override(&s, "process.execute", "denied").is_ok());
        assert_eq!(check_agent(&s, &id, "process.execute"), Decision::Denied);
    }

    /// The hard-gate path must not re-entrantly deadlock: run it on a child thread with a timeout so a real deadlock fails the test instead of hanging the suite.
    #[test]
    fn check_agent_hard_gate_does_not_reentrant_deadlock() {
        let s = db_store("gate-deadlock");
        let (id, _) = register(&s, "claude", "mcp").unwrap();
        permission::set_global_override(&s, "clipboard.read", "denied").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let store = s.clone();
        let agent = id.clone();
        std::thread::spawn(move || {
            let _ = tx.send(check_agent(&store, &agent, "clipboard.read"));
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("check_agent re-entrant deadlock (timeout): global_override must only be called after releasing the lock"),
            Decision::Denied
        );
    }

    #[test]
    fn mem_store_register_fails_gracefully() {
        let s = mem_store();
        assert_eq!(register(&s, "claude", "mcp"), Err("store unavailable"));
        assert_eq!(verify(&s, "ag_x_00", "ocx1_x"), VerifyResult::UnknownAgent);
        assert!(!revoke(&s, "ag_x_00"));
        assert!(list(&s).is_empty());
    }

    #[test]
    fn token_file_roundtrip_and_permissions_bits() {
        // Explicit path, leaves HOME untouched (changing env under parallel tests would pollute other tests)
        let dir = std::env::temp_dir().join(format!("opencapx-tok-{}-{}", std::process::id(), nanos()));
        let _ = std::fs::create_dir_all(&dir);
        let creds = Credentials { agent_id: "ag_t_01".into(), token: "ocx1_t".into() };
        save_credentials_to(&token_file_in(&dir, "Claude Code!"), &creds);
        // Same name after kind normalization (sanitize → claude-code)
        assert_eq!(load_credentials_from(&token_file_in(&dir, "claude-code")), Some(creds));
        assert!(load_credentials_from(&token_file_in(&dir, "codex")).is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_file_in(&dir, "claude-code"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "token file must be 0600");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_kind_prefers_env() {
        // custom when no env is set (the test process has no CLAUDECODE)
        std::env::remove_var("OPEN_CAPX_AGENT");
        let k = detect_kind();
        assert!(k == "custom" || k == "claude" || k == "opencode" || k == "codex");
    }
}
