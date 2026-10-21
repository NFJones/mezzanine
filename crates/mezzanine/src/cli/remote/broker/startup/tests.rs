//! Startup orchestration over real election and authenticated local readiness.
//!
//! Injected launch notification drives the actual foreground broker composition
//! without spawning an unowned background process or enabling ordinary CLI attach.
//! Fixture launchers model launch and exact-child liveness without discovering or
//! signaling a real process, so exit classification stays deterministic.

use super::*;
use crate::cli::CliEnv;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

mod compatibility;

/// Adapts one launch-only closure to the startup port.
///
/// Fixtures that never spawn or observe a real child use this adapter to keep
/// the launch request explicit. `observe_exit` always reports no child, so a
/// live or absent startup remains classified as a live-but-not-ready timeout.
struct LaunchOnly<F>(Option<F>);

impl<F> LaunchOnly<F> {
    /// Wraps the single launch closure this startup attempt may invoke.
    fn new(launch: F) -> Self {
        Self(Some(launch))
    }
}

impl<F> StartupLauncher for LaunchOnly<F>
where
    F: FnOnce(&StartupElection) -> Result<()>,
{
    fn launch(&mut self, election: &StartupElection) -> Result<()> {
        let launch = self
            .0
            .take()
            .ok_or_else(|| MezError::invalid_state("fixture launch already consumed"))?;
        launch(election)
    }

    fn observe_exit(&mut self) -> Result<Option<ExitStatus>> {
        Ok(None)
    }
}

/// Models one launched exact child whose exit is reported after the launch
/// request without spawning or signaling a process.
///
/// The configured status stays observable so repeated probes are idempotent, as
/// with an already-reaped exact handle. The counters prove readiness ran beside
/// observation rather than after an exhausted deadline.
struct ExitingLauncher {
    launches: Arc<AtomicUsize>,
    observations: Arc<AtomicUsize>,
    launched: bool,
    status_raw: i32,
}

impl StartupLauncher for ExitingLauncher {
    fn launch(&mut self, election: &StartupElection) -> Result<()> {
        election.validate()?;
        self.launched = true;
        self.launches.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn observe_exit(&mut self) -> Result<Option<ExitStatus>> {
        self.observations.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .launched
            .then_some(ExitStatus::from_raw(self.status_raw)))
    }
}

/// Two startup callers at one configuration root must invoke only one launcher
/// and receive distinct retained authenticated streams from the same broker.
/// The election guard survives launch until readiness; caller cancellation then
/// completes foreground teardown rather than leaving a detached test owner.
#[tokio::test]
async fn outbound_startup_composition_elects_once_and_returns_retained_clients() {
    let home = std::env::temp_dir().join(format!("mez-start-{:032x}", rand::random::<u128>()));
    let env = CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    let launch = Arc::new(tokio::sync::Notify::new());
    let stop = Arc::new(tokio::sync::Notify::new());
    let launches = Arc::new(AtomicUsize::new(0));
    let broker_launch = launch.clone();
    let broker_stop = stop.clone();
    let broker = async {
        broker_launch.notified().await;
        super::super::run(&env, async move { broker_stop.notified().await }).await
    };
    let clients = async {
        let mut first_launcher = LaunchOnly::new({
            let launch_first = launch.clone();
            let count_first = launches.clone();
            move |guard: &StartupElection| -> Result<()> {
                guard.validate()?;
                count_first.fetch_add(1, Ordering::SeqCst);
                launch_first.notify_one();
                Ok(())
            }
        });
        let mut second_launcher = LaunchOnly::new({
            let launch_second = launch.clone();
            let count_second = launches.clone();
            move |guard: &StartupElection| -> Result<()> {
                guard.validate()?;
                count_second.fetch_add(1, Ordering::SeqCst);
                launch_second.notify_one();
                Ok(())
            }
        });
        let first =
            connect_with_launcher(paths.root(), Duration::from_secs(5), &mut first_launcher);
        let second =
            connect_with_launcher(paths.root(), Duration::from_secs(5), &mut second_launcher);
        let (first, second) = tokio::join!(first, second);
        let first = first.unwrap();
        let second = second.unwrap();
        assert_ne!(first.handle().unwrap(), second.handle().unwrap());
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        drop(first);
        drop(second);
        stop.notify_one();
    };
    let (served, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(broker, clients)
    })
    .await
    .unwrap();
    assert_eq!(served.unwrap(), 2);
    assert!(!paths.root().join("outbound.sock").exists());
    drop(StartupElection::acquire(paths.root()).unwrap().unwrap());
    std::fs::remove_dir_all(home).unwrap();
}

/// Failed launch and absent readiness must release the short-lived election
/// guard, with at most one launcher invocation and no ownership unlink/replay.
/// A live-but-unready fixture must keep the timeout classification rather than
/// being reported as an exited child. Retained same-description duplicates make
/// release independent of incidental fork/exec timing, rather than qualifying
/// the fix with passing reruns alone.
#[tokio::test]
async fn outbound_startup_composition_failure_and_timeout_release_election() {
    let root = std::env::temp_dir().join(format!("mez-start-fail-{:032x}", rand::random::<u128>()));
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    let count = AtomicUsize::new(0);
    let duplicates = std::cell::RefCell::new(Vec::new());
    {
        let mut failing = LaunchOnly::new(|guard: &StartupElection| -> Result<()> {
            duplicates
                .borrow_mut()
                .push(guard.fixture_duplicate_lock()?);
            count.fetch_add(1, Ordering::SeqCst);
            Err(MezError::invalid_state("fixture launch failure"))
        });
        let failed = connect_with_launcher(&root, Duration::from_secs(1), &mut failing)
            .await
            .err()
            .unwrap();
        assert_eq!(failed.message(), "fixture launch failure");
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    {
        let mut unready = LaunchOnly::new(|guard: &StartupElection| -> Result<()> {
            duplicates
                .borrow_mut()
                .push(guard.fixture_duplicate_lock()?);
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let timed_out = connect_with_launcher(&root, Duration::from_millis(200), &mut unready)
            .await
            .err()
            .unwrap();
        assert!(timed_out.message().contains("timeout"));
        assert!(
            !timed_out.message().contains("exited"),
            "a live-or-absent child must not be classified as an exit: {timed_out}"
        );
    }
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(duplicates.borrow().len(), 2);
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    drop(duplicates);
    assert!(!root.join("remote/client/endpoint.key").exists());
    std::fs::remove_dir_all(root).unwrap();
}

/// Unsafe discovery is a terminal failure, not permission to launch a broker
/// or replace the authored socket entry. Only missing/refused discovery retries.
#[tokio::test]
async fn outbound_startup_composition_unsafe_discovery_never_launches() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root =
        std::env::temp_dir().join(format!("mez-start-unsafe-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(root.join("authored"), b"preserve").unwrap();
    symlink("authored", root.join("outbound.sock")).unwrap();
    let count = AtomicUsize::new(0);
    let mut launcher = LaunchOnly::new(|_: &StartupElection| -> Result<()> {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert!(
        connect_with_launcher(&root, Duration::from_secs(1), &mut launcher)
            .await
            .is_err()
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserve");
    assert!(!root.join("outbound.startup.lock").exists());
    std::fs::remove_dir_all(root).unwrap();
}

/// A launched exact child that exits nonzero before publishing must be reported
/// promptly and distinctly, not disguised as an exhausted readiness deadline.
/// The launcher runs once, observation executes beside readiness, and the error
/// keeps the action-oriented startup-exit classification without deleting any
/// retained election, discovery, lock or key evidence.
#[tokio::test]
async fn outbound_startup_nonzero_child_exit_is_reported_before_deadline() {
    let root = std::env::temp_dir().join(format!("mez-start-exit-{:032x}", rand::random::<u128>()));
    let launches = Arc::new(AtomicUsize::new(0));
    let observations = Arc::new(AtomicUsize::new(0));
    let mut launcher = ExitingLauncher {
        launches: launches.clone(),
        observations: observations.clone(),
        launched: false,
        status_raw: 1 << 8,
    };
    let started = tokio::time::Instant::now();
    let error = connect_with_launcher(&root, Duration::from_secs(10), &mut launcher)
        .await
        .err()
        .expect("an exited child cannot report readiness");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "exact-child exit must be reported promptly, not after the deadline"
    );
    assert!(error.message().contains("exited before local readiness"));
    assert!(error.message().contains("exit status: 1"));
    assert!(!error.message().contains("before timeout"));
    assert_eq!(launches.load(Ordering::SeqCst), 1);
    assert!(observations.load(Ordering::SeqCst) >= 1);
    assert!(root.join("outbound.startup.lock").exists());
    assert!(!root.join("outbound.sock").exists());
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

/// A successful exit that never published readiness is its own actionable
/// classification: the child did not fail, yet no broker became reachable. A
/// retained identity owner may still explain why nothing was published, so the
/// same deadline and ownership boundaries remain in force.
#[tokio::test]
async fn outbound_startup_clean_child_exit_without_publication_is_distinct() {
    let root =
        std::env::temp_dir().join(format!("mez-start-clean-{:032x}", rand::random::<u128>()));
    let launches = Arc::new(AtomicUsize::new(0));
    let observations = Arc::new(AtomicUsize::new(0));
    let mut launcher = ExitingLauncher {
        launches: launches.clone(),
        observations: observations.clone(),
        launched: false,
        status_raw: 0,
    };
    let started = tokio::time::Instant::now();
    let error = connect_with_launcher(&root, Duration::from_secs(10), &mut launcher)
        .await
        .err()
        .expect("a clean no-publication exit cannot report readiness");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "clean no-publication exit must be reported promptly"
    );
    assert!(
        error
            .message()
            .contains("exited successfully before local readiness")
    );
    assert!(!error.message().contains("before timeout"));
    assert_eq!(launches.load(Ordering::SeqCst), 1);
    assert!(observations.load(Ordering::SeqCst) >= 1);
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

/// Deadline-edge control: an exact child that already exited must still be
/// classified as an exit when the remaining budget is below the probe threshold.
/// Observation precedes deadline classification, so the central defect cannot
/// reappear at the end of a short attempt.
#[tokio::test]
async fn outbound_startup_exited_child_is_reported_at_deadline_edge() {
    let root = std::env::temp_dir().join(format!("mez-start-edge-{:032x}", rand::random::<u128>()));
    let launches = Arc::new(AtomicUsize::new(0));
    let observations = Arc::new(AtomicUsize::new(0));
    let mut launcher = ExitingLauncher {
        launches: launches.clone(),
        observations: observations.clone(),
        launched: true,
        status_raw: 1 << 8,
    };
    let error = connect_with_launcher(&root, Duration::from_millis(100), &mut launcher)
        .await
        .err()
        .expect("an exited child cannot report readiness");
    assert!(error.message().contains("exited before local readiness"));
    assert!(
        !error.message().contains("before timeout"),
        "deadline edge must not mask an observed exit: {error}"
    );
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    assert!(observations.load(Ordering::SeqCst) >= 1);
    assert!(!root.join("outbound.startup.lock").exists());
}

/// A startup election held by another launcher is a distinct contention outcome,
/// not a silent live-but-not-ready timeout. No launcher runs, and the held lock is
/// preserved rather than stolen or unlinked.
#[tokio::test]
async fn outbound_startup_election_contention_is_distinct_from_readiness_timeout() {
    let root =
        std::env::temp_dir().join(format!("mez-start-contend-{:032x}", rand::random::<u128>()));
    let held = StartupElection::acquire(&root).unwrap().unwrap();
    let launches = AtomicUsize::new(0);
    {
        let mut launcher = LaunchOnly::new(|_: &StartupElection| -> Result<()> {
            launches.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let error = connect_with_launcher(&root, Duration::from_millis(250), &mut launcher)
            .await
            .err()
            .expect("a held election cannot start a broker");
        assert!(
            error.message().contains("election remained held"),
            "held election needs its own classification: {error}"
        );
        assert!(!error.message().contains("did not become ready"));
        assert!(!error.message().contains("exited"));
    }
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    assert!(root.join("outbound.startup.lock").exists());
    drop(held);
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

/// Dropping startup while its child is still unready must not kill, relaunch or
/// replace ownership. Cancellation releases only the short-lived election guard,
/// leaving discovery and identity state untouched for the owner's own disposal.
#[tokio::test]
async fn outbound_startup_cancellation_preserves_ownership_and_releases_election() {
    let root =
        std::env::temp_dir().join(format!("mez-start-cancel-{:032x}", rand::random::<u128>()));
    let launches = Arc::new(AtomicUsize::new(0));
    {
        let mut launcher = LaunchOnly::new({
            let launches = launches.clone();
            move |_: &StartupElection| -> Result<()> {
                launches.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        let cancelled = tokio::time::timeout(
            Duration::from_millis(500),
            connect_with_launcher(&root, Duration::from_secs(5), &mut launcher),
        )
        .await;
        assert!(cancelled.is_err(), "fixture startup must not become ready");
    }
    assert_eq!(launches.load(Ordering::SeqCst), 1);
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    assert!(!root.join("outbound.sock").exists());
    std::fs::remove_dir_all(root).unwrap();
}
