//! Explicit launcher input and diagnostic safety qualification.
//!
//! Disposable fixtures use no remote credentials, broker startup or provider
//! work. Command construction is inspected separately from actual child reaping.

use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bounded exact-child cleanup owner for the opt-in native fixture.
///
/// It is established immediately after each elected spawn so an early return or
/// assertion failure cannot leak this test's own broker. Normal paths shut down
/// and reap gracefully; on unwind this guard requests termination of only its
/// retained exact child and reaps it with a bounded synchronous wait. It never
/// discovers, groups, guesses or signals any other process, and unresolved
/// bounded reaping stays an unreaped exact child rather than a guessed PID act.
struct ExactFixtureChild {
    child: LaunchedBroker,
    forced_reaps: Arc<AtomicUsize>,
}

impl ExactFixtureChild {
    /// Takes ownership of the exact child this fixture just spawned.
    fn new(child: LaunchedBroker, forced_reaps: Arc<AtomicUsize>) -> Self {
        Self {
            child,
            forced_reaps,
        }
    }

    /// Observes this exact child without transferring ownership.
    fn child_mut(&mut self) -> &mut LaunchedBroker {
        &mut self.child
    }

    /// Requests graceful shutdown of only this exact child, then reaps it.
    async fn shutdown(&mut self) -> Result<ExitStatus> {
        self.child.shutdown_for_tests().await
    }
}

impl Drop for ExactFixtureChild {
    /// Fallback cleanup for early return or panic: terminate and boundedly reap
    /// only this exact retained child. No socket lookup, process group, sibling
    /// inference or unknown-PID signal participates.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        if self.child.start_kill_for_tests().is_err() {
            return;
        }
        for _ in 0..200 {
            match self.child.try_wait() {
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

/// Takes a startup slot's exact child, if any, into the bounded cleanup owner.
fn take_exact_child(
    slot: &mut Option<LaunchedBroker>,
    forced_reaps: &Arc<AtomicUsize>,
) -> Option<ExactFixtureChild> {
    slot.take()
        .map(|child| ExactFixtureChild::new(child, forced_reaps.clone()))
}

/// Concurrent startup callers must elect exactly one real broker child and
/// obtain distinct authenticated frontend handles. Disposal must not kill the
/// shared owner; the fixture signals and reaps only its retained child. This is
/// startup qualification, not simultaneous interactive session creation.
#[tokio::test]
#[ignore = "requires explicit MEZ_BROKER_EXECUTABLE for actual process qualification"]
async fn outbound_launcher_concurrent_callers_elect_one_actual_child() {
    let executable = std::path::PathBuf::from(
        std::env::var_os("MEZ_BROKER_EXECUTABLE").expect("explicit executable required"),
    );
    assert!(executable.is_absolute());
    let (home, env, election) = fixture();
    drop(election);
    let mut first_child = None;
    let mut second_child = None;
    let (first, second) = tokio::join!(
        connect_owned(
            &executable,
            &env,
            std::time::Duration::from_secs(10),
            &mut first_child
        ),
        connect_owned(
            &executable,
            &env,
            std::time::Duration::from_secs(10),
            &mut second_child
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
    let children = usize::from(first_child.is_some()) + usize::from(second_child.is_some());
    drop(first);
    drop(second);
    let mut shutdowns = Vec::new();
    for child in [&mut first_child, &mut second_child].into_iter().flatten() {
        let shutdown = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            child.shutdown_for_tests(),
        )
        .await;
        if child.try_wait().unwrap().is_none() {
            tokio::time::timeout(std::time::Duration::from_secs(5), child.terminate())
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
    let paths = env.config_paths().unwrap();
    assert!(!paths.root().join("outbound.sock").exists());
    drop(crate::security::remote::RemoteClientIdentity::load_or_create(paths.root()).unwrap());
    std::fs::remove_dir_all(home).unwrap();
}

/// Explicit released-binary fixture: the caller supplies a freshly built trusted
/// executable. Two authenticated readiness clients reuse one launched broker;
/// disposing them does not kill it. The test deliberately signals and reaps its
/// exact child, falling back to explicit termination on failure. No remote
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
    let mut child = None;
    let qualified = async {
        let first = connect_owned(
            &executable,
            &env,
            std::time::Duration::from_secs(10),
            &mut child,
        )
        .await?;
        let mut sibling_child = None;
        let second = connect_owned(
            Path::new("/nonexistent/unused-broker"),
            &env,
            std::time::Duration::from_secs(2),
            &mut sibling_child,
        )
        .await?;
        assert!(
            sibling_child.is_none(),
            "ready owner must not launch a sibling endpoint"
        );
        assert_ne!(first.handle()?, second.handle()?);
        drop(first);
        drop(second);
        let launched = child.as_mut().expect("elected caller retains child");
        assert!(launched.try_wait()?.is_none());
        let pid = launched
            .child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
            .expect("owned live child PID");
        rustix::process::kill_process(pid, rustix::process::Signal::TERM)
            .map_err(std::io::Error::from)?;
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), launched.wait())
            .await
            .map_err(|_| MezError::invalid_state("fixture graceful broker shutdown timed out"))??;
        assert!(status.success());
        Ok::<(), MezError>(())
    }
    .await;
    if let Some(launched) = child.as_mut()
        && launched.try_wait().unwrap().is_none()
    {
        tokio::time::timeout(std::time::Duration::from_secs(5), launched.terminate())
            .await
            .expect("fixture child cleanup must finish")
            .unwrap();
    }
    qualified.unwrap();
    let paths = env.config_paths().unwrap();
    assert!(!paths.root().join("outbound.sock").exists());
    drop(crate::security::remote::RemoteClientIdentity::load_or_create(paths.root()).unwrap());
    drop(child);
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
/// credentials or network/provider work participate.
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
    let mut owner_slot = None;
    let first_result = connect_owned(
        &executable,
        &env,
        std::time::Duration::from_secs(10),
        &mut owner_slot,
    )
    .await;
    let mut owner = take_exact_child(&mut owner_slot, &forced_reaps);
    let first = first_result.expect("fixture owner must publish readiness");
    let paths = env.config_paths().unwrap();
    let socket = paths.root().join("outbound.sock");
    let observed = std::fs::symlink_metadata(&socket).unwrap();
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
        owner
            .as_mut()
            .expect("elected owner child")
            .child_mut()
            .try_wait()
            .unwrap()
            .is_none(),
        "socket unlink must not stop the retained owner"
    );
    let mut replacement_slot = None;
    let started = tokio::time::Instant::now();
    let replacement_result = connect_owned(
        &executable,
        &env,
        std::time::Duration::from_secs(10),
        &mut replacement_slot,
    )
    .await;
    let mut replacement = take_exact_child(&mut replacement_slot, &forced_reaps);
    let error = replacement_result
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
        .as_mut()
        .expect("replacement child remains caller-owned")
        .child_mut()
        .try_wait()
        .unwrap()
        .expect("replacement child status must be reaped");
    assert!(!failed.success());
    assert!(
        owner
            .as_mut()
            .expect("elected owner child")
            .child_mut()
            .try_wait()
            .unwrap()
            .is_none(),
        "replacement failure must not disturb the original owner"
    );
    let stopped = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        owner.as_mut().expect("elected owner child").shutdown(),
    )
    .await
    .expect("fixture owner shutdown must finish")
    .expect("fixture owner shutdown must succeed");
    assert!(stopped.success());
    drop(replacement);
    let mut reused_slot = None;
    let fresh_result = connect_owned(
        &executable,
        &env,
        std::time::Duration::from_secs(20),
        &mut reused_slot,
    )
    .await;
    let mut reused = take_exact_child(&mut reused_slot, &forced_reaps);
    let fresh = fresh_result.expect("released identity must publish a fresh owner");
    fresh.handle().unwrap();
    drop(fresh);
    let shutdown = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        reused.as_mut().expect("fresh owner child").shutdown(),
    )
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
        let mut guard = ExactFixtureChild::new(child, forced_reaps.clone());
        assert!(
            guard.child_mut().try_wait().unwrap().is_none(),
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
