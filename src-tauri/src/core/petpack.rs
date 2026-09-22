//! Pet pack.
//!
//! Directory layout `~/.opencapx/pets/<slug>/`:`pet.json` + sprite-sheet files. The manifest fields follow the existing
//! pet-pack convention (`id` / `displayName` / `description` / `spritesheetPath`), so that
//! off-the-shelf community packs can be dropped in directly.
//!
//! The sprite sheet **makes no grid assumption**: slicing auto-detects rows/columns on the render side from alpha gaps
//! (see `src/petpack.ts`), so both tidy 8×9 sheets and "AI-generated, unevenly spaced"
//! sheets can be sliced. This file only finds files, validates manifests, and reads sheets into data URLs on demand.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MANIFEST: &str = "pet.json";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PetPackInfo {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub description: String,
    /// `"2d"` (sprite sheet) or `"3d"` (glTF model).
    pub kind: String,
    /// Absolute path of the 2D sprite sheet (or an inline data URL); empty for a 3D pack.
    #[serde(rename = "sheetPath")]
    pub sheet_path: String,
    /// Absolute path of the 3D model; empty for a 2D pack.
    #[serde(rename = "modelPath")]
    pub model_path: String,
    /// State → animation clip name (3D). Empty = guess by order/name.
    pub clips: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Manifest {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "displayName", alias = "name")]
    display_name: String,
    #[serde(default)]
    description: String,
    #[serde(
        default,
        rename = "spritesheetPath",
        alias = "spritesheet",
        alias = "image",
        alias = "spritePath"
    )]
    sheet: String,
    /// 3D: model path (`.glb` / `.gltf`).
    #[serde(
        default,
        rename = "modelPath",
        alias = "model",
        alias = "glb",
        alias = "src"
    )]
    model: String,
    /// Explicitly declared kind; if absent, infer "3d when a model is present".
    #[serde(default)]
    kind: String,
    /// 3D: state → clip name (working / waiting / done / idle).
    #[serde(default)]
    clips: std::collections::HashMap<String, String>,
}

pub fn pets_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(".opencapx").join("pets");
    }
    std::env::temp_dir().join("opencapx-pets")
}

/// Directory name → stable id. Non-alphanumerics collapse to `-`; an empty string falls back to "pet".
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "pet".into()
    } else {
        out
    }
}

/// The sole file in the directory with the given extension (if there are several, do not guess).
fn sole_file_with(dir: &Path, exts: &[&str]) -> Option<String> {
    let mut found: Option<String> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        if exts.iter().any(|x| lower.ends_with(x)) {
            if found.is_some() {
                return None;
            }
            found = Some(name);
        }
    }
    found
}

/// Resolve an asset path: an inline data URL is returned as-is, otherwise it must exist inside the directory.
fn resolve_asset(dir: &Path, rel: &str, inline_prefix: &str, inline_limit: usize) -> Option<String> {
    if rel.starts_with(inline_prefix) {
        // Inline assets must not be path-joined — `dir.join("data:…")` yields a nonexistent path,
        // and the whole pack gets dropped (this is exactly why inline sheets did not show up before).
        return (rel.len() <= inline_limit).then(|| rel.to_string());
    }
    let p = dir.join(rel);
    p.is_file().then(|| p.to_string_lossy().into_owned())
}

/// 3D model extensions (VRM is itself a glTF container).
const MODEL_EXTS: &[&str] = &[".glb", ".gltf", ".vrm"];

/// Read a pack directory's manifest; asset paths must stay inside the directory (prevents the manifest from pointing outside).
fn read_pack(dir: &Path) -> Option<PetPackInfo> {
    let text = std::fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let m: Manifest = serde_json::from_str(&text).ok()?;

    // 3D: explicit kind=3d, or a modelPath is given, or the directory holds only glb/gltf
    let looks_3d = m.kind.eq_ignore_ascii_case("3d")
        || !m.model.trim().is_empty()
        || (m.sheet.trim().is_empty() && sole_file_with(dir, MODEL_EXTS).is_some());
    if looks_3d {
        let rel = if m.model.trim().is_empty() {
            sole_file_with(dir, MODEL_EXTS)?
        } else {
            m.model.clone()
        };
        let model_path = resolve_asset(dir, &rel, "data:", MAX_INLINE_MODEL_BYTES)?;
        return Some(PetPackInfo {
            id: pack_id(dir, &m)?,
            display_name: pack_name(dir, &m),
            description: m.description.clone(),
            kind: "3d".into(),
            sheet_path: String::new(),
            model_path,
            clips: m.clips.clone(),
        });
    }
    if m.kind.eq_ignore_ascii_case("2d") && m.sheet.trim().is_empty() && m.model.trim().is_empty() {
        return None;
    }
    // 2D: with no spritesheetPath, take the directory's sole image
    let sheet_rel = if m.sheet.trim().is_empty() {
        sole_file_with(dir, &[".png", ".webp"])?
    } else {
        m.sheet.clone()
    };
    let sheet_path = resolve_asset(dir, &sheet_rel, "data:image/", MAX_INLINE_SHEET_BYTES)?;
    Some(PetPackInfo {
        id: pack_id(dir, &m)?,
        display_name: pack_name(dir, &m),
        description: m.description.clone(),
        kind: "2d".into(),
        sheet_path,
        model_path: String::new(),
        clips: std::collections::HashMap::new(),
    })
}

/// Pack id: the manifest's id (slugified) takes priority, otherwise use the directory name.
fn pack_id(dir: &Path, m: &Manifest) -> Option<String> {
    let dir_name = dir.file_name()?.to_string_lossy().into_owned();
    Some(if m.id.trim().is_empty() {
        dir_name
    } else {
        slugify(&m.id)
    })
}

/// Display name: the manifest's displayName takes priority, otherwise use the directory name.
fn pack_name(dir: &Path, m: &Manifest) -> String {
    if m.display_name.trim().is_empty() {
        dir.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        m.display_name.clone()
    }
}

/// Whether the sheet is an inline data URL or an on-disk file path.
fn is_inline_sheet(sheet: &str) -> bool {
    sheet.starts_with("data:image/")
}

/// Upper bound for an inline sheet (in characters). Exceeding it marks the manifest invalid, avoiding parsing
/// a tens-of-MB base64 blob stuffed into pet.json on every manifest read.
const MAX_INLINE_SHEET_BYTES: usize = 4 * 1024 * 1024;

/// Upper bound for an inline model (base64 characters). 3D models are usually a few MB, so give more room but not unlimited.
const MAX_INLINE_MODEL_BYTES: usize = 12 * 1024 * 1024;

/// Model file cap. glTF with textures easily reaches tens of MB, so this is much looser than the sheet cap.
pub const MAX_MODEL_BYTES: u64 = 128 * 1024 * 1024;

/// List all available pet packs (sorted by directory name, keeping the UI stable).
pub fn list() -> Vec<PetPackInfo> {
    let dir = pets_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.iter().filter_map(|d| read_pack(d)).collect()
}

pub fn load(id: &str) -> Option<PetPackInfo> {
    if id.trim().is_empty() {
        return None;
    }
    read_pack(&pets_dir().join(id))
}

/// Install a pack directory. Use `pet.json` when present; when absent but the directory has a single model/image,
/// auto-generate a manifest (so a user can drag in "a folder containing a model" and it just works).
pub fn install_from_dir(src: &Path) -> Result<String, String> {
    if let Some(info) = read_pack(src) {
        let dest = pets_dir().join(&info.id);
        copy_dir(src, &dest)?;
        return Ok(info.id);
    }
    // No manifest: infer the kind
    let (kind, file) = if let Some(m) = sole_file_with(src, MODEL_EXTS) {
        ("3d", m)
    } else if let Some(i) = sole_file_with(src, &[".png", ".webp"]) {
        ("2d", i)
    } else {
        return Err("notAPack".into());
    };
    let stem = Path::new(&file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pet".into());
    let id = slugify(&stem);
    let dest = pets_dir().join(&id);
    copy_dir(src, &dest)?;
    let manifest = if kind == "3d" {
        serde_json::json!({"id": id, "displayName": stem, "description": "", "kind": "3d", "modelPath": file})
    } else {
        serde_json::json!({"id": id, "displayName": stem, "description": "", "spritesheetPath": file})
    };
    std::fs::write(
        dest.join(MANIFEST),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

/// Install from a single image: auto-generate pet.json (so a user can just drop in an image).
pub fn install_from_image(src: &Path, name: Option<&str>) -> Result<String, String> {
    let file_name = src
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or_else(|| "invalid image path".to_string())?;
    let stem = Path::new(&file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pet".into());
    let display = name.map(|s| s.to_string()).unwrap_or_else(|| stem.clone());
    let id = slugify(&display);
    let dest = pets_dir().join(&id);
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    std::fs::copy(src, dest.join(&file_name)).map_err(|e| e.to_string())?;
    let manifest = serde_json::json!({
        "id": id,
        "displayName": display,
        "description": "",
        "spritesheetPath": file_name,
    });
    std::fs::write(
        dest.join(MANIFEST),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

/// Install a pet pack from a 3D model (single-file `.glb` recommended; if a `.gltf` references external .bin/textures,
/// use "Import Directory" to bring the whole pack in).
pub fn install_from_model(src: &Path, name: Option<&str>) -> Result<String, String> {
    let file_name = src
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or_else(|| "invalid model path".to_string())?;
    let lower = file_name.to_ascii_lowercase();
    if !MODEL_EXTS.iter().any(|x| lower.ends_with(x)) {
        return Err("onlyGlb".into());
    }
    let stem = Path::new(&file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pet".into());
    let display = name.map(|s| s.to_string()).unwrap_or(stem);
    let id = slugify(&display);
    let dest = pets_dir().join(&id);
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    std::fs::copy(src, dest.join(&file_name)).map_err(|e| e.to_string())?;
    let manifest = serde_json::json!({
        "id": id,
        "displayName": display,
        "description": "",
        "kind": "3d",
        "modelPath": file_name,
    });
    std::fs::write(
        dest.join(MANIFEST),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

/// Read the raw 3D model bytes. `.glb` is binary; hand it to the front end's `GLTFLoader.parse(ArrayBuffer)`.
pub fn model_bytes(id: &str) -> Result<Vec<u8>, String> {
    let info = load(id).ok_or_else(|| format!("pet pack {id} does not exist"))?;
    if info.kind != "3d" {
        return Err(format!("pet pack {id} is not a 3D pack"));
    }
    if info.model_path.starts_with("data:") {
        let b64 = info
            .model_path
            .split_once(',')
            .map(|(_, b)| b)
            .ok_or_else(|| "malformed inline model".to_string())?;
        use base64::Engine;
        return base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| e.to_string());
    }
    let size = std::fs::metadata(&info.model_path)
        .map_err(|e| e.to_string())?
        .len();
    if size > MAX_MODEL_BYTES {
        return Err(format!("tooLarge:{}", size / (1024 * 1024)));
    }
    std::fs::read(&info.model_path).map_err(|e| e.to_string())
}

/// Read a 3D pack's additional assets (`.bin` / textures referenced by `.gltf`). `rel` must stay inside the pack directory.
pub fn model_asset(id: &str, rel: &str) -> Result<Vec<u8>, String> {
    let dir = pets_dir().join(slug_or_id(id)?);
    // Directory traversal: allow only relative paths inside the directory
    if rel.trim().is_empty()
        || rel.starts_with('/')
        || rel.starts_with('\\')
        || rel.contains("..")
        || rel.contains(':')
    {
        return Err("badAssetPath".into());
    }
    let path = dir.join(rel);
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > MAX_MODEL_BYTES {
        return Err(format!("tooLarge:{}", size / (1024 * 1024)));
    }
    std::fs::read(&path).map_err(|e| e.to_string())
}

/// id → directory name (strip any path components, preventing `../../x` from pointing elsewhere).
fn slug_or_id(id: &str) -> Result<String, String> {
    let name = Path::new(id)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or_else(|| "badPackId".to_string())?;
    if name.trim().is_empty() || name.contains("..") {
        return Err("badPackId".into());
    }
    Ok(name)
}

/// Download from a direct image URL and install it as a pet pack.
///
/// Why download on the Rust side instead of letting the WebView use `<img src>` directly:
/// 1. **No cross-origin restriction** — the front end cannot read remote image pixels, so slicing/thumbnails are impossible;
/// 2. **content-type can be checked first** — users easily paste a web-page URL (e.g. ordinals
///    `/inscription/<id>` returns text/html, while the direct image URL is actually `/content/<id>`);
/// 3. it lands locally once downloaded and then works offline, instead of depending on the network on every render.
///
/// Errors are returned as `code[:detail]`, and the front end renders the copy per language.
pub fn install_from_url(url: &str, name: Option<&str>) -> Result<String, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("badUrl".into());
    }
    let resp = reqwest::blocking::get(url).map_err(|e| {
        // Include the reason: network failures most need the scene; the UI only shows "download failed", so detail aids diagnosis
        let mut reason = e.to_string().replace('\n', " ");
        reason.truncate(80);
        format!("network:{reason}")
    })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("http:{}", status.as_u16()));
    }
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !ctype.starts_with("image/") {
        return Err(format!("notImage:{}", if ctype.is_empty() { "?" } else { &ctype }));
    }
    let bytes = resp.bytes().map_err(|_| "network".to_string())?;
    if bytes.len() as u64 > MAX_FETCH_BYTES {
        return Err(format!("tooLarge:{}", bytes.len() / (1024 * 1024)));
    }
    // After landing in a temp file, reuse the "install from a single image" path (auto-generates pet.json)
    let tmp = std::env::temp_dir().join(format!(
        "opencapx-fetch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let with_ext = tmp.with_extension(ext_for_content_type(&ctype));
    std::fs::write(&with_ext, &bytes).map_err(|e| e.to_string())?;
    // With no name given, derive one from the URL's last segment so the pet does not show up as a temp filename in the list
    let display = name.map(|s| s.to_string()).unwrap_or_else(|| name_from_url(url));
    let out = install_from_image(&with_ext, Some(&display));
    let _ = std::fs::remove_file(&with_ext);
    out
}

/// When no name is given, derive a readable display name from the URL's last segment (drop the extension + truncate).
pub fn name_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let after_scheme = path.split("://").nth(1).unwrap_or(path);
    let (host, rest) = match after_scheme.split_once('/') {
        Some((h, r)) => (h, r),
        None => (after_scheme, ""),
    };
    // Path present → take the filename and drop the extension; no path → use the whole host name
    let seg = rest.rsplit('/').find(|s| !s.is_empty()).unwrap_or(host);
    let stem = if rest.is_empty() {
        seg.to_string()
    } else {
        seg.split('.').next().unwrap_or(seg).to_string()
    };
    let cut: String = stem.chars().take(24).collect();
    if cut.trim().is_empty() {
        "remote".to_string()
    } else {
        cut
    }
}


/// Download size cap: enough for pet sheets, preventing a URL from dragging in hundreds of MB.
const MAX_FETCH_BYTES: u64 = 32 * 1024 * 1024;

/// content-type → extension (used by install_from_image when writing the file).
fn ext_for_content_type(ctype: &str) -> &'static str {
    match ctype {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/avif" => "avif",
        _ => "png",
    }
}

pub fn delete(id: &str) -> Result<(), String> {
    let dir = pets_dir().join(id);
    if !dir.is_dir() {
        return Err(format!("pet pack {id} does not exist"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())
}

/// Read the sheet into a data URL for the WebView to render.
///
/// Use a data URL rather than the asset protocol: one fewer protocol/scope config, and pet sheets are only a few hundred KB.
pub fn sheet_data_url(id: &str) -> Result<String, String> {
    let info = load(id).ok_or_else(|| format!("pet pack {id} does not exist"))?;
    // Inline sheet: already a data URL, pass it through directly (no disk access)
    if is_inline_sheet(&info.sheet_path) {
        return Ok(info.sheet_path);
    }
    let bytes = std::fs::read(&info.sheet_path).map_err(|e| e.to_string())?;
    let mime = if info.sheet_path.to_ascii_lowercase().ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    };
    Ok(format!(
        "data:{mime};base64,{}",
        base64_encode(&bytes)
    ))
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn copy_dir(src: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            copy_dir(&path, &dest.join(name))?;
        } else {
            std::fs::copy(&path, dest.join(name)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Each test gets its own HOME to avoid stepping on each other.
    fn with_home<T>(tag: &str, f: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("opencapx-pets-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::env::var("HOME").ok();
        std::env::set_var("HOME", &dir);
        let out = f();
        if let Some(h) = old {
            std::env::set_var("HOME", h);
        }
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    fn write_pack(root: &Path, slug: &str, manifest: &str, sheet: &[u8]) {
        let dir = root.join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join(MANIFEST)).unwrap();
        f.write_all(manifest.as_bytes()).unwrap();
        std::fs::write(dir.join("sprite.png"), sheet).unwrap();
    }

    #[test]
    fn slugify_is_stable_and_readable() {
        assert_eq!(slugify("Boba The Panda!"), "boba-the-panda");
        assert_eq!(slugify("  "), "pet");
        assert_eq!(slugify("a_b/c"), "a-b-c");
    }

    #[test]
    fn list_and_load_packs() {
        with_home("list", || {
            let root = pets_dir();
            write_pack(
                &root,
                "boba",
                r#"{"id":"boba","displayName":"Boba","description":"panda","spritesheetPath":"sprite.png"}"#,
                b"\x89PNG-fake",
            );
            let packs = list();
            assert_eq!(packs.len(), 1);
            assert_eq!(packs[0].id, "boba");
            assert_eq!(packs[0].display_name, "Boba");
            assert!(packs[0].sheet_path.ends_with("sprite.png"));
            assert!(load("boba").is_some());
            assert!(load("nope").is_none());
        });
    }

    #[test]
    fn pack_without_sheet_path_falls_back_to_the_only_image() {
        with_home("fallback", || {
            let root = pets_dir();
            write_pack(&root, "solo", r#"{"displayName":"Solo"}"#, b"img");
            let packs = list();
            assert_eq!(packs.len(), 1);
            assert_eq!(packs[0].id, "solo");
            assert_eq!(packs[0].display_name, "Solo");
        });
    }

    #[test]
    fn broken_manifests_are_skipped_not_fatal() {
        with_home("broken", || {
            let root = pets_dir();
            // No pet.json
            std::fs::create_dir_all(root.join("empty")).unwrap();
            // Manifest points to a file outside the directory
            write_pack(
                &root,
                "escape",
                r#"{"displayName":"Escape","spritesheetPath":"../../etc/hosts"}"#,
                b"img",
            );
            // Sheet missing
            let d = root.join("nosheet");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(MANIFEST), r#"{"spritesheetPath":"missing.png"}"#).unwrap();
            assert!(list().is_empty());
        });
    }

    #[test]
    fn three_d_pack_is_listed_with_clips_and_model_path() {
        with_home("pack3d", || {
            let dir = pets_dir().join("fox3d");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(MANIFEST),
                r#"{"id":"fox3d","displayName":"Fox","kind":"3d","modelPath":"fox.glb",
                    "clips":{"working":"Run","waiting":"Survey","idle":"Walk"}}"#,
            )
            .unwrap();
            std::fs::write(dir.join("fox.glb"), b"glTF-fake").unwrap();
            let packs = list();
            assert_eq!(packs.len(), 1);
            let p = &packs[0];
            assert_eq!(p.kind, "3d");
            assert!(p.sheet_path.is_empty());
            assert!(p.model_path.ends_with("fox.glb"));
            assert_eq!(p.clips.get("working").map(String::as_str), Some("Run"));
            // Bytes are read out as-is (starting with the glTF magic number)
            assert_eq!(model_bytes("fox3d").unwrap(), b"glTF-fake");
            // The 2D read path errors on it rather than returning an empty sheet
            assert!(sheet_data_url("fox3d").is_err());
        });
    }

    #[test]
    fn three_d_pack_without_a_model_file_is_skipped() {
        with_home("pack3dbad", || {
            let dir = pets_dir().join("ghost");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(MANIFEST),
                r#"{"kind":"3d","modelPath":"missing.glb"}"#,
            )
            .unwrap();
            assert!(list().is_empty(), "a 3D pack with a missing model should be dropped");
        });
    }

    #[test]
    fn kind_is_inferred_from_a_lone_model_file() {
        with_home("infer3d", || {
            let dir = pets_dir().join("solo");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(MANIFEST), r#"{"displayName":"Solo Model"}"#).unwrap();
            std::fs::write(dir.join("pet.glb"), b"glTF").unwrap();
            let packs = list();
            assert_eq!(packs.len(), 1);
            assert_eq!(packs[0].kind, "3d", "with only a glb, it should be inferred as a 3D pack");
            assert_eq!(packs[0].display_name, "Solo Model");
        });
    }

    #[test]
    fn install_from_dir_without_manifest_generates_one_for_a_model() {
        with_home("installdir3d", || {
            let src = std::env::temp_dir().join(format!("opencapx-modelsrc-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&src);
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(src.join("My Fox.glb"), b"glTF").unwrap();
            let id = install_from_dir(&src).unwrap();
            assert_eq!(id, "my-fox");
            let packs = list();
            assert_eq!(packs.len(), 1);
            assert_eq!(packs[0].kind, "3d");
            assert!(packs[0].model_path.ends_with("My Fox.glb"));
            let _ = std::fs::remove_dir_all(&src);
        });
    }

    #[test]
    fn install_from_model_rejects_non_model_files() {
        with_home("installmodel", || {
            let src = std::env::temp_dir().join(format!("opencapx-img-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&src);
            std::fs::create_dir_all(&src).unwrap();
            let img = src.join("a.png");
            std::fs::write(&img, b"png").unwrap();
            assert_eq!(install_from_model(&img, None), Err("onlyGlb".into()));
            let m = src.join("fox.glb");
            std::fs::write(&m, b"glTF").unwrap();
            assert_eq!(install_from_model(&m, None).unwrap(), "fox");
            let _ = std::fs::remove_dir_all(&src);
        });
    }

    #[test]
    fn inline_data_url_sheet_is_listed_and_passed_through() {
        with_home("inline", || {
            let root = pets_dir();
            let dir = root.join("inline");
            std::fs::create_dir_all(&dir).unwrap();
            // Hand-made pack: the sheet is inlined directly in pet.json (previously dropped because dir.join() built a fake path)
            std::fs::write(
                dir.join(MANIFEST),
                r#"{"id":"inline","displayName":"Inline","spritesheetPath":"data:image/png;base64,AAAA"}"#,
            )
            .unwrap();
            let packs = list();
            assert_eq!(packs.len(), 1, "a pack with an inline sheet must be listable");
            assert_eq!(packs[0].sheet_path, "data:image/png;base64,AAAA");
            // On read it is passed through as-is, without touching disk
            assert_eq!(
                sheet_data_url("inline").unwrap(),
                "data:image/png;base64,AAAA"
            );
        });
    }

    #[test]
    fn oversized_inline_sheet_is_rejected() {
        with_home("inlinebig", || {
            let root = pets_dir();
            let dir = root.join("big");
            std::fs::create_dir_all(&dir).unwrap();
            let huge = format!(
                r#"{{"displayName":"Big","spritesheetPath":"data:image/png;base64,{}"}}"#,
                "A".repeat(MAX_INLINE_SHEET_BYTES + 8)
            );
            std::fs::write(dir.join(MANIFEST), huge).unwrap();
            assert!(list().is_empty(), "an oversized inline sheet should be judged an invalid manifest");
        });
    }

    #[test]
    fn install_from_dir_copies_and_install_from_image_generates_manifest() {
        with_home("install", || {
            // Source directory is outside
            let src = std::env::temp_dir().join(format!("opencapx-src-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&src);
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(
                src.join(MANIFEST),
                r#"{"id":"Neo Pet","displayName":"Neo","spritesheetPath":"sprite.png"}"#,
            )
            .unwrap();
            std::fs::write(src.join("sprite.png"), b"png").unwrap();
            let id = install_from_dir(&src).unwrap();
            assert_eq!(id, "neo-pet", "the id must be slugified");
            assert!(list().iter().any(|p| p.id == "neo-pet"));

            // Single image → auto-generate a manifest
            let img = src.join("dropped.png");
            std::fs::write(&img, b"png").unwrap();
            let id2 = install_from_image(&img, Some("My Cat")).unwrap();
            assert_eq!(id2, "my-cat");
            let packs = list();
            let cat = packs.iter().find(|p| p.id == "my-cat").unwrap();
            assert_eq!(cat.display_name, "My Cat");

            // Delete
            delete("my-cat").unwrap();
            assert!(!list().iter().any(|p| p.id == "my-cat"));
            assert!(delete("my-cat").is_err());
            let _ = std::fs::remove_dir_all(&src);
        });
    }

    #[test]
    fn url_import_rejects_non_http_and_bad_scheme() {
        // Only http(s) is accepted; local paths/file:// are always rejected
        assert_eq!(install_from_url("/tmp/x.png", None), Err("badUrl".into()));
        assert_eq!(
            install_from_url("file:///tmp/x.png", None),
            Err("badUrl".into())
        );
    }

    #[test]
    fn name_from_url_takes_last_segment_without_extension() {
        assert_eq!(name_from_url("https://x.dev/pets/boba.png"), "boba");
        assert_eq!(name_from_url("https://ordinals.com/content/ab12cd34i0"), "ab12cd34i0");
        assert_eq!(name_from_url("https://x.dev/a/b/c.webp?raw=1#f"), "c");
        assert_eq!(name_from_url("https://x.dev/"), "x.dev");
        // Truncate over-long segments so the list does not blow up
        let long = format!("https://x.dev/{}", "y".repeat(80));
        assert_eq!(name_from_url(&long).chars().count(), 24);
    }

    #[test]
    fn content_type_maps_to_extension() {
        assert_eq!(ext_for_content_type("image/jpeg"), "jpg");
        assert_eq!(ext_for_content_type("image/avif"), "avif");
        assert_eq!(ext_for_content_type("image/webp"), "webp");
        // Unknown/empty → write as png (decoding relies on content, not the extension, anyway)
        assert_eq!(ext_for_content_type(""), "png");
        assert_eq!(ext_for_content_type("image/png"), "png");
    }

    #[test]
    fn sheet_data_url_is_base64_png() {
        with_home("dataurl", || {
            let root = pets_dir();
            write_pack(&root, "p", r#"{"spritesheetPath":"sprite.png"}"#, b"abc");
            let url = sheet_data_url("p").unwrap();
            assert!(url.starts_with("data:image/png;base64,"), "got {url}");
            // "abc" → "YWJj"
            assert!(url.ends_with("YWJj"));
            assert!(sheet_data_url("missing").is_err());
        });
    }
}
