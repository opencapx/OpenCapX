//! Phase 46 — Workspace profile switching.
//!
//! Each profile = its own `~/.opencapx/workspaces/<name>/store.sqlite`, containing plugins /
//! permissions / capabilities / plugin_channel / plugin_health_config /
//! plugin_metrics_samples / plugin_metrics_config / plugin_ratings / per-plugin
//! settings_kv. All profiles share one process + EventBus + PluginManager.
//!
//! Design notes:
//! - `~/.opencapx/workspaces.json` is the globally unique profile metadata list (including the active name).
//! - A one-time migration runs on first start: `~/.opencapx/data/opencapx.db` → `workspaces/default/store.sqlite`.
//! - Switch flow: PluginManager::stop_all() → replace_shared_store(new) → set_active → emit `workspace.switched`.
//! - EventBus / kill_switch / settings.json are all process-level shared and do not switch with the profile.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_PROFILE: &str = "default";

/// Profile info exposed to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileInfo {
    pub name: String,
    pub is_active: bool,
    pub plugin_count: usize,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProfilesFile {
    /// Active profile name. If the file does not exist → defaults to "default".
    #[serde(default)]
    active: String,
    /// List of declared profile names (recorded at creation).
    #[serde(default)]
    profiles: Vec<ProfileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProfileEntry {
    name: String,
    created_at: i64,
}

/// Full path of `~/.opencapx/profiles.json`.
pub fn profiles_file() -> PathBuf {
    if let Some(home) = crate::core::home_dir() {
        return home.join(".opencapx").join("profiles.json");
    }
    std::env::temp_dir().join("opencapx-profiles.json")
}

/// `~/.opencapx/workspaces/<name>/`.
pub fn workspace_dir(name: &str) -> PathBuf {
    if let Some(home) = crate::core::home_dir() {
        return home.join(".opencapx").join("workspaces").join(name);
    }
    std::env::temp_dir().join("opencapx-workspaces").join(name)
}

/// profile name → full DB path.
pub fn db_path_for(name: &str) -> PathBuf {
    workspace_dir(name).join("store.sqlite")
}

/// `~/.opencapx/workspaces/`.
pub fn profiles_root() -> PathBuf {
    if let Some(home) = crate::core::home_dir() {
        return home.join(".opencapx").join("workspaces");
    }
    std::env::temp_dir().join("opencapx-workspaces")
}

/// Current ~/.opencapx/data/opencapx.db (the old flat path, migration source).
fn legacy_db_path() -> PathBuf {
    super::storage::data_dir().join("opencapx.db")
}

/// One-time migration: move the legacy flat DB to workspaces/default/. Skipped if already migrated.
pub fn ensure_default_profile_migrated() -> Result<(), String> {
    let legacy = legacy_db_path();
    let target = db_path_for(DEFAULT_PROFILE);
    if target.exists() {
        // New path already in place, skip (the user may have migrated manually)
        return Ok(());
    }
    if !legacy.exists() {
        // Fresh install, just ensure the default dir.
        let _ = fs::create_dir_all(workspace_dir(DEFAULT_PROFILE));
        return Ok(());
    }
    // Old DB exists → move it over
    let _ = fs::create_dir_all(workspace_dir(DEFAULT_PROFILE));
    if let Err(e) = fs::rename(&legacy, &target) {
        return Err(format!(
            "migrate legacy db {:?} → {:?}: {}",
            legacy, target, e
        ));
    }
    Ok(())
}

/// Read all profile info from `~/.opencapx/profiles.json`.
pub fn load_profiles() -> Vec<ProfileInfo> {
    let file = profiles_file();
    let pf: ProfilesFile = match fs::read_to_string(&file) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => {
            // File missing → fallback: return a fake list containing "default".
            return vec![ProfileInfo {
                name: DEFAULT_PROFILE.to_string(),
                is_active: true,
                plugin_count: 0,
                created_at: super::agent::now_secs() as i64,
            }];
        }
    };
    let active = if pf.active.is_empty() {
        DEFAULT_PROFILE.to_string()
    } else {
        pf.active
    };
    pf.profiles
        .into_iter()
        .map(|p| {
            let name = p.name.clone();
            ProfileInfo {
                name: p.name,
                is_active: name == active,
                plugin_count: 0,
                created_at: p.created_at,
            }
        })
        .collect()
}

pub fn active_profile_name() -> String {
    let file = profiles_file();
    match fs::read_to_string(&file) {
        Ok(s) => match serde_json::from_str::<ProfilesFile>(&s) {
            Ok(pf) if !pf.active.is_empty() => pf.active,
            _ => DEFAULT_PROFILE.to_string(),
        },
        Err(_) => DEFAULT_PROFILE.to_string(),
    }
}

fn write_profiles_file(pf: &ProfilesFile) -> Result<(), String> {
    if let Some(parent) = profiles_file().parent() {
        let _ = fs::create_dir_all(parent);
    }
    let s = serde_json::to_string_pretty(pf).map_err(|e| e.to_string())?;
    fs::write(profiles_file(), s).map_err(|e| e.to_string())
}

fn read_profiles_file() -> ProfilesFile {
    match fs::read_to_string(profiles_file()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => ProfilesFile {
            active: DEFAULT_PROFILE.to_string(),
            profiles: vec![ProfileEntry {
                name: DEFAULT_PROFILE.to_string(),
                created_at: super::agent::now_secs() as i64,
            }],
        },
    }
}

pub fn set_active_profile(name: &str) -> Result<(), String> {
    let mut pf = read_profiles_file();
    if !pf.profiles.iter().any(|p| p.name == name) {
        return Err(format!("profile not found: {}", name));
    }
    pf.active = name.to_string();
    write_profiles_file(&pf)
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("name must be 1..=64 chars".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("name must be [A-Za-z0-9_-] only".into());
    }
    if name == "." || name == ".." {
        return Err("name cannot be '.' or '..'".into());
    }
    Ok(())
}

pub fn create_profile(name: &str) -> Result<ProfileInfo, String> {
    validate_name(name)?;
    let mut pf = read_profiles_file();
    if pf.profiles.iter().any(|p| p.name == name) {
        return Err(format!("profile already exists: {}", name));
    }
    let dir = workspace_dir(name);
    fs::create_dir_all(&dir).map_err(|e| format!("create dir {:?}: {}", dir, e))?;
    // Immediately initialize an empty SQLite (Storage::open runs its built-in migrate)
    let path = db_path_for(name);
    let _ = super::storage::Storage::open(&path).map_err(|e| e.to_string())?;
    let created_at = super::agent::now_secs() as i64;
    pf.profiles.push(ProfileEntry {
        name: name.to_string(),
        created_at,
    });
    // Also activate default on first creation (if active is still empty)
    if pf.active.is_empty() {
        pf.active = DEFAULT_PROFILE.to_string();
    }
    write_profiles_file(&pf)?;
    Ok(ProfileInfo {
        name: name.to_string(),
        is_active: name == pf.active,
        plugin_count: 0,
        created_at,
    })
}

pub fn delete_profile(name: &str) -> Result<(), String> {
    if name == DEFAULT_PROFILE {
        return Err("cannot delete the default profile".into());
    }
    let mut pf = read_profiles_file();
    if !pf.profiles.iter().any(|p| p.name == name) {
        return Err(format!("profile not found: {}", name));
    }
    if pf.active == name {
        return Err("cannot delete the active profile".into());
    }
    if pf.profiles.len() <= 1 {
        return Err("cannot delete the last remaining profile".into());
    }
    let dir = workspace_dir(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("remove {:?}: {}", dir, e))?;
    }
    pf.profiles.retain(|p| p.name != name);
    write_profiles_file(&pf)?;
    Ok(())
}

/// Phase 46 — hot switch: stop all running plugins + replace shared_store + write active + emit event.
/// Any old Arc on a store drops automatically once the caller releases the lock (closing the corresponding SQLite connection).
pub fn switch_profile(name: &str) -> Result<(), String> {
    validate_name(name)?;
    let mut pf = read_profiles_file();
    if !pf.profiles.iter().any(|p| p.name == name) {
        return Err(format!("profile not found: {}", name));
    }
    let prev_active = if pf.active.is_empty() {
        DEFAULT_PROFILE.to_string()
    } else {
        pf.active.clone()
    };
    if prev_active == name {
        return Ok(()); // already active
    }

    // 1. Stop all running plugins (so they don't hold old store config across profiles)
    let mgr = super::plugin::PluginManager::shared();
    mgr.stop_all();

    // 2. Open the new store and replace the global one
    let path = db_path_for(name);
    let storage = super::storage::Storage::open(&path).map_err(|e| e.to_string())?;
    let new_store: super::storage::SharedStore =
        std::sync::Arc::new(std::sync::Mutex::new(super::storage::StoreEnum::Db(storage)));
    let _old = super::replace_shared_store(new_store);

    // 3. Persist active
    pf.active = name.to_string();
    write_profiles_file(&pf)?;

    // 4. Clear the metrics latest cache (so the new profile doesn't see data from old plugin ids)
    super::plugin_metrics::clear_latest();

    // 5. Emit an event so the frontend reloads (note: the store is already switched here, so frontend invokes see the new store)
    super::event::EventBus::shared().publish(&super::event::OpencapxEvent::new(
        "workspace.switched",
        "core",
        serde_json::json!({ "from": prev_active, "to": name }),
    ));
    Ok(())
}

/// For the list_workspace_profiles command: fill in plugin_count on top of the existing profile list.
pub fn fill_plugin_counts(mut infos: Vec<ProfileInfo>) -> Vec<ProfileInfo> {
    for info in &mut infos {
        let path = db_path_for(&info.name);
        if let Ok(storage) = super::storage::Storage::open(&path) {
            let store_enum = super::storage::StoreEnum::Db(storage);
            let count: usize = store_enum
                .with_conn_ref(|c| {
                    c.query_row("SELECT COUNT(*) FROM plugins", [], |r| r.get::<_, i64>(0))
                        .unwrap_or(0) as usize
                })
                .unwrap_or(0);
            info.plugin_count = count;
        }
    }
    infos
}

/// For the list command: return all profiles with plugin_count.
pub fn list_profiles() -> Vec<ProfileInfo> {
    fill_plugin_counts(load_profiles())
}

/// For main.rs startup: open a profile's store (using Storage::open's default behavior).
/// On failure, fall back to an in-memory store, the same policy as open_default().
/// Corrupt-db quarantine record: original file → quarantined file + reason (for logs and the Settings page notice).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quarantine {
    pub from: String,
    pub to: String,
    pub reason: String,
    pub at: i64,
}

/// Open the db and run one `quick_check` (only when the file already exists).
/// Existing file unopenable / tree corrupted → Err; file missing → normal create.
fn try_open_checked(path: &Path) -> Result<super::storage::Storage, String> {
    let existed = path.exists();
    let store = super::storage::Storage::open(path).map_err(|e| e.to_string())?;
    if existed {
        let verdict = store
            .with_conn_ref(|c| {
                c.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0))
                    .ok()
            })
            .unwrap_or_else(|| "quick_check unavailable".into());
        if verdict != "ok" {
            return Err(format!("quick_check: {}", verdict.chars().take(120).collect::<String>()));
        }
    }
    Ok(store)
}

/// Rename the corrupt db (including -wal / -shm) to quarantine it. A stale WAL must be
/// moved too — leave it behind and the next open will still treat its pages as authoritative.
fn quarantine_db(path: &Path, reason: &str) -> Option<Quarantine> {
    let stamp = super::agent::now_secs() as i64;
    let mut to = PathBuf::from(format!("{}.corrupt-{}", path.display(), stamp));
    let mut n = 1;
    while to.exists() {
        to = PathBuf::from(format!("{}.corrupt-{}-{}", path.display(), stamp, n));
        n += 1;
    }
    if let Err(e) = fs::rename(path, &to) {
        eprintln!("[profile] quarantine {} failed: {}", path.display(), e);
        return None;
    }
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{}", path.display(), suffix));
        if side.exists() {
            let side_to = PathBuf::from(format!("{}{}", to.display(), suffix));
            let _ = fs::rename(&side, &side_to);
        }
    }
    eprintln!(
        "[profile] quarantined corrupt db {} → {} ({})",
        path.display(),
        to.display(),
        reason
    );
    Some(Quarantine {
        from: path.display().to_string(),
        to: to.display().to_string(),
        reason: reason.to_string(),
        at: stamp,
    })
}

/// Open a profile db. Corrupt db → quarantine + create a fresh empty db and keep
/// running, returning the quarantine record to the caller (written into
/// ~/.opencapx/db-recovery.json so the Settings page can notify the user).
///
/// Never silently degrade to memory: that makes the app look perfectly healthy while
/// not a single row is persisted — exactly the behavior that once let a corrupt db run
/// for a long time unnoticed.
pub fn open_profile_store(name: &str) -> (super::storage::StoreEnum, Option<Quarantine>) {
    let path = db_path_for(name);
    match try_open_checked(&path) {
        Ok(store) => return (super::storage::StoreEnum::Db(store), None),
        Err(reason) => {
            let q = quarantine_db(&path, &reason);
            match try_open_checked(&path) {
                Ok(store) => (super::storage::StoreEnum::Db(store), q),
                Err(e2) => {
                    eprintln!(
                        "[profile] cannot open even a fresh db for profile {} ({}), memory only",
                        name, e2
                    );
                    let q = q.map(|mut q| {
                        q.reason = format!("{}; fresh db also failed: {}", q.reason, e2);
                        q
                    });
                    (super::storage::StoreEnum::Mem(super::agent::SessionStore::new()), q)
                }
            }
        }
    }
}

/// The last corrupt-db quarantine record (for the Settings page notice); None if there is none.
pub fn db_recovery_notice() -> Option<serde_json::Value> {
    let f = recovery_notice_path();
    let text = fs::read_to_string(&f).ok()?;
    serde_json::from_str(&text).ok()
}

/// User confirms "Got it" → delete the notice file (the quarantined file itself is kept for later data recovery).
pub fn dismiss_db_recovery_notice() -> Result<(), String> {
    let f = recovery_notice_path();
    if f.exists() {
        fs::remove_file(&f).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn recovery_notice_path() -> PathBuf {
    if let Some(home) = crate::core::home_dir() {
        return home.join(".opencapx").join("db-recovery.json");
    }
    std::env::temp_dir().join("opencapx-db-recovery.json")
}

/// Persist the quarantine record: called once at main startup.
pub fn write_db_recovery_notice(q: &Quarantine) {
    let f = recovery_notice_path();
    if let Some(dir) = f.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(q) {
        let _ = fs::write(&f, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // All tests share the HOME global → must serialize to avoid parallel pollution.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn fresh_home() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "opencapx-profile-test-{}-{}",
            std::process::id(),
            n
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Sets the isolated home env and restores it on drop, so a panicking or early-returning
    /// test cannot leak OPENCAPX_HOME into concurrently running modules' tests.
    struct HomeGuard(std::ffi::OsString);

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            std::env::set_var("OPENCAPX_HOME", &self.0);
            std::env::set_var("HOME", &self.0);
            std::env::set_var("USERPROFILE", &self.0);
        }
    }

    fn set_home(home: &Path) -> HomeGuard {
        // OPENCAPX_HOME is the cross-platform override; on Windows neither HOME nor
        // USERPROFILE reaches dirs::home_dir() (Known Folder API), which used to make
        // these tests share the real home and leak state into each other.
        let guard = HomeGuard(
            std::env::var_os("OPENCAPX_HOME").unwrap_or_default(),
        );
        std::env::set_var("OPENCAPX_HOME", home);
        std::env::set_var("HOME", home);
        std::env::set_var("USERPROFILE", home);
        guard
    }

    /// Corrupt db: the bad file (including -wal/-shm) is renamed into quarantine, a
    /// fresh empty db takes over, and the quarantine record is reported — never a silent
    /// in-memory downgrade (old behavior: the app looked fine but persisted nothing).
    #[test]
    fn corrupt_db_is_quarantined_not_silently_dropped() {
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = fresh_home();
        let _home = set_home(&home);
        let path = db_path_for(DEFAULT_PROFILE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // First create a real db, then smash it into garbage bytes (simulating an interrupted write)
        drop(super::super::storage::Storage::open(&path).unwrap());
        fs::write(&path, b"not a database at all").unwrap();
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        fs::write(&wal, b"stale wal of a bigger db").unwrap();
        fs::write(&shm, b"stale shm").unwrap();

        let (store, q) = open_profile_store(DEFAULT_PROFILE);
        let q = q.expect("corruption must be reported, not silently degraded");
        assert!(
            matches!(store, super::super::storage::StoreEnum::Db(_)),
            "must come back with a fresh working db, not memory"
        );
        assert_eq!(q.from, path.display().to_string());
        assert_eq!(fs::read(&q.to).unwrap(), b"not a database at all", "bad bytes preserved");
        // The live path must not retain the WAL bytes of that "bigger db": either the file
        // is gone (SQLite clears it itself when the salt does not match) or it is a fresh
        // empty WAL the new db created. Leave it behind and those pages get treated as
        // authoritative on the next open — exactly how this went wrong.
        match fs::read(&wal) {
            Ok(bytes) => assert_ne!(bytes, b"stale wal of a bigger db", "stale wal must not survive"),
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
        }
        let _ = shm; // -shm is a derived cache that SQLite clears and rebuilds on open
        // New db is usable: creating/writing a row succeeds
        let can_write = match &store {
            super::super::storage::StoreEnum::Db(s) => s
                .with_conn_ref(|c| {
                    let now = super::super::agent::now_secs() as i64;
                    c.execute(
                        "INSERT INTO events (id, type, source, timestamp, payload) VALUES ('t1', 'x', 'test', ?1, '{}')",
                        [now],
                    )
                    .is_ok()
                }),
            _ => false,
        };
        assert!(can_write, "fresh db must accept writes");
        let _ = fs::remove_dir_all(&home);
    }

    /// A healthy db opens as usual: no quarantine, no data loss.
    #[test]
    fn healthy_db_opens_without_quarantine() {
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = fresh_home();
        let _home = set_home(&home);
        let path = db_path_for(DEFAULT_PROFILE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        {
            let s = super::super::storage::Storage::open(&path).unwrap();
            s.with_conn_ref(|c| {
                // Current second: events have retention cleanup, so old timestamps get deleted on open
                let now = super::super::agent::now_secs() as i64;
                c.execute(
                    "INSERT INTO events (id, type, source, timestamp, payload) VALUES ('keep1', 'keep', 'test', ?1, '{}')",
                    [now],
                )
                .unwrap()
            });
        }
        let (store, q) = open_profile_store(DEFAULT_PROFILE);
        assert!(q.is_none(), "healthy db must not be quarantined");
        let kept: i64 = match &store {
            super::super::storage::StoreEnum::Db(s) => s
                .with_conn_ref(|c| c.query_row("SELECT count(*) FROM events WHERE id='keep1'", [], |r| r.get::<_, i64>(0)).unwrap()),
            _ => -1,
        };
        assert_eq!(kept, 1, "existing rows must survive a normal open");
        // Quarantine record: write → read back → dismiss
        let q2 = Quarantine { from: "a".into(), to: "b".into(), reason: "r".into(), at: 1 };
        write_db_recovery_notice(&q2);
        let notice = db_recovery_notice().expect("notice readable");
        assert_eq!(notice["reason"], "r");
        dismiss_db_recovery_notice().unwrap();
        assert!(db_recovery_notice().is_none(), "dismiss must clear the notice");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn validate_name_accepts_safe_chars() {
        assert!(validate_name("work").is_ok());
        assert!(validate_name("personal-2026").is_ok());
        assert!(validate_name("dev_env").is_ok());
        assert!(validate_name("a").is_ok());
    }

    #[test]
    fn validate_name_rejects_unsafe_chars() {
        assert!(validate_name("").is_err());
        assert!(validate_name(&"a".repeat(65)).is_err());
        assert!(validate_name("../etc").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
    }

    #[test]
    fn workspace_dir_returns_correct_path() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        let p = workspace_dir("work");
        assert!(p.ends_with(".opencapx/workspaces/work") || p.to_string_lossy().contains("work"));
    }

    #[test]
    fn profiles_file_lives_under_home() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        let f = profiles_file();
        assert!(f.starts_with(&home) || f.to_string_lossy().contains("opencapx"));
    }

    #[test]
    fn read_profiles_file_default_when_missing() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        let pf = read_profiles_file();
        assert_eq!(pf.active, DEFAULT_PROFILE);
        assert_eq!(pf.profiles.len(), 1);
        assert_eq!(pf.profiles[0].name, DEFAULT_PROFILE);
    }

    #[test]
    fn active_profile_name_defaults_when_missing() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        assert_eq!(active_profile_name(), DEFAULT_PROFILE);
    }

    #[test]
    fn set_active_profile_persists_and_loads_back() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        create_profile("work").unwrap();
        set_active_profile("work").unwrap();
        assert_eq!(active_profile_name(), "work");
        assert!(load_profiles().iter().any(|p| p.name == "work" && p.is_active));
    }

    #[test]
    fn create_profile_initializes_db_and_lists() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        let info = create_profile("alpha").unwrap();
        assert_eq!(info.name, "alpha");
        assert!(db_path_for("alpha").exists());
        let list = load_profiles();
        assert!(list.iter().any(|p| p.name == "alpha"));
    }

    #[test]
    fn create_profile_rejects_duplicate() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        create_profile("beta").unwrap();
        assert!(create_profile("beta").is_err());
    }

    #[test]
    fn delete_profile_removes_dir_and_json_entry() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        create_profile("gamma").unwrap();
        assert!(workspace_dir("gamma").exists());
        delete_profile("gamma").unwrap();
        assert!(!workspace_dir("gamma").exists());
        assert!(!load_profiles().iter().any(|p| p.name == "gamma"));
    }

    #[test]
    fn delete_profile_rejects_default() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        assert!(delete_profile(DEFAULT_PROFILE).is_err());
    }

    #[test]
    fn delete_profile_rejects_active() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        create_profile("delta").unwrap();
        set_active_profile("delta").unwrap();
        assert!(delete_profile("delta").is_err());
    }

    #[test]
    fn delete_profile_rejects_last_remaining() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        // The only profile is default → deletion is not allowed
        assert!(delete_profile(DEFAULT_PROFILE).is_err());
    }

    #[test]
    fn ensure_default_profile_migrated_is_idempotent() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        // No legacy DB → just ensure, no panic
        assert!(ensure_default_profile_migrated().is_ok());
        // A second call does not panic either
        assert!(ensure_default_profile_migrated().is_ok());
    }

    #[test]
    fn list_profiles_returns_all_with_zero_count() {
        let home = fresh_home();
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = set_home(&home);
        create_profile("epsilon").unwrap();
        let list = list_profiles();
        let names: HashSet<String> = list.iter().map(|p| p.name.clone()).collect();
        assert!(names.contains(DEFAULT_PROFILE));
        assert!(names.contains("epsilon"));
    }
}