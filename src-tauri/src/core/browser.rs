//! v1.2 browser builtin provider (browser.open / browser.read, docs/capability.md "builtin providers").
//!
//! - `browser.open`: a platform command invokes the system default browser (macOS `open` / Linux `xdg-open` /
//!   Windows `rundll32 url.dll,FileProtocolHandler` — avoiding `cmd /C start`'s
//!   quote/title parsing traps). The scheme allowlist is http/https, blocking file://, javascript:,
//!   custom schemes, and command-injection characters.
//! - `browser.read`: reqwest fetches HTML (no JS rendering — an honest limitation, plugins can override) +
//!   scraper extracts the body. The network egress is gated by browser.control ask; once authorized, any URL
//!   (including localhost/intranet) is reachable — domain scope is v2 (the structure is already reserved in permissions.md).

use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

/// browser.read response cap (same tier as file.read; prevents the Agent from swallowing a giant page).
const READ_BODY_CAP: usize = 2 * 1024 * 1024;
/// Body truncation threshold (characters, not bytes).
const TEXT_CAP: usize = 100_000;

/// URL allowlist validation: absolute http(s), ≤2048, no control characters/whitespace.
/// Control characters are also the backstop for the platform command-injection surface (every platform passes the URL as a single argument, never through a shell).
fn validate_http_url(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("invalid input: url must be an absolute http(s) URL".into());
    }
    if url.is_empty() || url.len() > 2048 {
        return Err("invalid input: url must be 1..=2048 chars".into());
    }
    if url.bytes().any(|b| b.is_ascii_control() || b == b' ') {
        return Err("invalid input: url must not contain control chars or spaces".into());
    }
    Ok(())
}

/// Platform command: open the system default browser. The URL has already passed validate_http_url.
#[cfg(target_os = "macos")]
fn open_command(url: &str) -> std::process::Command {
    let mut c = std::process::Command::new("open");
    c.arg(url);
    c
}
#[cfg(target_os = "linux")]
fn open_command(url: &str) -> std::process::Command {
    let mut c = std::process::Command::new("xdg-open");
    c.arg(url);
    c
}
#[cfg(target_os = "windows")]
fn open_command(url: &str) -> std::process::Command {
    let mut c = std::process::Command::new("rundll32");
    c.args(["url.dll,FileProtocolHandler", url]);
    c
}

/// browser.open builtin implementation.
pub fn open(input: &Value) -> Result<Value, String> {
    let Some(url) = input.get("url").and_then(|u| u.as_str()) else {
        return Err("invalid input: url (string) required".into());
    };
    validate_http_url(url)?;
    let status = open_command(url)
        .status()
        .map_err(|e| {
            #[cfg(target_os = "linux")]
            let hint = " (install xdg-utils)";
            #[cfg(not(target_os = "linux"))]
            let hint = "";
            format!("open failed{}: {}", hint, e)
        })?;
    if !status.success() {
        return Err(format!("open failed: exit {:?}", status.code()));
    }
    Ok(json!({ "ok": true }))
}

/// The scheme blocklist for v1.3 url.scheme.open: schemes that can read the local disk / execute scripts / escalate privileges,
/// and browser-internal schemes are never opened. http(s) is not accepted either — that belongs to browser.open.
const BLOCKED_SCHEMES: &[&str] = &[
    "file", "javascript", "data", "vbscript", "about", "blob", "view-source", "jar",
    "ws", "wss", "chrome", "chromium", "chrome-extension", "moz-extension", "intent",
];

/// scheme URL validation (pure function): the scheme syntax is valid (starts with a letter, alphanumeric +.-,
/// 2..=30), not blocklisted, not http(s), the whole ≤2048, no control characters/whitespace.
/// Returns the lowercased scheme.
fn validate_scheme_url(url: &str) -> Result<String, String> {
    if url.is_empty() || url.len() > 2048 {
        return Err("invalid input: url must be 1..=2048 chars".into());
    }
    if url.bytes().any(|b| b.is_ascii_control() || b == b' ') {
        return Err("invalid input: url must not contain control chars or spaces".into());
    }
    let Some((scheme, _rest)) = url.split_once(':') else {
        return Err("invalid input: url must look like scheme:rest".into());
    };
    let lower = scheme.to_ascii_lowercase();
    if lower == "http" || lower == "https" {
        return Err("invalid input: http(s) URLs belong to browser.open".into());
    }
    if BLOCKED_SCHEMES.contains(&lower.as_str()) {
        return Err(format!("scheme not allowed: {}", lower));
    }
    let mut chars = lower.chars();
    let valid = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        }
        _ => false,
    };
    if !valid || lower.len() < 2 || lower.len() > 30 {
        return Err("invalid input: url scheme must be 2..=30 chars ([a-z][a-z0-9+.-])".into());
    }
    Ok(lower)
}

/// url.scheme.open builtin implementation (v1.3): hand a registered scheme to the system to open
/// (mailto: / zoommtg: / tg: …). Whether a handler is registered is up to the OS; failures are reported honestly.
/// Permission `url.scheme.open` (ask): a scheme handler can be any installed app, so ask before opening.
pub fn open_scheme(input: &Value) -> Result<Value, String> {
    let Some(url) = input.get("url").and_then(|u| u.as_str()) else {
        return Err("invalid input: url (string) required".into());
    };
    validate_scheme_url(url)?;
    let status = open_command(url)
        .status()
        .map_err(|e| {
            #[cfg(target_os = "linux")]
            let hint = " (install xdg-utils)";
            #[cfg(not(target_os = "linux"))]
            let hint = "";
            format!("open failed{}: {}", hint, e)
        })?;
    if !status.success() {
        return Err(format!(
            "open failed: exit {:?} (no handler registered for this scheme?)",
            status.code()
        ));
    }
    Ok(json!({ "ok": true }))
}

/// Case-insensitively strip script/style blocks (including unclosed ones: delete to EOF).
/// No regex: a hand-written scan for the two tags, which also cleans code/style noise out of the body.
fn strip_script_style(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut rest = 0usize; // Position consumed so far (lower and html have equal length, indices interchangeable)
    loop {
        let (start, end_tag) = match (
            lower[rest..].find("<script"),
            lower[rest..].find("<style"),
        ) {
            (Some(s), None) => (s, "</script>"),
            (None, Some(st)) => (st, "</style>"),
            (Some(s), Some(st)) => {
                if s <= st {
                    (s, "</script>")
                } else {
                    (st, "</style>")
                }
            }
            (None, None) => break,
        };
        let abs_start = rest + start;
        out.push_str(&html[rest..abs_start]);
        // Unclosed block: delete to EOF, done
        match lower[abs_start..].find(end_tag) {
            Some(e) => {
                rest = abs_start + e + end_tag.len();
            }
            None => {
                rest = html.len();
            }
        }
    }
    out.push_str(&html[rest..]);
    out
}

/// Extract the title + normalized body from HTML (already stripped of script/style), capped at TEXT_CAP.
/// The body is taken from <body> only (falling back to root only when there is no body tag); the title is not mixed into the body;
/// text nodes are joined with spaces then normalized, preventing adjacent elements' text from sticking together.
fn extract_text(html: &str) -> (String, String, bool) {
    let doc = scraper::Html::parse_document(html);
    let title = scraper::Selector::parse("title")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        .map(|t| {
            t.text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let raw = match scraper::Selector::parse("body")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
    {
        Some(body) => body.text().collect::<Vec<_>>().join(" "),
        None => doc.root_element().text().collect::<Vec<_>>().join(" "),
    };
    let mut text = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let truncated = text.chars().count() > TEXT_CAP;
    if truncated {
        text = text.chars().take(TEXT_CAP).collect();
    }
    (title, text, truncated)
}

/// browser.read builtin implementation: GET → cap → extract body.
pub fn read(input: &Value) -> Result<Value, String> {
    let Some(url) = input.get("url").and_then(|u| u.as_str()) else {
        return Err("invalid input: url (string) required".into());
    };
    validate_http_url(url)?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let mut resp = client
        .get(url)
        .header("User-Agent", "OpenCapX/0.2 browser.read builtin")
        .send()
        .map_err(|e| format!("url unreachable: {}", e))?
        .error_for_status()
        .map_err(|e| format!("http error: {}", e))?;

    // If a Content-Type header is present it must be text/html (missing is allowed — many servers omit it)
    if let Some(ct) = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        if !ct.to_ascii_lowercase().starts_with("text/html") {
            return Err(format!(
                "unsupported content type: {} (builtin reads HTML only; install a browser plugin to override)",
                ct
            ));
        }
    }
    // Content-Length pre-check + actual read cap (chunked transfer has no CL)
    if let Some(len) = resp.content_length() {
        if len as usize > READ_BODY_CAP {
            return Err(format!("response too large (max {} bytes)", READ_BODY_CAP));
        }
    }
    let mut body = String::new();
    let mut limited = resp.take((READ_BODY_CAP + 1) as u64);
    limited
        .read_to_string(&mut body)
        .map_err(|e| format!("body read failed: {}", e))?;
    if body.len() > READ_BODY_CAP {
        return Err(format!("response too large (max {} bytes)", READ_BODY_CAP));
    }

    let (title, text, truncated) = extract_text(&strip_script_style(&body));
    Ok(json!({ "title": title, "text": text, "truncated": truncated }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_url_scheme_length_and_control_chars() {
        assert!(validate_http_url("https://example.com/x?y=1").is_ok());
        assert!(validate_http_url("HTTP://EXAMPLE.COM").is_ok());
        assert!(validate_http_url("file:///etc/passwd").is_err());
        assert!(validate_http_url("javascript:alert(1)").is_err());
        assert!(validate_http_url("ftp://x").is_err());
        assert!(validate_http_url("macos://open").is_err());
        assert!(validate_http_url("").is_err());
        assert!(validate_http_url("https://ex ample.com").is_err());
        assert!(validate_http_url("https://x/\n").is_err());
        assert!(validate_http_url(&format!("https://x/{}", "a".repeat(3000))).is_err());
    }

    #[test]
    fn strips_script_and_style_blocks() {
        let html = r#"<html><style>body{color:red}</style><body>
<script type="text/javascript">var x = "</scr" + "ipt>";</script>
<p>keep me</p>
<SCRIPT>more noise</SCRIPT>
</body></html>"#;
        let out = strip_script_style(html);
        assert!(out.contains("keep me"));
        assert!(!out.to_ascii_lowercase().contains("var x"));
        assert!(!out.to_ascii_lowercase().contains("color:red"));
        assert!(!out.to_ascii_lowercase().contains("more noise"));
        // Unclosed script: delete to EOF, no panic
        let out2 = strip_script_style("<p>a</p><script>never closed");
        assert!(out2.contains("<p>a</p>"));
        assert!(!out2.contains("never"));
    }

    #[test]
    fn extracts_title_text_and_truncation() {
        let html = r#"<html><head><title>  Hello   Page </title></head>
<body><h1>Heading</h1><p>Some    body text.</p></body></html>"#;
        let (title, text, truncated) = extract_text(html);
        assert_eq!(title, "Hello Page");
        assert_eq!(text, "Heading Some body text.");
        assert!(!truncated);

        let big = format!("<html><body>{}</body></html>", "word ".repeat(60_000));
        let (_, text, truncated) = extract_text(&big);
        assert!(truncated);
        assert_eq!(text.chars().count(), TEXT_CAP);
    }

    #[test]
    fn validates_scheme_url_allowlist() {
        // A valid registered scheme is allowed, returning the lowercased scheme
        assert_eq!(validate_scheme_url("mailto:a@b.c").unwrap(), "mailto");
        assert_eq!(validate_scheme_url("zoommtg://join?conf=1").unwrap(), "zoommtg");
        assert_eq!(validate_scheme_url("MACAPPS:something").unwrap(), "macapps");
        // Blocklist: local disk / scripts / browser-internal
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "data:text/html,x",
                    "vbscript:x", "about:blank", "blob:https://x", "view-source:https://x",
                    "chrome://settings", "moz-extension://abc"] {
            assert!(validate_scheme_url(bad).is_err(), "{} should be blocked", bad);
        }
        // http(s) belongs to browser.open
        assert!(validate_scheme_url("https://example.com").unwrap_err().contains("browser.open"));
        // Syntax: missing colon / single-char scheme / leading digit / over-long scheme
        assert!(validate_scheme_url("mailto").unwrap_err().contains("scheme:rest"));
        assert!(validate_scheme_url("a:b").unwrap_err().contains("2..=30"));
        assert!(validate_scheme_url("1abc:x").unwrap_err().contains("2..=30"));
        assert!(validate_scheme_url(&format!("{}:x", "s".repeat(31))).unwrap_err().contains("2..=30"));
        // Length / control characters
        assert!(validate_scheme_url(&format!("mailto:{}", "a".repeat(3000))).is_err());
        assert!(validate_scheme_url("mail to:x").is_err());
        assert!(validate_scheme_url("mailto:a\n").is_err());
    }

    #[test]
    fn open_scheme_requires_url() {
        let err = open_scheme(&json!({})).unwrap_err();
        assert!(err.contains("url"), "{}", err);
    }

    #[test]
    fn read_requires_url() {
        let err = read(&json!({})).unwrap_err();
        assert!(err.contains("url"), "{}", err);
    }

    /// Unreachable address (127.0.0.1:1 refuses immediately) → honest error, no external network needed.
    #[test]
    fn read_errors_on_unreachable() {
        let err = read(&json!({ "url": "http://127.0.0.1:1/" })).unwrap_err();
        assert!(err.contains("unreachable") || err.contains("http error"), "{}", err);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "opens real browser; run with --ignored manually"]
    fn open_roundtrip_manual() {
        let out = open(&json!({ "url": "https://example.com" })).unwrap();
        assert_eq!(out["ok"], json!(true));
    }

    #[test]
    #[ignore = "needs network; run with --ignored manually"]
    fn read_real_page_manual() {
        let out = read(&json!({ "url": "https://example.com" })).unwrap();
        assert_eq!(out["title"].as_str().unwrap(), "Example Domain");
        assert!(!out["text"].as_str().unwrap().is_empty());
    }
}
