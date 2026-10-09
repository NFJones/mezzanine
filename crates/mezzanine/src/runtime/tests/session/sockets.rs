//! Runtime tests for session sockets behavior.

use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt};

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
        tmpdir: Some(OsString::from("/var/folders/user/T")),
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
#[cfg(not(target_os = "macos"))]
#[test]
fn default_socket_directory_uses_xdg_runtime_dir_before_tmp() {
    let env = RuntimeEnv {
        mez_tmpdir: None,
        xdg_runtime_dir: Some(OsString::from("/run/user/1000")),
        tmpdir: None,
        uid: 1000,
    };

    let directory = default_socket_directory(&env).unwrap();

    assert_eq!(directory.source, SocketDirectorySource::XdgRuntimeDir);
    assert_eq!(directory.path, PathBuf::from("/run/user/1000/mez"));
}

/// Verifies macOS uses its per-user temporary directory for daemon files.
///
/// macOS does not provide Linux-style `/run/user/<uid>` directories. This
/// regression protects the platform-native fallback used when no explicit
/// Mezzanine runtime override is configured.
#[cfg(target_os = "macos")]
#[test]
fn default_socket_directory_uses_macos_user_tmpdir() {
    let env = RuntimeEnv {
        mez_tmpdir: None,
        xdg_runtime_dir: Some(OsString::from("/run/user/1000")),
        tmpdir: Some(OsString::from("/var/folders/user/T")),
        uid: 1000,
    };

    let directory = default_socket_directory(&env).unwrap();

    assert_eq!(directory.source, SocketDirectorySource::MacOsTmpdir);
    assert_eq!(
        directory.path,
        PathBuf::from("/var/folders/user/T/mez-1000")
    );
}

/// Verifies macOS avoids a per-user temporary root that cannot hold all
/// default runtime endpoints.
///
/// The message endpoint is longer than the control endpoint. Falling back
/// before the directory is created prevents a daemon from starting and then
/// failing only while binding its auxiliary listener.
#[cfg(target_os = "macos")]
#[test]
fn default_socket_directory_uses_tmp_when_macos_tmpdir_is_too_long() {
    let env = RuntimeEnv {
        mez_tmpdir: None,
        xdg_runtime_dir: None,
        tmpdir: Some(OsString::from(format!("/{}", "a".repeat(128)))),
        uid: 1000,
    };

    let directory = default_socket_directory(&env).unwrap();

    assert_eq!(directory.source, SocketDirectorySource::Tmp);
    assert_eq!(directory.path, PathBuf::from("/tmp/mez-1000"));
}

/// Verifies macOS never falls back to a Linux-style XDG runtime path.
///
/// A stripped-down launch environment may omit `TMPDIR`. In that case macOS
/// must use its portable `/tmp` fallback rather than an unusable inherited
/// `/run/user/<uid>` value.
#[cfg(target_os = "macos")]
#[test]
fn default_socket_directory_ignores_xdg_runtime_dir_on_macos() {
    let env = RuntimeEnv {
        mez_tmpdir: None,
        xdg_runtime_dir: Some(OsString::from("/run/user/1000")),
        tmpdir: None,
        uid: 1000,
    };

    let directory = default_socket_directory(&env).unwrap();

    assert_eq!(directory.source, SocketDirectorySource::Tmp);
    assert_eq!(directory.path, PathBuf::from("/tmp/mez-1000"));
}

/// Verifies macOS rejects a relative per-user temporary directory.
///
/// Socket discovery and daemon launch exchange absolute paths, so accepting a
/// relative `TMPDIR` would make the selected endpoint depend on process working
/// directory and violate the runtime path security contract.
#[cfg(target_os = "macos")]
#[test]
fn default_socket_directory_rejects_relative_macos_tmpdir() {
    let env = RuntimeEnv {
        mez_tmpdir: None,
        xdg_runtime_dir: None,
        tmpdir: Some(OsString::from("relative")),
        uid: 1000,
    };

    let error = default_socket_directory(&env).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
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
        tmpdir: None,
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

/// Verifies named socket construction rejects a pathname before the operating
/// system reports an implementation-specific bind failure.
///
/// A typed error lets callers distinguish invalid endpoint selection from
/// directory ownership or listener startup errors on every supported Unix host.
#[test]
fn socket_path_for_name_rejects_paths_beyond_the_platform_limit() {
    let directory = Path::new("/").join("a".repeat(128));

    let error = socket_path_for_name(&directory, "control.sock").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("Unix socket limit"));
}

/// Hosted session names encode the complete numeric identity compactly, so
/// distinct and maximum-width IDs remain collision-free without consuming the
/// verbose compatibility prefix used by older hosted paths.
#[test]
fn hosted_session_socket_names_are_compact_and_collision_free() {
    let directory = Path::new("/tmp/mez-hosted");
    let first = crate::runtime::hosted_session_socket_path(
        directory,
        &mez_core::ids::SessionId::new('$', 1),
    )
    .unwrap();
    let second = crate::runtime::hosted_session_socket_path(
        directory,
        &mez_core::ids::SessionId::new('$', 2),
    )
    .unwrap();
    let maximum = crate::runtime::hosted_session_socket_path(
        directory,
        &mez_core::ids::SessionId::new('$', u64::MAX),
    )
    .unwrap();

    assert_eq!(first.file_name().unwrap(), "h1.sock");
    assert_eq!(second.file_name().unwrap(), "h2.sock");
    assert_eq!(maximum.file_name().unwrap(), "hffffffffffffffff.sock");
    assert_ne!(first, second);
}

/// A runtime root that cannot fit even the compact maximum-width hosted name
/// fails before bind with the standard actionable Unix pathname diagnostic.
#[test]
fn hosted_session_socket_path_rejects_an_impossible_runtime_root() {
    let directory = Path::new("/").join("a".repeat(128));
    let error = crate::runtime::hosted_session_socket_path(
        &directory,
        &mez_core::ids::SessionId::new('$', u64::MAX),
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("Unix socket limit"));
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

/// Verifies auxiliary endpoint derivation rejects a suffix that crosses the
/// platform pathname limit even though its control endpoint is valid.
///
/// This protects foreground services from binding a control socket successfully
/// and failing later when they derive the message or event listener path.
#[test]
fn auxiliary_socket_paths_reject_suffixes_beyond_the_platform_limit() {
    let directory = Path::new("/").join("a".repeat(94));
    let control = directory.join("x.sock");

    let error = auxiliary_socket_path_for_control_socket(&control, AuxiliarySocketKind::Message)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("Unix socket limit"));
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

/// Verifies native peer credential lookup reports the connected process UID.
///
/// Linux and Apple-family hosts expose peer identity through different socket
/// APIs. Exercising the production descriptor adapter with a connected Unix
/// stream protects both implementations from target-specific compilation or
/// credential-conversion regressions.
#[test]
fn unix_peer_uid_reports_effective_uid_for_connected_stream() {
    let (_client, server) = UnixStream::pair().unwrap();

    let peer_uid = unix_peer_uid(server.as_raw_fd()).unwrap();

    assert_eq!(peer_uid, effective_uid());
}

/// Verifies stale socket cleanup removes only unserved runtime sockets.
///
/// This regression scenario protects startup cleanup from deleting live Mez
/// endpoints while still removing refused socket files left behind by crashed
/// processes.
/// The stale endpoint is created only in a bounded single-test child and observed
/// after its exit, so unrelated parent forks cannot inherit its listener. A second
/// live endpoint retains a duplicate after dropping its original descriptor.
#[test]
fn prune_stale_socket_files_removes_refused_socket_and_preserves_live_socket() {
    let root = select_socket_fixture_root(
        &fs::canonicalize(std::env::temp_dir()).unwrap(),
        &fs::canonicalize("/tmp").unwrap(),
        std::process::id(),
        rand::random::<u64>(),
    )
    .unwrap();
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    ensure_private_socket_directory(&root, effective_uid()).unwrap();
    let stale = root.join("stale.sock");
    let live = root.join("live.sock");
    let duplicated = root.join("duplicate.sock");
    let non_socket = root.join("not-a-socket.sock");

    create_stale_socket_in_exited_fixture(&stale);
    let original = std::os::unix::net::UnixListener::bind(&duplicated).unwrap();
    let inherited_listener = original.try_clone().unwrap();
    drop(original);
    let _live_listener = bind_control_socket(&live, effective_uid()).unwrap();
    fs::write(&non_socket, "leave this alone").unwrap();

    let removed = prune_stale_socket_files_in_directory(&root, effective_uid()).unwrap();

    assert_eq!(removed, 1);
    assert!(!stale.exists());
    assert!(live.exists());
    assert!(duplicated.exists());
    assert!(non_socket.exists());
    drop(inherited_listener);

    let _ = fs::remove_dir_all(&root);
}

/// Chooses a root whose longest socket fits Linux and macOS, counting exact Unix
/// path bytes. Inputs are already physical directories; fallback does not expand
/// permission or reuse an existing fixture. No filesystem effects occur here.
fn select_socket_fixture_root(
    preferred: &Path,
    fallback: &Path,
    pid: u32,
    nonce: u64,
) -> Result<PathBuf> {
    let name = format!("mez-stale-{pid}-{nonce:016x}");
    for base in [preferred, fallback] {
        let root = base.join(&name);
        if root.join("duplicate.sock").as_os_str().as_bytes().len() <= 103 {
            return Ok(root);
        }
    }
    Err(MezError::invalid_args(
        "socket fixture root exceeds portable Unix path limit",
    ))
}

/// macOS's canonical /private/var/folders TMPDIR can exceed sun_path even when
/// the shorter stale.sock fits. The selector must evaluate the longest pathname,
/// preserve short preferred roots, use a bounded physical fallback for long or
/// multibyte bases, and reject when neither base fits, without creating any files.
#[test]
fn stale_socket_fixture_root_bounds_include_long_mac_os_temp_paths() {
    let long = PathBuf::from(format!("/private/var/folders/{}/T", "a".repeat(33)));
    let name = "mez-stale-12345-0123456789abcdef";
    assert!(
        long.join(name)
            .join("duplicate.sock")
            .as_os_str()
            .as_bytes()
            .len()
            > 103
    );
    let fallback = Path::new("/private/tmp");
    let chosen = select_socket_fixture_root(&long, fallback, 12345, 0x0123456789abcdef).unwrap();
    assert_eq!(chosen, fallback.join(name));
    assert!(chosen.join("duplicate.sock").as_os_str().as_bytes().len() <= 103);
    assert_eq!(
        select_socket_fixture_root(Path::new("/tmp"), fallback, 12345, 0x0123456789abcdef).unwrap(),
        Path::new("/tmp").join(name)
    );
    let multibyte = PathBuf::from(format!("/{}", "é".repeat(45)));
    assert!(
        select_socket_fixture_root(&multibyte, fallback, u32::MAX, u64::MAX)
            .unwrap()
            .starts_with(fallback)
    );
    assert!(select_socket_fixture_root(&long, &long, u32::MAX, u64::MAX).is_err());
}

/// Creates a socket inode only inside an isolated exact-test child. Parent-side
/// fork/exec activity cannot inherit the child's subsequently created descriptor.
/// A finite wait kills/reaps only this fixture-owned child on failure; success
/// requires process exit before returning, not pathname existence or a sleep.
fn create_stale_socket_in_exited_fixture(path: &Path) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "runtime::tests::session::sockets::stale_socket_fixture_child",
            "--quiet",
        ])
        .env_clear()
        .env("MEZ_TEST_STALE_SOCKET_PATH", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "stale socket fixture child failed: {status}"
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("stale socket fixture child exceeded deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_socket());
}

/// Guarded single-test child binds/drops only the supplied fixture-owned stale
/// socket. No child fork, daemon/provider, inherited listener or cleanup retry is
/// involved. Without the private test environment marker this test is inert.
#[test]
fn stale_socket_fixture_child() {
    let Some(path) = std::env::var_os("MEZ_TEST_STALE_SOCKET_PATH").map(PathBuf::from) else {
        return;
    };
    assert_eq!(path.file_name().unwrap(), "stale.sock");
    assert!(
        path.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("mez-stale-")
    );
    let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
    drop(listener);
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
