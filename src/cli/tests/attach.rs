//! CLI attach tests.

use super::*;

/// Verifies parses socket selection before command.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_socket_selection_before_command() {
    let (env, home) = test_env("socket-selection");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "-L".to_string(),
            "work.sock".to_string(),
            "list-sessions".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    assert_eq!(String::from_utf8(stdout).unwrap(), "[]\n");
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies list reads durable session registry.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn list_reads_durable_session_registry() {
    let (env, home) = test_env("list-registry");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    let socket_path = directory.path.join("default.sock");
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    fs::write(&socket_path, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$1".to_string(),
            name: "work".to_string(),
            state: RegistrySessionState::Detached,
            socket_path,
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: Some(120),
            window_count: 2,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 100,
            authoritative_rows: 30,
        })
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec!["mez".to_string(), "list".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("\"session_id\":\"$1\""));
    assert!(output.contains("\"index_alias\":\"$1\""));
    assert!(output.contains("\"state\":\"detached\""));
    assert!(output.contains("\"primary_available\":true"));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies attach uses selected control socket.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attach_uses_selected_control_socket() {
    let (env, home) = test_env("attach-control");
    fs::create_dir_all(&home).unwrap();
    let root = home.join("runtime");
    let socket = root.join("default.sock");
    let listener = match bind_control_socket(&socket, env.runtime.uid) {
        Ok(listener) => listener,
        Err(error)
            if error.kind() == crate::error::MezErrorKind::Io
                && error.message().contains("Operation not permitted") =>
        {
            let _ = fs::remove_dir_all(home);
            return;
        }
        Err(error) => panic!("{error}"),
    };
    let server = spawn_noninteractive_attach_stub_server(
        listener,
        Some(r#""method":"session/get""#),
        r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary","client_id":"c1"}}"#,
        Some(r#"{"jsonrpc":"2.0","id":"cli","result":{"session":{"session_id":"$1"}}}"#),
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().to_string(),
            "attach".to_string(),
        ],
        env,
        true,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""session_id":"$1""#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies attach observer requests pending observer without session data.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attach_observer_requests_pending_observer_without_session_data() {
    let (env, home) = test_env("attach-observer-control");
    fs::create_dir_all(&home).unwrap();
    let root = home.join("runtime");
    let socket = root.join("default.sock");
    let listener = match bind_control_socket(&socket, env.runtime.uid) {
        Ok(listener) => listener,
        Err(error)
            if error.kind() == crate::error::MezErrorKind::Io
                && error.message().contains("Operation not permitted") =>
        {
            let _ = fs::remove_dir_all(home);
            return;
        }
        Err(error) => panic!("{error}"),
    };
    let server = spawn_noninteractive_attach_stub_server(
        listener,
        None,
        r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"pending_observer","approval_pending":true,"session":null}}"#,
        None,
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().to_string(),
            "attach".to_string(),
            "--observer".to_string(),
        ],
        env,
        true,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""granted_role":"pending_observer""#));
    assert!(output.contains(r#""approval_pending":true"#));
    assert!(output.contains(r#""session":null"#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies attach requires interactive terminal.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attach_requires_interactive_terminal() {
    let (env, home) = test_env("attach-noninteractive");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec!["mez".to_string(), "attach".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies snapshot create uses selected control socket.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_create_uses_selected_control_socket() {
    let (env, home) = test_env("snapshot-create-control");
    fs::create_dir_all(&home).unwrap();
    let root = home.join("runtime");
    let socket = root.join("default.sock");
    let listener = match bind_control_socket(&socket, env.runtime.uid) {
        Ok(listener) => listener,
        Err(error)
            if error.kind() == crate::error::MezErrorKind::Io
                && error.message().contains("Operation not permitted") =>
        {
            let _ = fs::remove_dir_all(home);
            return;
        }
        Err(error) => panic!("{error}"),
    };
    let server = thread::spawn(move || {
        let (mut stream, _addr) = listener.accept().unwrap();
        let mut request = vec![0; 4096];
        let read = stream.read(&mut request).unwrap();
        request.truncate(read);
        let (initialize, consumed) = decode_control_frame(&request, 4096).unwrap();
        let (body, _) = decode_control_frame(&request[consumed..], 4096).unwrap();
        assert!(initialize.contains(r#""method":"control/initialize""#));
        assert!(body.contains(r#""method":"snapshot/create""#));
        assert!(body.contains(r#""target":{"default":true}"#));
        assert!(body.contains(r#""name":"checkpoint""#));
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary"}}"#,
            ))
            .unwrap();
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli","result":{"snapshot":{"snapshot_id":"snap1"}}}"#,
            ))
            .unwrap();
    });
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().to_string(),
            "snapshot".to_string(),
            "create".to_string(),
            "--name".to_string(),
            "checkpoint".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""snapshot_id":"snap1""#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies that default `mez attach` discovers a live session through the
/// registry instead of blindly connecting to `default.sock`. Fresh `mez new`
/// sessions use per-session sockets, so default attach must choose an available
/// registry record for primary reattachment.
#[test]
fn default_attach_uses_registry_socket_with_available_primary() {
    let (env, home) = test_env("attach-default-registry");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let first_socket = directory.path.join("first.sock");
    let second_socket = directory.path.join("second.sock");
    fs::write(&first_socket, "").unwrap();
    fs::write(&second_socket, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$1".to_string(),
            name: "busy".to_string(),
            state: RegistrySessionState::Running,
            socket_path: first_socket,
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 1,
            primary_available: false,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$2".to_string(),
            name: "free".to_string(),
            state: RegistrySessionState::Detached,
            socket_path: second_socket.clone(),
            created_at_unix_seconds: 101,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();
    let default_socket = directory.path.join(crate::runtime::DEFAULT_SOCKET_NAME);

    let selection = super::super::attach::default_attach_socket_selection(
        &SocketSelection::Default(default_socket),
        env.runtime.uid,
        "primary",
    )
    .unwrap()
    .expect("default attach should select a registry socket");

    assert_eq!(selected_socket_path(&selection), &second_socket);

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies that default primary attachment does not silently choose a busy
/// registry record. A stale or occupied record should produce a clear conflict
/// instead of connecting to an arbitrary socket and surfacing a confusing
/// initialize failure.
#[test]
fn default_attach_reports_conflict_when_no_primary_is_available() {
    let (env, home) = test_env("attach-default-no-primary");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let busy_socket = directory.path.join("busy.sock");
    fs::write(&busy_socket, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$1".to_string(),
            name: "busy".to_string(),
            state: RegistrySessionState::Running,
            socket_path: busy_socket,
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: Some(110),
            window_count: 1,
            client_count: 1,
            primary_available: false,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();
    let default_socket = directory.path.join(crate::runtime::DEFAULT_SOCKET_NAME);

    let error = super::super::attach::default_attach_socket_selection(
        &SocketSelection::Default(default_socket),
        env.runtime.uid,
        "primary",
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
    assert!(
        error
            .message()
            .contains("no registered session currently accepts primary attachment")
    );

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies that `mez attach SESSION_ID` resolves the requested session through
/// the durable registry. This keeps explicit attachment attempts from being
/// routed to the default socket when multiple live sessions exist.
#[test]
fn attach_session_id_uses_matching_registry_socket() {
    let (env, home) = test_env("attach-session-id");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let default_socket = directory.path.join(crate::runtime::DEFAULT_SOCKET_NAME);
    let target_socket = directory.path.join("target.sock");
    fs::write(&target_socket, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$target".to_string(),
            name: "target".to_string(),
            state: RegistrySessionState::Detached,
            socket_path: target_socket.clone(),
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();

    let request = super::super::attach::attach_request_from_args(
        &SocketSelection::Default(default_socket),
        &["$target".to_string()],
        env.runtime.uid,
    )
    .unwrap();

    assert_eq!(request.requested_role, "primary");
    assert_eq!(
        selected_socket_path(&request.socket_selection),
        &target_socket
    );

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies that `mez attach` accepts creation-order session aliases in both
/// displayed `$N` form and bare numeric form. This keeps the CLI target syntax
/// short while still deriving the target from the same registry order shown by
/// `mez list`.
#[test]
fn attach_session_alias_uses_creation_order_registry_socket() {
    let (env, home) = test_env("attach-session-alias");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let default_socket = directory.path.join(crate::runtime::DEFAULT_SOCKET_NAME);
    let oldest_socket = directory.path.join("oldest.sock");
    let newest_socket = directory.path.join("newest.sock");
    fs::write(&oldest_socket, "").unwrap();
    fs::write(&newest_socket, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$newest".to_string(),
            name: "newest".to_string(),
            state: RegistrySessionState::Detached,
            socket_path: newest_socket.clone(),
            created_at_unix_seconds: 200,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$oldest".to_string(),
            name: "oldest".to_string(),
            state: RegistrySessionState::Detached,
            socket_path: oldest_socket.clone(),
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();

    let first = super::super::attach::attach_request_from_args(
        &SocketSelection::Default(default_socket.clone()),
        &["$1".to_string()],
        env.runtime.uid,
    )
    .unwrap();
    let second = super::super::attach::attach_request_from_args(
        &SocketSelection::Default(default_socket),
        &["2".to_string()],
        env.runtime.uid,
    )
    .unwrap();

    assert_eq!(
        selected_socket_path(&first.socket_selection),
        &oldest_socket
    );
    assert_eq!(
        selected_socket_path(&second.socket_selection),
        &newest_socket
    );

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies observer attachment uses the same index alias resolution as primary
/// attachment while preserving the requested observer role.
#[test]
fn attach_observer_accepts_session_index_alias() {
    let (env, home) = test_env("attach-observer-alias");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let default_socket = directory.path.join(crate::runtime::DEFAULT_SOCKET_NAME);
    let target_socket = directory.path.join("target.sock");
    fs::write(&target_socket, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$target".to_string(),
            name: "target".to_string(),
            state: RegistrySessionState::Running,
            socket_path: target_socket.clone(),
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: Some(120),
            window_count: 1,
            client_count: 1,
            primary_available: false,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();

    let request = super::super::attach::attach_request_from_args(
        &SocketSelection::Default(default_socket),
        &["--observe".to_string(), "1".to_string()],
        env.runtime.uid,
    )
    .unwrap();

    assert_eq!(request.requested_role, "observer");
    assert_eq!(
        selected_socket_path(&request.socket_selection),
        &target_socket
    );

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies CLI startup removes unserved sockets from the default runtime
/// directory.
///
/// This regression scenario confirms stale endpoint cleanup runs before normal
/// command dispatch without touching explicit socket paths or requiring a
/// registry record.
#[test]
fn startup_removes_unserved_default_runtime_socket_files() {
    let (env, home) = test_env("startup-stale-socket-cleanup");
    let directory = default_socket_directory(&env.runtime).unwrap();
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let stale_socket = directory.path.join("orphan.sock");
    let stale_listener = std::os::unix::net::UnixListener::bind(&stale_socket).unwrap();
    drop(stale_listener);
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match UnixStream::connect(&stale_socket) {
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => break,
            Err(error) if Instant::now() >= deadline => {
                panic!("stale socket did not become refused: {error}");
            }
            Ok(stream) if Instant::now() >= deadline => {
                drop(stream);
                panic!("stale socket unexpectedly remained connectable");
            }
            Ok(stream) => drop(stream),
            Err(_) => {}
        }
        thread::sleep(Duration::from_millis(1));
    }

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_with(
        vec!["mez".to_string(), "list".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    assert!(!stale_socket.exists());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}
