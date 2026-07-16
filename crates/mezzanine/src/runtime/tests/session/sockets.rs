//! Runtime tests for session sockets behavior.

use super::*;

/// Verifies default socket directory prefers mez tmpdir.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn default_socket_directory_prefers_mez_tmpdir() {
    let env = RuntimeEnv {
        mez_tmpdir: Some(OsString::from("/run/user/custom")),
        xdg_runtime_dir: Some(OsString::from("/run/user/1000")),
        uid: 1000,
    };

    let directory = default_socket_directory(&env).unwrap();

    assert_eq!(directory.source, SocketDirectorySource::MezTmpdir);
    assert_eq!(directory.path, PathBuf::from("/run/user/custom/mez-1000"));
}

/// Verifies default socket directory uses xdg runtime dir before tmp.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn default_socket_directory_uses_xdg_runtime_dir_before_tmp() {
    let env = RuntimeEnv {
        mez_tmpdir: None,
        xdg_runtime_dir: Some(OsString::from("/run/user/1000")),
        uid: 1000,
    };

    let directory = default_socket_directory(&env).unwrap();

    assert_eq!(directory.source, SocketDirectorySource::XdgRuntimeDir);
    assert_eq!(directory.path, PathBuf::from("/run/user/1000/mez"));
}

/// Verifies default socket directory rejects relative env paths.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn default_socket_directory_rejects_relative_env_paths() {
    let env = RuntimeEnv {
        mez_tmpdir: Some(OsString::from("relative")),
        xdg_runtime_dir: None,
        uid: 1000,
    };

    let error = default_socket_directory(&env).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies ensure private socket directory creates mode 0700 directory.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn ensure_private_socket_directory_creates_mode_0700_directory() {
    let root = std::env::temp_dir().join(format!("mez-runtime-test-create-{}", std::process::id()));
    let path = root.join("socket");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();

    ensure_private_socket_directory(&path, effective_uid()).unwrap();
    let metadata = fs::metadata(&path).unwrap();

    assert!(metadata.is_dir());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);

    let _ = fs::remove_dir_all(&root);
}

/// Verifies socket name must be single component.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn socket_name_must_be_single_component() {
    let error = socket_path_for_name(Path::new("/tmp/mez-1000"), "../bad").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies auxiliary socket paths are derived from control socket name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auxiliary_socket_paths_are_derived_from_control_socket_name() {
    let control = Path::new("/tmp/mez-1000/default.sock");

    let message =
        auxiliary_socket_path_for_control_socket(control, AuxiliarySocketKind::Message).unwrap();
    let event =
        auxiliary_socket_path_for_control_socket(control, AuxiliarySocketKind::Event).unwrap();

    assert_eq!(message, PathBuf::from("/tmp/mez-1000/default.message.sock"));
    assert_eq!(event, PathBuf::from("/tmp/mez-1000/default.event.sock"));
}

/// Verifies auxiliary socket paths preserve nonstandard control socket names.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auxiliary_socket_paths_preserve_nonstandard_control_socket_names() {
    let control = Path::new("/tmp/mez-1000/control");

    let message =
        auxiliary_socket_path_for_control_socket(control, AuxiliarySocketKind::Message).unwrap();

    assert_eq!(message, PathBuf::from("/tmp/mez-1000/control.message.sock"));
}

/// Verifies unix peer uid authorization rejects uid mismatch.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn unix_peer_uid_authorization_rejects_uid_mismatch() {
    let error = authorize_unix_peer_uid(1001, 1000).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies unix peer authorization accepts same user stream.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn unix_peer_authorization_accepts_same_user_stream() {
    let (_client, server) = UnixStream::pair().unwrap();

    authorize_unix_peer(&server, effective_uid()).unwrap();
}

/// Verifies stale socket cleanup removes only unserved runtime sockets.
///
/// This regression scenario protects startup cleanup from deleting live Mez
/// endpoints while still removing refused socket files left behind by crashed
/// processes.
#[test]
fn prune_stale_socket_files_removes_refused_socket_and_preserves_live_socket() {
    let root = std::env::temp_dir().join(format!(
        "mez-runtime-test-stale-sockets-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    ensure_private_socket_directory(&root, effective_uid()).unwrap();
    let stale = root.join("stale.sock");
    let live = root.join("live.sock");
    let non_socket = root.join("not-a-socket.sock");

    let stale_listener = std::os::unix::net::UnixListener::bind(&stale).unwrap();
    drop(stale_listener);
    let _live_listener = bind_control_socket(&live, effective_uid()).unwrap();
    fs::write(&non_socket, "leave this alone").unwrap();

    let removed = prune_stale_socket_files_in_directory(&root, effective_uid()).unwrap();

    assert_eq!(removed, 1);
    assert!(!stale.exists());
    assert!(live.exists());
    assert!(non_socket.exists());

    let _ = fs::remove_dir_all(&root);
}

/// Verifies pane environment places socket path first.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_environment_places_socket_path_first() {
    let mut ids = IdFactory::default();
    let session = ids.session();
    let window = ids.window();
    let pane = ids.pane();

    let env = pane_environment(
        Path::new("/tmp/mez-1000/default.sock"),
        &session,
        &window,
        &pane,
    )
    .unwrap();

    let separator = MEZ_ENV_FIELD_SEPARATOR.to_string();
    let fields = env.mez.split(MEZ_ENV_FIELD_SEPARATOR).collect::<Vec<_>>();
    assert_eq!(fields[0], "/tmp/mez-1000/default.sock");
    assert_eq!(fields[1], format!("session={session}"));
    assert!(env.mez.contains(&separator));
    assert_eq!(env.session, session.to_string());
    assert_eq!(env.window, window.to_string());
    assert_eq!(env.pane, pane.to_string());
    assert_eq!(env.term, DEFAULT_PANE_TERM);
}
