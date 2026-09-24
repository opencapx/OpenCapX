//! Wow 6 — plugin signing/verification.
//!
//! Design goals (aligned with i1.md § install flow "verify signature/integrity"):
//! 1. **Integrity**: for every entry in the zip except the manifest, sorted by name,
//!    compute the SHA-256 of the concatenated `<name>\n<size>\n<bytes>` as the archive hash.
//!    The manifest carries a `sha256` field declaring this hash; a comparison failure = the package was modified.
//! 2. **Signature**: manifest `signature.keyId` identifies the signer,
//!    `signature.sig` = HMAC-SHA256(key, "opencapx-v1\n" + archive_hash),
//!    the key comes from `{keyId: hex_secret}` in `~/.opencapx/trusted-keys.json`.
//! 3. **Policy**: if the trusted-keys file is missing or empty → signature verification is treated as disabled,
//!    a manifest without a signature field can still be installed (back-compat); once enabled, a missing signature or
//!    a bad signature is rejected.
//!
//! SHA-256 / HMAC go through the `sha2` / `hmac` crates (unified export point in marketplace.rs),
//! satisfying the v1 security bar while keeping dependencies minimal.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use super::marketplace::{hmac_sha256_hex, sha256_hex};
use super::signing;

/// Shape of the signature field in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature {
    #[serde(rename = "keyId")]
    pub key_id: String,
    pub sig: String,
    /// Signature algorithm channel: absent/`hmac-v1` = v1 local HMAC; `ed25519` = v2 distribution channel.
    /// WHY: old manifests lack this field, and `default` guarantees backward-compatible parsing.
    #[serde(rename = "alg", default, skip_serializing_if = "Option::is_none")]
    pub alg: Option<String>,
}

/// The two shapes of a trust entry in trusted-keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustedKey {
    /// v1 local channel: HMAC secret (raw bytes after hex decoding).
    Hmac(Vec<u8>),
    /// v2 distribution channel: Ed25519 public key (32 bytes).
    Ed25519([u8; 32]),
}

/// Verification outcome, for install_ocplugin / preview_ocplugin to decide on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The manifest is unsigned and trusted-keys is empty → allowed (backward compatible).
    Unsigned,
    /// Signature verification passed; records the signing keyId.
    Trusted { key_id: String },
    /// The manifest claims to be signed but trusted-keys has no such keyId.
    UnknownKey { key_id: String },
    /// The manifest's sha256 field does not match the actual archive hash → the package was modified.
    HashMismatch {
        declared: String,
        actual: String,
    },
    /// signature.sig does not match the recomputed HMAC → forged signature.
    BadSignature { key_id: String },
    /// The manifest declares it is signed but the signature field cannot be parsed.
    MalformedSignature,
}

/// F7 — three-state install allowance (D4 implementation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowance {
    /// Trusted, install directly.
    Direct,
    /// Unverified source (unsigned / unknown key): yellow badge + allowed after explicit confirmation (with the master switch OFF the caller upgrades to hard deny).
    SoftWarn,
    /// Integrity failure (tampered / bad-signature / malformed): cannot be bypassed under any circumstance.
    HardDeny,
}

impl VerifyOutcome {
    /// F7 three-state decision: direct install / warn-and-confirm / hard deny.
    /// The "allow unsigned packages" master switch and confirmUnsigned handling live at the install entry point; this layer only
    /// classifies by fact — the tampered family never enters the confirmable tier.
    pub fn allowance(&self) -> Allowance {
        match self {
            VerifyOutcome::Trusted { .. } => Allowance::Direct,
            VerifyOutcome::Unsigned | VerifyOutcome::UnknownKey { .. } => Allowance::SoftWarn,
            _ => Allowance::HardDeny,
        }
    }

    /// Short label, used by settings.ts to render the badge.
    pub fn label(&self) -> &'static str {
        match self {
            VerifyOutcome::Unsigned => "unsigned",
            VerifyOutcome::Trusted { .. } => "trusted",
            VerifyOutcome::UnknownKey { .. } => "unknown-key",
            VerifyOutcome::HashMismatch { .. } => "tampered",
            VerifyOutcome::BadSignature { .. } => "bad-signature",
            VerifyOutcome::MalformedSignature => "malformed-signature",
        }
    }
}

/// M3 fills in the official public key in the signature index (rotating on app release is the only carrier for trust-root updates).
/// CI-PROD (2026-09-19, decision C): the official key effective from v0.1.0, generated on this machine and seeded into
/// the GitHub secret `OPENCAPX_REGISTRY_SIGNING_SEED` for CI to sign the index; the private key never enters the repo
/// (.gitignore already locks out `*.key.hex`). Planned to rotate before the ecosystem scales, per key-ceremony §6
/// "add first, revoke later" to an air-gapped ceremony key (the owner has prepared that key); the rotation carrier is an app release.
#[allow(dead_code)]
pub const OFFICIAL_ED25519_PUBKEYS: &[(&str, &str)] = &[(
    "com.opencapx",
    "538ee9ec42bde11687fb6a471b02aebb90ccf5643523d16f3c790ccba088ebd9",
)];

/// Trusted-keys file location: prefers the OPENCAPX_TRUSTED_KEYS env var (for tests),
/// otherwise `~/.opencapx/trusted-keys.json`.
pub fn trusted_keys_path() -> PathBuf {
    if let Ok(p) = std::env::var("OPENCAPX_TRUSTED_KEYS") {
        return PathBuf::from(p);
    }
    crate::core::home_dir()
        .map(|h| h.join(".opencapx").join("trusted-keys.json"))
        .unwrap_or_else(|| std::env::temp_dir().join("opencapx-trusted-keys.json"))
}

/// Read trusted-keys; a missing file returns an empty map (equivalent to "signature verification disabled").
/// Two shapes: a string value = legacy HMAC secret (hex decoded); an object
/// `{alg:"ed25519", publicKey:"<64 hex>"}` = Ed25519 public key.
/// Any invalid field (bad hex / wrong alg / wrong length / missing field) → skip that row, no panic.
pub fn load_trusted_keys() -> BTreeMap<String, TrustedKey> {
    load_trusted_keys_from(&trusted_keys_path())
}

/// Read trusted-keys from the given path (for M6 gate `--keys` injection; same format as the default file).
pub fn load_trusted_keys_from(path: &std::path::Path) -> BTreeMap<String, TrustedKey> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return BTreeMap::new(),
    };
    let Some(map) = parsed.as_object() else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for (k, v) in map {
        if let Some(hex_str) = v.as_str() {
            if let Ok(bytes) = hex_decode(hex_str) {
                out.insert(k.clone(), TrustedKey::Hmac(bytes));
            }
            continue;
        }
        let Some(obj) = v.as_object() else { continue };
        if obj.get("alg").and_then(|a| a.as_str()) != Some("ed25519") {
            continue;
        }
        let Some(pk_hex) = obj.get("publicKey").and_then(|p| p.as_str()) else {
            continue;
        };
        if let Ok(bytes) = hex_decode(pk_hex) {
            if let Ok(pk) = <[u8; 32]>::try_from(bytes.as_slice()) {
                out.insert(k.clone(), TrustedKey::Ed25519(pk));
            }
        }
    }
    out
}

/// Simple hex decoding (we trust trusted-keys.json since we generate it ourselves).
fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("odd length".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16)
            .map_err(|e| format!("hex @{}: {}", i, e))?;
        out.push(byte);
    }
    Ok(out)
}

/// Collect content entries from an opened zip: skip directory entries, skip the manifest itself,
/// returning `(name, size, bytes)` sorted by name byte order.
///
/// WHY: v1's archive hash and v2's digest must use exactly the same collection rules for "which entries participate and in what order",
/// otherwise the two signing channels silently drift; extracted into a single implementation.
pub fn collect_zip_entries<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<Vec<(String, u64, Vec<u8>)>, String> {
    let mut entries: Vec<(String, u64, Vec<u8>)> = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("zip read error: {}", e))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if name == "opencapx-plugin.json" || name.ends_with("/opencapx-plugin.json") {
            continue;
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut buf)
            .map_err(|e| format!("read {}: {}", name, e))?;
        entries.push((name, entry.size(), buf));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

/// Open the archive and collect content entries (see [`collect_zip_entries`]).
pub fn collect_archive_entries(archive: &Path) -> Result<Vec<(String, u64, Vec<u8>)>, String> {
    let file = std::fs::File::open(archive)
        .map_err(|e| format!("cannot open {}: {}", archive.display(), e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("bad zip: {}", e))?;
    collect_zip_entries(&mut zip)
}

/// Compute the archive hash: walk all non-manifest entries in the zip, sort by name, then
/// concatenate `<name>\n<size>\n<bytes>` and SHA-256.
/// Skip directories + skip the manifest itself (otherwise self-referential).
pub fn compute_archive_hash(archive: &Path) -> Result<String, String> {
    let entries = collect_archive_entries(archive)?;
    let mut acc: Vec<u8> = Vec::new();
    for (name, size, bytes) in &entries {
        acc.extend_from_slice(name.as_bytes());
        acc.push(b'\n');
        acc.extend_from_slice(size.to_string().as_bytes());
        acc.push(b'\n');
        acc.extend_from_slice(bytes);
    }
    Ok(sha256_hex(&acc))
}

/// Parse the `sha256` + `signature` fields in the manifest text.
/// On error, does not panic; returns (None, None).
fn parse_sig_fields(manifest_text: &str) -> (Option<String>, Option<Signature>) {
    let v: serde_json::Value = match serde_json::from_str(manifest_text) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let sha256 = v.get("sha256").and_then(|x| x.as_str()).map(String::from);
    let sig = v
        .get("signature")
        .and_then(|x| serde_json::from_value::<Signature>(x.clone()).ok());
    (sha256, sig)
}

/// Trust source (M3 three-source tag): local trusted-keys / registry publisher / none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustSource {
    Local,
    Registry,
    None,
}

/// Main entry point (signature-compatible): returns only the outcome; for the version with trust source see `verify_with_source`.
pub fn verify(archive: &Path) -> VerifyOutcome {
    verify_with_source(archive).0
}

/// Main entry point: open the archive, parse the manifest signature fields, compute the digest, decide the outcome and trust source.
/// ed25519 channel: local trusted-keys wins; when the local set lacks that keyId, fall back to the **offline-readable
/// signed registry** (cache/seed) publisher public key — never touches the network.
pub fn verify_with_source(archive: &Path) -> (VerifyOutcome, TrustSource) {
    let keys = load_trusted_keys();
    let index = super::registry::load_offline();
    verify_with(archive, &keys, index.as_ref())
}

/// M6 — injectable version (gate/tests): explicit trust table + offline index instead of global loading.
pub fn verify_with(
    archive: &Path,
    keys: &BTreeMap<String, TrustedKey>,
    index: Option<&super::registry::RegistryIndex>,
) -> (VerifyOutcome, TrustSource) {
    let file = match std::fs::File::open(archive) {
        Ok(f) => f,
        Err(_) => return (VerifyOutcome::Unsigned, TrustSource::None), // On error, conservatively treat as Unsigned; install will reject again
    };
    let mut zip = match zip::ZipArchive::new(file) {
        Ok(z) => z,
        Err(_) => return (VerifyOutcome::Unsigned, TrustSource::None),
    };
    let mut manifest_text = String::new();
    let mut found = false;
    for i in 0..zip.len() {
        let mut entry = match zip.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.name() == "opencapx-plugin.json" {
            if entry.read_to_string(&mut manifest_text).is_ok() {
                found = true;
                break;
            }
        }
    }
    if !found {
        return (VerifyOutcome::Unsigned, TrustSource::None);
    }
    let (declared_sha, sig) = parse_sig_fields(&manifest_text);
    let (Some(declared_sha), Some(sig)) = (declared_sha, sig) else {
        return (VerifyOutcome::Unsigned, TrustSource::None);
    };
    match sig.alg.as_deref() {
        None | Some("hmac-v1") => {
            // Decode the declared sha256
            let declared_bytes = match hex_decode(&declared_sha) {
                Ok(b) => b,
                Err(_) => return (VerifyOutcome::MalformedSignature, TrustSource::None),
            };
            let actual = match compute_archive_hash(archive) {
                Ok(s) => s,
                Err(_) => return (VerifyOutcome::MalformedSignature, TrustSource::None),
            };
            if !declared_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>().eq_ignore_ascii_case(&actual) {
                return (
                    VerifyOutcome::HashMismatch {
                        declared: declared_sha,
                        actual,
                    },
                    TrustSource::None,
                );
            }
            // Find the trusted key (a non-HMAC shape = type mismatch, treated as an unknown key)
            let Some(secret) = keys.get(&sig.key_id).and_then(|k| match k {
                TrustedKey::Hmac(s) => Some(s),
                TrustedKey::Ed25519(_) => None,
            }) else {
                return (
                    VerifyOutcome::UnknownKey {
                        key_id: sig.key_id.clone(),
                    },
                    TrustSource::None,
                );
            };
            let expected = hmac_sha256_hex(secret, format!("opencapx-v1\n{}", actual).as_bytes());
            if !expected.eq_ignore_ascii_case(&sig.sig) {
                return (
                    VerifyOutcome::BadSignature {
                        key_id: sig.key_id.clone(),
                    },
                    TrustSource::None,
                );
            }
            (
                VerifyOutcome::Trusted {
                    key_id: sig.key_id,
                },
                TrustSource::Local,
            )
        }
        Some("ed25519") => {
            // v2: the digest covers the manifest, so manifest tampering also changes the digest (patches v1's hole)
            let digest = match signing::digest_v2(archive) {
                Ok(d) => d,
                Err(_) => return (VerifyOutcome::MalformedSignature, TrustSource::None),
            };
            if !declared_sha.eq_ignore_ascii_case(&digest) {
                return (
                    VerifyOutcome::HashMismatch {
                        declared: declared_sha,
                        actual: digest,
                    },
                    TrustSource::None,
                );
            }
            // Key lookup: explicit local registration wins (registered but type-mismatched = reject, no fallback);
            // only when the local set lacks that keyId do we consult the injected offline registry index.
            let (pubkey, source) = match keys.get(&sig.key_id) {
                Some(TrustedKey::Ed25519(pk)) => (Some(*pk), TrustSource::Local),
                Some(TrustedKey::Hmac(_)) => (None, TrustSource::None),
                None => match index
                    .and_then(|idx| super::registry::publisher_pubkey(idx, &sig.key_id))
                {
                    Some(pk) => (Some(pk), TrustSource::Registry),
                    None => (None, TrustSource::None),
                },
            };
            let Some(pubkey) = pubkey else {
                return (
                    VerifyOutcome::UnknownKey {
                        key_id: sig.key_id.clone(),
                    },
                    TrustSource::None,
                );
            };
            let Ok(verifying_key) = ed25519_dalek::VerifyingKey::from_bytes(&pubkey) else {
                return (VerifyOutcome::MalformedSignature, TrustSource::None);
            };
            let Ok(sig_bytes) = hex_decode(&sig.sig) else {
                return (VerifyOutcome::MalformedSignature, TrustSource::None);
            };
            let Ok(sig_bytes) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
                return (VerifyOutcome::MalformedSignature, TrustSource::None);
            };
            let message = format!("opencapx-v2\n{}", digest);
            match verifying_key.verify_strict(
                message.as_bytes(),
                &ed25519_dalek::Signature::from_bytes(&sig_bytes),
            ) {
                Ok(()) => (
                    VerifyOutcome::Trusted {
                        key_id: sig.key_id.clone(),
                    },
                    source,
                ),
                Err(_) => (
                    VerifyOutcome::BadSignature {
                        key_id: sig.key_id.clone(),
                    },
                    source,
                ),
            }
        }
        Some(_) => (VerifyOutcome::MalformedSignature, TrustSource::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::signing;
    use ed25519_dalek::Signer;
    use std::io::Write;
    use std::sync::Mutex;
    use zip::write::SimpleFileOptions;

    /// RFC 8032 TEST 1 fixed seed: same key signs/verifies, making the vector reproducible.
    const V2_TEST_SEED_HEX: &str =
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    /// RFC 8032 TEST 1 corresponding public key, i.e. the trusted-keys registration value.
    const V2_TEST_PUBKEY_HEX: &str =
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    /// Another seed: used to produce a "well-formed but failing" signature (BadSignature).
    const V2_WRONG_SEED_HEX: &str =
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    /// Serialize env var mutation, avoiding collisions on OPENCAPX_TRUSTED_KEYS when cargo test runs in parallel.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Lock both env and the global store: OPENCAPX_TRUSTED_KEYS is a process-level variable, and install/preview
    /// tests read it inside TEST_STORE_LOCK; new tests must serialize the same way, otherwise they could leak a temporary trusted-keys
    /// into someone else's install verification (the old v1 tests use a single lock and serialize with each other via this module's ENV_LOCK).
    fn lock_env_and_store() -> (
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let store_guard = crate::core::TEST_STORE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        (env_guard, store_guard)
    }

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let f = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = SimpleFileOptions::default();
        for (name, data) in entries {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
    }

    fn make_signed_zip(path: &Path, key_id: &str, secret: &[u8]) -> String {
        let bin = b"#!/bin/sh\necho ok\n";
        let manifest_no_sig = serde_json::json!({
            "id": "com.opencapx.test-sig",
            "name": "test-sig",
            "version": "0.1.0",
            "apiVersion": "1",
            "type": "capability",
            "capabilities": ["image.analyze"],
            "permissions": ["image.read"],
        });
        // First pack once with the "base" content lacking sha256/signature to compute the archive hash
        let tmp = path.with_extension("unsigned.ocplugin");
        let manifest_text = serde_json::to_string(&manifest_no_sig).unwrap();
        make_zip(&tmp, &[
            ("opencapx-plugin.json", manifest_text.as_bytes()),
            ("bin/run.sh", bin),
        ]);
        let hash = compute_archive_hash(&tmp).unwrap();
        // Embed the hash + signature back into the manifest
        let mut m: serde_json::Value = serde_json::from_str(&manifest_text).unwrap();
        let sig_hex = hmac_sha256_hex(secret, format!("opencapx-v1\n{}", hash).as_bytes());
        m["sha256"] = serde_json::Value::String(hash.clone());
        m["signature"] = serde_json::json!({"keyId": key_id, "sig": sig_hex});
        let signed_manifest = serde_json::to_string(&m).unwrap();
        // Repack (to keep the archive hash consistent: changing the manifest → the hash must be recomputed → but the manifest is skipped by sha256,
        // so after repacking the archive hash is still the concatenation of the same two non-manifest entries)
        make_zip(path, &[
            ("opencapx-plugin.json", signed_manifest.as_bytes()),
            ("bin/run.sh", bin),
        ]);
        // Clean up the temporary file
        let _ = std::fs::remove_file(&tmp);
        hash
    }

    fn v2_seed() -> [u8; 32] {
        hex_decode(V2_TEST_SEED_HEX).unwrap().try_into().unwrap()
    }

    fn v2_base_manifest() -> serde_json::Value {
        serde_json::json!({
            "id": "com.opencapx.test-v2",
            "name": "test-v2",
            "version": "0.1.0",
            "apiVersion": "1",
            "type": "capability",
            "capabilities": ["image.analyze"],
            "permissions": ["image.read"],
        })
    }

    /// Two-phase signing of a v2 package: first pack a base without signature artifacts to compute `digest_v2`, then sign with the seed, and finally write back
    /// `sha256` + `signature` (including alg). The digest is stable because canonical form excludes these two fields.
    fn make_v2_signed_zip(path: &Path, key_id: &str, seed: [u8; 32]) -> String {
        let bin = b"#!/bin/sh\necho v2\n";
        let base = v2_base_manifest();
        let base_text = serde_json::to_string(&base).unwrap();
        let tmp = path.with_extension("v2unsigned.ocplugin");
        make_zip(
            &tmp,
            &[
                ("opencapx-plugin.json", base_text.as_bytes()),
                ("bin/run.sh", bin),
            ],
        );
        let digest = signing::digest_v2(&tmp).unwrap();
        let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
        let sig_hex = hex_encode(&sk.sign(format!("opencapx-v2\n{}", digest).as_bytes()).to_bytes());
        let mut m = base.clone();
        m["sha256"] = serde_json::Value::String(digest.clone());
        m["signature"] = serde_json::json!({"keyId": key_id, "sig": sig_hex, "alg": "ed25519"});
        let signed = serde_json::to_string(&m).unwrap();
        make_zip(
            path,
            &[
                ("opencapx-plugin.json", signed.as_bytes()),
                ("bin/run.sh", bin),
            ],
        );
        let _ = std::fs::remove_file(&tmp);
        digest
    }

    fn write_ed25519_keys(dir: &Path, key_id: &str, pubkey_hex: &str) -> std::path::PathBuf {
        let p = dir.join("trusted-keys.json");
        let mut obj = serde_json::Map::new();
        obj.insert(
            key_id.to_string(),
            serde_json::json!({ "alg": "ed25519", "publicKey": pubkey_hex }),
        );
        std::fs::write(&p, serde_json::to_string(&serde_json::Value::Object(obj)).unwrap()).unwrap();
        p
    }

    fn read_zip_entries(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut zr = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut out = Vec::new();
        for i in 0..zr.len() {
            let mut e = zr.by_index(i).unwrap();
            if e.is_dir() {
                continue;
            }
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            out.push((e.name().to_string(), buf));
        }
        out
    }

    fn repack_zip(path: &Path, entries: &[(String, Vec<u8>)]) {
        let f = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = SimpleFileOptions::default();
        for (n, b) in entries {
            zw.start_file(n.as_str(), opts).unwrap();
            zw.write_all(b).unwrap();
        }
        zw.finish().unwrap();
    }

    #[test]
    fn hmac_matches_rfc4231_test_case_1() {
        // RFC 4231 Test Case 1: key = 0x0b * 20, data = "Hi There"
        // expected = b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7
        let key = vec![0x0b; 20];
        let got = hmac_sha256_hex(&key, b"Hi There");
        assert_eq!(got, "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
    }

    /// RFC 8032 TEST 1 anchor: fixed seed → fixed public key (proves the dalek usage is correct).
    #[test]
    fn ed25519_matches_rfc8032_test1() {
        let sk = ed25519_dalek::SigningKey::from_bytes(&v2_seed());
        assert_eq!(
            hex_encode(sk.verifying_key().as_bytes()),
            V2_TEST_PUBKEY_HEX
        );
    }

    /// Legacy JSON (no alg) must parse as-is; the v2 shape with alg parses as usual.
    #[test]
    fn signature_serde_is_backward_compatible() {
        let legacy: Signature = serde_json::from_str(r#"{"keyId":"k","sig":"ab"}"#).unwrap();
        assert_eq!(legacy.alg, None, "missing alg → None (v1 channel)");
        let v2: Signature =
            serde_json::from_str(r#"{"keyId":"k","sig":"ab","alg":"ed25519"}"#).unwrap();
        assert_eq!(v2.alg.as_deref(), Some("ed25519"));
        let v1: Signature =
            serde_json::from_str(r#"{"keyId":"k","sig":"ab","alg":"hmac-v1"}"#).unwrap();
        assert_eq!(v1.alg.as_deref(), Some("hmac-v1"));
    }

    /// trusted-keys both shapes: string = HMAC; object (alg=ed25519) = Ed25519; bad entries skipped.
    #[test]
    fn load_trusted_keys_accepts_both_shapes_and_skips_bad_rows() {
        let _guards = lock_env_and_store();
        let dir = std::env::temp_dir().join(format!("opencapx-sig-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keys_path = dir.join("trusted-keys.json");
        let secret = b"shared-secret-32-bytes-xxxxxxx";
        let keys = serde_json::json!({
            "hmac-key": hex_encode(secret),
            "pub-key": { "alg": "ed25519", "publicKey": V2_TEST_PUBKEY_HEX },
            "bad-hex": "zzzz",
            "bad-alg": { "alg": "rsa-v1", "publicKey": V2_TEST_PUBKEY_HEX },
            "bad-len": { "alg": "ed25519", "publicKey": "abcd" },
            "no-pubkey": { "alg": "ed25519" },
        });
        std::fs::write(&keys_path, serde_json::to_string(&keys).unwrap()).unwrap();
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &keys_path);

        let loaded = load_trusted_keys();
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(loaded.len(), 2, "only 2 valid rows, the rest skipped: {:?}", loaded.keys());
        assert_eq!(loaded.get("hmac-key"), Some(&TrustedKey::Hmac(secret.to_vec())));
        let want_pk: [u8; 32] = hex_decode(V2_TEST_PUBKEY_HEX).unwrap().try_into().unwrap();
        assert_eq!(loaded.get("pub-key"), Some(&TrustedKey::Ed25519(want_pk)));
    }

    /// v2 positive: a signed v2 package + registered public key → Trusted.
    #[test]
    fn verify_trusted_for_v2_ed25519() {
        let _guards = lock_env_and_store();
        let dir = std::env::temp_dir().join(format!("opencapx-v2-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keys_path = write_ed25519_keys(&dir, "pub-key-1", V2_TEST_PUBKEY_HEX);
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &keys_path);

        let zip = dir.join("v2.zip");
        make_v2_signed_zip(&zip, "pub-key-1", v2_seed());
        assert_eq!(
            verify(&zip),
            VerifyOutcome::Trusted { key_id: "pub-key-1".into() }
        );
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v2 tampered manifest (sneak in a high-risk permission after signing, keeping the old sha256/signature) → HashMismatch.
    /// This is exactly v1's hole (v1's archive hash deliberately excludes the manifest, so a change cannot be detected).
    #[test]
    fn verify_detects_tampered_manifest_in_v2() {
        let _guards = lock_env_and_store();
        let dir = std::env::temp_dir().join(format!("opencapx-v2-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keys_path = write_ed25519_keys(&dir, "pub-key-1", V2_TEST_PUBKEY_HEX);
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &keys_path);

        let zip = dir.join("v2.zip");
        make_v2_signed_zip(&zip, "pub-key-1", v2_seed());
        let mut entries = read_zip_entries(&zip);
        for (n, b) in entries.iter_mut() {
            if n == "opencapx-plugin.json" {
                let mut m: serde_json::Value = serde_json::from_slice(b).unwrap();
                m["permissions"] = serde_json::json!(["image.read", "shell.exec"]);
                *b = serde_json::to_vec(&m).unwrap();
            }
        }
        repack_zip(&zip, &entries);

        match verify(&zip) {
            VerifyOutcome::HashMismatch { .. } => {}
            other => panic!(
                "manifest tampering that v1 cannot catch must be HashMismatch in v2, got {:?}",
                other
            ),
        }
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v2 negative matrix: tampered entry / wrong signature / unknown key / type mismatch / unknown alg.
    #[test]
    fn verify_v2_negative_matrix() {
        let _guards = lock_env_and_store();
        let dir = std::env::temp_dir().join(format!("opencapx-v2-neg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keys_path = write_ed25519_keys(&dir, "pub-key-1", V2_TEST_PUBKEY_HEX);
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &keys_path);

        // 1) tampered entry → HashMismatch
        let zip = dir.join("tamper-entry.zip");
        make_v2_signed_zip(&zip, "pub-key-1", v2_seed());
        let mut entries = read_zip_entries(&zip);
        for (n, b) in entries.iter_mut() {
            if n == "bin/run.sh" {
                b[0] = b'X';
            }
        }
        repack_zip(&zip, &entries);
        match verify(&zip) {
            VerifyOutcome::HashMismatch { .. } => {}
            other => panic!("tampered entry should be HashMismatch, got {:?}", other),
        }

        // 2) sign with a different seed (well-formed but failing) → BadSignature
        let zip = dir.join("wrong-sig.zip");
        let wrong_seed: [u8; 32] = hex_decode(V2_WRONG_SEED_HEX).unwrap().try_into().unwrap();
        make_v2_signed_zip(&zip, "pub-key-1", wrong_seed);
        match verify(&zip) {
            VerifyOutcome::BadSignature { key_id } => assert_eq!(key_id, "pub-key-1"),
            other => panic!("wrong signature should be BadSignature, got {:?}", other),
        }

        // 3) unknown keyId → UnknownKey
        let zip = dir.join("unknown-key.zip");
        make_v2_signed_zip(&zip, "ghost-key", v2_seed());
        match verify(&zip) {
            VerifyOutcome::UnknownKey { key_id } => assert_eq!(key_id, "ghost-key"),
            other => panic!("unknown keyId should be UnknownKey, got {:?}", other),
        }

        // 4) the registered keyId is HMAC but declares ed25519 → type mismatch → UnknownKey
        let hmac_keys = dir.join("trusted-keys.json");
        let secret = b"shared-secret-32-bytes-xxxxxxx";
        let keys = serde_json::json!({ "test-key-1": hex_encode(secret) });
        std::fs::write(&hmac_keys, serde_json::to_string(&keys).unwrap()).unwrap();
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &hmac_keys);
        let zip = dir.join("type-mismatch.zip");
        make_v2_signed_zip(&zip, "test-key-1", v2_seed());
        match verify(&zip) {
            VerifyOutcome::UnknownKey { key_id } => assert_eq!(key_id, "test-key-1"),
            other => panic!("an HMAC key against an ed25519 declaration should be UnknownKey, got {:?}", other),
        }

        // 5) unknown alg (rsa-v1) → MalformedSignature (dispatch rejects directly before computing the digest)
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &keys_path);
        let zip = dir.join("unknown-alg.zip");
        make_v2_signed_zip(&zip, "pub-key-1", v2_seed());
        let mut entries = read_zip_entries(&zip);
        for (n, b) in entries.iter_mut() {
            if n == "opencapx-plugin.json" {
                let mut m: serde_json::Value = serde_json::from_slice(b).unwrap();
                m["signature"]["alg"] = serde_json::Value::String("rsa-v1".into());
                *b = serde_json::to_vec(&m).unwrap();
            }
        }
        repack_zip(&zip, &entries);
        assert_eq!(verify(&zip), VerifyOutcome::MalformedSignature);

        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_hash_is_deterministic() {
        let dir = std::env::temp_dir().join(format!("opencapx-sig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("a.zip");
        make_zip(&zip, &[("a.txt", b"hello"), ("b.txt", b"world")]);
        let h1 = compute_archive_hash(&zip).unwrap();
        let h2 = compute_archive_hash(&zip).unwrap();
        assert_eq!(h1, h2, "identical input produces identical hash");
        assert_eq!(h1.len(), 64, "SHA-256 = 64 hex chars");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_hash_skips_manifest_and_dirs() {
        let dir = std::env::temp_dir().join(format!("opencapx-sig2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("a.zip");
        // Includes a directory + manifest; expect the hash to cover only a.txt
        let f = std::fs::File::create(&zip).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = SimpleFileOptions::default();
        zw.add_directory("nested/", opts).unwrap();
        zw.start_file("opencapx-plugin.json", opts).unwrap();
        zw.write_all(br#"{"id":"x"}"#).unwrap();
        zw.start_file("a.txt", opts).unwrap();
        zw.write_all(b"payload").unwrap();
        zw.finish().unwrap();
        let h = compute_archive_hash(&zip).unwrap();
        // Independently compute the reference: only a.txt (payload = 7 bytes)
        let mut acc: Vec<u8> = Vec::new();
        acc.extend_from_slice(b"a.txt\n7\npayload");
        let expect = sha256_hex(&acc);
        assert_eq!(h, expect);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_trusted_when_signed_with_known_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("opencapx-sig3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keys_path = dir.join("trusted-keys.json");
        let secret = b"shared-secret-32-bytes-xxxxxxx";
        let keys = serde_json::json!({
            "test-key-1": hex_encode(secret),
        });
        std::fs::write(&keys_path, serde_json::to_string(&keys).unwrap()).unwrap();
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &keys_path);

        let zip = dir.join("signed.zip");
        make_signed_zip(&zip, "test-key-1", secret);
        let outcome = verify(&zip);
        assert_eq!(outcome, VerifyOutcome::Trusted { key_id: "test-key-1".into() });
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_unknown_key_when_keyid_not_in_registry() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("opencapx-sig4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Empty trusted-keys → but the manifest carries a signature → expect UnknownKey
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", dir.join("missing.json"));

        let zip = dir.join("signed.zip");
        make_signed_zip(&zip, "ghost-key", b"doesnt-matter");
        let outcome = verify(&zip);
        assert_eq!(outcome, VerifyOutcome::UnknownKey { key_id: "ghost-key".into() });
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_bad_signature_when_hmac_mismatches() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("opencapx-sig5-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keys_path = dir.join("trusted-keys.json");
        let real_secret = b"real-secret-32-bytes-xxxxxxx";
        let wrong_secret = b"WRONG-secret-32-bytes-xxxxxx";
        let keys = serde_json::json!({
            "test-key-1": hex_encode(real_secret),
        });
        std::fs::write(&keys_path, serde_json::to_string(&keys).unwrap()).unwrap();
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &keys_path);

        let zip = dir.join("signed.zip");
        // Sign with wrong_secret → keyId is in trusted-keys but the sig does not match
        make_signed_zip(&zip, "test-key-1", wrong_secret);
        let outcome = verify(&zip);
        match outcome {
            VerifyOutcome::BadSignature { key_id } => assert_eq!(key_id, "test-key-1"),
            other => panic!("expected BadSignature, got {:?}", other),
        }
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_hash_mismatch_when_entry_tampered() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("opencapx-sig6-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keys_path = dir.join("trusted-keys.json");
        let secret = b"shared-secret-32-bytes-xxxxxxx";
        let keys = serde_json::json!({
            "test-key-1": hex_encode(secret),
        });
        std::fs::write(&keys_path, serde_json::to_string(&keys).unwrap()).unwrap();
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &keys_path);

        let zip = dir.join("signed.zip");
        make_signed_zip(&zip, "test-key-1", secret);
        // Change one byte: alter bin/run.sh content → the recomputed archive hash must mismatch manifest.sha256
        let mut zip_r = zip::ZipArchive::new(std::fs::File::open(&zip).unwrap()).unwrap();
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for i in 0..zip_r.len() {
            let mut e = zip_r.by_index(i).unwrap();
            if e.is_dir() {
                continue;
            }
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            entries.push((e.name().to_string(), buf));
        }
        drop(zip_r);
        for (n, b) in entries.iter_mut() {
            if n == "bin/run.sh" {
                b[0] = b'X';
            }
        }
        let f = std::fs::File::create(&zip).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = SimpleFileOptions::default();
        for (n, b) in &entries {
            zw.start_file(n.as_str(), opts).unwrap();
            zw.write_all(b).unwrap();
        }
        zw.finish().unwrap();

        let outcome = verify(&zip);
        match outcome {
            VerifyOutcome::HashMismatch { .. } => {}
            other => panic!("expected HashMismatch, got {:?}", other),
        }
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_unsigned_when_no_signature_field_and_empty_keys() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("opencapx-sig7-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", dir.join("missing.json"));

        let zip = dir.join("plain.zip");
        make_zip(&zip, &[
            ("opencapx-plugin.json", br#"{"id":"x","name":"x","version":"0.1.0","apiVersion":"1","type":"capability","capabilities":["image.analyze"],"permissions":[]}"#),
            ("bin/run.sh", b"#!/bin/sh"),
        ]);
        let outcome = verify(&zip);
        assert_eq!(outcome, VerifyOutcome::Unsigned);
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn hex_encode(b: &[u8]) -> String {
        b.iter().map(|x| format!("{:02x}", x)).collect()
    }

    /// Fixture root: `fixtures/signing` at the repo root (CARGO_MANIFEST_DIR = src-tauri).
    /// WHY: golden vectors cross-language (Rust CLI + Python SDK) pin the same byte digest in both directions,
    /// so they must be fixed artifacts committed to the repo, not temp files generated at test time.
    fn fixtures_signing_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("fixtures")
            .join("signing")
    }

    /// Golden-vector regression: the committed `signed.ocplugin`'s digest_v2 and signature are pinned byte-for-byte,
    /// and verified directly with the fixture public key (not via trusted-keys). Any implementation drift (frame, canonical, ordering)
    /// will turn this test red — this is the shared anchor of Rust and the Python SDK.
    #[test]
    fn golden_vectors_pin_digest_and_signature() {
        let root = fixtures_signing_root();
        let archive = root.join("golden").join("signed.ocplugin");
        let expect_digest =
            std::fs::read_to_string(root.join("golden").join("digest_v2.hex")).unwrap();
        assert_eq!(
            signing::digest_v2(&archive).unwrap(),
            expect_digest.trim(),
            "the committed signed.ocplugin digest must match golden/digest_v2.hex byte-for-byte"
        );
        // Direct public-key verification (not via trusted-keys): the signature is valid over "opencapx-v2\n"+digest_hex
        let pubkey: [u8; 32] = hex_decode(
            std::fs::read_to_string(root.join("key.pub.hex"))
                .unwrap()
                .trim(),
        )
        .unwrap()
        .try_into()
        .unwrap();
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&pubkey).unwrap();
        let sig_hex = std::fs::read_to_string(root.join("golden").join("signature.hex")).unwrap();
        let sig_bytes: [u8; 64] = hex_decode(sig_hex.trim()).unwrap().try_into().unwrap();
        let msg = format!("opencapx-v2\n{}", expect_digest.trim());
        vk.verify_strict(msg.as_bytes(), &ed25519_dalek::Signature::from_bytes(&sig_bytes))
            .expect("golden signature must verify");
    }

    /// Golden archive + fixture trusted-keys → verify must return Trusted (end-to-end through alg dispatch + loading).
    #[test]
    fn golden_archive_verifies_trusted_with_fixture_keys() {
        let _guards = lock_env_and_store();
        let root = fixtures_signing_root();
        let archive = root.join("golden").join("signed.ocplugin");
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", root.join("trusted-keys.json"));
        assert_eq!(
            verify(&archive),
            VerifyOutcome::Trusted {
                key_id: "com.opencapx.test-signing".into()
            }
        );
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
    }

    /// Python SDK root path: CARGO_MANIFEST_DIR = src-tauri → repo root's packages/plugin-sdk.
    fn sdk_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("packages")
            .join("plugin-sdk")
    }

    /// Cross-language loop (Python → Rust): the digest computed by the Python subprocess for the fixture directory must equal
    /// golden, and a package packed by the Python SDK must be judged Trusted by Rust `verify()`.
    ///
    /// WHY: drift in any of frame/canonical/ordering makes the two implementations silently mismatch on the real signing chain;
    /// the reverse direction (Rust → Python) is covered by a Python test that directly verifies the golden archive committed by Rust.
    /// Gracefully skips when python3 or cryptography is missing (so an environment without the dependencies does not go all red).
    #[test]
    fn cross_language_sdk_agrees_with_rust() {
        let _guards = lock_env_and_store();
        match std::process::Command::new("python3")
            .args(["-c", "import cryptography"])
            .status()
        {
            Ok(s) if s.success() => {}
            _ => {
                eprintln!(
                    "skip: python3 or cryptography unavailable, cross_language_sdk_agrees_with_rust skipped"
                );
                return;
            }
        }

        let root = fixtures_signing_root();
        let tmp = std::env::temp_dir()
            .join(format!("opencapx-sdk-cross-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let packed = tmp.join("sdk-packed.ocplugin");

        let script = r#"
import os, sys
from opencapx_sdk import signing
root = sys.argv[1]
plugin = os.path.join(root, "plugin")
print(signing.digest_v2_for_dir(plugin))
_, d = signing.pack_dir(
    plugin,
    sys.argv[2],
    seed_hex=open(os.path.join(root, "key.seed.hex")).read().strip(),
    key_id="com.opencapx.test-signing",
)
assert d == open(os.path.join(root, "golden", "digest_v2.hex")).read().strip(), "digest != golden"
"#;
        let out = std::process::Command::new("python3")
            .arg("-c")
            .arg(script)
            .arg(root.to_str().unwrap())
            .arg(packed.to_str().unwrap())
            .env("PYTHONPATH", sdk_root())
            .output()
            .expect("spawn python3");
        assert!(
            out.status.success(),
            "python3 SDK subprocess failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        let printed = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        let expect = std::fs::read_to_string(root.join("golden").join("digest_v2.hex")).unwrap();
        assert_eq!(
            printed,
            expect.trim(),
            "Python SDK digest must match golden byte-for-byte"
        );

        std::env::set_var("OPENCAPX_TRUSTED_KEYS", root.join("trusted-keys.json"));
        let outcome = verify(&packed);
        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(
            outcome,
            VerifyOutcome::Trusted {
                key_id: "com.opencapx.test-signing".into()
            },
            "Rust must trust the package packed by the Python SDK"
        );
    }

    /// M3: when local trusted-keys is empty, verification falls back to the **offline registry's publisher public key**,
    /// and the source is tagged Registry (three sources: Local / Registry / None).
    #[test]
    fn verify_falls_back_to_registry_publisher() {
        let _guards = lock_env_and_store();
        let reg_dir = std::env::temp_dir().join(format!("opencapx-reg-fb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&reg_dir);
        std::fs::create_dir_all(&reg_dir).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("fixtures")
            .join("registry");
        // registry cache = signed fixture index; local trusted-keys = empty object.
        std::fs::copy(fixtures.join("index.signed.json"), reg_dir.join("cache.json")).unwrap();
        let empty_keys = reg_dir.join("trusted-keys.json");
        std::fs::write(&empty_keys, "{}").unwrap();
        let pub_hex = std::fs::read_to_string(fixtures.join("official.pub.hex")).unwrap();
        std::env::set_var("OPENCAPX_TRUSTED_KEYS", &empty_keys);
        std::env::set_var("OPENCAPX_REGISTRY_DIR", &reg_dir);
        std::env::set_var(
            "OPENCAPX_REGISTRY_OFFICIAL_KEYS",
            format!("com.opencapx.test-official={}", pub_hex.trim()),
        );

        let archive = fixtures_signing_root().join("golden").join("signed.ocplugin");
        let (outcome, source) = verify_with_source(&archive);

        std::env::remove_var("OPENCAPX_TRUSTED_KEYS");
        std::env::remove_var("OPENCAPX_REGISTRY_DIR");
        std::env::remove_var("OPENCAPX_REGISTRY_OFFICIAL_KEYS");
        let _ = std::fs::remove_dir_all(&reg_dir);

        assert_eq!(
            outcome,
            VerifyOutcome::Trusted {
                key_id: "com.opencapx.test-signing".into()
            },
            "with no local registration, the registry publisher public key should be allowed"
        );
        assert_eq!(source, TrustSource::Registry);
    }
}
