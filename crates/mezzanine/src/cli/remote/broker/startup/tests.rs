//! Startup orchestration over real election and authenticated local readiness.
//!
//! Injected launch notification drives the actual foreground broker composition
//! without spawning an unowned background process or enabling ordinary CLI attach.

use super::*;
use crate::cli::CliEnv;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
        let launch_first = launch.clone();
        let launch_second = launch.clone();
        let count_first = launches.clone();
        let count_second = launches.clone();
        let first = connect_with_launcher(paths.root(), Duration::from_secs(5), move |guard| {
            guard.validate()?;
            count_first.fetch_add(1, Ordering::SeqCst);
            launch_first.notify_one();
            Ok(())
        });
        let second = connect_with_launcher(paths.root(), Duration::from_secs(5), move |guard| {
            guard.validate()?;
            count_second.fetch_add(1, Ordering::SeqCst);
            launch_second.notify_one();
            Ok(())
        });
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
#[tokio::test]
async fn outbound_startup_composition_failure_and_timeout_release_election() {
    let root = std::env::temp_dir().join(format!("mez-start-fail-{:032x}", rand::random::<u128>()));
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    let count = AtomicUsize::new(0);
    let failed = connect_with_launcher(&root, Duration::from_secs(1), |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Err(MezError::invalid_state("fixture launch failure"))
    })
    .await
    .err()
    .unwrap();
    assert_eq!(failed.message(), "fixture launch failure");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    let timed_out = connect_with_launcher(&root, Duration::from_millis(200), |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await
    .err()
    .unwrap();
    assert!(timed_out.message().contains("timeout"));
    assert_eq!(count.load(Ordering::SeqCst), 2);
    drop(StartupElection::acquire(&root).unwrap().unwrap());
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
    assert!(
        connect_with_launcher(&root, Duration::from_secs(1), |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserve");
    assert!(!root.join("outbound.startup.lock").exists());
    std::fs::remove_dir_all(root).unwrap();
}
