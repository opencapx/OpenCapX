//! M3 — registry v2 client: signed-index types, verification chain, version selection, cache/replay protection, and offline fallback.
//!
//! Trust chain (app-bundled official public key → verify index signature → obtain publisher public key → verify package signature):
//! This file only handles "verify index + select version"; package signature verification lives in `plugin_sig` (falling back via `load_offline` to
//! the publisher public keys registered in this module). HTTPS is only the transport layer — mirrors, proxies, and local seed files are equal,
//! and the trust source is always the index's own signature.
//!
//! ## Frozen spec (2026-09-14, M3)
//!
//! ```text
//! index_digest      = SHA256( "opencapx-index-v1\n" ‖ canonical_index_utf8 )
//! canonical_index   = index JSON with the "indexSignature" key removed, then lexicographic keys at every level, compact, UTF-8
//! indexSignature.sig = hex( Ed25519(SK, "opencapx-v2\n" ‖ ascii(index_digest)) )
//! versions[].sha256  = SHA-256 of the .ocplugin file bytes (download integrity);
//!                      package-content trust is carried by versions[].signature (matching the signature in the manifest)
//! ```
//!
//! Replay protection: the `generatedAt` of an index fetched over HTTP must be ≥ the local max seen (`state.json`);
//! an older index is rejected. The local cache/seed has passed verification and is treated as trusted material (same-user writes are outside the threat model).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::marketplace::{
    channel_rank, parse_version_lenient, version_is_newer, PluginMarketVersion,
};
use super::plugin_sig::Signature;
use super::signing;

pub const SCHEMA_VERSION: u32 = 2;
pub const INDEX_DOMAIN: &str = "opencapx-index-v1\n";
const SIG_DOMAIN: &str = "opencapx-v2\n";
// Since v1.5 this points to the real hosting repo (opencapx/opencapx-registry, raw static files).
// The old value opencapx/registry never existed — an earlier release shipped with a dead link; this is the fix.
const DEFAULT_INDEX_URL: &str =
    "https://raw.githubusercontent.com/opencapx/opencapx-registry/main/index.json";
const DEFAULT_TTL_SECS: u64 = 24 * 60 * 60;

// ─────────────────────────── wire types ───────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryIndex {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "generatedAt")]
    pub generated_at: u64,
    #[serde(default)]
    pub publishers: Vec<Publisher>,
    #[serde(rename = "revokedKeys", default)]
    pub revoked_keys: Vec<RevokedKey>,
    #[serde(default)]
    pub entries: Vec<RegistryEntry>,
    #[serde(rename = "indexSignature")]
    pub index_signature: IndexSignature,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Publisher {
    #[serde(rename = "keyId")]
    pub key_id: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    /// Observation/review flag: only verified publishers can be selected and used for trust fallback.
    #[serde(default = "default_true")]
    pub verified: bool,
    #[serde(default)]
    pub since: String,
    #[serde(
        rename = "displayName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RevokedKey {
    #[serde(rename = "keyId")]
    pub key_id: String,
    pub at: u64,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub summary: String,
    pub author: AuthorRef,
    #[serde(default)]
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub versions: Vec<RegistryVersion>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthorRef {
    #[serde(rename = "keyId")]
    pub key_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryVersion {
    pub version: String,
    #[serde(
        rename = "minCoreVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub min_core_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(rename = "downloadUrl")]
    pub download_url: String,
    /// SHA-256 of the .ocplugin file bytes (hex, lowercase) — download integrity.
    pub sha256: String,
    /// The signature object from the package's manifest (same shape as M2's signature).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
    #[serde(
        rename = "releasedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub released_at: Option<u64>,
    #[serde(rename = "sizeBytes", default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexSignature {
    #[serde(default = "default_ed25519")]
    pub alg: String,
    #[serde(rename = "keyId")]
    pub key_id: String,
    pub sig: String,
}

fn default_ed25519() -> String {
    "ed25519".to_string()
}

// ─────────────────────────── Official keys (trust root) ───────────────────────────

/// Official keys = source constants (empty placeholder, filled in by the M8 ceremony) ∪ env-var override (dev/test).
/// WHY the env override: the official key ceremony has not been performed (OFFICIAL_ED25519_PUBKEYS is empty), so local
/// end-to-end and CI need a way to "pin a set of test official keys"; the production trust root is always the shipped constants.
/// Format: `OPENCAPX_REGISTRY_OFFICIAL_KEYS="keyId=hex,keyId2=hex2"`.
pub fn official_keys() -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = super::plugin_sig::OFFICIAL_ED25519_PUBKEYS
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if let Ok(raw) = std::env::var("OPENCAPX_REGISTRY_OFFICIAL_KEYS") {
        for pair in raw.split(',') {
            if let Some((k, v)) = pair.split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                if !k.is_empty() && !v.is_empty() {
                    out.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    out
}

// ─────────────────────────── Canonicalization / digest / verification ───────────────────────────

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err("odd length".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte =
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("hex @{}: {}", i, e))?;
        out.push(byte);
    }
    Ok(out)
}

/// canonical_index: drop `indexSignature`, lexicographic keys at every level, compact, UTF-8.
pub fn canonical_index(raw: &[u8]) -> Result<String, String> {
    let mut v: Value = serde_json::from_slice(raw)
        .map_err(|e| format!("registry index is not valid json: {}", e))?;
    signing::ensure_signable_numbers(&v)?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| "registry index must be a JSON object".to_string())?;
    obj.remove("indexSignature");
    serde_json::to_string(&v).map_err(|e| format!("canonical index serialize: {}", e))
}

pub fn index_digest(canonical_utf8: &str) -> String {
    let mut acc = Vec::with_capacity(INDEX_DOMAIN.len() + canonical_utf8.len());
    acc.extend_from_slice(INDEX_DOMAIN.as_bytes());
    acc.extend_from_slice(canonical_utf8.as_bytes());
    super::marketplace::sha256_hex(&acc)
}

/// Signing entry point for the official signer (shared by the CLI `sign-index` and tests): inject/overwrite `indexSignature`.
pub fn sign_index(unsigned_raw: &[u8], seed: &[u8; 32], key_id: &str) -> Result<String, String> {
    use ed25519_dalek::Signer;
    let mut v: Value = serde_json::from_slice(unsigned_raw)
        .map_err(|e| format!("registry index is not valid json: {}", e))?;
    signing::ensure_signable_numbers(&v)?;
    {
        let obj = v
            .as_object_mut()
            .ok_or_else(|| "registry index must be a JSON object".to_string())?;
        obj.remove("indexSignature"); // re-signing = overwrite the old signature
    }
    let canonical = serde_json::to_string(&v).map_err(|e| format!("canonical: {}", e))?;
    let digest = index_digest(&canonical);
    let sk = ed25519_dalek::SigningKey::from_bytes(seed);
    let sig = sk.sign(format!("{}{}", SIG_DOMAIN, digest).as_bytes());
    let obj = v
        .as_object_mut()
        .ok_or_else(|| "registry index must be a JSON object".to_string())?;
    obj.insert(
        "indexSignature".to_string(),
        json!({"alg": "ed25519", "keyId": key_id, "sig": hex_encode(&sig.to_bytes())}),
    );
    serde_json::to_string_pretty(&v).map_err(|e| format!("serialize: {}", e))
}

/// Verify index: parse → schemaVersion check → canonicalize → verify with official keys. Any failure means reject.
pub fn verify_index(raw: &[u8]) -> Result<RegistryIndex, String> {
    let v: Value = serde_json::from_slice(raw)
        .map_err(|e| format!("registry index is not valid json: {}", e))?;
    signing::ensure_signable_numbers(&v)?;
    let idx: RegistryIndex = serde_json::from_value(v.clone())
        .map_err(|e| format!("registry index schema invalid: {}", e))?;
    if idx.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "unsupported registry schemaVersion {} (expect {})",
            idx.schema_version, SCHEMA_VERSION
        ));
    }
    if idx.index_signature.alg != "ed25519" {
        return Err(format!(
            "unsupported indexSignature.alg {:?}",
            idx.index_signature.alg
        ));
    }
    let canonical = canonical_index(raw)?;
    let digest = index_digest(&canonical);
    let keys = official_keys();
    if keys.is_empty() {
        return Err(
            "no official registry keys pinned (M8 ceremony pending; set OPENCAPX_REGISTRY_OFFICIAL_KEYS for dev)"
                .to_string(),
        );
    }
    let Some(pk_hex) = keys.get(&idx.index_signature.key_id) else {
        return Err(format!(
            "index signed by unpinned key {}",
            idx.index_signature.key_id
        ));
    };
    let pk: [u8; 32] = hex_decode(pk_hex)
        .map_err(|e| format!("official key hex: {}", e))?
        .try_into()
        .map_err(|_| "official key must be 32 bytes".to_string())?;
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&pk)
        .map_err(|e| format!("official key invalid: {}", e))?;
    let sig_bytes: [u8; 64] = hex_decode(&idx.index_signature.sig)
        .map_err(|e| format!("indexSignature.sig hex: {}", e))?
        .try_into()
        .map_err(|_| "indexSignature.sig must be 64 bytes".to_string())?;
    let msg = format!("{}{}", SIG_DOMAIN, digest);
    vk.verify_strict(
        msg.as_bytes(),
        &ed25519_dalek::Signature::from_bytes(&sig_bytes),
    )
    .map_err(|_| "index signature invalid".to_string())?;
    Ok(idx)
}

// ─────────────────────────── Trust facade / version selection ───────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStatus {
    /// Registered, verified, not revoked.
    Registered,
    /// In revokedKeys (takes precedence over registration status).
    Revoked,
    /// No such key.
    Unknown,
}

pub fn key_status(index: &RegistryIndex, key_id: &str) -> KeyStatus {
    if index.revoked_keys.iter().any(|r| r.key_id == key_id) {
        return KeyStatus::Revoked;
    }
    match index.publishers.iter().find(|p| p.key_id == key_id) {
        Some(p) if p.verified => KeyStatus::Registered,
        _ => KeyStatus::Unknown,
    }
}

/// Publisher public key (32 bytes). Unregistered/revoked/invalid hex → None.
pub fn publisher_pubkey(index: &RegistryIndex, key_id: &str) -> Option<[u8; 32]> {
    if key_status(index, key_id) != KeyStatus::Registered {
        return None;
    }
    let hex = &index
        .publishers
        .iter()
        .find(|p| p.key_id == key_id)?
        .public_key;
    hex_decode(hex).ok()?.try_into().ok()
}

fn core_ok(core_version: &str, min: Option<&str>) -> bool {
    match min {
        None => true,
        Some(min) => match (
            parse_version_lenient(core_version),
            parse_version_lenient(min),
        ) {
            (Some(c), Some(m)) => c >= m,
            _ => true, // unparseable → allow (same criteria as plugin::check_core_compat)
        },
    }
}

/// Version selection: channel ≤ ceiling, core-compatible, **the package must be signed and the signing key must be a registered, non-revoked publisher**,
/// take the highest version. None when not found (caller shows a "no available version" message).
pub fn select_version<'a>(
    index: &'a RegistryIndex,
    plugin_id: &str,
    core_version: &str,
    channel_ceiling: &str,
) -> Option<&'a RegistryVersion> {
    let entry = index.entries.iter().find(|e| e.id == plugin_id)?;
    entry
        .versions
        .iter()
        .filter(|v| {
            let ch = v.channel.as_deref().unwrap_or("stable");
            if channel_rank(ch) > channel_rank(channel_ceiling) {
                return false;
            }
            if !core_ok(core_version, v.min_core_version.as_deref()) {
                return false;
            }
            let Some(sig) = &v.signature else {
                return false; // no signature = cannot enter the distribution trust chain
            };
            if sig.alg.as_deref() != Some("ed25519") {
                return false;
            }
            key_status(index, &sig.key_id) == KeyStatus::Registered
        })
        .fold(None, |best: Option<&'a RegistryVersion>, v| match best {
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

/// registry version → marketplace version (the app-side download reuses the `download_target` shape).
pub fn to_market_version(v: &RegistryVersion) -> PluginMarketVersion {
    PluginMarketVersion {
        version: v.version.clone(),
        min_core_version: v.min_core_version.clone(),
        channel: v.channel.clone(),
        download_url: v.download_url.clone(),
        sha256: v.sha256.clone(),
    }
}

// ─────────────────────────── Cache / seed / replay protection ───────────────────────────

pub fn registry_root() -> PathBuf {
    if let Ok(dir) = std::env::var("OPENCAPX_REGISTRY_DIR") {
        return PathBuf::from(dir);
    }
    crate::core::home_dir()
        .map(|h| h.join(".opencapx").join("registry"))
        .unwrap_or_else(|| std::env::temp_dir().join("opencapx-registry"))
}

fn cache_path() -> PathBuf {
    registry_root().join("cache.json")
}

fn seed_path() -> PathBuf {
    registry_root().join("seed.json")
}

/// Replay-protection watermark: the max `generatedAt` seen locally.
fn state_path() -> PathBuf {
    registry_root().join("state.json")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrySource {
    /// Hit a verified cache within its TTL.
    Cache,
    /// Remote fetch and verification succeeded.
    Fetch,
    /// Built-in seed (offline fallback).
    Seed,
}

impl RegistrySource {
    pub fn label(&self) -> &'static str {
        match self {
            RegistrySource::Cache => "cache",
            RegistrySource::Fetch => "fetch",
            RegistrySource::Seed => "seed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Loaded {
    pub index: RegistryIndex,
    pub source: RegistrySource,
}

fn current_url() -> Option<String> {
    match std::env::var("OPENCAPX_REGISTRY_URL") {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        Ok(_) => None, // an explicit empty string = disable network, local only
        Err(_) => Some(DEFAULT_INDEX_URL.to_string()),
    }
}

fn ttl_secs() -> u64 {
    std::env::var("OPENCAPX_REGISTRY_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TTL_SECS)
}

fn read_state() -> u64 {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("generatedAt").and_then(|x| x.as_u64()))
        .unwrap_or(0)
}

fn write_state(generated_at: u64) {
    let _ = std::fs::create_dir_all(registry_root());
    let _ = std::fs::write(
        state_path(),
        serde_json::to_string(&json!({"generatedAt": generated_at})).unwrap_or_default(),
    );
}

/// For tests/tools: write the raw bytes of a verified index into the cache.
pub fn write_cache_raw(bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(registry_root()).map_err(|e| format!("mkdir: {}", e))?;
    std::fs::write(cache_path(), bytes).map_err(|e| format!("write cache: {}", e))
}

fn read_verified(path: PathBuf) -> Option<(Vec<u8>, RegistryIndex)> {
    let bytes = std::fs::read(&path).ok()?;
    let idx = verify_index(&bytes).ok()?;
    Some((bytes, idx))
}

fn fetch_and_verify() -> Result<(Vec<u8>, RegistryIndex), String> {
    let Some(url) = current_url() else {
        return Err("no registry URL configured".into());
    };
    let text = fetch_url(&url)?;
    let bytes = text.into_bytes();
    let idx = verify_index(&bytes).map_err(|e| format!("fetched index rejected: {}", e))?;
    Ok((bytes, idx))
}

fn fetch_url(url: &str) -> Result<String, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path, e));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        // Same policy as marketplace (https-only + no downgrade redirects): no HTTP client, use system curl.
        let out = std::process::Command::new("curl")
            .args([
                "-sS",
                "-L",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--max-time",
                "20",
                url,
            ])
            .output()
            .map_err(|e| format!("curl spawn: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "curl {} failed: {}",
                url,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        return String::from_utf8(out.stdout).map_err(|e| format!("utf8: {}", e));
    }
    std::fs::read_to_string(url).map_err(|e| format!("read {}: {}", url, e))
}

/// Load strategy:
/// 1. verified cache within TTL → Cache;
/// 2. otherwise a successful fetch (verify + replay check) → write cache/watermark, Fetch;
/// 3. fetch failure → fall back to the (stale but verified) cache;
/// 4. no cache → built-in seed (verified);
/// 5. none of the above → Err.
pub fn load() -> Result<Loaded, String> {
    if let Some((_bytes, index)) = read_verified(cache_path()) {
        let stale = std::fs::metadata(cache_path())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|age| age.as_secs() > ttl_secs())
            .unwrap_or(false);
        if !stale {
            write_state(read_state().max(index.generated_at));
            return Ok(Loaded {
                index,
                source: RegistrySource::Cache,
            });
        }
        match fetch_and_verify() {
            Ok((raw, fetched)) => {
                let seen = read_state();
                if fetched.generated_at < seen {
                    // A fetched index older than one already seen = replay attack surface; reject and fall back to the stale cache.
                    return Ok(Loaded {
                        index,
                        source: RegistrySource::Cache,
                    });
                }
                let _ = write_cache_raw(&raw);
                write_state(seen.max(fetched.generated_at));
                return Ok(Loaded {
                    index: fetched,
                    source: RegistrySource::Fetch,
                });
            }
            Err(_) => {
                return Ok(Loaded {
                    index,
                    source: RegistrySource::Cache,
                })
            }
        }
    }
    // No-cache path: fetch first, then seed on failure.
    match fetch_and_verify() {
        Ok((raw, fetched)) => {
            let seen = read_state();
            if fetched.generated_at < seen {
                return Err(format!(
                    "registry index replayed: generatedAt {} < last seen {}",
                    fetched.generated_at, seen
                ));
            }
            let _ = write_cache_raw(&raw);
            write_state(seen.max(fetched.generated_at));
            Ok(Loaded {
                index: fetched,
                source: RegistrySource::Fetch,
            })
        }
        Err(fetch_err) => match read_verified(seed_path()) {
            Some((_, index)) => Ok(Loaded {
                index,
                source: RegistrySource::Seed,
            }),
            None => Err(format!(
                "no verified registry index available ({})",
                fetch_err
            )),
        },
    }
}

/// Force refresh (ignore TTL): fetch → verify → replay check → write cache/watermark. Any failure is returned as an error.
pub fn refresh() -> Result<Loaded, String> {
    let (raw, index) = fetch_and_verify()?;
    let seen = read_state();
    if index.generated_at < seen {
        return Err(format!(
            "registry index replayed: generatedAt {} < last seen {}",
            index.generated_at, seen
        ));
    }
    write_cache_raw(&raw)?;
    write_state(seen.max(index.generated_at));
    Ok(Loaded {
        index,
        source: RegistrySource::Fetch,
    })
}

/// Offline read (used by the verification path, **never touches the network**): cache → seed.
pub fn load_offline() -> Option<RegistryIndex> {
    read_verified(cache_path())
        .map(|(_, idx)| idx)
        .or_else(|| read_verified(seed_path()).map(|(_, idx)| idx))
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistryStatusDto {
    pub source: String,
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "generatedAt")]
    pub generated_at: u64,
    pub publishers: usize,
    pub entries: usize,
    #[serde(rename = "revokedKeys")]
    pub revoked_keys: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn dto_from(source: &str, index: &RegistryIndex) -> RegistryStatusDto {
    RegistryStatusDto {
        source: source.to_string(),
        schema_version: index.schema_version,
        generated_at: index.generated_at,
        publishers: index.publishers.len(),
        entries: index.entries.len(),
        revoked_keys: index.revoked_keys.len(),
        url: current_url(),
        error: None,
    }
}

/// Build the status DTO from a loaded result (reused by the refresh command).
pub fn status_of(loaded: &Loaded) -> RegistryStatusDto {
    dto_from(loaded.source.label(), &loaded.index)
}

/// Status query: if it can load, report source and size; if it cannot, still return a DTO (source="none") with an error,
/// so the settings page can show "registry unavailable" instead of the command failing.
pub fn status() -> RegistryStatusDto {
    match load() {
        Ok(loaded) => status_of(&loaded),
        Err(e) => RegistryStatusDto {
            source: "none".to_string(),
            schema_version: 0,
            generated_at: 0,
            publishers: 0,
            entries: 0,
            revoked_keys: 0,
            url: current_url(),
            error: Some(e),
        },
    }
}

// ─────────────────────────── tests ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Serialize env changes (REGISTRY_* are process-global); also hold TEST_STORE_LOCK,
    /// so it is mutually exclusive with other modules' global-env tests (plugin_sig integration cases).
    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    fn guard() -> (
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let store = crate::core::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        (store, env)
    }

    const TEST_SEED: [u8; 32] = [7u8; 32];

    fn test_pubkey_hex() -> String {
        use ed25519_dalek::SigningKey;
        let sk = SigningKey::from_bytes(&TEST_SEED);
        hex_encode(sk.verifying_key().as_bytes())
    }

    fn set_official(keys: &str) {
        std::env::set_var("OPENCAPX_REGISTRY_OFFICIAL_KEYS", keys);
    }

    fn unsigned_index(generated_at: u64) -> String {
        let pubkey = test_pubkey_hex();
        format!(
            r#"{{
  "schemaVersion": 2,
  "generatedAt": {generated_at},
  "publishers": [
    {{"keyId": "com.opencapx.test-pub", "publicKey": "{pubkey}", "verified": true, "since": "2026-09-14"}}
  ],
  "revokedKeys": [],
  "entries": [
    {{
      "id": "com.example.weather-demo",
      "name": "Weather Demo",
      "summary": "offline demo",
      "author": {{"keyId": "com.opencapx.test-pub"}},
      "repo": "opencapx/weather-demo",
      "versions": [
        {{
          "version": "0.1.0",
          "minCoreVersion": "0.4.0",
          "channel": "stable",
          "downloadUrl": "https://example.invalid/w.ocplugin",
          "sha256": "aa",
          "signature": {{"alg": "ed25519", "keyId": "com.opencapx.test-pub", "sig": "bb"}}
        }}
      ]
    }}
  ],
  "indexSignature": {{"alg": "ed25519", "keyId": "com.opencapx.test-official", "sig": "00"}}
}}"#
        )
    }

    fn signed_index(generated_at: u64) -> String {
        sign_index(
            unsigned_index(generated_at).as_bytes(),
            &TEST_SEED,
            "com.opencapx.test-official",
        )
        .unwrap()
    }

    fn tmp_registry_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opencapx-reg-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn setup_env(dir: &PathBuf) {
        std::env::set_var("OPENCAPX_REGISTRY_DIR", dir);
        set_official(&format!("com.opencapx.test-official={}", test_pubkey_hex()));
        std::env::remove_var("OPENCAPX_REGISTRY_TTL_SECS");
    }

    fn teardown_env() {
        std::env::remove_var("OPENCAPX_REGISTRY_DIR");
        std::env::remove_var("OPENCAPX_REGISTRY_OFFICIAL_KEYS");
        std::env::remove_var("OPENCAPX_REGISTRY_URL");
        std::env::remove_var("OPENCAPX_REGISTRY_TTL_SECS");
    }

    #[test]
    fn sign_then_verify_round_trip() {
        let _g = guard();
        set_official(&format!("com.opencapx.test-official={}", test_pubkey_hex()));
        let signed = signed_index(100);
        let idx = verify_index(signed.as_bytes()).expect("verify ok");
        assert_eq!(idx.schema_version, 2);
        assert_eq!(idx.generated_at, 100);
        assert_eq!(idx.publishers.len(), 1);
        assert_eq!(idx.entries[0].id, "com.example.weather-demo");
        teardown_env();
    }

    /// key-ceremony §6 "add first, revoke later" rotation mechanism: while two official keys are both pinned,
    /// an index signed by either keyId is valid; a third unpinned key is still rejected. The behavioral contract during rotation.
    #[test]
    fn verify_accepts_either_official_key_during_rotation_window() {
        let _g = guard();
        let old_seed = TEST_SEED;
        let new_seed = [9u8; 32];
        let old_pk = {
            let sk = ed25519_dalek::SigningKey::from_bytes(&old_seed);
            hex_encode(sk.verifying_key().as_bytes())
        };
        let new_pk = {
            let sk = ed25519_dalek::SigningKey::from_bytes(&new_seed);
            hex_encode(sk.verifying_key().as_bytes())
        };
        // Add first: old and new keys coexist
        set_official(&format!(
            "com.opencapx.old={},com.opencapx.new={}",
            old_pk, new_pk
        ));
        let unsigned = unsigned_index(200);
        // Signed by old key → valid
        let by_old = sign_index(unsigned.as_bytes(), &old_seed, "com.opencapx.old").unwrap();
        assert!(
            verify_index(by_old.as_bytes()).is_ok(),
            "old key must verify during window"
        );
        // Signed by new key → valid (the index shape of the later rotation phase)
        let by_new = sign_index(unsigned.as_bytes(), &new_seed, "com.opencapx.new").unwrap();
        assert!(
            verify_index(by_new.as_bytes()).is_ok(),
            "new key must verify during window"
        );
        // A third unpinned key → reject
        let by_stranger =
            sign_index(unsigned.as_bytes(), &[11u8; 32], "com.opencapx.stranger").unwrap();
        let err = verify_index(by_stranger.as_bytes()).unwrap_err();
        assert!(err.contains("unpinned"), "{err}");
        teardown_env();
    }

    #[test]
    fn verify_rejects_tampered_index() {
        let _g = guard();
        set_official(&format!("com.opencapx.test-official={}", test_pubkey_hex()));
        let signed = signed_index(100);
        // Tamper with one field (without re-signing) → reject
        let tampered = signed.replace("offline demo", "tampered summary");
        assert!(verify_index(tampered.as_bytes()).is_err());
        teardown_env();
    }

    #[test]
    fn verify_rejects_unpinned_official_key() {
        let _g = guard();
        set_official(&format!("com.other-official={}", test_pubkey_hex()));
        let signed = signed_index(100);
        let err = verify_index(signed.as_bytes()).unwrap_err();
        assert!(err.contains("unpinned"), "{err}");
        teardown_env();
    }

    #[test]
    fn verify_rejects_unsupported_schema_version() {
        let _g = guard();
        set_official(&format!("com.opencapx.test-official={}", test_pubkey_hex()));
        let unsigned = unsigned_index(100).replace("\"schemaVersion\": 2", "\"schemaVersion\": 3");
        let signed = sign_index(
            unsigned.as_bytes(),
            &TEST_SEED,
            "com.opencapx.test-official",
        )
        .unwrap();
        let err = verify_index(signed.as_bytes()).unwrap_err();
        assert!(err.contains("schemaVersion"), "{err}");
        teardown_env();
    }

    #[test]
    fn select_version_prefers_highest_registered_and_compatible() {
        let _g = guard();
        // Test version selection directly with an in-memory index varying signature/channel/core/revocation (no IO).
        let pubkey = test_pubkey_hex();
        let raw = format!(
            r#"{{
  "schemaVersion": 2, "generatedAt": 1,
  "publishers": [
    {{"keyId": "com.a", "publicKey": "{pubkey}", "verified": true}},
    {{"keyId": "com.b", "publicKey": "{pubkey}", "verified": true}},
    {{"keyId": "com.unverified", "publicKey": "{pubkey}", "verified": false}}
  ],
  "revokedKeys": [{{"keyId": "com.revoked", "at": 9, "reason": "test"}}],
  "entries": [{{
    "id": "com.x.plug", "name": "P", "author": {{"keyId": "com.a"}}, "repo": "x/y",
    "versions": [
      {{"version": "2.0.0", "channel": "beta",  "downloadUrl": "u", "sha256": "s", "signature": {{"alg":"ed25519","keyId":"com.a","sig":"z"}}}},
      {{"version": "1.9.0", "channel": "beta",  "downloadUrl": "u", "sha256": "s", "signature": {{"alg":"ed25519","keyId":"com.revoked","sig":"z"}}}},
      {{"version": "1.8.0", "channel": "stable","downloadUrl": "u", "sha256": "s", "signature": {{"alg":"ed25519","keyId":"com.b","sig":"z"}}}},
      {{"version": "1.7.0", "channel": "stable","downloadUrl": "u", "sha256": "s"}},
      {{"version": "9.9.9", "channel": "stable","downloadUrl": "u", "sha256": "s", "signature": {{"alg":"ed25519","keyId":"com.unverified","sig":"z"}}}},
      {{"version": "5.0.0", "channel": "stable","downloadUrl": "u", "sha256": "s", "signature": {{"alg":"ed25519","keyId":"com.a","sig":"z"}}}}
    ]
  }}],
  "indexSignature": {{"alg":"ed25519","keyId":"k","sig":"0"}}
}}"#
        );
        let idx: RegistryIndex = serde_json::from_str(&raw).unwrap();
        // stable ceiling: excludes beta (2.0.0/1.9.0), the unsigned 1.7.0, and 9.9.9 from an unverified publisher;
        // highest available = 5.0.0 (com.a, registered).
        let v = select_version(&idx, "com.x.plug", "0.5.0", "stable").unwrap();
        assert_eq!(v.version, "5.0.0");
        // Under a beta ceiling 2.0.0 is available, but 5.0.0 is higher → still take 5.0.0.
        let v = select_version(&idx, "com.x.plug", "0.5.0", "beta").unwrap();
        assert_eq!(v.version, "5.0.0");
        // core 0.1.0 + minCoreVersion? This case sets no min → no effect; revocation predicate unit check:
        assert_eq!(key_status(&idx, "com.revoked"), KeyStatus::Revoked);
        assert_eq!(key_status(&idx, "com.unverified"), KeyStatus::Unknown);
        assert_eq!(key_status(&idx, "com.a"), KeyStatus::Registered);
        // Unknown plugin → None
        assert!(select_version(&idx, "com.nope", "0.5.0", "stable").is_none());
        teardown_env();
    }

    #[test]
    fn load_prefers_fresh_cache() {
        let _g = guard();
        let dir = tmp_registry_dir("fresh");
        setup_env(&dir);
        write_cache_raw(signed_index(200).as_bytes()).unwrap();
        let loaded = load().expect("load");
        assert_eq!(loaded.source, RegistrySource::Cache);
        assert_eq!(loaded.index.generated_at, 200);
        teardown_env();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_falls_back_to_seed_offline() {
        let _g = guard();
        let dir = tmp_registry_dir("seed");
        setup_env(&dir);
        std::fs::write(seed_path(), signed_index(50)).unwrap();
        std::env::set_var("OPENCAPX_REGISTRY_URL", "file:///nonexistent-index.json");
        let loaded = load().expect("seed fallback");
        assert_eq!(loaded.source, RegistrySource::Seed);
        assert_eq!(loaded.index.generated_at, 50);
        teardown_env();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_replayed_fetch() {
        let _g = guard();
        let dir = tmp_registry_dir("replay");
        setup_env(&dir);
        // watermark = 200; URL serves 100 (signed) → reject
        write_state(200);
        let old = dir.join("old-index.json");
        std::fs::write(&old, signed_index(100)).unwrap();
        std::env::set_var("OPENCAPX_REGISTRY_URL", format!("file://{}", old.display()));
        let err = load().unwrap_err();
        assert!(err.contains("replayed"), "{err}");
        // refresh rejects the same way
        let err = refresh().unwrap_err();
        assert!(err.contains("replayed"), "{err}");
        teardown_env();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_accepts_newer_then_rejects_older() {
        let _g = guard();
        let dir = tmp_registry_dir("refresh");
        setup_env(&dir);
        let newer = dir.join("new.json");
        std::fs::write(&newer, signed_index(300)).unwrap();
        std::env::set_var(
            "OPENCAPX_REGISTRY_URL",
            format!("file://{}", newer.display()),
        );
        let loaded = refresh().expect("refresh ok");
        assert_eq!(loaded.source, RegistrySource::Fetch);
        assert_eq!(read_state(), 300);
        // Same URL switched to an older one (150) → reject, and the watermark does not drop
        std::fs::write(&newer, signed_index(150)).unwrap();
        let err = refresh().unwrap_err();
        assert!(err.contains("replayed"), "{err}");
        assert_eq!(read_state(), 300);
        teardown_env();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_uses_stale_cache_when_fetch_fails() {
        let _g = guard();
        let dir = tmp_registry_dir("stale");
        setup_env(&dir);
        write_cache_raw(signed_index(210).as_bytes()).unwrap();
        std::env::set_var("OPENCAPX_REGISTRY_TTL_SECS", "0"); // any cache counts as stale
        std::env::set_var("OPENCAPX_REGISTRY_URL", "file:///nonexistent-index.json");
        let loaded = load().expect("stale cache fallback");
        assert_eq!(loaded.source, RegistrySource::Cache);
        assert_eq!(loaded.index.generated_at, 210);
        teardown_env();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_offline_reads_cache_and_ignores_garbage() {
        let _g = guard();
        let dir = tmp_registry_dir("offline");
        setup_env(&dir);
        // Garbage cache (unsigned html) → None
        std::fs::write(cache_path(), "<html>not an index</html>").unwrap();
        assert!(load_offline().is_none());
        // Real cache → Some
        write_cache_raw(signed_index(220).as_bytes()).unwrap();
        let idx = load_offline().expect("offline cache");
        assert_eq!(idx.generated_at, 220);
        teardown_env();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn to_market_version_carries_download_fields() {
        let v = RegistryVersion {
            version: "1.2.3".into(),
            min_core_version: Some("0.5.0".into()),
            channel: Some("beta".into()),
            download_url: "https://x/y.ocplugin".into(),
            sha256: "ab".repeat(32),
            signature: None,
            released_at: None,
            size_bytes: None,
        };
        let mv = to_market_version(&v);
        assert_eq!(mv.version, "1.2.3");
        assert_eq!(mv.download_url, "https://x/y.ocplugin");
        assert_eq!(mv.sha256, "ab".repeat(32));
    }

    // ───────── Golden fixtures (paired with fixtures/registry) ─────────

    fn fixtures_registry_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("fixtures")
            .join("registry")
    }

    fn fixture_official_env() -> String {
        let pub_hex =
            std::fs::read_to_string(fixtures_registry_dir().join("official.pub.hex")).unwrap();
        format!("com.opencapx.test-official={}", pub_hex.trim())
    }

    /// Golden index: verification passes + structural assertions + selection hits weather-demo + sha matches the package file.
    #[test]
    fn golden_index_verifies_and_selects() {
        let _g = guard();
        set_official(&fixture_official_env());
        let raw = std::fs::read(fixtures_registry_dir().join("index.signed.json")).unwrap();
        let idx = verify_index(&raw).expect("golden index must verify");
        assert_eq!(idx.generated_at, 1757846400);
        assert_eq!(idx.publishers.len(), 1);
        assert_eq!(idx.entries.len(), 1);
        let v = select_version(&idx, "com.example.weather-demo", "0.5.0", "stable")
            .expect("selectable");
        assert_eq!(v.version, "0.1.0");
        let file_sha =
            std::fs::read_to_string(fixtures_registry_dir().join("package.sha256.hex")).unwrap();
        assert_eq!(v.sha256, file_sha.trim());
        teardown_env();
    }

    /// A tampered fixture (signature untouched, one field changed) must be rejected.
    #[test]
    fn golden_index_rejects_tampered_fixture() {
        let _g = guard();
        set_official(&fixture_official_env());
        let raw = std::fs::read(fixtures_registry_dir().join("index.tampered.json")).unwrap();
        assert!(verify_index(&raw).is_err());
        teardown_env();
    }

    /// Cross-fixture trust chain: registry publisher public key → directly verify the M2 golden package's signature (mathematical closure).
    #[test]
    fn golden_chain_publisher_key_verifies_signing_archive() {
        let _g = guard();
        set_official(&fixture_official_env());
        let raw = std::fs::read(fixtures_registry_dir().join("index.signed.json")).unwrap();
        let idx = verify_index(&raw).unwrap();
        let pak = publisher_pubkey(&idx, "com.opencapx.test-signing").expect("publisher key");
        let archive = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("fixtures")
            .join("signing")
            .join("golden")
            .join("signed.ocplugin");
        let digest = crate::core::signing::digest_v2(&archive).unwrap();
        let sig_hex = idx.entries[0].versions[0]
            .signature
            .as_ref()
            .expect("fixture version signature")
            .sig
            .clone();
        let sig_bytes: [u8; 64] = hex_decode(&sig_hex).unwrap().try_into().unwrap();
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&pak).unwrap();
        vk.verify_strict(
            format!("opencapx-v2\n{}", digest).as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&sig_bytes),
        )
        .expect("registry publisher must verify the package signature");
        teardown_env();
    }

    /// Replay protection: watermark above the old signed fixture index → reject.
    #[test]
    fn golden_replay_fixture_rejected_above_watermark() {
        let _g = guard();
        let dir = tmp_registry_dir("golden-replay");
        setup_env(&dir);
        set_official(&fixture_official_env());
        write_state(1757846400);
        let old = fixtures_registry_dir().join("index.old.signed.json");
        std::env::set_var("OPENCAPX_REGISTRY_URL", format!("file://{}", old.display()));
        let err = load().unwrap_err();
        assert!(err.contains("replayed"), "{err}");
        teardown_env();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
