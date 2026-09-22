//! Subscription registry + built-in file.watch / screen.watch watchers.
//! Implementation of capability.md "Capability Types (v1.1, v1.3 +screen.watch)" /
//! mcp.md "Subscription Tool Pair (v1.1)".
//!
//! Subscriptions live only in the in-memory registry (not persisted to SQLite) and do not survive across MCP connections: each CLI process holds a
//! unique conn id; X-OpenCapX-Conn is reported with both /rpc and SSE /events; when SSE disconnects
//! (Agent host exit) all subscriptions and underlying watchers under that conn are cleaned up.
//!
//! Events flow through the EventBus end to end (`capability.subscribed` / `capability.event` /
//! `capability.unsubscribed`); the Automation evaluation §15 is built on the same stream.

use super::event::{OpencapxEvent, EventBus};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// The set of subscribe-type capabilities (docs/capability.md "Capability Types").
/// Not in this list (including call type and unknown ids) → `capability is not subscribable`.
/// v1.3 +screen.watch: periodic screenshot diff (permission maps to screen.capture in the same tier).
pub const SUBSCRIBE_IDS: &[&str] = &["file.watch", "screen.watch"];

/// Per-Agent subscription cap (v1, mcp.md: leak prevention).
pub const MAX_PER_AGENT: usize = 32;

/// Watcher polling interval (production); tests use a shorter interval and call spawn_file_watcher directly.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Max events reported per diff: a bulk change in a large directory (e.g. git checkout) must not flood
/// the event table; the excess is dropped with the snapshot update (capability.md already notes the cap).
const MAX_EVENTS_PER_CYCLE: usize = 100;
/// Snapshot entry cap: prevents mistakenly subscribing to a huge directory (like ~) from dragging down polling.
const MAX_ENTRIES: usize = 5000;
/// screen.watch polling interval bounds (seconds): the lower bound prevents high-frequency screenshots flooding + CPU spinning,
/// the upper bound prevents an agent passing an astronomical number as a DoS.
const SCREEN_INTERVAL_MIN: u64 = 5;
const SCREEN_INTERVAL_MAX: u64 = 3600;
/// screen.watch default interval (seconds).
const SCREEN_INTERVAL_DEFAULT: u64 = 30;

struct Sub {
    agent_id: String,
    conn_id: String,
    capability: String,
    stop: Arc<AtomicBool>,
}

fn registry() -> &'static Mutex<HashMap<String, Sub>> {
    static R: OnceLock<Mutex<HashMap<String, Sub>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn subscribable(capability: &str) -> bool {
    SUBSCRIBE_IDS.contains(&capability)
}

fn new_id() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("sub_{:x}", n)
}

fn count_for_agent(agent_id: &str) -> usize {
    registry()
        .lock()
        .map(|r| r.values().filter(|s| s.agent_id == agent_id).count())
        .unwrap_or(0)
}

/// file.watch input validation: .path is a required string; recursive is an optional bool.
fn validate_watch_input(input: &Value) -> Result<(PathBuf, bool), String> {
    let Some(path) = input.get("path").and_then(|p| p.as_str()) else {
        return Err("invalid input: path (string) required".into());
    };
    if path.is_empty() {
        return Err("invalid input: path must be non-empty".into());
    }
    let recursive = input.get("recursive").and_then(|r| r.as_bool()).unwrap_or(false);
    Ok((PathBuf::from(path), recursive))
}

/// screen.watch input validation: interval seconds ∈ [5,3600], default 30; region shape is checked first
/// ({x,y,width,height} non-negative integers, w/h ≥1); out-of-range clamping is left to vision::capture.
/// This only blocks "the shape is simply wrong": the watcher skips capture errors without reporting, so a bad region
/// that is not stopped at subscription time manifests as silently never emitting events.
fn validate_screen_input(input: &Value) -> Result<(u64, Option<Value>), String> {
    let interval = input.get("interval").and_then(|i| i.as_u64()).unwrap_or(SCREEN_INTERVAL_DEFAULT);
    if !(SCREEN_INTERVAL_MIN..=SCREEN_INTERVAL_MAX).contains(&interval) {
        return Err(format!(
            "invalid input: interval must be {}..={} seconds (default {})",
            SCREEN_INTERVAL_MIN, SCREEN_INTERVAL_MAX, SCREEN_INTERVAL_DEFAULT
        ));
    }
    let region = input.get("region").filter(|r| r.is_object()).cloned();
    if let Some(r) = &region {
        let get = |k: &str| r.get(k).and_then(|v| v.as_i64());
        match (get("x"), get("y"), get("width"), get("height")) {
            (Some(x), Some(y), Some(w), Some(h)) => {
                if x < 0 || y < 0 {
                    return Err("invalid input: region x and y must be ≥0".into());
                }
                if w < 1 || h < 1 {
                    return Err("invalid input: region width and height must be ≥1".into());
                }
            }
            _ => {
                return Err(
                    "invalid input: region must be {x, y, width, height} (integers)".into()
                )
            }
        }
    }
    Ok((interval, region))
}

/// Subscription parameters: the validation result per capability type, each watcher takes what it needs.
enum WatchSpec {
    File { path: PathBuf, recursive: bool },
    Screen { interval: Duration, region: Option<Value> },
}

/// Establish a subscription: validate → write to the table → start the watcher → `capability.subscribed` on the bus.
/// The Agent-layer permission was already decided in rpc::handle (decided once at subscription time; event pushes are not re-judged per item).
/// Arc<EventBus>: the watcher thread holds it continuously.
pub fn subscribe(
    agent_id: &str,
    conn_id: &str,
    capability: &str,
    input: &Value,
    bus: &Arc<EventBus>,
) -> Result<String, String> {
    if !subscribable(capability) {
        return Err(format!("capability is not subscribable: {}", capability));
    }
    if count_for_agent(agent_id) >= MAX_PER_AGENT {
        return Err(format!("subscription limit reached ({} per agent)", MAX_PER_AGENT));
    }
    let spec = match capability {
        "file.watch" => {
            let (path, recursive) = validate_watch_input(input)?;
            WatchSpec::File { path, recursive }
        }
        "screen.watch" => {
            let (interval, region) = validate_screen_input(input)?;
            WatchSpec::Screen { interval: Duration::from_secs(interval), region }
        }
        _ => return Err(format!("capability is not subscribable: {}", capability)),
    };
    let id = new_id();
    let stop = Arc::new(AtomicBool::new(false));
    if let Ok(mut r) = registry().lock() {
        r.insert(
            id.clone(),
            Sub {
                agent_id: agent_id.to_string(),
                conn_id: conn_id.to_string(),
                capability: capability.to_string(),
                stop: stop.clone(),
            },
        );
    }
    match spec {
        WatchSpec::File { path, recursive } => spawn_file_watcher(
            bus.clone(), stop, id.clone(), capability, agent_id, path, recursive, POLL_INTERVAL,
        ),
        WatchSpec::Screen { interval, region } => spawn_screen_watcher(
            bus.clone(), stop, id.clone(), capability, agent_id, interval, region,
        ),
    }
    bus.publish(&OpencapxEvent::new(
        "capability.subscribed",
        "core",
        json!({ "subscriptionId": id, "capability": capability, "agentId": agent_id }),
    ));
    Ok(id)
}

/// Unsubscribe (idempotent: a non-existent id also counts as success, duplicate unsubscribe is not an error). Returns whether it actually unsubscribed.
pub fn unsubscribe(subscription_id: &str, bus: &EventBus) -> bool {
    unsubscribe_with_reason(subscription_id, bus, "agent")
}

/// SSE disconnect cleanup: unsubscribe everything under that conn. Returns the number cleaned up.
pub fn cleanup_conn(conn_id: &str, bus: &EventBus) -> usize {
    let ids: Vec<String> = registry()
        .lock()
        .map(|r| {
            r.iter()
                .filter(|(_, s)| s.conn_id == conn_id)
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    for id in &ids {
        unsubscribe_with_reason(id, bus, "disconnect");
    }
    ids.len()
}

fn unsubscribe_with_reason(subscription_id: &str, bus: &EventBus, reason: &str) -> bool {
    let removed = registry().lock().ok().and_then(|mut r| r.remove(subscription_id));
    match removed {
        Some(sub) => {
            sub.stop.store(true, Ordering::Relaxed);
            bus.publish(&OpencapxEvent::new(
                "capability.unsubscribed",
                "core",
                json!({
                    "subscriptionId": subscription_id,
                    "capability": sub.capability,
                    "agentId": sub.agent_id,
                    "reason": reason,
                }),
            ));
            true
        }
        None => false,
    }
}

/// Subscription table snapshot (for tests / debugging).
pub fn active() -> Vec<Value> {
    registry()
        .lock()
        .map(|r| {
            r.iter()
                .map(|(id, s)| {
                    json!({ "subscriptionId": id, "capability": s.capability, "agentId": s.agent_id })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One directory scan: files are all collected; directories are collected but only report created/removed (to prevent a parent directory
/// mtime from flooding as child files change). Truncated over MAX_ENTRIES.
fn scan(root: &Path, recursive: bool) -> HashMap<PathBuf, std::time::SystemTime> {
    let mut out: HashMap<PathBuf, std::time::SystemTime> = HashMap::new();
    // root is a file: watch only this one
    if root.is_file() {
        if let Ok(m) = root.metadata().and_then(|md| md.modified()) {
            out.insert(root.to_path_buf(), m);
        }
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            if out.len() >= MAX_ENTRIES {
                return out;
            }
            let Ok(md) = e.metadata() else { continue };
            let Ok(m) = md.modified() else { continue };
            let p = e.path();
            if md.is_dir() {
                // Directories record presence but not mtime: use UNIX_EPOCH as a placeholder so the modified check naturally never fires
                out.insert(p, std::time::UNIX_EPOCH);
                if recursive {
                    stack.push(e.path());
                }
            } else {
                out.insert(p, m);
            }
        }
    }
    out
}

/// Diff two snapshots → a list of (event, path), capped per round at MAX_EVENTS_PER_CYCLE.
fn diff(prev: &HashMap<PathBuf, std::time::SystemTime>, next: &HashMap<PathBuf, std::time::SystemTime>) -> Vec<(&'static str, PathBuf)> {
    let mut events = Vec::new();
    let mut push = |event: &'static str, p: &PathBuf| {
        if events.len() < MAX_EVENTS_PER_CYCLE {
            events.push((event, p.clone()));
        }
    };
    for (p, m) in next {
        match prev.get(p) {
            None => push("created", p),
            Some(old) if *m > *old && *old != std::time::SystemTime::UNIX_EPOCH => push("modified", p),
            _ => {}
        }
    }
    for p in prev.keys() {
        if !next.contains_key(p) {
            push("removed", p);
        }
    }
    events
}

/// One poll round: scan → diff → publish, and advance the snapshot.
/// Extracted into a function so tests can drive a round **deterministically** — waiting on wall-clock thread events on a heavily loaded
/// CI runner starves (observed 2026-09-20: 60s budget exhausted with still no event).
#[allow(clippy::too_many_arguments)]
fn poll_file_watch(
    bus: &EventBus,
    prev: &mut HashMap<PathBuf, std::time::SystemTime>,
    path: &Path,
    recursive: bool,
    sub_id: &str,
    capability: &str,
    agent_id: &str,
) {
    let next = scan(path, recursive);
    for (event, p) in diff(prev, &next) {
        bus.publish(&OpencapxEvent::new(
            "capability.event",
            "core",
            json!({
                "subscriptionId": sub_id,
                "capability": capability,
                "agentId": agent_id,
                "event": event,
                "path": p.to_string_lossy(),
            }),
        ));
    }
    *prev = next;
}

/// file.watch watcher: polls and diffs, events go on the bus in the eventSchema shape ({event, path}).
fn spawn_file_watcher(
    bus: Arc<EventBus>,
    stop: Arc<AtomicBool>,
    sub_id: String,
    capability: &str,
    agent_id: &str,
    path: PathBuf,
    recursive: bool,
    interval: Duration,
) {
    let capability = capability.to_string();
    let agent_id = agent_id.to_string();
    std::thread::spawn(move || {
        // The first round only builds the baseline: files already present at subscription time do not count as created
        let mut prev = scan(&path, recursive);
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(interval);
            if stop.load(Ordering::Relaxed) {
                return;
            }
            poll_file_watch(&bus, &mut prev, &path, recursive, &sub_id, &capability, &agent_id);
        }
    });
}

/// screen.watch watcher: periodic vision::capture → decode pixel hash and diff; on change it emits
/// `capability.event` {event:"changed", image} (frame path, the agent can then read the image).
/// Frame file rotation: only the latest frame is kept (delete the old frame on change, delete the new frame when unchanged), so the cache does not grow over time.
/// A capture/decode failure only skips that round (environmental issues like screenshot TCC denial or a missing scrot must not kill
/// the subscription); the first error is logged, reset on success — so continuous failures do not spam stderr every round.
fn spawn_screen_watcher(
    bus: Arc<EventBus>,
    stop: Arc<AtomicBool>,
    sub_id: String,
    capability: &str,
    agent_id: &str,
    interval: Duration,
    region: Option<Value>,
) {
    let capability = capability.to_string();
    let agent_id = agent_id.to_string();
    std::thread::spawn(move || {
        use std::hash::{Hash, Hasher};
        // (pixel hash, frame path); the first round only builds the baseline, and the baseline frame emits no event (same semantics as file.watch)
        let mut prev: Option<(u64, PathBuf)> = None;
        let mut warned = false;
        loop {
            if stop.load(Ordering::Relaxed) {
                if let Some((_, p)) = prev.take() {
                    let _ = std::fs::remove_file(p);
                }
                return;
            }
            std::thread::sleep(interval);
            if stop.load(Ordering::Relaxed) {
                if let Some((_, p)) = prev.take() {
                    let _ = std::fs::remove_file(p);
                }
                return;
            }
            let mut input = json!({});
            if let Some(r) = &region {
                input["region"] = r.clone();
            }
            let frame = match super::vision::capture(&input) {
                Ok(out) => out.get("image").and_then(|p| p.as_str()).map(PathBuf::from),
                Err(e) => {
                    if !warned {
                        eprintln!("screen.watch: capture failed (will retry): {}", e);
                        warned = true;
                    }
                    None
                }
            };
            let Some(path) = frame else { continue };
            let hash = match image::open(&path) {
                Ok(img) => {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    img.to_rgba8().as_raw().hash(&mut h);
                    h.finish()
                }
                Err(e) => {
                    if !warned {
                        eprintln!("screen.watch: decode failed (will retry): {}", e);
                        warned = true;
                    }
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
            };
            warned = false;
            match prev.take() {
                // Unchanged: drop the new frame and keep the old one (the next round still compares against the same content)
                Some((ph, pp)) if ph == hash => {
                    let _ = std::fs::remove_file(&path);
                    prev = Some((ph, pp));
                }
                // Changed: delete the old frame and emit an event (carrying the new frame path)
                Some((_, pp)) => {
                    let _ = std::fs::remove_file(&pp);
                    prev = Some((hash, path.clone()));
                    bus.publish(&OpencapxEvent::new(
                        "capability.event",
                        "core",
                        json!({
                            "subscriptionId": sub_id,
                            "capability": capability,
                            "agentId": agent_id,
                            "event": "changed",
                            "image": path.to_string_lossy(),
                        }),
                    ));
                }
                // baseline
                None => prev = Some((hash, path)),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry is a process-level global, so tests are serialized to avoid interference.
    /// Tests use a unique suffix: avoids a same-named file entering the baseline during race rewrites.
    fn nanos_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static L: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        // Poisoning is allowed through too: otherwise one test panic would leave the rest of the group stuck on lock()
        // (observed in CI on 2026-09-20: 1 flake amplified into 3 reds).
        L.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn only_subscribe_ids_are_subscribable() {
        assert!(subscribable("file.watch"));
        assert!(subscribable("screen.watch"), "v1.3 second subscribe type");
        assert!(!subscribable("file.read"), "call type is not subscribable");
        assert!(!subscribable("screen.capture"), "call type is not subscribable");
        assert!(!subscribable("image.analyze"), "call type is not subscribable");
        assert!(!subscribable("nope.nope"), "unknown id is not subscribable");
        assert!(super::super::capability::known("file.watch"), "file.watch must be in CAPABILITY_IDS");
        assert!(super::super::capability::known("screen.watch"), "screen.watch must be in CAPABILITY_IDS");
    }

    #[test]
    fn watch_input_requires_path() {
        assert!(validate_watch_input(&json!({})).unwrap_err().contains("path"));
        assert!(validate_watch_input(&json!({ "path": "" })).unwrap_err().contains("non-empty"));
        let (p, r) = validate_watch_input(&json!({ "path": "/tmp" })).unwrap();
        assert_eq!(p, PathBuf::from("/tmp"));
        assert!(!r);
        assert!(validate_watch_input(&json!({ "path": "/tmp", "recursive": true })).unwrap().1);
    }

    /// screen.watch input: interval defaults to 30 / bounds; region shape is checked first.
    #[test]
    fn screen_input_validates_interval_and_region() {
        let (iv, region) = validate_screen_input(&json!({})).unwrap();
        assert_eq!(iv, SCREEN_INTERVAL_DEFAULT);
        assert!(region.is_none(), "region is optional");
        let (iv, _) = validate_screen_input(&json!({ "interval": 5 })).unwrap();
        assert_eq!(iv, SCREEN_INTERVAL_MIN);
        let (iv, region) = validate_screen_input(
            &json!({ "interval": 60, "region": { "x": 0, "y": 100, "width": 800, "height": 600 } }),
        )
        .unwrap();
        assert_eq!(iv, 60);
        assert_eq!(region.unwrap()["width"], json!(800));
        // interval out of range
        assert!(validate_screen_input(&json!({ "interval": 4 })).unwrap_err().contains("interval"));
        assert!(validate_screen_input(&json!({ "interval": 3601 })).unwrap_err().contains("interval"));
        // Bad region shapes: missing field / negative coordinate / zero size
        assert!(validate_screen_input(&json!({ "region": { "x": 0 } }))
            .unwrap_err()
            .contains("region must be"));
        assert!(validate_screen_input(&json!({ "region": { "x": -1, "y": 0, "width": 10, "height": 10 } }))
            .unwrap_err()
            .contains("≥0"));
        assert!(validate_screen_input(&json!({ "region": { "x": 0, "y": 0, "width": 0, "height": 10 } }))
            .unwrap_err()
            .contains("≥1"));
        // region is not an object: treated as absent (same leniency as vision::capture, does not block subscription)
        assert!(validate_screen_input(&json!({ "region": "full" })).unwrap().1.is_none());
    }

    #[test]
    fn subscribe_rejects_and_enforces_agent_cap() {
        let _g = lock();
        let bus = Arc::new(EventBus::new());
        // call type / unknown id → not subscribable
        let e = subscribe("ag_t", "conn-a", "file.read", &json!({ "path": "/tmp" }), &bus).unwrap_err();
        assert!(e.contains("not subscribable"), "{}", e);
        // missing path
        let e = subscribe("ag_t", "conn-a", "file.watch", &json!({}), &bus).unwrap_err();
        assert!(e.contains("path"), "{}", e);
        // screen.watch: an out-of-range interval is stopped at subscription time (the watcher only skips without reporting, so it must fail fast here)
        let e = subscribe("ag_t", "conn-a", "screen.watch", &json!({ "interval": 1 }), &bus).unwrap_err();
        assert!(e.contains("interval"), "{}", e);
        let e = subscribe("ag_t", "conn-a", "screen.watch", &json!({ "region": { "x": 0 } }), &bus).unwrap_err();
        assert!(e.contains("region"), "{}", e);
        // A valid screen.watch subscription (default 30s interval; the test process ends long before the first frame, so no real screenshot)
        let sw = subscribe("ag_t", "conn-a", "screen.watch", &json!({ "interval": 3600 }), &bus).unwrap();
        assert!(sw.starts_with("sub_"));
        assert!(unsubscribe(&sw, &bus));
        // The same agent reaches 32 (subscribing to a non-existent path still counts as establishing a subscription — the watcher spins idle, which is legal)
        let mut ids = Vec::new();
        for i in 0..MAX_PER_AGENT {
            ids.push(subscribe("ag_cap", "conn-a", "file.watch", &json!({ "path": "/definitely/not/here" }), &bus).unwrap());
            assert_eq!(ids.len(), i + 1);
        }
        let e = subscribe("ag_cap", "conn-a", "file.watch", &json!({ "path": "/tmp" }), &bus).unwrap_err();
        assert!(e.contains("limit"), "{}", e);
        // Other agents are unaffected; unsubscribing one frees a slot
        assert!(subscribe("ag_other", "conn-b", "file.watch", &json!({ "path": "/tmp" }), &bus).is_ok());
        assert!(unsubscribe(&ids[0], &bus));
        assert!(subscribe("ag_cap", "conn-a", "file.watch", &json!({ "path": "/tmp" }), &bus).is_ok());
        // clean up
        cleanup_conn("conn-a", &bus);
        cleanup_conn("conn-b", &bus);
    }

    #[test]
    fn unsubscribe_is_idempotent_and_cleanup_conn_scoped() {
        let _g = lock();
        let bus = Arc::new(EventBus::new());
        let a1 = subscribe("ag_x", "conn-1", "file.watch", &json!({ "path": "/tmp/none-1" }), &bus).unwrap();
        let a2 = subscribe("ag_x", "conn-1", "file.watch", &json!({ "path": "/tmp/none-2" }), &bus).unwrap();
        let b1 = subscribe("ag_x", "conn-2", "file.watch", &json!({ "path": "/tmp/none-3" }), &bus).unwrap();
        // Idempotent: duplicate unsubscribe is not an error
        assert!(unsubscribe(&a1, &bus));
        assert!(!unsubscribe(&a1, &bus));
        assert!(!unsubscribe("sub_nothing", &bus));
        // Disconnect cleanup only touches this conn
        assert_eq!(cleanup_conn("conn-1", &bus), 1);
        assert_eq!(active().len(), 1);
        assert!(active()[0]["subscriptionId"].as_str().unwrap_or("").len() > 0);
        assert_eq!(cleanup_conn("conn-2", &bus), 1);
        assert_eq!(active().len(), 0);
        assert_eq!(cleanup_conn("conn-1", &bus), 0);
        assert!(!unsubscribe(&a2, &bus), "conn-1 already cleaned up");
        assert!(!unsubscribe(&b1, &bus), "conn-2 already cleaned up");
    }

    /// One scan→diff→publish round of file.watch (the same `poll_file_watch` as the watcher thread).
    /// Deterministic: no sleep, no wall-clock budget, unaffected by CI scheduling starvation.
    #[test]
    fn file_watch_poll_emits_created_modified_removed() {
        let _g = lock();
        let bus = EventBus::new();
        let rx = bus.subscribe();
        let dir = std::env::temp_dir().join(format!("opencapx-fwp-{}", nanos_suffix()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sub_id = "sub_test";
        let mut prev = scan(&dir, false);
        let f = dir.join("notes.txt");
        let full = f.to_string_lossy().to_string();
        let poll = |bus: &EventBus, prev: &mut HashMap<PathBuf, std::time::SystemTime>| {
            poll_file_watch(bus, prev, &dir, false, sub_id, "file.watch", "ag_w");
        };

        std::fs::write(&f, "v1").unwrap();
        poll(&bus, &mut prev);
        let e = rx.recv_timeout(Duration::from_secs(2)).expect("created");
        assert_eq!(e.kind, "capability.event");
        assert_eq!(e.payload["event"], json!("created"));
        assert_eq!(e.payload["path"], json!(full));
        assert_eq!(e.payload["subscriptionId"], json!(sub_id));
        assert_eq!(e.payload["capability"], json!("file.watch"));
        assert_eq!(e.payload["agentId"], json!("ag_w"));

        // mtime must actually advance (diff uses `m > old` to judge modified): give the filesystem clock some granularity.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&f, "v2 with more content").unwrap();
        poll(&bus, &mut prev);
        let e = rx.recv_timeout(Duration::from_secs(2)).expect("modified");
        assert_eq!(e.payload["event"], json!("modified"));
        assert_eq!(e.payload["path"], json!(full));

        std::fs::remove_file(&f).unwrap();
        poll(&bus, &mut prev);
        let e = rx.recv_timeout(Duration::from_secs(2)).expect("removed");
        assert_eq!(e.payload["event"], json!("removed"));
        assert_eq!(e.payload["path"], json!(full));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// file.watch end-to-end (thread + real wall clock): the three-state events created / modified / removed
    /// go on the bus in order, with payloads matching eventSchema ({event, path}) + subscriptionId.
    /// **Not in CI**: the assertions depend on the watcher thread being scheduled within budget; on a starved CI runner even 60s waits produce no
    /// event (observed 2026-09-20). Payload and classification are already covered by the deterministic case above; this case is kept for
    /// `--ignored` manual runs (the "no more events after the watcher stops" point can only be verified by it).
    #[test]
    #[ignore = "wall-clock/thread scheduling dependent; deterministic coverage is file_watch_poll_emits_created_modified_removed"]
    fn file_watcher_emits_created_modified_removed() {
        let _g = lock();
        let bus = Arc::new(EventBus::new());
        let rx = bus.subscribe();
        let dir = std::env::temp_dir().join(format!("opencapx-fw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let sub_id = "sub_test".to_string();
        spawn_file_watcher(
            bus.clone(),
            stop.clone(),
            sub_id.clone(),
            "file.watch",
            "ag_w",
            dir.clone(),
            true,
            Duration::from_millis(50),
        );

        // Poll within a 60s overall deadline: under heavy CI load a single poll round may starve; the intent is
        // "events will eventually arrive in order" rather than "must arrive within 5s".
        let wait_for = |event: &str, path_contains: &str| {
            let start = std::time::Instant::now();
            let budget = Duration::from_secs(60);
            loop {
                if start.elapsed() >= budget {
                    panic!(
                        "timed out waiting for {event} on {path_contains}"
                    );
                }
                match rx.recv_timeout(Duration::from_secs(5)) {
                    Ok(e)
                        if e.kind == "capability.event"
                            && e.payload["event"].as_str() == Some(event)
                            && e.payload["path"]
                                .as_str()
                                .map(|p| p.contains(path_contains))
                                .unwrap_or(false) =>
                    {
                        return e;
                    }
                    _ => continue,
                }
            }
        };

        // Only the created for the file just written is accepted: if v2 is written too fast the watcher first sees v2,
        // only created is reported and modified never arrives (highly likely under heavy load). A 120s overall deadline prevents hanging.
        let start = std::time::Instant::now();
        let budget = Duration::from_secs(120);
        let (e, f) = loop {
            if start.elapsed() >= budget {
                panic!("timed out waiting for a fresh created event");
            }
            let f = dir.join(format!("notes{}.txt", nanos_suffix()));
            std::fs::write(&f, "v1").unwrap();
            let want = f.to_string_lossy().to_string();
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(e)
                    if e.kind == "capability.event"
                        && e.payload["event"] == "created"
                        && e.payload["path"].as_str() == Some(want.as_str()) =>
                {
                    break (e, f)
                }
                _ => continue, // baseline not established or non-target event: rename and write again
            }
        };
        assert_eq!(e.payload["subscriptionId"], json!(sub_id));
        assert_eq!(e.payload["capability"], json!("file.watch"));
        assert_eq!(e.payload["agentId"], json!("ag_w"));

        std::fs::write(&f, "v2 with more content").unwrap();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        wait_for("modified", &name);

        std::fs::remove_file(&f).unwrap();
        wait_for("removed", &name);

        stop.store(true, Ordering::Relaxed);
        // The watcher has stopped: further changes do not go on the bus (give one poll cycle to verify)
        std::fs::write(&f, "ghost").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        // drain: there should be no more capability.event
        while let Ok(e) = rx.try_recv() {
            assert_ne!(e.kind, "capability.event", "no more events should be reported after the watcher stops");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// screen.watch end-to-end (manual on a real machine): real screenshots + screen-recording TCC.
    /// The menu-bar clock/cursor animation almost certainly makes adjacent frames differ, so changed should be received within a short window,
    /// and the frame file carried by the event must actually exist (the contract for the agent then doing image.analyze).
    #[test]
    #[ignore = "captures the real screen (TCC); run with --ignored manually"]
    fn screen_watcher_emits_changed_manual() {
        let _g = lock();
        let bus = Arc::new(EventBus::new());
        let rx = bus.subscribe();
        let sid = subscribe("ag_sw", "conn-sw", "screen.watch", &json!({ "interval": 5 }), &bus)
            .expect("screen.watch subscribable");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let mut got = None;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(e) if e.kind == "capability.event"
                    && e.payload["capability"] == json!("screen.watch")
                    && e.payload["event"] == json!("changed") =>
                {
                    got = Some(e);
                    break;
                }
                _ => continue,
            }
        }
        let e = got.expect("changed event within 60s (screen nearly always changes)");
        assert_eq!(e.payload["subscriptionId"], json!(sid));
        assert_eq!(e.payload["agentId"], json!("ag_sw"));
        let img = e.payload["image"].as_str().unwrap();
        assert!(Path::new(img).is_file(), "event carries an existing frame file: {}", img);
        unsubscribe(&sid, &bus);
        assert!(active().is_empty());
    }

    /// diff pure function: created/modified/removed + directories do not report modified + per-round cap.
    #[test]
    fn diff_classifies_and_caps() {
        let t0 = std::time::SystemTime::UNIX_EPOCH;
        let t1 = t0 + Duration::from_secs(10);
        let t2 = t0 + Duration::from_secs(20);
        let mut prev = HashMap::new();
        prev.insert(PathBuf::from("/a/stay.txt"), t1);
        prev.insert(PathBuf::from("/a/stay.txt.mod"), t1); // mtime changes later → modified
        prev.insert(PathBuf::from("/a/gone.txt"), t1);
        prev.insert(PathBuf::from("/a/old-dir"), t0); // directory placeholder = EPOCH
        let mut next = HashMap::new();
        next.insert(PathBuf::from("/a/stay.txt"), t1); // unchanged
        next.insert(PathBuf::from("/a/stay.txt.mod"), t2); // mtime changes
        next.insert(PathBuf::from("/a/new.txt"), t1); // added
        next.insert(PathBuf::from("/a/old-dir"), t2); // directory mtime changes → not reported
        next.insert(PathBuf::from("/a/dir-new"), t0); // new directory → created
        let mut events = diff(&prev, &next);
        events.sort_by_key(|(e, p)| (e.to_string(), p.clone()));
        assert_eq!(events.len(), 4, "new.txt + dir-new created, stay.txt.mod modified, gone.txt removed: {:?}", events);
        assert!(events.contains(&("created", PathBuf::from("/a/new.txt"))));
        assert!(events.contains(&("created", PathBuf::from("/a/dir-new"))));
        assert!(events.contains(&("modified", PathBuf::from("/a/stay.txt.mod"))));
        assert!(events.iter().any(|(e, p)| *e == "removed" && p.ends_with("gone.txt")));
        // Cap: 300 new files → only MAX_EVENTS_PER_CYCLE reported
        let empty = HashMap::new();
        let mut big = HashMap::new();
        for i in 0..300 {
            big.insert(PathBuf::from(format!("/b/{}.txt", i)), t1);
        }
        assert_eq!(diff(&empty, &big).len(), MAX_EVENTS_PER_CYCLE);
    }

    /// scan: files/directories collected, recursive vs single level, over-limit truncation.
    #[test]
    fn scan_walks_and_caps() {
        let dir = std::env::temp_dir().join(format!("opencapx-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("x/y")).unwrap();
        std::fs::write(dir.join("top.txt"), "1").unwrap();
        std::fs::write(dir.join("x/mid.txt"), "2").unwrap();
        std::fs::write(dir.join("x/y/deep.txt"), "3").unwrap();

        let flat = scan(&dir, false);
        assert!(flat.contains_key(dir.join("top.txt").as_path()));
        assert!(flat.contains_key(dir.join("x").as_path()), "single level also records directory presence");
        assert!(!flat.contains_key(dir.join("x/mid.txt").as_path()), "single level does not enter subdirectory files");

        let deep = scan(&dir, true);
        assert!(deep.contains_key(dir.join("x/mid.txt").as_path()));
        assert!(deep.contains_key(dir.join("x/y/deep.txt").as_path()));
        assert_eq!(deep.len(), 5, "top.txt + x + mid.txt + y + deep.txt");

        // single-file root
        let single = scan(&dir.join("top.txt"), true);
        assert_eq!(single.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
