//! the permission lexicon: the known-permissions table, Decision, name validation, reserved sets, defaults.
//! Mechanical move from core/permission.rs.

use super::*;

/// (permission, default decision). New permissions must be synced to docs/permissions.md.
pub const PERMISSIONS: &[(&str, &str)] = &[
    ("pet.animation", "granted"),
    ("storage.local", "granted"),
    ("notification.post", "ask"),
    ("image.read", "ask"),
    ("file.read", "ask"),
    ("clipboard.read", "ask"),
    ("clipboard.write", "ask"),
    ("network.request", "ask"),
    ("browser.control", "ask"),
    ("screen.capture", "ask"),
    ("microphone", "denied"),
    ("camera", "denied"),
    ("filesystem.write", "denied"),
    ("process.execute", "denied"),
    // v1.3 high/medium-value batch (docs/permissions.md v1.3)
    ("automation.control", "ask"), // launch/drive other apps (AppleScript), high-risk
    ("input.control", "denied"),   // synthesize keyboard/mouse events, high-risk
    ("plugin.install", "denied"), // v1.3 vocabulary placeholder: no capability mapping yet, high-risk
    ("photos.read", "ask"),
    ("contacts.read", "ask"),
    ("calendar.read", "ask"),
    ("location.read", "ask"),
    ("audio.output", "ask"), // audio output channel, same tier as notification.post
    ("url.scheme.open", "ask"), // registered schemes such as mailto:/zoommtg:
    // v1.4 high-value batch (docs/permissions.md v1.4)
    ("media.control", "ask"),        // playback control (play/pause/skip)
    ("messages.read", "denied"), // iMessage chat history (needs machine-wide Full Disk Access), high-risk
    ("window.management", "denied"), // list/focus windows (computer-use foundation), high-risk
    ("power.control", "denied"), // sleep/lock screen (irreversible actions), high-risk
    // v1.4 medium-value batch
    ("notes.read", "ask"),
    ("reminders.read", "ask"),
    ("reminders.write", "ask"), // record reminders for the user; write surface but confined to the Reminders app
    ("mail.read", "ask"),       // mail metadata (large phishing surface), high-risk
    ("system.settings", "ask"), // dark mode/wallpaper/volume
    ("printer.control", "ask"),
    // v1.5 Things data surface (docs/permissions.md v1.5): reads and writes both ask, not high-risk
    ("things.read", "ask"),
    ("things.write", "ask"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Granted,
    Denied,
    Ask,
}

pub fn known(permission: &str) -> bool {
    PERMISSIONS.iter().any(|(p, _)| *p == permission)
}

/// Capability/permission name lexicon: `^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$` —
/// 2+ segments, each starting with a lowercase letter, length ≤ 64, pure ASCII. The pure-ASCII check is equivalent to
/// "still pure ASCII after NFKC normalization": ASCII strings are unchanged by NFKC, while non-ASCII (including homoglyphs/full-width)
/// fails the lexicon as-is and is rejected. Empty segments/leading-trailing-double dots are covered by the split + empty-segment check.
pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 || !name.is_ascii() {
        return false;
    }
    let mut segments = 0usize;
    for seg in name.split('.') {
        segments += 1;
        let b = seg.as_bytes();
        if b.is_empty() || !b[0].is_ascii_lowercase() {
            return false;
        }
        if !b
            .iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        {
            return false;
        }
    }
    segments >= 2
}

/// Name's first segment (domain). The caller guarantees valid_name has passed (so there are no empty segments).
pub fn first_segment(name: &str) -> &str {
    name.split('.').next().unwrap_or("")
}

/// Reserved capability set = CAPABILITY_IDS: a reserved id may only be a provider in string form;
/// declaring a reserved id in object form is rejected outright (§4.2 reserved-domain closure).
pub fn reserved_capability(id: &str) -> bool {
    crate::core::capability::CAPABILITY_IDS.contains(&id)
}

/// Reserved domains = {all first segments of CAPABILITY_IDS ∪ PERMISSIONS} ∪ {"opencapx"} (§4.2).
/// A plugin's declared new domain must not fall within a reserved domain (review H1: prevent squatting on official domains).
pub fn reserved_domain(domain: &str) -> bool {
    domain == "opencapx"
        || crate::core::capability::CAPABILITY_IDS
            .iter()
            .any(|c| first_segment(c) == domain)
        || PERMISSIONS.iter().any(|(p, _)| first_segment(p) == domain)
}

pub fn default_decision(permission: &str) -> Decision {
    parse_decision(
        PERMISSIONS
            .iter()
            .find(|(p, _)| *p == permission)
            .map(|(_, d)| *d)
            .unwrap_or("denied"),
    )
}

pub fn parse_decision(s: &str) -> Decision {
    match s {
        "granted" => Decision::Granted,
        "ask" => Decision::Ask,
        _ => Decision::Denied,
    }
}

/// capability → permission required to execute. See the docs/capability.md v1 standard set.
pub fn capability_permission(capability: &str) -> Option<&'static str> {
    Some(match capability {
        "image.analyze" | "image.ocr" => "image.read",
        "audio.transcribe" | "file.read" => "file.read",
        // v1.2 tightening: speech output is a human-facing channel (same spam/phishing surface as notifications),
        // remapped from storage.local (granted) to notification.post (ask)
        "speech.synthesize" => "notification.post",
        "screen.capture" => "screen.capture",
        // v1.3 subscription screenshot diff: data surface equals screen.capture (every frame's content passes through),
        // same permission tier; judged once when the subscription is established, not per frame
        "screen.watch" => "screen.capture",
        "clipboard.read" => "clipboard.read",
        "clipboard.write" => "clipboard.write",
        "file.write" => "filesystem.write",
        "file.search" => "file.read",
        // subscribe type (docs/capability.md "capability typing"): reads metadata only, same tier as file.search
        "file.watch" => "file.read",
        // A7 Computer Context: app name/window title are low-sensitivity, but the reply includes clipboard fragments —
        // take the strictest component and pass the Agent-layer gate at the clipboard.read tier (ask)
        "context.get_current" => "clipboard.read",
        "browser.open" | "browser.read" => "browser.control",
        // v1.3: automation / input / PIM / audio / scheme
        "automation.run" => "automation.control",
        "input.send" => "input.control",
        "photos.read" => "photos.read",
        "contacts.search" => "contacts.read",
        "calendar.events" => "calendar.read",
        "location.get" => "location.read",
        "audio.play" => "audio.output",
        "url.scheme.open" => "url.scheme.open",
        // v1.4 high/medium-value batch
        "media.playback" => "media.control",
        "messages.recent" => "messages.read",
        "window.list" | "window.focus" => "window.management",
        "system.sleep" | "system.lock" => "power.control",
        "notes.read" => "notes.read",
        "reminders.read" => "reminders.read",
        "reminders.write" => "reminders.write",
        "mail.recent" => "mail.read",
        "system.settings" => "system.settings",
        "printer.print" => "printer.control",
        // v1.5 Things data surface: the three reads go through things.read, the three writes through things.write (both ask)
        "things.list" | "things.show" | "things.search" => "things.read",
        "things.add" | "things.update" | "things.delete" => "things.write",
        _ => return None,
    })
}
