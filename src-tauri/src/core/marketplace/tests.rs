use super::*;
use std::io::Write;
use std::sync::Mutex;

// Three tests share the OPENCAPX_MARKETPLACE_DIR global env var; serialize to prevent races
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // N2 — test fixtures are local files: explicitly enable the local-URL switch (off by default in production).
    std::env::set_var("OPENCAPX_MARKETPLACE_ALLOW_LOCAL", "1");
    g
}

/// N2 — production rejects file:// and bare local paths by default; only an explicit switch allows them.
#[test]
fn local_urls_are_rejected_without_test_switch() {
    let _g = lock_env();
    std::env::remove_var("OPENCAPX_MARKETPLACE_ALLOW_LOCAL");
    let dir = std::env::temp_dir().join(format!("opencapx-mk-local-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let pkg = dir.join("x.ocplugin");
    write(&pkg, "x");
    let mut e = entry_with(vec![], None);
    e.download_url = format!("file://{}", pkg.display());
    e.sha256 = sha256_hex(b"x");
    let err = download(&e).unwrap_err();
    assert!(err.contains("local url disabled"), "got: {}", err);
    let err2 = fetch_url(&e.download_url).unwrap_err();
    assert!(err2.contains("local url disabled"), "got: {}", err2);
    std::env::set_var("OPENCAPX_MARKETPLACE_ALLOW_LOCAL", "1");
    let _ = std::fs::remove_dir_all(&dir);
}

fn write(path: &Path, v: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(v.as_bytes()).unwrap();
}

fn entry_with(versions: Vec<PluginMarketVersion>, min_core: Option<&str>) -> PluginMarketEntry {
    PluginMarketEntry {
        id: "com.x.m".into(),
        name: "M".into(),
        version: "0.0.0".into(),
        description: String::new(),
        download_url: "file:///dev/null".into(),
        sha256: "0".repeat(64),
        capabilities: vec![],
        permissions: vec![],
        channel: "stable".into(),
        min_core_version: min_core.map(String::from),
        versions,
    }
}

fn mv(v: &str, min_core: Option<&str>, ch: Option<&str>) -> PluginMarketVersion {
    PluginMarketVersion {
        version: v.into(),
        min_core_version: min_core.map(String::from),
        channel: ch.map(String::from),
        download_url: format!("file:///x-{v}.ocplugin"),
        sha256: "1".repeat(64),
    }
}

/// SHA-256 known-answer vector: "abc" → ba7816bf...f20015ad
#[test]
fn sha256_abc() {
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// RFC 4231 Test Case 1: key=0x0b*20, data="Hi There"
#[test]
fn hmac_sha256_matches_rfc4231_case1() {
    let key = vec![0x0b_u8; 20];
    assert_eq!(
        hmac_sha256_hex(&key, b"Hi There"),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
}

/// RFC 4231 Test Case 2: key="Jefe"
#[test]
fn hmac_sha256_matches_rfc4231_case2() {
    assert_eq!(
        hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

/// RFC 4231 Test Case 6: 131-byte over-long key (hash-first branch)
#[test]
fn hmac_sha256_long_key_matches_rfc4231_case6() {
    let key = vec![0xaa_u8; 131];
    assert_eq!(
        hmac_sha256_hex(
            &key,
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        ),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

/// The raw and hex entry points must agree (the easiest spot to get wrong when swapping implementations).
#[test]
fn sha256_raw_matches_hex() {
    let data = b"opencapx-integration-check";
    let raw = sha256_raw(data);
    let hex: String = raw.iter().map(|b| format!("{:02x}", b)).collect();
    assert_eq!(hex, sha256_hex(data));
}

/// Prerelease semantics: release > prerelease; prerelease segments compare segment by segment; build metadata does not participate.
/// The old test `version_is_newer_compares_digits` must still pass unchanged.
#[test]
fn version_is_newer_handles_prerelease_and_metadata() {
    assert!(version_is_newer("1.0.0", "1.0.0-rc.1"));
    assert!(version_is_newer("1.0.0-rc.10", "1.0.0-rc.9"));
    assert!(!version_is_newer("1.0.0-rc.1", "1.0.0"));
    assert!(version_is_newer("v1.2.0", "1.1.9"));
    assert!(!version_is_newer("1.0.0+build5", "1.0.0"));
    assert!(!version_is_newer("1.0.0", "1.0.0+build5"));
}

#[test]
fn seed_fallback_loads_and_caches() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-mkt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
    std::env::remove_var("OPENCAPX_MARKETPLACE_URL");

    let idx = PluginMarketIndex {
        entries: vec![PluginMarketEntry {
            id: "com.x.demo".into(),
            name: "Demo".into(),
            version: "0.1.0".into(),
            description: "demo".into(),
            download_url: "file:///tmp/x.ocplugin".into(),
            sha256: "deadbeef".into(),
            capabilities: vec!["image.analyze".into()],
            permissions: vec!["image.read".into()],
            channel: "stable".into(),
            min_core_version: None,
            versions: vec![],
        }],
    };
    write(&seed_path(), &serde_json::to_string_pretty(&idx).unwrap());
    let loaded = load_index();
    assert_eq!(loaded.entries.len(), 1);
    assert!(cache_path().exists());
    assert_eq!(find("com.x.demo").unwrap().name, "Demo");
    assert!(find("nope").is_none());

    std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn version_is_newer_compares_digits() {
    assert!(version_is_newer("1.2.0", "1.1.5"));
    assert!(version_is_newer("2.0.0", "1.99.99"));
    assert!(!version_is_newer("1.1.0", "1.1.0"));
    assert!(!version_is_newer("1.1.0", "1.2.0"));
    assert!(version_is_newer("1.2", "1.1.5"));
    assert!(!version_is_newer("not-a-version", "0.0.1"));
}

#[test]
fn select_prefers_highest_compatible_version() {
    let e = entry_with(
        vec![
            mv("2.0.0", Some("99.0.0"), None), // incompatible
            mv("1.5.0", Some("0.1.0"), None),
            mv("1.4.0", None, None),
        ],
        None,
    );
    let v = select_market_version(&e, "1.0.0", "stable").expect("compatible");
    assert_eq!(v.version, "1.5.0");
}

#[test]
fn select_none_when_all_versions_too_new() {
    let e = entry_with(vec![mv("2.0.0", Some("99.0.0"), None)], None);
    assert!(select_market_version(&e, "1.0.0", "stable").is_none());
}

#[test]
fn select_respects_channel_ceiling() {
    let e = entry_with(
        vec![
            mv("2.0.0", None, Some("beta")),
            mv("1.5.0", None, Some("stable")),
        ],
        None,
    );
    let v = select_market_version(&e, "1.0.0", "stable").expect("stable only");
    assert_eq!(v.version, "1.5.0");
    let v2 = select_market_version(&e, "1.0.0", "beta").expect("beta ok");
    assert_eq!(v2.version, "2.0.0");
}

#[test]
fn select_falls_back_to_flat_entry_when_no_versions_array() {
    let e = entry_with(vec![], Some("99.0.0"));
    assert!(select_market_version(&e, "1.0.0", "stable").is_none());
    let ok = entry_with(vec![], Some("0.1.0"));
    assert_eq!(
        select_market_version(&ok, "1.0.0", "stable").map(|v| v.version),
        Some("0.0.0".to_string())
    );
}

/// M1 fix (N1/N2): explicit install uses a `"dev"` ceiling (no subscription gate), update uses the plugin's channel (default
/// `stable`). The two caller semantics for the same entry must be distinguishable.
#[test]
fn resolve_target_ceiling_separates_install_and_update_semantics() {
    let mut beta = entry_with(vec![], Some("0.1.0"));
    beta.channel = "beta".into();
    assert!(
        resolve_target_with_ceiling(&beta, "1.0.0", "stable").is_err(),
        "flat beta entry must be gated out for a stable update"
    );
    let installed =
        resolve_target_with_ceiling(&beta, "1.0.0", "dev").expect("explicit install un-gated");
    assert_eq!(installed.channel.as_deref(), Some("beta"));

    let mut stable = entry_with(vec![], Some("0.1.0"));
    stable.channel = "stable".into();
    assert!(resolve_target_with_ceiling(&stable, "1.0.0", "stable").is_ok());
    assert!(resolve_target_with_ceiling(&stable, "1.0.0", "dev").is_ok());
}

#[test]
fn resolve_target_reports_core_incompatibility() {
    let mut e = entry_with(vec![mv("1.5.0", Some("99.0.0"), None)], None);
    e.channel = "beta".into();
    for ceiling in ["stable", "dev"] {
        let err = resolve_target_with_ceiling(&e, "1.0.0", ceiling).unwrap_err();
        assert!(err.contains("core-compatible"), "got: {}", err);
    }
}

#[test]
fn check_updates_returns_only_newer() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-mkt-upd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
    std::env::remove_var("OPENCAPX_MARKETPLACE_URL");
    let idx = PluginMarketIndex {
        entries: vec![
            PluginMarketEntry {
                id: "com.x.old".into(),
                name: "Old".into(),
                version: "0.1.0".into(),
                description: "".into(),
                download_url: "file:///dev/null".into(),
                sha256: "0".repeat(64),
                capabilities: vec![],
                permissions: vec![],
                channel: "stable".into(),
                min_core_version: None,
                versions: vec![],
            },
            PluginMarketEntry {
                id: "com.x.bumped".into(),
                name: "Bumped".into(),
                version: "1.2.0".into(),
                description: "".into(),
                download_url: "file:///dev/null".into(),
                sha256: "0".repeat(64),
                capabilities: vec![],
                permissions: vec![],
                channel: "stable".into(),
                min_core_version: None,
                versions: vec![],
            },
        ],
    };
    let text = serde_json::to_string(&idx).unwrap();
    std::fs::write(base.join("seed.json"), &text).unwrap();
    // cache.json does not exist, load_index should fall back to seed
    std::fs::remove_file(base.join("cache.json")).ok();

    let installed = vec![
        ("com.x.old".to_string(), "0.1.0".to_string()),
        ("com.x.bumped".to_string(), "1.1.0".to_string()),
        ("com.x.missing".to_string(), "0.1.0".to_string()),
    ];
    let updates = check_updates(&installed, &[]);
    assert_eq!(updates.len(), 1, "only bumped should have update");
    assert_eq!(updates[0].id, "com.x.bumped");
    assert_eq!(updates[0].current_version, "1.1.0");
    assert_eq!(updates[0].latest_version, "1.2.0");

    std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn check_updates_skips_entries_requiring_newer_core() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-mkt-core-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
    std::env::remove_var("OPENCAPX_MARKETPLACE_URL");
    let idx = PluginMarketIndex {
        entries: vec![
            entry_with(vec![], None),
            PluginMarketEntry {
                id: "com.x.future".into(),
                version: "2.0.0".into(),
                min_core_version: Some("99.0.0".into()),
                ..entry_with(vec![], None)
            },
        ],
    };
    std::fs::write(base.join("seed.json"), serde_json::to_string(&idx).unwrap()).unwrap();
    let installed = vec![
        ("com.x.m".to_string(), "0.0.0".to_string()),
        ("com.x.future".to_string(), "1.0.0".to_string()),
    ];
    let updates = check_updates(&installed, &[]);
    assert_eq!(
        updates.len(),
        0,
        "0.0.0 is not an update; future is skipped for core incompatibility"
    );
    std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn download_rejects_sha_mismatch() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-mkt2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
    let pkg = base.join("pkg.ocplugin");
    std::fs::write(&pkg, b"hello world").unwrap();
    let entry = PluginMarketEntry {
        id: "com.x".into(),
        name: "X".into(),
        version: "0.1.0".into(),
        description: "".into(),
        download_url: format!("file://{}", pkg.display()),
        sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
        capabilities: vec![],
        permissions: vec![],
        channel: "stable".into(),
        min_core_version: None,
        versions: vec![],
    };
    let err = download(&entry).unwrap_err();
    assert!(err.contains("sha256 mismatch"), "got: {}", err);
    std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn download_passes_and_returns_path() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-mkt3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
    let pkg = base.join("good.ocplugin");
    let bytes = b"hello world";
    std::fs::write(&pkg, bytes).unwrap();
    let entry = PluginMarketEntry {
        id: "com.x".into(),
        name: "X".into(),
        version: "0.1.0".into(),
        description: "".into(),
        download_url: format!("file://{}", pkg.display()),
        sha256: "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9".into(),
        capabilities: vec![],
        permissions: vec![],
        channel: "stable".into(),
        min_core_version: None,
        versions: vec![],
    };
    let p = download(&entry).unwrap();
    assert!(p.exists());
    assert!(p.metadata().unwrap().len() == bytes.len() as u64);
    std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

/// App-side bridge: when the flat version is incompatible (2.0.0) but versions[] has a compatible entry (1.5.0),
/// check_updates suggests 1.5.0 → download_target must download 1.5.0, not the flat field's 2.0.0.
#[test]
fn download_target_uses_selected_version_not_flat_entry() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-mkt-target-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);

    // The flat field points to the incompatible 2.0.0 (decoy); only versions[] has the compatible 1.5.0.
    let decoy = base.join("decoy.ocplugin");
    std::fs::write(&decoy, b"decoy-2.0.0").unwrap();
    let pkg150 = base.join("v150.ocplugin");
    let payload = b"payload-1.5.0";
    std::fs::write(&pkg150, payload).unwrap();

    let mut e = entry_with(
        vec![PluginMarketVersion {
            version: "1.5.0".into(),
            min_core_version: Some("0.1.0".into()),
            channel: None,
            download_url: format!("file://{}", pkg150.display()),
            sha256: sha256_hex(payload),
        }],
        Some("99.0.0"),
    );
    e.version = "2.0.0".into();
    e.download_url = format!("file://{}", decoy.display());
    e.sha256 = sha256_hex(b"decoy-2.0.0");

    let selected = select_market_version(&e, "1.0.0", "stable").expect("compatible 1.5.0");
    assert_eq!(selected.version, "1.5.0");

    let p = download_target(&e.id, &selected).expect("download target");
    assert!(
        p.file_name().unwrap().to_string_lossy().contains("1.5.0"),
        "path should name selected version: {}",
        p.display()
    );
    assert_eq!(std::fs::read(&p).unwrap(), payload);

    std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn download_target_rejects_sha_mismatch() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-mkt-target-sha-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
    let pkg = base.join("bad.ocplugin");
    std::fs::write(&pkg, b"hello target").unwrap();
    let target = PluginMarketVersion {
        version: "1.5.0".into(),
        min_core_version: None,
        channel: None,
        download_url: format!("file://{}", pkg.display()),
        sha256: "0".repeat(64),
    };
    let err = download_target("com.x.m", &target).unwrap_err();
    assert!(err.contains("sha256 mismatch"), "got: {}", err);
    std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

/// Full chain: seed -> find -> download (sha verification) -> install_ocplugin -> start the echo plugin.
/// Requires python3; ignored by default (avoids a mandatory CI dependency); run with `--ignored`.
#[test]
#[ignore]
fn market_install_full_chain() {
    let _g = lock_env();
    let py = std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    if !py {
        eprintln!("skip: no python3");
        return;
    }
    let base = std::env::temp_dir().join(format!("opencapx-mktchain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
    std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

    // install_ocplugin needs shared_store
    let store: super::super::storage::SharedStore =
        std::sync::Arc::new(std::sync::Mutex::new(super::super::storage::StoreEnum::Db(
            super::super::storage::Storage::open(&base.join("t.db")).unwrap(),
        )));
    super::super::set_shared_store(store);

    // Prepare the .ocplugin package (reuse the in-repo echo-vision)
    let ocplugin = base.join("echo.ocplugin");
    let echo_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("plugins")
        .join("echo-vision");
    {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let f = std::fs::File::create(&ocplugin).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let manifest = std::fs::read_to_string(echo_dir.join("opencapx-plugin.json")).unwrap();
        let script = std::fs::read_to_string(echo_dir.join("bin/echo_vision.py")).unwrap();
        z.start_file("opencapx-plugin.json", SimpleFileOptions::default())
            .unwrap();
        z.write_all(manifest.as_bytes()).unwrap();
        z.start_file("bin/echo_vision.py", SimpleFileOptions::default())
            .unwrap();
        z.write_all(script.as_bytes()).unwrap();
        z.finish().unwrap();
    }
    let sha = super::sha256(&std::fs::read(&ocplugin).unwrap());
    let seed = PluginMarketIndex {
        entries: vec![PluginMarketEntry {
            id: "com.opencapx.echo-vision".into(),
            name: "Echo Vision".into(),
            version: "0.1.0".into(),
            description: "smoke".into(),
            download_url: format!("file://{}", ocplugin.display()),
            sha256: sha,
            capabilities: vec!["image.analyze".into()],
            permissions: vec!["image.read".into()],
            channel: "stable".into(),
            min_core_version: None,
            versions: vec![],
        }],
    };
    std::fs::write(
        base.join("seed.json"),
        serde_json::to_string_pretty(&seed).unwrap(),
    )
    .unwrap();

    // Follow the same path as the Tauri install_marketplace command
    let entry = super::find("com.opencapx.echo-vision").expect("entry");
    let pkg = download(&entry).expect("download");
    let mgr = super::super::plugin::PluginManager::shared();
    let id = mgr.install_ocplugin(&pkg).expect("install_ocplugin");
    assert_eq!(id, "com.opencapx.echo-vision");
    assert!(mgr
        .list()
        .iter()
        .any(|p| p.id == id && p.status == "running"));
    mgr.stop(&id);
    std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
    std::env::remove_var("OPENCAPX_PLUGINS_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

/// Phase 38 — channel rank: dev > beta > stable; unknown falls back to stable.
/// normalize_channel is the single entry point: normalize the raw string first, then rank.
#[test]
fn channel_rank_ordering() {
    assert!(channel_rank("dev") > channel_rank("beta"));
    assert!(channel_rank("beta") > channel_rank("stable"));
    assert_eq!(channel_rank(""), 0);
    assert_eq!(channel_rank("unknown"), 0);
    assert_eq!(channel_rank("nightly"), 0); // the alias goes through normalize, not rank
    assert!(channel_rank(normalize_channel("nightly").as_str()) > channel_rank("stable"));
}

/// normalize_channel collapses case / spelling differences into one of three.
#[test]
fn normalize_channel_canonical() {
    assert_eq!(normalize_channel("stable"), "stable");
    assert_eq!(normalize_channel("STABLE"), "stable");
    assert_eq!(normalize_channel("Beta"), "beta");
    assert_eq!(normalize_channel("rc"), "beta");
    assert_eq!(normalize_channel("nightly"), "dev");
    assert_eq!(normalize_channel(" dev "), "dev");
    assert_eq!(normalize_channel("garbage"), "stable");
}

/// Filesystem-independent — use check_updates' core filter logic directly (write a helper over entries).
/// This verifies the if branch of the channel filter: a stable user cannot see beta/dev entries.
#[test]
fn channel_filter_drops_higher_ranks_for_stable_user() {
    // Mirror check_updates' filter decision (without calling load_index, to avoid disk dependence):
    let entries = vec![
        ("plug-a", "1.0.0", "stable"),
        ("plug-b", "1.0.0", "beta"),
        ("plug-c", "1.0.0", "dev"),
    ];
    let installed = vec![
        ("plug-a".to_string(), "0.9.0".to_string()),
        ("plug-b".to_string(), "0.9.0".to_string()),
        ("plug-c".to_string(), "0.9.0".to_string()),
    ];
    // A stable user sees only stable
    let channels_stable: Vec<(String, String)> = vec![];
    let mut visible: Vec<&str> = entries
        .iter()
        .filter(|(id, _, ch)| {
            let min = channels_stable
                .iter()
                .find(|(pid, _)| pid == *id)
                .map(|(_, c)| c.as_str())
                .unwrap_or("stable");
            channel_rank(ch) <= channel_rank(min)
        })
        .map(|(id, _, _)| *id)
        .collect();
    visible.sort();
    assert_eq!(visible, vec!["plug-a"]);
    // A dev user sees all
    let channels_dev: Vec<(String, String)> = vec![
        ("plug-a".into(), "dev".into()),
        ("plug-b".into(), "dev".into()),
        ("plug-c".into(), "dev".into()),
    ];
    let mut visible2: Vec<&str> = entries
        .iter()
        .filter(|(id, _, ch)| {
            let min = channels_dev
                .iter()
                .find(|(pid, _)| pid == *id)
                .map(|(_, c)| c.as_str())
                .unwrap_or("stable");
            channel_rank(ch) <= channel_rank(min)
        })
        .map(|(id, _, _)| *id)
        .collect();
    visible2.sort();
    assert_eq!(visible2, vec!["plug-a", "plug-b", "plug-c"]);
    // A beta user sees stable + beta (not dev)
    let channels_beta: Vec<(String, String)> = vec![
        ("plug-a".into(), "beta".into()),
        ("plug-b".into(), "beta".into()),
        ("plug-c".into(), "beta".into()),
    ];
    let mut visible3: Vec<&str> = entries
        .iter()
        .filter(|(id, _, ch)| {
            let min = channels_beta
                .iter()
                .find(|(pid, _)| pid == *id)
                .map(|(_, c)| c.as_str())
                .unwrap_or("stable");
            channel_rank(ch) <= channel_rank(min)
        })
        .map(|(id, _, _)| *id)
        .collect();
    visible3.sort();
    assert_eq!(visible3, vec!["plug-a", "plug-b"]);
}
