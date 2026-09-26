//! Shared kind/weekday/hour helpers used by routes, silences, correlations, and the simulator.
//! Mechanical move from core/alerting.rs.

/// Derive weekday from unix seconds: 0=Sun, 1=Mon, ..., 6=Sat (aligned with Unix libc).
pub(crate) fn weekday_from_unix(ts: u64) -> u8 {
    // The Unix epoch 1970-01-01 is a Thursday. Thursday in the 0=Sun representation = 4.
    let days = ts / 86400;
    ((days + 4) % 7) as u8
}

/// Derive hour-of-day from unix seconds (UTC hour; local hour needs tz data, so this uses UTC as a simplification,
/// and the UI has the user configure the silent hour range in UTC).
pub(crate) fn hour_from_unix(ts: u64) -> u8 {
    ((ts % 86400) / 3600) as u8
}

/// kind_pattern matching rules: `*` matches all / `prefix.*` prefix match / `exact` exact match.
pub(crate) fn kind_matches(pattern: &str, kind: &str) -> bool {
    if pattern == "*" || pattern.is_empty() {
        return true;
    }
    if let Some(p) = pattern.strip_suffix(".*") {
        return kind.starts_with(p) && kind.len() > p.len() + 1;
    }
    pattern == kind
}
