//! v1.2 clipboard builtin provider (clipboard.read / clipboard.write, docs/capability.md "builtin providers").
//! arboard cross-platform (macOS NSPasteboard / Windows Win32 / Linux X11 + Wayland),
//! replacing the v1.1 pbcopy/xclip/clip platform-command subprocesses; `get_image` is normalized to
//! RGBA on every platform (on Windows done by the image decoder + into_rgba8), with no manual channel swapping.
//! Image clipboard: PNG lands in `~/.opencapx/cache/` (reusing vision::cache_dir).

use arboard::Clipboard;
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

/// Process-level clipboard lock: concurrent access to arboard's macOS backend (NSPasteboard) crashes
/// (ObjC runtime abort). Multi-threaded /rpc calls and parallel tests must all serialize through here.
/// pub(crate): capability.rs tests that use arboard directly to save/restore the clipboard must hold it too.
pub(crate) fn clipboard_lock_guard() -> std::sync::MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

/// For the settings page "copy path": the UI writes text straight into the system clipboard (bypassing the Agent permission gate).
/// Same arboard path as clipboard.write (same lock, preventing macOS concurrent abort).
pub fn write_text(text: &str) -> Result<(), String> {
    let _guard = clipboard_lock_guard();
    let mut cb = Clipboard::new().map_err(|e| format!("clipboard unavailable: {}", e))?;
    cb.set_text(text.to_string())
        .map_err(|e| format!("clipboard write failed: {}", e))?;
    Ok(())
}

/// clipboard.write: text into the system clipboard. Output matches the v1.1 subprocess version, `{ok: true}`.
/// The write itself is delegated to write_text — there is only one arboard write path globally.
pub fn write(input: &Value) -> Result<Value, String> {
    let Some(text) = input.get("text").and_then(|t| t.as_str()) else {
        return Err("invalid input: text (string) required".into());
    };
    write_text(text)?;
    Ok(json!({ "ok": true }))
}

/// clipboard.read: text first; when the clipboard holds an image, save the PNG and return the path.
/// Neither available = empty clipboard, returning empty text (empty is not an error).
pub fn read(_input: &Value) -> Result<Value, String> {
    let _guard = clipboard_lock_guard();
    let mut cb = Clipboard::new().map_err(|e| format!("clipboard unavailable: {}", e))?;
    match cb.get_text() {
        Ok(text) => Ok(json!({ "text": text })),
        Err(_) => match cb.get_image() {
            Ok(img) => {
                let (w, h) = (img.width as u32, img.height as u32);
                let rgba = image::RgbaImage::from_raw(w, h, img.bytes.into_owned())
                    .ok_or("clipboard image has inconsistent dimensions")?;
                let dir = super::vision::cache_dir()?;
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let out = dir.join(format!("clipboard-{}.png", nanos));
                let out_str = out.to_string_lossy().to_string();
                rgba.save(&out)
                    .map_err(|e| format!("clipboard image save failed: {}", e))?;
                Ok(json!({ "text": "", "image": out_str }))
            }
            // Neither text nor image available: empty clipboard, honestly return empty text
            Err(_) => Ok(json!({ "text": "" })),
        },
    }
}

/// Lightweight text read reused by context.get_current (serialized on the same lock).
pub fn text() -> Option<String> {
    let _guard = clipboard_lock_guard();
    Clipboard::new().ok().and_then(|mut c| c.get_text().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Validation style matches existing builtins: a missing field gives an invalid input error.
    #[test]
    fn write_requires_text() {
        let err = write(&json!({})).unwrap_err();
        assert!(err.contains("text"), "{}", err);
    }

    /// The clipboard may be unavailable in headless environments: the error must be clipboard unavailable, not a panic.
    #[test]
    fn read_never_panics() {
        let _ = read(&json!({}));
    }

    /// UI write → existing text() read-back. Holds TEST_STORE_LOCK to serialize: the clipboard cases in capability.rs
    /// hold it too, avoiding interleaved real reads/writes (cannot hold clipboard_lock_guard outside —
    /// write_text/text each take the same non-reentrant lock internally, which would deadlock).
    #[test]
    fn write_text_roundtrips_through_text() {
        // arboard's Linux backend needs X11/Wayland; headless CI has no display, so skip then
        // (the macOS/Windows clipboard is always present, so do not skip).
        #[cfg(target_os = "linux")]
        if std::env::var_os("WAYLAND_DISPLAY").is_none()
            && std::env::var_os("DISPLAY").is_none()
        {
            eprintln!("skipping clipboard roundtrip: headless Linux, no display");
            return;
        }
        let _g = crate::core::TEST_STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let unique = format!("opencapx-ui-clip-{}", std::process::id());
        write_text(&unique).expect("write_text");
        assert_eq!(text().as_deref(), Some(unique.as_str()));
    }
}
