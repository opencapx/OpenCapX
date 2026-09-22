//! SQLite storage: sessions, event audit ring, plugin/permission/capability tables (used by P4).
//! See docs/plugin-manifest.md, docs/events.md.

use super::agent::{state_str, AgentState, Session, SessionSink};
use super::event::OpencapxEvent;
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const EVENT_RETENTION_DAYS: u64 = 14;

/// Acquire the shared_store lock, call `f(&mut StoreEnum)`, returns None when there is no store. Added in Phase 53.
/// Usage: `let row = with_store(|s| s.list_alerting_severity_hints())?;`
pub fn with_store<R>(f: impl FnOnce(&mut StoreEnum) -> R) -> Option<R> {
    let store = super::shared_store()?;
    let mut guard = store.lock().ok()?;
    Some(f(&mut *guard))
}

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
        let _ = self.conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN speech TEXT NOT NULL DEFAULT ''",
        );
        // Session's full working directory (bubble grouping + branch lookup). A separate batch:
        // rusqlite's execute_batch stops at the first failing statement,
        // so a statement mixed with an existing column would skip cwd "because speech already exists".
        let _ = self.conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN cwd TEXT NOT NULL DEFAULT ''",
        );
        // Phase 37: Probe self-check — status text (passed/failed/pending) + timestamp + JSON report.
        let _ = self.conn.execute_batch(
            "ALTER TABLE plugins ADD COLUMN probe_status TEXT;
             ALTER TABLE plugins ADD COLUMN probe_at INTEGER;
             ALTER TABLE plugins ADD COLUMN probe_report TEXT",
        );
        // Migration backfill: auto_reload is a later-added column — old DBs lack it at CREATE, while plugin::list()'s
        // SELECT references it directly; a missing column makes the whole plugin list read empty (startup restore then fails).
        // A separate batch: execute_batch stops at the first failing statement, so it cannot be merged into the group above.
        let _ = self.conn.execute_batch(
            "ALTER TABLE plugins ADD COLUMN auto_reload INTEGER NOT NULL DEFAULT 0",
        );
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
        let _ = self.conn.execute_batch(
            "ALTER TABLE capability_declarations ADD COLUMN timeout_secs INTEGER",
        );
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
        ) else { return Vec::new() };
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
        it.ok().map(|i| i.filter_map(|x| x.ok()).collect()).unwrap_or_default()
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
        ) else { return Vec::new() };
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
        it.ok().map(|i| i.filter_map(|x| x.ok()).collect()).unwrap_or_default()
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
        let _ = self.conn.execute("DELETE FROM sessions WHERE id = ?1", [id]);
    }

    fn clear(&mut self) {
        let _ = self.conn.execute("DELETE FROM sessions", []);
    }
}

/// Archive retention days (session history).
pub const ARCHIVE_KEEP_DAYS: u64 = 90;

impl Storage {
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

/// The storage the app actually uses: SQLite preferred, falls back to memory if it cannot open.
pub enum StoreEnum {
    Db(Storage),
    Mem(super::agent::SessionStore),
}

impl SessionSink for StoreEnum {
    fn upsert(&mut self, s: Session) {
        match self {
            StoreEnum::Db(x) => x.upsert(s),
            StoreEnum::Mem(x) => x.upsert(s),
        }
    }

    fn get(&self, id: &str) -> Option<Session> {
        match self {
            StoreEnum::Db(x) => x.get(id),
            StoreEnum::Mem(x) => x.get(id),
        }
    }

    fn active(&self, now: u64) -> Vec<Session> {
        match self {
            StoreEnum::Db(x) => x.active(now),
            StoreEnum::Mem(x) => x.active(now),
        }
    }

    fn all(&self) -> Vec<Session> {
        match self {
            StoreEnum::Db(x) => x.all(),
            StoreEnum::Mem(x) => x.all(),
        }
    }

    fn dismiss(&mut self, id: &str) {
        match self {
            StoreEnum::Db(x) => x.dismiss(id),
            StoreEnum::Mem(x) => x.dismiss(id),
        }
    }

    fn clear(&mut self) {
        match self {
            StoreEnum::Db(x) => x.clear(),
            StoreEnum::Mem(x) => x.clear(),
        }
    }

    fn sweep(&mut self, now: u64) -> usize {
        match self {
            StoreEnum::Db(x) => x.sweep(now),
            StoreEnum::Mem(x) => x.sweep(now),
        }
    }
}

impl StoreEnum {
    /// Session history (descending). The Mem variant has no archive, returns empty.
    pub fn list_session_archive(&self, limit: usize) -> Vec<crate::core::agent::ArchivedSession> {
        match self {
            StoreEnum::Db(s) => s.list_session_archive(limit),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Clean up archives past the retention period (the Mem variant is a no-op).
    pub fn prune_session_archive(&self, now: u64) -> usize {
        match self {
            StoreEnum::Db(s) => s.prune_session_archive(now),
            StoreEnum::Mem(_) => 0,
        }
    }

    pub fn log_event(&mut self, e: &OpencapxEvent) {
        if let StoreEnum::Db(x) = self {
            x.insert_event(e);
        }
    }

    /// Recent events (the Mem variant returns empty). Used by the audit timeline / SSE cold-start replay.
    pub fn list_events(&self, kind_prefix: &str, limit: usize) -> Vec<OpencapxEvent> {
        match self {
            StoreEnum::Db(s) => s.list_events(kind_prefix, limit),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Phase 35 — Audit search/filter (kind prefix + query + time range). The Mem variant returns empty.
    pub fn list_events_filtered(
        &self,
        kind_prefix: Option<&str>,
        query: Option<&str>,
        since: Option<u64>,
        until: Option<u64>,
        limit: usize,
    ) -> Vec<OpencapxEvent> {
        match self {
            StoreEnum::Db(s) => s.list_events_filtered(kind_prefix, query, since, until, limit),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Read-only SQL escape hatch (the Mem variant returns None). Used by plugin/permission/capability tables.
    pub fn with_conn_ref<R>(&self, f: impl FnOnce(&rusqlite::Connection) -> R) -> Option<R> {
        match self {
            StoreEnum::Db(s) => Some(f(&s.conn)),
            StoreEnum::Mem(_) => None,
        }
    }

    /// Writable SQL escape hatch, returns the number of affected rows.
    pub fn with_conn(&mut self, f: impl FnOnce(&rusqlite::Connection) -> usize) -> Option<usize> {
        match self {
            StoreEnum::Db(s) => Some(f(&s.conn)),
            StoreEnum::Mem(_) => None,
        }
    }

    /// Writable SQL escape hatch (**fallible version**): used by paths needing transactions + error propagation.
    /// The Mem variant returns None (the caller treats it as "storage unavailable").
    pub fn try_with_conn<R>(
        &mut self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<R, String>,
    ) -> Option<Result<R, String>> {
        match self {
            StoreEnum::Db(s) => Some(f(&s.conn)),
            StoreEnum::Mem(_) => None,
        }
    }

    #[allow(dead_code)]
    pub fn is_db(&self) -> bool {
        matches!(self, StoreEnum::Db(_))
    }
}

/// Plugin rating (shared by the marketplace / installed plugins). Plugin Rating DTO, exposed to Tauri.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginRating {
    pub id: String,
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub score: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub comment: Option<String>,
    pub ts: u64,
}

/// Phase 48 — one dead-letter delivery record. `payload` is a JSON string; the frontend parses it as needed.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedDeliveryRow {
    pub id: String,
    pub source: String,
    pub url: String,
    pub payload: String,
    pub first_attempt_ts: u64,
    pub last_attempt_ts: u64,
    pub attempts: u32,
    pub max_attempts: u32,
    pub last_error: String,
    pub next_retry_ts: u64,
    pub state: String,
    /// Phase 49: the corresponding endpoint's id (may be None = legacy data from the Phase 47 single-endpoint path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_id: Option<String>,
}

/// Phase 67 — alert recipient config (webhook / log:stderr / log:file / email:smtp).
/// `config` is a `serde_json::Value`; the schema differs per kind (jointly constrained by the UI form + import bundle).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(default = "default_recipient_config_json")]
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: u64,
}

fn default_recipient_config_json() -> serde_json::Value {
    serde_json::json!({})
}

/// Phase 49 — a webhook endpoint config.
/// `headers_json` / `source_filter_json` are the JSON-serialized forms of Vec<(String,String)> / Vec<String>;
/// read out by deserializing, written in by serializing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertingEndpointRow {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub headers: Vec<(String, String)>,
    /// HMAC-SHA256 shared secret (empty = no signing). Stored in plaintext in SQLite (single-machine local, consistent with the other settings_kv).
    pub secret: String,
    /// Empty = accept all sources; non-empty = only these sources trigger.
    pub source_filter: Vec<String>,
    pub created_at: u64,
    /// Phase 52: `0` = Phase 47/49 legacy payload; `1` = canonical envelope. Default 0.
    #[serde(default)]
    pub schema_version: u32,
    /// Phase 57: per-endpoint template (`None` = use the Phase 47-56 default envelope; `Some(...)` = render this template
    /// to override the default body; supports `{{source}}` / `{{severity}}` / `{{timestamp}}` / `{{payload.x}}` + `{{#if expr}}`
    /// + `{{#each path}}`). See `core::alerting::render_template`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Phase 58: the sample JSON the user fills in while editing a template in the settings UI, used as the input envelope for live preview.
    /// `None` = use an empty `{}` as the payload during preview (only top-level fields are rendered). 16 KB limit.
    #[serde(default, rename = "templateSample", skip_serializing_if = "Option::is_none")]
    pub template_sample: Option<String>,
    /// Phase 72 — per-source severity override, a JSON-serialized `Vec<(source, severity)>`.
    /// `None` or a parse failure → the endpoint is treated as having no override (uses the propagation result).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity_overrides: Option<String>,
}

/// Phase 50 — a silence rule.
/// `starts_at`/`ends_at` are unix seconds; `weekdays` is a bitmask (Mon=1, Tue=2, ..., Sun=64);
/// `start_hour`/`end_hour` are local hours in 0..=24 (>=0 and <= end; for crossing midnight use start<end,
/// the current implementation does not support a 23→1 wrap). `kind_pattern` supports `*` match-all / `prefix.*` prefix match / exact match.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SilenceRuleRow {
    pub id: String,
    pub name: String,
    pub kind_pattern: String,
    pub starts_at: u64,
    pub ends_at: u64,
    pub weekdays: u8,
    pub start_hour: u8,
    pub end_hour: u8,
    pub created_at: u64,
}

/// Phase 50 — an ack suppression record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckRuleRow {
    pub id: String,
    pub kind_pattern: String,
    pub ack_until: u64,
    pub created_at: u64,
}

/// Phase 51 — a DSL route rule.
/// `kind_pattern` matches the source kind (same Phase 50 semantics); `payload_path` + `payload_match` optionally
/// match payload fields (simple substring/regex); `target_endpoint_ids` is the endpoint id list (only these are sent on a hit),
/// `tags` are attached labels (can be surfaced in payload wrapping; currently metadata only). Lower `priority` matches first; the first enabled matching rule wins.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteRuleRow {
    pub id: String,
    pub name: String,
    pub priority: i32,
    pub enabled: bool,
    pub kind_pattern: String,
    pub payload_path: Option<String>,
    pub payload_match: Option<String>,
    pub target_endpoint_ids: Vec<String>,
    /// Phase 66 — multi-channel recipient refs (each spec resolved by notification::resolve_recipient)
    pub recipients: Vec<String>,
    pub tags: Vec<String>,
    /// Phase 68 — time-window condition (a serde_json-serialized `SeenInLastSpec` or `null`).
    pub seen_in_last_json: Option<String>,
    pub created_at: u64,
}

/// Phase 59 — a template preset (user-defined; the 5 built-ins live in the `core::alerting::BUILTIN_PRESETS` constant).
/// `kind` distinguishes builtin / user: builtin uses `"builtin:<slug>"`, user uses `"user:<uuid>"` (user-modifiable);
/// `builtin=true` is also stored in SQLite, but `delete_template_preset` only deletes builtin=0 rows (to avoid accidental deletion).
/// Phase 60 — adds `version: u32` (auto-bumps on each save of the same id) + `changelog: Option<String>` (optional note).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplatePresetRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub kind: String,
    pub template: String,
    pub sample: Option<String>,
    pub builtin: bool,
    pub version: u32,
    pub changelog: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// Phase 53 — Per-source severity hint。
/// `origin = "manifest"` comes from the plugin manifest (`alerting.severityHints`) and is cleaned up when the plugin is uninstalled;
/// `origin = "user"` comes from manual user config in the settings UI and can be added/removed independently; `severity` is a lowercase string.
/// `plugin_id` has a value only for the manifest origin; `updated_at` is epoch seconds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeverityHintRow {
    pub source: String,
    pub severity: String,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    pub updated_at: i64,
}

/// Phase 54 — frequency-threshold aggregation rule. `action` decides what happens after M hits within N seconds:
/// - `downgrade` → downgrade envelope.severity to `target_severity`
/// - `suppress` → skip this dispatch entirely (no webhook sent)
/// - `merge` → rewrite envelope.payload into a `{merged_count, since, last_payload}` summary description
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregationRuleRow {
    pub id: String,
    pub name: String,
    pub kind_pattern: String,
    pub window_secs: u64,
    pub threshold_count: u32,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_severity: Option<String>,
    pub enabled: bool,
    pub created_at: u64,
}

/// Phase 55 — a correlation suppression rule: B is suppressed within window_secs after A occurs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrelationRuleRow {
    pub id: String,
    pub name: String,
    #[serde(rename = "kindPatternA")]
    pub kind_pattern_a: String,
    #[serde(rename = "kindPatternB")]
    pub kind_pattern_b: String,
    #[serde(rename = "windowSecs")]
    pub window_secs: u64,
    pub enabled: bool,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// Phase 56 — an escalation rule: while source keeps firing within escalate_after_secs and is unacked, escalate to target_severity and send to target_endpoint_ids (empty fans out like a normal dispatch).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationRuleRow {
    pub id: String,
    pub name: String,
    #[serde(rename = "kindPattern")]
    pub kind_pattern: String,
    #[serde(rename = "escalateAfterSecs")]
    pub escalate_after_secs: u64,
    #[serde(rename = "targetSeverity")]
    pub target_severity: String,
    #[serde(rename = "targetEndpointIds", skip_serializing_if = "Option::is_none")]
    pub target_endpoint_ids: Option<Vec<String>>,
    pub enabled: bool,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// Rating summary (count + average, rounded to 2 decimals).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginRatingSummary {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub count: i64,
    pub avg: f64,
}

/// A single capability stat row (for the Phase 30 Dashboard).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CapabilityStat {
    pub capability: String,
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub count: i64,
    #[serde(rename = "avgMs")]
    pub avg_ms: f64,
    #[serde(rename = "p50Ms")]
    pub p50_ms: i64,
    #[serde(rename = "p95Ms")]
    pub p95_ms: i64,
    #[serde(rename = "lastUsedAt")]
    pub last_used_at: u64,
    /// Number of non-ok samples (result != 'ok').
    #[serde(rename = "failCount")]
    pub fail_count: i64,
    /// Failure counts aggregated by error_kind (only samples with result != 'ok').
    #[serde(rename = "errors", skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub errors: std::collections::BTreeMap<String, i64>,
}

impl Storage {
    /// Add a rating; returns the generated row. An out-of-range score returns Err (so the caller reports 4xx).
    pub fn add_rating(
        &mut self,
        plugin_id: &str,
        score: i64,
        comment: Option<&str>,
        ts: u64,
    ) -> Result<PluginRating, String> {
        if !(1..=5).contains(&score) {
            return Err(format!("score out of range: {}", score));
        }
        let id = format!("rat-{}-{}", ts, plugin_id);
        let comment_owned = comment.map(|s| s.to_string());
        let res = self.conn.execute(
            "INSERT INTO plugin_ratings (id, plugin_id, score, comment, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, plugin_id, score, &comment_owned, ts as i64],
        );
        match res {
            Ok(_) => Ok(PluginRating {
                id,
                plugin_id: plugin_id.to_string(),
                score,
                comment: comment_owned,
                ts,
            }),
            Err(e) => Err(format!("insert rating: {}", e)),
        }
    }

    pub fn list_ratings(&self, plugin_id: &str, limit: usize) -> Vec<PluginRating> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, plugin_id, score, comment, ts FROM plugin_ratings
             WHERE plugin_id = ?1 ORDER BY ts DESC LIMIT ?2",
        ) else { return Vec::new() };
        stmt.query_map(rusqlite::params![plugin_id, limit as i64], |r| {
            Ok(PluginRating {
                id: r.get(0)?,
                plugin_id: r.get(1)?,
                score: r.get(2)?,
                comment: r.get(3)?,
                ts: r.get::<_, i64>(4)? as u64,
            })
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    pub fn rating_summary(&self, plugin_id: &str) -> PluginRatingSummary {
        let mut count: i64 = 0;
        let mut avg = 0.0_f64;
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT COUNT(*), COALESCE(AVG(score), 0) FROM plugin_ratings WHERE plugin_id = ?1",
        ) {
            if let Ok(rows) = stmt.query_map(rusqlite::params![plugin_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
            }) {
                if let Some(Ok((c, a))) = rows.into_iter().next() {
                    count = c;
                    avg = (a * 100.0).round() / 100.0;
                }
            }
        }
        PluginRatingSummary {
            plugin_id: plugin_id.to_string(),
            count,
            avg,
        }
    }

    /// Phase 37 — Probe self-check: write a report row (overwrite).
    /// status is expected to be "pending" / "passed" / "failed"; report is the full JSON-serialized ProbeReport.
    pub fn set_probe_status(
        &mut self,
        plugin_id: &str,
        status: &str,
        report: Option<&str>,
        at: u64,
    ) {
        let _ = self.conn.execute(
            "UPDATE plugins SET probe_status = ?2, probe_at = ?3, probe_report = ?4 WHERE id = ?1",
            rusqlite::params![plugin_id, status, at as i64, report],
        );
    }

    /// Phase 37 — read the most recent probe report. Returns (status, at, report_json).
    pub fn get_probe_report(&self, plugin_id: &str) -> Option<(String, u64, String)> {
        let mut stmt = self.conn.prepare(
            "SELECT probe_status, probe_at, probe_report FROM plugins WHERE id = ?1",
        ).ok()?;
        stmt.query_row(rusqlite::params![plugin_id], |r| {
            let status: Option<String> = r.get(0)?;
            let at: Option<i64> = r.get(1)?;
            let report: Option<String> = r.get(2)?;
            Ok((status.unwrap_or_default(), at.unwrap_or(0) as u64, report.unwrap_or_default()))
        })
        .ok()
        .filter(|(s, _, _)| !s.is_empty())
    }

    /// Phase 38 — write a plugin's channel (`channel` is already one of the stable stable/beta/dev, normalized by the caller).
    pub fn set_plugin_channel(&mut self, plugin_id: &str, channel: &str) {
        let _ = self.conn.execute(
            "INSERT INTO plugin_channel (plugin_id, channel, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(plugin_id) DO UPDATE SET channel=?2, updated_at=?3",
            rusqlite::params![plugin_id, channel, super::agent::now_secs() as i64],
        );
    }

    /// Phase 38 — fetch all (plugin_id, channel) at once. Used for batch filtering in check_plugin_updates.
    pub fn list_plugin_channels(&self) -> Vec<(String, String)> {
        let Ok(mut stmt) = self.conn.prepare("SELECT plugin_id, channel FROM plugin_channel") else {
            return Vec::new();
        };
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .ok()
            .map(|i| i.filter_map(|x| x.ok()).collect())
            .unwrap_or_default()
    }

    /// Phase 40 — read a plugin's health config (all columns allow NULL; missing values use the default).
    pub fn get_health_config(&self, plugin_id: &str) -> super::health::PluginHealthConfig {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT heartbeat_sec, ping_timeout_ms, max_retries, backoff_initial_ms, enabled
             FROM plugin_health_config WHERE plugin_id = ?1",
        ) else {
            return super::health::PluginHealthConfig::default();
        };
        stmt.query_row(rusqlite::params![plugin_id], |r| {
            Ok(super::health::config_from_row(
                r.get::<_, Option<i64>>(0).ok().flatten(),
                r.get::<_, Option<i64>>(1).ok().flatten(),
                r.get::<_, Option<i64>>(2).ok().flatten(),
                r.get::<_, Option<i64>>(3).ok().flatten(),
                r.get::<_, Option<i64>>(4).ok().flatten(),
            ))
        })
        .unwrap_or_default()
    }

    /// Phase 40 — write a plugin's health config (upsert). The caller validates with validate first.
    pub fn upsert_health_config(
        &mut self,
        plugin_id: &str,
        cfg: &super::health::PluginHealthConfig,
    ) {
        let now = super::agent::now_secs() as i64;
        let _ = self.conn.execute(
            "INSERT INTO plugin_health_config
                (plugin_id, heartbeat_sec, ping_timeout_ms, max_retries, backoff_initial_ms, enabled, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(plugin_id) DO UPDATE SET
                heartbeat_sec=?2, ping_timeout_ms=?3, max_retries=?4,
                backoff_initial_ms=?5, enabled=?6, updated_at=?7",
            rusqlite::params![
                plugin_id,
                cfg.heartbeat_sec as i64,
                cfg.ping_timeout_ms as i64,
                cfg.max_retries as i64,
                cfg.backoff_initial_ms as i64,
                if cfg.enabled { 1 } else { 0 },
                now,
            ],
        );
    }

    /// Phase 40 — fetch all plugin_health_config rows at once. Used by the list command for batch returns.
    pub fn list_health_configs(&self) -> Vec<(String, super::health::PluginHealthConfig)> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT plugin_id, heartbeat_sec, ping_timeout_ms, max_retries, backoff_initial_ms, enabled
             FROM plugin_health_config",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            let pid: String = r.get(0)?;
            let cfg = super::health::config_from_row(
                r.get::<_, Option<i64>>(1).ok().flatten(),
                r.get::<_, Option<i64>>(2).ok().flatten(),
                r.get::<_, Option<i64>>(3).ok().flatten(),
                r.get::<_, Option<i64>>(4).ok().flatten(),
                r.get::<_, Option<i64>>(5).ok().flatten(),
            );
            Ok((pid, cfg))
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// Phase 38 — global key/value config (used for defaultChannel). A nonexistent key returns None.
    pub fn get_setting(&self, key: &str) -> Option<String> {
        let mut stmt = self.conn.prepare("SELECT v FROM settings_kv WHERE k = ?1").ok()?;
        stmt.query_row(rusqlite::params![key], |r| r.get::<_, String>(0)).ok()
    }

    /// Phase 38 — write global key/value.
    pub fn set_setting(&mut self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO settings_kv (k, v) VALUES (?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v=?2",
            rusqlite::params![key, value],
        );
    }

    /// Phase 45 — write a metrics sample. Returns the rowid.
    pub fn insert_metrics_sample(
        &mut self,
        plugin_id: &str,
        ts: i64,
        cpu_pct: f64,
        rss_bytes: i64,
        threads: i64,
        fds: i64,
    ) {
        let _ = self.conn.execute(
            "INSERT INTO plugin_metrics_samples (plugin_id, ts, cpu_pct, rss_bytes, threads, fds)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![plugin_id, ts, cpu_pct, rss_bytes, threads, fds],
        );
    }

    /// Phase 45 — fetch a plugin's historical samples after from_ts (or all if from_ts=0), by ts ascending.
    pub fn list_metrics_samples(
        &self,
        plugin_id: &str,
        from_ts: i64,
        limit: usize,
    ) -> Vec<(i64, f64, i64, i64, i64)> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT ts, cpu_pct, rss_bytes, threads, fds FROM plugin_metrics_samples
             WHERE plugin_id = ?1 AND ts >= ?2 ORDER BY ts DESC LIMIT ?3",
        ) else {
            return Vec::new();
        };
        let mut rows: Vec<(i64, f64, i64, i64, i64)> = stmt
            .query_map(
                rusqlite::params![plugin_id, from_ts, limit as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .ok()
            .map(|i| i.filter_map(|x| x.ok()).collect())
            .unwrap_or_default();
        rows.reverse();
        rows
    }

    /// Phase 45 — keep only the latest 1500 rows per plugin (discard older ones beyond that). Batch-deletes the oldest.
    pub fn prune_metrics_samples(&mut self, keep_per_plugin: usize) {
        let _ = self.conn.execute_batch(&format!(
            "DELETE FROM plugin_metrics_samples WHERE id IN (
                SELECT id FROM plugin_metrics_samples m1
                WHERE (SELECT COUNT(*) FROM plugin_metrics_samples m2
                       WHERE m2.plugin_id = m1.plugin_id AND m2.id >= m1.id) > {keep}
             )",
            keep = keep_per_plugin as i64,
        ));
    }

    // ─── Phase 48: alerting_failed_deliveries ──────────────────────────────────

    /// Write a dead-letter record (state='pending'). `id` is generated by the caller (nano timestamp or uuid).
    /// Phase 49 adds `endpoint_id`: None means it came from the Phase 47 single-endpoint compatibility path.
    pub fn insert_failed_delivery(
        &mut self,
        id: &str,
        source: &str,
        url: &str,
        payload_json: &str,
        now_ts: u64,
        max_attempts: u32,
        next_retry_ts: u64,
        last_error: &str,
        endpoint_id: Option<&str>,
    ) {
        let _ = self.conn.execute(
            "INSERT INTO alerting_failed_deliveries
                (id, source, url, payload, first_attempt_ts, last_attempt_ts,
                 attempts, max_attempts, last_error, next_retry_ts, state, endpoint_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, 1, ?6, ?7, ?8, 'pending', ?9)",
            rusqlite::params![
                id,
                source,
                url,
                payload_json,
                now_ts as i64,
                max_attempts as i64,
                last_error,
                next_retry_ts as i64,
                endpoint_id,
            ],
        );
    }

    /// List dead letters for a given state (state nullable: "pending"/"exhausted"/"resolved"). By next_retry_ts ascending.
    pub fn list_failed_deliveries(
        &self,
        state: Option<&str>,
        limit: usize,
    ) -> Vec<FailedDeliveryRow> {
        let limit = limit.max(1).min(2000);
        let (sql, use_state): (&str, bool) = match state {
            Some(s) if !s.is_empty() => (
                "SELECT id, source, url, payload, first_attempt_ts, last_attempt_ts,
                        attempts, max_attempts, last_error, next_retry_ts, state, endpoint_id
                   FROM alerting_failed_deliveries
                   WHERE state = ?1
                   ORDER BY next_retry_ts ASC, last_attempt_ts DESC
                   LIMIT ?2",
                true,
            ),
            _ => (
                "SELECT id, source, url, payload, first_attempt_ts, last_attempt_ts,
                        attempts, max_attempts, last_error, next_retry_ts, state, endpoint_id
                   FROM alerting_failed_deliveries
                   ORDER BY next_retry_ts ASC, last_attempt_ts DESC
                   LIMIT ?1",
                false,
            ),
        };
        let Ok(mut stmt) = self.conn.prepare(sql) else {
            return Vec::new();
        };
        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<FailedDeliveryRow> {
            let payload: String = r.get(3)?;
            Ok(FailedDeliveryRow {
                id: r.get(0)?,
                source: r.get(1)?,
                url: r.get(2)?,
                payload,
                first_attempt_ts: r.get::<_, i64>(4)? as u64,
                last_attempt_ts: r.get::<_, i64>(5)? as u64,
                attempts: r.get::<_, i64>(6)? as u32,
                max_attempts: r.get::<_, i64>(7)? as u32,
                last_error: r.get(8)?,
                next_retry_ts: r.get::<_, i64>(9)? as u64,
                state: r.get(10)?,
                endpoint_id: r.get(11)?,
            })
        };
        if use_state {
            stmt.query_map(rusqlite::params![state.unwrap(), limit as i64], mapper)
                .ok()
                .map(|i| i.filter_map(|x| x.ok()).collect())
                .unwrap_or_default()
        } else {
            stmt.query_map(rusqlite::params![limit as i64], mapper)
                .ok()
                .map(|i| i.filter_map(|x| x.ok()).collect())
                .unwrap_or_default()
        }
    }

    /// Fetch due pending rows (called by the retry loop). limit defaults to 50 to avoid scanning too many at once.
    pub fn fetch_due_failed_deliveries(
        &self,
        now_ts: u64,
        limit: usize,
    ) -> Vec<FailedDeliveryRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, source, url, payload, first_attempt_ts, last_attempt_ts,
                    attempts, max_attempts, last_error, next_retry_ts, state, endpoint_id
               FROM alerting_failed_deliveries
               WHERE state = 'pending' AND next_retry_ts <= ?1
               ORDER BY next_retry_ts ASC
               LIMIT ?2",
        ) else {
            return Vec::new();
        };
        stmt.query_map(rusqlite::params![now_ts as i64, limit as i64], |r| {
            let payload: String = r.get(3)?;
            Ok(FailedDeliveryRow {
                id: r.get(0)?,
                source: r.get(1)?,
                url: r.get(2)?,
                payload,
                first_attempt_ts: r.get::<_, i64>(4)? as u64,
                last_attempt_ts: r.get::<_, i64>(5)? as u64,
                attempts: r.get::<_, i64>(6)? as u32,
                max_attempts: r.get::<_, i64>(7)? as u32,
                last_error: r.get(8)?,
                next_retry_ts: r.get::<_, i64>(9)? as u64,
                state: r.get(10)?,
                endpoint_id: r.get(11)?,
            })
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// Phase 49: delete all dead letters for the given endpoint (cascade cleanup).
    pub fn delete_failed_deliveries_for_endpoint(&mut self, endpoint_id: &str) -> usize {
        self.conn
            .execute(
                "DELETE FROM alerting_failed_deliveries WHERE endpoint_id = ?1",
                rusqlite::params![endpoint_id],
            )
            .unwrap_or(0)
    }

    /// Update a failed retry: attempts++, refreshing last_attempt_ts / last_error / next_retry_ts.
    /// Passing next_retry_ts = 0 means "exhausted"; state switches to 'exhausted'.
    pub fn update_failed_delivery_retry(
        &mut self,
        id: &str,
        now_ts: u64,
        next_retry_ts: u64,
        last_error: &str,
    ) {
        let _ = self.conn.execute(
            "UPDATE alerting_failed_deliveries
                SET attempts = attempts + 1,
                    last_attempt_ts = ?2,
                    last_error = ?3,
                    next_retry_ts = ?4,
                    state = CASE WHEN ?4 = 0 THEN 'exhausted' ELSE 'pending' END
              WHERE id = ?1",
            rusqlite::params![id, now_ts as i64, last_error, next_retry_ts as i64],
        );
    }

    /// Mark as resolved (2xx response). state='resolved', kept for N days before cleanup.
    pub fn mark_failed_delivery_resolved(&mut self, id: &str, now_ts: u64) {
        let _ = self.conn.execute(
            "UPDATE alerting_failed_deliveries
                SET state = 'resolved', last_attempt_ts = ?2
              WHERE id = ?1",
            rusqlite::params![id, now_ts as i64],
        );
    }

    /// Manual retry: reset attempts to zero, state back to 'pending', next_retry_ts=now.
    pub fn reset_failed_delivery_for_retry(&mut self, id: &str, now_ts: u64) {
        let _ = self.conn.execute(
            "UPDATE alerting_failed_deliveries
                SET state = 'pending', attempts = 0, next_retry_ts = ?2, last_error = 'manual retry'
              WHERE id = ?1",
            rusqlite::params![id, now_ts as i64],
        );
    }

    /// Delete a dead letter.
    pub fn delete_failed_delivery(&mut self, id: &str) -> bool {
        let n = self
            .conn
            .execute(
                "DELETE FROM alerting_failed_deliveries WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0);
        n > 0
    }

    /// Clear all exhausted + resolved rows (user manually empties the panel).
    pub fn clear_resolved_failed_deliveries(&mut self) -> usize {
        self.conn
            .execute(
                "DELETE FROM alerting_failed_deliveries WHERE state IN ('exhausted','resolved')",
                [],
            )
            .unwrap_or(0)
    }

    /// Prune resolved / exhausted rows older than retention_days (to avoid long-term buildup).
    pub fn prune_old_failed_deliveries(&mut self, now_ts: u64, retention_days: u32) {
        let cutoff = now_ts.saturating_sub((retention_days as u64).saturating_mul(86400));
        let _ = self.conn.execute(
            "DELETE FROM alerting_failed_deliveries
              WHERE state IN ('exhausted','resolved') AND last_attempt_ts < ?1",
            rusqlite::params![cutoff as i64],
        );
    }

    // ─── Phase 49: alerting_endpoints multi-endpoint fanout ──────────────────────

    fn parse_endpoint_row(
        &self,
        id: String,
        name: String,
        url: String,
        enabled: i64,
        headers_json: String,
        secret: String,
        source_filter_json: String,
        created_at: i64,
        schema_version: i64,
        template: Option<String>,
        template_sample: Option<String>,
        severity_overrides: Option<String>,
    ) -> AlertingEndpointRow {
        let headers: Vec<(String, String)> =
            serde_json::from_str(&headers_json).unwrap_or_default();
        let source_filter: Vec<String> =
            serde_json::from_str(&source_filter_json).unwrap_or_default();
        AlertingEndpointRow {
            id,
            name,
            url,
            enabled: enabled != 0,
            headers,
            secret,
            source_filter,
            created_at: created_at as u64,
            schema_version: schema_version.max(0) as u32,
            template,
            template_sample,
            severity_overrides,
        }
    }

    /// List all endpoints, by created_at ascending.
    pub fn list_alerting_endpoints(&self) -> Vec<AlertingEndpointRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, url, enabled, headers_json, secret, source_filter_json, created_at, schema_version, template, template_sample, severity_overrides_json
               FROM alerting_endpoints ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .ok()
        .map(|i| {
            i.filter_map(|x| x.ok())
                .map(|(id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so)| {
                    self.parse_endpoint_row(id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so)
                })
                .collect()
        })
        .unwrap_or_default()
    }

    /// List all enabled endpoints — used during dispatcher fanout.
    pub fn list_enabled_alerting_endpoints(&self) -> Vec<AlertingEndpointRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, url, enabled, headers_json, secret, source_filter_json, created_at, schema_version, template, template_sample, severity_overrides_json
               FROM alerting_endpoints WHERE enabled = 1 ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .ok()
        .map(|i| {
            i.filter_map(|x| x.ok())
                .map(|(id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so)| {
                    self.parse_endpoint_row(id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so)
                })
                .collect()
        })
        .unwrap_or_default()
    }

    /// Get a single endpoint by id.
    pub fn get_alerting_endpoint(&self, id: &str) -> Option<AlertingEndpointRow> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, url, enabled, headers_json, secret, source_filter_json, created_at, schema_version, template, template_sample, severity_overrides_json
                   FROM alerting_endpoints WHERE id = ?1",
            )
            .ok()?;
        stmt.query_row(rusqlite::params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .ok()
        .map(|(id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so)| {
            self.parse_endpoint_row(id, name, url, en, hj, sec, sfj, ca, sv, tpl, tpl_s, so)
        })
    }

    /// Upsert: update when there is an id, otherwise use the passed-in id (generated by the caller).
    pub fn upsert_alerting_endpoint(&mut self, row: &AlertingEndpointRow) {
        let headers_json =
            serde_json::to_string(&row.headers).unwrap_or_else(|_| "[]".into());
        let source_filter_json =
            serde_json::to_string(&row.source_filter).unwrap_or_else(|_| "[]".into());
        let _ = self.conn.execute(
            "INSERT INTO alerting_endpoints
                (id, name, url, enabled, headers_json, secret, source_filter_json, created_at, schema_version, template, template_sample, severity_overrides_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name,
                url=excluded.url,
                enabled=excluded.enabled,
                headers_json=excluded.headers_json,
                secret=excluded.secret,
                source_filter_json=excluded.source_filter_json,
                schema_version=excluded.schema_version,
                template=excluded.template,
                template_sample=excluded.template_sample,
                severity_overrides_json=excluded.severity_overrides_json",
            rusqlite::params![
                row.id,
                row.name,
                row.url,
                if row.enabled { 1i64 } else { 0i64 },
                headers_json,
                row.secret,
                source_filter_json,
                row.created_at as i64,
                row.schema_version as i64,
                row.template.as_deref(),
                row.template_sample.as_deref(),
                row.severity_overrides.as_deref(),
            ],
        );
    }

    /// Delete an endpoint, cascading cleanup of its dead letters. Returns (endpoint_deleted, deliveries_deleted).
    pub fn delete_alerting_endpoint(&mut self, id: &str) -> (bool, usize) {
        let n = self
            .conn
            .execute(
                "DELETE FROM alerting_endpoints WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0);
        let cleared = self.delete_failed_deliveries_for_endpoint(id);
        (n > 0, cleared)
    }

    // ─── Phase 59: template_presets (per-user custom template library)───────────────

    /// List user-defined presets (builtins always live in the `core::alerting::BUILTIN_PRESETS` constant).
    pub fn list_user_template_presets(&self) -> Result<Vec<TemplatePresetRow>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, kind, template, sample, builtin, version, changelog, created_at
               FROM template_presets WHERE builtin = 0 ORDER BY created_at ASC",
        ).map_err(|e| format!("prepare list_user_template_presets: {}", e))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, i64>(9)?,
            ))
        }).map_err(|e| format!("query list_user_template_presets: {}", e))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, description, kind, template, sample, builtin, version, changelog, created_at) =
                row.map_err(|e| format!("row: {}", e))?;
            out.push(TemplatePresetRow {
                id, name, description, kind, template, sample,
                builtin: builtin != 0,
                version: version.max(1) as u32,
                changelog,
                created_at: created_at.max(0) as u64,
            });
        }
        Ok(out)
    }

    /// Upsert preset; `builtin=true` is also allowed (so builtin rows can still be persisted when exported/imported).
    /// Phase 60 — adds `version` + `changelog` columns; upsert writes both (allows resetting version = 1 on import).
    pub fn upsert_template_preset(&mut self, p: &TemplatePresetRow) -> Result<(), String> {
        let _ = self.conn.execute(
            "INSERT INTO template_presets
                (id, name, description, kind, template, sample, builtin, version, changelog, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name,
                description=excluded.description,
                kind=excluded.kind,
                template=excluded.template,
                sample=excluded.sample,
                builtin=excluded.builtin,
                version=excluded.version,
                changelog=excluded.changelog",
            rusqlite::params![
                p.id,
                p.name,
                p.description.as_deref(),
                p.kind,
                p.template,
                p.sample.as_deref(),
                if p.builtin { 1i64 } else { 0i64 },
                p.version as i64,
                p.changelog.as_deref(),
                p.created_at as i64,
            ],
        ).map_err(|e| format!("upsert template_preset: {}", e))?;
        Ok(())
    }

    /// Delete only builtin=0 rows (avoid accidental deletion of built-ins). Returns true when actually deleted.
    pub fn delete_template_preset(&mut self, id: &str) -> bool {
        let n = self.conn.execute(
            "DELETE FROM template_presets WHERE id = ?1 AND builtin = 0",
            rusqlite::params![id],
        ).unwrap_or(0);
        n > 0
    }

    // ─── Phase 50: alerting_silences + alerting_acks ──────────────────────────

    /// List all silence rules, by starts_at ascending.
    pub fn list_alerting_silences(&self) -> Vec<SilenceRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern, starts_at, ends_at, weekdays, start_hour, end_hour, created_at
               FROM alerting_silences ORDER BY starts_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok(SilenceRuleRow {
                id: r.get(0)?,
                name: r.get(1)?,
                kind_pattern: r.get(2)?,
                starts_at: r.get::<_, i64>(3)? as u64,
                ends_at: r.get::<_, i64>(4)? as u64,
                weekdays: r.get::<_, i64>(5)? as u8,
                start_hour: r.get::<_, i64>(6)? as u8,
                end_hour: r.get::<_, i64>(7)? as u8,
                created_at: r.get::<_, i64>(8)? as u64,
            })
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// List all silences that could be active (ends_at > now + some buffer); the dispatch phase avoids querying all to prevent staleness.
    pub fn list_active_silences(&self, now_ts: u64) -> Vec<SilenceRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern, starts_at, ends_at, weekdays, start_hour, end_hour, created_at
               FROM alerting_silences WHERE ends_at > ?1 ORDER BY starts_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map(rusqlite::params![now_ts as i64], |r| {
            Ok(SilenceRuleRow {
                id: r.get(0)?,
                name: r.get(1)?,
                kind_pattern: r.get(2)?,
                starts_at: r.get::<_, i64>(3)? as u64,
                ends_at: r.get::<_, i64>(4)? as u64,
                weekdays: r.get::<_, i64>(5)? as u8,
                start_hour: r.get::<_, i64>(6)? as u8,
                end_hour: r.get::<_, i64>(7)? as u8,
                created_at: r.get::<_, i64>(8)? as u64,
            })
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// Upsert a silence (`id` generated by the caller).
    pub fn upsert_alerting_silence(&mut self, row: &SilenceRuleRow) {
        let _ = self.conn.execute(
            "INSERT INTO alerting_silences
                (id, name, kind_pattern, starts_at, ends_at, weekdays, start_hour, end_hour, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name,
                kind_pattern=excluded.kind_pattern,
                starts_at=excluded.starts_at,
                ends_at=excluded.ends_at,
                weekdays=excluded.weekdays,
                start_hour=excluded.start_hour,
                end_hour=excluded.end_hour",
            rusqlite::params![
                row.id,
                row.name,
                row.kind_pattern,
                row.starts_at as i64,
                row.ends_at as i64,
                row.weekdays as i64,
                row.start_hour as i64,
                row.end_hour as i64,
                row.created_at as i64,
            ],
        );
    }

    /// Delete a silence. Returns whether it was actually deleted.
    pub fn delete_alerting_silence(&mut self, id: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_silences WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0)
            > 0
    }

    /// List all ack records (may include expired ones — the UI filters display by ack_until itself).
    pub fn list_alerting_acks(&self) -> Vec<AckRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, kind_pattern, ack_until, created_at FROM alerting_acks ORDER BY ack_until DESC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok(AckRuleRow {
                id: r.get(0)?,
                kind_pattern: r.get(1)?,
                ack_until: r.get::<_, i64>(2)? as u64,
                created_at: r.get::<_, i64>(3)? as u64,
            })
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// List ack records that have not expired (used by the dispatch check).
    pub fn list_active_acks(&self, now_ts: u64) -> Vec<AckRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, kind_pattern, ack_until, created_at FROM alerting_acks WHERE ack_until > ?1",
        ) else {
            return Vec::new();
        };
        stmt.query_map(rusqlite::params![now_ts as i64], |r| {
            Ok(AckRuleRow {
                id: r.get(0)?,
                kind_pattern: r.get(1)?,
                ack_until: r.get::<_, i64>(2)? as u64,
                created_at: r.get::<_, i64>(3)? as u64,
            })
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// Upsert an ack.
    pub fn upsert_alerting_ack(&mut self, row: &AckRuleRow) {
        let _ = self.conn.execute(
            "INSERT INTO alerting_acks (id, kind_pattern, ack_until, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                kind_pattern=excluded.kind_pattern,
                ack_until=excluded.ack_until",
            rusqlite::params![
                row.id,
                row.kind_pattern,
                row.ack_until as i64,
                row.created_at as i64,
            ],
        );
    }

    /// Delete an ack.
    pub fn delete_alerting_ack(&mut self, id: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_acks WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0)
            > 0
    }

    /// Clear all expired acks (`ack_until <= now`).
    pub fn clear_expired_acks(&mut self, now_ts: u64) -> usize {
        self.conn
            .execute(
                "DELETE FROM alerting_acks WHERE ack_until <= ?1",
                rusqlite::params![now_ts as i64],
            )
            .unwrap_or(0)
    }

    // ─── Phase 51: alerting_routes (DSL when/then) ──────────────────────────────

    /// All DSL route rules (by priority ASC, then created_at ASC). Added in Phase 51.
    pub fn list_alerting_routes(&self) -> Vec<RouteRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, priority, enabled, kind_pattern, target_endpoint_ids_json,
                    payload_path, payload_match, tags_json, recipients_json, seen_in_last_json, created_at
             FROM alerting_routes ORDER BY priority ASC, created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            let ids_json: String = r.get(5)?;
            let tags_json: Option<String> = r.get(8)?;
            let recipients_json: Option<String> = r.get(9)?;
            let seen_in_last_json: Option<String> = r.get(10)?;
            let target_endpoint_ids: Vec<String> =
                serde_json::from_str(&ids_json).unwrap_or_default();
            let tags: Vec<String> = tags_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let recipients: Vec<String> = recipients_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            Ok(RouteRuleRow {
                id: r.get(0)?,
                name: r.get(1)?,
                priority: r.get(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                kind_pattern: r.get(4)?,
                payload_path: r.get(6)?,
                payload_match: r.get(7)?,
                target_endpoint_ids,
                recipients,
                tags,
                seen_in_last_json,
                created_at: r.get(11)?,
            })
        })
        .ok()
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// Upsert a route (id overwrites; created_at keeps the old value to avoid UI jumps). Added in Phase 51.
    pub fn upsert_alerting_route(&mut self, row: &RouteRuleRow) {
        let ids_json = serde_json::to_string(&row.target_endpoint_ids).unwrap_or_else(|_| "[]".into());
        let tags_json = serde_json::to_string(&row.tags).unwrap_or_else(|_| "[]".into());
        let recipients_json = serde_json::to_string(&row.recipients).unwrap_or_else(|_| "[]".into());
        let _ = self.conn.execute(
            "INSERT INTO alerting_routes
             (id, name, priority, enabled, kind_pattern, payload_path, payload_match,
              target_endpoint_ids_json, tags_json, recipients_json, seen_in_last_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name,
                priority=excluded.priority,
                enabled=excluded.enabled,
                kind_pattern=excluded.kind_pattern,
                payload_path=excluded.payload_path,
                payload_match=excluded.payload_match,
                target_endpoint_ids_json=excluded.target_endpoint_ids_json,
                tags_json=excluded.tags_json,
                recipients_json=excluded.recipients_json,
                seen_in_last_json=excluded.seen_in_last_json",
            rusqlite::params![
                row.id,
                row.name,
                row.priority,
                row.enabled as i64,
                row.kind_pattern,
                row.payload_path,
                row.payload_match,
                ids_json,
                tags_json,
                recipients_json,
                row.seen_in_last_json,
                row.created_at as i64,
            ],
        );
    }

    /// Delete a route. Added in Phase 51.
    pub fn delete_alerting_route(&mut self, id: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_routes WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0)
            > 0
    }

    /// Clear all routes (for atomic import). Added in Phase 51.
    pub fn clear_alerting_routes(&mut self) -> usize {
        self.conn
            .execute("DELETE FROM alerting_routes", [])
            .unwrap_or(0)
    }

    // ─── Phase 67: alerting_recipients ──────────────────────────────────────

    /// All recipients (by created_at ASC). Added in Phase 67.
    pub fn list_alerting_recipients(&self) -> Vec<RecipientRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind, config_json, enabled, created_at
             FROM alerting_recipients ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            let cfg: String = r.get(3)?;
            let config: serde_json::Value =
                serde_json::from_str(&cfg).unwrap_or_else(|_| serde_json::json!({}));
            Ok(RecipientRow {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                config,
                enabled: r.get::<_, i64>(4)? != 0,
                created_at: r.get::<_, i64>(5)? as u64,
            })
        })
        .ok()
        .map(|it| it.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// Upsert recipient. A `name` UNIQUE conflict → returns Err (handled by the caller's upper layer).
    /// Added in Phase 67.
    pub fn upsert_alerting_recipient(&mut self, row: &RecipientRow) -> Result<(), String> {
        let config_json = serde_json::to_string(&row.config).unwrap_or_else(|_| "{}".into());
        self.conn
            .execute(
                "INSERT INTO alerting_recipients
                    (id, name, kind, config_json, enabled, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                    name=excluded.name,
                    kind=excluded.kind,
                    config_json=excluded.config_json,
                    enabled=excluded.enabled",
                rusqlite::params![
                    row.id,
                    row.name,
                    row.kind,
                    config_json,
                    if row.enabled { 1i64 } else { 0i64 },
                    row.created_at as i64,
                ],
            )
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("UNIQUE") {
                    format!("recipient name '{}' already exists", row.name)
                } else {
                    format!("upsert recipient: {msg}")
                }
            })?;
        Ok(())
    }

    /// Delete a recipient + cascade-clean dangling refs to this id in routes.recipients.
    /// Returns (deleted, routes_cleared). Added in Phase 67.
    pub fn delete_alerting_recipient(&mut self, id: &str) -> (bool, usize) {
        let n = self
            .conn
            .execute(
                "DELETE FROM alerting_recipients WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0);
        let mut cleared = 0usize;
        if n > 0 {
            // collect routes referencing this id (`webhook:{id}` or `recipient:{id}`)
            let rows = self.list_alerting_routes();
            for row in rows {
                let before = row.recipients.len();
                let mut new_row = row.clone();
                let filtered: Vec<String> = new_row
                    .recipients
                    .drain(..)
                    .filter(|c| {
                        !(c.starts_with(&format!("webhook:{id}"))
                            || c.starts_with(&format!("recipient:{id}")))
                    })
                    .collect();
                if filtered.len() < before {
                    new_row.recipients = filtered;
                    self.upsert_alerting_route(&new_row);
                    cleared += 1;
                }
            }
        }
        (n > 0, cleared)
    }

    // ─── Phase 53: alerting_severity_hints ────────────────────────────────────

    /// All hints (by source ASC). Added in Phase 53.
    pub fn list_alerting_severity_hints(&self) -> Vec<SeverityHintRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT source, severity, origin, plugin_id, updated_at
             FROM alerting_severity_hints ORDER BY source ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok(SeverityHintRow {
                source: r.get(0)?,
                severity: r.get(1)?,
                origin: r.get(2)?,
                plugin_id: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })
        .ok()
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// Upsert a hint. Added in Phase 53.
    pub fn upsert_alerting_severity_hint(
        &mut self,
        source: &str,
        severity: &str,
        origin: &str,
        plugin_id: Option<&str>,
        updated_at: i64,
    ) -> Result<(), String> {
        let ok = self.conn.execute(
            "INSERT INTO alerting_severity_hints (source, severity, origin, plugin_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source, origin) DO UPDATE SET severity=?2, plugin_id=?4, updated_at=?5",
            rusqlite::params![source, severity, origin, plugin_id, updated_at],
        );
        match ok {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("upsert severity hint failed: {e}")),
        }
    }

    pub fn delete_alerting_severity_hint_by_source_and_origin(
        &mut self,
        source: &str,
        origin: &str,
    ) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_severity_hints WHERE source = ?1 AND origin = ?2",
                rusqlite::params![source, origin],
            )
            .unwrap_or(0)
            > 0
    }

    /// Batch-delete hints matching (origin, plugin_id), returning the delete count. Added in Phase 53.
    pub fn delete_alerting_severity_hints_by_origin_plugin(
        &mut self,
        origin: &str,
        plugin_id: &str,
    ) -> usize {
        self.conn
            .execute(
                "DELETE FROM alerting_severity_hints WHERE origin = ?1 AND plugin_id = ?2",
                rusqlite::params![origin, plugin_id],
            )
            .unwrap_or(0)
    }

    /// Clear all hints with origin user. Added in Phase 53, returns the delete count.
    pub fn clear_user_alerting_severity_hints(&mut self) -> usize {
        self.conn
            .execute(
                "DELETE FROM alerting_severity_hints WHERE origin = 'user'",
                [],
            )
            .unwrap_or(0)
    }

    /// Look up the hint matching (source, origin) (single row). Added in Phase 53.
    pub fn get_alerting_severity_hint(
        &self,
        source: &str,
        origin: &str,
    ) -> Option<SeverityHintRow> {
        let mut stmt = self.conn.prepare(
            "SELECT source, severity, origin, plugin_id, updated_at
             FROM alerting_severity_hints WHERE source = ?1 AND origin = ?2 LIMIT 1",
        ).ok()?;
        stmt.query_row(rusqlite::params![source, origin], |r| {
            Ok(SeverityHintRow {
                source: r.get(0)?,
                severity: r.get(1)?,
                origin: r.get(2)?,
                plugin_id: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })
        .ok()
    }

    /// Phase 39 — all hotkey bindings. combo is the case-normalized string (e.g. "Cmd+Shift+K").
    pub fn list_hotkeys(&self) -> Vec<HotkeyRow> {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT combo, label, payload, created_at, updated_at FROM hotkey_binding ORDER BY combo")
        else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok(HotkeyRow {
                combo: r.get(0)?,
                label: r.get(1)?,
                payload: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })
        .ok()
        .map(|i| i.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    /// Record a capability call + latency. Used by the Phase 30 dashboard.
    /// Phase 31: adds result + error_kind, failure classification.
    pub fn record_capability_call(
        &mut self,
        cap: &str,
        plugin: &str,
        elapsed_ms: i64,
        ts: u64,
        result: &str,
        error_kind: Option<&str>,
    ) {
        let _ = self.conn.execute(
            "INSERT INTO capability_stats (capability, plugin_id, elapsed_ms, ts, result, error_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![cap, plugin, elapsed_ms, ts as i64, result, error_kind],
        );
    }

    /// Aggregate the latest N calls per (capability, plugin_id) pair: count + avg + p50 + p95 + last_used + fail_count + errors.
    /// Computes percentiles in memory (N≤500 to prevent blowup), not via SQLite math functions. p50/p95 use only result='ok' samples; failed times are excluded from the latency distribution.
    pub fn capability_stats_summary(&self, samples_per_pair: usize) -> Vec<CapabilityStat> {
        let n = samples_per_pair.max(10).min(500);
        // take the latest n per pair: use a subquery to find the n newest ts for each (cap, plugin), then aggregate.
        // SQLite has no LATERAL, so use the window function ROW_NUMBER() OVER (PARTITION BY ... ORDER BY ts DESC).
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT capability, plugin_id, elapsed_ms, ts, result, error_kind FROM (
                SELECT capability, plugin_id, elapsed_ms, ts, result, error_kind,
                       ROW_NUMBER() OVER (PARTITION BY capability, plugin_id ORDER BY ts DESC) AS rn
                FROM capability_stats
             ) WHERE rn <= ?1 ORDER BY capability ASC, ts DESC",
        ) else { return Vec::new() };
        // (cap, plugin, elapsed, ts, result, error_kind)
        let rows: Vec<(String, String, i64, i64, String, Option<String>)> = stmt
            .query_map([n as i64], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .ok()
            .map(|i| i.filter_map(|x| x.ok()).collect())
            .unwrap_or_default();
        // group by (cap, plugin)
        use std::collections::BTreeMap;
        let mut groups: BTreeMap<(String, String), Vec<(i64, i64, String, Option<String>)>> =
            BTreeMap::new();
        let mut last_used: std::collections::HashMap<(String, String), u64> = Default::default();
        for (cap, plugin, elapsed, ts, result, error_kind) in rows {
            groups
                .entry((cap.clone(), plugin.clone()))
                .or_default()
                .push((elapsed, ts, result, error_kind));
            let key = (cap, plugin);
            let entry = last_used.entry(key).or_insert(0);
            if (ts as u64) > *entry {
                *entry = ts as u64;
            }
        }
        let mut out: Vec<CapabilityStat> = groups
            .into_iter()
            .map(|((capability, plugin_id), mut samples)| {
                let last_used_at = last_used
                    .remove(&(capability.clone(), plugin_id.clone()))
                    .unwrap_or(0);
                let count = samples.len() as i64;
                // compute latency percentiles from ok only (a failed timeout=60000 would pollute p95)
                let mut ok_elapsed: Vec<i64> = samples
                    .iter()
                    .filter(|s| s.2 == "ok")
                    .map(|s| s.0)
                    .collect();
                ok_elapsed.sort_unstable();
                let (avg_ms, p50_ms, p95_ms) = if ok_elapsed.is_empty() {
                    (0.0, 0, 0)
                } else {
                    let sum: i64 = ok_elapsed.iter().sum();
                    let avg = sum as f64 / ok_elapsed.len() as f64;
                    let p50 = ok_elapsed[ok_elapsed.len() / 2];
                    let p95_idx = ((ok_elapsed.len() as f64 * 0.95).ceil() as usize)
                        .saturating_sub(1)
                        .min(ok_elapsed.len() - 1);
                    let p95 = ok_elapsed[p95_idx];
                    ((avg * 100.0).round() / 100.0, p50, p95)
                };
                // failure bucketing
                let mut errors: BTreeMap<String, i64> = BTreeMap::new();
                let mut fail_count: i64 = 0;
                for (_, _, result, error_kind) in &samples {
                    if result != "ok" {
                        fail_count += 1;
                        let kind = error_kind
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());
                        *errors.entry(kind).or_insert(0) += 1;
                    }
                }
                CapabilityStat {
                    capability,
                    plugin_id,
                    count,
                    avg_ms,
                    p50_ms,
                    p95_ms,
                    last_used_at,
                    fail_count,
                    errors,
                }
            })
            .collect();
        // show to the UI by count descending (hot paths first)
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.capability.cmp(&b.capability)));
        out
    }

    // ─── Phase 54: alerting_aggregations frequency-threshold rules ─────────────────────────

    fn aggregation_row_from_stmt(r: &rusqlite::Row) -> rusqlite::Result<AggregationRuleRow> {
        Ok(AggregationRuleRow {
            id: r.get(0)?,
            name: r.get(1)?,
            kind_pattern: r.get(2)?,
            window_secs: r.get::<_, i64>(3)? as u64,
            threshold_count: r.get::<_, i64>(4)? as u32,
            action: r.get(5)?,
            target_severity: r.get(6)?,
            enabled: r.get::<_, i64>(7)? != 0,
            created_at: r.get::<_, i64>(8)? as u64,
        })
    }

    /// All frequency-threshold rules (added in Phase 54). Phase 51 routes sort by priority similarly; here it is by created_at ASC.
    pub fn list_alerting_aggregations(&self) -> Vec<AggregationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern, window_secs, threshold_count, action,
                    target_severity, enabled, created_at
             FROM alerting_aggregations ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::aggregation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Enabled rules only (added in Phase 54).
    pub fn list_enabled_alerting_aggregations(&self) -> Vec<AggregationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern, window_secs, threshold_count, action,
                    target_severity, enabled, created_at
             FROM alerting_aggregations WHERE enabled = 1 ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::aggregation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Upsert an aggregation rule (added in Phase 54). id overwrites; created_at keeps the old value (to avoid UI time jumps).
    pub fn upsert_alerting_aggregation(&mut self, row: &AggregationRuleRow) {
        let _ = self.conn.execute(
            "INSERT INTO alerting_aggregations
             (id, name, kind_pattern, window_secs, threshold_count, action,
              target_severity, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET name=?2, kind_pattern=?3, window_secs=?4,
                threshold_count=?5, action=?6, target_severity=?7, enabled=?8",
            rusqlite::params![
                row.id,
                row.name,
                row.kind_pattern,
                row.window_secs as i64,
                row.threshold_count as i64,
                row.action,
                row.target_severity,
                row.enabled as i64,
                row.created_at as i64,
            ],
        );
    }

    pub fn delete_alerting_aggregation(&mut self, id: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_aggregations WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0)
            > 0
    }

    pub fn clear_alerting_aggregations(&mut self) -> usize {
        self.conn
            .execute("DELETE FROM alerting_aggregations", [])
            .unwrap_or(0)
    }

    // ─── Phase 55: alerting_correlations correlation suppression rules ─────────────────────────

    fn correlation_row_from_stmt(r: &rusqlite::Row) -> rusqlite::Result<CorrelationRuleRow> {
        Ok(CorrelationRuleRow {
            id: r.get(0)?,
            name: r.get(1)?,
            kind_pattern_a: r.get(2)?,
            kind_pattern_b: r.get(3)?,
            window_secs: r.get::<_, i64>(4)? as u64,
            enabled: r.get::<_, i64>(5)? != 0,
            created_at: r.get::<_, i64>(6)? as u64,
        })
    }

    pub fn list_alerting_correlations(&self) -> Vec<CorrelationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern_a, kind_pattern_b, window_secs,
                    enabled, created_at
             FROM alerting_correlations ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::correlation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn list_enabled_alerting_correlations(&self) -> Vec<CorrelationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern_a, kind_pattern_b, window_secs,
                    enabled, created_at
             FROM alerting_correlations WHERE enabled = 1 ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::correlation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn upsert_alerting_correlation(&mut self, row: &CorrelationRuleRow) {
        let _ = self.conn.execute(
            "INSERT INTO alerting_correlations
             (id, name, kind_pattern_a, kind_pattern_b, window_secs, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET name=?2, kind_pattern_a=?3, kind_pattern_b=?4,
                window_secs=?5, enabled=?6",
            rusqlite::params![
                row.id,
                row.name,
                row.kind_pattern_a,
                row.kind_pattern_b,
                row.window_secs as i64,
                row.enabled as i64,
                row.created_at as i64,
            ],
        );
    }

    pub fn delete_alerting_correlation(&mut self, id: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_correlations WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0)
            > 0
    }

    pub fn clear_alerting_correlations(&mut self) -> usize {
        self.conn
            .execute("DELETE FROM alerting_correlations", [])
            .unwrap_or(0)
    }

    // ─── Phase 56: alerting_escalations escalation rules ────────────────────────────────

    fn escalation_row_from_stmt(r: &rusqlite::Row) -> rusqlite::Result<EscalationRuleRow> {
        let ids_json: String = r.get(5)?;
        let target_endpoint_ids: Option<Vec<String>> = if ids_json.is_empty() || ids_json == "[]" {
            None
        } else {
            serde_json::from_str(&ids_json).ok()
        };
        Ok(EscalationRuleRow {
            id: r.get(0)?,
            name: r.get(1)?,
            kind_pattern: r.get(2)?,
            escalate_after_secs: r.get::<_, i64>(3)? as u64,
            target_severity: r.get(4)?,
            target_endpoint_ids,
            enabled: r.get::<_, i64>(6)? != 0,
            created_at: r.get::<_, i64>(7)? as u64,
        })
    }

    pub fn list_alerting_escalations(&self) -> Vec<EscalationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern, escalate_after_secs, target_severity,
                    target_endpoint_ids, enabled, created_at
             FROM alerting_escalations ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::escalation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn list_enabled_alerting_escalations(&self) -> Vec<EscalationRuleRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, kind_pattern, escalate_after_secs, target_severity,
                    target_endpoint_ids, enabled, created_at
             FROM alerting_escalations WHERE enabled = 1 ORDER BY created_at ASC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], Self::escalation_row_from_stmt)
            .ok()
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn upsert_alerting_escalation(&mut self, row: &EscalationRuleRow) {
        let ids_json = serde_json::to_string(row.target_endpoint_ids.as_ref().unwrap_or(&Vec::new()))
            .unwrap_or_else(|_| "[]".into());
        let _ = self.conn.execute(
            "INSERT INTO alerting_escalations
             (id, name, kind_pattern, escalate_after_secs, target_severity,
              target_endpoint_ids, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET name=?2, kind_pattern=?3, escalate_after_secs=?4,
                target_severity=?5, target_endpoint_ids=?6, enabled=?7",
            rusqlite::params![
                row.id,
                row.name,
                row.kind_pattern,
                row.escalate_after_secs as i64,
                row.target_severity,
                ids_json,
                row.enabled as i64,
                row.created_at as i64,
            ],
        );
    }

    pub fn delete_alerting_escalation(&mut self, id: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM alerting_escalations WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap_or(0)
            > 0
    }

    pub fn clear_alerting_escalations(&mut self) -> usize {
        self.conn
            .execute("DELETE FROM alerting_escalations", [])
            .unwrap_or(0)
    }
}

impl StoreEnum {
    /// Add a rating (the Mem variant returns success directly but does not count as persisted).
    pub fn add_rating(
        &mut self,
        plugin_id: &str,
        score: i64,
        comment: Option<&str>,
        ts: u64,
    ) -> Result<PluginRating, String> {
        match self {
            StoreEnum::Db(x) => x.add_rating(plugin_id, score, comment, ts),
            StoreEnum::Mem(_) => Ok(PluginRating {
                id: format!("rat-{}-{}", ts, plugin_id),
                plugin_id: plugin_id.to_string(),
                score,
                comment: comment.map(|s| s.to_string()),
                ts,
            }),
        }
    }

    pub fn list_ratings(&self, plugin_id: &str, limit: usize) -> Vec<PluginRating> {
        match self {
            StoreEnum::Db(x) => x.list_ratings(plugin_id, limit),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn rating_summary(&self, plugin_id: &str) -> PluginRatingSummary {
        match self {
            StoreEnum::Db(x) => x.rating_summary(plugin_id),
            StoreEnum::Mem(_) => PluginRatingSummary {
                plugin_id: plugin_id.to_string(),
                count: 0,
                avg: 0.0,
            },
        }
    }

    /// Phase 30 Dashboard: aggregate call stats per (capability, plugin_id) pair.
    pub fn capability_stats_summary(&self, samples_per_pair: usize) -> Vec<CapabilityStat> {
        match self {
            StoreEnum::Db(x) => x.capability_stats_summary(samples_per_pair),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Record a call (the Mem variant swallows it; only Db persists).
    /// Phase 31: result + error_kind distinguish success/failure classification.
    pub fn record_capability_call(
        &mut self,
        cap: &str,
        plugin: &str,
        elapsed_ms: i64,
        ts: u64,
        result: &str,
        error_kind: Option<&str>,
    ) {
        if let StoreEnum::Db(x) = self {
            x.record_capability_call(cap, plugin, elapsed_ms, ts, result, error_kind);
        }
    }

    /// Phase 37 — Probe report written to disk. The Mem variant is a silent no-op (tests do not need persistence either).
    pub fn set_probe_status(
        &mut self,
        plugin_id: &str,
        status: &str,
        report: Option<&str>,
        at: u64,
    ) {
        if let StoreEnum::Db(x) = self {
            x.set_probe_status(plugin_id, status, report, at);
        }
    }

    /// Phase 37 — read the probe report. The Mem variant returns None.
    pub fn get_probe_report(&self, plugin_id: &str) -> Option<(String, u64, String)> {
        match self {
            StoreEnum::Db(x) => x.get_probe_report(plugin_id),
            StoreEnum::Mem(_) => None,
        }
    }

    /// Phase 38 — set the plugin channel. The Mem variant is a silent no-op.
    pub fn set_plugin_channel(&mut self, plugin_id: &str, channel: &str) {
        if let StoreEnum::Db(x) = self {
            x.set_plugin_channel(plugin_id, channel);
        }
    }

    /// Phase 38 — batch-read all (plugin_id, channel). The Mem variant returns empty.
    pub fn list_plugin_channels(&self) -> Vec<(String, String)> {
        match self {
            StoreEnum::Db(x) => x.list_plugin_channels(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Phase 38 — global key/value. The Mem variant returns None.
    pub fn get_setting(&self, key: &str) -> Option<String> {
        match self {
            StoreEnum::Db(x) => x.get_setting(key),
            StoreEnum::Mem(_) => None,
        }
    }

    /// Phase 38 — global key/value. The Mem variant is a silent no-op.
    pub fn set_setting(&mut self, key: &str, value: &str) {
        if let StoreEnum::Db(x) = self {
            x.set_setting(key, value);
        }
    }

    /// Phase 39 — full hotkey list (the Mem variant returns empty).
    pub fn list_hotkeys(&self) -> Vec<HotkeyRow> {
        match self {
            StoreEnum::Db(x) => x.list_hotkeys(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    /// Phase 40 — read health config (the Mem variant returns the default).
    pub fn get_health_config(&self, plugin_id: &str) -> super::health::PluginHealthConfig {
        match self {
            StoreEnum::Db(x) => x.get_health_config(plugin_id),
            StoreEnum::Mem(_) => super::health::PluginHealthConfig::default(),
        }
    }

    /// Phase 40 — upsert health config (the Mem variant is a no-op).
    pub fn upsert_health_config(
        &mut self,
        plugin_id: &str,
        cfg: &super::health::PluginHealthConfig,
    ) {
        if let StoreEnum::Db(x) = self {
            x.upsert_health_config(plugin_id, cfg);
        }
    }

    /// Phase 40 — all health configs (the Mem variant returns empty).
    pub fn list_health_configs(&self) -> Vec<(String, super::health::PluginHealthConfig)> {
        match self {
            StoreEnum::Db(x) => x.list_health_configs(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    // ─── Phase 53: alerting_severity_hints forwarding ────────────────────────────────

    pub fn list_alerting_severity_hints(&self) -> Vec<SeverityHintRow> {
        match self {
            StoreEnum::Db(x) => x.list_alerting_severity_hints(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn upsert_alerting_severity_hint(
        &mut self,
        source: &str,
        severity: &str,
        origin: &str,
        plugin_id: Option<&str>,
        updated_at: i64,
    ) -> Result<(), String> {
        match self {
            StoreEnum::Db(x) => x.upsert_alerting_severity_hint(
                source,
                severity,
                origin,
                plugin_id,
                updated_at,
            ),
            StoreEnum::Mem(_) => Ok(()),
        }
    }

    pub fn get_alerting_severity_hint(
        &self,
        source: &str,
        origin: &str,
    ) -> Option<SeverityHintRow> {
        match self {
            StoreEnum::Db(x) => x.get_alerting_severity_hint(source, origin),
            StoreEnum::Mem(_) => None,
        }
    }

    pub fn delete_alerting_severity_hint_by_source_and_origin(
        &mut self,
        source: &str,
        origin: &str,
    ) -> bool {
        match self {
            StoreEnum::Db(x) => x.delete_alerting_severity_hint_by_source_and_origin(source, origin),
            StoreEnum::Mem(_) => false,
        }
    }

    pub fn delete_alerting_severity_hints_by_origin_plugin(
        &mut self,
        origin: &str,
        plugin_id: &str,
    ) -> usize {
        match self {
            StoreEnum::Db(x) => x.delete_alerting_severity_hints_by_origin_plugin(origin, plugin_id),
            StoreEnum::Mem(_) => 0,
        }
    }

    pub fn clear_user_alerting_severity_hints(&mut self) -> usize {
        match self {
            StoreEnum::Db(x) => x.clear_user_alerting_severity_hints(),
            StoreEnum::Mem(_) => 0,
        }
    }

    // ─── Phase 54: alerting_aggregations forwarding ──────────────────────────────────

    pub fn list_alerting_aggregations(&self) -> Vec<AggregationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_alerting_aggregations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn list_enabled_alerting_aggregations(&self) -> Vec<AggregationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_enabled_alerting_aggregations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn upsert_alerting_aggregation(&mut self, row: &AggregationRuleRow) {
        match self {
            StoreEnum::Db(x) => x.upsert_alerting_aggregation(row),
            StoreEnum::Mem(_) => {}
        }
    }

    pub fn delete_alerting_aggregation(&mut self, id: &str) -> bool {
        match self {
            StoreEnum::Db(x) => x.delete_alerting_aggregation(id),
            StoreEnum::Mem(_) => false,
        }
    }

    pub fn clear_alerting_aggregations(&mut self) -> usize {
        match self {
            StoreEnum::Db(x) => x.clear_alerting_aggregations(),
            StoreEnum::Mem(_) => 0,
        }
    }

    // ─── Phase 55: alerting_correlations forwarding ──────────────────────────────────

    pub fn list_alerting_correlations(&self) -> Vec<CorrelationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_alerting_correlations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn list_enabled_alerting_correlations(&self) -> Vec<CorrelationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_enabled_alerting_correlations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn upsert_alerting_correlation(&mut self, row: &CorrelationRuleRow) {
        match self {
            StoreEnum::Db(x) => x.upsert_alerting_correlation(row),
            StoreEnum::Mem(_) => {}
        }
    }

    pub fn delete_alerting_correlation(&mut self, id: &str) -> bool {
        match self {
            StoreEnum::Db(x) => x.delete_alerting_correlation(id),
            StoreEnum::Mem(_) => false,
        }
    }

    pub fn clear_alerting_correlations(&mut self) -> usize {
        match self {
            StoreEnum::Db(x) => x.clear_alerting_correlations(),
            StoreEnum::Mem(_) => 0,
        }
    }

    // ─── Phase 56: alerting_escalations forwarding ───────────────────────────────────

    pub fn list_alerting_escalations(&self) -> Vec<EscalationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_alerting_escalations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn list_enabled_alerting_escalations(&self) -> Vec<EscalationRuleRow> {
        match self {
            StoreEnum::Db(x) => x.list_enabled_alerting_escalations(),
            StoreEnum::Mem(_) => Vec::new(),
        }
    }

    pub fn upsert_alerting_escalation(&mut self, row: &EscalationRuleRow) {
        match self {
            StoreEnum::Db(x) => x.upsert_alerting_escalation(row),
            StoreEnum::Mem(_) => {}
        }
    }

    pub fn delete_alerting_escalation(&mut self, id: &str) -> bool {
        match self {
            StoreEnum::Db(x) => x.delete_alerting_escalation(id),
            StoreEnum::Mem(_) => false,
        }
    }

    pub fn clear_alerting_escalations(&mut self) -> usize {
        match self {
            StoreEnum::Db(x) => x.clear_alerting_escalations(),
            StoreEnum::Mem(_) => 0,
        }
    }
}

pub type SharedStore = Arc<Mutex<StoreEnum>>;

/// O1 — shared entry point for event-table retention cleanup: besides the once-at-startup pass (see `Storage::open`), the retention worker
/// calls it every 24h, keeping events bounded even when the long-running desktop process is never restarted. Returns the deleted row count.
pub(crate) fn prune_events_shared(store: &SharedStore) -> usize {
    let cutoff = crate::core::agent::now_secs().saturating_sub(EVENT_RETENTION_DAYS * 86400);
    store
        .lock()
        .ok()
        .and_then(|mut s| {
            s.with_conn(|c| {
                c.execute("DELETE FROM events WHERE timestamp < ?1", [cutoff])
                    .unwrap_or(0)
            })
        })
        .unwrap_or(0)
}

/// ~/.opencapx/data/opencapx.db, falls back to memory if it cannot open.
pub fn open_default() -> StoreEnum {
    let path = data_dir().join("opencapx.db");
    match Storage::open(&path) {
        Ok(s) => StoreEnum::Db(s),
        Err(e) => {
            eprintln!("[storage] sqlite unavailable ({}), falling back to memory", e);
            StoreEnum::Mem(super::agent::SessionStore::new())
        }
    }
}

pub fn data_dir() -> std::path::PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(".opencapx").join("data");
    }
    std::env::temp_dir().join("opencapx-data")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::{now_secs, SessionStore};
    use crate::core::event::OpencapxEvent;

    fn tmpdb(tag: &str) -> Storage {
        let dir = std::env::temp_dir().join(format!("opencapx-db-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        Storage::open(&dir.join("t.db")).unwrap()
    }

    fn sess(id: &str, state: AgentState) -> Session {
        Session {
            id: id.into(),
            agent: "claude".into(),
            project: "p".into(),
            cwd: String::new(),
            message: "m".into(),
            state,
            started_at: 1000,
            updated_at: 1000,
            model: String::new(),
            speech: String::new(),
            choices: None,
            answered: None,
        }
    }

    #[test]
    fn session_model_roundtrips_and_migrates_old_db() {
        // an old DB has no model column → after open()'s migration it should read/write normally
        let dir = std::env::temp_dir().join(format!("opencapx-db-model-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, agent TEXT NOT NULL, project TEXT NOT NULL,
                    message TEXT NOT NULL, state TEXT NOT NULL, updated_at INTEGER NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions VALUES ('old','claude','p','m','working',1)",
                [],
            )
            .unwrap();
        }
        let mut db = Storage::open(&path).unwrap();
        // after migration old rows are readable, model defaults to an empty string
        let old = db.all();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].model, "");
        // write a session with a model and read it back
        let mut s = sess("m1", AgentState::Working);
        s.model = "claude-opus-4-1".into();
        db.upsert(s);
        assert_eq!(db.get("m1").unwrap().model, "claude-opus-4-1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_speech_roundtrips_and_migrates_old_db() {
        // an old DB has no speech column (it is a later-added field) → after open()'s migration it should read/write normally
        let dir = std::env::temp_dir().join(format!("opencapx-db-speech-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, agent TEXT NOT NULL, project TEXT NOT NULL,
                    message TEXT NOT NULL, state TEXT NOT NULL, updated_at INTEGER NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions VALUES ('old','claude','p','m','working',1)",
                [],
            )
            .unwrap();
        }
        let mut db = Storage::open(&path).unwrap();
        assert_eq!(db.all()[0].speech, "");
        // sticky fields are preserved by upsert: write the body, then write an event without a body, and the body must not be wiped
        let mut s = sess("sp1", AgentState::Working);
        s.speech = "Let me first look at the bubble rendering path.".into();
        db.upsert(s);
        assert_eq!(db.get("sp1").unwrap().speech, "Let me first look at the bubble rendering path.");
        let mut later = sess("sp1", AgentState::Working);
        later.model = "claude-sonnet-4-5".into();
        db.upsert(later);
        let back = db.get("sp1").unwrap();
        assert_eq!(back.speech, "", "storage writes faithfully (stickiness is event::ingest's job)");
        assert_eq!(back.model, "claude-sonnet-4-5");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugins_auto_reload_column_migrates_old_db() {
        // an old DB's plugins table has no auto_reload column (created before it) → open()'s migration should add it:
        // plugin::list()'s SELECT references it directly; a missing column makes the whole plugin list read empty.
        let dir = std::env::temp_dir().join(format!("opencapx-db-autoreload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE plugins (
                    id TEXT PRIMARY KEY, version TEXT NOT NULL, type TEXT NOT NULL,
                    status TEXT NOT NULL, path TEXT NOT NULL, manifest TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO plugins VALUES ('com.x','0.1.0','capability','running','/tmp/x','{}')",
                [],
            )
            .unwrap();
        }
        let db = Storage::open(&path).unwrap();
        // after migration the column exists (the SELECT would error if it were missing) + old rows are readable with a default of 0
        let auto: i64 = db
            .conn
            .query_row("SELECT auto_reload FROM plugins WHERE id = 'com.x'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(auto, 0, "old rows default auto_reload to 0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// O1 — shared prune: old events deleted, new events kept; repeated runs are idempotent.
    #[test]
    fn prune_events_shared_bounds_old_rows() {
        let dir = std::env::temp_dir().join(format!("opencapx-db-evprune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let now = crate::core::agent::now_secs();
        let store: SharedStore =
            std::sync::Arc::new(std::sync::Mutex::new(StoreEnum::Db(Storage::open(&path).unwrap())));
        {
            // insert after open: the startup path prunes, so inserting first would get cleared by it.
            let mut g = store.lock().unwrap();
            let inserted = g.with_conn(|c| {
                c.execute(
                    "INSERT INTO events (id, type, source, timestamp, payload) VALUES ('old','t','s',?1,'{}')",
                    [now.saturating_sub((EVENT_RETENTION_DAYS + 1) * 86400)],
                )
                .unwrap_or(0)
                    + c.execute(
                        "INSERT INTO events (id, type, source, timestamp, payload) VALUES ('new','t','s',?1,'{}')",
                        [now],
                    )
                    .unwrap_or(0)
            });
            assert_eq!(inserted, Some(2));
        }
        assert_eq!(prune_events_shared(&store), 1, "only old rows cleared");
        assert_eq!(prune_events_shared(&store), 0, "repeated runs are idempotent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// O3 — WAL is in effect (standard for a local desktop DB).
    #[test]
    fn sqlite_pragmas_are_applied() {
        let dir = std::env::temp_dir().join(format!("opencapx-db-pragma-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Storage::open(&dir.join("t.db")).unwrap();
        let mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugins_last_version_column_migrates_old_db() {
        // M8/F12 across versions — an old DB's plugins table has no last_version column (before M7) → open()
        // migration adds the column; plugin::last_version_of SELECTs it directly, and a missing column would fail the read.
        let dir = std::env::temp_dir().join(format!("opencapx-db-lastver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE plugins (
                    id TEXT PRIMARY KEY, version TEXT NOT NULL, type TEXT NOT NULL,
                    status TEXT NOT NULL, path TEXT NOT NULL, manifest TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO plugins VALUES ('com.x','0.2.0','capability','stopped','/tmp/x','{}')",
                [],
            )
            .unwrap();
        }
        let db = Storage::open(&path).unwrap();
        // after migration the column exists + old rows are NULL (no successful handshake yet)
        let v: Option<String> = db
            .conn
            .query_row("SELECT last_version FROM plugins WHERE id = 'com.x'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(v, None, "old rows have an empty last_version");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_cwd_roundtrips_and_migrates_old_db() {
        // an old DB has no cwd column → after open()'s migration it should read/write normally
        let dir = std::env::temp_dir().join(format!("opencapx-db-cwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, agent TEXT NOT NULL, project TEXT NOT NULL,
                    message TEXT NOT NULL, state TEXT NOT NULL, updated_at INTEGER NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions VALUES ('old','claude','p','m','working',1)",
                [],
            )
            .unwrap();
        }
        let mut db = Storage::open(&path).unwrap();
        assert_eq!(db.all()[0].cwd, "");
        let mut s = sess("cw1", AgentState::Working);
        s.cwd = "/Users/me/work/OpenCapX".into();
        db.upsert(s);
        assert_eq!(db.get("cw1").unwrap().cwd, "/Users/me/work/OpenCapX");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sqlite_session_roundtrip_and_expiry() {
        let mut db = tmpdb("roundtrip");
        db.upsert(sess("a", AgentState::Done));
        db.upsert(sess("w", AgentState::Working));
        assert!(db.get("a").is_some());
        assert!(db.active(1031).iter().all(|s| s.id != "a"));
        // working with no heartbeat for over STALE_ACTIVE_TTL also counts as expired (zombie protection)
        assert!(db.active(1900).iter().any(|s| s.id == "w"));
        assert!(db.active(1901).iter().all(|s| s.id != "w"));
        db.dismiss("w");
        assert!(db.get("w").is_none());
        db.upsert(sess("b", AgentState::Idle));
        db.clear();
        assert!(db.all().is_empty());
    }

    #[test]
    fn sweep_archives_then_deletes_and_lists_history() {
        let mut db = tmpdb("archive");
        db.upsert(sess("old", AgentState::Working)); // updated_at = 1000
        let mut live = sess("live", AgentState::Working);
        live.updated_at = 5000;
        db.upsert(live);

        assert_eq!(db.sweep(5300), 1);
        assert!(db.get("old").is_none(), "expired session left the active table");
        assert!(db.get("live").is_some());

        let hist = db.list_session_archive(10);
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].id, "old");
        assert_eq!(hist[0].state, "working");
        assert_eq!(hist[0].ended_at, 1000);
        assert_eq!(hist[0].duration, 0);

        // a repeated sweep does not produce duplicate archives
        assert_eq!(db.sweep(5300), 0);
        assert_eq!(db.list_session_archive(10).len(), 1);
    }

    #[test]
    fn prune_session_archive_drops_only_old_records() {
        let mut db = tmpdb("archiveprune");
        db.upsert(sess("old", AgentState::Working));
        db.sweep(9999);
        assert_eq!(db.list_session_archive(10).len(), 1);
        // now is far from exceeding retention → do not delete
        assert_eq!(db.prune_session_archive(1000), 0);
        assert_eq!(db.list_session_archive(10).len(), 1);
        // now exceeds 90 days → delete (record ended_at = 1000)
        let later = 1000 + (ARCHIVE_KEEP_DAYS + 1) * 86_400;
        assert_eq!(db.prune_session_archive(later), 1);
        assert!(db.list_session_archive(10).is_empty());
    }

    #[test]
    fn sqlite_logs_and_prunes_events() {
        let dir = std::env::temp_dir().join(format!("opencapx-db-{}-prune", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let mut db = Storage::open(&dir.join("t.db")).unwrap();
            let old = OpencapxEvent {
                id: "e1".into(),
                kind: "agent.completed".into(),
                source: "hooks:claude".into(),
                timestamp: 1000,
                payload: serde_json::json!({"x": 1}),
            };
            db.insert_event(&old);
            let fresh = OpencapxEvent {
                id: "e2".into(),
                kind: "agent.started".into(),
                source: "mcp".into(),
                timestamp: now_secs(),
                payload: serde_json::json!({}),
            };
            db.insert_event(&fresh);
        }
        let db = Storage::open(&dir.join("t.db")).unwrap();
        let n: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "old event should be pruned on reopen");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_enum_delegates() {
        let mut s = StoreEnum::Mem(SessionStore::new());
        s.upsert(sess("a", AgentState::Working));
        assert_eq!(s.active(1000).len(), 1);
        assert!(!s.is_db());
        let e = OpencapxEvent {
            id: "e".into(),
            kind: "k".into(),
            source: "s".into(),
            timestamp: 1,
            payload: serde_json::json!(null),
        };
        s.log_event(&e); // mem variant: no-op, must not panic
    }

    #[test]
    fn list_events_filters_by_prefix_and_orders_desc() {
        let mut db = tmpdb("audit");
        db.insert_event(&OpencapxEvent {
            id: "p1".into(),
            kind: "permission.ask".into(),
            source: "core".into(),
            timestamp: 100,
            payload: serde_json::json!({"pluginId":"echo-vision","permission":"image.read","decision":"granted"}),
        });
        db.insert_event(&OpencapxEvent {
            id: "p2".into(),
            kind: "permission.deny".into(),
            source: "core".into(),
            timestamp: 200,
            payload: serde_json::json!({"pluginId":"x","permission":"shell.exec","decision":"denied","reason":"high-risk"}),
        });
        db.insert_event(&OpencapxEvent {
            id: "x1".into(),
            kind: "agent.started".into(),
            source: "mcp".into(),
            timestamp: 300,
            payload: serde_json::json!({}),
        });
        let only_perm = db.list_events("permission.", 50);
        assert_eq!(only_perm.len(), 2);
        assert_eq!(only_perm[0].id, "p2", "newest first");
        assert_eq!(only_perm[1].id, "p1");
        assert_eq!(only_perm[0].payload["decision"], "denied");
        let capped = db.list_events("permission.", 1);
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].id, "p2");
    }

    /// Phase 35 — Audit search/filter: kind prefix + full text + time range + special-character escaping.
    #[test]
    fn list_events_filtered_applies_all_clauses() {
        let mut db = tmpdb("filter");
        let cases = [
            ("p1", "permission.granted", 100, "image.read", "granted", ""),
            ("p2", "permission.denied", 200, "shell.exec", "denied", "high-risk"),
            ("p3", "permission.ask", 300, "camera", "ask", "user-decision"),
            ("l1", "plugin.lifecycle.starting", 150, "echo-vision", "", ""),
            ("c1", "capability.completed", 250, "image.analyze", "ok", ""),
            ("p4", "permission.granted", 400, "image.read", "granted", "rematch"),
        ];
        for (id, kind, ts, plugin, decision, reason) in cases {
            db.insert_event(&OpencapxEvent {
                id: id.into(),
                kind: kind.into(),
                source: "core".into(),
                timestamp: ts,
                payload: serde_json::json!({
                    "pluginId": plugin,
                    "permission": kind.split('.').nth(1).unwrap_or(""),
                    "decision": decision,
                    "reason": reason,
                }),
            });
        }

        // 1) kind_prefix only
        let only_perm = db.list_events_filtered(Some("permission."), None, None, None, 50);
        assert_eq!(only_perm.len(), 4, "4 permission.* rows");
        assert!(only_perm.iter().all(|e| e.kind.starts_with("permission.")));

        // 2) kind_prefix + query (full-text match "den") — only p2's kind/payload contains "den" (the other
//   permission.* events have reasons "high-risk"/"user-decision"/"rematch", with no den substring)
        let denied = db.list_events_filtered(Some("permission."), Some("den"), None, None, 50);
        assert_eq!(denied.len(), 1, "permission.* + 'den' matches only p2 (permission.denied)");
        assert_eq!(denied[0].id, "p2");

        // 3) time range since..until
        let ranged = db.list_events_filtered(None, None, Some(180), Some(350), 50);
        assert_eq!(ranged.len(), 3, "ts ∈ [180, 350]");
        let ids: Vec<&str> = ranged.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"p2"));
        assert!(ids.contains(&"p3"));
        assert!(ids.contains(&"c1"));

        // 4) combination: permission.* + full text + time
        let combo = db.list_events_filtered(
            Some("permission."),
            Some("image"),
            Some(50),
            Some(500),
            50,
        );
        assert_eq!(combo.len(), 2, "permission.* and payload contains image and ts ∈ [50,500]");
        let combo_ids: Vec<&str> = combo.iter().map(|e| e.id.as_str()).collect();
        assert!(combo_ids.contains(&"p1"));
        assert!(combo_ids.contains(&"p4"));

        // 5) limit truncation
        let capped = db.list_events_filtered(Some("permission."), None, None, None, 2);
        assert_eq!(capped.len(), 2);

        // 6) special-character escaping: a user searching "_" must not be treated as a wildcard
        // if "_" were a wildcard it would match payloads like "pluginId":"echo-vision", image.read (no _), shell.exec (no _)…
        // so the fuzzy search "_" below should match only payloads actually containing _ (0 here → verifies no over-matching)
        let underscore = db.list_events_filtered(None, Some("_"), None, None, 50);
        assert_eq!(underscore.len(), 0, "an unescaped _ would match every event; escaped, it matches only a literal underscore");

        // 7) empty query / None query behave identically
        let none_q = db.list_events_filtered(Some("permission."), None, None, None, 50);
        let empty_q = db.list_events_filtered(Some("permission."), Some(""), None, None, 50);
        assert_eq!(none_q.len(), empty_q.len());
    }

    /// Phase 28 — ratings: add writes to the DB + list_ratings descending + rating_summary computes the average.
    /// Cross-plugin isolation, consistent add/list behavior across StoreEnum, out-of-range scores rejected.
    #[test]
    fn rating_add_list_summary_round_trip() {
        let dir = std::env::temp_dir().join(format!("opencapx-rating-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut db = Storage::open(&dir.join("t.db")).unwrap();
        // out-of-range scores: both 0 and 6 are rejected.
        assert!(db.add_rating("com.x.demo-7", 0, None, 100).is_err());
        assert!(db.add_rating("com.x.demo-7", 6, None, 100).is_err());

        db.add_rating("com.x.demo-7", 5, Some("love it"), 100).unwrap();
        db.add_rating("com.x.demo-7", 3, None, 200).unwrap();
        db.add_rating("com.x.demo-7", 4, Some("good"), 300).unwrap();
        // noise: another plugin id that must not be included.
        db.add_rating("com.x.other-7", 1, None, 400).unwrap();

        let rows = db.list_ratings("com.x.demo-7", 50);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].score, 4, "newest first (ts=300)");
        assert_eq!(rows[0].comment.as_deref(), Some("good"));
        assert_eq!(rows[1].score, 3);
        assert_eq!(rows[2].score, 5);

        let summary = db.rating_summary("com.x.demo-7");
        assert_eq!(summary.count, 3);
        // (5+3+4)/3 = 4.0 → rounded 4.00
        assert!((summary.avg - 4.0).abs() < 0.01, "avg={}", summary.avg);

        // a plugin with no ratings → count=0, avg=0
        let empty = db.rating_summary("com.x.nothing-7");
        assert_eq!(empty.count, 0);
        assert_eq!(empty.avg, 0.0);

        // limit=2 takes only the first two
        assert_eq!(db.list_ratings("com.x.demo-7", 2).len(), 2);

        // StoreEnum passthrough
        let mut store: StoreEnum = StoreEnum::Db(db);
        store
            .add_rating("com.x.demo-7", 2, None, 500)
            .expect("enum add_rating");
        assert_eq!(store.rating_summary("com.x.demo-7").count, 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase 30 — capability_stats: record + summary aggregate count + avg + p50 + p95,
    /// taking only the latest N per pair (via the ROW_NUMBER window function).
    #[test]
    fn capability_stats_summary_aggregates_percentiles() {
        let dir = std::env::temp_dir().join(format!("opencapx-capstats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut db = Storage::open(&dir.join("t.db")).unwrap();

        // 100 calls for (image.analyze, plug-a), latency 10ms..109ms increasing
        for i in 0..100i64 {
            db.record_capability_call(
                "image.analyze",
                "plug-a",
                10 + i,
                (1_000 + i) as u64,
                "ok",
                None,
            );
        }
        // noise: another plugin, only 5 calls, all with high latency
        for i in 0..5i64 {
            db.record_capability_call(
                "image.analyze",
                "plug-b",
                500 + i,
                (2_000 + i) as u64,
                "ok",
                None,
            );
        }

        let rows = db.capability_stats_summary(50);
        // two pairs
        assert_eq!(rows.len(), 2);

        // by count descending: plug-a (100) should come first
        assert_eq!(rows[0].plugin_id, "plug-a");
        assert_eq!(rows[0].count, 50, "limit=50 takes the latest 50");
        // the latest 50 latencies = 60..109, average = (60+109)*50/2/50 = 84.5
        let avg_a = rows[0].avg_ms;
        assert!((avg_a - 84.5).abs() < 0.5, "avg_a={}", avg_a);
        // p50: samples[len/2] = samples[25], after sorting = 60+25 = 85
        assert_eq!(rows[0].p50_ms, 85);
        // p95: ceil(50*0.95)=48, idx=47 (0-based), after sorting = 60+47 = 107
        assert_eq!(rows[0].p95_ms, 107);

        // plug-b: all 5 collected, count=5
        let b = rows.iter().find(|r| r.plugin_id == "plug-b").unwrap();
        assert_eq!(b.count, 5);
        assert!((b.avg_ms - 502.0).abs() < 0.5);
        assert_eq!(b.p50_ms, 502); // sorted [500,501,502,503,504], len/2=2 → 502
        assert_eq!(b.p95_ms, 504); // ceil(5*0.95)=5,idx=4 → 504

        // last_used_at takes the maximum
        assert!(rows.iter().all(|r| r.last_used_at > 0));

        // returns [] when empty
        assert!(Storage::open(&dir.join("t.db")).unwrap().capability_stats_summary(50).is_empty()
            == false); // the same file has data; just verify the call does not crash

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase 31 — capability_stats failure classification: result + error_kind + fail_count + errors bucketing.
    /// Verifies: ok paths are computed into p50/p95, failures are not; fail_count and errors aggregate by error_kind.
    #[test]
    fn capability_stats_summary_tracks_failures_and_errors() {
        let dir = std::env::temp_dir().join(format!("opencapx-capstats-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut db = Storage::open(&dir.join("t.db")).unwrap();

        // 7 ok calls (latency 50..56) + 2 timeout + 1 denied
        for i in 0..7i64 {
            db.record_capability_call(
                "image.analyze",
                "plug-a",
                50 + i,
                (1_000 + i) as u64,
                "ok",
                None,
            );
        }
        db.record_capability_call(
            "image.analyze",
            "plug-a",
            60_000,
            2_000,
            "timeout",
            Some("timeout"),
        );
        db.record_capability_call(
            "image.analyze",
            "plug-a",
            61_000,
            2_001,
            "timeout",
            Some("timeout"),
        );
        db.record_capability_call(
            "image.analyze",
            "plug-a",
            0,
            2_002,
            "denied",
            Some("permission_denied"),
        );

        let rows = db.capability_stats_summary(50);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];

        // total samples = 7 ok + 2 timeout + 1 denied = 10
        assert_eq!(r.count, 10);
        assert_eq!(r.fail_count, 3, "3 failures: 2 timeout + 1 denied");

        // errors bucketing
        assert_eq!(r.errors.get("timeout").copied(), Some(2));
        assert_eq!(r.errors.get("permission_denied").copied(), Some(1));
        assert_eq!(r.errors.len(), 2);

        // p50/p95/avg use only the 7 ok samples (50..56)
        // sorted [50,51,52,53,54,55,56], len/2=3 → 53
        assert_eq!(r.p50_ms, 53);
        // ceil(7*0.95)=7,idx=6 → 56
        assert_eq!(r.p95_ms, 56);
        // avg = (50+51+52+53+54+55+56)/7 = 53.0
        assert!((r.avg_ms - 53.0).abs() < 0.01);

        // second plugin: 3 all err + 1 ok, verifying fail_count and ok are both correct
        for i in 0..3i64 {
            db.record_capability_call(
                "image.analyze",
                "plug-b",
                100,
                (3_000 + i) as u64,
                "err",
                Some("rpc_error"),
            );
        }
        db.record_capability_call(
            "image.analyze",
            "plug-b",
            200,
            3_010,
            "ok",
            None,
        );

        let rows2 = db.capability_stats_summary(50);
        let b = rows2.iter().find(|r| r.plugin_id == "plug-b").unwrap();
        assert_eq!(b.count, 4);
        assert_eq!(b.fail_count, 3);
        assert_eq!(b.errors.get("rpc_error").copied(), Some(3));
        // p50/p95/avg use only 1 ok sample (200)
        assert_eq!(b.p50_ms, 200);
        assert_eq!(b.p95_ms, 200);
        assert!((b.avg_ms - 200.0).abs() < 0.01);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase 40 — health config upsert/list roundtrip; get_health_config falls back to the default when there is no row.
    #[test]
    fn health_config_round_trips_and_falls_back_to_default() {
        let dir = std::env::temp_dir().join(format!("opencapx-db-{}-health", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut db = Storage::open(&dir.join("t.db")).unwrap();

        // not yet upserted → get the default
        let cfg = db.get_health_config("plug-x");
        assert_eq!(cfg, crate::core::health::PluginHealthConfig::default());
        assert_eq!(cfg.heartbeat_sec, 0);
        assert_eq!(cfg.max_retries, 3);
        assert!(cfg.enabled);

        // after upsert it roundtrips
        let custom = crate::core::health::PluginHealthConfig {
            heartbeat_sec: 30,
            ping_timeout_ms: 1500,
            max_retries: 5,
            backoff_initial_ms: 2000,
            enabled: false,
        };
        db.upsert_health_config("plug-x", &custom);
        let back = db.get_health_config("plug-x");
        assert_eq!(back, custom);

        // upserting the same plugin again = overwrite
        let custom2 = crate::core::health::PluginHealthConfig {
            heartbeat_sec: 60,
            ..crate::core::health::PluginHealthConfig::default()
        };
        db.upsert_health_config("plug-x", &custom2);
        let back2 = db.get_health_config("plug-x");
        assert_eq!(back2.heartbeat_sec, 60);
        assert_eq!(back2.max_retries, 3); // other fields keep the default

        // list fetches both plugins at once
        db.upsert_health_config("plug-y", &crate::core::health::PluginHealthConfig::default());
        let all = db.list_health_configs();
        let ids: Vec<&str> = all.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"plug-x"));
        assert!(ids.contains(&"plug-y"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
