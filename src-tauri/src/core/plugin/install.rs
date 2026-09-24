//! Install/update pipeline: unpack, signature handoff, staging swap, DB transaction, preview.
//! Mechanical move from core/plugin.rs.

use super::*;

impl PluginManager {
    /// F6 — swap failure recovery: roll the DB row back to the old manifest and restore the original runtime state as needed.
    /// (the directory is already restored by the caller; here we fix up the DB row and process, eliminating "failed after stop leaves stopped").
    fn restore_after_failed_swap(&self, id: &str, old: &Option<Manifest>, was_running: bool) {
        if let Some(om) = old {
            if let Some(store) = Self::store() {
                if let Ok(mut s) = store.lock() {
                    let manifest_json = serde_json::to_string(om).unwrap_or_default();
                    let _ = s.with_conn(|c| {
                        c.execute(
                            "UPDATE plugins SET version = ?2, manifest = ?3, status = 'stopped' WHERE id = ?1",
                            params![id, om.version, manifest_json],
                        )
                        .unwrap_or(0)
                    });
                }
            }
        }
        if was_running {
            let _ = self.start(id);
        }
    }

    /// **dev/test only (not exposed in the settings page)**: install from an already-unpacked directory — for test fixtures and local
    /// development; the distribution path is `install_ocplugin` (.ocplugin ZIP) and the marketplace.
    /// §4.4: confirm item by item first (pure collection, zero writes), then commit (single transaction + events + hints).
    pub fn install_from_dir(&self, dir: &PathBuf) -> Result<String, String> {
        let _install_guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let m = Self::read_manifest(dir)?;
        // F4/F5 — install gate: core version and dependency cycles are caught before confirmation/DB write (zero side effects).
        Self::check_core_compat(&m)?;
        self.check_install_dependencies(&m)?;
        self.check_declaration_consistency(&m)?;
        let plan = Self::install_ask_plan(&m);
        let decisions = crate::core::permission::confirm_install(&m.id, &plan)?;
        self.write_install_tx(&m, dir, &decisions)?;
        self.after_install(&m)
    }

    /// §4.2 "global consistency": the effective mapping of all live providers of the same capability must agree (including the built-in static mapping).
    /// Inconsistent → reject the install, avoiding two enforcement meanings for the same capability name.
    fn check_declaration_consistency(&self, m: &Manifest) -> Result<(), String> {
        let Some(store) = Self::store() else {
            return Ok(()); // storage unavailable: the later commit phase will reject, so don't report again here
        };
        for (capability, permission, default, _timeout) in m.declarations() {
            // built-in static mapping takes precedence: a declaration must not land on a reserved capability (validate already blocks it; this is a fallback)
            if crate::core::capability::is_builtin(&capability) {
                return Err(format!(
                    "reserved capability {} cannot be declared",
                    capability
                ));
            }
            if let Some(other) = crate::core::declaration::conflicting_provider(
                &store,
                &capability,
                &permission,
                &default,
                &m.id,
            ) {
                return Err(format!(
                    "capability {} is already provided by {} with a different permission mapping",
                    capability, other
                ));
            }
        }
        Ok(())
    }

    /// F5 install-time precheck: only blocks cycles (missing dependencies are allowed through — the install order is user-controlled, with a start-time fallback).
    fn check_install_dependencies(&self, m: &Manifest) -> Result<(), String> {
        let Some(store) = Self::store() else {
            return Ok(());
        };
        let installed: Vec<(String, BTreeMap<String, semver::VersionReq>)> = store
            .lock()
            .ok()
            .and_then(|s| {
                s.with_conn_ref(|c| {
                    let mut st = c.prepare("SELECT id, manifest FROM plugins").ok()?;
                    let it = st
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                        .ok()?;
                    Some(
                        it.filter_map(|x| x.ok())
                            .filter_map(|(id, json)| {
                                let dm: Manifest = serde_json::from_str(&json).ok()?;
                                Some((id, crate::core::plugin_deps::parse_deps(&dm.dependencies)))
                            })
                            .collect::<Vec<_>>(),
                    )
                })
            })
            .flatten()
            .unwrap_or_default();
        let new_deps = crate::core::plugin_deps::parse_deps(&m.dependencies);
        if let Some(cyc) =
            crate::core::plugin_deps::would_create_cycle(&m.id, &new_deps, &installed)
        {
            return Err(format!("dependency cycle: {}", cyc.join(" -> ")));
        }
        Ok(())
    }

    /// §4.4 step 5 — commit phase (DB writes only). **Called only after all confirmations pass**.
    /// The `plugins` row + `plugin_permissions` are a **single transaction**; if any row fails the whole batch rolls back,
    /// leaving no half state. Events / alerting hints / probe / start are not here (see
    /// `after_install`), so on failure the caller only needs to clean up staging.
    fn write_install_tx(
        &self,
        m: &Manifest,
        dir: &Path,
        decisions: &[(String, String)],
    ) -> Result<(), String> {
        let manifest_json = serde_json::to_string(m).unwrap_or_default();
        let Some(store) = Self::store() else {
            return Err("storage unavailable".into());
        };
        let now = crate::core::plugin_trace::now_secs();
        let path = dir.display().to_string();
        let r = store.lock().ok().and_then(|mut s| {
            s.try_with_conn(|c| {
                let tx = c
                    .unchecked_transaction()
                    .map_err(|e| format!("begin failed: {}", e))?;
                tx.execute(
                    "INSERT INTO plugins (id, version, type, status, path, manifest)
                     VALUES (?1, ?2, ?3, 'probe_pending', ?4, ?5)
                     ON CONFLICT(id) DO UPDATE SET version=?2, type=?3, path=?4, manifest=?5, status='probe_pending'",
                    params![m.id, m.version, m.ptype, path, manifest_json],
                )
                .map_err(|e| format!("plugin row failed: {}", e))?;
                crate::core::permission::upsert_install_decisions_in_tx(&tx, &m.id, decisions, now as i64)?;
                // §4.6 frozen declaration table + domain registration (P1 installs locally and occupies first) — same transaction,
                // a domain conflict here must also roll back the whole batch, leaving no half state
                crate::core::declaration::write_in_tx(&tx, &m.id, &m.declarations(), now)?;
                tx.commit().map_err(|e| format!("commit failed: {}", e))?;
                Ok(())
            })
        });
        match r {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(e),
            None => Err("sqlite unavailable".into()),
        }
    }

    /// §4.4 second half of step 5 — post-commit side effects: events, alerting hints, probe, start.
    /// Review 2: `plugin.installed` and hints were previously emitted **before** user confirmation, leaving dirty
    /// records on rejection; now both happen after confirmation + a successful commit.
    fn after_install(&self, m: &Manifest) -> Result<String, String> {
        let id = m.id.clone();
        crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
            "plugin.installed",
            "core",
            json!({ "pluginId": id, "version": m.version }),
        ));
        // Phase 53 — load the severity hints declared by the manifest (if any).
        crate::core::alerting::install_manifest_hints(&id, &m.alerting);
        // Phase 37 — Probe self-check. When the manifest declares no capability (pure pet / config-only plugin)
        // it is skipped; otherwise `core.probe.capability` is called once per capability, and on failure
        // the plugin does not enter the lifecycle and the settings tab shows a red badge so the user can retry.
        let probe = crate::core::probe::run_and_publish(&id, &m.capability_ids());
        if probe.status == "failed" {
            Self::set_status(&id, "probe_failed");
            return Ok(id);
        }
        self.start(&id)?;
        Ok(id)
    }

    /// The plugin install root. Tests can override it with OPENCAPX_PLUGINS_DIR.
    pub fn plugins_root() -> PathBuf {
        if let Ok(dir) = std::env::var("OPENCAPX_PLUGINS_DIR") {
            return PathBuf::from(dir);
        }
        crate::core::home_dir()
            .map(|h| h.join(".opencapx").join("plugins"))
            .unwrap_or_else(|| std::env::temp_dir().join("opencapx-plugins"))
    }

    /// Install from .ocplugin (ZIP). See the install flow in docs/plugin-manifest.md.
    /// Safety: validate the manifest before extracting; check every entry name (reject absolute paths / ../ backslashes),
    /// with a 100MB per-file and 256MB per-package limit; extract to a temp directory then atomically rename.
    /// F7 — three-state install entry (fail-closed by default: soft warnings need explicit confirmation).
    /// Production commands go through [`Self::install_ocplugin_ex`]; this wrapper is for tests and future SDK default calls.
    #[allow(dead_code)]
    pub fn install_ocplugin(&self, archive: &Path) -> Result<String, String> {
        self.install_ocplugin_ex(archive, InstallOptions::default())
    }

    /// F7 — install/update entry with confirmation options: trusted direct install / soft-warning explicit confirmation / integrity hard reject;
    /// a key change on an installed version additionally needs `confirm_key_change` (the UI catches the marker and retries with confirmation).
    pub fn install_ocplugin_ex(
        &self,
        archive: &Path,
        opts: InstallOptions,
    ) -> Result<String, String> {
        let _install_guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let file = std::fs::File::open(archive)
            .map_err(|e| format!("cannot open {}: {}", archive.display(), e))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("bad zip: {}", e))?;

        // the manifest must be at the package root
        let mut manifest_text = String::new();
        let mut found = false;
        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|e| format!("zip read error: {}", e))?;
            if entry.name() == "opencapx-plugin.json" {
                std::io::Read::read_to_string(&mut entry, &mut manifest_text)
                    .map_err(|e| format!("manifest read error: {}", e))?;
                found = true;
                break;
            }
        }
        if !found {
            return Err("missing opencapx-plugin.json at archive root".into());
        }
        let m: Manifest =
            serde_json::from_str(&manifest_text).map_err(|e| format!("bad manifest: {}", e))?;
        Self::validate_manifest(&m)?;
        // F6 check order, position 1: revocation. A revoked-disabled plugin must first be explicitly reopened;
        // if the target publisher key is in the registry revocation list → reject outright (same path for install and update).
        if let Some(key) = Self::revoked_key_of(&m.id) {
            return Err(format!(
                "plugin revoked: publisher key {} (reopen explicitly before reinstall/update)",
                key
            ));
        }
        if let Some(key_id) = manifest_signature_key_id(&m) {
            if crate::core::revocation::key_is_revoked(&key_id) {
                return Err(format!("publisher key revoked: {}", key_id));
            }
        }
        // F4/F5 — install gate: caught before any side effect (signature event / extraction staging).
        Self::check_core_compat(&m)?;
        self.check_install_dependencies(&m)?;

        // Wow 6 + F7 — signature/integrity check (three states: direct install / soft-warning confirmation / hard reject).
        // verify before extracting, to prevent a malicious zip from landing in plugins/<id>/ before triggering side effects.
        let outcome = crate::core::plugin_sig::verify(archive);
        match outcome.allowance() {
            crate::core::plugin_sig::Allowance::Direct => {}
            crate::core::plugin_sig::Allowance::SoftWarn => {
                if !allow_unsigned() {
                    return Err(format!(
                        "unsigned package rejected: allow_unsigned is off ({})",
                        outcome.label()
                    ));
                }
                if !opts.confirm_unsigned {
                    return Err(format!("unsigned-confirm-required: {}", outcome.label()));
                }
            }
            crate::core::plugin_sig::Allowance::HardDeny => {
                return Err(format!(
                    "signature verification failed: {}",
                    outcome.label()
                ));
            }
        }
        if let VerifyOutcome::Trusted { key_id } = &outcome {
            // leave a trace: trusted install goes into the audit
            crate::core::event::EventBus::shared().publish(
                &crate::core::event::OpencapxEvent::new(
                    "plugin.signature.verified",
                    "core",
                    serde_json::json!({ "id": m.id, "keyId": key_id }),
                ),
            );
        }
        // F6 — key continuity check (after the trust chain, before the permission diff): key change = new publisher,
        // which needs explicit confirmation (the F7 warning-confirm path; the error marker lets the UI catch it and retry with confirmation).
        let old_manifest = Self::row_manifest(&m.id).ok().map(|(_, om)| om);
        let key_changed = old_manifest
            .as_ref()
            .map(|om| manifest_signature_key_id(om) != manifest_signature_key_id(&m))
            .unwrap_or(false);
        if key_changed && !opts.confirm_key_change {
            return Err(format!(
                "publisher-key-change-confirm-required: {} -> {}",
                old_manifest
                    .as_ref()
                    .and_then(manifest_signature_key_id)
                    .unwrap_or_else(|| "<none>".into()),
                manifest_signature_key_id(&m).unwrap_or_else(|| "<none>".into()),
            ));
        }

        // §4.2 "global consistency": compare the mapping with other already-frozen providers. Placed **before extraction** —
        // a failure here needs no staging cleanup (it is not created yet), keeping the error path shorter.
        self.check_declaration_consistency(&m)?;

        // entry-name safety check + size limits
        const MAX_FILE: u64 = 100 * 1024 * 1024;
        const MAX_TOTAL: u64 = 256 * 1024 * 1024;
        let mut total: u64 = 0;
        for i in 0..zip.len() {
            let entry = zip
                .by_index(i)
                .map_err(|e| format!("zip read error: {}", e))?;
            let name = entry.name();
            if name.starts_with('/')
                || name.contains("..")
                || name.contains('\\')
                || Path::new(name).is_absolute()
            {
                return Err(format!("unsafe path in archive: {}", name));
            }
            if entry.size() > MAX_FILE {
                return Err(format!(
                    "entry too large: {} ({} bytes)",
                    name,
                    entry.size()
                ));
            }
            total += entry.size();
        }
        if total > MAX_TOTAL {
            return Err("archive too large".into());
        }

        // extract to a temp directory (P0: the lexical check already blocks `..`; the component-level prefix assertion here is a fallback)
        let root = Self::plugins_root();
        let tmp = root.join(format!(".tmp-{}-{}", m.id, std::process::id()));
        if !tmp.starts_with(&root) {
            return Err("install temp path escapes plugins root".into());
        }
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| format!("mkdir failed: {}", e))?;
        // M8 hardening: any failure in the staging phase (corrupt entry / recheck failure) must clean up tmp,
        // otherwise .tmp-* is left in the plugins root (pinned by the corrupted-package test).
        let staged = (|| -> Result<(), String> {
            for i in 0..zip.len() {
                let mut entry = zip
                    .by_index(i)
                    .map_err(|e| format!("zip read error: {}", e))?;
                let out = tmp.join(entry.name());
                if entry.is_dir() {
                    std::fs::create_dir_all(&out).map_err(|e| format!("mkdir failed: {}", e))?;
                    continue;
                }
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {}", e))?;
                }
                let mut f = std::fs::File::create(&out)
                    .map_err(|e| format!("extract {} failed: {}", entry.name(), e))?;
                std::io::copy(&mut entry, &mut f)
                    .map_err(|e| format!("extract {} failed: {}", entry.name(), e))?;
                // preserve the unix executable bit so binary plugins work
                #[cfg(unix)]
                if let Some(mode) = entry.unix_mode() {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode));
                }
            }
            // recheck the manifest from the extraction result
            Self::read_manifest(&tmp)?;
            Ok(())
        })();
        if let Err(e) = staged {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }

        // P0: dest must still be inside the plugins root (lexical check + component-level assertion, two layers)
        let dest = root.join(&m.id);
        if !dest.starts_with(&root) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err("install path escapes plugins root".into());
        }

        // §4.4 step 4 — item-by-item confirmation (**pure collection, zero writes**). Must happen before any destructive action
        // before it: at this point only the staging directory has been touched, so a user rejection → clean staging and return,
        // with the installed version's directory and DB records intact (review C2: previously it was "swap the directory / write the DB first,
        // confirm later", so a rejection destroyed the old version).
        // F6: updates go by diff — same key and unchanged permissions/mapping gives an empty plan (silently reuse
        // the existing consent); key changed/unknown → full re-confirmation (new-publisher path).
        let (plan, _) = Self::update_plan_for(old_manifest.as_ref(), &m);
        if key_changed {
            crate::core::event::EventBus::shared().publish(
                &crate::core::event::OpencapxEvent::new(
                    "plugin.update.key_changed",
                    "core",
                    serde_json::json!({
                        "pluginId": m.id,
                        "from": old_manifest.as_ref().and_then(manifest_signature_key_id),
                        "to": manifest_signature_key_id(&m),
                    }),
                ),
            );
        }
        let decisions = match crate::core::permission::confirm_install(&m.id, &plan) {
            Ok(d) => d,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
        };

        // §4.4 step 5a — commit the DB first (single transaction). On failure the old directory / old records are untouched,
        // so cleaning staging is enough and the old version is intact. The directory switch comes after the commit: DB failures are more common of the two,
        // and doing that first would expose a half-installed state where the old version is deleted but the new one is not in the DB.
        if let Err(e) = self.write_install_tx(&m, &dest, &decisions) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }

        // §4.4 step 5b — directory switch. Stop the old process, move it to .bak as a safety net, then rename the new one into
        // place; if any step fails, move .bak back, restore the DB row and the original runtime state (F6 transactionality:
        // eliminating "failure after stop leaves stopped"; a same-partition rename almost never fails, and the recovery branch
        // guarantees we are never stranded on both sides).
        let backup = root.join(format!(".old-{}-{}", m.id, std::process::id()));
        let had_old = dest.exists();
        let was_running = self
            .get_process(&m.id)
            .map(|p| p.is_alive())
            .unwrap_or(false);
        if had_old {
            self.stop(&m.id);
            let _ = std::fs::remove_dir_all(&backup);
            if let Err(e) = std::fs::rename(&dest, &backup) {
                let _ = std::fs::remove_dir_all(&tmp);
                self.restore_after_failed_swap(&m.id, &old_manifest, was_running);
                return Err(format!("failed to set aside old version: {}", e));
            }
        }
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            if had_old {
                let _ = std::fs::rename(&backup, &dest);
            }
            let _ = std::fs::remove_dir_all(&tmp);
            self.restore_after_failed_swap(&m.id, &old_manifest, was_running);
            return Err(format!("install move failed: {}", e));
        }
        if had_old {
            let _ = std::fs::remove_dir_all(&backup);
        }

        // §4.4 step 5c — side effects (events / hints / probe / start)
        // F7 audit: after a soft-warning tier (unsigned / unknown key) install succeeds, write `plugin.installed.unsigned`.
        let id = self.after_install(&m)?;
        if matches!(
            outcome.allowance(),
            crate::core::plugin_sig::Allowance::SoftWarn
        ) {
            crate::core::event::EventBus::shared().publish(
                &crate::core::event::OpencapxEvent::new(
                    "plugin.installed.unsigned",
                    "core",
                    serde_json::json!({
                        "pluginId": m.id,
                        "status": outcome.label(),
                        "keyId": match &outcome {
                            VerifyOutcome::UnknownKey { key_id } => Some(key_id.clone()),
                            _ => None,
                        },
                    }),
                ),
            );
        }
        Ok(id)
    }

    /// Wow 5 — let the user take a look before installing. Shares the manifest parsing path with install_ocplugin
    /// (aligned with i1.md §18 path safety check + apiVersion validation), but does **not** extract,
    /// does not write the DB, and does not spawn. The returned permissions array carries the high_risk marker,
    /// and the frontend dialog marks "camera / microphone / filesystem.write / process.execute" with red badges.
    pub fn preview_ocplugin(archive: &Path) -> Result<PluginPreviewDto, String> {
        let file = std::fs::File::open(archive)
            .map_err(|e| format!("cannot open {}: {}", archive.display(), e))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("bad zip: {}", e))?;
        let mut manifest_text = String::new();
        let mut found = false;
        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|e| format!("zip read error: {}", e))?;
            if entry.name() == "opencapx-plugin.json" {
                std::io::Read::read_to_string(&mut entry, &mut manifest_text)
                    .map_err(|e| format!("manifest read error: {}", e))?;
                found = true;
                break;
            }
        }
        if !found {
            return Err("missing opencapx-plugin.json at archive root".into());
        }
        let m: Manifest =
            serde_json::from_str(&manifest_text).map_err(|e| format!("bad manifest: {}", e))?;
        Self::validate_manifest(&m)?;
        // §4.4 step 4: preview and confirmation share one source = permissions[] ∪ inline-mapping permissions (M1),
        // declaration-derived items carry the `declared` marker → the frontend hides Always (the UI face of once-only)
        let permissions = Self::install_ask_plan(&m)
            .into_iter()
            .map(|ask| PermissionPreviewDto {
                high_risk: crate::core::permission::HIGH_RISK.contains(&ask.permission.as_str()),
                declared: ask.declared,
                name: ask.permission,
            })
            .collect();
        // Wow 6 — signature/integrity status is surfaced too (even if preview does not block, the UI badge must show it)
        let outcome = crate::core::plugin_sig::verify(archive);
        let (status, key_id) = match outcome {
            VerifyOutcome::Trusted { key_id } => ("trusted".to_string(), Some(key_id)),
            VerifyOutcome::Unsigned => ("unsigned".to_string(), None),
            VerifyOutcome::HashMismatch { .. } => ("tampered".to_string(), None),
            VerifyOutcome::BadSignature { .. } => ("bad-signature".to_string(), None),
            VerifyOutcome::UnknownKey { key_id } => ("unknown-key".to_string(), Some(key_id)),
            VerifyOutcome::MalformedSignature => ("malformed-signature".to_string(), None),
        };
        let capability_ids = m.capability_ids();
        // F7 — provenance verification / official marker / compatibility / permission diff (rendering facts, not gating decisions).
        let verified = key_id.as_deref().and_then(|k| {
            crate::core::registry::load_offline().map(|idx| {
                crate::core::registry::key_status(&idx, k)
                    == crate::core::registry::KeyStatus::Registered
            })
        });
        let official = key_id
            .as_deref()
            .map(|k| k.starts_with("com.opencapx"))
            .unwrap_or(false);
        let compat = CompatPreviewDto {
            ok: Self::check_core_compat(&m).is_ok(),
            min_core_version: m.min_core_version.clone(),
            current: env!("CARGO_PKG_VERSION").to_string(),
        };
        let permission_diff = Self::row_manifest(&m.id).ok().map(|(_, om)| {
            let old: std::collections::BTreeSet<String> = Self::install_ask_plan(&om)
                .into_iter()
                .map(|a| a.permission)
                .collect();
            let new: std::collections::BTreeSet<String> = Self::install_ask_plan(&m)
                .into_iter()
                .map(|a| a.permission)
                .collect();
            PermissionDiffDto {
                added: new.difference(&old).cloned().collect(),
                removed: old.difference(&new).cloned().collect(),
            }
        });
        Ok(PluginPreviewDto {
            id: m.id,
            name: m.name,
            description: m.description,
            author: m.author,
            homepage: m.homepage,
            license: m.license,
            version: m.version,
            ptype: m.ptype,
            capabilities: capability_ids,
            permissions,
            signature: SignaturePreviewDto { status, key_id },
            verified,
            official,
            compat,
            permission_diff,
            sandbox_declared: m.sandbox.is_some(),
        })
    }
}
