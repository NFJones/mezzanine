//! CLI snapshot tests.

use super::*;

/// Verifies snapshot list reads local snapshot repository.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_list_reads_local_snapshot_repository() {
    let (env, home) = test_env("snapshot-list");
    let repository =
        SnapshotRepository::new(home.join(".config").join("mezzanine").join("snapshots"));
    repository
        .write(&SnapshotManifest {
            state: SnapshotState {
                id: "snap1".to_string(),
                version: 1,
                session_id: "$1".to_string(),
                name: Some("manual".to_string()),
                created_at: "2026-04-30T00:00:00Z".to_string(),
                kind: SnapshotKind::Manual,
                restorable: true,
                window_count: 1,
                pane_count: 1,
                limitations: vec!["pane primary processes must be restarted".to_string()],
                storage_ref: "snap1.payload".to_string(),
            },
            contains_terminal_history: false,
            contains_agent_transcripts: false,
            contains_raw_credentials: false,
            active_approvals_restored: false,
            restart_required_panes: Vec::new(),
        })
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "snapshot".to_string(),
            "list".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""snapshot_id":"snap1""#));
    assert!(output.contains(r#""kind":"manual""#));
    assert!(output.contains(r#""limitations":["pane primary processes must be restarted"]"#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies snapshot resume restores local session shape.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_resume_restores_local_session_shape() {
    let (env, home) = test_env("snapshot-resume");
    let repository =
        SnapshotRepository::new(home.join(".config").join("mezzanine").join("snapshots"));
    let mut session = Session::new_default(
        resolve_shell(Some(OsString::from("/bin/sh"))).unwrap(),
        Size::new(80, 24).unwrap(),
    );
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    repository
        .create_from_session("snap-resume", Some("manual".to_string()), &session)
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "snapshot".to_string(),
            "resume".to_string(),
            "snap-resume".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""restored":true"#));
    assert!(output.contains(r#""live":false"#));
    assert!(output.contains(r#""pane_count":2"#));
    assert!(output.contains(r#""restart_required_panes":[]"#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies snapshot resume can restart restored panes with explicit command.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_resume_can_restart_restored_panes_with_explicit_command() {
    let (env, home) = test_env("snapshot-resume-restart");
    let repository =
        SnapshotRepository::new(home.join(".config").join("mezzanine").join("snapshots"));
    let mut session = Session::new_default(
        resolve_shell(Some(OsString::from("/bin/sh"))).unwrap(),
        Size::new(80, 24).unwrap(),
    );
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    repository
        .create_from_session("snap-restart", Some("manual".to_string()), &session)
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "snapshot".to_string(),
            "resume".to_string(),
            "snap-restart".to_string(),
            "--restart-command".to_string(),
            "true".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""restored":true"#));
    assert!(output.contains(r#""live":false"#));
    assert!(output.contains(r#""restarted":true"#));
    assert!(output.contains(r#""restarted_panes":["#));
    assert!(output.contains(r#""primary_pid":"#));
    assert!(output.contains(r#""pane_count":2"#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies that `snapshot resume --serve` starts a live control daemon from a
/// restored snapshot and assigns that live daemon a fresh discovery identity.
/// Snapshot payloads retain the original session id, but concurrent restored
/// daemons must not overwrite each other in the live session registry.
#[test]
fn snapshot_resume_can_serve_restored_session_over_control_socket() {
    let (env, home) = test_env("snapshot-resume-serve");
    let repository =
        SnapshotRepository::new(home.join(".config").join("mezzanine").join("snapshots"));
    let session = Session::new_default(
        resolve_shell(Some(OsString::from("/bin/sh"))).unwrap(),
        Size::new(80, 24).unwrap(),
    );
    let pane_id = session.windows()[0].panes()[0].id.to_string();
    repository
        .create_from_session_with_captures(
            "snap-serve",
            Some("manual".to_string()),
            &session,
            &[SnapshotPaneCapture {
                pane_id,
                primary_pid: None,
                process_state: Some("exited".to_string()),
                current_working_directory: None,
                readiness_state: Some("unknown".to_string()),
                terminal_history: vec!["snapshot-history".to_string()],
                terminal_history_line_style_spans: vec![Vec::new()],
                visible_lines: vec!["snapshot-visible".to_string()],
                visible_line_style_spans: vec![Vec::new()],
                terminal_modes: mez_terminal::TerminalModeState::default(),
                terminal_saved_state: mez_terminal::TerminalSavedState::default(),
                exit_status: None,
                alternate_screen_active: false,
                transcript_refs: Vec::new(),
            }],
        )
        .unwrap();

    let socket = home.join("runtime").join("snapshot-serve.sock");
    let socket_for_server = socket.clone();
    let server = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_with(
            vec![
                "mez".to_string(),
                "-S".to_string(),
                socket_for_server.to_string_lossy().to_string(),
                "snapshot".to_string(),
                "resume".to_string(),
                "snap-serve".to_string(),
                "--serve".to_string(),
                "--no-aux-sockets".to_string(),
                "--max-control-connections".to_string(),
                "1".to_string(),
            ],
            env,
            false,
            &mut stdout,
            &mut stderr,
        );
        (
            result,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    });

    assert!(
        wait_for_path(&socket),
        "snapshot resume --serve did not bind socket"
    );
    let mut stream =
        connect_when_ready(&socket).expect("snapshot resume socket did not accept connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":1,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let view = r#"{"jsonrpc":"2.0","id":"view","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#;
    stream.write_all(&encode_control_body(initialize)).unwrap();
    stream.write_all(&encode_control_body(view)).unwrap();
    stream.flush().unwrap();

    let response = read_control_response_frames(&mut stream, 1024 * 1024, 2).unwrap();
    let (initialize_response, consumed) = decode_control_frame(&response, 1024 * 1024).unwrap();
    let (view_response, _) = decode_control_frame(&response[consumed..], 1024 * 1024).unwrap();
    assert!(initialize_response.contains(r#""granted_role":"primary""#));
    assert!(
        view_response.contains("pane restarted with a fresh primary PID"),
        "{view_response}"
    );
    drop(stream);

    let (result, stdout, stderr) = server.join().unwrap();
    result.unwrap();
    assert!(stdout.contains(r#""serving":true"#));
    assert!(stdout.contains(r#""restored":true"#));
    assert!(stdout.contains("snapshot-serve.sock"));
    let live_session_id = stdout
        .split(r#""session_id":""#)
        .nth(1)
        .and_then(|tail| tail.split('"').next())
        .unwrap();
    assert_ne!(live_session_id, "$1");
    assert!(stderr.is_empty());
    assert!(!socket.exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies snapshot resume latest selects newest matching snapshot.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_resume_latest_selects_newest_matching_snapshot() {
    let (env, home) = test_env("snapshot-resume-latest");
    let repository =
        SnapshotRepository::new(home.join(".config").join("mezzanine").join("snapshots"));
    let mut session = Session::new_default(
        resolve_shell(Some(OsString::from("/bin/sh"))).unwrap(),
        Size::new(80, 24).unwrap(),
    );
    repository
        .create_from_session("snap-a", Some("old".to_string()), &session)
        .unwrap();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    repository
        .create_from_session("snap-z", Some("new".to_string()), &session)
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "snapshot".to_string(),
            "resume-latest".to_string(),
            "--session-id".to_string(),
            session.id.to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""restored":true"#));
    assert!(output.contains(r#""session_id":"$1""#));
    assert!(output.contains(r#""pane_count":2"#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}
