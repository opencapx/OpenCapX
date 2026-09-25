//! the Ask tier: in-flight ask channels, the UiAsker bridge, session-tier grants, and audit emission.
//! Mechanical move from core/permission.rs.

use super::*;

/// docs/permissions.md: high-risk permissions can only be granted once at a time, Always is not offered.
pub const HIGH_RISK: &[&str] = &[
    "process.execute",
    "filesystem.write",
    "microphone",
    "camera",
    // v1.3: driving apps on the user's behalf / synthesizing input / installing plugins — impact extends beyond this process, always grant once at a time
    "automation.control",
    "input.control",
    "plugin.install",
    // v1.4: chat history (machine-wide FDA surface) / window manipulation (computer-use surface) / power actions
    // (irreversible) / mail (phishing surface) — likewise grant only once at a time
    "messages.read",
    "window.management",
    "power.control",
    "mail.read",
];

pub const ASK_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) fn asks() -> &'static Mutex<HashMap<String, Sender<String>>> {
    static ASKS: OnceLock<Mutex<HashMap<String, Sender<String>>>> = OnceLock::new();
    ASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Callback command entry for the frontend to answer the permission prompt. answer: once | always | deny.
pub fn resolve_ask(id: &str, answer: &str) -> bool {
    match asks().lock() {
        Ok(mut m) => match m.remove(id) {
            Some(tx) => {
                let sent = tx.send(answer.to_string()).is_ok();
                if !sent {
                    eprintln!("ask(perm): receiver for {id} gone before answer arrived");
                }
                sent
            }
            None => {
                // stale dialog (already timed out) or an answer routed to the wrong registry
                eprintln!("ask(perm): answer {answer:?} for unknown/expired id {id} dropped");
                false
            }
        },
        Err(_) => {
            eprintln!("ask(perm): registry lock poisoned, answer for {id} dropped");
            false
        }
    }
}

/// F12 — injectable abstraction for prompt asks: production uses [`UiAsker`] (event + channel wait, same behavior as the old implementation),
/// tests inject a stub to cover the four branches: allow once / Always (downgraded) / deny / timeout.
pub struct AskRequest {
    pub id: String,
    pub event: &'static str,
    pub payload: serde_json::Value,
    pub timeout: Duration,
}

/// F12 — ask outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskOutcome {
    /// User answer ("once" / "always" / "deny").
    Answered(String),
    /// UI absent (test process / frontend not started).
    NoUi,
    /// Timed out with no answer.
    Timeout,
}

pub trait Asker: Send + Sync {
    /// Whether the UI is available (unavailable takes the non-interactive fallback path; gate series = fast deny).
    fn is_available(&self) -> bool;
    /// Initiate an ask and wait for the answer (only called when `is_available()` is true).
    fn ask(&self, req: &AskRequest) -> AskOutcome;
}

/// Production implementation: emit event → wait for the frontend `answer_*` command to respond → timeout.
pub struct UiAsker;

impl Asker for UiAsker {
    fn is_available(&self) -> bool {
        crate::core::app_handle().is_some()
    }

    fn ask(&self, req: &AskRequest) -> AskOutcome {
        let Some(app) = crate::core::app_handle() else {
            return AskOutcome::NoUi;
        };
        let (tx, rx) = mpsc::channel::<String>();
        if let Ok(mut m) = asks().lock() {
            m.insert(req.id.clone(), tx);
        }
        let _ = app.emit(req.event, req.payload.clone());
        let outcome = await_answer(&rx, req.timeout);
        if let Ok(mut m) = asks().lock() {
            m.remove(&req.id);
        }
        outcome
    }
}

/// F6 — wait for the user's answer; the timeout ceiling is pinned by the caller (`req.timeout`, production = `ASK_TIMEOUT` 60s).
/// Kept as a separate function so unit tests can pin the "timeout → Timeout" semantics (the production path needs a real AppHandle).
pub(crate) fn await_answer(rx: &mpsc::Receiver<String>, timeout: Duration) -> AskOutcome {
    match rx.recv_timeout(timeout) {
        Ok(a) => AskOutcome::Answered(a),
        Err(_) => AskOutcome::Timeout,
    }
}

pub(crate) fn default_asker() -> &'static UiAsker {
    static DEFAULT: UiAsker = UiAsker;
    &DEFAULT
}

// ===== v1.5 Session-tier grants: Ask's third tier, in-process memory, cleared on restart =====
// semantics sit between once (this call only) and always (persisted): no more asks for this process's lifetime.
// only takes effect when the DB decision is ask; explicit persisted granted/denied values win. High-risk permissions allow
// the session tier — a pressure valve for prompt fatigue that opens no permanent hole. Declared-derived permissions (once-only,
// anti-Always laundering) do not accept session and auto-downgrade to once.
fn session_grants() -> &'static std::sync::Mutex<std::collections::HashSet<(String, String)>> {
    static G: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<(String, String)>>> =
        std::sync::OnceLock::new();
    G.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

pub(crate) fn session_grant(principal: &str, permission: &str) {
    if let Ok(mut g) = session_grants().lock() {
        g.insert((principal.to_string(), permission.to_string()));
    }
}

pub(crate) fn session_has(principal: &str, permission: &str) -> bool {
    session_grants()
        .lock()
        .map(|g| g.contains(&(principal.to_string(), permission.to_string())))
        .unwrap_or(false)
}

/// Revoke all session grants (settings page entry / test isolation).
pub fn session_revoke_all() {
    if let Ok(mut g) = session_grants().lock() {
        g.clear();
    }
}

pub(crate) fn audit(kind: &str, plugin_id: &str, permission: &str, extra: serde_json::Value) {
    let mut payload = json!({ "pluginId": plugin_id, "permission": permission });
    if let (Some(o), Some(e)) = (payload.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
        &format!("permission.{}", kind),
        "core",
        payload,
    ));
}

/// Global policy change audit (docs/permissions.md "Global policy (hard gate)"): event name
/// `permission.policy.changed`,payload {permission, decision, by}。
/// does not reuse `audit()` — that path's subject is the plugin (with pluginId); global policy has no plugin subject.
pub(crate) fn audit_policy(permission: &str, decision: &str) {
    crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
        "permission.policy.changed",
        "core",
        json!({ "permission": permission, "decision": decision, "by": "settings" }),
    ));
}

pub fn decision_str(d: Decision) -> &'static str {
    match d {
        Decision::Granted => "granted",
        Decision::Denied => "denied",
        Decision::Ask => "ask",
    }
}
