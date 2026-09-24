use std::path::Path;

/// P2 — offline queue cap: the queue must not grow unbounded while the app is not running (over the cap, drop the oldest).
const MAX_QUEUED: usize = 5000;

pub fn enqueue(dir: &Path, payload: &str) -> std::io::Result<()> {
    enqueue_capped(dir, payload, MAX_QUEUED)
}

fn enqueue_capped(dir: &Path, payload: &str, cap: usize) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    // P2 — over the cap, drop the oldest (filenames start with a nanosecond timestamp, so lexicographic order ≈ time order).
    let mut existing: Vec<std::path::PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    if existing.len() >= cap {
        existing.sort();
        let drop_n = existing.len() - cap + 1;
        for p in existing.into_iter().take(drop_n) {
            let _ = std::fs::remove_file(p);
        }
    }
    // P2 — sequence backstop: two enqueues in the same nanosecond in the same process do not overwrite each other (the same idea as event ids).
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("{}-{}-{}.json", chrono_nanos(), std::process::id(), seq);
    std::fs::write(dir.join(name), payload)
}

pub fn drain(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        out.push(std::fs::read_to_string(&f)?);
        let _ = std::fs::remove_file(&f);
    }
    Ok(out)
}

fn chrono_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("opencapx-q-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn enqueue_then_drain_roundtrips_in_order() {
        let dir = tmpdir("order");
        enqueue(&dir, r#"{"a":1}"#).unwrap();
        enqueue(&dir, r#"{"a":2}"#).unwrap();
        let out = drain(&dir).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[0].contains('1'));
        assert!(out[1].contains('2'));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drain_clears_files() {
        let dir = tmpdir("clear");
        enqueue(&dir, r#"{"a":1}"#).unwrap();
        let _ = drain(&dir).unwrap();
        let out = drain(&dir).unwrap();
        assert!(out.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drain_missing_dir_errors() {
        let dir = std::env::temp_dir().join(format!(
            "opencapx-q-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        assert!(drain(&dir).is_err());
    }

    /// P2 — cap: over the cap, drop the oldest while staying at the cap; the sequence backstop keeps same-nanosecond writes from overwriting.
    #[test]
    fn enqueue_caps_and_drops_oldest() {
        let dir = tmpdir("cap");
        for i in 0..5 {
            enqueue_capped(&dir, &format!(r#"{{"n":{}}}"#, i), 5).unwrap();
        }
        enqueue_capped(&dir, r#"{"n":5}"#, 5).unwrap(); // triggers dropping the oldest
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            5,
            "stays at the cap"
        );
        let out = drain(&dir).unwrap();
        assert_eq!(out.len(), 5);
        assert!(
            out[0].contains("\"n\":1"),
            "oldest n=0 dropped: {:?}",
            out.first()
        );
        // Sequence backstop: multiple same-nanosecond writes do not overwrite each other
        for i in 0..3 {
            enqueue_capped(&dir, &format!(r#"{{"seq":{}}}"#, i), 100).unwrap();
        }
        assert_eq!(drain(&dir).unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
