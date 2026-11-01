//! Original queued producer identity and bounded enrollment lifetime.
//! These actual actor/descriptor cases preserve allocation before first claim
//! and prove cached completed evidence does not own its enrollment back.

use super::*;

/// Root registration must retain the same allocation in actor source custody
/// before a monitor's first claim. Recovery cannot replace an already owned
/// unissued producer merely because no dispatch/worker exists. The held monitor
/// never claims or executes; all actual actor/pane owners are reaped before the
/// captured preclaim ownership assertion, including the pre-fix failure path.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_original_unissued_producer_is_registered_before_first_claim() {
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let producer = NativeFilesystemProducer::new(offers[0].attempt().clone());
    let task = worker_ownership::spawn_owned_filesystem_agent_worker(
        &mut workers,
        &mut owners,
        producer.clone(),
        std::future::pending::<Result<AsyncAgentProviderWorkerResult>>(),
    )
    .unwrap();
    assert!(!producer.is_claimed().unwrap());
    let recovered = handle.recover_native_filesystem_producers().await.unwrap();
    let original_registered = recovered.iter().any(|(offer, original)| {
        offer == &offers[0]
            && original
                .as_ref()
                .is_some_and(|actual| actual.same(&producer))
    });
    let still_unclaimed = !producer.is_claimed().unwrap();
    task.abort();
    let joined = workers.join_next_with_id().await.unwrap();
    record_owned_agent_provider_worker(joined, &handle, &mut report(&handle), &mut owners, &drains)
        .await
        .unwrap();
    let (stop, persistence) = writer(handle.clone());
    retire_agent_provider_workers(
        &mut workers,
        &handle,
        &drains,
        &mut owners,
        &mut ProviderCleanupFailures::default(),
    )
    .await
    .unwrap();
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    assert!(owners.is_empty());
    assert!(
        producer
            .refusal_acknowledgement()
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    assert!(producer.joined_outcome().is_none());
    assert!(!root.0.join("native-files/f0").exists());
    drop(finish(&handle, actor).await);
    assert!(still_unclaimed);
    assert!(
        original_registered,
        "actor recovery lost the root's actual preclaim producer allocation"
    );
}

/// A replacement root recovers the ACTUAL preclaim producer even after the
/// first monitor is lost. Both roots retain the same unissued allocation until
/// canonical refusal and storage accept it; neither recreates dispatch or I/O.
/// Every real monitor/recovery join and pane process is reaped before comparison.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_recovers_original_preclaim_allocation_after_monitor_loss() {
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let mut old_workers = JoinSet::new();
    let mut old_owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let producer = NativeFilesystemProducer::new(offers[0].attempt().clone());
    let old = worker_ownership::spawn_owned_filesystem_agent_worker(
        &mut old_workers,
        &mut old_owners,
        producer.clone(),
        std::future::pending::<Result<AsyncAgentProviderWorkerResult>>(),
    )
    .unwrap();
    producer.close().unwrap();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    filesystem_ownership::recover_filesystem_workers(&handle, &mut workers, &mut owners, &drains)
        .await
        .unwrap();
    assert_eq!(owners.len(), 1);
    let recovered =
        worker_ownership::filesystem_owner_for_tests(&owners, *owners.keys().next().unwrap())
            .unwrap();
    let same = recovered.same(&producer);
    let (stop, persistence) = writer(handle.clone());
    retire_agent_provider_workers(
        &mut workers,
        &handle,
        &drains,
        &mut owners,
        &mut ProviderCleanupFailures::default(),
    )
    .await
    .unwrap();
    old.abort();
    let joined = old_workers.join_next_with_id().await.unwrap();
    record_owned_agent_provider_worker(
        joined,
        &handle,
        &mut report(&handle),
        &mut old_owners,
        &drains,
    )
    .await
    .unwrap();
    retire_agent_provider_workers(
        &mut old_workers,
        &handle,
        &drains,
        &mut old_owners,
        &mut ProviderCleanupFailures::default(),
    )
    .await
    .unwrap();
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    assert!(owners.is_empty());
    assert!(old_owners.is_empty());
    assert!(
        producer
            .refusal_acknowledgement()
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    assert!(!producer.is_claimed().unwrap());
    assert!(producer.joined_outcome().is_none());
    assert!(!root.0.join("native-files/f0").exists());
    drop(finish(&handle, actor).await);
    assert!(
        same,
        "recovery replaced the original root allocation before any claim"
    );
}

/// Source enrollment owns a producer, but producer-owned tokens and archive
/// evidence must not own enrollment back. Actual native execution, checked
/// archive work and exact handoff run before all offered-source owners drop;
/// retaining the real producer's historical acknowledgement cannot keep its
/// former enrollment cell alive. A strong-token archive request exercises the
/// cache normalization boundary rather than relying only on weak host inputs.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_completed_producer_and_archive_do_not_retain_enrollment_cycle() {
    use crate::host::async_runtime::native_filesystem::{
        claim_and_start_native_filesystem_producer, prepare_joined_native_filesystem_archive,
    };
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let producer = NativeFilesystemProducer::new(offers[0].attempt().clone());
    let executing = producer.clone();
    let context = handle.clone();
    let (sent, ready) = tokio::sync::oneshot::channel();
    let task = worker_ownership::spawn_owned_filesystem_agent_worker(
        &mut workers,
        &mut owners,
        producer.clone(),
        async move {
            assert!(claim_and_start_native_filesystem_producer(context, &executing).await?);
            executing.observe().await?;
            sent.send(()).unwrap();
            std::future::pending::<Result<AsyncAgentProviderWorkerResult>>().await
        },
    )
    .unwrap();
    ready.await.unwrap();
    assert!(producer.attempt().enrollment_alive_for_tests());
    let (stop, persistence) = writer(handle.clone());
    tokio::time::timeout(Duration::from_secs(10), async {
        while prepare_joined_native_filesystem_archive(&handle, &producer)
            .await
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let source = handle
        .prepare_native_filesystem_archive(offers[0].attempt().clone())
        .await
        .unwrap()
        .unwrap();
    if let Some(receipt) = producer.register_archive_worker(&source).unwrap() {
        receipt.recheck().await.unwrap();
    }
    drop(source);
    task.abort();
    let joined = workers.join_next_with_id().await.unwrap();
    record_owned_agent_provider_worker(joined, &handle, &mut report(&handle), &mut owners, &drains)
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        retire_agent_provider_workers(
            &mut workers,
            &handle,
            &drains,
            &mut owners,
            &mut ProviderCleanupFailures::default(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(owners.is_empty());
    assert!(
        producer
            .transfer_acknowledgement()
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    drop(finish(&handle, actor).await);
    drop(offers);
    assert_eq!(
        std::fs::read(root.0.join("native-files/f0")).unwrap(),
        b"root-0\n"
    );
    assert!(
        !producer.attempt().enrollment_alive_for_tests(),
        "producer/archive cache owns its own source enrollment"
    );
}
