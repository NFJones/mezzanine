//! CLI new serve tests.

use super::*;

/// Verifies noninteractive new requires dry run.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn noninteractive_new_requires_dry_run() {
    let (env, home) = test_env("new-fails");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec!["mez".to_string(), "new".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    let _ = fs::remove_dir_all(home);
}

/// Verifies bare mez enters new session path.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn bare_mez_enters_new_session_path() {
    let (env, home) = test_env("bare-new");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec!["mez".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(error.message().contains("interactive terminal"));

    let _ = fs::remove_dir_all(home);
}

/// Verifies that `mez new` does not reuse the default control socket when an
/// existing session is already serving it. The interactive command must launch
/// a daemon on a fresh socket so the subsequent attach step cannot accidentally
/// reconnect to the older session.
#[test]
fn new_session_default_socket_allocates_fresh_socket_when_default_is_active() {
    let (env, home) = test_env("new-fresh-socket");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let default_socket =
        crate::runtime::socket_path_for_name(&directory.path, crate::runtime::DEFAULT_SOCKET_NAME)
            .unwrap();
    let _listener = bind_control_socket(&default_socket, env.runtime.uid).unwrap();

    let selection = super::super::serve::socket_selection_for_new_session(
        &SocketSelection::Default(default_socket.clone()),
    )
    .unwrap();

    assert_ne!(selected_socket_path(&selection), &default_socket);
    assert!(
        selected_socket_path(&selection)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(".new."))
    );

    let _ = fs::remove_dir_all(home);
}

/// Verifies that early background-daemon startup failures include the child
/// process stderr. This keeps `mez new` launch failures diagnosable when the
/// foreground client has not yet connected to the daemon socket.
#[test]
fn new_session_daemon_startup_error_includes_child_stderr() {
    let (_env, home) = test_env("new-daemon-stderr");
    let socket_path = home.join("runtime").join("daemon.sock");
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("printf 'daemon config failed\\n' >&2; exit 1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let error = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let mut daemon = super::super::serve::BackgroundControlDaemon::spawn(command).unwrap();
            super::super::serve::wait_for_background_control_daemon(&socket_path, &mut daemon).await
        })
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error.message().contains("daemon config failed"),
        "{}",
        error.message()
    );
    let _ = fs::remove_dir_all(home);
}

/// Verifies background daemon startup waits for a complete control response.
///
/// The daemon binds its socket before pane startup and actor supervision are
/// ready. A connect-only readiness check can therefore report success while the
/// child is still starting or about to exit, making the immediate attach fail
/// with a reset socket. This regression server accepts the probe immediately
/// but delays its framed response so the wait helper must not return early.
#[test]
fn new_session_daemon_startup_waits_for_control_probe_response() {
    let (_env, home) = test_env("new-daemon-probe-response");
    let socket_path = home.join("runtime").join("daemon-probe.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _addr) = listener.accept().unwrap();
        let mut request = [0; 1024];
        let read = stream.read(&mut request).unwrap();
        assert!(
            String::from_utf8_lossy(&request[..read]).contains("cli-startup-probe"),
            "{}",
            String::from_utf8_lossy(&request[..read])
        );
        thread::sleep(Duration::from_millis(100));
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-startup-probe","error":{"code":-32003,"message":"first control request must be control/initialize"}}"#,
            ))
            .unwrap();
    });
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("sleep 5")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let started = Instant::now();
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let mut daemon = super::super::serve::BackgroundControlDaemon::spawn(command).unwrap();
            super::super::serve::wait_for_background_control_daemon(&socket_path, &mut daemon)
                .await
                .unwrap();
            daemon.terminate_for_test().await;
        });

    assert!(
        started.elapsed() >= Duration::from_millis(80),
        "startup probe returned before the control response was written"
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(home);
}

/// Verifies dry run new builds default session model.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dry_run_new_builds_default_session_model() {
    let (env, home) = test_env("new-dry-run");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with_plain(
        vec![
            "mez".to_string(),
            "new".to_string(),
            "--dry-run".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("session_id: $1"));
    assert!(output.contains("window_count: 1"));
    assert!(output.contains("pane_count: 1"));
    assert!(output.contains("dry_run: true"));

    let _ = fs::remove_dir_all(home);
}

/// Verifies that live daemon sessions do not reuse the deterministic `$1`
/// in-memory construction id. The durable registry keys records by session id;
/// if two independently launched daemons both publish `$1`, the later upsert
/// replaces the earlier record and `mez list` hides active sessions.
#[test]
fn live_daemon_session_ids_are_unique_for_registry_listing() {
    let (env, home) = test_env("live-session-id-registry");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let shell = resolve_shell(Some(OsString::from("/bin/sh"))).unwrap();
    let mut first = Session::new_default(shell.clone(), Size::new(80, 24).unwrap());
    let mut second = Session::new_default(shell, Size::new(80, 24).unwrap());
    assign_unique_live_session_id(&mut first).unwrap();
    assign_unique_live_session_id(&mut second).unwrap();
    assert_ne!(first.id, second.id);
    let first_socket = directory.path.join("first.sock");
    let second_socket = directory.path.join("second.sock");
    fs::write(&first_socket, "").unwrap();
    fs::write(&second_socket, "").unwrap();

    registry
        .upsert(SessionRecord::from_session(&first, first_socket, 100, None))
        .unwrap();
    registry
        .upsert(SessionRecord::from_session(
            &second,
            second_socket,
            101,
            None,
        ))
        .unwrap();

    let records = registry.list().unwrap();
    assert_eq!(records.len(), 2);

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies serve starts foreground control daemon.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn serve_starts_foreground_control_daemon() {
    let (env, home) = test_env("serve-control");
    let socket = home.join("runtime").join("serve.sock");
    let socket_for_server = socket.clone();
    let server = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_with(
            vec![
                "mez".to_string(),
                "-S".to_string(),
                socket_for_server.to_string_lossy().to_string(),
                "serve".to_string(),
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

    let mut stream =
        connect_when_ready(&socket).expect("serve command did not accept socket connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":1,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let get = r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#;
    stream.write_all(&encode_control_body(initialize)).unwrap();
    stream.write_all(&encode_control_body(get)).unwrap();
    stream.flush().unwrap();

    let response = read_control_response_frames(&mut stream, 1024 * 1024, 2).unwrap();
    let (initialize_response, consumed) = decode_control_frame(&response, 1024 * 1024).unwrap();
    let (session_response, _) = decode_control_frame(&response[consumed..], 1024 * 1024).unwrap();
    assert!(initialize_response.contains(r#""granted_role":"primary""#));
    assert!(session_response.contains(r#""session_id":"$"#));
    drop(stream);

    let (result, stdout, stderr) = server.join().unwrap();
    result.unwrap();
    assert!(stdout.contains(r#""serving":true"#));
    assert!(stdout.contains("serve.sock"));
    assert!(stderr.is_empty());
    assert!(!socket.exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies synchronous control-response reads fail at the socket boundary when
/// EOF arrives before a full protocol frame. Callers should not pass partial
/// buffers to the strict frame decoder and leak its low-level header diagnostic
/// as a user-facing CLI crash.
#[test]
fn read_control_response_frames_rejects_eof_before_complete_frame() {
    let (mut writer, mut reader) = UnixStream::pair().unwrap();
    writer.write_all(b"Content-Length: 16\r\n").unwrap();
    writer.flush().unwrap();
    drop(writer);

    let error = read_control_response_frames(&mut reader, 1024 * 1024, 1).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error
            .message()
            .contains("control socket closed before complete response frame"),
        "{error}"
    );
    assert!(
        !error.message().contains("missing header terminator"),
        "{error}"
    );
}

/// Verifies that daemon startup does not spawn an auth refresh worker when the
/// persisted OpenAI access token is still well outside the refresh leeway
/// window. This keeps the launch-time background refresh trigger conditional
/// rather than starting network work on every session.
#[test]
fn serve_skips_background_auth_refresh_when_openai_token_is_still_fresh() {
    let (env, home) = test_env("serve-auth-refresh-fresh");
    let paths = env.config_paths().unwrap();
    let auth_store = AuthStore::new(AuthPaths::under_config_root(paths.root()));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_provider_credential(
            "default",
            OpenAiProviderCredential {
                api_key: "access-secret".to_string(),
                refresh_token: Some("refresh-secret".to_string()),
                account_id: Some("acct_123".to_string()),
                organization_id: Some("org_123".to_string()),
                token_expires_at: Some("9999999999".to_string()),
            },
            &credential_store,
        )
        .unwrap();

    assert!(!super::super::serve::spawn_openai_auth_refresh_if_needed(
        auth_store,
        crate::auth::DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS,
    ));

    let _ = fs::remove_dir_all(home);
}

/// Verifies serve attach primary requires interactive terminal.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn serve_attach_primary_requires_interactive_terminal() {
    let (env, home) = test_env("serve-attach-noninteractive");
    let socket = home.join("runtime").join("serve.sock");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().to_string(),
            "serve".to_string(),
            "--attach-primary".to_string(),
            "--no-aux-sockets".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(error.message().contains("interactive terminal"));
    assert!(!socket.exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies serve can start message protocol socket.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn serve_can_start_message_protocol_socket() {
    let (env, home) = test_env("serve-message");
    let control_socket = home.join("runtime").join("serve.sock");
    let message_socket = home.join("runtime").join("serve.message.sock");
    let control_for_server = control_socket.clone();
    let message_for_server = message_socket.clone();
    let server = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_with(
            vec![
                "mez".to_string(),
                "-S".to_string(),
                control_for_server.to_string_lossy().to_string(),
                "serve".to_string(),
                "--no-aux-sockets".to_string(),
                "--message-socket".to_string(),
                message_for_server.to_string_lossy().to_string(),
                "--max-control-connections".to_string(),
                "1".to_string(),
                "--max-message-connections".to_string(),
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

    assert!(wait_for_path(&control_socket));
    assert!(wait_for_path(&message_socket));

    let mut control_stream =
        connect_when_ready(&control_socket).expect("control socket did not accept connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":1,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    control_stream
        .write_all(&encode_control_body(initialize))
        .unwrap();
    control_stream.flush().unwrap();
    let control_response =
        read_control_response_frames(&mut control_stream, 1024 * 1024, 1).unwrap();
    let (control_body, _) = decode_control_frame(&control_response, 1024 * 1024).unwrap();
    assert!(control_body.contains(r#""granted_role":"primary""#));
    drop(control_stream);

    let mut message_stream = UnixStream::connect(&message_socket).unwrap();
    message_stream
        .write_all(&crate::message::encode_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
        ))
        .unwrap();
    message_stream.flush().unwrap();
    let mut message_response = vec![0; 4096];
    let read = message_stream.read(&mut message_response).unwrap();
    message_response.truncate(read);
    let (message_body, _) = crate::message::decode_mmp_frame(&message_response, 4096).unwrap();
    assert!(message_body.contains(r#""type":"welcome""#));
    drop(message_stream);

    let (result, stdout, stderr) = server.join().unwrap();
    result.unwrap();
    assert!(stdout.contains(r#""message":true"#));
    assert!(stdout.contains("serve.message.sock"));
    assert!(stderr.is_empty());
    assert!(!control_socket.exists());
    assert!(!message_socket.exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies serve can start event stream socket.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn serve_can_start_event_stream_socket() {
    let (env, home) = test_env("serve-event");
    let control_socket = home.join("runtime").join("serve.sock");
    let event_socket = home.join("runtime").join("serve.event.sock");
    let control_for_server = control_socket.clone();
    let event_for_server = event_socket.clone();
    let server = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_with(
            vec![
                "mez".to_string(),
                "-S".to_string(),
                control_for_server.to_string_lossy().to_string(),
                "serve".to_string(),
                "--no-aux-sockets".to_string(),
                "--event-socket".to_string(),
                event_for_server.to_string_lossy().to_string(),
                "--max-control-connections".to_string(),
                "1".to_string(),
                "--max-event-connections".to_string(),
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

    assert!(wait_for_path(&control_socket));
    assert!(wait_for_path(&event_socket));
    let mut event_stream =
        connect_when_ready(&event_socket).expect("event socket did not accept connections");
    event_stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut control_stream =
        connect_when_ready(&control_socket).expect("control socket did not accept connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":1,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let kill = r#"{"jsonrpc":"2.0","id":"kill","method":"session/kill","params":{"force":true,"idempotency_key":"kill"}}"#;
    control_stream
        .write_all(&encode_control_body(initialize))
        .unwrap();
    control_stream
        .write_all(&encode_control_body(kill))
        .unwrap();
    control_stream.flush().unwrap();
    let control_response =
        read_control_response_frames(&mut control_stream, 1024 * 1024, 2).unwrap();
    let (initialize_body, consumed) = decode_control_frame(&control_response, 1024 * 1024).unwrap();
    let (kill_body, _) = decode_control_frame(&control_response[consumed..], 1024 * 1024).unwrap();
    assert!(initialize_body.contains(r#""granted_role":"primary""#));
    assert!(kill_body.contains(r#""killed":true"#));
    drop(control_stream);

    let event_response = read_control_response_frames(&mut event_stream, 1024 * 1024, 1).unwrap();
    let (event_body, _) = decode_control_frame(&event_response, 1024 * 1024).unwrap();
    assert!(event_body.contains(r#""method":"event/"#));
    drop(event_stream);

    let (result, stdout, stderr) = server.join().unwrap();
    result.unwrap();
    assert!(stdout.contains(r#""event":true"#));
    assert!(stdout.contains("serve.event.sock"));
    assert!(stderr.is_empty());
    assert!(!control_socket.exists());
    assert!(!event_socket.exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies serve derives default auxiliary sockets.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn serve_derives_default_auxiliary_sockets() {
    let (env, home) = test_env("serve-default-aux");
    let control_socket = home.join("runtime").join("serve.sock");
    let message_socket = home.join("runtime").join("serve.message.sock");
    let event_socket = home.join("runtime").join("serve.event.sock");
    let control_for_server = control_socket.clone();
    let server = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_with(
            vec![
                "mez".to_string(),
                "-S".to_string(),
                control_for_server.to_string_lossy().to_string(),
                "serve".to_string(),
                "--max-control-connections".to_string(),
                "1".to_string(),
                "--max-message-connections".to_string(),
                "1".to_string(),
                "--max-event-connections".to_string(),
                "1".to_string(),
                "--max-event-batches-per-connection".to_string(),
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

    assert!(wait_for_path(&control_socket));
    assert!(wait_for_path(&message_socket));
    assert!(wait_for_path(&event_socket));

    let mut message_stream =
        connect_when_ready(&message_socket).expect("message socket did not accept connections");
    message_stream
        .write_all(&crate::message::encode_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
        ))
        .unwrap();
    message_stream.flush().unwrap();
    let mut message_response = vec![0; 4096];
    let read = message_stream.read(&mut message_response).unwrap();
    message_response.truncate(read);
    let (message_body, _) = crate::message::decode_mmp_frame(&message_response, 4096).unwrap();
    assert!(message_body.contains(r#""type":"welcome""#));
    drop(message_stream);

    let mut event_stream =
        connect_when_ready(&event_socket).expect("event socket did not accept connections");
    event_stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut control_stream =
        connect_when_ready(&control_socket).expect("control socket did not accept connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":1,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let kill = r#"{"jsonrpc":"2.0","id":"kill","method":"session/kill","params":{"force":true,"idempotency_key":"kill"}}"#;
    control_stream
        .write_all(&encode_control_body(initialize))
        .unwrap();
    control_stream
        .write_all(&encode_control_body(kill))
        .unwrap();
    control_stream.flush().unwrap();
    let control_response =
        read_control_response_frames(&mut control_stream, 1024 * 1024, 2).unwrap();
    let (initialize_body, consumed) = decode_control_frame(&control_response, 1024 * 1024).unwrap();
    let (kill_body, _) = decode_control_frame(&control_response[consumed..], 1024 * 1024).unwrap();
    assert!(initialize_body.contains(r#""granted_role":"primary""#));
    assert!(kill_body.contains(r#""killed":true"#));
    drop(control_stream);

    let event_response = read_control_response_frames(&mut event_stream, 1024 * 1024, 1).unwrap();
    let (event_body, _) = decode_control_frame(&event_response, 1024 * 1024).unwrap();
    assert!(event_body.contains(r#""method":"event/"#));
    drop(event_stream);

    let (result, stdout, stderr) = server.join().unwrap();
    result.unwrap();
    assert!(stdout.contains(r#""message":true"#));
    assert!(stdout.contains(r#""event":true"#));
    assert!(stdout.contains("serve.message.sock"));
    assert!(stdout.contains("serve.event.sock"));
    assert!(stderr.is_empty());
    assert!(!control_socket.exists());
    assert!(!message_socket.exists());
    assert!(!event_socket.exists());

    let _ = fs::remove_dir_all(home);
}
