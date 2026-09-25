use super::*;

/// Process-level env, sharing one lock with the plugin_trace / retention tests (see above).
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    crate::core::plugin_trace::traces_env_lock()
}

/// Read the whole trace file and parse line by line (same parsing as the debugging view).
fn read_all(agent: &str, trace: &str) -> Vec<RpcTraceLine> {
    let text = std::fs::read_to_string(trace_path(agent, trace)).unwrap();
    text.lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<RpcTraceLine>(l).ok())
        .collect()
}

#[test]
fn full_chain_writes_start_event_end_lines() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    let tid = begin("ag_x_01", "conn-7", "");
    event("dispatch", serde_json::json!({ "tool": "opencapx.say" }));
    let sp = span(
        "capability.opencapx.execute",
        serde_json::json!({ "capability": "opencapx.execute" }),
    );
    event("gate.denied", serde_json::json!({ "pluginId": "com.demo" }));
    sp.end(true, None, serde_json::json!({ "elapsedMs": 12 }));
    finish(true, None, serde_json::json!({ "durMs": 20 }));

    let lines = read_all("ag_x_01", &tid);
    assert_eq!(
        lines.len(),
        6,
        "start+event+start+event+end+end is 6 lines in total"
    );
    // Root start: no parent, name=rpc, with agent/conn
    match &lines[0] {
        RpcTraceLine::Start {
            span_id,
            parent_id,
            name,
            attrs,
            ..
        } => {
            assert_eq!(span_id, "s0");
            assert!(parent_id.is_none());
            assert_eq!(name, "rpc");
            assert_eq!(attrs["agent"], "ag_x_01");
            assert_eq!(attrs["conn"], "conn-7");
        }
        other => panic!("line0 should be Start, actual {:?}", other),
    }
    // The dispatch event on the root hangs off s0
    match &lines[1] {
        RpcTraceLine::Event { span_id, name, .. } => {
            assert_eq!(span_id, "s0");
            assert_eq!(name, "dispatch");
        }
        other => panic!("line1 should be Event, actual {:?}", other),
    }
    // Child span parent = s0
    match &lines[2] {
        RpcTraceLine::Start {
            span_id, parent_id, ..
        } => {
            assert_eq!(span_id, "s1");
            assert_eq!(parent_id.as_deref(), Some("s0"));
        }
        other => panic!("line2 should be Start, actual {:?}", other),
    }
    // The event on the child span hangs off s1
    match &lines[3] {
        RpcTraceLine::Event { span_id, name, .. } => {
            assert_eq!(span_id, "s1");
            assert_eq!(name, "gate.denied");
        }
        other => panic!("line3 should be Event, actual {:?}", other),
    }
    // child end ok
    match &lines[4] {
        RpcTraceLine::End {
            span_id, status, ..
        } => {
            assert_eq!(span_id, "s1");
            assert_eq!(*status, SpanStatus::Ok);
        }
        other => panic!("line4 should be End, actual {:?}", other),
    }
    // root end ok + durMs
    match &lines[5] {
        RpcTraceLine::End {
            span_id,
            status,
            attrs,
            ..
        } => {
            assert_eq!(span_id, "s0");
            assert_eq!(*status, SpanStatus::Ok);
            assert_eq!(attrs.as_ref().unwrap()["durMs"], 20);
        }
        other => panic!("line5 should be End, actual {:?}", other),
    }

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn no_context_all_calls_are_noop() {
    // Hold the lock: rpc_traces_root() reads env; without the lock parallel tests could redirect the root and drift assertions
    let _g = lock_env();
    let sp = span("orphan", serde_json::json!({}));
    event("ev", serde_json::json!({}));
    finish(true, None, serde_json::Value::Null);
    sp.end(true, None, serde_json::Value::Null); // empty id, should be skipped internally
    assert!(rpc_traces_root().join("ag_none").read_dir().is_err());
}

#[test]
fn dropped_span_writes_error_end() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    let tid = begin("ag_x_02", "", "");
    {
        let _sp = span("plugin.demo", serde_json::json!({})); // no explicit end
    } // Drop triggers the fallback
    finish(true, None, serde_json::Value::Null);

    let lines = read_all("ag_x_02", &tid);
    // root start + child start + child end (error, dropped) + root end
    match &lines[2] {
        RpcTraceLine::End {
            span_id,
            status,
            error,
            ..
        } => {
            assert_eq!(span_id, "s1");
            assert_eq!(*status, SpanStatus::Error);
            assert_eq!(error.as_deref(), Some("dropped without end"));
        }
        other => panic!("line2 should be End, actual {:?}", other),
    }

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn list_and_read_traces_roundtrip() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    let t1 = begin("ag_list", "", "");
    finish(true, None, serde_json::Value::Null);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let t2 = begin("ag_list", "", "");
    finish(false, Some("boom"), serde_json::Value::Null);
    // Pending: only root start, no end — ended_at must be None (so the viewer can distinguish "in progress")
    let t3 = begin("ag_list", "", "");

    let list = list_traces("ag_list");
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].trace_id, t3, "newest first");
    assert!(
        list[0].ended_at.is_none(),
        "a pending trace's ended_at should be None"
    );
    assert!(
        list[1].ended_at.is_some(),
        "a finished trace should have ended_at"
    );
    assert!(list[1].line_count >= 2);
    assert!(list[2].size_bytes > 0);

    let lines = read_trace("ag_list", &t1, 50);
    assert!(lines.len() >= 2);
    // Reverse order: the last line (root end) first
    assert!(matches!(lines[0], RpcTraceLine::End { .. }));
    // limit takes effect
    assert!(read_trace("ag_list", &t1, 1).len() == 1);

    // Other agents are mutually invisible
    assert!(list_traces("ag_other").is_empty());

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

/// hook events are persisted directly to a file (no thread-local context), and the session file accumulates per event.
#[test]
fn hook_event_appends_without_ctx() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    // Do not call begin — hook_event must work independently of the /rpc context
    hook_event(
        "ag_h",
        "sess-a1",
        "agent.started",
        serde_json::json!({ "message": "prompt" }),
    );
    hook_event("ag_h", "sess-a1", "agent.completed", serde_json::json!({}));
    hook_event("ag_h", "sess-b2", "agent.started", serde_json::json!({}));

    let list = list_hook_traces("ag_h");
    assert_eq!(list.len(), 2, "the two hook sessions each have one file");
    assert!(list[0].line_count >= 1);
    let lines = read_hook_trace("ag_h", "sess-a1", 50);
    assert_eq!(lines.len(), 2);
    // read_*_at reverse order (newest first): [0] = the later-written completed, [1] = the earlier-written started.
    // spanId is always "s0" (a uniform viewer shape, no parent/child)
    assert!(
        matches!(&lines[0], RpcTraceLine::Event { span_id, name, .. } if span_id == "s0" && name == "agent.completed")
    );
    assert!(
        matches!(&lines[1], RpcTraceLine::Event { span_id, name, .. } if span_id == "s0" && name == "agent.started")
    );

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

/// tab data source: merge counts from the rpc and hooks trees, newest activity descending; agents with no files are not listed.
#[test]
fn list_trace_agents_merges_rpc_and_hooks() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    finish(true, None, serde_json::Value::Null); // clear any leftover context
    begin("ag_a", "", "");
    finish(true, None, serde_json::Value::Null);
    begin("ag_a", "", "");
    finish(true, None, serde_json::Value::Null);
    begin("ag_b", "", "");
    finish(true, None, serde_json::Value::Null);
    hook_event("ag_a", "s1", "agent.started", serde_json::json!({}));
    hook_event("ag_c", "s1", "agent.started", serde_json::json!({}));

    let list = list_trace_agents();
    assert_eq!(list.len(), 3, "the three agents each have a trace");
    let find = |id: &str| list.iter().find(|x| x.agent_id == id).unwrap();
    assert_eq!(find("ag_a").rpc_count, 2);
    assert_eq!(find("ag_a").hook_count, 1);
    assert_eq!(find("ag_b").rpc_count, 1);
    assert_eq!(find("ag_b").hook_count, 0);
    assert_eq!(find("ag_c").rpc_count, 0);
    assert_eq!(find("ag_c").hook_count, 1);
    // Reverse order: last_ts is non-increasing
    assert!(list.windows(2).all(|w| w[0].last_ts >= w[1].last_ts));

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn trunc_str_cuts_on_char_boundary() {
    assert_eq!(trunc_str("abc", 10), "abc");
    let s = "aあbあcあ";
    let out = trunc_str(s, 4); // "aあ" = 4 bytes, the next 'b' is exactly on the boundary
    assert!(out.starts_with("aあ"));
    assert!(out.ends_with("…[truncated]"));
}

/// The root span carries the project reported by the CLI; the viewer groups by it.
#[test]
fn root_span_carries_project() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    let tid = begin("ag_p", "c9", "proj/x");
    finish(true, None, serde_json::Value::Null);

    let lines = read_all("ag_p", &tid);
    match &lines[0] {
        RpcTraceLine::Start { attrs, .. } => {
            assert_eq!(attrs["project"], "proj/x");
            assert_eq!(attrs["agent"], "ag_p");
            assert_eq!(attrs["conn"], "c9");
        }
        other => panic!("line0 should be Start, actual {:?}", other),
    }

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

/// Full enumeration: each request chain across agents carries agent_id + project; an old trace with no project → "".
#[test]
fn list_all_traces_carries_agent_and_project() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    let t1 = begin("ag_pa", "", "proj/a");
    finish(true, None, serde_json::Value::Null);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let t2 = begin("ag_pb", "", ""); // old-style persistence: no project
    finish(true, None, serde_json::Value::Null);

    let all = list_all_traces();
    assert_eq!(all.len(), 2, "the two agents each have one trace");
    assert_eq!(all[0].trace_id, t2, "started_at descending: newest first");
    assert_eq!(all[0].agent_id, "ag_pb");
    assert_eq!(
        all[0].project, "",
        "a trace missing the project field is grouped as unknown"
    );
    assert_eq!(all[1].trace_id, t1);
    assert_eq!(all[1].agent_id, "ag_pa");
    assert_eq!(all[1].project, "proj/a");

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

/// viewer summary status/last_ts: finished → "ok" + last line ts;
/// begin without finish (request pending) → "" + the root start ts.
#[test]
fn list_all_traces_carries_status_and_last_ts() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    let done = begin("ag_st", "", "");
    finish(true, None, serde_json::Value::Null);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let hanging = begin("ag_st", "", ""); // no finish: root end missing

    let all = list_all_traces();
    let find = |tid: &str| all.iter().find(|e| e.trace_id == tid).unwrap();
    let done_entry = find(&done);
    assert_eq!(done_entry.status, "ok");
    assert_eq!(
        done_entry.last_ts,
        line_ts(read_all("ag_st", &done).last().unwrap()),
        "last_ts = the ts of the last line (root end)"
    );
    let hang_entry = find(&hanging);
    assert_eq!(hang_entry.status, "", "no root end while pending");
    assert_eq!(
        hang_entry.last_ts,
        line_ts(read_all("ag_st", &hanging).last().unwrap()),
        "pending last_ts = the root start ts"
    );

    finish(true, None, serde_json::Value::Null); // clear the thread-local context
    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

/// hooks full enumeration: each hook session file carries its project (written by event.rs into hook attrs).
#[test]
fn list_all_hook_sessions_carries_project() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    hook_event(
        "ag_ha",
        "sess-1",
        "agent.started",
        serde_json::json!({ "project": "proj/h" }),
    );
    hook_event("ag_hb", "sess-2", "agent.started", serde_json::json!({}));

    let all = list_all_hook_sessions();
    assert_eq!(all.len(), 2, "the two hook sessions each have one file");
    let find = |sid: &str| all.iter().find(|e| e.trace_id == sid).unwrap();
    assert_eq!(find("sess-1").agent_id, "ag_ha");
    assert_eq!(find("sess-1").project, "proj/h");
    assert_eq!(find("sess-2").agent_id, "ag_hb");
    assert_eq!(
        find("sess-2").project,
        "",
        "a hook session with no project is grouped as unknown"
    );

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
}

/// Export: raw-byte copy of same-project chains + index.json manifest; a rerun counts overwritten; no chains reports no_chains.
#[test]
fn export_chains_copies_raw_bytes_and_writes_manifest() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let out = std::env::temp_dir().join(format!("opencapx-reqexport-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&out);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    // Two chains in the same project; one in another project to verify filtering.
    let t1 = begin("ag_ex", "c1", "proj/exp");
    event("dispatch", serde_json::json!({ "tool": "opencapx.say" }));
    finish(true, None, serde_json::Value::Null);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let t2 = begin("ag_ex", "c2", "proj/exp");
    finish(false, Some("boom"), serde_json::Value::Null);
    let t3 = begin("ag_ex", "c3", "proj/other");
    finish(true, None, serde_json::Value::Null);

    let report = export_chains_to(out.to_str().unwrap(), "proj/exp").unwrap();
    assert_eq!(
        report.exported, 2,
        "only the two chains of proj/exp are exported"
    );
    assert_eq!(report.overwritten, 0);
    assert!(report.failures.is_empty());
    assert_eq!(report.project, "proj/exp");
    assert!(report
        .chains
        .iter()
        .all(|c| c.trace_id == t1 || c.trace_id == t2));
    assert!(report
        .chains
        .iter()
        .any(|c| c.trace_id == t1 && c.status == "ok"));
    assert!(report
        .chains
        .iter()
        .any(|c| c.trace_id == t2 && c.status == "error"));

    // Persisted at the single-level subdirectory `<dir>/<sanitize(project)>/`.
    let proj_dir = out.join("proj_exp");
    assert_eq!(
        report.dir,
        proj_dir.to_string_lossy().to_string(),
        "report.dir points at the actually written subdirectory"
    );

    // The directory holds exactly as many ndjson files as same-project chains.
    let ndjson: Vec<_> = std::fs::read_dir(&proj_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("ndjson"))
        .collect();
    assert_eq!(
        ndjson.len(),
        2,
        "the two same-project chains each have one file, excluding proj/other"
    );

    // Byte-level identity: source file == target file (no JSON round-trip).
    for tid in [&t1, &t2] {
        let src = trace_path("ag_ex", tid);
        let dst = proj_dir.join(format!("{}.ndjson", tid));
        assert_eq!(
            std::fs::read(&src).unwrap(),
            std::fs::read(&dst).unwrap(),
            "{} should be byte-identical",
            tid
        );
    }
    // t3 belongs to another project, not persisted.
    assert!(!proj_dir.join(format!("{}.ndjson", t3)).exists());

    // index.json is parseable, with schema/chainCount/project/chains correct.
    let manifest_path = proj_dir.join("index.json");
    assert_eq!(
        report.manifest_path,
        manifest_path.to_string_lossy().to_string()
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["schema"], 1);
    assert_eq!(manifest["chainCount"], 2);
    assert_eq!(manifest["project"], "proj/exp");
    assert_eq!(manifest["chains"].as_array().unwrap().len(), 2);

    // Rerun: both already exist → overwritten == exported.
    let again = export_chains_to(out.to_str().unwrap(), "proj/exp").unwrap();
    assert_eq!(again.exported, 2);
    assert_eq!(again.overwritten, 2, "both already existed on rerun");

    // A project with no chains → no_chains, and it **does not touch the filesystem**: neither the selection directory nor nested subdirectories are created.
    let untouched =
        std::env::temp_dir().join(format!("opencapx-reqexport-nc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&untouched);
    assert_eq!(
        export_chains_to(untouched.to_str().unwrap(), "proj/none").unwrap_err(),
        "no_chains"
    );
    assert!(
        !untouched.exists(),
        "a no_chains early return must not leave any directory behind"
    );

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&out);
    let _ = std::fs::remove_dir_all(&untouched);
}

/// project == "" → use the stable ASCII fallback directory name `_no-project` (no localized label).
#[test]
fn export_chains_empty_project_falls_back_to_no_project_dir() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let out = std::env::temp_dir().join(format!("opencapx-reqexport-np-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&out);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    let tid = begin("ag_np", "c1", "");
    event("dispatch", serde_json::json!({ "tool": "opencapx.say" }));
    finish(true, None, serde_json::Value::Null);

    let report = export_chains_to(out.to_str().unwrap(), "").unwrap();
    assert_eq!(report.exported, 1);
    assert_eq!(report.project, "");
    let proj_dir = out.join("_no-project");
    assert_eq!(report.dir, proj_dir.to_string_lossy().to_string());
    assert!(
        proj_dir.join(format!("{}.ndjson", tid)).exists(),
        "the chain lands under the _no-project level"
    );
    assert_eq!(
        report.manifest_path,
        proj_dir.join("index.json").to_string_lossy().to_string()
    );
    assert!(proj_dir.join("index.json").exists());

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&out);
}

/// dir points at an existing file (directory creation must fail) → dir_unwritable.
#[test]
fn export_chains_reports_dir_unwritable() {
    let _g = lock_env();
    let base = std::env::temp_dir().join(format!("opencapx-reqtrace-{}", std::process::id()));
    let bogus =
        std::env::temp_dir().join(format!("opencapx-reqexport-file-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_file(&bogus);
    std::env::set_var("OPENCAPX_TRACES_DIR", &base);

    let tid = begin("ag_du", "", "proj/du");
    finish(true, None, serde_json::Value::Null);
    assert!(!tid.is_empty());
    std::fs::write(&bogus, b"x").unwrap();

    assert_eq!(
        export_chains_to(bogus.to_str().unwrap(), "proj/du").unwrap_err(),
        "dir_unwritable"
    );
    // The nested target directory name likewise goes through sanitize, and no directory is created on failure.
    assert!(!bogus.join("proj_du").exists());

    std::env::remove_var("OPENCAPX_TRACES_DIR");
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_file(&bogus);
}
