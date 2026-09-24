//! Capability Registry + Router. See docs/capability.md.
//! Agents only see a capability id; providers are ordered by priority, with level-by-level fallback on failure.

use super::plugin::PluginManager;
use serde_json::{json, Value};
use std::time::Duration;

/// v1 standard capability set. Additions must be synced to docs/capability.md.
pub const CAPABILITY_IDS: &[&str] = &[
    "image.analyze",
    "image.ocr",
    "audio.transcribe",
    "speech.synthesize",
    "screen.capture",
    "clipboard.read",
    "browser.open",
    "browser.read",
    "file.read",
    "clipboard.write",
    "file.write",
    "file.search",
    // v1.1 first subscribe-type capability (capability typing in docs/capability.md)
    "file.watch",
    // v1.1(A7) Computer Context: get the user's current desktop context in one call
    "context.get_current",
    // v1.2 OS permission probing: read-only metadata, exempt from the Agent-layer gate (see permissions.md)
    "system.permission_status",
    // v1.3 high/medium-value batch: automation / input / PIM / audio / scheme
    "automation.run",
    "input.send",
    "photos.read",
    "contacts.search",
    "calendar.events",
    "location.get",
    "audio.play",
    "url.scheme.open",
    // v1.3 second subscribe type: periodic screenshot diff (capability.md "capability typing")
    "screen.watch",
    // v1.4 high-value batch: media / power (messages/window/PIM extension follows in the second batch)
    "media.playback",
    "system.sleep",
    "system.lock",
    // v1.4 medium-value batch: settings / print + second batch of personal data (messages/window/notes/reminders/mail)
    "system.settings",
    "printer.print",
    "messages.recent",
    "window.list",
    "window.focus",
    "notes.read",
    "reminders.read",
    "reminders.write",
    "mail.recent",
    // v1.5 Things data surface: provided by plugins only (no built-in fallback, not in BUILTIN_IDS)
    "things.add",
    "things.update",
    "things.list",
    "things.show",
    "things.search",
    "things.delete",
];

/// Capabilities covered by Core built-in providers (docs/capability.md "built-in providers").
/// Executed as a fallback when no plugin registers a capability of the same name; a plugin registration overrides the built-in.
/// v1.1(A9) +image.analyze (local Ollama) / screen.capture (platform command).
/// v1.2 +clipboard.read / file.read (arboard / std::fs).
/// v1.2 +browser.open / browser.read / speech.synthesize / image.ocr.
/// v1.3 +automation.run / input.send / photos.read / contacts.search /
/// calendar.events / location.get / audio.play / url.scheme.open.
/// v1.4 +media.playback / system.sleep|lock / system.settings / printer.print
/// / messages.recent / window.list|focus / notes.read / reminders.read|write
/// / mail.recent (full batch complete, 36 registry / 33 built-in).
pub const BUILTIN_IDS: &[&str] = &[
    "clipboard.read",
    "clipboard.write",
    "file.read",
    "file.write",
    "file.search",
    "image.analyze",
    "image.ocr",
    "screen.capture",
    "speech.synthesize",
    "browser.open",
    "browser.read",
    "context.get_current",
    "system.permission_status",
    "automation.run",
    "input.send",
    "photos.read",
    "contacts.search",
    "calendar.events",
    "location.get",
    "audio.play",
    "url.scheme.open",
    "media.playback",
    "system.sleep",
    "system.lock",
    "system.settings",
    "printer.print",
    "messages.recent",
    "window.list",
    "window.focus",
    "notes.read",
    "reminders.read",
    "reminders.write",
    "mail.recent",
];

/// Built-in (reserved) capability set: IDs can only be added/changed by Core; plugins can only be providers in **string form**.
pub fn is_builtin(capability: &str) -> bool {
    CAPABILITY_IDS.contains(&capability)
}

/// Route admission = reserved ∪ frozen declarations (§4.3 access point #1).
/// Declaration miss / storage unavailable → only built-ins count (fail-closed).
pub fn known(capability: &str) -> bool {
    if is_builtin(capability) {
        return true;
    }
    match super::shared_store() {
        Some(store) => super::declaration::declared_capability_ids(&store)
            .iter()
            .any(|c| c == capability),
        None => false,
    }
}

pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

fn providers(capability: &str) -> Vec<String> {
    let Some(store) = super::shared_store() else {
        return Vec::new();
    };
    store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                let mut stmt = c
                    .prepare(
                        "SELECT plugin_id FROM capabilities WHERE id = ?1 AND enabled = 1
                         ORDER BY priority ASC, plugin_id ASC",
                    )
                    .ok()?;
                let it = stmt
                    .query_map([capability], |r| r.get::<_, String>(0))
                    .ok()?;
                Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
            })
        })
        .flatten()
        .unwrap_or_default()
}

fn touch(capability: &str, plugin_id: &str, elapsed_ms: u128) {
    if let Some(store) = super::shared_store() {
        if let Ok(mut s) = store.lock() {
            let _ = s.with_conn(|c| {
                // Running average: new_avg = (old_avg * old_n + new_sample) / (old_n +1)
                // We do not store n, so we approximate with exponential decay α=0.3.
                c.execute(
                    "UPDATE capabilities SET
                        last_used_at = ?3,
                        avg_latency_ms = CASE
                            WHEN avg_latency_ms IS NULL THEN ?4
                            ELSE CAST((avg_latency_ms * 7 + ?4) / 8 AS INTEGER)
                        END
                     WHERE id = ?1 AND plugin_id = ?2",
                    rusqlite::params![
                        capability,
                        plugin_id,
                        super::agent::now_secs(),
                        elapsed_ms as i64,
                    ],
                )
                .unwrap_or(0)
            });
            // Phase 30: also record one raw latency into capability_stats for p50/p95 computation.
            // Phase 31: success path is result='ok', error_kind=None (failures go through record_failure instead).
            s.record_capability_call(
                capability,
                plugin_id,
                elapsed_ms as i64,
                super::agent::now_secs(),
                "ok",
                None,
            );
        }
    }
}

/// Phase 31: failure path recorded separately (result ∈ {err, timeout, denied}, error_kind goes into bucket).
/// EMA is not updated — a failed latency would meaninglessly pollute avg.
fn record_failure(
    capability: &str,
    plugin_id: &str,
    elapsed_ms: u128,
    result: &str,
    error_kind: &str,
) {
    if let Some(store) = super::shared_store() {
        if let Ok(mut s) = store.lock() {
            s.record_capability_call(
                capability,
                plugin_id,
                elapsed_ms as i64,
                super::agent::now_secs(),
                result,
                Some(error_kind),
            );
        }
    }
}

/// Data source for list_capabilities.
pub fn list() -> Value {
    let Some(store) = super::shared_store() else {
        return json!({ "capabilities": [] });
    };
    let rows = store
        .lock()
        .ok()
        .and_then(|s| {
            s.with_conn_ref(|c| {
                let mut stmt = c
                    .prepare(
                        "SELECT id, version, plugin_id, avg_latency_ms, last_used_at
                         FROM capabilities WHERE enabled = 1
                         ORDER BY id ASC, priority ASC",
                    )
                    .ok()?;
                let it = stmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<i64>>(3)?,
                            r.get::<_, Option<i64>>(4)?,
                        ))
                    })
                    .ok()?;
                Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
            })
        })
        .flatten()
        .unwrap_or_default();

    // Aggregate providers
    let mut order: Vec<String> = Vec::new();
    let mut by_cap: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut versions: std::collections::HashMap<String, String> = Default::default();
    let mut latencies: std::collections::HashMap<String, i64> = Default::default();
    let mut last_used: std::collections::HashMap<String, i64> = Default::default();
    for (id, version, plugin, lat, used) in rows {
        if !order.contains(&id) {
            order.push(id.clone());
            versions.insert(id.clone(), version);
            if let Some(l) = lat {
                latencies.insert(id.clone(), l);
            }
            if let Some(u) = used {
                last_used.insert(id.clone(), u);
            }
        }
        by_cap.entry(id).or_default().push(plugin);
    }
    let mut caps: Vec<Value> = order
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "version": versions.get(&id).cloned().unwrap_or_default(),
                "providers": by_cap.get(&id).cloned().unwrap_or_default(),
                "avgLatencyMs": latencies.get(&id).copied().unwrap_or(0),
                "lastUsedAt": last_used.get(&id).copied().unwrap_or(0),
            })
        })
        .collect();
    // Built-in providers are listed too (providers = ["core"]); ones already registered by plugins are not duplicated.
    // Built-ins take a code path outside the Registry, have no measured latency row, and pass through as 0.
    // subscribe-type (file.watch) carries a type marker, so the Agent picks subscribe instead of execute.
    for id in BUILTIN_IDS {
        if !by_cap.contains_key(*id) {
            caps.push(json!({
                "id": id,
                "version": "1",
                "providers": ["core"],
                "avgLatencyMs": 0,
                "lastUsedAt": 0,
            }));
        }
    }
    for id in super::subscription::SUBSCRIBE_IDS {
        if !by_cap.contains_key(*id) {
            caps.push(json!({
                "id": id,
                "version": "1",
                "type": "subscribe",
                "providers": ["core"],
                "avgLatencyMs": 0,
                "lastUsedAt": 0,
            }));
        }
    }
    json!({ "capabilities": caps })
}

/// SessionStart additionalContext digest (docs/mcp.md "Session-start capability injection"):
/// one line per capability from the same data source as list_capabilities, capped at 60
/// entries / 4 KiB so a session start never pays unbounded tokens. None = nothing registered.
pub fn digest_text() -> Option<String> {
    digest_from(&list())
}

/// Pure formatting half of digest_text, so the capping rules are unit-testable
/// without a store.
fn digest_from(listed: &Value) -> Option<String> {
    let caps = listed.get("capabilities").and_then(|c| c.as_array())?;
    if caps.is_empty() {
        return None;
    }
    const MAX_ENTRIES: usize = 60;
    const MAX_BYTES: usize = 4096;
    let mut out = String::from(
        "OpenCapX (desktop layer for agents) is connected. Its MCP tools: opencapx.say, \
         opencapx.notify, opencapx.set_state, opencapx.ask, opencapx.list_capabilities, \
         opencapx.execute, opencapx.subscribe. Capabilities registered right now:",
    );
    let mut shown = 0usize;
    for cap in caps.iter().take(MAX_ENTRIES) {
        let id = cap.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let kind = if cap.get("type").and_then(|v| v.as_str()) == Some("subscribe") {
            "subscribe"
        } else {
            "execute"
        };
        let providers = cap
            .get("providers")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let line = format!("\n- {id} — {kind} — providers: {providers}");
        if out.len() + line.len() > MAX_BYTES {
            break;
        }
        out.push_str(&line);
        shown += 1;
    }
    if shown < caps.len() {
        out.push_str(&format!(
            "\n(+{} more — call opencapx.list_capabilities)",
            caps.len() - shown
        ));
    }
    Some(out)
}

/// Built-in provider dispatch. Agent-layer permission was already checked in rpc::handle; there is no plugin actor here,
/// so it does not go through the plugin-layer gate. Returning None = no built-in implementation for this capability.
fn builtin_dispatch(capability: &str, input: &Value) -> Option<Result<Value, String>> {
    match capability {
        // v1.2: arboard cross-platform clipboard (core/clipboard.rs)
        "clipboard.read" => Some(super::clipboard::read(input)),
        "clipboard.write" => Some(crate::core::clipboard::write(input)),
        "file.read" => Some(builtin_file_read(input)),
        "file.write" => Some(builtin_file_write(input)),
        "file.search" => Some(builtin_file_search(input)),
        // A9: vision/screenshot built-in providers (core/vision.rs); a plugin registering the same name overrides them
        "image.analyze" => Some(super::vision::analyze(input)),
        "image.ocr" => Some(super::vision::ocr(input)),
        "screen.capture" => Some(super::vision::capture(input)),
        // v1.2: platform TTS (core/speech.rs) and browser built-ins (core/browser.rs)
        "speech.synthesize" => Some(super::speech::synthesize(input)),
        "browser.open" => Some(super::browser::open(input)),
        "browser.read" => Some(super::browser::read(input)),
        // A7: Computer Context (core/context.rs)
        "context.get_current" => Some(super::context::current(input)),
        // v1.2: OS permission probing (core/osperm.rs), read-only, gate-exempt
        "system.permission_status" => Some(super::osperm::status(input)),
        // v1.3: app automation / synthetic input / PIM / audio / scheme opening
        "automation.run" => Some(super::appctl::run(input)),
        "input.send" => Some(super::inputctl::send(input)),
        "photos.read" => Some(super::pim::photos(input)),
        "contacts.search" => Some(super::pim::contacts(input)),
        "calendar.events" => Some(super::pim::calendar(input)),
        "location.get" => Some(super::pim::location(input)),
        "audio.play" => Some(super::audio::play(input)),
        "url.scheme.open" => Some(super::browser::open_scheme(input)),
        // v1.4 high/medium-value batch (second batch: personal data + window management)
        "media.playback" => Some(super::media::playback(input)),
        "system.sleep" => Some(super::power::sleep(input)),
        "system.lock" => Some(super::power::lock(input)),
        "system.settings" => Some(super::settings::set(input)),
        "printer.print" => Some(super::printer::print(input)),
        "messages.recent" => Some(super::messages::recent(input)),
        "window.list" => Some(super::windowctl::list(input)),
        "window.focus" => Some(super::windowctl::focus(input)),
        "notes.read" => Some(super::pim::notes(input)),
        "reminders.read" => Some(super::pim::reminders_read(input)),
        "reminders.write" => Some(super::pim::reminders_write(input)),
        "mail.recent" => Some(super::pim::mail(input)),
        _ => None,
    }
}

/// Read-file cap: 2MB (same tier as the browser.read response limit; prevents an Agent from swallowing a giant file in one bite).
const FILE_READ_CAP: u64 = 2 * 1024 * 1024;

/// Read a text file. The path is bounded by scope (core/scope.rs; two-layer enforcement at the execute entry point and
/// the provider loop; scope unconfigured = unrestricted, see permissions.md §Scope).
/// Non-UTF-8 content is honestly reported as an error by read_to_string, never silently lossy.
fn builtin_file_read(input: &Value) -> Result<Value, String> {
    let Some(path) = input.get("path").and_then(|p| p.as_str()) else {
        return Err("invalid input: path (string) required".into());
    };
    let meta = std::fs::metadata(path).map_err(|e| format!("read failed: {}", e))?;
    if !meta.is_file() {
        return Err(format!("not a file: {}", path));
    }
    if meta.len() > FILE_READ_CAP {
        return Err(format!("file too large (max {} bytes)", FILE_READ_CAP));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("read failed: {}", e))?;
    Ok(json!({ "text": text }))
}

/// Write to disk. Does not auto-create parent directories; failures surface the raw IO error.
fn builtin_file_write(input: &Value) -> Result<Value, String> {
    let Some(path) = input.get("path").and_then(|p| p.as_str()) else {
        return Err("invalid input: path (string) required".into());
    };
    let content = input.get("content").and_then(|c| c.as_str()).unwrap_or("");
    std::fs::write(path, content).map_err(|e| format!("write failed: {}", e))?;
    Ok(json!({ "ok": true, "path": path, "bytes": content.len() }))
}

/// Recursive substring search: a name (file/directory) containing pattern is a hit, stack-based DFS, cap 100.
const SEARCH_CAP: usize = 100;
fn builtin_file_search(input: &Value) -> Result<Value, String> {
    let Some(root) = input.get("root").and_then(|p| p.as_str()) else {
        return Err("invalid input: root (string) required".into());
    };
    let pattern = input.get("pattern").and_then(|p| p.as_str()).unwrap_or("");
    let root = std::path::Path::new(root);
    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }
    let mut found: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().contains(pattern) {
                if found.len() >= SEARCH_CAP {
                    truncated = true;
                    break;
                }
                found.push(e.path().to_string_lossy().to_string());
            }
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(e.path());
            }
        }
    }
    found.sort();
    Ok(json!({ "matches": found, "truncated": truncated }))
}

/// S4 — per-provider call timeout: declared value (seconds, 1..=600) wins, otherwise CALL_TIMEOUT (60s).
fn call_timeout_for(
    store: &Option<super::storage::SharedStore>,
    provider: &str,
    capability: &str,
) -> Duration {
    store
        .as_ref()
        .and_then(|st| super::declaration::timeout_for(st, provider, capability))
        .map(|secs| Duration::from_secs(secs as u64))
        .unwrap_or(CALL_TIMEOUT)
}

/// opencapx.execute main path: permission → routing → JSON-RPC → events.
/// agent is the caller identity (from /rpc auth), placed in events for Timeline attribution; None = non-Agent context (tests).
pub fn execute(capability: &str, input: &Value, agent: Option<&str>) -> Result<Value, String> {
    if !known(capability) {
        return Err(format!("unknown capability: {}", capability));
    }
    let bus = super::event::EventBus::shared();
    let mgr = PluginManager::shared();
    let agent = agent.unwrap_or("");

    let provider_list = providers(capability);
    let store = super::shared_store();

    // §4.3 single-point resolver: built-in static mapping ∪ frozen declaration table; no match = None → fail-closed.
    // (Moved ahead of the built-in branch: both scope-gate layers need the same permission name.)
    let permission: Option<String> = store
        .as_ref()
        .and_then(|st| super::declaration::resolve(st, capability))
        .map(|r| r.permission);

    // Path-type scope gate · agent layer (permissions.md §Scope). scope unconfigured = unrestricted;
    // once configured it is closed by default. Denials emit an event + trace as evidence.
    if let Some(st) = store.as_ref() {
        if let Err(layer) = super::scope::enforce(st, capability, input, agent, None) {
            let path = super::scope::path_param(capability)
                .and_then(|p| input.get(p))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            super::req_trace::event(
                "gate.scope_denied",
                json!({ "layer": layer, "agent": agent, "permission": permission, "path": path }),
            );
            bus.publish(&super::event::OpencapxEvent::new(
                "capability.failed",
                "core",
                json!({ "capability": capability, "agent": agent, "error": "scope_denied",
                        "scopeLayer": layer }),
            ));
            record_failure(capability, "core", 0, "denied", "scope_denied");
            return Err(format!(
                "scope_denied:{}:{}",
                layer,
                permission.as_deref().unwrap_or("")
            ));
        }
    }

    if provider_list.is_empty() {
        // No plugin providers → built-in fallback (docs/capability.md "built-in providers").
        // Agent-layer permission was already checked in rpc::handle; there is no plugin actor, so the plugin-layer gate is skipped.
        if let Some(res) = builtin_dispatch(capability, input) {
            bus.publish(&super::event::OpencapxEvent::new(
                "capability.started",
                "core",
                json!({ "capability": capability, "providers": ["core"], "agent": agent }),
            ));
            let call_start = std::time::Instant::now();
            return match res {
                Ok(out) => {
                    let elapsed = call_start.elapsed().as_millis();
                    touch(capability, "core", elapsed);
                    bus.publish(&super::event::OpencapxEvent::new(
                        "capability.completed",
                        "core",
                        json!({
                            "capability": capability,
                            "pluginId": "core",
                            "elapsedMs": elapsed as u64,
                            "agent": agent,
                        }),
                    ));
                    Ok(out)
                }
                Err(e) => {
                    record_failure(
                        capability,
                        "core",
                        call_start.elapsed().as_millis(),
                        "err",
                        "builtin",
                    );
                    bus.publish(&super::event::OpencapxEvent::new(
                        "capability.failed",
                        "core",
                        json!({ "capability": capability, "agent": agent, "attemptedProviders": [
                            { "pluginId": "core", "error": e }
                        ] }),
                    ));
                    Err("capability_failed".into())
                }
            };
        }
        return Err("capability_unavailable".into());
    }

    // S4 — input gate: a payload > 1 MiB after serialization may not enter plugin stdin.
    // (The in-process built-in path skips this gate: it does not use stdin, so large clipboard/file writes are unaffected)
    {
        const MAX_INPUT_BYTES: usize = 1024 * 1024;
        let len = serde_json::to_string(input).map(|s| s.len()).unwrap_or(0);
        if len > MAX_INPUT_BYTES {
            record_failure(capability, "core", 0, "err", "payload_too_large");
            bus.publish(&super::event::OpencapxEvent::new(
                "capability.failed",
                "core",
                json!({
                    "capability": capability,
                    "agent": agent,
                    "error": "payload_too_large",
                    "attemptedProviders": []
                }),
            ));
            return Err("payload_too_large".into());
        }
    }

    // §4.3 single-point resolver was already moved ahead of the built-in branch (see above); do not re-resolve here.

    bus.publish(&super::event::OpencapxEvent::new(
        "capability.started",
        "core",
        json!({ "capability": capability, "providers": provider_list, "agent": agent }),
    ));

    let mut attempted: Vec<Value> = Vec::new();
    for pid in &provider_list {
        let permitted = match (&store, permission.as_deref()) {
            (Some(st), Some(perm)) => {
                super::permission::gate(st, pid, perm, "capability", None)
                    == super::permission::Decision::Granted
            }
            _ => false,
        };
        if !permitted {
            // trace: plugin-layer gate denied (the caller can see "which provider did not run and for what permission";
            // permission null = declaration resolution miss, another form of fail-closed)
            super::req_trace::event(
                "gate.denied",
                json!({ "pluginId": pid, "permission": permission }),
            );
            record_failure(capability, pid, 0, "denied", "permission_denied");
            attempted.push(json!({ "pluginId": pid, "error": "permission_denied" }));
            continue;
        }
        // Path-type scope gate · plugin layer: this provider's scope denies → move to the next provider
        // (Isomorphic to provider fallback; the agent layer already checked at the entry point).
        if let Some(st) = store.as_ref() {
            if let Err(layer) = super::scope::enforce(st, capability, input, agent, Some(pid)) {
                super::req_trace::event(
                    "gate.scope_denied",
                    json!({ "layer": layer, "pluginId": pid, "permission": permission }),
                );
                record_failure(capability, pid, 0, "denied", "scope_denied");
                attempted.push(json!({ "pluginId": pid, "error": "scope_denied" }));
                continue;
            }
        }
        let proc = match mgr.ensure_running(pid) {
            Ok(p) => p,
            Err(e) => {
                record_failure(capability, pid, 0, "err", "ensure_running");
                attempted.push(json!({ "pluginId": pid, "error": e }));
                continue;
            }
        };
        // trace: a single plugin JSON-RPC call (sessionId ties it to the plugin_trace frame-level dump)
        let sp = super::req_trace::span(
            &format!("plugin.{}", pid),
            json!({ "pluginId": pid, "sessionId": proc.session_id() }),
        );
        let call_start = std::time::Instant::now();
        let timeout = call_timeout_for(&store, pid, capability);
        match proc.call(capability, input.clone(), timeout) {
            Ok(out) => {
                let elapsed = call_start.elapsed().as_millis();
                touch(capability, pid, elapsed);
                sp.end(true, None, json!({ "elapsedMs": elapsed as u64 }));
                bus.publish(&super::event::OpencapxEvent::new(
                    "capability.completed",
                    "core",
                    json!({
                        "capability": capability,
                        "pluginId": pid,
                        "elapsedMs": elapsed as u64,
                        "agent": agent,
                    }),
                ));
                return Ok(out);
            }
            Err(e) => {
                let elapsed = call_start.elapsed().as_millis();
                sp.end(false, Some(&e), json!({ "elapsedMs": elapsed as u64 }));
                let (result, kind) = if e.starts_with("timeout ") {
                    ("timeout", "timeout")
                } else {
                    ("err", "rpc_error")
                };
                record_failure(capability, pid, elapsed, result, kind);
                attempted.push(json!({ "pluginId": pid, "error": e }));
            }
        }
    }
    bus.publish(&super::event::OpencapxEvent::new(
        "capability.failed",
        "core",
        json!({ "capability": capability, "agent": agent, "attemptedProviders": attempted }),
    ));
    // All plugin-layer permissions denied: give the caller an explainable error (instead of sinking into capability_failed),
    // the rpc side maps this to 40002 + a remediation hint — key information for the Agent's self-healing surface.
    let all_denied = !attempted.is_empty()
        && attempted
            .iter()
            .all(|a| a.get("error").and_then(|e| e.as_str()) == Some("permission_denied"));
    if all_denied {
        return Err(format!(
            "plugin_permission_denied:{}",
            permission.as_deref().unwrap_or("")
        ));
    }
    Err("capability_failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_formats_and_truncates() {
        // empty registry → no injection at all
        assert!(digest_from(&json!({ "capabilities": [] })).is_none());
        assert!(digest_from(&json!({})).is_none());

        let listed = json!({"capabilities": [
            {"id": "image.analyze", "version": "1", "providers": ["core"]},
            {"id": "file.watch", "version": "1", "type": "subscribe", "providers": ["core"]},
            {"id": "media.play", "version": "2", "providers": ["com.acme.media", "core"]},
        ]});
        let s = digest_from(&listed).unwrap();
        assert!(s.contains("opencapx.execute"), "must teach the entry tools");
        assert!(s.contains("- image.analyze — execute — providers: core"));
        assert!(s.contains("- file.watch — subscribe — providers: core"));
        assert!(s.contains("- media.play — execute — providers: com.acme.media, core"));

        // past the entry cap → truncation marker with the remainder count
        let many: Vec<Value> = (0..70)
            .map(|i| json!({"id": format!("c.{i}"), "version": "1", "providers": ["p"]}))
            .collect();
        let s2 = digest_from(&json!({"capabilities": many})).unwrap();
        assert!(s2.contains("(+10 more"), "70 registered, 60 shown");
        assert!(!s2.contains("- c.69"), "the cut entries must not leak");
    }

    #[test]
    fn v1_registry_is_the_docs_set() {
        // v1.1: +file.watch (first subscribe type), +context.get_current (A7)
        // v1.2: +system.permission_status; v1.3: +8 system surfaces +screen.watch (24)
        // v1.4: +12 (second batch complete: media/power/settings/print + messages/window/PIM extensions)
        // v1.5: +6 Things data surface (plugins only, no built-in fallback)
        assert_eq!(CAPABILITY_IDS.len(), 42);
        assert!(known("image.analyze"));
        assert!(known("clipboard.write"));
        assert!(known("clipboard.read"));
        assert!(known("file.watch"));
        assert!(known("context.get_current"));
        assert!(known("system.permission_status"));
        // v1.3 batch
        for id in [
            "automation.run",
            "input.send",
            "photos.read",
            "contacts.search",
            "calendar.events",
            "location.get",
            "audio.play",
            "url.scheme.open",
        ] {
            assert!(known(id), "{} missing", id);
        }
        assert!(!known("video.analyze"));
        assert!(known("screen.watch"), "v1.3 second subscribe type");
        // v1.4 first batch
        for id in [
            "media.playback",
            "system.sleep",
            "system.lock",
            "system.settings",
            "printer.print",
        ] {
            assert!(known(id), "{} missing", id);
        }
        // v1.4 second batch
        for id in [
            "messages.recent",
            "window.list",
            "window.focus",
            "notes.read",
            "reminders.read",
            "reminders.write",
            "mail.recent",
        ] {
            assert!(known(id), "{} missing", id);
        }
        // v1.5 Things data surface: registered but with no built-in provider (plugin-only)
        for id in [
            "things.add",
            "things.update",
            "things.list",
            "things.show",
            "things.search",
            "things.delete",
        ] {
            assert!(known(id), "{} missing", id);
        }
    }

    /// BUILTIN_IDS stays in sync with the dispatch implementation and the registry.
    #[test]
    fn builtin_ids_dispatch_and_are_known() {
        // Unconditional global side-effect group (real sleep/real lock, no input validation, executes even on empty input):
        // Dispatching once in the test process = the machine immediately sleeps/locks. The real execution path is
        // covered by power.rs's #[ignore] manual test; here we only verify registry sync.
        const SIDE_EFFECT_IDS: &[&str] = &["system.sleep", "system.lock"];

        for id in BUILTIN_IDS {
            assert!(known(id), "{} not in CAPABILITY_IDS", id);
            if SIDE_EFFECT_IDS.contains(&id) {
                continue;
            }
            assert!(
                builtin_dispatch(id, &json!({})).is_some(),
                "{} not dispatchable",
                id
            );
        }
        assert!(
            builtin_dispatch("image.analyze", &json!({})).is_some(),
            "A9:image.analyze has a built-in"
        );
        assert!(
            builtin_dispatch("screen.capture", &json!({})).is_some(),
            "A9:screen.capture has a built-in"
        );
    }

    /// Built-in file.write roundtrip + missing field reports capability_failed (details in the event stream).
    /// TEST_STORE_LOCK: prevents parallel tests' set_shared_store from injecting a store with plugin rows,
    /// where providers() is always empty → taking the built-in path.
    #[test]
    fn builtin_file_write_roundtrip() {
        let _guard = crate::core::TEST_STORE_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("opencapx-bw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        let out = execute(
            "file.write",
            &json!({
                "path": path.to_str().unwrap(), "content": "hello opencapx"
            }),
            None,
        )
        .unwrap();
        assert_eq!(out["ok"], json!(true));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello opencapx");
        // Missing path → the built-in errors, and the protocol surface converges to capability_failed
        assert_eq!(
            execute("file.write", &json!({ "content": "x" }), None),
            Err("capability_failed".into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Built-in file.search: name substring hits, recursion, cap 100 + truncated.
    #[test]
    fn builtin_file_search_finds_and_caps() {
        let _guard = crate::core::TEST_STORE_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("opencapx-bs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("needle-a.txt"), "x").unwrap();
        std::fs::write(dir.join("sub/needle-b.md"), "y").unwrap();
        std::fs::write(dir.join("other.txt"), "z").unwrap();

        let out = execute(
            "file.search",
            &json!({
                "root": dir.to_str().unwrap(), "pattern": "needle"
            }),
            None,
        )
        .unwrap();
        let m = out["matches"].as_array().unwrap();
        assert_eq!(m.len(), 2, "2 hits: {:?}", m);
        assert!(m
            .iter()
            .any(|x| x.as_str().unwrap().contains("needle-a.txt")));
        assert!(m
            .iter()
            .any(|x| x.as_str().unwrap().contains("needle-b.md")));
        assert_eq!(out["truncated"], json!(false));

        // Cap: 120 hit files → 100 + truncated
        for i in 0..120 {
            std::fs::write(dir.join(format!("needle-{:03}.log", i)), "x").unwrap();
        }
        let out = execute(
            "file.search",
            &json!({
                "root": dir.to_str().unwrap(), "pattern": "needle"
            }),
            None,
        )
        .unwrap();
        assert_eq!(out["matches"].as_array().unwrap().len(), SEARCH_CAP);
        assert_eq!(out["truncated"], json!(true));

        // root is not a directory → capability_failed
        assert_eq!(
            execute(
                "file.search",
                &json!({ "root": dir.join("nope").to_str().unwrap() }),
                None
            ),
            Err("capability_failed".into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Built-in clipboard.write end-to-end (macOS): write into the system clipboard, verify with pbpaste,
    /// and restore the user's original clipboard after the test.
    #[cfg(target_os = "macos")]
    #[test]
    fn builtin_clipboard_write_roundtrip() {
        use std::io::Write;
        let _guard = crate::core::TEST_STORE_LOCK.lock().unwrap();
        let saved = std::process::Command::new("pbpaste").output().ok();
        let out = execute(
            "clipboard.write",
            &json!({ "text": "opencapx-test-42" }),
            None,
        )
        .unwrap();
        assert_eq!(out["ok"], json!(true));
        let got = std::process::Command::new("pbpaste")
            .output()
            .unwrap()
            .stdout;
        assert_eq!(String::from_utf8_lossy(&got), "opencapx-test-42");
        // Restore
        if let Some(s) = saved {
            if let Ok(mut child) = std::process::Command::new("pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(&s.stdout);
                }
                let _ = child.wait();
            }
        }
    }

    /// Built-in file.read: normal read + missing path / nonexistent file / over 2MB → capability_failed.
    #[test]
    fn builtin_file_read_roundtrip_and_caps() {
        let _guard = crate::core::TEST_STORE_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("opencapx-fr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("readme.txt");
        std::fs::write(&path, "café opencapx").unwrap();

        let out = execute(
            "file.read",
            &json!({ "path": path.to_str().unwrap() }),
            None,
        )
        .unwrap();
        assert_eq!(out["text"], json!("café opencapx"));

        // Missing path / nonexistent file → capability_failed
        assert_eq!(
            execute("file.read", &json!({}), None),
            Err("capability_failed".into())
        );
        assert_eq!(
            execute(
                "file.read",
                &json!({ "path": dir.join("nope.txt").to_str().unwrap() }),
                None
            ),
            Err("capability_failed".into())
        );

        // Over 2MB → capability_failed
        let big = dir.join("big.bin");
        std::fs::write(&big, vec![0u8; (FILE_READ_CAP + 1) as usize]).unwrap();
        assert_eq!(
            execute("file.read", &json!({ "path": big.to_str().unwrap() }), None),
            Err("capability_failed".into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Built-in clipboard.read end-to-end (macOS): write → read roundtrip via arboard,
    /// restoring the user's original clipboard after the test (save/restore via arboard, no longer relying on pbpaste/pbcopy).
    #[cfg(target_os = "macos")]
    #[test]
    fn builtin_clipboard_read_text_roundtrip() {
        let _guard = crate::core::TEST_STORE_LOCK.lock().unwrap();
        // Save the original clipboard: raw arboard still needs the lock, but it is released within the block scope to avoid deadlocking with execute's internal lock
        let saved = {
            let _g = crate::core::clipboard::clipboard_lock_guard();
            arboard::Clipboard::new().and_then(|mut c| c.get_text())
        };
        execute(
            "clipboard.write",
            &json!({ "text": "opencapx-read-42" }),
            None,
        )
        .unwrap();
        let out = execute("clipboard.read", &json!({}), None).unwrap();
        assert_eq!(out["text"], json!("opencapx-read-42"));
        if let Ok(t) = saved {
            let _ = crate::core::clipboard::write(&json!({ "text": t }));
        }
    }

    /// Built-in clipboard.read image path (macOS): a 2×2 image into the clipboard →
    /// read returns a PNG on-disk path, decode and assert the dimensions; restore after the test.
    #[cfg(target_os = "macos")]
    #[test]
    fn builtin_clipboard_read_image_roundtrip() {
        use arboard::ImageData;
        let _guard = crate::core::TEST_STORE_LOCK.lock().unwrap();
        let saved_text = {
            let _g = crate::core::clipboard::clipboard_lock_guard();
            arboard::Clipboard::new().and_then(|mut c| c.get_text())
        };
        // 2×2 pure red RGBA
        {
            let _g = crate::core::clipboard::clipboard_lock_guard();
            let mut cb = arboard::Clipboard::new().unwrap();
            cb.set_image(ImageData {
                width: 2,
                height: 2,
                bytes: [255u8, 0, 0, 255].repeat(4).into(),
            })
            .unwrap();
        }
        let out = execute("clipboard.read", &json!({}), None).unwrap();
        assert_eq!(out["text"], json!(""));
        let p = out["image"].as_str().expect("image path").to_string();
        let img = image::open(&p).expect("decode clipboard png");
        assert_eq!((img.width(), img.height()), (2, 2));
        let _ = std::fs::remove_file(&p);
        if let Ok(t) = saved_text {
            let _ = crate::core::clipboard::write(&json!({ "text": t }));
        }
    }

    /// A capability with no plugin provider and no built-in implementation (like audio.transcribe) stays
    /// capability_unavailable — the built-in fallback does not change the "plugin-only capability" semantics.
    #[test]
    fn no_builtin_capability_stays_unavailable() {
        let _guard = crate::core::TEST_STORE_LOCK.lock().unwrap();
        assert_eq!(
            execute("audio.transcribe", &json!({}), None),
            Err("capability_unavailable".into())
        );
    }

    /// A9:image.analyze built-in path, input validation failure → capability_failed
    /// (Details are in the event stream's attemptedProviders; the real Ollama call is tested in vision.rs).
    #[test]
    fn builtin_image_analyze_validates_input() {
        let _guard = crate::core::TEST_STORE_LOCK.lock().unwrap();
        assert_eq!(
            execute("image.analyze", &json!({}), None),
            Err("capability_failed".into())
        );
    }

    #[test]
    fn execute_unknown_capability_rejected() {
        let err = execute("nope.nope", &json!({}), None).unwrap_err();
        assert!(err.contains("unknown capability"));
    }

    /// Pure-function test of the EMA step logic (no global shared_store needed).
    #[test]
    fn ema_step_formula_is_correct() {
        // (old * 7 + new) / 8
        let step = |old: i64, new: i64| -> i64 { (old * 7 + new) / 8 };
        assert_eq!(step(100, 100), 100);
        assert_eq!(step(100, 50), 93); // (700+50)/8 = 93
        assert_eq!(step(0, 80), 10); // first hit still weighted at 1/8
    }

    /// S4 — per-provider timeout resolution: no store/no declaration → default 60s.
    /// (The declared-value branch is verified against a real declaration DB in plugin.rs integration tests.)
    #[test]
    fn call_timeout_defaults_without_declaration() {
        assert_eq!(call_timeout_for(&None, "com.x", "a.b"), CALL_TIMEOUT);
    }
}
