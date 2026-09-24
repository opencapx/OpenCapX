//! M6 — pack tool core: package a plugin directory into a signed `.ocplugin` (the CLI `opencapx pack`,
//! the template release flow, and tests all share the same path). The numeric-dialect guard runs at the pack boundary (rejected before anything lands on disk).

use std::io::Write;
use std::path::Path;

use super::signing;

/// Pack directory → signed artifact; returns the v2 digest (hex). On failure, cleans up the half-built output and returns the reason.
pub fn pack_dir(dir: &Path, seed: &[u8; 32], key_id: &str, out: &Path) -> Result<String, String> {
    let manifest_path = dir.join("opencapx-plugin.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("failed to read {}: {}", manifest_path.display(), e))?;
    let mut manifest: serde_json::Value = serde_json::from_str(&manifest_text)
        .map_err(|e| format!("manifest is not valid JSON: {}", e))?;
    // WHY: Python (json) and serde_json (ryu) serialize floats / out-of-range integers differently, which would make
    // cross-language digests silently diverge; this is the pack boundary, nothing has landed on disk yet, so reject outright.
    signing::ensure_signable_numbers(&manifest)?;
    let digest = signing::digest_v2_for_dir(dir)?;
    let sk = ed25519_dalek::SigningKey::from_bytes(seed);
    let msg = format!("opencapx-v2\n{}", digest);
    use ed25519_dalek::Signer;
    let sig_bytes = sk.sign(msg.as_bytes()).to_bytes();

    let obj = manifest
        .as_object_mut()
        .ok_or_else(|| "manifest top level must be a JSON object".to_string())?;
    obj.insert("sha256".into(), serde_json::Value::String(digest.clone()));
    obj.insert(
        "signature".into(),
        serde_json::json!({"alg": "ed25519", "keyId": key_id, "sig": hex_encode(&sig_bytes)}),
    );
    let signed_manifest = serde_json::to_string(&manifest)
        .map_err(|e| format!("failed to serialize manifest: {}", e))?;
    let files = collect_plugin_files(dir)?;

    let write = (|| -> Result<(), String> {
        let file = std::fs::File::create(out)
            .map_err(|e| format!("failed to create {}: {}", out.display(), e))?;
        let mut zw = zip::ZipWriter::new(file);
        zip_add(&mut zw, "opencapx-plugin.json", signed_manifest.as_bytes())?;
        for (name, bytes) in &files {
            zip_add(&mut zw, name, bytes)?;
        }
        zw.finish()
            .map_err(|e| format!("failed to finish zip: {}", e))?;
        Ok(())
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(out);
        return Err(e);
    }

    // Self-check: a signing tool that produces a package it cannot itself verify is a silent failure worse than not signing; stop it before it lands on disk.
    let digest_ok = signing::digest_v2(out)
        .map(|d| d == digest)
        .unwrap_or(false);
    let sig_ok = sk
        .verifying_key()
        .verify_strict(
            msg.as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&sig_bytes),
        )
        .is_ok();
    if !digest_ok || !sig_ok {
        let _ = std::fs::remove_file(out);
        return Err(format!(
            "self-check failed (digest_ok={}, sig_ok={}), artifact deleted",
            digest_ok, sig_ok
        ));
    }
    Ok(digest)
}

fn zip_add(zw: &mut zip::ZipWriter<std::fs::File>, name: &str, data: &[u8]) -> Result<(), String> {
    let opts = zip::write::SimpleFileOptions::default();
    zw.start_file(name, opts)
        .map_err(|e| format!("failed to write zip entry {}: {}", name, e))?;
    zw.write_all(data)
        .map_err(|e| format!("failed to write zip entry {}: {}", name, e))?;
    Ok(())
}

/// Recursively collect regular files in the plugin directory; relative paths use `/` uniformly; the manifest is excluded,
/// sorted by name bytes. WHY: it must match `signing::digest_v2_for_dir`'s collection criteria item by item,
/// otherwise the directory digest and the actually packed content silently drift, and the packed artifact fails self-check.
pub fn collect_plugin_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    fn walk(root: &Path, cur: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
        let mut children: Vec<std::path::PathBuf> = std::fs::read_dir(cur)
            .map_err(|e| format!("failed to read directory {}: {}", cur.display(), e))?
            .filter_map(|r| r.ok().map(|e| e.path()))
            .collect();
        children.sort();
        for p in children {
            let meta = std::fs::symlink_metadata(&p)
                .map_err(|e| format!("stat {} failed: {}", p.display(), e))?;
            if meta.is_dir() {
                walk(root, &p, out)?;
                continue;
            }
            if !meta.is_file() {
                continue; // Non-regular files such as symlinks are not packed
            }
            let rel = p
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
            let bytes =
                std::fs::read(&p).map_err(|e| format!("failed to read {}: {}", p.display(), e))?;
            out.push((rel_str, bytes));
        }
        Ok(())
    }
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(out)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}
