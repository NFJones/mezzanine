//! CLI snapshot tests.

use super::*;

/// Verifies the removed snapshot planning commands are rejected by the CLI.
///
/// The planning projections remain internal restore metadata, but none of the
/// former `-plan` command names or aliases may remain available to users.
#[test]
fn snapshot_plan_commands_are_rejected() {
    for command in [
        "resume-plan",
        "latest-plan",
        "resume-latest-plan",
        "rollback-plan",
    ] {
        let (env, home) = test_env(command);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let error = run_with(
            vec![
                "mez".to_string(),
                "snapshot".to_string(),
                command.to_string(),
                "snapshot-id".to_string(),
            ],
            env,
            false,
            &mut stdout,
            &mut stderr,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unrecognized subcommand"));
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let _ = fs::remove_dir_all(home);
    }
}

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
    )
    .unwrap();
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
    )
    .unwrap();
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
    )
    .unwrap();
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
    let server = thread::Builder::new()
        .name("snapshot-resume-serve".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
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
        })
        .unwrap();

    assert!(
        wait_for_path(&socket),
        "snapshot resume --serve did not bind socket"
    );
    let mut stream =
        connect_when_ready(&socket).expect("snapshot resume socket did not accept connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":2,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let view = r#"{"jsonrpc":"2.0","id":"view","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#;
    let kill = r#"{"jsonrpc":"2.0","id":"kill","method":"session/kill","params":{"force":true,"idempotency_key":"kill-snapshot-serve-test"}}"#;
    stream.write_all(&encode_control_body(initialize)).unwrap();
    stream.write_all(&encode_control_body(view)).unwrap();
    stream.write_all(&encode_control_body(kill)).unwrap();
    stream.flush().unwrap();

    let response = read_control_response_frames(&mut stream, 1024 * 1024, 3).unwrap();
    let (initialize_response, consumed) = decode_control_frame(&response, 1024 * 1024).unwrap();
    let (view_response, view_consumed) =
        decode_control_frame(&response[consumed..], 1024 * 1024).unwrap();
    let (kill_response, _) =
        decode_control_frame(&response[consumed + view_consumed..], 1024 * 1024).unwrap();
    assert!(initialize_response.contains(r#""granted_role":"primary""#));
    assert!(
        view_response.contains("pane restarted with a fresh primary PID"),
        "{view_response}"
    );
    assert!(
        kill_response.contains(r#""killed":true"#),
        "{kill_response}"
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
/// The two snapshots carry distinct creation instants so the assertion depends
/// on the durable creation order rather than on how much host-clock time passed
/// between two back-to-back saves.
#[test]
fn snapshot_resume_latest_selects_newest_matching_snapshot() {
    let fixture = resume_latest_fixture("snapshot-resume-latest", 1_800_000_000, 1_800_000_001);

    let output = resume_latest_output(fixture.env, &fixture.session_id);
    assert!(output.contains(r#""restored":true"#));
    assert!(output.contains(r#""session_id":"$1""#));
    assert!(output.contains(r#""pane_count":2"#));

    let selected = selected_snapshot(&fixture.snapshots_root, &fixture.session_id);
    assert_eq!(selected.id, "snap-z");
    assert_eq!(selected.pane_count, 2);
    assert!(fixture.two_panes.created_at > fixture.one_pane.created_at);

    let _ = fs::remove_dir_all(fixture.home);
}

/// Verifies a snapshot saved inside the same wall-clock second still becomes the
/// latest snapshot.
///
/// Back-to-back saves frequently share a second, so selection must follow
/// creation order instead of leaving the older snapshot in the latest slot until
/// the clock happens to advance.
#[test]
fn snapshot_resume_latest_keeps_creation_order_within_one_wall_clock_second() {
    let fixture = resume_latest_fixture(
        "snapshot-resume-latest-one-second",
        1_800_000_000,
        1_800_000_000,
    );

    let output = resume_latest_output(fixture.env, &fixture.session_id);
    assert!(output.contains(r#""pane_count":2"#));

    let selected = selected_snapshot(&fixture.snapshots_root, &fixture.session_id);
    assert_eq!(selected.id, "snap-z");
    assert_eq!(selected.pane_count, 2);
    assert!(fixture.two_panes.created_at > fixture.one_pane.created_at);

    let _ = fs::remove_dir_all(fixture.home);
}

/// Verifies creation order outranks the snapshot id for saves inside one second.
///
/// The ids here run opposite to creation order, so a selection that falls back
/// to id order reports the older one-pane save. Latest selection must follow the
/// store's creation stamp instead of the snapshot name.
#[test]
fn snapshot_resume_latest_uses_creation_order_over_snapshot_id_within_one_second() {
    let fixture = resume_latest_fixture_with_ids(
        "snapshot-resume-latest-reversed-ids",
        1_800_000_000,
        "snap-z",
        1_800_000_000,
        "snap-a",
    );

    let output = resume_latest_output(fixture.env, &fixture.session_id);
    assert!(output.contains(r#""pane_count":2"#));

    let selected = selected_snapshot(&fixture.snapshots_root, &fixture.session_id);
    assert_eq!(selected.id, "snap-a");
    assert_eq!(selected.pane_count, 2);
    assert!(fixture.two_panes.created_at > fixture.one_pane.created_at);

    let _ = fs::remove_dir_all(fixture.home);
}

/// Verifies resume latest keeps creation order when the host clock regresses.
///
/// Latest selection orders by the persisted creation stamp. A clock that reads
/// earlier for the second save used to leave the older one-pane snapshot as the
/// winner, so resume-latest restored the wrong topology; the store now advances
/// the stamp so the newest snapshot always wins.
#[test]
fn snapshot_resume_latest_keeps_creation_order_when_the_wall_clock_regresses() {
    let fixture = resume_latest_fixture(
        "snapshot-resume-latest-clock-regressed",
        1_800_000_100,
        1_800_000_000,
    );

    let output = resume_latest_output(fixture.env, &fixture.session_id);
    assert!(output.contains(r#""restored":true"#));
    assert!(output.contains(r#""pane_count":2"#));

    let selected = selected_snapshot(&fixture.snapshots_root, &fixture.session_id);
    assert_eq!(selected.id, "snap-z");
    assert_eq!(selected.pane_count, 2);
    assert!(fixture.two_panes.created_at > fixture.one_pane.created_at);

    let _ = fs::remove_dir_all(fixture.home);
}

/// Deterministic inputs and persisted states for the resume-latest fixture.
struct ResumeLatestFixture {
    env: CliEnv,
    home: PathBuf,
    session_id: String,
    snapshots_root: PathBuf,
    one_pane: SnapshotState,
    two_panes: SnapshotState,
}

/// Builds one session with a one-pane snapshot and a two-pane snapshot.
///
/// The two creation instants are supplied by the caller so every ordering
/// assertion depends on the store's creation order instead of on the host clock.
/// Returns the CLI environment, home directory, exact session id, snapshot store
/// root, and both persisted states in creation order.
fn resume_latest_fixture(
    name: &str,
    first_seconds: u64,
    second_seconds: u64,
) -> ResumeLatestFixture {
    resume_latest_fixture_with_ids(name, first_seconds, "snap-a", second_seconds, "snap-z")
}

/// Builds the same fixture with caller-chosen snapshot ids.
///
/// The ids are explicit so a regression can publish saves whose id order does not
/// match creation order while both still carry the caller's creation instants.
fn resume_latest_fixture_with_ids(
    name: &str,
    first_seconds: u64,
    first_id: &str,
    second_seconds: u64,
    second_id: &str,
) -> ResumeLatestFixture {
    let (env, home) = test_env(name);
    let snapshots_root = home.join(".config").join("mezzanine").join("snapshots");
    let mut session = Session::new_default(
        resolve_shell(Some(OsString::from("/bin/sh"))).unwrap(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let session_id = session.id.to_string();
    let one_pane = SnapshotRepository::new(snapshots_root.clone())
        .with_creation_seconds(first_seconds)
        .create_from_session(first_id, Some("old".to_string()), &session)
        .unwrap();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    let two_panes = SnapshotRepository::new(snapshots_root.clone())
        .with_creation_seconds(second_seconds)
        .create_from_session(second_id, Some("new".to_string()), &session)
        .unwrap();
    assert_eq!(one_pane.pane_count, 1);
    assert_eq!(two_panes.pane_count, 2);
    ResumeLatestFixture {
        env,
        home,
        session_id,
        snapshots_root,
        one_pane,
        two_panes,
    }
}

/// Returns the snapshot the durable store selects for one exact session.
fn selected_snapshot(snapshots_root: &Path, session_id: &str) -> SnapshotState {
    SnapshotRepository::new(snapshots_root.to_path_buf())
        .latest(Some(session_id))
        .unwrap()
        .expect("the fixture publishes matching snapshots")
}

/// Runs `mez snapshot resume-latest --session-id` and returns its JSON output.
fn resume_latest_output(env: CliEnv, session_id: &str) -> String {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "snapshot".to_string(),
            "resume-latest".to_string(),
            "--session-id".to_string(),
            session_id.to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(stderr.is_empty());
    String::from_utf8(stdout).unwrap()
}
