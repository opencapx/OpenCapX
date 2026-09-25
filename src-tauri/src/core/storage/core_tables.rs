//! the Storage handle: schema creation, core table accessors (sessions/events/plugins/permissions/capabilities/hotkeys), session sink, archiving.
//! Mechanical move from core/storage.rs.

use super::*;

/// Phase 39 — a hotkey binding. `payload` is JSON; the concrete schema is defined in the `core::hotkey` module.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HotkeyRow {
    pub combo: String,
    pub label: String,
    pub payload: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Phase 35 — LIKE pattern escaping. Escapes all `\`, `%`, `_` in user input, paired with
/// SQL `... LIKE ? ESCAPE '\\'` so they are not treated as wildcards. Mind the order: escape `\` first or the next two steps would double-escape.
pub(crate) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub struct Storage {
    pub(crate) conn: Connection,
}

impl SessionSink for Storage {
    fn upsert(&mut self, s: Session) {
        let _ = self.conn.execute(
            // started_at is written only on insert and deliberately not updated on conflict — it is the "session first-appearance time"
            "INSERT INTO sessions (id, agent, project, cwd, message, state, updated_at, model, started_at, speech)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET agent=?2, project=?3, cwd=?4, message=?5, state=?6, updated_at=?7, model=?8, speech=?10",
            rusqlite::params![
                s.id,
                s.agent,
                s.project,
                s.cwd,
                s.message,
                state_str(s.state),
                s.updated_at,
                s.model,
                if s.started_at == 0 { s.updated_at } else { s.started_at },
                s.speech,
            ],
        );
    }

    fn get(&self, id: &str) -> Option<Session> {
        self.conn
            .query_row(
                "SELECT id, agent, project, cwd, message, state, updated_at, model, started_at, speech FROM sessions WHERE id = ?1",
                [id],
                |r| {
                    Ok(Session {
                        id: r.get(0)?,
                        agent: r.get(1)?,
                        project: r.get(2)?,
                        cwd: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        message: r.get(4)?,
                        state: Self::state_from_str(&r.get::<_, String>(5)?),
                        updated_at: r.get(6)?,
                        model: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                        started_at: r.get::<_, Option<u64>>(8)?.unwrap_or(0),
                        speech: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                        choices: None,
                        answered: None,
                    })
                },
            )
            .ok()
    }

    fn active(&self, now: u64) -> Vec<Session> {
        self.all()
            .into_iter()
            .filter(|s| crate::core::agent::is_active(s, now))
            .collect()
    }

    /// Expired sessions are first written into `session_archive` (the data source for session history) then deleted from the active table.
    fn sweep(&mut self, now: u64) -> usize {
        let expired: Vec<Session> = self
            .all()
            .into_iter()
            .filter(|s| !crate::core::agent::is_active(s, now))
            .collect();
        for s in &expired {
            let started = if s.started_at == 0 {
                s.updated_at
            } else {
                s.started_at
            };
            let _ = self.conn.execute(
                "INSERT INTO session_archive
                     (id, agent, project, message, state, model, started_at, ended_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET state=?5, message=?4, ended_at=?8",
                rusqlite::params![
                    s.id,
                    s.agent,
                    s.project,
                    s.message,
                    state_str(s.state),
                    s.model,
                    started,
                    s.updated_at
                ],
            );
            let _ = self
                .conn
                .execute("DELETE FROM sessions WHERE id = ?1", [&s.id]);
        }
        expired.len()
    }

    fn all(&self) -> Vec<Session> {
        let mut stmt = match self
            .conn
            .prepare("SELECT id, agent, project, cwd, message, state, updated_at, model, started_at, speech FROM sessions")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| {
            Ok(Session {
                id: r.get(0)?,
                agent: r.get(1)?,
                project: r.get(2)?,
                cwd: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                message: r.get(4)?,
                state: Self::state_from_str(&r.get::<_, String>(5)?),
                updated_at: r.get(6)?,
                model: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                started_at: r.get::<_, Option<u64>>(8)?.unwrap_or(0),
                speech: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                choices: None,
                answered: None,
            })
        });
        match rows {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    fn dismiss(&mut self, id: &str) {
        let _ = self
            .conn
            .execute("DELETE FROM sessions WHERE id = ?1", [id]);
    }

    fn clear(&mut self) {
        let _ = self.conn.execute("DELETE FROM sessions", []);
    }
}

/// Archive retention days (session history).
pub const ARCHIVE_KEEP_DAYS: u64 = 90;

impl Storage {
    /// Read-only borrow of the underlying connection (same shape as `StoreEnum::with_conn_ref`; used by startup integrity checks).
    pub fn with_conn_ref<R>(&self, f: impl FnOnce(&rusqlite::Connection) -> R) -> R {
        f(&self.conn)
    }

    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        // O3 — standard for a local desktop DB: WAL + synchronous=NORMAL + busy_timeout. The default DELETE journal
        // fsyncs the journal on every write under the global Mutex, amplifying the event hot path; under WAL reads do not block writes.
        // (opencapx.db is never copied whole-file — backup.rs only backs up config/business JSON, so there is no WAL-copy issue.)
        let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()));
        let _ = conn.execute_batch("PRAGMA synchronous=NORMAL");
        let _ = conn.busy_timeout(std::time::Duration::from_millis(5000));
        let mut s = Self { conn };
        s.migrate()?;
        s.prune_events(crate::core::agent::now_secs())?;
        let _ = s.prune_session_archive(crate::core::agent::now_secs());
        Ok(s)
    }

    fn migrate(&mut self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                agent TEXT NOT NULL,
                project TEXT NOT NULL,
                cwd TEXT NOT NULL DEFAULT '',
                message TEXT NOT NULL,
                state TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                started_at INTEGER NOT NULL DEFAULT 0,
                model TEXT NOT NULL DEFAULT '',
                speech TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS session_archive (
                id TEXT PRIMARY KEY,
                agent TEXT NOT NULL,
                project TEXT NOT NULL,
                message TEXT NOT NULL,
                state TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                started_at INTEGER NOT NULL,
                ended_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_session_archive_ended ON session_archive(ended_at DESC);
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                type TEXT NOT NULL,
                source TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                payload TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_events_ts ON events(timestamp);
            CREATE TABLE IF NOT EXISTS plugins (
                id TEXT PRIMARY KEY,
                version TEXT NOT NULL,
                type TEXT NOT NULL,
                status TEXT NOT NULL,
                path TEXT NOT NULL,
                manifest TEXT NOT NULL,
                auto_reload INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS plugin_permissions (
                plugin_id TEXT NOT NULL,
                permission TEXT NOT NULL,
                scope TEXT,
                decision TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (plugin_id, permission)
            );
            CREATE TABLE IF NOT EXISTS capabilities (
                id TEXT NOT NULL,
                version TEXT NOT NULL,
                plugin_id TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 100,
                enabled INTEGER NOT NULL DEFAULT 1,
                avg_latency_ms INTEGER,
                last_used_at INTEGER,
                PRIMARY KEY (id, plugin_id)
            );
            CREATE TABLE IF NOT EXISTS plugin_ratings (
                id TEXT PRIMARY KEY,
                plugin_id TEXT NOT NULL,
                score INTEGER NOT NULL CHECK (score >= 1 AND score <= 5),
                comment TEXT,
                ts INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ratings_plugin_ts ON plugin_ratings(plugin_id, ts DESC);
            CREATE TABLE IF NOT EXISTS capability_stats (
                capability TEXT NOT NULL,
                plugin_id TEXT NOT NULL,
                elapsed_ms INTEGER NOT NULL,
                ts INTEGER NOT NULL,
                result TEXT NOT NULL DEFAULT 'ok',
                error_kind TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_capstats_cap_plugin ON capability_stats(capability, plugin_id, ts DESC);
            -- docs/permission-domains.md §4.6: plugin domain declaration (frozen table + domain registry)
            CREATE TABLE IF NOT EXISTS capability_declarations (
                capability       TEXT NOT NULL,
                plugin_id        TEXT NOT NULL,
                permission       TEXT NOT NULL,
                default_decision TEXT NOT NULL,
                confirmed_at     INTEGER NOT NULL,
                PRIMARY KEY (capability, plugin_id)
            );
            CREATE INDEX IF NOT EXISTS idx_capdecl_perm ON capability_declarations(permission);
            CREATE TABLE IF NOT EXISTS domain_registry (
                domain       TEXT PRIMARY KEY,
                plugin_id    TEXT,
                publisher_id TEXT,
                source       TEXT NOT NULL
            );",
        );
        // Phase 31: silent migration of old tables (ALTER on an existing column fails and is ignored).
        let _ = self.conn.execute_batch(
            "ALTER TABLE capability_stats ADD COLUMN result TEXT NOT NULL DEFAULT 'ok';
             ALTER TABLE capability_stats ADD COLUMN error_kind TEXT",
        );
        // Phase 47: the session's current model name (mostly absent from hook payloads, filled in from the transcript tail)
        // and first-appearance time (used to compute duration when archiving).
        let _ = self.conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN model TEXT NOT NULL DEFAULT '';
             ALTER TABLE sessions ADD COLUMN started_at INTEGER NOT NULL DEFAULT 0",
        );
        // Session body (the last assistant text in the transcript tail): the bubble's second line.
        // Only in the active table — the archive is a historical view keeping only message/model, not the body.
        let _ = self
            .conn
            .execute_batch("ALTER TABLE sessions ADD COLUMN speech TEXT NOT NULL DEFAULT ''");
        // Session's full working directory (bubble grouping + branch lookup). A separate batch:
        // rusqlite's execute_batch stops at the first failing statement,
        // so a statement mixed with an existing column would skip cwd "because speech already exists".
        let _ = self
            .conn
            .execute_batch("ALTER TABLE sessions ADD COLUMN cwd TEXT NOT NULL DEFAULT ''");
        // Phase 37: Probe self-check — status text (passed/failed/pending) + timestamp + JSON report.
        let _ = self.conn.execute_batch(
            "ALTER TABLE plugins ADD COLUMN probe_status TEXT;
             ALTER TABLE plugins ADD COLUMN probe_at INTEGER;
             ALTER TABLE plugins ADD COLUMN probe_report TEXT",
        );
        // Migration backfill: auto_reload is a later-added column — old DBs lack it at CREATE, while plugin::list()'s
        // SELECT references it directly; a missing column makes the whole plugin list read empty (startup restore then fails).
        // A separate batch: execute_batch stops at the first failing statement, so it cannot be merged into the group above.
        let _ = self
            .conn
            .execute_batch("ALTER TABLE plugins ADD COLUMN auto_reload INTEGER NOT NULL DEFAULT 0");
        // M4: revocation channel columns — revoked_key=publisher key that hit registry revokedKeys
        // (non-NULL means default-disabled; the start gate rejects based on it); revoked_at=hit time;
        // revocation_ack=key the user explicitly reopened and exempted (sweep no longer re-disables).
        let _ = self.conn.execute_batch(
            "ALTER TABLE plugins ADD COLUMN revoked_key TEXT;
             ALTER TABLE plugins ADD COLUMN revoked_at INTEGER;
             ALTER TABLE plugins ADD COLUMN revocation_ack TEXT",
        );
        // M7: F9 data migration — the version of the last **successful start** (initialize handshake succeeded);
        // NULL before the first successful start. On the next start it is injected to the plugin as previousVersion for self-migration.
        let _ = self
            .conn
            .execute_batch("ALTER TABLE plugins ADD COLUMN last_version TEXT");
        // S4: per-capability call timeout — the object-form declared timeoutSecs (seconds, 1..=600);
        // NULL = default CALL_TIMEOUT (60s). Backfilled in old DBs; declaration reading depends on it.
        let _ = self
            .conn
            .execute_batch("ALTER TABLE capability_declarations ADD COLUMN timeout_secs INTEGER");
        // Phase 38: per-plugin update channel preference (stable / beta / dev). The table is separate from plugins
        // because channel is a user preference (may change), not metadata the plugin carries.
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS plugin_channel (
                plugin_id TEXT PRIMARY KEY,
                channel TEXT NOT NULL DEFAULT 'stable',
                updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS settings_kv (
                k TEXT PRIMARY KEY,
                v TEXT NOT NULL
             );",
        );
        // Phase 39: global hotkey bindings — combo is the unique key; payload is JSON describing a builtin or plugin action.
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS hotkey_binding (
                combo TEXT PRIMARY KEY,
                label TEXT NOT NULL DEFAULT '',
                payload TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             );",
        );
        // Phase 40: per-plugin watchdog / heartbeat config — plugins without a row use hardcoded defaults (aligned with Phase 10).
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS plugin_health_config (
                plugin_id TEXT PRIMARY KEY,
                heartbeat_sec INTEGER NOT NULL DEFAULT 0,
                ping_timeout_ms INTEGER NOT NULL DEFAULT 1000,
                max_retries INTEGER NOT NULL DEFAULT 3,
                backoff_initial_ms INTEGER NOT NULL DEFAULT 1000,
                enabled INTEGER NOT NULL DEFAULT 1,
                updated_at INTEGER NOT NULL DEFAULT 0
             );
             -- Phase 45: plugin runtime metrics sampling ring table (each plugin keeps at most 1500 rows ≈ 1h @ 2s sampling).
             CREATE TABLE IF NOT EXISTS plugin_metrics_samples (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                plugin_id TEXT NOT NULL,
                ts INTEGER NOT NULL,
                cpu_pct REAL NOT NULL,
                rss_bytes INTEGER NOT NULL,
                threads INTEGER NOT NULL DEFAULT 0,
                fds INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_metrics_plugin_ts ON plugin_metrics_samples(plugin_id, ts DESC);
             -- Phase 48: webhook delivery dead-letter queue (enqueued after a failed POST, retried in the background with exponential backoff).
             CREATE TABLE IF NOT EXISTS alerting_failed_deliveries (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                url TEXT NOT NULL,
                payload TEXT NOT NULL,
                first_attempt_ts INTEGER NOT NULL,
                last_attempt_ts INTEGER NOT NULL,
                attempts INTEGER NOT NULL,
                max_attempts INTEGER NOT NULL,
                last_error TEXT NOT NULL,
                next_retry_ts INTEGER NOT NULL,
                state TEXT NOT NULL DEFAULT 'pending'
             );
             CREATE INDEX IF NOT EXISTS idx_alerting_state_retry ON alerting_failed_deliveries(state, next_retry_ts);",
        );
        // Phase 49: multi-endpoint fanout. New table + add endpoint_id column to failed_deliveries.
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS alerting_endpoints (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                url TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                headers_json TEXT NOT NULL DEFAULT '[]',
                secret TEXT NOT NULL DEFAULT '',
                source_filter_json TEXT NOT NULL DEFAULT '[]',
                created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_alerting_endpoints_enabled ON alerting_endpoints(enabled);
             ALTER TABLE alerting_failed_deliveries ADD COLUMN endpoint_id TEXT;
             ALTER TABLE alerting_endpoints ADD COLUMN schema_version INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE alerting_endpoints ADD COLUMN template TEXT DEFAULT NULL;
             ALTER TABLE alerting_endpoints ADD COLUMN template_sample TEXT DEFAULT NULL;
             ALTER TABLE alerting_endpoints ADD COLUMN severity_overrides_json TEXT;
             CREATE TABLE IF NOT EXISTS template_presets (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                kind TEXT NOT NULL,
                template TEXT NOT NULL,
                sample TEXT,
                builtin INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_template_presets_builtin ON template_presets(builtin)",
        );
        // Phase 60: add version + changelog to template_presets (fork builtin + version tracking). try-catch tolerant
        let _ = self.conn.execute_batch(
            "ALTER TABLE template_presets ADD COLUMN version INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE template_presets ADD COLUMN changelog TEXT",
        );
        // Phase 50: silence rules (based on time window + weekday + hour range) and on-call ack suppression.
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS alerting_silences (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                kind_pattern TEXT NOT NULL DEFAULT '*',
                starts_at INTEGER NOT NULL,
                ends_at INTEGER NOT NULL,
                weekdays INTEGER NOT NULL DEFAULT 127,
                start_hour INTEGER NOT NULL DEFAULT 0,
                end_hour INTEGER NOT NULL DEFAULT 24,
                created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_silences_active ON alerting_silences(starts_at, ends_at);
             CREATE TABLE IF NOT EXISTS alerting_acks (
                id TEXT PRIMARY KEY,
                kind_pattern TEXT NOT NULL,
                ack_until INTEGER NOT NULL,
                created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_acks_until ON alerting_acks(ack_until)",
        );
        // Phase 51: DSL route rules (when/then), ordered by priority + enabled index.
        // Phase 68: `seen_in_last_json` stores `SeenInLastSpec` (or NULL).
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS alerting_routes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 100,
                enabled INTEGER NOT NULL DEFAULT 1,
                kind_pattern TEXT NOT NULL DEFAULT '*',
                payload_path TEXT,
                payload_match TEXT,
                target_endpoint_ids_json TEXT NOT NULL,
                tags_json TEXT,
                recipients_json TEXT NOT NULL DEFAULT '[]',
                seen_in_last_json TEXT,
                created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_routes_priority ON alerting_routes(enabled, priority)",
        );
        // Phase 66 — alert recipients refs (multi-channel fanout: webhook / log:stderr / log:file / email:smtp)
        // Tolerant: ALTER-adds the column when an old DB lacks it (separate try, not coupled to CREATE).
        let _ = self.conn.execute(
            "ALTER TABLE alerting_routes ADD COLUMN recipients_json TEXT NOT NULL DEFAULT '[]'",
            [],
        );
        // Phase 53: Per-source severity hints (plugin manifest or user override). uniqueness by source.
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS alerting_severity_hints (
                source TEXT NOT NULL,
                severity TEXT NOT NULL,
                origin TEXT NOT NULL,
                plugin_id TEXT,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (source, origin)
             );
             CREATE INDEX IF NOT EXISTS idx_alerting_severity_hints_origin_plugin
                ON alerting_severity_hints(origin, plugin_id)",
        );
        // Phase 67 — alert recipients table (webhook / log:stderr / log:file / email:smtp).
        // Persists alert recipient config; frontend CRUD; the backend uses it as resolve_recipient's lookup.
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS alerting_recipients (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                kind TEXT NOT NULL,
                config_json TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_alerting_recipients_enabled
                ON alerting_recipients(enabled)",
        );
        // Phase 54: frequency-threshold aggregation rules (action=downgrade|suppress|merge, triggered when threshold_count is exceeded within window_secs).
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS alerting_aggregations (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                kind_pattern TEXT NOT NULL DEFAULT '*',
                window_secs INTEGER NOT NULL,
                threshold_count INTEGER NOT NULL,
                action TEXT NOT NULL,
                target_severity TEXT,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_alerting_aggregations_enabled
                ON alerting_aggregations(enabled)",
        );
        // Phase 55: cross-event correlation suppression (B is suppressed within window_secs after A occurs).
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS alerting_correlations (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                kind_pattern_a TEXT NOT NULL,
                kind_pattern_b TEXT NOT NULL,
                window_secs INTEGER NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_alerting_correlations_enabled
                ON alerting_correlations(enabled)",
        );
        // Phase 56: upgrade / escalation chain (re-sends to the escalation endpoint while source keeps firing and is unacked).
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS alerting_escalations (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                kind_pattern TEXT NOT NULL,
                escalate_after_secs INTEGER NOT NULL,
                target_severity TEXT NOT NULL,
                target_endpoint_ids TEXT NOT NULL DEFAULT '[]',
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_alerting_escalations_enabled
                ON alerting_escalations(enabled)",
        );
        // Agent identity (per docs/permissions.md "Agent identity"): a security subject, ≠ the sessions observation table.
        // v1 one kind one identity (UNIQUE kind); token stores only a hash; revoked state is restored from the settings page.
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agents (
                agent_id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                display_name TEXT NOT NULL,
                token_hash TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                first_seen INTEGER NOT NULL,
                last_seen INTEGER NOT NULL,
                registered_via TEXT NOT NULL DEFAULT 'mcp'
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_agents_kind ON agents(kind);
             CREATE TABLE IF NOT EXISTS agent_permissions (
                agent_id TEXT NOT NULL,
                permission TEXT NOT NULL,
                scope TEXT,
                decision TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (agent_id, permission)
             );
             CREATE TABLE IF NOT EXISTS permission_policy (
                permission TEXT PRIMARY KEY,
                decision TEXT NOT NULL,
                updated_at INTEGER NOT NULL
             )",
        );
        Ok(())
    }

    pub fn insert_event(&mut self, e: &OpencapxEvent) {
        let payload = serde_json::to_string(&e.payload).unwrap_or_else(|_| "null".into());
        let _ = self.conn.execute(
            "INSERT OR REPLACE INTO events (id, type, source, timestamp, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![e.id, e.kind, e.source, e.timestamp, payload],
        );
    }

    pub fn list_events(&self, kind_prefix: &str, limit: usize) -> Vec<OpencapxEvent> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, type, source, timestamp, payload FROM events
             WHERE type LIKE ?1 ORDER BY timestamp DESC LIMIT ?2",
        ) else {
            return Vec::new();
        };
        let pat = format!("{}%", kind_prefix);
        let it = stmt.query_map(rusqlite::params![pat, limit as i64], |r| {
            let payload: String = r.get(4)?;
            Ok(OpencapxEvent {
                id: r.get(0)?,
                kind: r.get(1)?,
                source: r.get(2)?,
                timestamp: r.get(3)?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
            })
        });
        it.ok()
            .map(|i| i.filter_map(|x| x.ok()).collect())
            .unwrap_or_default()
    }

    /// Phase 35 — Audit search/filter: kind prefix + full text + time range + limit.
    /// `query` searches the type + payload JSON text, with `ESCAPE '\\'` so %/_ in user input are not treated as wildcards.
    /// The 4 optional clauses are skipped via empty-string / 0 placeholders, using SQLite's `(p = '' OR ...)` short-circuit pattern.
    pub fn list_events_filtered(
        &self,
        kind_prefix: Option<&str>,
        query: Option<&str>,
        since: Option<u64>,
        until: Option<u64>,
        limit: usize,
    ) -> Vec<OpencapxEvent> {
        // empty string means "no filter"; escape_like prevents % / \ / _ in user input from over-matching
        let prefix_pat = kind_prefix
            .filter(|s| !s.is_empty())
            .map(|p| format!("{}%", escape_like(p)));
        let query_pat = query
            .filter(|s| !s.is_empty())
            .map(|q| format!("%{}%", escape_like(q)));
        // 0 means "no lower/upper bound" (real event timestamps > 0)
        let since_v = since.unwrap_or(0);
        let until_v = until.unwrap_or(0);
        let prefix_pat_str = prefix_pat.as_deref().unwrap_or("");
        let query_pat_str = query_pat.as_deref().unwrap_or("");

        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, type, source, timestamp, payload FROM events
             WHERE (?1 = '' OR type LIKE ?1 ESCAPE '\\')
               AND (?2 = '' OR type LIKE ?2 ESCAPE '\\' OR payload LIKE ?2 ESCAPE '\\')
               AND (?3 = 0 OR timestamp >= ?3)
               AND (?4 = 0 OR timestamp <= ?4)
             ORDER BY timestamp DESC LIMIT ?5",
        ) else {
            return Vec::new();
        };
        let it = stmt.query_map(
            rusqlite::params![
                prefix_pat_str,
                query_pat_str,
                since_v as i64,
                until_v as i64,
                limit as i64,
            ],
            |r| {
                let payload: String = r.get(4)?;
                Ok(OpencapxEvent {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    source: r.get(2)?,
                    timestamp: r.get(3)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                })
            },
        );
        it.ok()
            .map(|i| i.filter_map(|x| x.ok()).collect())
            .unwrap_or_default()
    }

    fn prune_events(&mut self, now: u64) -> rusqlite::Result<()> {
        let cutoff = now.saturating_sub(EVENT_RETENTION_DAYS * 86400);
        self.conn
            .execute("DELETE FROM events WHERE timestamp < ?1", [cutoff])?;
        Ok(())
    }

    fn state_from_str(s: &str) -> AgentState {
        match s {
            "working" => AgentState::Working,
            "waiting" => AgentState::Waiting,
            "done" => AgentState::Done,
            _ => AgentState::Idle,
        }
    }

    /// Session history (descending, most recently ended first).
    pub fn list_session_archive(&self, limit: usize) -> Vec<crate::core::agent::ArchivedSession> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, agent, project, message, state, model, started_at, ended_at
             FROM session_archive ORDER BY ended_at DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([limit as i64], |r| {
            let started: u64 = r.get(6)?;
            let ended: u64 = r.get(7)?;
            Ok(crate::core::agent::ArchivedSession {
                id: r.get(0)?,
                agent: r.get(1)?,
                project: r.get(2)?,
                message: r.get(3)?,
                state: r.get(4)?,
                model: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                started_at: started,
                ended_at: ended,
                duration: ended.saturating_sub(started),
            })
        });
        match rows {
            Ok(it) => it.filter_map(|x| x.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Delete archives past the retention period (default 90 days). Returns the number deleted.
    pub fn prune_session_archive(&self, now: u64) -> usize {
        let cutoff = now.saturating_sub(ARCHIVE_KEEP_DAYS * 86_400);
        self.conn
            .execute("DELETE FROM session_archive WHERE ended_at < ?1", [cutoff])
            .unwrap_or(0)
    }
}
