//! Automation(i2 §15): Event → Rule → Action.
//! Built on the Event Bus (the same stream B10 subscribes to, no separate channel):
//!
//! ```text
//! Event Bus ──▶ Rule match (when: event type + payload conditions) ──▶ Action (notify | say)
//! ```
//!
//! - Rules live in `~/.opencapx/automation.json` (hand-editable; the future settings page uses the same file);
//!   the engine hot-reloads on file mtime, no restart
//! - `when.match`: field values are equal; `<key>_contains` does substring matching (the file.watch path-prefix case)
//! - Actions expose only Core's native low-risk surface: `notify` (toast + notification.posted),
//!   `say` (the pet bubble). Arbitrary capability execution is not exposed — that needs a permission chain, left to v2
//! - Trigger throttling: the same rule does not re-fire within 5s (mass Downloads writes do not flood)
//! - Each trigger publishes an `automation.rule_fired` event (into the Timeline, auditable)

use crate::core::event::{OpencapxEvent, EventBus};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Minimum interval between two triggers of the same rule.
const RULE_COOLDOWN: Duration = Duration::from_secs(5);

/// Rules file `~/.opencapx/automation.json`.
pub fn rules_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".opencapx")
        .join("automation.json")
}

/// Read rules (missing file = empty; bad JSON = empty + stderr, without blowing up the engine).
fn load_from(path: &std::path::Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => v
            .get("rules")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.is_object())
            .collect(),
        Err(e) => {
            eprintln!("[automation] bad {}: {}", path.display(), e);
            Vec::new()
        }
    }
}

pub fn load_rules() -> Vec<Value> {
    load_from(&rules_path())
}

fn save_to(path: &std::path::Path, rules: &[Value]) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent dir")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;
    let body = json!({ "rules": rules });
    std::fs::write(path, serde_json::to_string_pretty(&body).unwrap())
        .map_err(|e| format!("write {}: {}", path.display(), e))
}

pub fn save_rules(rules: &[Value]) -> Result<(), String> {
    save_to(&rules_path(), rules)
}

/// Rule shape validation (shared by CLI add and the engine). Returns the normalized rule (with id/enabled filled in).
pub fn validate_rule(when: &Value, then: &Value) -> Result<Value, String> {
    let event = when.get("event").and_then(|e| e.as_str()).unwrap_or("");
    if event.is_empty() {
        return Err("when.event (string) is required, e.g. \"capability.event\"".into());
    }
    if let Some(m) = when.get("match") {
        if !m.is_object() {
            return Err("when.match must be an object of {field: value}".into());
        }
    }
    let action = then.get("action").and_then(|a| a.as_str()).unwrap_or("");
    match action {
        "notify" => {
            if then.get("body").and_then(|b| b.as_str()).unwrap_or("").is_empty() {
                return Err("then.body is required for notify".into());
            }
        }
        "say" => {
            if then.get("text").and_then(|t| t.as_str()).unwrap_or("").is_empty() {
                return Err("then.text is required for say".into());
            }
        }
        other => return Err(format!("unknown then.action: {} (allowed: notify, say)", other)),
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(json!({
        "id": format!("rule-{:x}", nanos),
        "enabled": true,
        "when": when,
        "then": then,
    }))
}

pub fn add_rule(when: &Value, then: &Value) -> Result<Value, String> {
    let rule = validate_rule(when, then)?;
    let mut rules = load_rules();
    rules.push(rule.clone());
    save_rules(&rules)?;
    Ok(rule)
}

pub fn remove_rule(id: &str) -> Result<bool, String> {
    let mut rules = load_rules();
    let before = rules.len();
    rules.retain(|r| r.get("id").and_then(|i| i.as_str()) != Some(id));
    if rules.len() == before {
        return Ok(false);
    }
    save_rules(&rules)?;
    Ok(true)
}

/// Set enabled on the rule matching id and return the updated rule (pure function, easy to test).
fn set_enabled_in(rules: &mut [Value], id: &str, enabled: bool) -> Option<Value> {
    let mut updated = None;
    for r in rules.iter_mut() {
        if r.get("id").and_then(|i| i.as_str()) == Some(id) {
            r["enabled"] = json!(enabled);
            updated = Some(r.clone());
        }
    }
    updated
}

/// Enable/disable a rule (the settings page toggle); returns the updated rule, errors if absent.
pub fn set_rule_enabled(id: &str, enabled: bool) -> Result<Value, String> {
    let mut rules = load_rules();
    let rule =
        set_enabled_in(&mut rules, id, enabled).ok_or_else(|| format!("no such rule: {}", id))?;
    save_rules(&rules)?;
    Ok(rule)
}

/// Pure matching: when.event == ev.kind; every when.match entry requires an equal payload field,
/// and keys ending in `_contains` do substring matching. A missing field = no match.
pub fn matches(when: &Value, ev: &OpencapxEvent) -> bool {
    let Some(want) = when.get("event").and_then(|e| e.as_str()) else {
        return false;
    };
    if want != ev.kind {
        return false;
    }
    let Some(m) = when.get("match").and_then(|m| m.as_object()) else {
        return true;
    };
    for (key, val) in m {
        if let Some(field) = key.strip_suffix("_contains") {
            let hit = ev
                .payload
                .get(field)
                .and_then(|p| p.as_str())
                .and_then(|p| val.as_str().map(|v| p.contains(v)))
                .unwrap_or(false);
            if !hit {
                return false;
            }
        } else {
            if ev.payload.get(key) != Some(val) {
                return false;
            }
        }
    }
    true
}

/// Trigger-throttle table (rule id → last fired instant).
fn last_fired() -> &'static Mutex<HashMap<String, Instant>> {
    static M: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cooled_down(rule_id: &str) -> bool {
    match last_fired().lock() {
        Ok(mut m) => {
            let now = Instant::now();
            match m.get(rule_id) {
                Some(t) if now.duration_since(*t) < RULE_COOLDOWN => false,
                _ => {
                    m.insert(rule_id.to_string(), now);
                    true
                }
            }
        }
        Err(_) => false,
    }
}

/// Engine: subscribe to the Bus, hot-reload rules on mtime, execute on a hit.
/// Called once during app setup (a resident thread, the same pattern as spawn_subscribers).
pub fn start(handle: tauri::AppHandle, bus: Arc<EventBus>) {
    std::thread::spawn(move || {
        let mut cached: Vec<Value> = Vec::new();
        let mut mtime: Option<std::time::SystemTime> = None;
        for ev in bus.subscribe() {
            // Hot reload: re-read only when the file has been touched
            if let Ok(meta) = std::fs::metadata(rules_path()) {
                let m = meta.modified().ok();
                if m != mtime {
                    mtime = m;
                    cached = load_rules();
                }
            } else if !cached.is_empty() {
                cached.clear(); // File deleted → rules cleared
                mtime = None;
            }
            for rule in &cached {
                if rule.get("enabled").and_then(|e| e.as_bool()) == Some(false) {
                    continue;
                }
                let Some(when) = rule.get("when") else { continue };
                if !matches(when, &ev) {
                    continue;
                }
                let Some(id) = rule.get("id").and_then(|i| i.as_str()) else { continue };
                if !cooled_down(id) {
                    continue;
                }
                fire(&handle, &bus, id, rule.get("then").cloned().unwrap_or(Value::Null), &ev);
            }
        }
    });
}

/// Execute the action + record an audit event. Actions are only Core's native low-risk surface (notify/say).
fn fire(handle: &tauri::AppHandle, bus: &Arc<EventBus>, rule_id: &str, then: Value, ev: &OpencapxEvent) {
    use tauri::Emitter;
    use tauri_plugin_notification::NotificationExt;
    let action = then.get("action").and_then(|a| a.as_str()).unwrap_or("");
    match action {
        "notify" => {
            let title = then.get("title").and_then(|t| t.as_str()).unwrap_or("OpenCapX Automation");
            let body = then.get("body").and_then(|b| b.as_str()).unwrap_or("");
            let _ = handle.notification().builder().title(title).body(body).show();
            bus.publish(&OpencapxEvent::new(
                "notification.posted",
                "automation",
                json!({ "agentId": "automation", "title": title, "body": body, "severity": "info" }),
            ));
        }
        "say" => {
            let text = then.get("text").and_then(|t| t.as_str()).unwrap_or("");
            bus.publish(&OpencapxEvent::new("pet.say", "automation", json!({ "text": text })));
            let _ = handle.emit("opencapx-say", json!({ "text": text }));
        }
        _ => {
            // Validated at add time; a hand-edited file may inject a bad action — record the event, do not execute
            bus.publish(&OpencapxEvent::new(
                "automation.rule_failed",
                "automation",
                json!({ "ruleId": rule_id, "error": format!("unknown action: {}", action) }),
            ));
            return;
        }
    }
    bus.publish(&OpencapxEvent::new(
        "automation.rule_fired",
        "automation",
        json!({
            "ruleId": rule_id,
            "event": ev.kind,
            "action": action,
        }),
    ));
}

/// CLI (`opencapx automation …`) entry: users manage rules without depending on MCP/UI.
/// `opencapx automation` — clap owns the parsing and the generated help.
#[derive(clap::Parser)]
#[command(name = "opencapx automation", about = "Automation rules: list, add and remove")]
struct AutomationCli {
    #[command(subcommand)]
    cmd: AutomationCmd,
}

#[derive(clap::Subcommand)]
enum AutomationCmd {
    /// List the configured automation rules
    List,
    /// Add a rule from a '<when/then JSON>' spec
    Add { spec: String },
    /// Remove a rule by id
    Remove { id: String },
}

pub fn run_cli(args: &[String]) -> i32 {
    let cli = match super::cli::parse::<AutomationCli>("opencapx automation", args) {
        Ok(c) => c,
        Err(code) => return code,
    };
    match cli.cmd {
        AutomationCmd::List => {
            let rules = load_rules();
            println!("{}", serde_json::to_string_pretty(&json!({ "rules": rules })).unwrap());
            0
        }
        AutomationCmd::Add { spec } => {
            let Ok(v) = serde_json::from_str::<Value>(&spec) else {
                eprintln!("bad JSON: {}", spec);
                return 1;
            };
            let empty = Value::Null;
            let when = v.get("when").unwrap_or(&empty);
            let then = v.get("then").unwrap_or(&empty);
            match add_rule(when, then) {
                Ok(rule) => {
                    println!("{}", serde_json::to_string_pretty(&rule).unwrap());
                    0
                }
                Err(e) => {
                    eprintln!("{}", e);
                    1
                }
            }
        }
        AutomationCmd::Remove { id } => match remove_rule(&id) {
            Ok(true) => {
                println!("removed {}", id);
                0
            }
            Ok(false) => {
                eprintln!("no such rule: {}", id);
                1
            }
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str, payload: Value) -> OpencapxEvent {
        OpencapxEvent::new(kind, "test", payload)
    }

    #[test]
    fn match_event_type_and_fields() {
        let e = ev(
            "capability.event",
            json!({ "subscriptionId": "s1", "capability": "file.watch", "event": "created", "path": "/Users/x/Downloads/a.pdf" }),
        );
        // Event type only
        assert!(matches(&json!({"event": "capability.event"}), &e));
        assert!(!matches(&json!({"event": "agent.started"}), &e));
        // Field equality
        assert!(matches(
            &json!({"event": "capability.event", "match": {"capability": "file.watch", "event": "created"}}),
            &e
        ));
        assert!(!matches(&json!({"event": "capability.event", "match": {"event": "modified"}}), &e));
        // Missing field = no match
        assert!(!matches(&json!({"event": "capability.event", "match": {"nope": 1}}), &e));
    }

    #[test]
    fn match_contains_for_paths() {
        let e = ev("capability.event", json!({ "capability": "file.watch", "path": "/Users/x/Downloads/new.pdf" }));
        // i2 §15 example: a new file lands in ~/Downloads
        assert!(matches(
            &json!({"event": "capability.event", "match": {"path_contains": "/Downloads/"}}),
            &e
        ));
        assert!(!matches(
            &json!({"event": "capability.event", "match": {"path_contains": "/Documents/"}}),
            &e
        ));
        // contains on a non-string field = no match, no panic
        assert!(!matches(&json!({"event": "capability.event", "match": {"path_contains": 5}}), &e));
    }

    #[test]
    fn rule_validation_shapes() {
        let ok = validate_rule(
            &json!({"event": "capability.event", "match": {"path_contains": "/Downloads/"}}),
            &json!({"action": "notify", "title": "New file", "body": "Downloads has a new file"}),
        )
        .unwrap();
        assert!(ok["id"].as_str().unwrap().starts_with("rule-"));
        assert_eq!(ok["enabled"], json!(true));
        // Missing event / unknown action / action missing body
        assert!(validate_rule(&json!({}), &json!({"action": "notify", "body": "x"})).unwrap_err().contains("when.event"));
        assert!(validate_rule(&json!({"event": "x"}), &json!({"action": "shell"})).unwrap_err().contains("unknown then.action"));
        assert!(validate_rule(&json!({"event": "x"}), &json!({"action": "notify"})).unwrap_err().contains("body"));
        assert!(validate_rule(&json!({"event": "x"}), &json!({"action": "say"})).unwrap_err().contains("text"));
    }

    /// Toggle: a hit sets enabled and is returned; a missing id returns None.
    #[test]
    fn set_enabled_toggles_and_misses() {
        let mut rules = vec![json!({
            "id": "r1", "enabled": true,
            "when": {"event": "x"}, "then": {"action": "say", "text": "y"}
        })];
        assert_eq!(set_enabled_in(&mut rules, "r1", false).unwrap()["enabled"], json!(false));
        assert_eq!(rules[0]["enabled"], json!(false));
        assert!(set_enabled_in(&mut rules, "nope", true).is_none());
    }

    /// Add/remove persistence round trip (temp file, does not touch the real ~/.opencapx).
    #[test]
    fn add_remove_roundtrip_on_temp_file() {
        let dir = std::env::temp_dir().join(format!("opencapx-auto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("automation.json");
        // Start from an empty file
        assert!(load_from(&path).is_empty());
        let rule = validate_rule(
            &json!({"event": "agent.completed"}),
            &json!({"action": "say", "text": "All done"}),
        )
        .unwrap();
        save_to(&path, &[rule.clone()]).unwrap();
        let back = load_from(&path);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0]["id"], rule["id"]);
        // remove semantics: delete if present, false if absent
        let mut rules = back.clone();
        rules.retain(|r| r["id"] != rule["id"]);
        save_to(&path, &rules).unwrap();
        assert!(load_from(&path).is_empty());
        // Bad JSON → empty + no blow-up
        std::fs::write(&path, "{broken").unwrap();
        assert!(load_from(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cooldown: the same id does not pass a second time within 5s.
    #[test]
    fn rule_cooldown_window() {
        let id = format!("rule-cool-{}", std::process::id());
        assert!(cooled_down(&id), "first trigger allowed");
        assert!(!cooled_down(&id), "second one within the window blocked");
        last_fired().lock().unwrap().remove(&id);
    }

    /// CLI: missing subcommand / missing argument are usage errors (2); bad JSON is a runtime error (1).
    #[test]
    fn cli_rejects_bad_usage() {
        assert_eq!(run_cli(&[]), 2);
        assert_eq!(run_cli(&["add".into()]), 2);
        assert_eq!(run_cli(&["add".into(), "{bad".into()]), 1);
    }
}
