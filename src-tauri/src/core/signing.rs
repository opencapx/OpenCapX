//! M2 — v2 canonical digest (a digest that covers the manifest).
//!
//! WHY: v1's archive hash deliberately excluded `opencapx-plugin.json`, so tampering with the
//! manifest (e.g. secretly adding a high-risk permission) could not be detected by the signature. v2 folds the manifest's **canonical form**
//! (dropping the two signature-product fields `sha256`/`signature`, keys in lexicographic order at every level, no whitespace, UTF-8 as-is)
//! into the digest together with the entry files, so the manifest itself is signature-protected too.
//!
//! The frame spec is frozen (byte for byte); see docs/superpowers/plans/2026-09-14-m2-signing.md
//! Global Constraints; the Python SDK (Task 4) must replicate it byte for byte, with golden vectors pinned from both sides.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::marketplace::sha256_hex;
use super::plugin_sig;

/// Normalize the manifest JSON into the byte form that participates in the digest.
///
/// Rule: parse → drop `sha256`/`signature` (they are signature products; including them would be self-referential) → serialize.
/// serde_json's default `Map` = `BTreeMap` (lexicographic keys at every level), and `to_string` is compact with no whitespace
/// and preserves non-ASCII as UTF-8 — exactly what the spec requires, no extra handling needed.
pub fn canonical_manifest(manifest_json: &str) -> Result<String, String> {
    let mut value: serde_json::Value = serde_json::from_str(manifest_json)
        .map_err(|e| format!("manifest is not valid JSON: {}", e))?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| "manifest top level must be a JSON object".to_string())?;
    obj.remove("sha256");
    obj.remove("signature");
    serde_json::to_string(&value).map_err(|e| format!("canonical serialization failed: {}", e))
}

/// Numeric dialect for signed manifests: Python (json) and serde_json (ryu) serialize
/// floats / out-of-range integers differently → cross-language digests would diverge. Rejected explicitly at pack time; the boundary is fixed here.
pub fn ensure_signable_numbers(v: &serde_json::Value) -> Result<(), String> {
    match v {
        serde_json::Value::Number(n) => {
            if n.is_f64() {
                return Err(format!(
                    "manifest number {n} is not a 64-bit integer (floats / out-of-range integers are unsupported in signed manifests)"
                ));
            }
            Ok(())
        }
        serde_json::Value::Array(a) => a.iter().try_for_each(ensure_signable_numbers),
        serde_json::Value::Object(o) => o.values().try_for_each(ensure_signable_numbers),
        _ => Ok(()),
    }
}

/// Collect content entries from the archive (same criteria as v1, see [`plugin_sig::collect_archive_entries`]).
pub fn entries_from_archive(archive: &Path) -> Result<Vec<(String, u64, Vec<u8>)>, String> {
    plugin_sig::collect_archive_entries(archive)
}

/// Concatenate per the frozen frame spec and SHA-256:
/// `"opencapx-canon-v2\n" ‖ decimal(len(canonical)) ‖ "\n" ‖ canonical
///  ‖ Σ name ‖ "\n" ‖ decimal(size) ‖ "\n" ‖ bytes` (entries ordered by name bytes).
pub fn digest_v2_from_parts(canonical: &str, entries: &[(String, u64, Vec<u8>)]) -> String {
    let mut sorted: Vec<&(String, u64, Vec<u8>)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut acc: Vec<u8> = Vec::new();
    acc.extend_from_slice(b"opencapx-canon-v2\n");
    acc.extend_from_slice(canonical.as_bytes().len().to_string().as_bytes());
    acc.push(b'\n');
    acc.extend_from_slice(canonical.as_bytes());
    for (name, size, bytes) in sorted {
        acc.extend_from_slice(name.as_bytes());
        acc.push(b'\n');
        acc.extend_from_slice(size.to_string().as_bytes());
        acc.push(b'\n');
        acc.extend_from_slice(bytes);
    }
    sha256_hex(&acc)
}

/// Compute the v2 digest for a packed `.ocplugin` (the manifest section comes from the manifest inside the archive).
pub fn digest_v2(archive: &Path) -> Result<String, String> {
    let canonical = canonical_manifest(&manifest_text_from_archive(archive)?)?;
    let entries = entries_from_archive(archive)?;
    Ok(digest_v2_from_parts(&canonical, &entries))
}

/// Compute the v2 digest for an unpacked plugin directory (used by pack).
/// Recursively walk; relative paths use `/` uniformly; the manifest is excluded; the rest follows the same frame.
pub fn digest_v2_for_dir(dir: &Path) -> Result<String, String> {
    let manifest_path = dir.join("opencapx-plugin.json");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("failed to read {}: {}", manifest_path.display(), e))?;
    let canonical = canonical_manifest(&manifest)?;
    let entries = collect_dir_entries(dir)?;
    Ok(digest_v2_from_parts(&canonical, &entries))
}

/// Read the text of root's `opencapx-plugin.json` from the archive.
fn manifest_text_from_archive(archive: &Path) -> Result<String, String> {
    let file = std::fs::File::open(archive)
        .map_err(|e| format!("cannot open {}: {}", archive.display(), e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("bad zip: {}", e))?;
    let mut entry = zip
        .by_name("opencapx-plugin.json")
        .map_err(|_| format!("{} is missing opencapx-plugin.json", archive.display()))?;
    let mut text = String::new();
    entry
        .read_to_string(&mut text)
        .map_err(|e| format!("failed to read manifest: {}", e))?;
    Ok(text)
}

/// Recursively collect regular files in the directory; relative paths join with `/`; the manifest is excluded.
fn collect_dir_entries(dir: &Path) -> Result<Vec<(String, u64, Vec<u8>)>, String> {
    let mut out: Vec<(String, u64, Vec<u8>)> = Vec::new();
    walk_dir(dir, dir, &mut out)?;
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(out)
}

fn walk_dir(root: &Path, cur: &Path, out: &mut Vec<(String, u64, Vec<u8>)>) -> Result<(), String> {
    let mut children: Vec<PathBuf> = std::fs::read_dir(cur)
        .map_err(|e| format!("failed to read directory {}: {}", cur.display(), e))?
        .filter_map(|r| r.ok().map(|e| e.path()))
        .collect();
    children.sort();
    for path in children {
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("stat {} failed: {}", path.display(), e))?;
        if meta.is_dir() {
            walk_dir(root, &path, out)?;
            continue;
        }
        if !meta.is_file() {
            continue; // Non-regular files such as symlinks are not packed
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|e| format!("relative path failed: {}", e))?;
        let rel_str = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if rel_str == "opencapx-plugin.json" || rel_str.ends_with("/opencapx-plugin.json") {
            continue;
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        out.push((rel_str, bytes.len() as u64, bytes));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            for (name, data) in entries {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(data).unwrap();
            }
            zw.finish().unwrap();
        }
        buf
    }

    /// Hand-computed frame reference: the manifest section has a length prefix; the entries section is isomorphic to v1.
    #[test]
    fn digest_framing_is_exact() {
        let canonical = r#"{"a":1}"#; // 7 bytes
        let entries = vec![("z.txt".to_string(), 5u64, b"hello".to_vec())];
        let mut acc: Vec<u8> = Vec::new();
        acc.extend_from_slice(b"opencapx-canon-v2\n");
        acc.extend_from_slice(b"7\n");
        acc.extend_from_slice(canonical.as_bytes());
        acc.extend_from_slice(b"z.txt\n5\nhello");
        let expect = crate::core::marketplace::sha256_hex(&acc);
        assert_eq!(digest_v2_from_parts(canonical, &entries), expect);
    }

    /// canonical: drops sha256/signature; sorts keys; compacts whitespace; preserves non-ASCII as-is.
    #[test]
    fn canonical_manifest_removes_sig_fields_and_sorts_keys() {
        let raw = r#"{"version":"1.0.0","name":"café","sha256":"aa","signature":{"keyId":"k","sig":"s"},"id":"com.x"}"#;
        let c = canonical_manifest(raw).unwrap();
        assert_eq!(c, r#"{"id":"com.x","name":"café","version":"1.0.0"}"#);
    }

    /// A nested structure of pure integers + strings is signable (this is what the golden fixture looks like; it must not be broken).
    #[test]
    fn ensure_signable_numbers_accepts_integers() {
        let v: serde_json::Value = serde_json::json!({
            "id": "com.x",
            "count": 3,
            "nested": {"a": [1, 2, 0], "b": "s"},
            "flag": true,
            "nothing": null,
        });
        assert!(ensure_signable_numbers(&v).is_ok());
    }

    /// Floats go through ryu and diverge from Python json serialization → reject.
    #[test]
    fn ensure_signable_numbers_rejects_floats() {
        let v: serde_json::Value = serde_json::json!({"ratio": 1.5});
        assert!(ensure_signable_numbers(&v).is_err());
        let sci: serde_json::Value = serde_json::from_str(r#"{"big":1e16}"#).unwrap();
        assert!(ensure_signable_numbers(&sci).is_err());
    }

    /// Integer literals beyond the u64 range are parsed by serde_json as f64 → reject.
    #[test]
    fn ensure_signable_numbers_rejects_out_of_range_integer_literal() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"huge":100000000000000000000}"#).unwrap();
        assert!(ensure_signable_numbers(&v).is_err());
    }

    /// Digest determinism + agreement between the directory and archive sources.
    #[test]
    fn digest_v2_dir_and_zip_agree() {
        let dir = std::env::temp_dir().join(format!("opencapx-sign-t1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        // WHY: both sources must represent the same content (the manifest byte-for-byte too), otherwise the canonical section differs
        // and the digests were never meant to be equal.
        std::fs::write(dir.join("opencapx-plugin.json"), r#"{"id":"com.x"}"#).unwrap();
        std::fs::write(dir.join("bin/run.sh"), b"echo ok\n").unwrap();
        // WHY: the archive lives outside the walked directory, otherwise it would itself enter the directory-side digest as an entry.
        let zip_path = dir.with_extension("ocplugin");
        std::fs::write(
            &zip_path,
            zip_with(&[
                ("opencapx-plugin.json", b"{\"id\":\"com.x\"}"),
                ("bin/run.sh", b"echo ok\n"),
            ]),
        )
        .unwrap();
        assert_eq!(
            digest_v2_for_dir(&dir).unwrap(),
            digest_v2(&zip_path).unwrap()
        );
        let _ = std::fs::remove_file(&zip_path);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
