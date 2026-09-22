//! Permission Manager: checked in the Rust Core, before capability execution. See docs/permissions.md.
//! ask decisions go through a runtime prompt: Allow once / Always / Deny; 60s of no action counts as Deny;
//! high-risk permissions are not offered Always. When the UI is absent (test process / frontend not ready), ask quickly resolves to Deny.

use super::storage::SharedStore;
use rusqlite::params;
use serde_json::json;
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::Emitter;

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

fn asks() -> &'static Mutex<HashMap<String, Sender<String>>> {
    static ASKS: OnceLock<Mutex<HashMap<String, Sender<String>>>> = OnceLock::new();
    ASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn nanos() -> u128 {
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
        super::app_handle().is_some()
    }

    fn ask(&self, req: &AskRequest) -> AskOutcome {
        let Some(app) = super::app_handle() else {
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
fn await_answer(rx: &mpsc::Receiver<String>, timeout: Duration) -> AskOutcome {
    match rx.recv_timeout(timeout) {
        Ok(a) => AskOutcome::Answered(a),
        Err(_) => AskOutcome::Timeout,
    }
}

fn default_asker() -> &'static UiAsker {
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

fn session_grant(principal: &str, permission: &str) {
    if let Ok(mut g) = session_grants().lock() {
        g.insert((principal.to_string(), permission.to_string()));
    }
}

fn session_has(principal: &str, permission: &str) -> bool {
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

fn audit(kind: &str, plugin_id: &str, permission: &str, extra: serde_json::Value) {
    let mut payload = json!({ "pluginId": plugin_id, "permission": permission });
    if let (Some(o), Some(e)) = (payload.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
        &format!("permission.{}", kind),
        "core",
        payload,
    ));
}

/// Global policy change audit (docs/permissions.md "Global policy (hard gate)"): event name
/// `permission.policy.changed`,payload {permission, decision, by}。
/// does not reuse `audit()` — that path's subject is the plugin (with pluginId); global policy has no plugin subject.
fn audit_policy(permission: &str, decision: &str) {
    super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionEntryDto {
    pub permission: String,
    /// Currently effective decision (DB override > default table)
    pub decision: String,
    pub default: String,
    pub high_risk: bool,
    /// §4.7: declared-derived ("unverified domain" marker + once-only, the UI does not offer granted)
    pub declared: bool,
    /// Global policy override (permission_policy table; "" = no override). denied is a hard gate,
    /// overriding every per-agent granted — the UI shows the "global" badge based on this.
    #[serde(default)]
    pub global: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginPermissionsDto {
    pub plugin_id: String,
    pub name: String,
    pub status: String,
    pub permissions: Vec<PermissionEntryDto>,
}

/// Settings page Permissions view data: grouped by plugin, taking the permissions declared in the manifest
/// **∪ the plugin's permissions in the frozen declaration table** (§4.3 integration point #6: inline-mapped permissions may be absent from
/// `permissions[]`; missing them means "installed but invisible and uncontrollable on the settings page").
pub fn view(
    store: &SharedStore,
    plugins: &[super::plugin::PluginStatusDto],
) -> Vec<PluginPermissionsDto> {
    let all_decls = super::declaration::all(store);
    plugins
        .iter()
        .map(|p| {
            let mut perms: Vec<String> = p.permissions.clone();
            for d in all_decls.iter().filter(|d| d.plugin_id == p.id) {
                if !perms.contains(&d.permission) {
                    perms.push(d.permission.clone());
                }
            }
            perms.sort();
            PluginPermissionsDto {
                plugin_id: p.id.clone(),
                name: p.name.clone(),
                status: p.status.clone(),
                permissions: perms
                    .iter()
                    .map(|perm| PermissionEntryDto {
                        permission: perm.clone(),
                        decision: decision_str(check(store, &p.id, perm)).to_string(),
                        default: decision_str(default_decision_for(store, perm)).to_string(),
                        high_risk: HIGH_RISK.contains(&perm.as_str()),
                        declared: is_declared(store, perm),
                        global: global_override_str(store, perm).to_string(),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// Settings page Agents view data: the full permission vocabulary for a single agent (all listed,
/// decision = agent_permissions override > default table; default lets the frontend mark "unchanged").
/// §4.3 integration point #6: vocabulary = static table ∪ frozen declaration table, otherwise new-domain permissions are "denied by default and impossible to grant".
pub fn agent_view(store: &SharedStore, agent_id: &str) -> Vec<PermissionEntryDto> {
    let mut entries: Vec<PermissionEntryDto> = PERMISSIONS
        .iter()
        .map(|(perm, default)| PermissionEntryDto {
            permission: perm.to_string(),
            decision: decision_str(super::identity::check_agent(store, agent_id, perm)).to_string(),
            default: default.to_string(),
            high_risk: HIGH_RISK.contains(perm),
            declared: false,
            global: global_override_str(store, perm).to_string(),
        })
        .collect();
    for (perm, default) in super::declaration::declared_permission_defaults(store) {
        if entries.iter().any(|e| e.permission == perm) {
            continue;
        }
        entries.push(PermissionEntryDto {
            decision: decision_str(super::identity::check_agent(store, agent_id, &perm)).to_string(),
            default: decision_str(default).to_string(),
            high_risk: HIGH_RISK.contains(&perm.as_str()),
            declared: true,
            global: global_override_str(store, &perm).to_string(),
            permission: perm,
        });
    }
    entries.sort_by(|a, b| a.permission.cmp(&b.permission));
    entries
}

/// (permission, default decision). New permissions must be synced to docs/permissions.md.
pub const PERMISSIONS: &[(&str, &str)] = &[
    ("pet.animation", "granted"),
    ("storage.local", "granted"),
    ("notification.post", "ask"),
    ("image.read", "ask"),
    ("file.read", "ask"),
    ("clipboard.read", "ask"),
    ("clipboard.write", "ask"),
    ("network.request", "ask"),
    ("browser.control", "ask"),
    ("screen.capture", "ask"),
    ("microphone", "denied"),
    ("camera", "denied"),
    ("filesystem.write", "denied"),
    ("process.execute", "denied"),
    // v1.3 high/medium-value batch (docs/permissions.md v1.3)
    ("automation.control", "ask"),   // launch/drive other apps (AppleScript), high-risk
    ("input.control", "denied"),     // synthesize keyboard/mouse events, high-risk
    ("plugin.install", "denied"),    // v1.3 vocabulary placeholder: no capability mapping yet, high-risk
    ("photos.read", "ask"),
    ("contacts.read", "ask"),
    ("calendar.read", "ask"),
    ("location.read", "ask"),
    ("audio.output", "ask"),         // audio output channel, same tier as notification.post
    ("url.scheme.open", "ask"),      // registered schemes such as mailto:/zoommtg:
    // v1.4 high-value batch (docs/permissions.md v1.4)
    ("media.control", "ask"),        // playback control (play/pause/skip)
    ("messages.read", "denied"),     // iMessage chat history (needs machine-wide Full Disk Access), high-risk
    ("window.management", "denied"), // list/focus windows (computer-use foundation), high-risk
    ("power.control", "denied"),     // sleep/lock screen (irreversible actions), high-risk
    // v1.4 medium-value batch
    ("notes.read", "ask"),
    ("reminders.read", "ask"),
    ("reminders.write", "ask"),      // record reminders for the user; write surface but confined to the Reminders app
    ("mail.read", "ask"),            // mail metadata (large phishing surface), high-risk
    ("system.settings", "ask"),      // dark mode/wallpaper/volume
    ("printer.control", "ask"),
    // v1.5 Things data surface (docs/permissions.md v1.5): reads and writes both ask, not high-risk
    ("things.read", "ask"),
    ("things.write", "ask"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Granted,
    Denied,
    Ask,
}

pub fn known(permission: &str) -> bool {
    PERMISSIONS.iter().any(|(p, _)| *p == permission)
}

// ─── §4.2 lexicon and reserved sets (docs/permission-domains.md Step 1)─────────────────────

/// Capability/permission name lexicon: `^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$` —
/// 2+ segments, each starting with a lowercase letter, length ≤ 64, pure ASCII. The pure-ASCII check is equivalent to
/// "still pure ASCII after NFKC normalization": ASCII strings are unchanged by NFKC, while non-ASCII (including homoglyphs/full-width)
/// fails the lexicon as-is and is rejected. Empty segments/leading-trailing-double dots are covered by the split + empty-segment check.
pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 || !name.is_ascii() {
        return false;
    }
    let mut segments = 0usize;
    for seg in name.split('.') {
        segments += 1;
        let b = seg.as_bytes();
        if b.is_empty() || !b[0].is_ascii_lowercase() {
            return false;
        }
        if !b
            .iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        {
            return false;
        }
    }
    segments >= 2
}

/// Name's first segment (domain). The caller guarantees valid_name has passed (so there are no empty segments).
pub fn first_segment(name: &str) -> &str {
    name.split('.').next().unwrap_or("")
}

/// Reserved capability set = CAPABILITY_IDS: a reserved id may only be a provider in string form;
/// declaring a reserved id in object form is rejected outright (§4.2 reserved-domain closure).
pub fn reserved_capability(id: &str) -> bool {
    super::capability::CAPABILITY_IDS.contains(&id)
}

/// Reserved domains = {all first segments of CAPABILITY_IDS ∪ PERMISSIONS} ∪ {"opencapx"} (§4.2).
/// A plugin's declared new domain must not fall within a reserved domain (review H1: prevent squatting on official domains).
pub fn reserved_domain(domain: &str) -> bool {
    domain == "opencapx"
        || super::capability::CAPABILITY_IDS
            .iter()
            .any(|c| first_segment(c) == domain)
        || PERMISSIONS.iter().any(|(p, _)| first_segment(p) == domain)
}

pub fn default_decision(permission: &str) -> Decision {
    parse_decision(PERMISSIONS
        .iter()
        .find(|(p, _)| *p == permission)
        .map(|(_, d)| *d)
        .unwrap_or("denied"))
}

pub fn parse_decision(s: &str) -> Decision {
    match s {
        "granted" => Decision::Granted,
        "ask" => Decision::Ask,
        _ => Decision::Denied,
    }
}

/// capability → permission required to execute. See the docs/capability.md v1 standard set.
pub fn capability_permission(capability: &str) -> Option<&'static str> {
    Some(match capability {
        "image.analyze" | "image.ocr" => "image.read",
        "audio.transcribe" | "file.read" => "file.read",
        // v1.2 tightening: speech output is a human-facing channel (same spam/phishing surface as notifications),
        // remapped from storage.local (granted) to notification.post (ask)
        "speech.synthesize" => "notification.post",
        "screen.capture" => "screen.capture",
        // v1.3 subscription screenshot diff: data surface equals screen.capture (every frame's content passes through),
        // same permission tier; judged once when the subscription is established, not per frame
        "screen.watch" => "screen.capture",
        "clipboard.read" => "clipboard.read",
        "clipboard.write" => "clipboard.write",
        "file.write" => "filesystem.write",
        "file.search" => "file.read",
        // subscribe type (docs/capability.md "capability typing"): reads metadata only, same tier as file.search
        "file.watch" => "file.read",
        // A7 Computer Context: app name/window title are low-sensitivity, but the reply includes clipboard fragments —
        // take the strictest component and pass the Agent-layer gate at the clipboard.read tier (ask)
        "context.get_current" => "clipboard.read",
        "browser.open" | "browser.read" => "browser.control",
        // v1.3: automation / input / PIM / audio / scheme
        "automation.run" => "automation.control",
        "input.send" => "input.control",
        "photos.read" => "photos.read",
        "contacts.search" => "contacts.read",
        "calendar.events" => "calendar.read",
        "location.get" => "location.read",
        "audio.play" => "audio.output",
        "url.scheme.open" => "url.scheme.open",
        // v1.4 high/medium-value batch
        "media.playback" => "media.control",
        "messages.recent" => "messages.read",
        "window.list" | "window.focus" => "window.management",
        "system.sleep" | "system.lock" => "power.control",
        "notes.read" => "notes.read",
        "reminders.read" => "reminders.read",
        "reminders.write" => "reminders.write",
        "mail.recent" => "mail.read",
        "system.settings" => "system.settings",
        "printer.print" => "printer.control",
        // v1.5 Things data surface: the three reads go through things.read, the three writes through things.write (both ask)
        "things.list" | "things.show" | "things.search" => "things.read",
        "things.add" | "things.update" | "things.delete" => "things.write",
        _ => return None,
    })
}

/// Look up the plugin-level decision: DB override > default table.
pub fn check(store: &SharedStore, plugin_id: &str, permission: &str) -> Decision {
    let found: Option<String> = store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT decision FROM plugin_permissions WHERE plugin_id = ?1 AND permission = ?2",
                    params![plugin_id, permission],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
        })
        .flatten();
    found.as_deref().map(parse_decision).unwrap_or_else(|| default_decision_for(store, permission))
}

/// Explicit grant/revoke (settings page, install confirmation, runtime Always writeback).
/// §4.3 enforcement point 3: declared-derived permissions **refuse to be written as granted** — the settings page can only set ask / denied,
/// closeable but not permanently openable (the other half of Always-laundering H2).
pub fn set_decision(store: &SharedStore, plugin_id: &str, permission: &str, decision: &str) -> bool {
    if !known_or_declared(store, permission) || !["granted", "denied", "ask"].contains(&decision) {
        return false;
    }
    if decision == "granted" && is_declared(store, permission) {
        audit(
            "denied",
            plugin_id,
            permission,
            json!({ "reason": "declared-permission-once-only", "caller": "set-decision" }),
        );
        return false;
    }
    let now = super::agent::now_secs();
    store
        .lock()
        .ok()
        .and_then(|mut s| {
            s.with_conn(|c| {
                c.execute(
                    "INSERT INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at)
                     VALUES (?1, ?2, NULL, ?3, ?4)
                     ON CONFLICT(plugin_id, permission) DO UPDATE SET decision = ?3, updated_at = ?4",
                    params![plugin_id, permission, decision, now],
                )
                .unwrap_or(0)
            })
        })
        .map(|n| n > 0)
        .unwrap_or(false)
}

// ─── Global policy (hard gate; docs/permissions.md "Global policy (hard gate)")───────────────────
// permission_policy table: a global policy layer sitting above the Agent layer. Only two semantics:
//   · global denied = hard gate, overriding every per-agent granted (true kill switch)
//   · global granted / ask = only a new default; per-agent overrides still win
// the hard gate is enforced in identity::check_agent; this layer only handles storage and audit.

/// Look up the global policy override. None = no override (continue with per-agent / built-in default).
/// The Mem variant / lock acquisition failure is likewise treated as no override.
pub fn global_override(store: &SharedStore, permission: &str) -> Option<Decision> {
    // the lock guard is released at the end of the `let` statement; do not re-acquire the lock while holding it (same-thread reentrant deadlock)
    let found: Option<String> = store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                c.query_row(
                    "SELECT decision FROM permission_policy WHERE permission = ?1",
                    params![permission],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
        })
        .flatten();
    found.as_deref().map(parse_decision)
}

/// Global policy's UI string form ("" = no override, otherwise granted/ask/denied).
pub fn global_override_str(store: &SharedStore, permission: &str) -> &'static str {
    match global_override(store, permission) {
        Some(d) => decision_str(d),
        None => "",
    }
}

/// Write global policy. Only permissions within the static vocabulary; high-risk permissions reject global granted (high-risk allows only once-at-a-time grants).
/// On success emits a `permission.policy.changed` audit event.
pub fn set_global_override(store: &SharedStore, permission: &str, decision: &str) -> Result<(), String> {
    if !known(permission) {
        return Err(format!("unknown permission: {}", permission));
    }
    if !["granted", "denied", "ask"].contains(&decision) {
        return Err(format!("invalid decision: {}", decision));
    }
    if decision == "granted" && HIGH_RISK.contains(&permission) {
        return Err(
            "high-risk permission cannot be globally granted; allow once at runtime".into(),
        );
    }
    let now = super::agent::now_secs() as i64;
    let res = store.lock().ok().and_then(|mut s| {
        s.try_with_conn(|c| {
            c.execute(
                "INSERT INTO permission_policy (permission, decision, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(permission) DO UPDATE SET decision = ?2, updated_at = ?3",
                params![permission, decision, now],
            )
            .map(|_| ())
            .map_err(|e| format!("write policy failed: {}", e))
        })
    });
    match res {
        Some(Ok(())) => {
            audit_policy(permission, decision);
            Ok(())
        }
        Some(Err(e)) => Err(e),
        None => Err("storage unavailable".into()),
    }
}

/// Clear the global policy override (restores the built-in default). Also emits an audit event, with decision recorded as "none".
pub fn clear_global_override(store: &SharedStore, permission: &str) -> Result<(), String> {
    let res = store.lock().ok().and_then(|mut s| {
        s.try_with_conn(|c| {
            c.execute(
                "DELETE FROM permission_policy WHERE permission = ?1",
                params![permission],
            )
            .map(|_| ())
            .map_err(|e| format!("clear policy failed: {}", e))
        })
    });
    match res {
        Some(Ok(())) => {
            audit_policy(permission, "none");
            Ok(())
        }
        Some(Err(e)) => Err(e),
        None => Err("storage unavailable".into()),
    }
}

/// Reverse lookup: which core capabilities use a given permission (settings page "System capabilities" behavior labels).
pub fn core_capabilities_for(permission: &str) -> Vec<&'static str> {
    super::capability::CAPABILITY_IDS
        .iter()
        .copied()
        .filter(|cap| capability_permission(cap) == Some(permission))
        .collect()
}

/// Global policy view entry (settings page "System capabilities" tab data source).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorePermPolicyDto {
    pub permission: String,
    /// UI text for the core capabilities this permission covers (e.g. image.read → image.analyze · image.ocr)
    pub capabilities: Vec<String>,
    /// Built-in default tier (static table)
    pub builtin_default: String,
    /// Global override; None = follow the default (camelCase would produce overrideDecision,
    /// but the frontend contract wants "override", hence the explicit rename)
    #[serde(rename = "override")]
    pub override_decision: Option<String>,
    /// Effective decision at the global layer: denied when global is denied, otherwise equal to the built-in default
    pub effective: String,
    pub high_risk: bool,
}

/// Settings page "System capabilities" data: one row per permission **with a capability mapping**.
/// Permissions without a mapping (pet.animation / storage.local / network.request / microphone /
/// camera / process.execute / plugin.install) are not listed — they are not subject to a capability gate.
pub fn core_policy_list(store: &SharedStore) -> Vec<CorePermPolicyDto> {
    let mut out: Vec<CorePermPolicyDto> = PERMISSIONS
        .iter()
        .filter_map(|(perm, default)| {
            let capabilities = core_capabilities_for(perm);
            if capabilities.is_empty() {
                return None;
            }
            let ov = global_override(store, perm);
            Some(CorePermPolicyDto {
                permission: perm.to_string(),
                capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
                builtin_default: default.to_string(),
                override_decision: ov.map(|d| decision_str(d).to_string()),
                effective: match ov {
                    Some(Decision::Denied) => "denied".to_string(),
                    _ => default.to_string(),
                },
                high_risk: HIGH_RISK.contains(perm),
            })
        })
        .collect();
    out.sort_by(|a, b| a.permission.cmp(&b.permission));
    out
}

/// One item of install confirmation (§4.3 integration points #3 / #7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallAsk {
    pub permission: String,
    /// true = derived from a plugin-domain declaration → **once-only** (no Always; not grantable on the settings page)
    pub declared: bool,
    /// The default written for a declared item in the manifest (ask | denied); ignored for built-in items
    pub declared_default: String,
}

/// Default for declared-derived (§4.3 integration point #2): static table first, then frozen declaration table, then denied.
/// **Never implicit granted**.
pub fn default_decision_for(store: &SharedStore, permission: &str) -> Decision {
    if known(permission) {
        return default_decision(permission);
    }
    super::declaration::declared_default(store, permission).unwrap_or(Decision::Denied)
}

/// Whether a permission name is enforceable: built-in vocabulary ∪ frozen declaration table (shared by §4.3 integration points #3 / #4 / #5).
pub fn known_or_declared(store: &SharedStore, permission: &str) -> bool {
    known(permission) || super::declaration::is_declared_permission(store, permission)
}

/// Whether the permission name is **declared-derived** (the criterion for the four once-only enforcement points, §4.3).
pub fn is_declared(store: &SharedStore, permission: &str) -> bool {
    super::declaration::is_declared_permission(store, permission)
}

/// once-only criterion (§4.3 enforcement points 1/2): neither **high-risk** nor **declared-derived** permissions may offer Always.
/// Shared by `gate()` and `gate_agent()`; if either is true then `can_always = false`.
pub fn can_always(store: &SharedStore, permission: &str) -> bool {
    !HIGH_RISK.contains(&permission) && !is_declared(store, permission)
}

/// Permission confirmation at install time: the implementation behind the install bubble in docs/permissions.md.
/// - Shows a one-time dialog with per-item Always Allow / Permit Once / Deny (high-risk only Permit Once/Deny)
/// - §4.4 consent-before-commit (review C2): **collects decisions only, zero DB writes** —
///   returns a `(permission, decision)` list; the caller swaps the directory then persists in a single transaction;
///   deny / timeout → Err, and the caller must not produce any effective change (the installed version is unaffected)
/// - UI absent (test process): collect per the default table (review 4); persistence is likewise left to the commit phase
pub fn confirm_install(plugin_id: &str, plan: &[InstallAsk]) -> Result<Vec<(String, String)>, String> {
    confirm_install_with(default_asker(), plugin_id, plan)
}

/// F12 — injectable version of confirm_install: identical behavior (UI absent collects per the default table;
/// deny / timeout / invalid answer → Err, and the caller must not produce any effective change).
pub fn confirm_install_with(
    asker: &dyn Asker,
    plugin_id: &str,
    plan: &[InstallAsk],
) -> Result<Vec<(String, String)>, String> {
    if !asker.is_available() {
        return confirm_install_noninteractive(plugin_id, plan);
    }
    let mut decisions: Vec<(String, String)> = Vec::with_capacity(plan.len());
    for ask in plan {
        let perm = &ask.permission;
        if !ask.declared && !known(perm) {
            audit("denied", plugin_id, perm, json!({ "reason": "unknown-permission", "caller": "install" }));
            return Err(format!("unknown permission {} (plugin declares unknown capability?)", perm));
        }
        // review 3 + 7: declared-derived permissions **must not be Always** (the install dialog must block it too,
        // otherwise H2's once-only only blocks the runtime prompt)
        let can_always = !HIGH_RISK.contains(&perm.as_str()) && !ask.declared;
        let id = format!("install-{}-{}", plugin_id, nanos());
        audit(
            "install_requested",
            plugin_id,
            perm,
            json!({ "scope": null, "caller": "install", "canAlways": can_always, "declared": ask.declared }),
        );
        let payload = json!({
            "id": id,
            "pluginId": plugin_id,
            "permission": perm,
            "canAlways": can_always,
            "declared": ask.declared,
        });
        let decision = match asker.ask(&AskRequest {
            id: id.clone(),
            event: "opencapx-install-ask",
            payload,
            timeout: ASK_TIMEOUT,
        }) {
            AskOutcome::Answered(a) => match a.as_str() {
                "always" if can_always => {
                    audit("granted", plugin_id, perm, json!({ "decision": "always", "caller": "install" }));
                    "granted"
                }
                // §4.3 enforcement point 4: answering Always when Always is not allowed (high-risk / declared-derived) →
                // downgrade to once rather than rejecting the whole batch (an out-of-sync old frontend will not break the install)
                "always" => {
                    audit(
                        "granted",
                        plugin_id,
                        perm,
                        json!({ "decision": "once", "caller": "install", "downgradedFrom": "always" }),
                    );
                    "ask"
                }
                "once" => {
                    audit("granted", plugin_id, perm, json!({ "decision": "once", "caller": "install" }));
                    "ask"
                }
                "deny" => {
                    audit("denied", plugin_id, perm, json!({ "reason": "user", "caller": "install" }));
                    // the user denying one item at install → the whole plugin install fails (aligned with the docs install flow: item-by-item confirmation).
                    // at this point the directory has not been swapped / nothing persisted, so the installed version is intact (consent-before-commit).
                    return Err(format!("install denied: {} {}", plugin_id, perm));
                }
                _ => {
                    audit("denied", plugin_id, perm, json!({ "reason": "invalid-answer", "caller": "install" }));
                    return Err(format!("install aborted: {} {}", plugin_id, perm));
                }
            },
            AskOutcome::Timeout => {
                if let Some(app) = super::app_handle() {
                    let _ = app.emit("opencapx-install-ask-done", json!({ "id": id, "answer": null }));
                }
                audit("denied", plugin_id, perm, json!({ "reason": "timeout", "caller": "install" }));
                return Err(format!("install timed out: {} {}", plugin_id, perm));
            }
            AskOutcome::NoUi => {
                audit("denied", plugin_id, perm, json!({ "reason": "no-ui", "caller": "install" }));
                return Err(format!("install aborted (no ui): {} {}", plugin_id, perm));
            }
        };
        decisions.push((perm.clone(), decision.to_string()));
    }
    Ok(decisions)
}

/// Non-interactive confirmation (no UI: tests / CI). §4.4 review 4: collect per "declared default / static-table default",
/// **does not write to the DB** — persistence is likewise left to the commit phase. audit is marked `install-no-ui` to distinguish it.
fn confirm_install_noninteractive(plugin_id: &str, plan: &[InstallAsk]) -> Result<Vec<(String, String)>, String> {
    let mut decisions: Vec<(String, String)> = Vec::with_capacity(plan.len());
    for ask in plan {
        let perm = &ask.permission;
        if !ask.declared && !known(perm) {
            audit("denied", plugin_id, perm, json!({ "reason": "unknown-permission", "caller": "install-no-ui" }));
            return Err(format!("unknown permission {}", perm));
        }
        let d = if ask.declared {
            parse_decision(&ask.declared_default)
        } else {
            default_decision(perm)
        };
        decisions.push((perm.clone(), decision_str(d).to_string()));
        audit(
            if d == Decision::Denied { "denied" } else { "granted" },
            plugin_id,
            perm,
            json!({
                "decision": if d == Decision::Ask { "ask-default" } else { "default" },
                "caller": "install-no-ui",
                "declared": ask.declared
            }),
        );
    }
    Ok(decisions)
}

/// Commit phase (§4.4 step 5): writes the confirmed decisions into `plugin_permissions` in a **single transaction**.
/// Called only after the user confirms everything; any failure rolls back the whole batch (no half-applied decisions).
/// Empty list = no-op.
pub fn commit_install_decisions(
    store: &SharedStore,
    plugin_id: &str,
    decisions: &[(String, String)],
) -> Result<(), String> {
    if decisions.is_empty() {
        return Ok(());
    }
    let now = super::agent::now_secs();
    let Ok(mut s) = store.lock() else {
        return Err("storage unavailable".into());
    };
    match s.try_with_conn(|c| {
        let tx = c
            .unchecked_transaction()
            .map_err(|e| format!("begin failed: {}", e))?;
        upsert_install_decisions_in_tx(&tx, plugin_id, decisions, now as i64)?;
        tx.commit().map_err(|e| format!("commit failed: {}", e))?;
        Ok(())
    }) {
        Some(r) => r,
        None => Err("sqlite unavailable".into()),
    }
}

/// Upserts a batch of install decisions within a transaction (`plugin_permissions`). The install commit phase and
/// `commit_install_decisions` share this single SQL source.
pub fn upsert_install_decisions_in_tx(
    tx: &rusqlite::Transaction,
    plugin_id: &str,
    decisions: &[(String, String)],
    now: i64,
) -> Result<(), String> {
    for (perm, decision) in decisions {
        tx.execute(
            "INSERT INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at)
             VALUES (?1, ?2, NULL, ?3, ?4)
             ON CONFLICT(plugin_id, permission) DO UPDATE SET decision = ?3, updated_at = ?4",
            rusqlite::params![plugin_id, perm, decision, now],
        )
        .map_err(|e| format!("write {} failed: {}", perm, e))?;
    }
    Ok(())
}

/// Execution gate: Granted passes; Ask goes to a prompt (see the runtime check flow in docs/permissions.md).
/// caller goes into the audit event: capability (mcp execute) or plugin.reverse (core.requestPermission).
/// reason is an optional note from plugin.reverse (e.g. "upload image to vision API"); it goes into the audit + prompt text.
pub fn gate(
    store: &SharedStore,
    plugin_id: &str,
    permission: &str,
    caller: &str,
    reason: Option<&str>,
) -> Decision {
    gate_with(default_asker(), store, plugin_id, permission, caller, reason)
}

/// F12 — injectable version of gate: identical behavior to [`gate`], only the "ask" channel is replaceable.
pub fn gate_with(
    asker: &dyn Asker,
    store: &SharedStore,
    plugin_id: &str,
    permission: &str,
    caller: &str,
    reason: Option<&str>,
) -> Decision {
    let d = check(store, plugin_id, permission);
    if d != Decision::Ask {
        return d;
    }
    // session tier: already granted in this process → pass directly (no prompt; audit only lands on the grant)
    if session_has(&format!("plugin:{}", plugin_id), permission) {
        return Decision::Granted;
    }
    if !asker.is_available() {
        // UI absent (test process/frontend not started): fast deny, no pending request left behind
        let mut extra = json!({ "reason": "no-ui", "caller": caller });
        if let Some(r) = reason {
            if let Some(o) = extra.as_object_mut() {
                o.insert("requestReason".to_string(), serde_json::Value::String(r.to_string()));
            }
        }
        audit("denied", plugin_id, permission, extra);
        return Decision::Denied;
    }
    let id = format!("perm-{}", nanos());
    let mut requested_extra = json!({ "scope": null, "caller": caller });
    if let Some(r) = reason {
        if let (Some(o), Some(e)) = (requested_extra.as_object_mut(), json!({ "requestReason": r }).as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    audit("requested", plugin_id, permission, requested_extra);
    // §4.3 enforcement points 1/2: declared-derived permissions are always once-only (Always-laundering H2)
    let can_always = can_always(store, permission);
    let mut payload = json!({
        "id": id,
        "pluginId": plugin_id,
        "permission": permission,
        "canAlways": can_always,
    });
    if let Some(r) = reason {
        if let Some(o) = payload.as_object_mut() {
            o.insert("reason".to_string(), serde_json::Value::String(r.to_string()));
        }
    }
    match asker.ask(&AskRequest {
        id: id.clone(),
        event: "opencapx-permission-ask",
        payload,
        timeout: ASK_TIMEOUT,
    }) {
        AskOutcome::Answered(a) => match a.as_str() {
            "once" => {
                audit("granted", plugin_id, permission, json!({ "decision": "once" }));
                Decision::Granted
            }
            "always" => {
                if can_always {
                    set_decision(store, plugin_id, permission, "granted");
                    audit("granted", plugin_id, permission, json!({ "decision": "always" }));
                } else {
                    // high-risk is not persisted; grant for this call only
                    audit("granted", plugin_id, permission, json!({ "decision": "once" }));
                }
                Decision::Granted
            }
            "session" => {
                // v1.5 third tier: in-process memory grant, cleared on restart. Declared-derived (once-only) does not accept it,
                // downgraded to once; high-risk allows it (pressure valve, not persisted).
                if super::declaration::is_declared_permission(store, permission) {
                    audit("granted", plugin_id, permission,
                        json!({ "decision": "once", "downgradedFrom": "session" }));
                } else {
                    session_grant(&format!("plugin:{}", plugin_id), permission);
                    audit("granted", plugin_id, permission, json!({ "decision": "session" }));
                }
                Decision::Granted
            }
            _ => {
                audit("denied", plugin_id, permission, json!({ "reason": "user", "caller": caller }));
                Decision::Denied
            }
        },
        AskOutcome::Timeout => {
            // timeout: notify the frontend to dismiss the bubble
            if let Some(app) = super::app_handle() {
                let _ = app.emit("opencapx-permission-ask-done", json!({ "id": id, "answer": null }));
            }
            audit("denied", plugin_id, permission, json!({ "reason": "timeout", "caller": caller }));
            Decision::Denied
        }
        AskOutcome::NoUi => {
            audit("denied", plugin_id, permission, json!({ "reason": "no-ui", "caller": caller }));
            Decision::Denied
        }
    }
}

/// Agent-layer audit (docs/permissions.md "Audit": permission.* carries an agentId subject).
fn audit_agent(kind: &str, agent_id: &str, permission: &str, extra: serde_json::Value) {
    let mut payload = json!({ "agentId": agent_id, "permission": permission });
    if let (Some(o), Some(e)) = (payload.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
        &format!("permission.{}", kind),
        "core",
        payload,
    ));
}

/// Agent-layer gate (the Agent half of "two-layer judgment" in docs/permissions.md).
/// Isomorphic to gate(), with AgentIdentity as the subject; the prompt event name is separate (opencapx-agent-permission-ask),
/// and frontend answers likewise go through resolve_ask. Core native tools (say/notify/set_state/ask) pass only this layer.
/// v1 note: the two layers are serial gates (this function → plugin-layer gate), so the first ask may show two prompts in sequence;
/// merging into a single prompt is v2 UX; see the deviation note under "two-layer judgment" in docs/permissions.md.
pub fn gate_agent(store: &SharedStore, agent_id: &str, permission: &str, caller: &str) -> Decision {
    gate_agent_with(default_asker(), store, agent_id, permission, caller)
}

/// F12 — injectable version of gate_agent (same behavior as [`gate_agent`]).
pub fn gate_agent_with(
    asker: &dyn Asker,
    store: &SharedStore,
    agent_id: &str,
    permission: &str,
    caller: &str,
) -> Decision {
    let d = super::identity::check_agent(store, agent_id, permission);
    if d != Decision::Ask {
        return d;
    }
    // session tier (same semantics as the plugin layer; subject agent:<id>)
    if session_has(&format!("agent:{}", agent_id), permission) {
        return Decision::Granted;
    }
    if !asker.is_available() {
        // UI absent (test process/frontend not started): fast deny, no pending request left behind. Same behavior as gate().
        audit_agent("denied", agent_id, permission, json!({ "reason": "no-ui", "caller": caller }));
        return Decision::Denied;
    }
    let id = format!("agentperm-{}", nanos());
    audit_agent("requested", agent_id, permission, json!({ "caller": caller }));
    // §4.3 enforcement points 1/2: declared-derived permissions are always once-only (Always-laundering H2)
    let can_always = can_always(store, permission);
    let payload = json!({
        "id": id,
        "agentId": agent_id,
        "displayName": super::identity::display_name_for_agent(store, agent_id),
        "permission": permission,
        "canAlways": can_always,
    });
    match asker.ask(&AskRequest {
        id: id.clone(),
        event: "opencapx-agent-permission-ask",
        payload,
        timeout: ASK_TIMEOUT,
    }) {
        AskOutcome::Answered(a) => match a.as_str() {
            "once" => {
                audit_agent("granted", agent_id, permission, json!({ "decision": "once", "caller": caller }));
                Decision::Granted
            }
            "session" => {
                // v1.5 third tier (same semantics as the plugin layer): declared-derived downgrades to once, otherwise grant in-process memory
                if super::declaration::is_declared_permission(store, permission) {
                    audit_agent("granted", agent_id, permission,
                        json!({ "decision": "once", "caller": caller, "downgradedFrom": "session" }));
                } else {
                    session_grant(&format!("agent:{}", agent_id), permission);
                    audit_agent("granted", agent_id, permission,
                        json!({ "decision": "session", "caller": caller }));
                }
                Decision::Granted
            }
            "always" => {
                if can_always {
                    super::identity::set_agent_decision(store, agent_id, permission, "granted");
                    audit_agent("granted", agent_id, permission, json!({ "decision": "always", "caller": caller }));
                } else {
                    // high-risk is not persisted; grant for this call only
                    audit_agent("granted", agent_id, permission, json!({ "decision": "once", "caller": caller }));
                }
                Decision::Granted
            }
            _ => {
                audit_agent("denied", agent_id, permission, json!({ "reason": "user", "caller": caller }));
                Decision::Denied
            }
        },
        AskOutcome::Timeout => {
            if let Some(app) = super::app_handle() {
                let _ = app.emit("opencapx-agent-permission-ask-done", json!({ "id": id, "answer": null }));
            }
            audit_agent("denied", agent_id, permission, json!({ "reason": "timeout", "caller": caller }));
            Decision::Denied
        }
        AskOutcome::NoUi => {
            audit_agent("denied", agent_id, permission, json!({ "reason": "no-ui", "caller": caller }));
            Decision::Denied
        }
    }
}

/// Phase 33 — permission risk heatmap: one decision cell per (plugin, permission) +
/// a global permission-denial ranking (for the UI's "top-N most-denied permissions across your installed plugins").
#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionHeatmapCellDto {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub permission: String,
    pub decision: String,
    #[serde(rename = "highRisk")]
    pub high_risk: bool,
    /// §4.7: declared-derived (unverified domain)
    pub declared: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TopPermissionDto {
    pub permission: String,
    #[serde(rename = "grantedCount")]
    pub granted_count: i64,
    #[serde(rename = "deniedCount")]
    pub denied_count: i64,
    #[serde(rename = "askCount")]
    pub ask_count: i64,
    #[serde(rename = "highRisk")]
    pub high_risk: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionHeatmapDto {
    pub cells: Vec<PermissionHeatmapCellDto>,
    #[serde(rename = "topDenied")]
    pub top_denied: Vec<TopPermissionDto>,
}

/// Reads the plugin_permissions table from shared_store and returns heatmap data.
/// Deadlock fix: the is_declared family re-locks the store internally — never call it inside a with_conn_ref closure
/// (previously calling is_declared_permission per cell inside the closure → same-thread reentrant deadlock, freezing the plugin tab instantly).
/// Now: hold the lock to read raw rows → release it → fetch the declaration set with one DISTINCT → merge in memory (which also kills the N+1).
pub fn heatmap() -> PermissionHeatmapDto {
    use super::shared_store;
    let Some(store) = shared_store() else {
        return PermissionHeatmapDto { cells: Vec::new(), top_denied: Vec::new() };
    };
    // (1) hold the lock only to read raw rows; do nothing inside the lock that would re-acquire it
    let raw: Vec<(String, String, String)> = {
        let Ok(s) = store.lock() else {
            return PermissionHeatmapDto { cells: Vec::new(), top_denied: Vec::new() };
        };
        let mut out: Vec<(String, String, String)> = Vec::new();
        let _ = s.with_conn_ref(|c| {
            let Ok(mut stmt) = c.prepare(
                "SELECT plugin_id, permission, decision FROM plugin_permissions ORDER BY plugin_id ASC, permission ASC",
            ) else {
                return 0usize;
            };
            let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            }) else {
                return 0usize;
            };
            out = rows.flatten().collect();
            0usize
        });
        out
        // the guard is released here
    };
    // (2) after releasing, query the declaration set once (re-locking is now safe)
    let declared = super::declaration::declared_permission_set(&store);
    let cells: Vec<PermissionHeatmapCellDto> = raw
        .into_iter()
        .map(|(plugin_id, permission, decision)| PermissionHeatmapCellDto {
            high_risk: HIGH_RISK.contains(&permission.as_str()),
            declared: declared.contains(&permission),
            plugin_id,
            permission,
            decision,
        })
        .collect();
    // aggregate by permission (granted / denied / ask counts)
    use std::collections::BTreeMap;
    let mut agg: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    for cell in &cells {
        let e = agg.entry(cell.permission.clone()).or_insert((0, 0, 0));
        match cell.decision.as_str() {
            "granted" => e.0 += 1,
            "denied" => e.1 += 1,
            "ask" => e.2 += 1,
            _ => {}
        }
    }
    let mut top_denied: Vec<TopPermissionDto> = agg
        .into_iter()
        .map(|(permission, (granted, denied, ask))| TopPermissionDto {
            high_risk: HIGH_RISK.contains(&permission.as_str()),
            permission,
            granted_count: granted,
            denied_count: denied,
            ask_count: ask,
        })
        .collect();
    // denied descending, then granted descending, then permission ascending (stable)
    top_denied.sort_by(|a, b| {
        b.denied_count
            .cmp(&a.denied_count)
            .then(b.granted_count.cmp(&a.granted_count))
            .then(a.permission.cmp(&b.permission))
    });
    // cap at top 10
    top_denied.truncate(10);
    PermissionHeatmapDto { cells, top_denied }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// session_tier's three tests share the global overlay (session_grants) and must be serialized.
    static SESSION_LOCK: Mutex<()> = Mutex::new(());

    fn gate_store(tag: &str) -> SharedStore {
        let dir = std::env::temp_dir().join(format!("opencapx-session-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(Mutex::new(super::super::storage::StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )))
    }

    use crate::core::storage::StoreEnum;
    use crate::core::agent::SessionStore;
    use std::sync::{Arc, Mutex};

    fn mem_store() -> SharedStore {
        Arc::new(Mutex::new(StoreEnum::Mem(SessionStore::new())))
    }

    /// F6 — pins the timeout semantics of waiting for an answer: no answer → Timeout; answer → Answered;
    /// production ceiling constant = 60s (a short timeout only verifies the mechanism, it does not wait out the production value).
    #[test]
    fn await_answer_pins_timeout_semantics() {
        let (_tx, rx) = mpsc::channel::<String>();
        assert_eq!(await_answer(&rx, Duration::from_millis(30)), AskOutcome::Timeout);
        let (tx, rx2) = mpsc::channel::<String>();
        tx.send("once".to_string()).unwrap();
        assert_eq!(
            await_answer(&rx2, Duration::from_millis(300)),
            AskOutcome::Answered("once".to_string())
        );
        assert_eq!(ASK_TIMEOUT, Duration::from_secs(60));
    }

    /// §4.2 lexicon rejection table: case / homoglyphs / trailing dot / double dot / single segment / over 64 / digit-leading segment.
    #[test]
    fn name_lexicon_accepts_and_rejects() {
        for ok in [
            "weather.fetch", "things.add", "image.analyze", "a.b", "x2.y-z_w",
        ] {
            assert!(valid_name(ok), "{ok:?} should be valid");
        }
        for bad in [
            "", "weather",            // single segment
            ".weather.fetch", "weather.fetch.", "weather..fetch", // leading/trailing/double dot
            "Weather.fetch", "weather.Fetch", // case
            "2eather.fetch",          // digit-leading segment
            "weather.fetch ",         // whitespace
            "ｗeather.fetch",          // full-width homoglyph (non-ASCII on the NFKC surface)
            "wéather.fetch",          // diacritic
            "weather.fetch\n",
            &format!("{}.b", "a".repeat(63)), // over 64
        ] {
            assert!(!valid_name(bad), "{bad:?} should be invalid");
        }
    }

    /// §4.2 reserved sets: all built-in first segments are reserved; opencapx is reserved; new domains are not.
    #[test]
    fn reserved_sets_cover_builtin_domains() {
        for c in super::super::capability::CAPABILITY_IDS {
            assert!(reserved_capability(c), "{c} should be reserved capability");
            assert!(reserved_domain(first_segment(c)), "domain of {c} should be reserved");
        }
        for (p, _) in PERMISSIONS {
            assert!(reserved_domain(first_segment(p)), "domain of {p} should be reserved");
        }
        assert!(reserved_domain("opencapx"));
        assert!(!reserved_capability("weather.fetch"));
        assert!(!reserved_domain("weather"));
        assert!(!reserved_domain("com"));
    }

    #[test]
    fn defaults_match_docs_table() {
        assert_eq!(default_decision("pet.animation"), Decision::Granted);
        assert_eq!(default_decision("image.read"), Decision::Ask);
        assert_eq!(default_decision("process.execute"), Decision::Denied);
        assert_eq!(default_decision("clipboard.write"), Decision::Ask);
        // v1.3 batch spot-check
        assert_eq!(default_decision("automation.control"), Decision::Ask);
        assert_eq!(default_decision("input.control"), Decision::Denied);
        assert_eq!(default_decision("photos.read"), Decision::Ask);
        assert_eq!(default_decision("location.read"), Decision::Ask);
        assert_eq!(default_decision("audio.output"), Decision::Ask);
        assert_eq!(default_decision("url.scheme.open"), Decision::Ask);
        // v1.4 batch spot-check
        assert_eq!(default_decision("media.control"), Decision::Ask);
        assert_eq!(default_decision("messages.read"), Decision::Denied);
        assert_eq!(default_decision("window.management"), Decision::Denied);
        assert_eq!(default_decision("power.control"), Decision::Denied);
        assert_eq!(default_decision("notes.read"), Decision::Ask);
        assert_eq!(default_decision("reminders.write"), Decision::Ask);
        assert_eq!(default_decision("mail.read"), Decision::Ask);
        assert_eq!(default_decision("system.settings"), Decision::Ask);
        assert_eq!(default_decision("printer.control"), Decision::Ask);
        // v1.5 Things data surface: reads and writes both ask
        assert_eq!(default_decision("things.read"), Decision::Ask);
        assert_eq!(default_decision("things.write"), Decision::Ask);
        assert!(known("plugin.install"));
        assert!(!known("not.a.permission"));
    }

    #[test]
    fn capability_mapping() {
        assert_eq!(capability_permission("image.analyze"), Some("image.read"));
        assert_eq!(capability_permission("browser.read"), Some("browser.control"));
        assert_eq!(capability_permission("clipboard.write"), Some("clipboard.write"));
        assert_eq!(capability_permission("file.write"), Some("filesystem.write"));
        assert_eq!(capability_permission("file.search"), Some("file.read"));
        // v1.2: speech = human-facing channel, tightened to notification.post (ask)
        assert_eq!(capability_permission("speech.synthesize"), Some("notification.post"));
        // A7: the reply includes clipboard fragments, take the strictest component
        assert_eq!(capability_permission("context.get_current"), Some("clipboard.read"));
        // v1.2: OS permission probing is read-only, no gate needed
        assert_eq!(capability_permission("system.permission_status"), None);
        // v1.3: high/medium-value batch
        assert_eq!(capability_permission("automation.run"), Some("automation.control"));
        assert_eq!(capability_permission("input.send"), Some("input.control"));
        assert_eq!(capability_permission("photos.read"), Some("photos.read"));
        assert_eq!(capability_permission("contacts.search"), Some("contacts.read"));
        assert_eq!(capability_permission("calendar.events"), Some("calendar.read"));
        assert_eq!(capability_permission("location.get"), Some("location.read"));
        assert_eq!(capability_permission("audio.play"), Some("audio.output"));
        assert_eq!(capability_permission("url.scheme.open"), Some("url.scheme.open"));
        // v1.3: subscription screenshot diff is the same tier as screen.capture
        assert_eq!(capability_permission("screen.watch"), Some("screen.capture"));
        // v1.4: high/medium-value batch
        assert_eq!(capability_permission("media.playback"), Some("media.control"));
        assert_eq!(capability_permission("messages.recent"), Some("messages.read"));
        assert_eq!(capability_permission("window.list"), Some("window.management"));
        assert_eq!(capability_permission("window.focus"), Some("window.management"));
        assert_eq!(capability_permission("system.sleep"), Some("power.control"));
        assert_eq!(capability_permission("system.lock"), Some("power.control"));
        assert_eq!(capability_permission("notes.read"), Some("notes.read"));
        assert_eq!(capability_permission("reminders.read"), Some("reminders.read"));
        assert_eq!(capability_permission("reminders.write"), Some("reminders.write"));
        assert_eq!(capability_permission("mail.recent"), Some("mail.read"));
        assert_eq!(capability_permission("system.settings"), Some("system.settings"));
        assert_eq!(capability_permission("printer.print"), Some("printer.control"));
        // v1.5 Things data surface: the three reads → things.read, the three writes → things.write
        assert_eq!(capability_permission("things.list"), Some("things.read"));
        assert_eq!(capability_permission("things.show"), Some("things.read"));
        assert_eq!(capability_permission("things.search"), Some("things.read"));
        assert_eq!(capability_permission("things.add"), Some("things.write"));
        assert_eq!(capability_permission("things.update"), Some("things.write"));
        assert_eq!(capability_permission("things.delete"), Some("things.write"));
        // plugin.install is a vocabulary placeholder: no capability maps to it
        for cap in super::super::capability::CAPABILITY_IDS {
            assert_ne!(capability_permission(cap), Some("plugin.install"), "{} maps to plugin.install", cap);
        }
        assert_eq!(capability_permission("nope.nope"), None);
    }

    #[test]
    fn mem_store_falls_back_to_defaults() {
        let s = mem_store();
        assert_eq!(check(&s, "p", "image.read"), Decision::Ask);
        assert!(!set_decision(&s, "p", "image.read", "granted")); // mem has no DB
        assert_eq!(gate(&s, "p", "image.read", "capability", None), Decision::Denied); // ask + no UI → fast deny
        assert_eq!(gate(&s, "p", "pet.animation", "capability", None), Decision::Granted);
    }

    #[test]

    // ===== v1.5 session tier =====

    #[test]
    fn session_tier_plugin_gate_lasts_for_process_and_db_beats_it() {
        let _guard = SESSION_LOCK.lock().unwrap();
        session_revoke_all();
        let s = gate_store("plugin");
        // first ask: answer session → Granted, not persisted
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("session".into())]);
        assert_eq!(gate_with(&asker, &s, "sp1", "image.read", "capability", None), Decision::Granted);
        assert_eq!(check(&s, "sp1", "image.read"), Decision::Ask);
        // second ask: passes even with UI absent (overlay hit, no prompt again)
        let no_ui = ScriptAsker::new(false, vec![]);
        assert_eq!(gate_with(&no_ui, &s, "sp1", "image.read", "capability", None), Decision::Granted);
        // explicit denied persisted → DB wins, overlay is void
        assert!(set_decision(&s, "sp1", "image.read", "denied"));
        assert_eq!(gate_with(&no_ui, &s, "sp1", "image.read", "capability", None), Decision::Denied);
        // revoke all sessions: back to the ask path (no UI → fast deny, proving the overlay is cleared)
        assert!(set_decision(&s, "sp1", "image.read", "ask"));
        session_revoke_all();
        assert_eq!(gate_with(&no_ui, &s, "sp1", "image.read", "capability", None), Decision::Denied);
        assert_eq!(check(&s, "sp1", "image.read"), Decision::Ask);
        session_revoke_all();
    }

    #[test]
    fn session_tier_agent_gate_mirrors_plugin_gate() {
        let _guard = SESSION_LOCK.lock().unwrap();
        session_revoke_all();
        let s = gate_store("agent");
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("session".into())]);
        assert_eq!(gate_agent_with(&asker, &s, "ag_s1", "file.read", "mcp"), Decision::Granted);
        let no_ui = ScriptAsker::new(false, vec![]);
        assert_eq!(gate_agent_with(&no_ui, &s, "ag_s1", "file.read", "mcp"), Decision::Granted);
        // subject isolation: another agent does not receive this session grant
        assert_eq!(gate_agent_with(&no_ui, &s, "ag_s2", "file.read", "mcp"), Decision::Denied);
        session_revoke_all();
    }

    #[test]
    fn session_tier_declared_permission_downgrades_to_once() {
        let _guard = SESSION_LOCK.lock().unwrap();
        session_revoke_all();
        let s = gate_store("declared");
        // seed a declaration-table row → weather.demo domain-style third-party permission (once-only semantics)
        {
            let mut g = s.lock().unwrap();
            let n = g.with_conn(|c| c.execute(
                "INSERT INTO capability_declarations (capability, plugin_id, permission, default_decision, confirmed_at) VALUES ('x.y','dp1','third.weather','ask',0)",
                [],
            ).unwrap_or(0));
            assert_eq!(n, Some(1));
        }
        assert!(crate::core::declaration::is_declared_permission(&s, "third.weather"));
        // answer session → superficially Granted, but treated as once: not entered into the overlay
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("session".into())]);
        assert_eq!(gate_with(&asker, &s, "dp1", "third.weather", "capability", None), Decision::Granted);
        let no_ui = ScriptAsker::new(false, vec![]);
        assert_eq!(gate_with(&no_ui, &s, "dp1", "third.weather", "capability", None), Decision::Denied);
        session_revoke_all();
    }

    fn ask_registry_and_high_risk_set() {
        assert!(!resolve_ask("nope", "once"));
        assert!(asks().lock().unwrap().is_empty());
        assert!(HIGH_RISK.contains(&"process.execute"));
        assert!(!HIGH_RISK.contains(&"image.read"));
        // a built-in write goes through filesystem.write: high-risk, denied by default, only allow-once at runtime
        assert!(HIGH_RISK.contains(&capability_permission("file.write").unwrap()));
        // clipboard.write is not high-risk: paste surface, ask by default
        assert!(!HIGH_RISK.contains(&capability_permission("clipboard.write").unwrap()));
        // v1.3: driving apps / synthesizing input / installing plugins are high-risk; personal-data reads are not
        assert!(HIGH_RISK.contains(&"automation.control"));
        assert!(HIGH_RISK.contains(&"input.control"));
        assert!(HIGH_RISK.contains(&"plugin.install"));
        assert!(!HIGH_RISK.contains(&"photos.read"));
        assert!(!HIGH_RISK.contains(&"audio.output"));
        // v1.4: chat history / window manipulation / power / mail are high-risk; media, notes, reminders,
        // settings, printing are not (impact stays within this process or OS semantics, ask suffices)
        assert!(HIGH_RISK.contains(&"messages.read"));
        assert!(HIGH_RISK.contains(&"window.management"));
        assert!(HIGH_RISK.contains(&"power.control"));
        assert!(HIGH_RISK.contains(&"mail.read"));
        assert!(!HIGH_RISK.contains(&"media.control"));
        assert!(!HIGH_RISK.contains(&"reminders.write"));
        assert!(!HIGH_RISK.contains(&"system.settings"));
        assert!(!HIGH_RISK.contains(&"printer.control"));
        // v1.5: Things reads and writes are both non-high-risk (ask + Always allowed)
        assert!(!HIGH_RISK.contains(&"things.read"));
        assert!(!HIGH_RISK.contains(&"things.write"));
    }

    #[test]
    fn resolve_ask_delivers_answer_once() {
        let (tx, rx) = mpsc::channel();
        asks().lock().unwrap().insert("perm-t".into(), tx);
        assert!(resolve_ask("perm-t", "always"));
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "always");
        assert!(!resolve_ask("perm-t", "always")); // already consumed
    }

    /// F12 — scripted stub: returns preset results in order; available is fixed (for fail-closed with no UI).
    struct ScriptAsker {
        available: bool,
        answers: Mutex<std::collections::VecDeque<AskOutcome>>,
    }

    impl ScriptAsker {
        fn new(available: bool, answers: Vec<AskOutcome>) -> Self {
            Self {
                available,
                answers: Mutex::new(answers.into()),
            }
        }
    }

    impl Asker for ScriptAsker {
        fn is_available(&self) -> bool {
            self.available
        }
        fn ask(&self, _req: &AskRequest) -> AskOutcome {
            self.answers
                .lock()
                .ok()
                .and_then(|mut q| q.pop_front())
                .unwrap_or(AskOutcome::Timeout)
        }
    }

    /// F12 four branches (gate): allow once / Always persisted / Always downgraded (high-risk) / deny / timeout +
    /// fail-closed with no UI retained.
    #[test]
    fn gate_asker_four_branches() {
        let dir = std::env::temp_dir().join(format!("opencapx-gate-asker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        // allow once → Granted, not persisted
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("once".into())]);
        assert_eq!(
            gate_with(&asker, &s, "p1", "image.read", "capability", None),
            Decision::Granted
        );
        assert_eq!(check(&s, "p1", "image.read"), Decision::Ask);
        // Always (Always allowed) → Granted and persisted as granted
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("always".into())]);
        assert_eq!(
            gate_with(&asker, &s, "p2", "image.read", "capability", None),
            Decision::Granted
        );
        assert_eq!(check(&s, "p2", "image.read"), Decision::Granted);
        // Always (high-risk, Always not allowed) → downgrade to once: Granted but not persisted
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("always".into())]);
        assert_eq!(
            gate_with(&asker, &s, "p3", "automation.control", "capability", None),
            Decision::Granted
        );
        assert_eq!(check(&s, "p3", "automation.control"), Decision::Ask);
        // deny → Denied
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("deny".into())]);
        assert_eq!(
            gate_with(&asker, &s, "p4", "image.read", "capability", None),
            Decision::Denied
        );
        // timeout → Denied
        let asker = ScriptAsker::new(true, vec![AskOutcome::Timeout]);
        assert_eq!(
            gate_with(&asker, &s, "p5", "image.read", "capability", None),
            Decision::Denied
        );
        // no UI → fail-closed (behavior unchanged)
        let asker = ScriptAsker::new(false, vec![]);
        assert_eq!(
            gate_with(&asker, &s, "p6", "image.read", "capability", None),
            Decision::Denied
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F12 four branches (confirm_install): Always / once / declared downgrade / deny / timeout +
    /// no UI goes through the non-interactive default table (behavior unchanged).
    #[test]
    fn confirm_install_asker_four_branches() {
        let plan = vec![ask_builtin("image.read")];
        // Always (allowed) → granted
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("always".into())]);
        let d = confirm_install_with(&asker, "com.x", &plan).unwrap();
        assert_eq!(d, vec![("image.read".to_string(), "granted".to_string())]);
        // once → ask
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("once".into())]);
        let d = confirm_install_with(&asker, "com.x", &plan).unwrap();
        assert_eq!(d, vec![("image.read".to_string(), "ask".to_string())]);
        // declared-derived + Always → downgrade to ask (do not reject the whole batch)
        let plan_decl = vec![ask_declared("x.read", "ask")];
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("always".into())]);
        let d = confirm_install_with(&asker, "com.x", &plan_decl).unwrap();
        assert_eq!(d, vec![("x.read".to_string(), "ask".to_string())]);
        // deny → Err (whole-batch failure)
        let asker = ScriptAsker::new(true, vec![AskOutcome::Answered("deny".into())]);
        let err = confirm_install_with(&asker, "com.x", &plan).unwrap_err();
        assert!(err.contains("install denied"), "got: {}", err);
        // timeout → Err
        let asker = ScriptAsker::new(true, vec![AskOutcome::Timeout]);
        let err = confirm_install_with(&asker, "com.x", &plan).unwrap_err();
        assert!(err.contains("timed out"), "got: {}", err);
        // no UI → non-interactive default table (image.read → ask)
        let asker = ScriptAsker::new(false, vec![]);
        let d = confirm_install_with(&asker, "com.x", &plan).unwrap();
        assert_eq!(d, vec![("image.read".to_string(), "ask".to_string())]);
    }

    /// Test helper: confirmation item for a built-in permission.
    fn ask_builtin(p: &str) -> InstallAsk {
        InstallAsk { permission: p.to_string(), declared: false, declared_default: "ask".into() }
    }

    /// Test helper: confirmation item for a declared-derived permission (the default written in the manifest).
    fn ask_declared(p: &str, default: &str) -> InstallAsk {
        InstallAsk { permission: p.to_string(), declared: true, declared_default: default.into() }
    }

    #[test]
    fn confirm_install_noninteractive_persists_defaults() {
        let dir = std::env::temp_dir().join(format!("opencapx-confirminst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        // pet.animation default granted → granted; image.read default ask → ask
        let d = confirm_install_noninteractive(
            "com.x",
            &[ask_builtin("pet.animation"), ask_builtin("image.read")],
        )
        .expect("noninteractive confirm");
        assert_eq!(d, vec![
            ("pet.animation".to_string(), "granted".to_string()),
            ("image.read".to_string(), "ask".to_string()),
        ]);
        // §4.4 consent-before-commit: the confirmation phase **does not write to the DB**
        assert_eq!(check(&s, "com.x", "pet.animation"), Decision::Granted); // still the default-table answer
        let rows_before = permission_row_count(&s, "com.x");
        assert_eq!(rows_before, 0, "confirm must not touch the DB");
        // only the commit phase persists
        commit_install_decisions(&s, "com.x", &d).expect("commit");
        assert_eq!(permission_row_count(&s, "com.x"), 2);
        assert_eq!(check(&s, "com.x", "image.read"), Decision::Ask);
        // unknown names are rejected outright (and not written)
        let err = confirm_install_noninteractive("com.x", &[ask_builtin("nope.perm")]).unwrap_err();
        assert!(err.contains("unknown permission"));
        assert_eq!(permission_row_count(&s, "com.x"), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §4.3 integration point #3: a declared-derived permission's default comes from the **manifest inline declaration**, not the static table.
    #[test]
    fn declared_permission_uses_manifest_default() {
        let d = confirm_install_noninteractive(
            "com.weather",
            &[ask_declared("weather.read", "ask"), ask_declared("weather.admin", "denied")],
        )
        .expect("noninteractive confirm");
        assert_eq!(d, vec![
            ("weather.read".to_string(), "ask".to_string()),
            ("weather.admin".to_string(), "denied".to_string()),
        ]);
        // declared items are never persisted as granted
        assert!(d.iter().all(|(_, dec)| dec != "granted"));
    }

    /// §4.3 enforcement points 1/2 criterion: neither high-risk nor declared-derived may be Always; ordinary built-in permissions may.
    #[test]
    fn once_only_judgement_covers_declared_and_high_risk() {
        let dir = std::env::temp_dir().join(format!("opencapx-onceonly-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        // built-in ordinary → Always allowed; built-in high-risk → not allowed
        assert!(can_always(&s, "image.read"));
        assert!(!can_always(&s, "process.execute"));
        assert!(!can_always(&s, "filesystem.write"));
        // declared-derived → not allowed (even when absent from HIGH_RISK)
        declare_for_test(&s, "com.weather", "weather.fetch", "weather.read", "ask");
        assert!(!can_always(&s, "weather.read"), "declared permission must be once-only");
        // §4.3 enforcement point 3: neither settings page nor install writeback may write it as granted
        assert!(!set_decision(&s, "com.weather", "weather.read", "granted"));
        assert!(set_decision(&s, "com.weather", "weather.read", "denied"));
        assert_eq!(check(&s, "com.weather", "weather.read"), Decision::Denied);
        // integration point #2: default comes from the declaration when no DB override is persisted
        assert_eq!(default_decision_for(&s, "weather.read"), Decision::Ask);
        assert_eq!(default_decision_for(&s, "image.read"), Decision::Ask);
        assert_eq!(default_decision_for(&s, "nope.nope"), Decision::Denied);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Test helper: writes a frozen declaration directly (bypassing the install flow).
    fn declare_for_test(s: &SharedStore, plugin: &str, cap: &str, perm: &str, default: &str) {
        let rows = vec![(cap.to_string(), perm.to_string(), default.to_string(), None)];
        let mut st = s.lock().unwrap();
        st.try_with_conn(|c| {
            let tx = c.unchecked_transaction().unwrap();
            super::super::declaration::write_in_tx(&tx, plugin, &rows, 1)?;
            tx.commit().unwrap();
            Ok(())
        })
        .unwrap()
        .unwrap();
    }

    /// §4.4 step 6 regression: an error in the confirmation phase → zero changes in the DB (consent-before-commit).
    /// The interactive path's deny/timeout branches are isomorphic to this (likewise Err only, no writes).
    #[test]
    fn consent_before_commit_leaves_store_untouched_on_deny() {
        let dir = std::env::temp_dir().join(format!("opencapx-consent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        assert!(confirm_install_noninteractive("com.x", &[ask_builtin("nope.perm")]).is_err());
        assert_eq!(permission_row_count(&s, "com.x"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Commit-phase boundaries: empty list is a no-op; repeated commits overwrite rather than insert.
    /// (whole-batch rollback is guaranteed by `unchecked_transaction`'s drop-rollback semantics; this does not fabricate
    /// a failure case — the permission table only has PK / NOT NULL constraints, so a mid-way failure cannot be constructed.)
    #[test]
    fn commit_install_decisions_is_idempotent_and_noop_on_empty() {
        let dir = std::env::temp_dir().join(format!("opencapx-commit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        let ok = vec![("image.read".to_string(), "ask".to_string())];
        commit_install_decisions(&s, "com.x", &ok).unwrap();
        assert_eq!(permission_row_count(&s, "com.x"), 1);
        commit_install_decisions(&s, "com.x", &[]).unwrap();
        assert_eq!(permission_row_count(&s, "com.x"), 1);
        // repeated commit = overwrite, not insert
        commit_install_decisions(&s, "com.x", &ok).unwrap();
        assert_eq!(permission_row_count(&s, "com.x"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deduplicated in-tx helper: writes two decisions in an explicit transaction and reads them back after commit.
    #[test]
    fn upsert_install_decisions_in_tx_writes_and_overwrites() {
        use crate::core::storage::{StoreEnum, Storage};
        let dir = std::env::temp_dir().join(format!("opencapx-upsert-tx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = StoreEnum::Db(Storage::open(&dir.join("t.db")).unwrap());
        let decisions = vec![("image.read".to_string(), "ask".to_string())];
        s.try_with_conn(|c| {
            let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
            upsert_install_decisions_in_tx(&tx, "com.x", &decisions, 1)?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(())
        })
        .unwrap()
        .unwrap();
        // overwrite write: same-key update does not insert
        s.try_with_conn(|c| {
            let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
            upsert_install_decisions_in_tx(&tx, "com.x", &[("image.read".into(), "denied".into())], 2)?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(())
        })
        .unwrap()
        .unwrap();
        let count: i64 = s
            .try_with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM plugin_permissions WHERE plugin_id = 'com.x'", [], |r| r.get(0))
                    .map_err(|e| e.to_string())
            })
            .unwrap()
            .unwrap();
        assert_eq!(count, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Test helper: count the rows a plugin has in plugin_permissions.
    fn permission_row_count(s: &SharedStore, plugin_id: &str) -> usize {
        s.lock()
            .ok()
            .and_then(|st| {
                st.with_conn_ref(|c| {
                    c.query_row(
                        "SELECT COUNT(*) FROM plugin_permissions WHERE plugin_id = ?1",
                        params![plugin_id],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0) as usize
                })
            })
            .unwrap_or(0)
    }

    #[test]
    fn sqlite_decision_override() {
        let dir = std::env::temp_dir().join(format!("opencapx-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        assert_eq!(check(&s, "com.x", "image.read"), Decision::Ask);
        assert!(set_decision(&s, "com.x", "image.read", "granted"));
        assert_eq!(check(&s, "com.x", "image.read"), Decision::Granted);
        assert_eq!(gate(&s, "com.x", "image.read", "capability", None), Decision::Granted);
        assert!(set_decision(&s, "com.x", "image.read", "denied"));
        assert_eq!(gate(&s, "com.x", "image.read", "capability", None), Decision::Denied);
        assert!(!set_decision(&s, "com.x", "nope.nope", "granted"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn view_groups_overrides_defaults_and_high_risk() {
        let dir = std::env::temp_dir().join(format!("opencapx-permview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        let plugins = vec![crate::core::plugin::PluginStatusDto {
            id: "com.x".into(),
            name: "X".into(),
            description: None,
            author: None,
            homepage: None,
            license: None,
            version: "0.1.0".into(),
            ptype: "capability".into(),
            status: "running".into(),
            capabilities: vec![],
            permissions: vec!["image.read".into(), "process.execute".into()],
            path: None,
            auto_reload: false,
            probe_status: None,
            probe_at: None,
            channel: None,
            sandbox_declared: false,
            health_heartbeat_sec: None,
            health_max_retries: None,
            health_enabled: None,
            missing_dependencies: vec![],
            revoked_key: None,
            revoked_at: None,
        }];
        let v = view(&s, &plugins);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].plugin_id, "com.x");
        assert_eq!(v[0].permissions[0].decision, "ask");
        assert_eq!(v[0].permissions[0].default, "ask");
        assert!(!v[0].permissions[0].high_risk);
        assert!(v[0].permissions[1].high_risk);
        assert!(set_decision(&s, "com.x", "image.read", "granted"));
        let v2 = view(&s, &plugins);
        assert_eq!(v2[0].permissions[0].decision, "granted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// agent_view: full vocabulary + agent override > default table + high_risk flag.
    #[test]
    fn agent_view_lists_vocabulary_with_overrides() {
        let dir = std::env::temp_dir().join(format!("opencapx-agentview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s: SharedStore = Arc::new(Mutex::new(StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        let (agent_id, _) = super::super::identity::register(&s, "claude", "mcp").expect("register");
        let v = agent_view(&s, &agent_id);
        assert_eq!(v.len(), PERMISSIONS.len(), "full vocabulary");
        let pet = v.iter().find(|e| e.permission == "pet.animation").unwrap();
        assert_eq!(pet.decision, "granted");
        assert_eq!(pet.default, "granted");
        assert!(!pet.high_risk);
        let cam = v.iter().find(|e| e.permission == "camera").unwrap();
        assert_eq!(cam.decision, "denied", "default table denied");
        assert!(cam.high_risk);
        // after override the decision changes but default does not
        assert!(super::super::identity::set_agent_decision(&s, &agent_id, "camera", "ask"));
        let v2 = agent_view(&s, &agent_id);
        let cam2 = v2.iter().find(|e| e.permission == "camera").unwrap();
        assert_eq!(cam2.decision, "ask");
        assert_eq!(cam2.default, "denied", "default keeps the table value");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// gate()'s Ask + no-UI path emits a permission.denied audit + carries the reason into the payload.
    /// Verifies that plugin.reverse's reason field reaches the audit.
    #[test]
    fn gate_no_ui_emits_audit_with_reason() {
        let rx = super::super::event::EventBus::shared().subscribe();
        let s = mem_store();
        let d = gate(&s, "com.x", "image.read", "plugin.reverse", Some("analyze screenshot"));
        assert_eq!(d, Decision::Denied);
        // the shared bus sees other events under parallel tests (including sibling tests also using com.x); filter by pluginId + caller.
        let ev = loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(e)
                    if e.kind == "permission.denied"
                        && e.payload.get("pluginId").and_then(|v| v.as_str()) == Some("com.x")
                        && e.payload.get("caller").and_then(|v| v.as_str()) == Some("plugin.reverse") =>
                {
                    break e;
                }
                Ok(_) => continue,
                Err(_) => panic!("no permission.denied audit for com.x in 500ms"),
            }
        };
        assert_eq!(ev.payload.get("pluginId").and_then(|v| v.as_str()), Some("com.x"));
        assert_eq!(ev.payload.get("permission").and_then(|v| v.as_str()), Some("image.read"));
        assert_eq!(
            ev.payload.get("requestReason").and_then(|v| v.as_str()),
            Some("analyze screenshot"),
        );
        assert_eq!(ev.payload.get("reason").and_then(|v| v.as_str()), Some("no-ui"));
        assert_eq!(ev.payload.get("caller").and_then(|v| v.as_str()), Some("plugin.reverse"));
    }

    /// When no reason is passed, the audit payload must not contain a requestReason field.
    #[test]
    fn gate_without_reason_omits_request_reason() {
        let rx = super::super::event::EventBus::shared().subscribe();
        let s = mem_store();
        let _ = gate(&s, "com.x", "image.read", "capability", None);
        let ev = loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(e)
                    if e.kind == "permission.denied"
                        && e.payload.get("pluginId").and_then(|v| v.as_str()) == Some("com.x")
                        && e.payload.get("caller").and_then(|v| v.as_str()) == Some("capability") =>
                {
                    break e;
                }
                Ok(_) => continue,
                Err(_) => panic!("no capability permission.denied for com.x in 500ms"),
            }
        };
        assert!(ev.payload.get("requestReason").is_none());
        assert_eq!(ev.payload.get("caller").and_then(|v| v.as_str()), Some("capability"));
    }

    /// Phase 33 — heatmap() must correctly read the whole plugin_permissions table + flag high_risk + aggregate top_denied.
    /// Fills a temp DB with 7 decision rows, runs heatmap()'s SQL/aggregation, and asserts cell count + top ordering + high_risk.
    #[test]
    fn heatmap_reads_grid_and_aggregates_top_denied() {
        use crate::core::storage::{SharedStore, StoreEnum};
        let dir = std::env::temp_dir().join(format!("opencapx-heatmap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));

        let p_a = "com.opencapx.heat-a";
        let p_b = "com.opencapx.heat-b";
        let p_c = "com.opencapx.heat-c";
        let rows: &[(&str, &str, &str)] = &[
            (p_a, "image.read", "granted"),
            (p_a, "camera", "granted"),
            (p_a, "filesystem.write", "denied"),
            (p_b, "image.read", "denied"),
            (p_b, "camera", "ask"),
            (p_c, "browser.open", "granted"),
            (p_c, "microphone", "ask"),
        ];
        {
            let mut s = store.lock().unwrap();
            s.with_conn(|c| {
                for (pid, perm, dec) in rows {
                    c.execute(
                        "INSERT INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at)
                         VALUES (?1, ?2, NULL, ?3, 0)",
                        rusqlite::params![pid, perm, dec],
                    )
                    .unwrap_or(0);
                }
                0usize
            });
        }

        // mirror heatmap()'s read + aggregation path
        type TopRow = (String, i64, i64, i64, bool);
        let (cells_count, mut top): (usize, Vec<TopRow>) = {
            let s = store.lock().unwrap();
            s.with_conn_ref(|c| {
                let mut stmt = c
                    .prepare(
                        "SELECT plugin_id, permission, decision FROM plugin_permissions
                         ORDER BY plugin_id ASC, permission ASC",
                    )
                    .unwrap();
                let cells: Vec<(String, String, String)> = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .unwrap()
                    .filter_map(|x| x.ok())
                    .collect();
                let mut agg: std::collections::BTreeMap<String, (i64, i64, i64)> =
                    std::collections::BTreeMap::new();
                for (_, perm, dec) in &cells {
                    let e = agg.entry(perm.clone()).or_insert((0, 0, 0));
                    match dec.as_str() {
                        "granted" => e.0 += 1,
                        "denied" => e.1 += 1,
                        "ask" => e.2 += 1,
                        _ => {}
                    }
                }
                let mut t: Vec<TopRow> = agg
                    .into_iter()
                    .map(|(perm, (g, d, a))| {
                        let high = HIGH_RISK.contains(&perm.as_str());
                        (perm, g, d, a, high)
                    })
                    .collect();
                t.sort_by(|x, y| y.2.cmp(&x.2).then(y.1.cmp(&x.1)).then(x.0.cmp(&y.0)));
                (cells.len(), t)
            })
            .unwrap_or((0, vec![]))
        };

        assert_eq!(cells_count, 7, "7 decision rows");
        assert_eq!(top.len(), 5, "5 distinct permissions");

        // denied descending: filesystem.write(1) + image.read(1) tie for first, then granted descending, then name ascending
        // → image.read(granted=1) > filesystem.write(granted=0)
        assert_eq!(top[0].0, "image.read");
        assert_eq!(top[0].1, 1, "image.read granted=1");
        assert_eq!(top[0].2, 1, "image.read denied=1");
        assert!(!top[0].4, "image.read is not high_risk");

        assert_eq!(top[1].0, "filesystem.write");
        assert_eq!(top[1].2, 1);
        assert!(top[1].4, "filesystem.write is high_risk");

        // high-risk entries must be flagged
        let camera = top.iter().find(|(p, _, _, _, _)| p == "camera").unwrap();
        assert_eq!(camera.1, 1, "camera granted=1");
        assert_eq!(camera.3, 1, "camera ask=1");
        assert!(camera.4, "camera high_risk");

        let mic = top.iter().find(|(p, _, _, _, _)| p == "microphone").unwrap();
        assert_eq!(mic.3, 1, "microphone ask=1");
        assert!(mic.4, "microphone high_risk");

        let browser = top.iter().find(|(p, _, _, _, _)| p == "browser.open").unwrap();
        assert_eq!(browser.1, 1);
        assert!(!browser.4, "browser.open is not high_risk");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2026-09-16 regression — real DB + permission rows + declaration rows, calling the real heatmap():
    /// the old implementation called is_declared_permission per cell inside the closure (re-locking the store) → same-thread reentrant deadlock,
    /// freezing the plugin tab and the whole app (fix: separate row reading from the declaration-set query so the lock is not reentrant).
    /// Under the old code this test hangs forever; now it must return in milliseconds with correct declared flags.
    #[test]
    fn heatmap_with_db_rows_and_declarations_does_not_deadlock() {
        use crate::core::storage::{SharedStore, StoreEnum};
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-hm-deadlock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store: SharedStore = std::sync::Arc::new(std::sync::Mutex::new(StoreEnum::Db(
            crate::core::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )));
        crate::core::set_shared_store(store.clone());
        let pid = "com.opencapx.hm-deadlock";
        {
            let mut s = store.lock().unwrap();
            let _ = s.with_conn(|c| {
                c.execute(
                    "INSERT INTO plugin_permissions (plugin_id, permission, scope, decision, updated_at)
                     VALUES (?1, 'demo.perm', NULL, 'granted', 0)",
                    rusqlite::params![pid],
                )
                .unwrap_or(0)
                    + c.execute(
                        "INSERT INTO capability_declarations (capability, plugin_id, permission, default_decision, confirmed_at)
                         VALUES ('demo.cap', ?1, 'demo.perm', 'ask', 0)",
                        rusqlite::params![pid],
                    )
                    .unwrap_or(0)
            });
        }
        let t0 = std::time::Instant::now();
        let dto = heatmap();
        let elapsed = t0.elapsed();
        assert_eq!(dto.cells.len(), 1, "one permission row");
        assert!(dto.cells[0].declared, "demo.perm is declared");
        assert_eq!(dto.top_denied.len(), 1, "aggregates to one row");
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "heatmap must be fast: {:?} (a hang means the reentrant-deadlock regression)",
            elapsed
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DB store used by global policy tests (tag-isolated to avoid parallel tests colliding on the DB).
    fn policy_store(tag: &str) -> SharedStore {
        let dir = std::env::temp_dir().join(format!("opencapx-policy-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Arc::new(Mutex::new(StoreEnum::Db(
            super::super::storage::Storage::open(&dir.join("t.db")).unwrap(),
        )))
    }

    /// Global policy setter validation: unknown permission / invalid tier / high-risk granted all rejected; valid tiers write and read back.
    #[test]
    fn global_policy_setter_validates_and_roundtrips() {
        let s = policy_store("setter");
        assert!(set_global_override(&s, "nope.perm", "denied").is_err());
        assert!(set_global_override(&s, "clipboard.read", "yolo").is_err());
        // high-risk cannot be globally granted (only once-at-a-time grants allowed)
        assert!(set_global_override(&s, "process.execute", "granted").is_err());
        assert!(set_global_override(&s, "input.control", "granted").is_err());
        // no override → empty string; write → read back
        assert_eq!(global_override(&s, "clipboard.read"), None);
        assert_eq!(global_override_str(&s, "clipboard.read"), "");
        assert!(set_global_override(&s, "clipboard.read", "denied").is_ok());
        assert_eq!(global_override(&s, "clipboard.read"), Some(Decision::Denied));
        assert_eq!(global_override_str(&s, "clipboard.read"), "denied");
        // writing again = overwrite the same row, not insert
        assert!(set_global_override(&s, "clipboard.read", "ask").is_ok());
        assert_eq!(global_override(&s, "clipboard.read"), Some(Decision::Ask));
        // high-risk can be denied / ask
        assert!(set_global_override(&s, "process.execute", "denied").is_ok());
        assert!(set_global_override(&s, "process.execute", "ask").is_ok());
        // reset → back to no override; repeated resets are idempotent
        assert!(clear_global_override(&s, "clipboard.read").is_ok());
        assert_eq!(global_override(&s, "clipboard.read"), None);
        assert_eq!(global_override_str(&s, "clipboard.read"), "");
        assert!(clear_global_override(&s, "clipboard.read").is_ok());
    }

    /// Contract with the frontend settings.ts: the JSON key is override (not overrideDecision),
    /// and None serializes to null (the frontend treats null as "follow the default").
    #[test]
    fn core_policy_dto_wire_keys_match_frontend_contract() {
        let dto = CorePermPolicyDto {
            permission: "clipboard.read".into(),
            capabilities: vec!["clipboard.read".into(), "context.get_current".into()],
            builtin_default: "ask".into(),
            override_decision: Some("denied".into()),
            effective: "denied".into(),
            high_risk: false,
        };
        let v = serde_json::to_value(&dto).unwrap();
        assert_eq!(v["permission"], "clipboard.read");
        assert_eq!(v["capabilities"][1], "context.get_current");
        assert_eq!(v["builtinDefault"], "ask");
        assert_eq!(v["override"], "denied");
        assert_eq!(v["effective"], "denied");
        assert_eq!(v["highRisk"], false);
        assert!(v.get("overrideDecision").is_none(), "the key name must be override");
        let none = CorePermPolicyDto { override_decision: None, ..dto.clone() };
        assert!(serde_json::to_value(&none).unwrap()["override"].is_null());
        // key-set check: extra keys / missing keys / renames must all fail (a wrong name silently yields undefined on the frontend).
        // compare sets, not order — serde_json outputs in BTreeMap alphabetical order by default, so key order is not part of the contract.
        let keys: std::collections::BTreeSet<&str> =
            v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        let expected: std::collections::BTreeSet<&str> = [
            "permission",
            "capabilities",
            "builtinDefault",
            "override",
            "effective",
            "highRisk",
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, expected);
    }

    /// Mem variant (no DB): reads back no override, writes report storage unavailable.
    #[test]
    fn global_policy_mem_store_is_unavailable() {
        let s = mem_store();
        assert_eq!(global_override(&s, "clipboard.read"), None);
        assert!(set_global_override(&s, "clipboard.read", "denied").is_err());
        assert!(clear_global_override(&s, "clipboard.read").is_err());
    }

    /// Reverse mapping: every capability is covered by exactly one permission (system.permission_status is gate-free, the sole exception).
    #[test]
    fn core_capabilities_for_covers_every_capability() {
        for cap in super::super::capability::CAPABILITY_IDS {
            match capability_permission(cap) {
                Some(perm) => assert!(
                    core_capabilities_for(perm).contains(cap),
                    "{cap} should appear in {perm}'s capability list"
                ),
                None => assert_eq!(cap, &"system.permission_status"),
            }
        }
        assert_eq!(
            core_capabilities_for("clipboard.read"),
            vec!["clipboard.read", "context.get_current"]
        );
        // plugin.install is a vocabulary placeholder with no capability mapping
        assert!(core_capabilities_for("plugin.install").is_empty());
    }

    /// Settings page listing: only permissions with a capability mapping; alphabetical; override/high-risk/default tier all included.
    #[test]
    fn core_policy_list_only_lists_mapped_permissions() {
        let s = policy_store("list");
        assert!(set_global_override(&s, "clipboard.read", "denied").is_ok());
        let list = core_policy_list(&s);
        let find = |p: &str| list.iter().find(|e| e.permission == p);
        let cb = find("clipboard.read").expect("clipboard.read should be listed");
        assert_eq!(
            cb.capabilities,
            vec!["clipboard.read".to_string(), "context.get_current".to_string()]
        );
        assert_eq!(cb.builtin_default, "ask");
        assert_eq!(cb.override_decision.as_deref(), Some("denied"));
        assert_eq!(cb.effective, "denied");
        assert!(!cb.high_risk);
        // no mapping → not listed
        assert!(find("pet.animation").is_none());
        assert!(find("storage.local").is_none());
        assert!(find("plugin.install").is_none());
        // high-risk: tier included, default follows the static table, and effective = built-in default when not overridden
        let fw = find("filesystem.write").expect("filesystem.write should be listed");
        assert!(fw.high_risk);
        assert_eq!(fw.builtin_default, "denied");
        assert_eq!(fw.override_decision, None);
        assert_eq!(fw.effective, "denied");
        // permissions with no capability mapping are never listed — no capability gate triggers them,
        // so listing them is a "click and nothing happens" fake switch (process/microphone/camera etc.)
        assert!(find("process.execute").is_none());
        assert!(find("microphone").is_none());
        assert!(find("camera").is_none());
        assert!(find("network.request").is_none());
        // alphabetical order
        let got: Vec<String> = list.iter().map(|e| e.permission.clone()).collect();
        let mut sorted = got.clone();
        sorted.sort();
        assert_eq!(got, sorted);
        // the listing = the full set of "permissions with a mapping": neither extra (fake switches) nor missing (omitted switches).
        // do not hardcode the count — this invariant follows automatically when capabilities are added.
        let mapped: std::collections::BTreeSet<String> =
            super::super::capability::CAPABILITY_IDS
                .iter()
                .filter_map(|c| capability_permission(c))
                .map(|p| p.to_string())
                .collect();
        let listed: std::collections::BTreeSet<String> =
            list.iter().map(|e| e.permission.clone()).collect();
        assert_eq!(listed, mapped);
        // reset → override returns to None, effective back to the built-in default
        assert!(clear_global_override(&s, "clipboard.read").is_ok());
        let cb2 = core_policy_list(&s)
            .into_iter()
            .find(|e| e.permission == "clipboard.read")
            .expect("still listed");
        assert_eq!(cb2.override_decision, None);
        assert_eq!(cb2.effective, "ask");
    }
}
