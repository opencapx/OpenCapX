//! version logic: lenient parsing, update checks, channel-aware version selection.
//! Mechanical move from core/marketplace.rs.

use super::*;

pub fn find(id: &str) -> Option<PluginMarketEntry> {
    load_index().entries.into_iter().find(|e| e.id == id)
}

/// Compare installed (id, version) pairs against the marketplace index, returning the list with updates.
/// Version comparison follows `parse_version_lenient`'s semver precedence: compare major/minor/patch,
/// prerelease segments (`-rc.1` etc.) participate in precedence; build metadata (`+xxx`) does not.
/// When the current or candidate version cannot be parsed, treat it as "no update" (no error).
///
/// Phase 38 — the second parameter `channels` is each plugin's currently subscribed min_channel
/// (either a HashMap or an (id, channel) slice; a miss defaults to 'stable').
/// An upgrade is offered only when entry.channel_rank ≤ min_channel_rank.
pub fn check_updates(
    installed: &[(String, String)],
    channels: &[(String, String)],
) -> Vec<PluginUpdateInfo> {
    let idx = load_index();
    let mut out = Vec::new();
    for (id, current) in installed {
        let Some(entry) = idx.entries.iter().find(|e| &e.id == id) else {
            continue;
        };
        let min_channel = channels
            .iter()
            .find(|(pid, _)| pid == id)
            .map(|(_, c)| c.as_str())
            .unwrap_or("stable");
        if channel_rank(&entry.channel) > channel_rank(min_channel) {
            continue;
        }
        if let Some(selected) = select_market_version(entry, env!("CARGO_PKG_VERSION"), min_channel)
        {
            if version_is_newer(&selected.version, current) {
                out.push(PluginUpdateInfo {
                    id: id.clone(),
                    current_version: current.clone(),
                    latest_version: selected.version,
                    download_url: selected.download_url,
                    sha256: selected.sha256,
                    channel: selected.channel.unwrap_or_else(|| entry.channel.clone()),
                });
            }
        }
    }
    out
}

/// Backward-compat: degrades to the old signature when channels is absent (all plugins default to stable).
#[cfg(test)]
pub fn check_updates_default(installed: &[(String, String)]) -> Vec<PluginUpdateInfo> {
    check_updates(installed, &[])
}

/// Lenient parsing: strict semver first; on failure, pad the numeric core ("1.2"→"1.2.0", "1"→"1.0.0"),
/// allowing a `v` prefix and prerelease (`-rc.1`); build metadata (`+xxx`) does not participate in comparison.
/// Parse failure → None (callers treat it as "undecidable" and do not upgrade).
pub fn parse_version_lenient(raw: &str) -> Option<semver::Version> {
    let s = raw.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    if let Ok(v) = semver::Version::parse(s) {
        return Some(v);
    }
    let core = s.split(['-', '+']).next().unwrap_or("");
    let pre = s
        .split_once('-')
        .map(|(_, rest)| rest.split('+').next().unwrap_or(""));
    let mut nums: Vec<u64> = Vec::new();
    for seg in core.split('.') {
        nums.push(seg.parse::<u64>().ok()?);
    }
    if nums.is_empty() || nums.len() > 3 {
        return None;
    }
    while nums.len() < 3 {
        nums.push(0);
    }
    let mut v = semver::Version::new(nums[0], nums[1], nums[2]);
    if let Some(p) = pre {
        v.pre = semver::Prerelease::new(p).ok()?;
    }
    Some(v)
}

/// `a > b`? (with prerelease semantics; "1.0.0" > "1.0.0-rc.1").
/// If either side cannot be parsed → false (consistent with old behavior: undecidable means no upgrade).
pub fn version_is_newer(a: &str, b: &str) -> bool {
    match (parse_version_lenient(a), parse_version_lenient(b)) {
        // Note: `semver::Version`'s `Ord` includes build metadata (total order, not semver precedence),
        // so `cmp_precedence` is needed to match the "build does not participate" semantics.
        (Some(x), Some(y)) => x.cmp_precedence(&y) == std::cmp::Ordering::Greater,
        _ => false,
    }
}

/// F4 update-supply gate: select the highest version within the entry that is "channel-allowed and core-compatible".
/// When `versions[]` is empty, fall back to the legacy single-version fields (using the whole entry's channel).
pub fn select_market_version(
    entry: &PluginMarketEntry,
    core_version: &str,
    min_channel: &str,
) -> Option<PluginMarketVersion> {
    fn core_ok(core: &str, min: Option<&str>) -> bool {
        match min {
            None => true,
            Some(min) => match (parse_version_lenient(core), parse_version_lenient(min)) {
                (Some(c), Some(m)) => c >= m,
                _ => true, // unparseable → allow (same criteria as plugin::check_core_compat)
            },
        }
    }
    let candidates: Vec<PluginMarketVersion> = if entry.versions.is_empty() {
        vec![PluginMarketVersion {
            version: entry.version.clone(),
            min_core_version: entry.min_core_version.clone(),
            channel: Some(entry.channel.clone()),
            download_url: entry.download_url.clone(),
            sha256: entry.sha256.clone(),
        }]
    } else {
        entry.versions.clone()
    };
    candidates
        .into_iter()
        .filter(|v| {
            let ch = v.channel.as_deref().unwrap_or(&entry.channel);
            channel_rank(ch) <= channel_rank(min_channel)
                && core_ok(core_version, v.min_core_version.as_deref())
        })
        .fold(None, |best: Option<PluginMarketVersion>, v| match best {
            None => Some(v),
            Some(b) => {
                if version_is_newer(&v.version, &b.version) {
                    Some(v)
                } else {
                    Some(b)
                }
            }
        })
}

/// M1 fix (N1/N2): app-side target resolution, sharing the same implementation as check_updates' selector.
/// `ceiling` is set by the caller: for updates = the plugin's subscribed channel (default `stable`, same criteria as check_updates /
/// the settings-page dropdown); for explicit install = `"dev"` (no channel-subscription gate — the user clicked on the specific entry,
/// aligned with the pre-M1 install behavior).
pub fn resolve_target_with_ceiling(
    entry: &PluginMarketEntry,
    core_version: &str,
    ceiling: &str,
) -> Result<PluginMarketVersion, String> {
    select_market_version(entry, core_version, ceiling)
        .ok_or_else(|| format!("no core-compatible version available for {}", entry.id))
}
