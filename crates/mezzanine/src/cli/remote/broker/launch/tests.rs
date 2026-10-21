//! Explicit launcher input and diagnostic safety qualification.
//!
//! Disposable fixtures use no remote credentials or provider work. Command
//! construction is inspected separately from actual child reaping, and the
//! opt-in native fixtures bind only their own isolated local endpoint.

use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bounded exact-child cleanup ownership for the native fixtures.
///
/// It owns the caller slot from before startup is awaited, so a returned error,
/// cancellation, failed assertion or unwind cannot leak this test's own broker.
/// Normal paths shut down and reap gracefully; the fallback requests termination
/// of only the exact retained child and reaps it with a bounded synchronous
/// wait. It never discovers, groups, guesses or signals another process, and
/// unresolved bounded reaping stays an unreaped exact child rather than a
/// guessed-PID action.
struct ExactFixtureChild {
    slot: Option<LaunchedBroker>,
    forced_reaps: Arc<AtomicUsize>,
}

impl ExactFixtureChild {
    /// An empty guard to pass into startup, which may fill it with one exact child.
    fn empty(forced_reaps: Arc<AtomicUsize>) -> Self {
        Self {
            slot: None,
            forced_reaps,
        }
    }

    /// A guard that already holds this fixture's exact spawned child.
    fn holding(child: LaunchedBroker, forced_reaps: Arc<AtomicUsize>) -> Self {
        Self {
            slot: Some(child),
            forced_reaps,
        }
    }

    /// Borrows the caller slot exactly as startup expects it.
    fn slot_mut(&mut self) -> &mut Option<LaunchedBroker> {
        &mut self.slot
    }

    /// Reports whether an exact child is retained in this guard.
    fn is_held(&self) -> bool {
        self.slot.is_some()
    }

    /// Observes the retained exact child, if startup produced one.
    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        match self.slot.as_mut() {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }

    /// Requests graceful shutdown of only this exact child, then reaps it.
    async fn shutdown(&mut self) -> Result<ExitStatus> {
        match self.slot.as_mut() {
            Some(child) => child.shutdown_for_tests().await,
            None => Err(MezError::invalid_state("no exact fixture child retained")),
        }
    }

    /// Forces termination of only this exact child, then reaps it.
    async fn terminate(&mut self) -> Result<ExitStatus> {
        match self.slot.as_mut() {
            Some(child) => child.terminate().await,
            None => Err(MezError::invalid_state("no exact fixture child retained")),
        }
    }
}

impl Drop for ExactFixtureChild {
    /// Fallback cleanup for any exit path: terminate and boundedly reap only the
    /// exact child still held in this slot.
    fn drop(&mut self) {
        let Some(mut child) = self.slot.take() else {
            return;
        };
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        if child.start_kill_for_tests().is_err() {
            return;
        }
        for _ in 0..200 {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.forced_reaps.fetch_add(1, Ordering::SeqCst);
                    return;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(_) => return,
            }
        }
        eprintln!("exact fixture child reap unresolved within the bounded guard wait");
    }
}

/// Concurrent startup callers must elect exactly one real broker child and
/// obtain distinct authenticated frontend handles. Disposal must not kill the
/// shared owner; the fixture's bounded exact-child guards signal and reap only
/// the children it spawned, including on early return. This is startup
/// qualification, not simultaneous interactive session creation.
#[tokio::test]
#[ignore = "requires explicit MEZ_BROKER_EXECUTABLE for actual process qualification"]
async fn outbound_launcher_concurrent_callers_elect_one_actual_child() {
    let executable = std::path::PathBuf::from(
        std::env::var_os("MEZ_BROKER_EXECUTABLE").expect("explicit executable required"),
    );
    assert!(executable.is_absolute());
    let (home, env, election) = fixture();
    drop(election);
    let forced_reaps = Arc::new(AtomicUsize::new(0));
    let mut first_guard = ExactFixtureChild::empty(forced_reaps.clone());
    let mut second_guard = ExactFixtureChild::empty(forced_reaps.clone());
    let (first, second) = tokio::join!(
        connect_owned(
            &executable,
            &env,
            std::time::Duration::from_secs(10),
            first_guard.slot_mut()
        ),
        connect_owned(
            &executable,
            &env,
            std::time::Duration::from_secs(10),
            second_guard.slot_mut()
        ),
    );
    // Collect the verdict before cleanup so a failed startup still retains the
    // exact child handles for explicit disposal, not socket-derived signalling.
    let verdict = match (&first, &second) {
        (Ok(first), Ok(second)) => first
            .handle()
            .and_then(|first| second.handle().map(|second| first != second)),
        _ => Ok(false),
    };
    let children = usize::from(first_guard.is_held()) + usize::from(second_guard.is_held());
    drop(first);
    drop(second);
    let mut shutdowns = Vec::new();
    for guard in [&mut first_guard, &mut second_guard] {
        if !guard.is_held() {
            continue;
        }
        let shutdown =
            tokio::time::timeout(std::time::Duration::from_secs(10), guard.shutdown()).await;
        if guard.try_wait().unwrap().is_none() {
            tokio::time::timeout(std::time::Duration::from_secs(5), guard.terminate())
                .await
                .unwrap()
                .unwrap();
        }
        shutdowns.push(shutdown);
    }
    assert!(
        verdict.unwrap(),
        "both callers require distinct authenticated handles"
    );
    assert_eq!(
        children, 1,
        "one elected launcher must retain the sole child"
    );
    assert!(
        shutdowns
            .into_iter()
            .all(|result| result.is_ok_and(|result| result.is_ok_and(|status| status.success())))
    );
    assert_eq!(
        forced_reaps.load(Ordering::SeqCst),
        0,
        "graceful teardown, not the unwind guard, must reap the exact child"
    );
    let paths = env.config_paths().unwrap();
    assert!(!paths.root().join("outbound.sock").exists());
    drop(crate::security::remote::RemoteClientIdentity::load_or_create(paths.root()).unwrap());
    std::fs::remove_dir_all(home).unwrap();
}

/// Explicit released-binary fixture: the caller supplies a freshly built trusted
/// executable. Two authenticated readiness clients reuse one launched broker;
/// disposing them does not kill it. The test deliberately signals and reaps its
/// exact child through the bounded guard, including on early return. No remote
/// sessions or provider work occur, and ordinary CLI routing is not qualified.
#[tokio::test]
#[ignore = "requires explicit MEZ_BROKER_EXECUTABLE for actual process qualification"]
async fn outbound_launcher_actual_broker_retains_child_and_reuses_readiness() {
    let executable = std::path::PathBuf::from(
        std::env::var_os("MEZ_BROKER_EXECUTABLE").expect("explicit broker executable required"),
    );
    assert!(executable.is_absolute());
    let (home, env, election) = fixture();
    drop(election);
    let forced_reaps = Arc::new(AtomicUsize::new(0));
    let qualified = async {
        let mut guarded = ExactFixtureChild::empty(forced_reaps.clone());
        let first = connect_owned(
            &executable,
            &env,
            std::time::Duration::from_secs(10),
            guarded.slot_mut(),
        )
        .await?;
        let mut sibling_guard = ExactFixtureChild::empty(forced_reaps.clone());
        let second = connect_owned(
            Path::new("/nonexistent/unused-broker"),
            &env,
            std::time::Duration::from_secs(2),
            sibling_guard.slot_mut(),
        )
        .await?;
        assert!(
            !sibling_guard.is_held(),
            "ready owner must not launch a sibling endpoint"
        );
        assert_ne!(first.handle()?, second.handle()?);
        drop(first);
        drop(second);
        assert!(guarded.try_wait()?.is_none());
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), guarded.shutdown())
            .await
            .map_err(|_| MezError::invalid_state("fixture graceful broker shutdown timed out"))??;
        assert!(status.success());
        Ok::<(), MezError>(())
    }
    .await;
    qualified.unwrap();
    assert_eq!(
        forced_reaps.load(Ordering::SeqCst),
        0,
        "graceful shutdown, not the unwind guard, must reap the exact child"
    );
    let paths = env.config_paths().unwrap();
    assert!(!paths.root().join("outbound.sock").exists());
    drop(crate::security::remote::RemoteClientIdentity::load_or_create(paths.root()).unwrap());
    std::fs::remove_dir_all(home).unwrap();
}

/// An exited launcher with no published socket must be classified promptly and
/// distinctly instead of disguised as an exhausted readiness deadline. The
/// caller retains and reaps only its own exact child, and a second startup
/// attempt cannot silently replace that retained process evidence.
#[tokio::test]
async fn outbound_launcher_owned_startup_retains_failed_child() {
    let (home, env, election) = fixture();
    drop(election);
    let mut child = None;
    let started = tokio::time::Instant::now();
    let error = connect_owned(
        Path::new("/bin/true"),
        &env,
        std::time::Duration::from_secs(10),
        &mut child,
    )
    .await
    .err()
    .expect("a clean no-publication exit cannot report readiness");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "exited child must be reported before the retained deadline"
    );
    assert!(
        error
            .message()
            .contains("exited successfully before local readiness"),
        "clean no-publication exit needs its own classification: {error}"
    );
    assert!(!error.message().contains("before timeout"));
    let launched = child
        .as_mut()
        .expect("spawned child must survive readiness failure");
    assert!(launched.try_wait().unwrap().unwrap().success());
    assert!(
        connect_owned(
            Path::new("/bin/true"),
            &env,
            std::time::Duration::from_secs(1),
            &mut child
        )
        .await
        .is_err()
    );
    assert!(
        child
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .unwrap()
            .success()
    );
    drop(child);
    let paths = env.config_paths().unwrap();
    assert!(!paths.root().join("outbound.sock").exists());
    drop(StartupElection::acquire(paths.root()).unwrap().unwrap());
    std::fs::remove_dir_all(home).unwrap();
}

/// Creates a private primary root and retains its elected launcher guard.
fn fixture() -> (std::path::PathBuf, CliEnv, StartupElection) {
    let home = std::env::temp_dir().join(format!("mez-launch-{:032x}", rand::random::<u128>()));
    let env = CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    let election = StartupElection::acquire(paths.root()).unwrap().unwrap();
    (home, env, election)
}

/// Fixed argv, explicit HOME/cwd and a cleared ambient environment must not
/// export frontend routing or credentials. Invalid paths reject before spawn.
#[test]
fn outbound_launcher_inputs_are_explicit_and_environment_isolated() {
    let (home, env, election) = fixture();
    let command = launch_command(Path::new("/bin/true"), &env, &election).unwrap();
    assert_eq!(command.get_program(), "/bin/true");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        vec!["remote", "outbound-serve"]
    );
    assert_eq!(command.get_current_dir(), Some(home.as_path()));
    assert_eq!(
        command.get_envs().collect::<Vec<_>>(),
        vec![(std::ffi::OsStr::new("HOME"), Some(home.as_os_str()))]
    );
    assert!(launch_command(Path::new("mez"), &env, &election).is_err());
    let foreign = CliEnv {
        home: Some(home.join("other")),
        ..Default::default()
    };
    assert!(launch_command(Path::new("/bin/true"), &foreign, &election).is_err());
    drop(command);
    drop(election);
    std::fs::remove_dir_all(home).unwrap();
}

/// Diagnostic publication preserves authored unsafe entries rather than
/// following symlinks or changing permissions on permissive/hardlinked files.
#[test]
fn outbound_launcher_diagnostics_reject_unsafe_entries() {
    let (home, env, election) = fixture();
    let paths = env.config_paths().unwrap();
    let path = paths.root().join(DIAGNOSTIC_NAME);
    let target = paths.root().join("authored");
    std::fs::write(&target, b"preserve").unwrap();
    symlink(&target, &path).unwrap();
    assert!(launch_command(Path::new("/bin/true"), &env, &election).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"existing diagnostics").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(launch_command(Path::new("/bin/true"), &env, &election).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"existing diagnostics");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&path, paths.root().join("alias.log")).unwrap();
    assert!(launch_command(Path::new("/bin/true"), &env, &election).is_err());
    drop(election);
    std::fs::remove_dir_all(home).unwrap();
}

/// A real child remains in the caller's exact handle and is deliberately
/// reaped. This verifies process transport, not actual broker readiness.
#[tokio::test]
async fn outbound_launcher_reaps_its_exact_child() {
    let (home, env, election) = fixture();
    let mut launched = LaunchedBroker::spawn(Path::new("/bin/true"), &env, &election).unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), launched.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    assert!(launched.try_wait().unwrap().unwrap().success());
    let diagnostic = env.config_paths().unwrap().root().join(DIAGNOSTIC_NAME);
    assert_eq!(
        std::fs::metadata(diagnostic).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(launched);
    drop(election);
    std::fs::remove_dir_all(home).unwrap();
}

/// A real immediately failing child must be classified before the retained
/// deadline with its exact non-success status instead of a readiness timeout.
/// The caller reaps only its own handle, and startup classification creates,
/// replaces or deletes no discovery, lock or key entry.
#[tokio::test]
async fn outbound_launcher_reports_immediate_nonzero_child_exit() {
    let (home, env, election) = fixture();
    drop(election);
    let mut child = None;
    let started = tokio::time::Instant::now();
    let error = connect_owned(
        Path::new("/bin/false"),
        &env,
        std::time::Duration::from_secs(10),
        &mut child,
    )
    .await
    .err()
    .expect("a nonzero child exit cannot report readiness");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "nonzero child exit must be reported promptly"
    );
    assert!(error.message().contains("exited before local readiness"));
    assert!(!error.message().contains("before timeout"));
    let status = child
        .as_mut()
        .expect("failed child remains caller-owned")
        .try_wait()
        .unwrap()
        .expect("failed child status must be reaped");
    assert!(!status.success());
    assert_eq!(status.code(), Some(1));
    let paths = env.config_paths().unwrap();
    assert!(!paths.root().join("outbound.sock").exists());
    drop(child);
    drop(StartupElection::acquire(paths.root()).unwrap().unwrap());
    std::fs::remove_dir_all(home).unwrap();
}

/// Real synthetic disposable-root control for a stranded owner: one actual
/// broker publishes, only this fixture's socket pathname is unlinked while the
/// retained endpoint identity lock stays held, and a replacement launch must be
/// reported as an exact-child exit promptly. The original owner and sibling
/// evidence stay alive until this test explicitly reaps only its own child; a
/// same-identity replacement then publishes normally. No user broker,
/// credentials, remote session or provider work participate; the fixture broker
/// still binds its isolated local endpoint transport with relay, lookup and port
/// mapping disabled, and the fixture performs no remote dial.
#[tokio::test]
#[ignore = "requires explicit MEZ_BROKER_EXECUTABLE for actual process qualification"]
async fn outbound_launcher_reports_replacement_exit_after_fixture_socket_unlink() {
    let executable = std::path::PathBuf::from(
        std::env::var_os("MEZ_BROKER_EXECUTABLE").expect("explicit broker executable required"),
    );
    assert!(executable.is_absolute());
    let (home, env, election) = fixture();
    drop(election);
    let forced_reaps = Arc::new(AtomicUsize::new(0));
    let paths = env.config_paths().unwrap();
    let socket = paths.root().join("outbound.sock");
    // The retained identity lock is the longest named path this fixture creates,
    // so bounding it also bounds the publication socket and diagnostics file.
    let longest_bytes = paths
        .root()
        .join("remote/client/endpoint.lock")
        .as_os_str()
        .to_string_lossy()
        .len();
    assert!(
        longest_bytes <= 104,
        "disposable fixture root must stay inside the Unix bound: {longest_bytes} bytes"
    );
    let mut owner = ExactFixtureChild::empty(forced_reaps.clone());
    let first = connect_owned(
        &executable,
        &env,
        std::time::Duration::from_secs(10),
        owner.slot_mut(),
    )
    .await
    .expect("fixture owner must publish readiness");
    let observed = std::fs::symlink_metadata(&socket).unwrap();
    let key_path = paths.root().join("remote/client/endpoint.key");
    let key_before = std::fs::read(&key_path).unwrap();
    // A sibling frontend admitted before the unlink must survive the replacement
    // failure; only this fixture's own exact children are reaped.
    let mut sibling_guard = ExactFixtureChild::empty(forced_reaps.clone());
    let sibling = connect_owned(
        &executable,
        &env,
        std::time::Duration::from_secs(5),
        sibling_guard.slot_mut(),
    )
    .await
    .expect("sibling readiness must reuse the published owner");
    assert!(
        !sibling_guard.is_held(),
        "a ready owner must not launch a sibling endpoint"
    );
    assert_ne!(sibling.handle().unwrap(), first.handle().unwrap());
    assert!(paths.root().join("remote/client/endpoint.lock").exists());
    drop(first);
    let current = std::fs::symlink_metadata(&socket).unwrap();
    assert_eq!(
        (current.dev(), current.ino()),
        (observed.dev(), observed.ino()),
        "only this fixture's exact publication may be unlinked"
    );
    std::fs::remove_file(&socket).unwrap();
    assert!(
        owner.try_wait().unwrap().is_none(),
        "socket unlink must not stop the retained owner"
    );
    let mut replacement = ExactFixtureChild::empty(forced_reaps.clone());
    let started = tokio::time::Instant::now();
    let error = connect_owned(
        &executable,
        &env,
        std::time::Duration::from_secs(10),
        replacement.slot_mut(),
    )
    .await
    .err()
    .expect("replacement cannot publish over the retained identity owner");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "replacement exit must be classified rather than waited out"
    );
    assert!(
        error.message().contains("exited before local readiness"),
        "replacement exit needs the exact-child classification: {error}"
    );
    let failed = replacement
        .try_wait()
        .unwrap()
        .expect("replacement child status must be reaped");
    assert!(!failed.success());
    assert!(
        owner.try_wait().unwrap().is_none(),
        "replacement failure must not disturb the original owner"
    );
    assert!(
        paths.root().join("remote/client/endpoint.lock").exists(),
        "the retained identity lock must stay held by the original owner"
    );
    assert!(
        sibling.retained_stream_open_for_tests(),
        "the admitted sibling stream must survive the replacement failure"
    );
    drop(sibling);
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(10), owner.shutdown())
        .await
        .expect("fixture owner shutdown must finish")
        .expect("fixture owner shutdown must succeed");
    assert!(stopped.success());
    drop(replacement);
    let mut reused = ExactFixtureChild::empty(forced_reaps.clone());
    let fresh = connect_owned(
        &executable,
        &env,
        std::time::Duration::from_secs(20),
        reused.slot_mut(),
    )
    .await
    .expect("released identity must publish a fresh owner");
    fresh.handle().unwrap();
    assert_eq!(
        std::fs::read(&key_path).unwrap(),
        key_before,
        "fresh publication must reuse the same protected identity key"
    );
    drop(fresh);
    let shutdown = tokio::time::timeout(std::time::Duration::from_secs(10), reused.shutdown())
        .await
        .expect("fresh owner shutdown must finish")
        .expect("fresh owner shutdown must succeed");
    assert!(shutdown.success());
    assert_eq!(
        forced_reaps.load(Ordering::SeqCst),
        0,
        "graceful teardown, not the unwind guard, must reap the exact children"
    );
    drop((reused, owner));
    assert!(!paths.root().join("outbound.sock").exists());
    std::fs::remove_dir_all(home).unwrap();
}

/// Deliberate early-return control: the bounded exact-child guard must terminate
/// and reap only the child this fixture spawned, without discovery, process
/// groups or guessed PIDs. This is the unwind/early-return cleanup owner used by
/// the opt-in native fixture, so an assertion failure there cannot leak its
/// retained broker while sibling test children stay untouched.
#[tokio::test]
async fn outbound_launcher_exact_fixture_guard_reaps_on_early_return() {
    let (home, env, election) = fixture();
    let script = home.join("fixture-live-child");
    std::fs::write(&script, b"#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let forced_reaps = Arc::new(AtomicUsize::new(0));
    {
        let child = LaunchedBroker::spawn(&script, &env, &election).unwrap();
        let mut guard = ExactFixtureChild::holding(child, forced_reaps.clone());
        assert!(
            guard.try_wait().unwrap().is_none(),
            "fixture child must be live before the deliberate early return"
        );
    }
    assert_eq!(
        forced_reaps.load(Ordering::SeqCst),
        1,
        "the bounded guard must reap exactly this retained child"
    );
    drop(election);
    std::fs::remove_dir_all(home).unwrap();
}

/// A retained live exact child blocks another launch with the explicit retained
/// conflict, keeps the same live handle, and never deletes discovery/lock state or
/// signals a discovered process. This proves the caller-slot ownership boundary
/// rather than socket-derived kill authority.
#[tokio::test]
async fn outbound_launcher_retained_live_child_blocks_relaunch() {
    let (home, env, election) = fixture();
    let script = home.join("fixture-live-child");
    std::fs::write(&script, b"#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let forced_reaps = Arc::new(AtomicUsize::new(0));
    let child = LaunchedBroker::spawn(&script, &env, &election).unwrap();
    let mut guard = ExactFixtureChild::holding(child, forced_reaps.clone());
    drop(election);
    let error = connect_owned(
        Path::new("/bin/true"),
        &env,
        std::time::Duration::from_secs(2),
        guard.slot_mut(),
    )
    .await
    .err()
    .expect("a retained live child must block another launch");
    assert!(
        error.message().contains("already retained"),
        "retained-child conflict needed: {error}"
    );
    assert!(
        guard.try_wait().unwrap().is_none(),
        "the retained exact child must stay live"
    );
    drop(guard);
    assert_eq!(
        forced_reaps.load(Ordering::SeqCst),
        1,
        "fixture teardown must reap only its own retained child"
    );
    std::fs::remove_dir_all(home).unwrap();
}

/// Cancelling startup retains its exact live child in the caller slot: no kill,
/// relaunch or discovery cleanup is inferred from readiness absence. Only the
/// caller's explicit teardown reaps that child.
#[tokio::test]
async fn outbound_launcher_cancellation_retains_exact_child_without_kill() {
    let (home, env, election) = fixture();
    let script = home.join("fixture-live-child");
    std::fs::write(&script, b"#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(election);
    let forced_reaps = Arc::new(AtomicUsize::new(0));
    let mut guard = ExactFixtureChild::empty(forced_reaps.clone());
    let cancelled = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        connect_owned(
            &script,
            &env,
            std::time::Duration::from_secs(5),
            guard.slot_mut(),
        ),
    )
    .await;
    assert!(cancelled.is_err(), "fixture startup must not become ready");
    assert!(
        guard.is_held(),
        "cancelled startup must retain its exact child"
    );
    assert!(
        guard.try_wait().unwrap().is_none(),
        "cancellation must not kill the exact child"
    );
    drop(guard);
    assert_eq!(
        forced_reaps.load(Ordering::SeqCst),
        1,
        "explicit fixture teardown must reap only its spawned child"
    );
    std::fs::remove_dir_all(home).unwrap();
}
