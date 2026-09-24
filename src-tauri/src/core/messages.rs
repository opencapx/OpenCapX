//! v1.4 messages.recent builtin provider (docs/capability.md "builtin providers").
//!
//! Permission `messages.read` (denied + high-risk): iMessage history is the most private
//! personal-data surface, and the read channel = "Full Disk Access" (Full Disk Access, a machine-wide
//! TCC); denied by default, only allow-once at runtime, per call.
//!
//! The implementation opens `~/Library/Messages/chat.db` (SQLite) read-only:
//! - Without FDA the open fails immediately, with authorization guidance in the error (report honestly, do not pretend no permission)
//! - Only normal messages with text (tapbacks/stickers etc. with associated_message_type ≠ 0
//!   are skipped); attachment content is not read in v1
//! - The `date` column is Apple epoch (2001-01-01) nanoseconds, converted to Unix seconds when returned
//! macOS-only; non-macOS reports an honest error, plugins can override.

use serde_json::{json, Value};

/// Cap on messages returned in one call.
const MSG_LIMIT_MAX: i64 = 100;
/// The second offset between the Apple epoch (2001-01-01T00:00:00Z) and the Unix epoch.
const MAC_EPOCH_DELTA: i64 = 978_307_200;

/// message.date → Unix seconds (pure function). Modern DBs use nanoseconds; old DBs (migrated from
/// before Yosemite) still use seconds — distinguish by magnitude: above 1e12 is treated as nanoseconds.
fn mac_to_unix(d: i64) -> i64 {
    let secs = if d.abs() > 1_000_000_000_000 {
        d / 1_000_000_000
    } else {
        d
    };
    secs + MAC_EPOCH_DELTA
}

/// messages.recent builtin implementation.
pub fn recent(input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        return Err("messages.recent builtin is macOS-only (chat.db)".into());
    }
    #[cfg(target_os = "macos")]
    {
        let limit = input.get("limit").and_then(|l| l.as_i64()).unwrap_or(20);
        if !(1..=MSG_LIMIT_MAX).contains(&limit) {
            return Err(format!(
                "invalid input: limit must be 1..={} (default 20)",
                MSG_LIMIT_MAX
            ));
        }
        let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
        let db = format!("{}/Library/Messages/chat.db", home);
        if !std::path::Path::new(&db).exists() {
            return Err(format!("chat db not found: {} (iMessage not set up?)", db));
        }
        let conn = rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| {
            format!(
                "open chat.db failed: {} (grant Full Disk Access for OpenCapX: System Settings → Privacy & Security → Full Disk Access)",
                e
            )
        })?;
        let mut stmt = conn
            .prepare(
                "SELECT m.ROWID, m.text, m.is_from_me, m.date, h.id \
                 FROM message m LEFT JOIN handle h ON m.handle_id = h.ROWID \
                 WHERE m.text IS NOT NULL AND m.associated_message_type = 0 \
                 ORDER BY m.date DESC LIMIT ?1",
            )
            .map_err(|e| format!("query failed: {}", e))?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|e| format!("query failed: {}", e))?;
        let mut msgs = Vec::new();
        for row in rows {
            let (id, text, from_me, date, handle) =
                row.map_err(|e| format!("read row failed: {}", e))?;
            msgs.push(json!({
                "id": id,
                "text": text,
                "from_me": from_me != 0,
                "ts": mac_to_unix(date),
                "handle": handle,
            }));
        }
        // The query is newest-first; reverse to chronological order (so the agent can read in order)
        msgs.reverse();
        Ok(json!({ "count": msgs.len(), "messages": msgs }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_to_unix_handles_both_epochs() {
        // Nanoseconds (modern): an Apple nanosecond timestamp around 2024-06-01
        assert_eq!(
            mac_to_unix(738_300_000_000_000_000),
            738_300_000 + MAC_EPOCH_DELTA
        );
        // Seconds (old DB): the old format for the same instant
        assert_eq!(mac_to_unix(738_300_000), 738_300_000 + MAC_EPOCH_DELTA);
        // Negative values (before 2001) do not panic
        assert_eq!(
            mac_to_unix(-1_000_000_000),
            -1_000_000_000 + MAC_EPOCH_DELTA
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn validates_limit_before_touching_db() {
        assert!(recent(&json!({ "limit": 0 }))
            .unwrap_err()
            .contains("limit"));
        assert!(recent(&json!({ "limit": 101 }))
            .unwrap_err()
            .contains("limit"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn messages_reports_platform_limit() {
        assert!(recent(&json!({})).unwrap_err().contains("macOS-only"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "needs Full Disk Access TCC; run with --ignored manually"]
    fn recent_real_manual() {
        let out = recent(&json!({ "limit": 5 })).unwrap();
        assert!(out["count"].as_i64().unwrap() >= 0);
        let ms = out["messages"].as_array().unwrap();
        for m in ms {
            assert!(m["ts"].as_i64().is_some(), "{}", m);
        }
    }
}
