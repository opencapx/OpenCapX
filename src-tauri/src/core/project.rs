//! Project metadata: git branch and short path.
//!
//! The bubble group header shows "project · branch", but neither should be read from disk by the frontend (the WebView has no
//! filesystem permission, and granting it would only enlarge the attack surface). The Core side reads only `<cwd>/.git/HEAD`,
//! without spawning git; results are cached per cwd for 30s — the bubble re-renders every second and cannot read disk each time.

use super::throttle::PerKeyThrottle;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Metadata for one working directory. When `branch` is None the frontend shows only the project name.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectMeta {
    pub cwd: String,
    /// Last two segments, `$HOME` → `~` (the full path never enters the DOM; only the short path is used in a title).
    pub short: String,
    pub branch: Option<String>,
}

/// Read `<cwd>/.git/HEAD`:
/// - `ref: refs/heads/x` → `x`
/// - detached (40 hex digits) → the first 7
/// - `.git` is a file (worktree / submodule) → follow `gitdir: <path>` and read that
/// - not git / path missing / no permission → None (silent, event handling unaffected)
pub fn branch_for(cwd: &str) -> Option<String> {
    let head = head_path(Path::new(cwd))?;
    let text = std::fs::read_to_string(head).ok()?;
    parse_head(text.trim())
}

fn head_path(cwd: &Path) -> Option<PathBuf> {
    let dot_git = cwd.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git.join("HEAD"));
    }
    // worktree / submodule: .git is a file whose content is `gitdir: /path/to/gitdir`
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let dir = text.trim().strip_prefix("gitdir:")?.trim();
    let dir = if Path::new(dir).is_absolute() {
        PathBuf::from(dir)
    } else {
        cwd.join(dir)
    };
    let head = dir.join("HEAD");
    head.exists().then_some(head)
}

fn parse_head(text: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix("ref:") {
        let name = rest.trim();
        let name = name.strip_prefix("refs/heads/").unwrap_or(name);
        return (!name.is_empty()).then(|| name.to_string());
    }
    // detached: 40 hex digits (either case) → short hash
    let is_sha = text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit());
    is_sha.then(|| text.chars().take(7).collect())
}

/// Short path: last two segments, `$HOME` replaced by `~`; prefix `…/` when trimming is needed.
pub fn short_path(cwd: &str) -> String {
    let home = crate::core::home_dir().map(|h| h.to_string_lossy().into_owned());
    short_path_with(cwd, home.as_deref())
}

/// Pure-function version (tests do not depend on a real $HOME).
pub fn short_path_with(cwd: &str, home: Option<&str>) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    let display = match home {
        Some(h) if cwd == h => "~".to_string(),
        Some(h) if cwd.starts_with(&format!("{h}/")) => format!("~/{}", &cwd[h.len() + 1..]),
        _ => cwd.to_string(),
    };
    let without_home = display.strip_prefix("~/").unwrap_or(&display);
    let segs: Vec<&str> = without_home.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() <= 2 {
        return display;
    }
    format!("…/{}", segs[segs.len() - 2..].join("/"))
}

fn cache() -> &'static PerKeyThrottle {
    static C: OnceLock<PerKeyThrottle> = OnceLock::new();
    C.get_or_init(|| PerKeyThrottle::new(30))
}

fn snapshot() -> &'static Mutex<HashMap<String, ProjectMeta>> {
    static S: OnceLock<Mutex<HashMap<String, ProjectMeta>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cached metadata. Hits the cache within the window; a cache miss (e.g. the process just started) re-reads once.
pub fn meta(cwd: &str, now: u64) -> ProjectMeta {
    if !cache().should_run(cwd, now) {
        if let Some(hit) = snapshot().lock().ok().and_then(|m| m.get(cwd).cloned()) {
            return hit;
        }
    }
    let fresh = probe(cwd);
    if let Ok(mut m) = snapshot().lock() {
        m.insert(cwd.to_string(), fresh.clone());
    }
    fresh
}

fn probe(cwd: &str) -> ProjectMeta {
    ProjectMeta {
        cwd: cwd.to_string(),
        short: short_path(cwd),
        branch: branch_for(cwd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("opencapx-project-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn repo_with_head(tag: &str, head: &str) -> PathBuf {
        let d = tmpdir(tag);
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join(".git").join("HEAD"), head).unwrap();
        d
    }

    #[test]
    fn branch_from_plain_repo() {
        let d = repo_with_head("plain", "ref: refs/heads/main\n");
        assert_eq!(branch_for(d.to_str().unwrap()), Some("main".to_string()));
    }

    #[test]
    fn branch_keeps_nested_name() {
        let d = repo_with_head("nested", "ref: refs/heads/feat/bubble-grouping\n");
        assert_eq!(
            branch_for(d.to_str().unwrap()),
            Some("feat/bubble-grouping".to_string())
        );
    }

    #[test]
    fn detached_head_becomes_short_hash() {
        let d = repo_with_head("detached", "c1f5b7d9a2e64a0d8b3f1c7e5a9d2b4f6c8e0a1d\n");
        assert_eq!(branch_for(d.to_str().unwrap()), Some("c1f5b7d".to_string()));
    }

    #[test]
    fn worktree_git_file_is_followed() {
        // worktree / submodule: .git is a file whose content is `gitdir: <path>`
        let main = repo_with_head("wt-main", "ref: refs/heads/trunk\n");
        let gitdir = main.join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/feat/wt\n").unwrap();
        let wt = tmpdir("wt");
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", gitdir.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(branch_for(wt.to_str().unwrap()), Some("feat/wt".to_string()));
    }

    #[test]
    fn non_repo_or_missing_path_is_none() {
        let d = tmpdir("nonrepo");
        assert_eq!(branch_for(d.to_str().unwrap()), None);
        assert_eq!(branch_for("/definitely/not/here"), None);
    }

    #[test]
    fn short_path_keeps_two_tail_segments() {
        let home = Some("/Users/me");
        assert_eq!(
            short_path_with("/Users/me/VScode/temp-workspace/OpenCapX", home),
            "…/temp-workspace/OpenCapX"
        );
        assert_eq!(
            short_path_with("/Users/me/VScode/OpenCapX", home),
            "~/VScode/OpenCapX"
        );
        assert_eq!(short_path_with("/Users/me", home), "~");
        assert_eq!(short_path_with("/opt/data", home), "/opt/data");
        assert_eq!(short_path_with("", home), "");
    }

    #[test]
    fn meta_is_cached_for_30s() {
        let d = repo_with_head("cache", "ref: refs/heads/main\n");
        let cwd = d.to_str().unwrap();
        assert_eq!(meta(cwd, 1000).branch.as_deref(), Some("main"));
        std::fs::write(d.join(".git").join("HEAD"), "ref: refs/heads/other\n").unwrap();
        // Within the window: cache hit (no disk read)
        assert_eq!(meta(cwd, 1010).branch.as_deref(), Some("main"));
        // Outside the window: re-read from disk
        assert_eq!(meta(cwd, 1040).branch.as_deref(), Some("other"));
    }
}
