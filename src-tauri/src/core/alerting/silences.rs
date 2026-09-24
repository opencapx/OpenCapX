//! silence and ack rules.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Validate silence arguments.
pub fn validate_silence(s: &SilenceRule) -> Result<(), String> {
    if s.name.trim().is_empty() {
        return Err("silence name is required".into());
    }
    if s.name.len() > 128 {
        return Err("silence name too long (max 128 chars)".into());
    }
    if s.ends_at <= s.starts_at {
        return Err("ends_at must be > starts_at".into());
    }
    if s.ends_at > s.starts_at.saturating_add(7 * 86400) {
        return Err("silence window too long (max 7 days)".into());
    }
    if s.weekdays == 0 {
        return Err("at least one weekday must be selected".into());
    }
    if s.start_hour > 24 || s.end_hour > 24 {
        return Err("hour must be 0..=24".into());
    }
    if s.start_hour >= s.end_hour {
        return Err("end_hour must be > start_hour".into());
    }
    if s.kind_pattern.is_empty() || s.kind_pattern == "*" {
        // OK — full match
    } else if s.kind_pattern.contains("..") || s.kind_pattern.contains("//") {
        return Err("invalid kind_pattern".into());
    } else if s.kind_pattern.len() > 256 {
        return Err("kind_pattern too long".into());
    }
    Ok(())
}

/// Generate a silence id.
fn gen_silence_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("sil-{}-{}", nanos, n)
}

/// List all silences.
pub fn list_silences() -> Vec<SilenceRuleRow> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let Ok(s) = store.lock() else {
        return Vec::new();
    };
    if let StoreEnum::Db(db) = &*s {
        db.list_alerting_silences()
    } else {
        Vec::new()
    }
}

/// Upsert a silence (empty id = create). `created_at` keeps the old value (avoids a time jump in the UI).
pub fn save_silence(s: SilenceRule) -> Result<SilenceRuleRow, String> {
    validate_silence(&s)?;
    let Some(store) = crate::core::shared_store() else {
        return Err("store not initialized".into());
    };
    let Ok(mut st) = store.lock() else {
        return Err("store poisoned".into());
    };
    let StoreEnum::Db(db) = &mut *st else {
        return Err("sqlite store required".into());
    };
    let id = if s.id.is_empty() {
        gen_silence_id()
    } else {
        s.id.clone()
    };
    let now_secs = crate::core::agent::now_secs();
    let created_at = db
        .list_alerting_silences()
        .into_iter()
        .find(|r| r.id == id)
        .map(|r| r.created_at)
        .unwrap_or(now_secs);
    let row = SilenceRuleRow {
        id,
        name: s.name,
        kind_pattern: s.kind_pattern,
        starts_at: s.starts_at,
        ends_at: s.ends_at,
        weekdays: s.weekdays,
        start_hour: s.start_hour,
        end_hour: s.end_hour,
        created_at,
    };
    db.upsert_alerting_silence(&row);
    Ok(row)
}

pub fn delete_silence(id: &str) -> bool {
    let Some(store) = crate::core::shared_store() else {
        return false;
    };
    let Ok(mut s) = store.lock() else {
        return false;
    };
    if let StoreEnum::Db(db) = &mut *s {
        db.delete_alerting_silence(id)
    } else {
        false
    }
}

/// Determine whether `kind` hits any active silence at `now_ts`.
/// Hit condition: now ∈ [starts_at, ends_at) + weekday bitmask hit + hour in [start_hour, end_hour).
pub fn is_silenced(kind: &str, now_ts: u64) -> bool {
    let Some(store) = crate::core::shared_store() else {
        return false;
    };
    let Ok(s) = store.lock() else { return false };
    let StoreEnum::Db(db) = &*s else { return false };
    for r in db.list_active_silences(now_ts) {
        if now_ts < r.starts_at || now_ts >= r.ends_at {
            continue;
        }
        let wd = weekday_from_unix(now_ts);
        // wd: 0=Sun..6=Sat。bitmask: Mon=1<<0, ..., Sat=1<<5, Sun=1<<6
        let bit = ((wd as u16 + 6) % 7) as u8;
        if r.weekdays & (1u8 << bit) == 0 {
            continue;
        }
        let h = hour_from_unix(now_ts);
        if h < r.start_hour || h >= r.end_hour {
            continue;
        }
        if kind_matches(&r.kind_pattern, kind) {
            return true;
        }
    }
    false
}

/// List all acks (including expired — the UI filters them itself).
pub fn list_acks() -> Vec<AckRuleRow> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let Ok(s) = store.lock() else {
        return Vec::new();
    };
    if let StoreEnum::Db(db) = &*s {
        db.list_alerting_acks()
    } else {
        Vec::new()
    }
}

/// Add an ack for `kind_pattern`, expiring after window_secs.
pub fn ack_kind(kind_pattern: String, window_secs: u64) -> Result<AckRuleRow, String> {
    if kind_pattern.is_empty() {
        return Err("kind_pattern is required".into());
    }
    if window_secs == 0 || window_secs > 7 * 86400 {
        return Err("window_secs must be 1..=604800".into());
    }
    let Some(store) = crate::core::shared_store() else {
        return Err("store not initialized".into());
    };
    let Ok(mut s) = store.lock() else {
        return Err("store poisoned".into());
    };
    let StoreEnum::Db(db) = &mut *s else {
        return Err("sqlite store required".into());
    };
    let id = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("ack-{}-{}", nanos, n)
    };
    let now_secs = crate::core::agent::now_secs();
    let row = AckRuleRow {
        id,
        kind_pattern,
        ack_until: now_secs.saturating_add(window_secs),
        created_at: now_secs,
    };
    db.upsert_alerting_ack(&row);
    Ok(row)
}

pub fn delete_ack(id: &str) -> bool {
    let Some(store) = crate::core::shared_store() else {
        return false;
    };
    let Ok(mut s) = store.lock() else {
        return false;
    };
    if let StoreEnum::Db(db) = &mut *s {
        db.delete_alerting_ack(id)
    } else {
        false
    }
}

pub fn clear_expired_acks() -> usize {
    let Some(store) = crate::core::shared_store() else {
        return 0;
    };
    let Ok(mut s) = store.lock() else { return 0 };
    if let StoreEnum::Db(db) = &mut *s {
        db.clear_expired_acks(crate::core::agent::now_secs())
    } else {
        0
    }
}

/// Determine whether `kind` is inside some ack window.
pub fn is_acked(kind: &str, now_ts: u64) -> bool {
    let Some(store) = crate::core::shared_store() else {
        return false;
    };
    let Ok(s) = store.lock() else { return false };
    let StoreEnum::Db(db) = &*s else { return false };
    db.list_active_acks(now_ts)
        .iter()
        .any(|r| kind_matches(&r.kind_pattern, kind))
}
