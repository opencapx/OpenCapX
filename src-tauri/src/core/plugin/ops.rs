//! Lifecycle operations: uninstall, dependency graph, toggle, list, ensure_running.
//! Mechanical move from core/plugin.rs.

use super::*;

impl PluginManager {
    /// Uninstall a plugin: stop the process + delete the config file + clear sqlite rows (plugin / permissions /
    /// capabilities / **declaration + domain release**) + emit a `plugin.uninstalled` event.
    /// Failure-tolerant (clean up as much as possible).
    pub fn uninstall(&self, id: &str) -> Result<(), String> {
        let _install_guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // stop first, so the watchdog / restart loop does not bring the process back while we clean up
        self.stop(id);
        // remove from the auto_reload set
        if let Ok(mut set) = self.auto_reload_set.lock() {
            set.remove(id);
        }
        // delete the config file (ignore "not found" errors)
        let _ = crate::core::config::reset(id);
        // M7/F8 — the secret fallback file is cleaned up with the uninstall (keychain entries cannot be enumerated; see the config comment).
        let _ = crate::core::config::forget_plugin_secrets(id);
        // M8 hardening (review tightening) — delete managed plugin directories: strict equality check (path == plugins root/<id>,
        // i.e. the copy left by a .ocplugin install). Prefix matching does not guard against ".." components (Path::starts_with
        // does not normalize), and directory installs (dev/test) may write an arbitrary path to the DB — neither should be deleted.
        // read the path first; only delete files after the DB row is successfully removed (see below, closing the "row without file" window).
        let managed_path: Option<PathBuf> = Self::row_manifest(id).ok().and_then(|(path, _)| {
            if path == Self::plugins_root().join(id) {
                Some(path)
            } else {
                None
            }
        });
        // delete the plugin / permissions / capabilities tables
        let mut removed_row = false;
        if let Some(store) = Self::store() {
            if let Ok(mut s) = store.lock() {
                removed_row = s
                    .with_conn(|c| {
                        c.execute("DELETE FROM plugin_permissions WHERE plugin_id = ?1", [id])
                            .unwrap_or(0)
                            + c.execute("DELETE FROM capabilities WHERE plugin_id = ?1", [id])
                                .unwrap_or(0)
                            + c.execute("DELETE FROM plugins WHERE id = ?1", [id])
                                .unwrap_or(0)
                    })
                    .map(|n| n > 0)
                    .unwrap_or(false);
            }
            // §4.6 uninstall release: delete the frozen declaration + release the domain (the tombstone prompt belongs to the UI.
            // Reinstall = a fresh install with full confirmation; it does not accept implicit renewal of the old frozen state)
            let _ = crate::core::declaration::delete_for_plugin(&store, id);
        }
        // Only clear files after the DB row is truly deleted: avoid "row without file (startup always fails)"; the reverse "file without row" is harmless.
        if removed_row {
            if let Some(path) = managed_path {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
        crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
            "plugin.uninstalled",
            "core",
            json!({ "pluginId": id }),
        ));
        // Phase 53 — uninstall the manifest-origin severity hints.
        crate::core::alerting::uninstall_manifest_hints(id);
        Ok(())
    }

    /// Wow 7 — uninstall preview. Deletes nothing; only collects the metadata that would be cleared +
    /// detects capability overlap with other plugins (dependency warning).
    pub fn uninstall_preview(&self, id: &str) -> Result<UninstallPreviewDto, String> {
        // find this plugin: list() already includes auto_reload + capabilities + status
        let me = self
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("plugin not installed: {}", id))?;
        // whether the config file exists
        let config_path = crate::core::config::config_path(id);
        let config_exists = config_path.exists();
        // permission count (queried from sqlite)
        let permission_count = Self::store()
            .and_then(|s| {
                s.lock().ok().and_then(|mut g| {
                    g.with_conn(|c| {
                        c.query_row(
                            "SELECT COUNT(*) FROM plugin_permissions WHERE plugin_id = ?1",
                            [id],
                            |r| r.get::<_, i64>(0),
                        )
                        .unwrap_or(0) as usize
                    })
                })
            })
            .unwrap_or(0);
        let capability_count = me.capabilities.len();
        // dependents: ids of other installed plugins whose manifest contains any of this plugin's capabilities
        let mut dependents: Vec<String> = Vec::new();
        if !me.capabilities.is_empty() {
            for other in self.list() {
                if other.id == id {
                    continue;
                }
                if other
                    .capabilities
                    .iter()
                    .any(|c| me.capabilities.contains(c))
                {
                    dependents.push(other.id);
                }
            }
        }
        Ok(UninstallPreviewDto {
            id: me.id,
            name: me.name,
            version: me.version,
            auto_reload: me.auto_reload,
            config_exists,
            permission_count,
            capability_count,
            dependents,
        })
    }

    /// Phase 32 — all installed plugins + edges for shared capabilities. For the frontend SVG node-edge chart.
    /// O(n²), but n≤20 is usually fine and the shared-capability count is far smaller than the edge count.
    pub fn capability_dependency_graph(&self) -> CapabilityGraphDto {
        let plugins = self.list();
        let mut nodes: Vec<CapabilityGraphNodeDto> = plugins
            .iter()
            .map(|p| CapabilityGraphNodeDto {
                id: p.id.clone(),
                name: p.name.clone(),
                capabilities: p.capabilities.clone(),
            })
            .collect();
        let mut edges: Vec<CapabilityGraphEdgeDto> = Vec::new();
        for i in 0..plugins.len() {
            for j in (i + 1)..plugins.len() {
                let a = &plugins[i];
                let b = &plugins[j];
                let shared: Vec<String> = a
                    .capabilities
                    .iter()
                    .filter(|c| b.capabilities.contains(c))
                    .cloned()
                    .collect();
                if shared.is_empty() {
                    continue;
                }
                // sort from/to by id lexicographically to keep edges stable
                let (from, to) = if a.id <= b.id {
                    (a.id.clone(), b.id.clone())
                } else {
                    (b.id.clone(), a.id.clone())
                };
                edges.push(CapabilityGraphEdgeDto { from, to, shared });
            }
        }
        // nodes sorted by id, keeping the frontend layout stable
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        CapabilityGraphDto { nodes, edges }
    }

    /// F5 — all `(dependent, dep)` pairs (read from DB rows; rows that fail to parse are skipped).
    pub fn dependency_edges(&self) -> Vec<(String, String)> {
        let Some(store) = Self::store() else {
            return Vec::new();
        };
        store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    let mut st = c.prepare("SELECT id, manifest FROM plugins").ok()?;
                    let it = st
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                        .ok()?;
                    let mut out = Vec::new();
                    for (id, json) in it.filter_map(|x| x.ok()) {
                        if let Ok(dm) = serde_json::from_str::<Manifest>(&json) {
                            for dep in dm.dependencies.keys() {
                                out.push((id.clone(), dep.clone()));
                            }
                        }
                    }
                    Some(out)
                })
            })
            .flatten()
            .unwrap_or_default()
    }

    pub fn toggle(&self, id: &str) -> Result<bool, String> {
        let running = self
            .procs
            .lock()
            .ok()
            .and_then(|m| m.get(id).cloned())
            .map(|p| p.is_alive())
            .unwrap_or(false);
        if running {
            self.stop(id);
            Ok(false)
        } else {
            self.start(id)?;
            Ok(true)
        }
    }

    pub fn list(&self) -> Vec<PluginStatusDto> {
        let Some(store) = Self::store() else {
            return Vec::new();
        };
        let rows = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    let mut stmt = c
                        .prepare("SELECT id, manifest, status, auto_reload, probe_status, probe_at, revoked_key, revoked_at FROM plugins")
                        .ok()?;
                    let it = stmt
                        .query_map([], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                                r.get::<_, i64>(3)?,
                                r.get::<_, Option<String>>(4)?,
                                r.get::<_, Option<i64>>(5)?,
                                r.get::<_, Option<String>>(6)?,
                                r.get::<_, Option<i64>>(7)?,
                            ))
                        })
                        .ok()?;
                    Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
                })
            })
            .flatten()
            .unwrap_or_default();
        // F5 — fetch all (id, version) at once, so each manifest can check dependency satisfaction in memory (avoiding N+1).
        let installed: Vec<(String, String)> = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    let mut st = c.prepare("SELECT id, version FROM plugins").ok()?;
                    let it = st
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                        .ok()?;
                    Some(it.filter_map(|x| x.ok()).collect::<Vec<_>>())
                })
            })
            .flatten()
            .unwrap_or_default();
        rows.into_iter()
            .filter_map(
                |(
                    id,
                    manifest,
                    status,
                    auto_reload,
                    probe_status,
                    probe_at,
                    revoked_key,
                    revoked_at,
                )| {
                    let m: Manifest = serde_json::from_str(&manifest).ok()?;
                    // also fetch path, so settings can show "where it came from"
                    let path_row = store
                        .lock()
                        .ok()
                        .and_then(|s| {
                            s.with_conn_ref(|c| {
                                c.query_row(
                                    "SELECT path FROM plugins WHERE id = ?1",
                                    params![id],
                                    |r| r.get::<_, String>(0),
                                )
                                .ok()
                            })
                        })
                        .flatten();
                    // Phase 38 — query the plugin_channel table separately for the currently subscribed channel.
                    // fetch everything via list_plugin_channels then find in memory,
                    // one SELECT for everything, avoiding N+1 queries.
                    let channel = store
                        .lock()
                        .ok()
                        .and_then(|s| {
                            s.list_plugin_channels()
                                .into_iter()
                                .find(|(pid, _)| pid == &id)
                        })
                        .map(|(_, c)| c);
                    // Phase 40 — health config (fetch all once, find in memory). No row → None.
                    let health_cfg = store
                        .lock()
                        .ok()
                        .and_then(|s| {
                            s.list_health_configs()
                                .into_iter()
                                .find(|(pid, _)| pid == &id)
                        })
                        .map(|(_, c)| c);
                    let capability_ids = m.capability_ids();
                    Some(PluginStatusDto {
                        id,
                        name: m.name,
                        description: m.description,
                        author: m.author,
                        homepage: m.homepage,
                        license: m.license,
                        version: m.version,
                        ptype: m.ptype,
                        status,
                        capabilities: capability_ids,
                        permissions: m.permissions,
                        path: path_row,
                        auto_reload: auto_reload != 0,
                        probe_status: probe_status.filter(|s| !s.is_empty()),
                        probe_at: probe_at.map(|v| v as u64).filter(|v| *v > 0),
                        channel,
                        sandbox_declared: m.sandbox.is_some(),
                        health_heartbeat_sec: health_cfg.as_ref().map(|c| c.heartbeat_sec),
                        health_max_retries: health_cfg.as_ref().map(|c| c.max_retries),
                        health_enabled: health_cfg.as_ref().map(|c| c.enabled),
                        missing_dependencies: crate::core::plugin_deps::find_missing(
                            &m.dependencies,
                            &installed,
                        )
                        .into_iter()
                        .map(|(id, requirement)| MissingDepDto { id, requirement })
                        .collect(),
                        revoked_key,
                        revoked_at: revoked_at.map(|v| v as u64).filter(|v| *v > 0),
                    })
                },
            )
            .collect()
    }

    /// Ensures the process is running before the call (lazy start).
    pub fn ensure_running(&self, id: &str) -> Result<Arc<PluginProcess>, String> {
        if let Some(p) = self.procs.lock().ok().and_then(|m| m.get(id).cloned()) {
            if p.is_alive() {
                return Ok(p);
            }
        }
        self.start(id)?;
        self.procs
            .lock()
            .ok()
            .and_then(|m| m.get(id).cloned())
            .ok_or_else(|| "plugin process missing after start".to_string())
    }

    /// Phase 45 — for the metrics module: returns (plugin_id, pid) pairs of all running plugins.
    /// `pid` may be None (the process just started and has no pid yet / it exited but procs is not cleaned up).
    pub fn list_running_with_pid(&self) -> Vec<(String, u32)> {
        let Ok(procs) = self.procs.lock() else {
            return Vec::new();
        };
        procs
            .iter()
            .filter_map(|(id, p)| p.pid().map(|pid| (id.clone(), pid)))
            .collect()
    }
}
