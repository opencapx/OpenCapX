//! v1.3 PIM built-in providers: photos.read / contacts.search / calendar.events /
//! location.get; v1.4 extends: notes.read / reminders.read / reminders.write /
//! mail.recent (docs/capability.md "Built-in providers").
//!
//! macOS-only: Photos/Contacts/Calendar/Notes/Reminders/Mail go through AppleScript
//! (each triggers the system "Automation" TCC and reports honestly when denied), and
//! location goes through a one-shot swift script (CoreLocation, triggering the
//! "Location" TCC). On non-macOS it reports honestly; plugins can override.
//!
//! Each permission is ask (photos.read / contacts.read / calendar.read / location.read /
//! notes.read / reminders.read / reminders.write / mail.read):
//! personal-data read surfaces, gated by the OpenCapX layer ask + the OS layer TCC.
//!
//! Honest limits (synced with docs): the Notes body is HTML, so snippet = the first 200
//! chars after flattening; Reminders due dates are not captured in v1 (localized-format
//! parsing is unreliable, so it is not shipped); Mail dates are localized strings (same as Calendar).
//!
//! osperm does not extend to these four: photos/contacts/calendar have no "pre-flight
//! probe without a prompt" API (the probe itself is access), and
//! system.permission_status only reports surfaces with a clean preflight
//! (screen_recording / accessibility), see the permissions.md v1.3 note.

use serde_json::{json, Value};

/// Max contacts search query length.
const QUERY_MAX: usize = 200;
/// photos.read limit max.
const PHOTO_LIMIT_MAX: i64 = 100;
/// calendar.events event-count cap (truncated within a single loop).
const EVENT_CAP: i64 = 100;

/// AppleScript string literal escaping: double the quotes.
fn escape_as(s: &str) -> String {
    s.replace('"', "\"\"")
}

/// Run a piece of AppleScript and return stdout. 30s timeout (so a hung osascript does not drag the call down).
/// Since v1.4 this goes through the appctl::osascript_ok shared helper.
#[cfg(target_os = "macos")]
fn osascript(src: &str) -> Result<String, String> {
    let out = super::appctl::osascript_ok(src, std::time::Duration::from_secs(30), "osascript")?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// photos.read

/// The N most recent media items: total count on the first line, then `id|name|date`
/// lines (date is a localized string — the Photos AppleScript does not emit ISO; an
/// honest limit, noted in the docs).
#[cfg(target_os = "macos")]
fn photos_script(limit: i64) -> String {
    format!(
        r#"tell application "Photos"
	set theItems to media items
	set n to count of theItems
	set out to ""
	set firstIdx to n - {lim} + 1
	if firstIdx < 1 then set firstIdx to 1
	repeat with i from firstIdx to n
		set m to item i of theItems
		set out to out & (id of m) & "|" & (name of m) & "|" & ((date of m) as text) & linefeed
	end repeat
	return (n as text) & linefeed & out
end tell"#,
        lim = limit
    )
}

/// Pure function: parse photos_script's output.
fn parse_photos(out: &str) -> (i64, Vec<Value>) {
    let mut lines = out.lines();
    let count: i64 = lines.next().unwrap_or("").trim().parse().unwrap_or(0);
    let mut photos = Vec::new();
    for l in lines {
        let l = l.trim_end_matches('\r');
        if l.is_empty() {
            continue;
        }
        let mut parts = l.splitn(3, '|');
        let id = parts.next().unwrap_or("");
        let name = parts.next().unwrap_or("");
        let date = parts.next().unwrap_or("");
        if id.is_empty() {
            continue;
        }
        photos.push(json!({ "id": id, "name": name, "date": date }));
    }
    (count, photos)
}

/// photos.read built-in implementation.
pub fn photos(input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        return Err("photos.read builtin is macOS-only (Photos AppleScript)".into());
    }
    #[cfg(target_os = "macos")]
    {
        let limit = input.get("limit").and_then(|l| l.as_i64()).unwrap_or(20);
        if !(1..=PHOTO_LIMIT_MAX).contains(&limit) {
            return Err(format!(
                "invalid input: limit must be 1..={} (default 20)",
                PHOTO_LIMIT_MAX
            ));
        }
        let out = osascript(&photos_script(limit))?;
        let (count, photos) = parse_photos(&out);
        Ok(json!({ "count": count, "photos": photos }))
    }
}

// ---------------------------------------------------------------------------
// contacts.search

/// Contacts whose name contains the query: `id|name` lines. query is escaped, interpolated safely.
#[cfg(target_os = "macos")]
fn contacts_script(query: &str, limit: i64) -> String {
    format!(
        r#"tell application "Contacts"
	set thePeople to (every person whose name contains "{q}")
	set n to count of thePeople
	if n > {lim} then set n to {lim}
	set out to ""
	repeat with i from 1 to n
		set p to item i of thePeople
		set out to out & (id of p) & "|" & (name of p) & linefeed
	end repeat
	return out
end tell"#,
        q = escape_as(query),
        lim = limit
    )
}

/// Pure function: parse contacts_script's output.
fn parse_contacts(out: &str) -> Vec<Value> {
    out.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts = l.splitn(2, '|');
            json!({
                "id": parts.next().unwrap_or(""),
                "name": parts.next().unwrap_or(""),
            })
        })
        .collect()
}

/// contacts.search built-in implementation.
pub fn contacts(input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        return Err("contacts.search builtin is macOS-only (Contacts AppleScript)".into());
    }
    #[cfg(target_os = "macos")]
    {
        let Some(query) = input.get("query").and_then(|q| q.as_str()) else {
            return Err("invalid input: query (string) required".into());
        };
        if query.is_empty() {
            return Err("invalid input: query must be non-empty".into());
        }
        if query.chars().count() > QUERY_MAX {
            return Err(format!("invalid input: query must be ≤{} chars", QUERY_MAX));
        }
        let limit = input.get("limit").and_then(|l| l.as_i64()).unwrap_or(20);
        if !(1..=100).contains(&limit) {
            return Err("invalid input: limit must be 1..=100 (default 20)".into());
        }
        let out = osascript(&contacts_script(query, limit))?;
        Ok(json!({ "contacts": parse_contacts(&out) }))
    }
}

// ---------------------------------------------------------------------------
// calendar.events

/// Events over the next N days: `start|end|summary|calendar` lines, capped at EVENT_CAP.
/// Dates are localized strings (the Calendar AppleScript likewise does not emit ISO).
#[cfg(target_os = "macos")]
fn calendar_script(days: i64, cal: Option<&str>) -> String {
    let filter = match cal {
        None => "true".to_string(),
        Some(c) => format!("name of c is \"{}\"", escape_as(c)),
    };
    format!(
        r#"tell application "Calendar"
	set out to ""
	set startDate to current date
	set endDate to startDate + ({days} * days)
	set cnt to 0
	repeat with c in calendars
		if {filter} then
			tell c
				set evs to (every event whose start date ≥ startDate and start date ≤ endDate)
			end tell
			repeat with e in evs
				if cnt ≥ {cap} then exit repeat
				set cnt to cnt + 1
				set out to out & ((start date of e) as text) & "|" & ((end date of e) as text) & "|" & ((summary of e) as text) & "|" & (name of c) & linefeed
			end repeat
		end if
		if cnt ≥ {cap} then exit repeat
	end repeat
	return out
end tell"#,
        days = days,
        filter = filter,
        cap = EVENT_CAP
    )
}

/// Pure function: parse calendar_script's output.
fn parse_events(out: &str) -> Vec<Value> {
    out.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts = l.splitn(4, '|');
            json!({
                "start": parts.next().unwrap_or(""),
                "end": parts.next().unwrap_or(""),
                "summary": parts.next().unwrap_or(""),
                "calendar": parts.next().unwrap_or(""),
            })
        })
        .collect()
}

/// calendar.events built-in implementation.
pub fn calendar(input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        return Err("calendar.events builtin is macOS-only (Calendar AppleScript)".into());
    }
    #[cfg(target_os = "macos")]
    {
        let days = input.get("days").and_then(|d| d.as_i64()).unwrap_or(7);
        if !(1..=31).contains(&days) {
            return Err("invalid input: days must be 1..=31 (default 7)".into());
        }
        let cal = input
            .get("calendar")
            .and_then(|c| c.as_str())
            .filter(|c| !c.is_empty());
        if let Some(c) = cal {
            if c.chars().count() > 200 {
                return Err("invalid input: calendar must be ≤200 chars".into());
            }
        }
        let out = osascript(&calendar_script(days, cal))?;
        Ok(json!({ "events": parse_events(&out) }))
    }
}

// ---------------------------------------------------------------------------
// location.get

/// One-shot CoreLocation script: prints {lat, lon, accuracy} JSON to stdout within 7s;
/// a timeout/denial goes to stderr. Executed by the swift interpreter (present once CLT
/// is installed). The first run triggers the system location permission prompt — the
/// user cannot click in time within 7s, so this run times out and errors; the second
/// call succeeds after granting (honest behavior, no pretending).
#[cfg(target_os = "macos")]
const LOC_SWIFT: &str = r#"
import CoreLocation
import Foundation
let sem = DispatchSemaphore(value: 0)
final class L: NSObject, CLLocationManagerDelegate {
    let m = CLLocationManager()
    var done = false
    func fail(_ s: String) {
        if !done {
            done = true
            fputs("location: \(s)\n", stderr)
            sem.signal()
        }
    }
    func go() {
        m.delegate = self
        m.requestLocation()
    }
    func locationManager(_ m: CLLocationManager, didUpdateLocations a: [CLLocation]) {
        if !done, let c = a.first {
            let s = String(format: "{\"lat\":%.6f,\"lon\":%.6f,\"accuracy\":%.1f}",
                           c.coordinate.latitude, c.coordinate.longitude, c.horizontalAccuracy)
            print(s)
        }
        done = true
        sem.signal()
    }
    func locationManager(_ m: CLLocationManager, didFailWithError e: Error) {
        fail(e.localizedDescription)
    }
    func locationManagerDidChangeAuthorization(_ m: CLLocationManager) {
        if m.authorizationStatus == .denied || m.authorizationStatus == .restricted {
            fail("authorization denied (System Settings → Privacy & Security → Location)")
        }
    }
}
let l = L()
l.go()
_ = sem.wait(timeout: .now() + 7)
"#;

/// location.get built-in implementation.
pub fn location(_input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err("location.get builtin is macOS-only (CoreLocation)".into());
    }
    #[cfg(target_os = "macos")]
    {
        let dir = super::vision::cache_dir()?;
        let path = dir.join(format!("location-{}.swift", std::process::id()));
        std::fs::write(&path, LOC_SWIFT).map_err(|e| format!("script write failed: {}", e))?;
        let mut cmd = std::process::Command::new("swift");
        cmd.arg(&path);
        // swift cold start + location window: 15s total budget (the script self-terminates at 7s)
        let out = super::appctl::run_with_timeout(cmd, std::time::Duration::from_secs(15));
        let _ = std::fs::remove_file(&path);
        let out = out.map_err(|e| format!("swift failed: {} (install Xcode Command Line Tools)", e))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !stdout.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(&stdout) {
                if v.get("lat").is_some() {
                    return Ok(v);
                }
            }
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            "no fix within timeout (first run may need the Location prompt; retry after granting)".to_string()
        } else {
            stderr.chars().take(300).collect()
        };
        Err(format!("location failed: {}", detail))
    }
}

// ---------------------------------------------------------------------------
// v1.4: notes.read / reminders.read / reminders.write / mail.recent

/// notes.read limit max.
const NOTE_LIMIT_MAX: i64 = 100;
/// notes.read snippet truncation length (the body is HTML; flatten first, then truncate).
const NOTE_SNIPPET: usize = 200;
/// reminders enumeration cap / write field caps.
const REM_LIMIT_MAX: i64 = 200;
const REM_NAME_MAX: usize = 500;
const REM_BODY_MAX: usize = 2000;
/// mail.recent limit max (count itself is slow when the inbox is large).
const MAIL_LIMIT_MAX: i64 = 50;

/// Shared AppleScript handlers: fl flattens linefeeds/returns/tabs into spaces (keeping
/// the line protocol flat), and tx falls back to "" for missing value (so a mail with no
/// subject/sender does not blow up the whole output).
#[cfg(target_os = "macos")]
const PIM_HANDLERS: &str = r#"
on fl(t)
	set d to AppleScript's text item delimiters
	set AppleScript's text item delimiters to {linefeed, return, tab}
	set parts to text items of (t as text)
	set AppleScript's text item delimiters to " "
	set t to parts as text
	set AppleScript's text item delimiters to d
	return t
end fl
on tx(v)
	try
		return my fl(v as text)
	on error
		return ""
	end try
end tx
"#;

/// The N most recent notes: `id|name|snippet` lines (newest first, taking the tail; the
/// reversal is done by the AppleScript-side firstIdx; total count on the first line).
/// snippet = the first NOTE_SNIPPET chars of the flattened body.
#[cfg(target_os = "macos")]
fn notes_script(limit: i64) -> String {
    format!(
        r#"{h}tell application "Notes"
	set theNotes to notes
	set n to count of theNotes
	set out to ""
	set firstIdx to n - {lim} + 1
	if firstIdx < 1 then set firstIdx to 1
	repeat with i from firstIdx to n
		set nt to item i of theNotes
		set b to ""
		try
			set b to body of nt
		end try
		set b to my fl(b)
		if length of b > {snip} then set b to text 1 thru {snip} of b
		set out to out & (id of nt) & "|" & my tx(name of nt) & "|" & b & linefeed
	end repeat
	return (n as text) & linefeed & out
end tell"#,
        h = PIM_HANDLERS,
        lim = limit,
        snip = NOTE_SNIPPET
    )
}

/// Pure function: parse notes_script's output.
fn parse_notes(out: &str) -> (i64, Vec<Value>) {
    let mut lines = out.lines();
    let count: i64 = lines.next().unwrap_or("").trim().parse().unwrap_or(0);
    let mut notes = Vec::new();
    for l in lines {
        let l = l.trim_end_matches('\r');
        if l.is_empty() {
            continue;
        }
        let mut parts = l.splitn(3, '|');
        let id = parts.next().unwrap_or("");
        let name = parts.next().unwrap_or("");
        let snippet = parts.next().unwrap_or("");
        if id.is_empty() {
            continue;
        }
        notes.push(json!({ "id": id, "name": name, "snippet": snippet }));
    }
    (count, notes)
}

/// notes.read built-in implementation.
pub fn notes(input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        return Err("notes.read builtin is macOS-only (Notes AppleScript)".into());
    }
    #[cfg(target_os = "macos")]
    {
        let limit = input.get("limit").and_then(|l| l.as_i64()).unwrap_or(20);
        if !(1..=NOTE_LIMIT_MAX).contains(&limit) {
            return Err(format!(
                "invalid input: limit must be 1..={} (default 20)",
                NOTE_LIMIT_MAX
            ));
        }
        let out = osascript(&notes_script(limit))?;
        let (count, notes) = parse_notes(&out);
        Ok(json!({ "count": count, "notes": notes }))
    }
}

/// Reminders: `id|name|due|completed` lines. incomplete_only defaults to true (only incomplete ones).
#[cfg(target_os = "macos")]
fn reminders_script(incomplete_only: bool, limit: i64) -> String {
    let filter = if incomplete_only { "completed of r is false" } else { "true" };
    format!(
        r#"{h}tell application "Reminders"
	set out to ""
	set cnt to 0
	repeat with r in (every reminder)
		if {filter} then
			set out to out & (id of r) & "|" & my tx(name of r) & "|" & my tx(due date of r) & "|" & ((completed of r) as integer) & linefeed
			set cnt to cnt + 1
			if cnt ≥ {lim} then exit repeat
		end if
	end repeat
	return out
end tell"#,
        h = PIM_HANDLERS,
        filter = filter,
        lim = limit
    )
}

/// Pure function: parse reminders_script's output.
fn parse_reminders(out: &str) -> Vec<Value> {
    out.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts = l.splitn(4, '|');
            json!({
                "id": parts.next().unwrap_or(""),
                "name": parts.next().unwrap_or(""),
                "due": parts.next().unwrap_or(""),
                "completed": parts.next().unwrap_or("0") == "1",
            })
        })
        .collect()
}

/// reminders.read built-in implementation.
pub fn reminders_read(input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        return Err("reminders.read builtin is macOS-only (Reminders AppleScript)".into());
    }
    #[cfg(target_os = "macos")]
    {
        let limit = input.get("limit").and_then(|l| l.as_i64()).unwrap_or(100);
        if !(1..=REM_LIMIT_MAX).contains(&limit) {
            return Err(format!(
                "invalid input: limit must be 1..={} (default 100)",
                REM_LIMIT_MAX
            ));
        }
        let incomplete_only =
            input.get("incomplete_only").and_then(|b| b.as_bool()).unwrap_or(true);
        let out = osascript(&reminders_script(incomplete_only, limit))?;
        Ok(json!({ "reminders": parse_reminders(&out) }))
    }
}

/// Append a reminder to the end of the given list (defaults to the first list). name/body
/// are escaped; AppleScript string literals do not accept raw newlines, so the body is
/// flattened first.
#[cfg(target_os = "macos")]
fn reminders_write_script(list: Option<&str>, name: &str, body: &str) -> String {
    let target = match list {
        None => "first list".to_string(),
        Some(l) => format!("list \"{}\"", escape_as(l)),
    };
    format!(
        r#"{h}tell application "Reminders"
	tell {target}
		make new reminder at end with properties {{name:"{n}", body:"{b}"}}
	end tell
end tell"#,
        h = PIM_HANDLERS,
        target = target,
        n = escape_as(name),
        b = escape_as(body)
    )
}

/// reminders.write built-in implementation. Due dates are not captured in v1:
/// natural-language/localized-format parsing is unreliable, so not shipping beats
/// shipping it (noted in the docs).
pub fn reminders_write(input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        return Err("reminders.write builtin is macOS-only (Reminders AppleScript)".into());
    }
    #[cfg(target_os = "macos")]
    {
        let Some(name) = input.get("name").and_then(|n| n.as_str()) else {
            return Err("invalid input: name (string) required".into());
        };
        if name.trim().is_empty() {
            return Err("invalid input: name must be non-empty".into());
        }
        if name.chars().count() > REM_NAME_MAX {
            return Err(format!("invalid input: name must be ≤{} chars", REM_NAME_MAX));
        }
        let body = input.get("body").and_then(|b| b.as_str()).unwrap_or("");
        if body.chars().count() > REM_BODY_MAX {
            return Err(format!("invalid input: body must be ≤{} chars", REM_BODY_MAX));
        }
        // Newlines/tabs are illegal in AppleScript literals; flatten them
        let body = body.replace(['\n', '\r', '\t'], " ");
        let list = input
            .get("list")
            .and_then(|l| l.as_str())
            .filter(|l| !l.trim().is_empty());
        if let Some(l) = list {
            if l.chars().count() > 200 {
                return Err("invalid input: list must be ≤200 chars".into());
            }
        }
        let _ = osascript(&reminders_write_script(list, name, &body))?;
        Ok(json!({ "ok": true, "list": list.unwrap_or("(first list)") }))
    }
}

/// The N most recent inbox messages: `id|subject|sender|date|read` lines. Dates are
/// localized strings (the Mail AppleScript does not emit ISO; the same honest limit as Calendar).
#[cfg(target_os = "macos")]
fn mail_script(limit: i64) -> String {
    format!(
        r#"{h}tell application "Mail"
	set out to ""
	set n to count of messages of inbox
	if n > {lim} then set n to {lim}
	repeat with i from 1 to n
		set m to message i of inbox
		set out to out & (id of m) & "|" & my tx(subject of m) & "|" & my tx(sender of m) & "|" & my tx(date received of m) & "|" & ((read status of m) as integer) & linefeed
	end repeat
	return out
end tell"#,
        h = PIM_HANDLERS,
        lim = limit
    )
}

/// Pure function: parse mail_script's output.
fn parse_mail(out: &str) -> Vec<Value> {
    out.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts = l.splitn(5, '|');
            json!({
                "id": parts.next().unwrap_or(""),
                "subject": parts.next().unwrap_or(""),
                "sender": parts.next().unwrap_or(""),
                "date": parts.next().unwrap_or(""),
                "read": parts.next().unwrap_or("0") == "1",
            })
        })
        .collect()
}

/// mail.recent built-in implementation.
pub fn mail(input: &Value) -> Result<Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = input;
        return Err("mail.recent builtin is macOS-only (Mail AppleScript)".into());
    }
    #[cfg(target_os = "macos")]
    {
        let limit = input.get("limit").and_then(|l| l.as_i64()).unwrap_or(10);
        if !(1..=MAIL_LIMIT_MAX).contains(&limit) {
            return Err(format!(
                "invalid input: limit must be 1..={} (default 10)",
                MAIL_LIMIT_MAX
            ));
        }
        let out = osascript(&mail_script(limit))?;
        Ok(json!({ "messages": parse_mail(&out) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_applescript_quotes() {
        assert_eq!(escape_as("O\"Brien"), "O\"\"Brien");
        assert_eq!(escape_as("plain"), "plain");
    }

    #[test]
    fn parses_photos_output() {
        let (count, photos) = parse_photos("42\r\nID1|Sunset|2026-09-01 10:00:00\r\n\r\nID2|Cat|\r\n");
        assert_eq!(count, 42);
        assert_eq!(photos.len(), 2);
        assert_eq!(photos[0]["id"], json!("ID1"));
        assert_eq!(photos[0]["date"], json!("2026-09-01 10:00:00"));
        assert_eq!(photos[1]["name"], json!("Cat"));
        assert_eq!(photos[1]["date"], json!(""));
        // A bad first line is not an error: count 0
        let (count, _) = parse_photos("garbage\n");
        assert_eq!(count, 0);
    }

    #[test]
    fn parses_contacts_and_events_output() {
        let cs = parse_contacts("ABPerson:1|Alice\r\nABPerson:2|Bob|extra\r\n");
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0]["name"], json!("Alice"));
        // splitn(2): a | inside the name stays in name
        assert_eq!(cs[1]["name"], json!("Bob|extra"));
        let evs = parse_events("Mon 10:00|Mon 11:00|Standup|Work\r\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0]["summary"], json!("Standup"));
        assert_eq!(evs[0]["calendar"], json!("Work"));
    }

    #[test]
    fn parses_notes_reminders_mail_rows() {
        let (count, ns) = parse_notes("7\r\nX1|Groce|ry list|bread & butter\r\nX2||\r\n");
        assert_eq!(count, 7);
        assert_eq!(ns.len(), 2);
        // splitn(3): everything from the second | onward goes to snippet
        assert_eq!(ns[0]["name"], json!("Groce"));
        assert_eq!(ns[0]["snippet"], json!("ry list|bread & butter"));
        assert_eq!(ns[1]["snippet"], json!(""));
        let rs = parse_reminders("R1|Buy milk|vendredi 20 septembre 09:00:00|0\r\nR2|Done thing||1\r\n");
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0]["due"], json!("vendredi 20 septembre 09:00:00"));
        assert_eq!(rs[0]["completed"], json!(false));
        assert_eq!(rs[1]["due"], json!(""));
        assert_eq!(rs[1]["completed"], json!(true));
        let ms = parse_mail("M1|Hello|a@b.c|2026-09-14|1\r\nM2||||0\r\n");
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0]["read"], json!(true));
        assert_eq!(ms[1]["subject"], json!(""));
        assert_eq!(ms[1]["read"], json!(false));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn v14_scripts_embed_escaped_and_capped() {
        let s = notes_script(5);
        assert!(s.contains("on fl(t)"), "{}", s);
        assert!(s.contains("n - 5 + 1"), "{}", s);
        assert!(s.contains("text 1 thru 200"), "{}", s);
        let s = reminders_script(true, 50);
        assert!(s.contains("completed of r is false"), "{}", s);
        assert!(s.contains("if cnt ≥ 50 then exit repeat"), "{}", s);
        let s = reminders_script(false, 50);
        assert!(s.contains("if true then"), "{}", s);
        let s = reminders_write_script(Some("Work"), "Say \"hi\"", "flat body");
        assert!(s.contains("tell list \"Work\""), "{}", s);
        assert!(s.contains("name:\"Say \"\"hi\"\"\""), "{}", s);
        assert!(!s.contains("first list"), "{}", s);
        let s = reminders_write_script(None, "x", "");
        assert!(s.contains("tell first list"), "{}", s);
        let s = mail_script(25);
        assert!(s.contains("if n > 25 then set n to 25"), "{}", s);
        assert!(s.contains("read status of m"), "{}", s);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn validates_v14_inputs() {
        assert!(notes(&json!({ "limit": 0 })).unwrap_err().contains("limit"));
        assert!(notes(&json!({ "limit": 101 })).unwrap_err().contains("limit"));
        assert!(reminders_read(&json!({ "limit": 0 })).unwrap_err().contains("limit"));
        assert!(reminders_read(&json!({ "limit": 201 })).unwrap_err().contains("limit"));
        assert!(reminders_write(&json!({})).unwrap_err().contains("name"));
        assert!(reminders_write(&json!({ "name": "  " })).unwrap_err().contains("non-empty"));
        assert!(reminders_write(&json!({ "name": "x".repeat(501) })).unwrap_err().contains("500"));
        assert!(reminders_write(&json!({ "name": "x", "body": "y".repeat(2001) }))
            .unwrap_err()
            .contains("2000"));
        assert!(mail(&json!({ "limit": 0 })).unwrap_err().contains("limit"));
        assert!(mail(&json!({ "limit": 51 })).unwrap_err().contains("limit"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scripts_embed_escaped_query_and_limits() {
        let s = contacts_script("O\"B", 5);
        assert!(s.contains("name contains \"O\"\"B\""), "{}", s);
        assert!(s.contains("if n > 5 then set n to 5"), "{}", s);
        let s = calendar_script(7, Some("Work"));
        assert!(s.contains("name of c is \"Work\""), "{}", s);
        let s = calendar_script(7, None);
        assert!(s.contains("if true then"), "{}", s);
        let s = photos_script(3);
        assert!(s.contains("n - 3 + 1"), "{}", s);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn pim_reports_platform_limit() {
        assert!(photos(&json!({})).unwrap_err().contains("macOS-only"));
        assert!(contacts(&json!({})).unwrap_err().contains("macOS-only"));
        assert!(calendar(&json!({})).unwrap_err().contains("macOS-only"));
        assert!(location(&json!({})).unwrap_err().contains("macOS-only"));
        assert!(notes(&json!({})).unwrap_err().contains("macOS-only"));
        assert!(reminders_read(&json!({})).unwrap_err().contains("macOS-only"));
        assert!(reminders_write(&json!({})).unwrap_err().contains("macOS-only"));
        assert!(mail(&json!({})).unwrap_err().contains("macOS-only"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn validates_limits() {
        assert!(photos(&json!({ "limit": 0 })).unwrap_err().contains("limit"));
        assert!(photos(&json!({ "limit": 101 })).unwrap_err().contains("limit"));
        assert!(contacts(&json!({})).unwrap_err().contains("query"));
        assert!(contacts(&json!({ "query": "" })).unwrap_err().contains("non-empty"));
        assert!(contacts(&json!({ "query": "x".repeat(201) })).unwrap_err().contains("200"));
        assert!(calendar(&json!({ "days": 0 })).unwrap_err().contains("days"));
        assert!(calendar(&json!({ "days": 32 })).unwrap_err().contains("days"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches real Photos TCC; run with --ignored manually"]
    fn photos_real_manual() {
        let out = photos(&json!({ "limit": 5 })).unwrap();
        assert!(out["count"].as_i64().unwrap() >= 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches real Contacts TCC; run with --ignored manually"]
    fn contacts_real_manual() {
        let out = contacts(&json!({ "query": "a", "limit": 5 })).unwrap();
        assert!(out["contacts"].is_array());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches real Calendar TCC; run with --ignored manually"]
    fn calendar_real_manual() {
        let out = calendar(&json!({ "days": 1 })).unwrap();
        assert!(out["events"].is_array());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches real Location TCC (first run prompts and times out); run with --ignored manually"]
    fn location_real_manual() {
        let out = location(&json!({})).unwrap();
        assert!(out["lat"].as_f64().is_some(), "{}", out);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches real Notes TCC; run with --ignored manually"]
    fn notes_real_manual() {
        let out = notes(&json!({ "limit": 5 })).unwrap();
        assert!(out["count"].as_i64().unwrap() >= 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches real Reminders TCC; run with --ignored manually"]
    fn reminders_read_real_manual() {
        let out = reminders_read(&json!({ "limit": 10 })).unwrap();
        assert!(out["reminders"].is_array());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches real Reminders TCC and CREATES a reminder; run with --ignored manually"]
    fn reminders_write_real_manual() {
        let out = reminders_write(&json!({ "name": "opencapx test reminder (safe to delete)" }))
            .unwrap();
        assert_eq!(out["ok"], json!(true));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches real Mail TCC; run with --ignored manually"]
    fn mail_real_manual() {
        let out = mail(&json!({ "limit": 5 })).unwrap();
        assert!(out["messages"].is_array());
    }
}
