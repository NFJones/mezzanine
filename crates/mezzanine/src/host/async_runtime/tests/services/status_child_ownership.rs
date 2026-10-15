//! Task-ID settlement and detached blocking-capacity ownership regressions.

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Aborting outer async wrappers must immediately signal both dispatched and
/// capacity-waiting plans, retain their exact failure completions once, and
/// keep capacity charged until the detached blocking child really exits.
#[tokio::test]
async fn aborted_status_wrappers_cancel_exact_work_and_retain_blocking_capacity() {
    let (_lifecycle_tx, lifecycle) = watch::channel(RuntimeLifecycleState::Running);
    let permits = Arc::new(tokio::sync::Semaphore::new(1));
    let plans = (0..2)
        .map(|index| {
            crate::runtime::RuntimePaneStatusProviderRefreshPlan::for_tests(
                &format!("abort-{index}"),
                "unused",
                &std::env::temp_dir(),
                1_000,
            )
        })
        .collect::<Vec<_>>();
    let cancellations = plans
        .iter()
        .map(|plan| plan.cancellation.clone())
        .collect::<Vec<_>>();
    let expected = plans
        .iter()
        .map(|plan| RuntimeEvent::PaneStatusProvider(plan.failed_event()))
        .collect::<Vec<_>>();
    let mut queue = plans.into_iter().map(Box::new).collect::<VecDeque<_>>();
    let mut tasks = JoinSet::new();
    let mut failures = StatusTaskFailures::new();
    let started = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    let execute = {
        let started = started.clone();
        move |_plan| {
            started.store(true, Ordering::SeqCst);
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            None
        }
    };
    spawn_bounded_pane_status_provider_tasks(
        &mut tasks,
        &mut failures,
        &mut queue,
        &lifecycle,
        &permits,
        &execute,
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while !started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(permits.available_permits(), 0);
    tasks.abort_all();
    let mut settled = Vec::new();
    while let Some(joined) = tasks.join_next_with_id().await {
        let error = joined.unwrap_err();
        assert!(error.is_cancelled());
        settled.push(
            settle_status_task(Err(error), &mut failures)
                .unwrap()
                .unwrap(),
        );
    }
    assert!(failures.is_empty());
    assert_eq!(settled.len(), 2);
    assert!(expected.iter().all(|event| settled.contains(event)));
    assert!(
        cancellations
            .iter()
            .all(|cancellation| cancellation.is_cancelled())
    );
    assert_eq!(
        permits.available_permits(),
        0,
        "wrapper abort must not release blocking capacity"
    );
    release_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while permits.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// Lost retained identity indicates corrupted task ownership and must not be
/// silently converted to optional completion, including a successful child.
#[tokio::test]
async fn missing_status_child_identity_remains_fatal() {
    let mut tasks = JoinSet::new();
    tasks.spawn(async { None });
    let joined = tasks.join_next_with_id().await.unwrap();
    let error = settle_status_task(joined, &mut StatusTaskFailures::new()).unwrap_err();
    assert_eq!(
        error.message(),
        "status child completion has no retained refresh identity"
    );
}
