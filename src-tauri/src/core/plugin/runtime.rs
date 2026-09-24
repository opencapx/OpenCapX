//! Runtime: start gates and spawn, auto-reload, health/watchdog threads, stop.
//! Mechanical move from core/plugin.rs.

use super::*;

/// Start the auto-reload polling thread (singleton, idempotent). Checks all auto_reload=1 plugins every 3s.
pub fn spawn_auto_reload_poller() {
    use std::sync::OnceLock;
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    std::thread::spawn(auto_reload_loop);
}

/// Background polling: for auto_reload=1 plugins, a manifest mtime change → stop+start, and emit plugin.auto_reloaded.
fn auto_reload_loop() {
    let mut restored = false;
    loop {
        std::thread::sleep(Duration::from_secs(3));
        let mgr = self_ref();
        if !restored {
            if let Some(n) = mgr.restore_auto_reload() {
                restored = true;
                if n > 0 {
                    eprintln!("[plugin] auto-reload restored for {} plugin(s)", n);
                }
            }
        }
        // per-manager set: does not depend on the global shared_store (avoiding collisions with other plugin tests in parallel)
        let ids: Vec<String> = match mgr.auto_reload_set.lock() {
            Ok(s) => s.iter().cloned().collect(),
            Err(_) => continue,
        };
        // query path from the shared store (read-only, no writes)
        let Some(store) = PluginManager::store() else {
            continue;
        };
        let tracked: Vec<(String, String)> = match store.lock() {
            Ok(s) => {
                let mut out = Vec::new();
                for id in &ids {
                    let path = s.with_conn_ref(|c| {
                        c.query_row("SELECT path FROM plugins WHERE id = ?1", [id], |r| {
                            r.get::<_, String>(0)
                        })
                        .ok()
                    });
                    if let Some(Some(p)) = path {
                        out.push((id.clone(), p));
                    }
                }
                out
            }
            Err(_) => continue,
        };
        for (id, path) in tracked {
            let manifest_path = std::path::Path::new(&path).join("opencapx-plugin.json");
            let meta = match std::fs::metadata(&manifest_path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = match meta.modified() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let changed = mgr
                .last_mtimes
                .lock()
                .ok()
                .and_then(|m| m.get(&id).copied())
                .map(|last| mtime > last)
                .unwrap_or(true);
            if !changed {
                continue;
            }
            // stop and restart with reset=false (avoiding a watchdog cumulative-count trigger)
            mgr.stop(&id);
            if mgr.start_inner(&id, true).is_ok() {
                crate::core::event::EventBus::shared().publish(
                    &crate::core::event::OpencapxEvent::new(
                        "plugin.auto_reloaded",
                        "core",
                        json!({ "pluginId": &id }),
                    ),
                );
            }
        }
    }
}

fn self_ref() -> Arc<PluginManager> {
    PluginManager::shared()
}

/// Watchdog: polls the plugin child process; on an unexpected death it restarts with exponential backoff (1s, 2s, 4s),
/// and after exceeding WATCHDOG_MAX_RETRIES it disables the plugin and emits plugin.state_changed="error".
/// A manual stop() first clears the enabled flag; this loop exits immediately when it sees enabled=false.
fn watchdog_loop(mgr: Arc<PluginManager>, id: String, wd: Arc<Mutex<Watchdog>>) {
    loop {
        // Phase 40 — the tick interval is decided by cfg.heartbeat_sec; 0 = ping off, still using the 500ms is_alive check.
        let heartbeat_ms = wd
            .lock()
            .map(|w| {
                if w.cfg.heartbeat_sec == 0 {
                    WATCHDOG_TICK_MS
                } else {
                    (w.cfg.heartbeat_sec as u64)
                        .saturating_mul(1000)
                        .max(WATCHDOG_TICK_MS)
                }
            })
            .unwrap_or(WATCHDOG_TICK_MS);
        std::thread::sleep(Duration::from_millis(heartbeat_ms));
        let enabled = wd.lock().map(|w| w.enabled).unwrap_or(false);
        if !enabled {
            return;
        }
        let alive = mgr
            .procs
            .lock()
            .ok()
            .and_then(|m| m.get(&id).cloned())
            .map(|p| p.is_alive())
            .unwrap_or(false);

        // Phase 40 — when heartbeat_sec > 0, do a ping first; a ping timeout = treated as alive=false and triggers the retry path.
        if alive {
            let heartbeat_sec = wd.lock().map(|w| w.cfg.heartbeat_sec).unwrap_or(0);
            if heartbeat_sec > 0 {
                let timeout_ms = wd.lock().map(|w| w.cfg.ping_timeout_ms).unwrap_or(1000);
                let ping_ok = mgr
                    .procs
                    .lock()
                    .ok()
                    .and_then(|m| m.get(&id).cloned())
                    .map(|p| p.ping(std::time::Duration::from_millis(timeout_ms as u64)))
                    .unwrap_or(false);
                if ping_ok {
                    continue;
                }
                // ping failure → takes the same path as process death
            } else {
                continue;
            }
        }
        if alive {
            continue;
        }
        // the process died. Check whether our own stop caused it.
        let still_tracked = mgr
            .watchdogs
            .lock()
            .ok()
            .and_then(|m| m.get(&id).cloned())
            .is_some();
        if !still_tracked {
            return;
        }
        // count + backoff (cumulative across restarts; reset by start())
        let retry = {
            let w = match wd.lock() {
                Ok(w) => w,
                Err(_) => return,
            };
            if !w.enabled {
                return;
            }
            drop(w);
            crate::core::event::EventBus::shared().publish(
                &crate::core::event::OpencapxEvent::new(
                    "plugin.lifecycle.crashed",
                    "core",
                    json!({ "pluginId": &id, "reason": "process exited unexpectedly" }),
                ),
            );
            let mut counts = match mgr.retry_counts.lock() {
                Ok(c) => c,
                Err(_) => return,
            };
            let n = counts.get(&id).copied().unwrap_or(0) + 1;
            counts.insert(id.clone(), n);
            // Phase 40 — max_retries comes from cfg; 0 disables the watchdog outright; max_retries=3 matches the Phase 10 behavior.
            let max_retries = wd
                .lock()
                .map(|w| w.cfg.max_retries)
                .unwrap_or(WATCHDOG_MAX_RETRIES);
            if max_retries == 0 || n > max_retries {
                PluginManager::set_status(&id, "error");
                crate::core::event::EventBus::shared().publish(
                    &crate::core::event::OpencapxEvent::new(
                        "plugin.watchdog_disabled",
                        "core",
                        json!({ "pluginId": &id, "reason": "max retries exceeded" }),
                    ),
                );
                counts.remove(&id);
                drop(counts);
                if let Ok(mut m) = mgr.watchdogs.lock() {
                    m.remove(&id);
                }
                return;
            }
            n
        };
        // Phase 40 — backoff comes from cfg (default 1s, doubling, capped at 30s).
        let backoff_initial = wd.lock().map(|w| w.cfg.backoff_initial_ms).unwrap_or(1000);
        let backoff_ms = crate::core::health::compute_backoff_ms(retry, backoff_initial);
        std::thread::sleep(Duration::from_millis(backoff_ms));
        // re-confirm enabled (start may have already reset the watchdog)
        let still_enabled = wd.lock().map(|w| w.enabled).unwrap_or(false);
        if !still_enabled {
            return;
        }
        crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
            "plugin.restarting",
            "core",
            json!({ "pluginId": &id, "retry": retry }),
        ));
        if mgr.start_inner(&id, false).is_ok() {
            // start_inner calls register_watchdog() to replace us with a new enabled=true watchdog,
            // so here it is enough to exit the old loop.
            return;
        }
        // start failed: leave it for the next round to decide
    }
}

impl PluginManager {
    /// Audit event for a rejected start (same shape as kill_switch::guard_start's plugin.start.rejected).
    fn reject_start(id: &str, reason: &str) {
        crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
            "plugin.start.rejected",
            "core",
            json!({ "pluginId": id, "reason": reason }),
        ));
    }

    /// Start: spawn → initialize handshake → capabilities written to the registry → running.
    pub fn start(&self, id: &str) -> Result<(), String> {
        self.start_inner(id, true)
    }

    fn start_inner(&self, id: &str, reset_retry: bool) -> Result<(), String> {
        // Phase 44 — global kill switch gatekeeper. When active, return Err directly and do not start the process.
        crate::core::kill_switch::guard_start(id)?;
        // --safe-mode: startup diagnostic mode; no third-party plugin is started (manual start is likewise rejected).
        crate::core::safe_mode::guard_start(id)?;
        // F6 — revocation default-disable: a plugin hit by revokedKeys is forbidden to start until the user explicitly reopens it
        // (reopen clears revoked_key and records an ack; here we only look at the current disable marker).
        if let Some(key) = Self::revoked_key_of(id) {
            let reason = format!(
                "plugin revoked: publisher key {} (reopen explicitly to run)",
                key
            );
            Self::reject_start(id, &reason);
            return Err(reason);
        }
        if let Some(p) = self.procs.lock().ok().and_then(|m| m.get(id).cloned()) {
            if p.is_alive() {
                return Ok(());
            }
        }
        let (dir, m) = Self::row_manifest(id)?;
        // F4/F5 — start gate: forbid the start when compatibility and dependencies are not satisfied. The rejection happens before the state-machine
        // transition (starting is not set), and reuses kill_switch's plugin.start.rejected event shape.
        if let Err(e) = Self::check_core_compat(&m) {
            Self::reject_start(id, &e);
            return Err(e);
        }
        let installed: Vec<(String, String)> = Self::installed_versions();
        let missing = crate::core::plugin_deps::find_missing(&m.dependencies, &installed);
        if !missing.is_empty() {
            let detail = missing
                .iter()
                .map(|(d, r)| format!("{} ({})", d, r))
                .collect::<Vec<_>>()
                .join(", ");
            let e = format!("plugin_dependency_missing: requires {}", detail);
            Self::reject_start(id, &e);
            return Err(e);
        }
        Self::set_status(id, "starting");
        crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
            "plugin.lifecycle.starting",
            "core",
            json!({ "pluginId": id }),
        ));
        let runtime = m
            .runtime
            .clone()
            .ok_or_else(|| "pet plugin has no runtime".to_string())?;
        if runtime.rtype != "process" {
            let err = format!("unsupported runtime type {}", runtime.rtype);
            crate::core::event::EventBus::shared().publish(
                &crate::core::event::OpencapxEvent::new(
                    "plugin.lifecycle.crashed",
                    "core",
                    json!({ "pluginId": id, "reason": &err }),
                ),
            );
            return Err(err);
        }
        let spec = RuntimeSpec {
            command: runtime.command,
            args: runtime.args,
            env: runtime.env,
        };
        let plugin_id = id.to_string();
        let on_reverse: crate::core::process::OnReverse =
            Arc::new(move |v: serde_json::Value, reply| {
                handle_reverse(&plugin_id, v, reply);
            });
        // Wow 9: one trace session per start, session_id = now_secs + 4 random digits to avoid collisions from simultaneous starts.
        let session_id = format!(
            "{}-{:04x}",
            crate::core::plugin_trace::now_secs(),
            (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0))
                & 0xFFFF
        );
        // S1 — environment policy: isolated by default (minimal allowlist only) + the settings incremental allowlist; one switch falls back.
        let env_policy = crate::core::process::EnvPolicy {
            isolate: env_isolation_enabled(),
            allow: env_allowlist(),
        };
        // S5b/S5c — sandbox enforcement: declaration + (trusted ? switch : forced); None on non-macOS.
        let trusted = manifest_signature_key_id(&m)
            .map(|kid| crate::core::plugin_sig::load_trusted_keys().contains_key(&kid))
            .unwrap_or(false);
        let sandbox_spec = crate::core::sandbox::effective_profile(
            id,
            m.sandbox.as_ref(),
            trusted,
            sandbox_enforcement_enabled(),
        )
        .map(|profile| {
            // review F2 — the tmp directory is created alongside the data directory (the TMPDIR redirection target).
            let tmp_dir = crate::core::sandbox::plugin_tmp_dir(id);
            let _ = std::fs::create_dir_all(&tmp_dir);
            crate::core::process::SandboxSpec { profile, tmp_dir }
        });
        let proc = PluginProcess::spawn(
            id,
            &dir,
            &spec,
            &session_id,
            on_reverse,
            &env_policy,
            sandbox_spec.as_ref(),
        )
        .map_err(|e| format!("spawn failed: {}", e))?;
        let proc = Arc::new(proc);
        // M7/F9 — inject previousVersion (the version of the last successful start; omitted on first start),
        // so the plugin can perform idempotent data migration itself (the host neither moves data nor runs migration scripts).
        let previous_version = Self::last_version_of(id);
        let mut init_payload = json!({
            "coreVersion": env!("CARGO_PKG_VERSION"),
            "apiVersion": API_VERSION,
            "pluginId": id
        });
        if let Some(prev) = &previous_version {
            init_payload["previousVersion"] = serde_json::Value::String(prev.clone());
        }
        let init = proc
            .call("plugin.initialize", init_payload, Duration::from_secs(10))
            .map_err(|e| {
                Self::set_status(id, "error");
                crate::core::event::EventBus::shared().publish(
                    &crate::core::event::OpencapxEvent::new(
                        "plugin.lifecycle.crashed",
                        "core",
                        json!({ "pluginId": id, "reason": format!("initialize failed: {}", e) }),
                    ),
                );
                format!("initialize failed: {}", e)
            })?;
        let handshake_id = init.get("pluginId").and_then(|x| x.as_str()).unwrap_or("");
        if handshake_id != id {
            Self::set_status(id, "error");
            let _ = proc.notify("plugin.shutdown", json!({}));
            let reason = format!(
                "handshake pluginId mismatch: manifest {} vs handshake {}",
                id, handshake_id
            );
            crate::core::event::EventBus::shared().publish(
                &crate::core::event::OpencapxEvent::new(
                    "plugin.lifecycle.crashed",
                    "core",
                    json!({ "pluginId": id, "reason": &reason }),
                ),
            );
            return Err(reason);
        }
        // M7/F9 — a successful handshake = a successful start: record the version for injecting previousVersion on the next start.
        Self::set_last_version(id, &m.version);
        // capabilities registry
        if let Some(store) = Self::store() {
            if let Ok(mut s) = store.lock() {
                let _ = s.with_conn(|c| {
                    let mut n = 0;
                    for cap in &m.capability_ids() {
                        n = c
                            .execute(
                                "INSERT INTO capabilities (id, version, plugin_id, priority, enabled)
                                 VALUES (?1, '1', ?2, 100, 1)
                                 ON CONFLICT(id, plugin_id) DO UPDATE SET enabled = 1",
                                params![cap, id],
                            )
                            .unwrap_or(0);
                    }
                    n
                });
            }
        }
        if let Ok(mut m) = self.procs.lock() {
            m.insert(id.to_string(), proc);
        }
        Self::set_status(id, "running");
        crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
            "plugin.lifecycle.running",
            "core",
            json!({ "pluginId": id }),
        ));
        // a manual start resets the cumulative retries; a watchdog-triggered restart preserves the cumulative count.
        if reset_retry {
            if let Ok(mut m) = self.retry_counts.lock() {
                m.insert(id.to_string(), 0);
            }
        }
        // record the manifest mtime for auto-reload polling
        if let Ok((path, _)) = Self::row_manifest(id) {
            let manifest = path.join("opencapx-plugin.json");
            if let Ok(meta) = std::fs::metadata(&manifest) {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(mut m) = self.last_mtimes.lock() {
                        m.insert(id.to_string(), mtime);
                    }
                }
            }
        }
        self.register_watchdog(id);
        Ok(())
    }

    /// Toggle auto-reload. Can be called at runtime; once true, a background poller watches the manifest mtime.
    pub fn set_auto_reload(&self, id: &str, on: bool) -> Result<(), String> {
        let Some(store) = Self::store() else {
            return Err("storage unavailable".into());
        };
        let mut s = store.lock().map_err(|_| "poisoned".to_string())?;
        s.with_conn(|c| {
            c.execute(
                "UPDATE plugins SET auto_reload = ?2 WHERE id = ?1",
                params![id, on as i64],
            )
            .unwrap_or(0)
        });
        // per-manager set: the poller no longer depends on the shared store
        if let Ok(mut set) = self.auto_reload_set.lock() {
            if on {
                set.insert(id.to_string());
            } else {
                set.remove(id);
            }
        }
        Ok(())
    }

    /// Startup backfill: re-enroll plugins with auto_reload=1 from the DB into the polling set,
    /// using the current manifest mtime as the baseline — otherwise the first tick after a restart would
    /// indiscriminately stop+start every plugin that was ever checked.
    /// `None` = storage unavailable / query failed; `Some(count)` = success, where count is the number backfilled.
    pub fn restore_auto_reload(&self) -> Option<usize> {
        let store = Self::store()?;
        let rows: Vec<(String, String)> = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    let mut st = c
                        .prepare("SELECT id, path FROM plugins WHERE auto_reload = 1")
                        .ok()?;
                    let it = st
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                        .ok()?;
                    Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
                })
            })
            .flatten()?;
        let mut n = 0;
        if let Ok(mut set) = self.auto_reload_set.lock() {
            for (id, path) in rows {
                set.insert(id.clone());
                if let Ok(m) =
                    std::fs::metadata(std::path::Path::new(&path).join("opencapx-plugin.json"))
                {
                    if let Ok(t) = m.modified() {
                        if let Ok(mut lm) = self.last_mtimes.lock() {
                            lm.insert(id, t);
                        }
                    }
                }
                n += 1;
            }
        }
        Some(n)
    }

    /// Phase 40 — apply cfg to the running watchdog:
    /// - enabled=false → set the current watchdog's enabled=false and the old loop exits naturally.
    /// - enabled=true and the plugin is running → remove the old watchdog and register_watchdog rebuilds it with the new cfg.
    /// - plugin not running → do nothing (the next start_inner reads the new cfg from the store).
    pub fn apply_health_config(&self, id: &str, cfg: &crate::core::health::PluginHealthConfig) {
        if !cfg.enabled {
            if let Ok(m) = self.watchdogs.lock() {
                if let Some(wd) = m.get(id).cloned() {
                    if let Ok(mut w) = wd.lock() {
                        w.enabled = false;
                    }
                }
            }
            return;
        }
        let running = self
            .procs
            .lock()
            .ok()
            .map(|p| p.contains_key(id))
            .unwrap_or(false);
        if running {
            if let Ok(mut m) = self.watchdogs.lock() {
                // Q1 — disable the old thread before removing it: otherwise the old thread holds an Arc with a stale cfg and keeps monitoring
                // (each health-config save = +1 duplicate monitor thread). Same as the stop()/disable branch.
                if let Some(wd) = m.get(id).cloned() {
                    if let Ok(mut w) = wd.lock() {
                        w.enabled = false;
                    }
                }
                m.remove(id);
            }
            self.register_watchdog(id);
        }
    }

    /// Register/reset the watchdog and spawn the monitor thread. Idempotent.
    fn register_watchdog(&self, id: &str) {
        // Phase 40 — read the per-plugin config from the store; snapshot it at register time to avoid hitting the DB in later loops.
        let cfg = Self::store()
            .and_then(|s| s.lock().ok().map(|g| g.get_health_config(id)))
            .unwrap_or_default();
        let wd = Arc::new(Mutex::new(Watchdog::new(cfg)));
        if let Ok(mut m) = self.watchdogs.lock() {
            m.insert(id.to_string(), wd.clone());
        }
        let id_owned = id.to_string();
        let mgr = self_ref();
        std::thread::spawn(move || watchdog_loop(mgr, id_owned, wd));
    }

    pub fn get_process(&self, id: &str) -> Option<Arc<PluginProcess>> {
        self.procs.lock().ok().and_then(|m| m.get(id).cloned())
    }

    pub fn stop(&self, id: &str) {
        // disable the watchdog first, so it does not misjudge our shutdown as a crash and restart
        if let Ok(mut m) = self.watchdogs.lock() {
            if let Some(w) = m.get(id) {
                if let Ok(mut w) = w.lock() {
                    w.enabled = false;
                }
            }
            m.remove(id);
        }
        if let Some(p) = self.procs.lock().ok().and_then(|mut m| m.remove(id)) {
            // don't try_unwrap: in-flight calls / the watchdog may still hold Arc clones
            p.shutdown();
        }
        // remove from the registry
        if let Some(store) = Self::store() {
            if let Ok(mut s) = store.lock() {
                let _ = s.with_conn(|c| {
                    c.execute(
                        "UPDATE capabilities SET enabled = 0 WHERE plugin_id = ?1",
                        [id],
                    )
                    .unwrap_or(0)
                });
            }
        }
        Self::set_status(id, "stopped");
        crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
            "plugin.lifecycle.stopped",
            "core",
            json!({ "pluginId": id }),
        ));
    }

    /// Phase 46 — called on a profile switch: stops all running plugins and returns the list of stopped plugin_ids.
    /// Does not affect config / sqlite rows; it is just a clean shutdown.
    pub fn stop_all(&self) -> Vec<String> {
        let ids: Vec<String> = match self.procs.lock() {
            Ok(m) => m.keys().cloned().collect(),
            Err(_) => Vec::new(),
        };
        for id in &ids {
            self.stop(id);
        }
        ids
    }
}
