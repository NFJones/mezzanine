//! Positive canonical no-dispatch settlement under original source ownership.
//!
//! Real actor and ordered-writer paths qualify cancellation before any native
//! claim. No empty task result, cancellation flag or queue removal can replace
//! accepted original canonical/context/storage evidence or a qualified handoff.

use super::*;

/// Closing the actual registered root before its first claim must settle one
/// cancelled original tool result and its durable source without inventing a
/// native join. Cleanup cannot leave an unissued error as the final behavior.
/// All actual tasks/pane processes retire before the pre-fix assertion fails.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_preclaim_stop_settles_canonical_refusal_without_worker() {
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let turn = service
        .agent_turn_executions()
        .keys()
        .next()
        .unwrap()
        .clone();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let store = service.agent_transcript_store().unwrap().clone();
    let (handle, actor) = actor(service);
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let task = filesystem_ownership::spawn_filesystem_action_worker(
        &mut workers,
        &mut owners,
        handle.clone(),
        offers[0].clone(),
    )
    .unwrap();
    let producer = worker_ownership::filesystem_owner_for_tests(&owners, task.id()).unwrap();
    assert!(!producer.is_claimed().unwrap());
    producer.close().unwrap();
    // Real original storage remains pending without a writer. Repeated polling
    // must preserve canonical facts and cannot remove root/actor custody early.
    for _ in 0..2 {
        assert!(crate::host::async_runtime::native_filesystem::transfer_refused_native_filesystem_producer(&handle, &producer).await.unwrap().is_none());
        assert!(producer.refusal_acknowledgement().unwrap().is_none());
        assert_eq!(owners.len(), 1);
    }
    let (stop, persistence) = writer(handle.clone());
    let joined = workers.join_next_with_id().await.unwrap();
    record_owned_agent_provider_worker(joined, &handle, &mut report(&handle), &mut owners, &drains)
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        retire_agent_provider_workers(
            &mut workers,
            &handle,
            &drains,
            &mut owners,
            &mut ProviderCleanupFailures::default(),
        ),
    )
    .await;
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    let service = finish(&handle, actor).await;
    let rows = store.inspect(&conversation).unwrap_or_default();
    assert!(
        result.as_ref().is_ok_and(|result| result.is_ok()),
        "preclaim cancellation did not complete canonical refusal handoff: {result:?}"
    );
    assert!(owners.is_empty());
    assert!(producer.joined_outcome().is_none());
    assert!(producer.transfer_acknowledgement().unwrap().is_none());
    assert!(
        service
            .retained_native_filesystem_producer(offers[0].attempt())
            .is_none()
    );
    assert!(!root.0.join("native-files/f0").exists());
    assert_eq!(
        rows.iter()
            .filter(
                |row| row.role == mez_agent::transcript::TranscriptRole::Tool
                    && row.content.contains("not_dispatched")
                    && row.content.contains("cancelled")
            )
            .count(),
        1
    );
    drop(service);
    drop(offers);
    assert!(
        !producer.attempt().enrollment_alive_for_tests(),
        "refusal proof/archive acknowledgement retains its enrollment cell"
    );
}

/// A required executable guard is rejected by the actual actor without launching
/// it. The ordinary provider monitor contains that action refusal and completes
/// canonical/storage settlement; it must not turn a policy refusal into a global
/// provider failure or synthesize a joined native worker.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_policy_refusal_is_contained_and_durable_without_worker() {
    let (root, service, offers) = Box::pin(fixture_with_config(1, |service| {
        service.use_hook_effect_adapter();
        let mut layers = service.config_layers().to_vec();
        layers[0].text.push_str("\n[hooks.root-refusal-guard]\nevent=\"pre_shell_command\"\nprogram=\"/missing/refusal-guard\"\nrequired=true\non_failure=\"warn\"\n");
        service.replace_config_layers(layers).unwrap();
        service.permission_policy_mut().set_approval_bypass(true);
    })).await;
    let store = service.agent_transcript_store().unwrap().clone();
    let conversation = service
        .agent_turn_ledger()
        .turns()
        .last()
        .unwrap()
        .conversation_id
        .clone();
    let (handle, actor) = actor(service);
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let task = filesystem_ownership::spawn_filesystem_action_worker(
        &mut workers,
        &mut owners,
        handle.clone(),
        offers[0].clone(),
    )
    .unwrap();
    let producer = worker_ownership::filesystem_owner_for_tests(&owners, task.id()).unwrap();
    let (stop, persistence) = writer(handle.clone());
    let joined = workers.join_next_with_id().await.unwrap();
    let recorded = record_owned_agent_provider_worker(
        joined,
        &handle,
        &mut report(&handle),
        &mut owners,
        &drains,
    )
    .await;
    let retired = retire_agent_provider_workers(
        &mut workers,
        &handle,
        &drains,
        &mut owners,
        &mut ProviderCleanupFailures::default(),
    )
    .await;
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    let mut service = finish(&handle, actor).await;
    assert!(
        recorded.is_ok(),
        "actor policy refusal escaped provider monitor: {recorded:?}"
    );
    assert!(
        retired.is_ok(),
        "policy refusal settlement failed: {retired:?}"
    );
    assert!(owners.is_empty());
    let proof = producer.refusal().unwrap().unwrap();
    assert!(!proof.cancelled());
    assert!(
        proof.cause().message().contains("root-refusal-guard"),
        "unexpected original refusal: {:?}",
        proof.cause()
    );
    assert!(producer.joined_outcome().is_none());
    assert!(
        producer
            .refusal_acknowledgement()
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    assert!(
        service
            .drain_program_hook_transition()
            .side_effects
            .is_empty()
    );
    let rows = store.inspect(&conversation).unwrap();
    assert_eq!(
        rows.iter()
            .filter(
                |row| row.role == mez_agent::transcript::TranscriptRole::Tool
                    && row.content.contains("not_dispatched")
                    && row.content.contains("denied")
            )
            .count(),
        1
    );
    assert!(!root.0.join("native-files/f0").exists());
}

/// A stopped actor is a shared transport failure, even when no native work was
/// started. The monitor must report it and retain the original root; absence of
/// a dispatch cannot be reinterpreted as a successfully settled policy refusal.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_actor_loss_does_not_masquerade_as_policy_refusal() {
    let (_root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let service = finish(&handle, actor).await;
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let task = filesystem_ownership::spawn_filesystem_action_worker(
        &mut workers,
        &mut owners,
        handle.clone(),
        offers[0].clone(),
    )
    .unwrap();
    let producer = worker_ownership::filesystem_owner_for_tests(&owners, task.id()).unwrap();
    let joined = workers.join_next_with_id().await.unwrap();
    assert!(
        record_owned_agent_provider_worker(
            joined,
            &handle,
            &mut report(&handle),
            &mut owners,
            &drains
        )
        .await
        .is_err()
    );
    assert!(
        retire_agent_provider_workers(
            &mut workers,
            &handle,
            &drains,
            &mut owners,
            &mut ProviderCleanupFailures::default()
        )
        .await
        .is_err()
    );
    assert_eq!(owners.len(), 1);
    assert!(producer.refusal().unwrap().is_none());
    assert!(producer.refusal_acknowledgement().unwrap().is_none());
    assert!(producer.joined_outcome().is_none());
    assert!(
        service
            .retained_native_filesystem_producer(offers[0].attempt())
            .unwrap()
            .same(&producer)
    );
}

/// Failure of the actual same-actor writer prevents refusal handoff while
/// preserving the original unissued producer. A replacement writer and root
/// recover that source, complete its required archive, and never start mutation.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_unissued_refusal_retains_source_after_writer_failure() {
    use crate::host::async_runtime::persistence_retirement::PersistenceServiceRetirement;
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let writer_owner = PersistenceServiceRetirement::new(
        handle.clone(),
        crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
            max_polls: u64::MAX,
            drain_limit: 64,
            idle_interval: Duration::from_millis(1),
        },
    );
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    owners
        .persistence_dependencies()
        .lock()
        .unwrap()
        .push(writer_owner.dependency().unwrap());
    writer_owner.start().unwrap();
    writer_owner.abort_driver_for_tests();
    assert!(writer_owner.retire().await.is_err());
    let producer = offers[0].attempt().root_producer().unwrap();
    producer.close().unwrap();
    let drains: ProviderObservationDrains = Default::default();
    filesystem_ownership::recover_filesystem_workers(&handle, &mut workers, &mut owners, &drains)
        .await
        .unwrap();
    let error = tokio::time::timeout(
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
    .unwrap_err();
    assert!(error.message().contains("persistence"));
    assert_eq!(owners.len(), 1);
    assert!(producer.refusal().unwrap().unwrap().cancelled());
    assert!(producer.refusal_acknowledgement().unwrap().is_none());
    assert!(producer.joined_outcome().is_none());
    let mut recovered_workers = JoinSet::new();
    let mut recovered_owners = AgentProviderWorkerOwners::default();
    filesystem_ownership::recover_filesystem_workers(
        &handle,
        &mut recovered_workers,
        &mut recovered_owners,
        &drains,
    )
    .await
    .unwrap();
    let actual = worker_ownership::filesystem_owner_for_tests(
        &recovered_owners,
        *recovered_owners.keys().next().unwrap(),
    )
    .unwrap();
    assert!(actual.same(&producer));
    let (stop, persistence) = writer(handle.clone());
    tokio::time::timeout(
        Duration::from_secs(10),
        retire_agent_provider_workers(
            &mut recovered_workers,
            &handle,
            &drains,
            &mut recovered_owners,
            &mut ProviderCleanupFailures::default(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    // The failed old recovery remains reportable, but its root can now observe
    // the genuine acknowledgement obtained by the replacement source owner.
    assert!(
        retire_agent_provider_workers(
            &mut workers,
            &handle,
            &drains,
            &mut owners,
            &mut ProviderCleanupFailures::default()
        )
        .await
        .is_err()
    );
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    drop(finish(&handle, actor).await);
    assert!(owners.is_empty());
    assert!(recovered_owners.is_empty());
    assert!(
        producer
            .refusal_acknowledgement()
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    assert!(producer.joined_outcome().is_none());
    assert!(!root.0.join("native-files/f0").exists());
}

/// Both queued siblings can be cancelled before any claim. Their independently
/// retained refusal recoveries must advance ordered publication and one original
/// archive without deadlocking on a predecessor or manufacturing a native join.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_refused_siblings_settle_in_original_order() {
    let (root, service, offers) = Box::pin(fixture(2)).await;
    let conversation = service
        .agent_turn_ledger()
        .turns()
        .last()
        .unwrap()
        .conversation_id
        .clone();
    let store = service.agent_transcript_store().unwrap().clone();
    let (handle, actor) = actor(service);
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let mut producers = Vec::new();
    for offer in &offers {
        let task = filesystem_ownership::spawn_filesystem_action_worker(
            &mut workers,
            &mut owners,
            handle.clone(),
            offer.clone(),
        )
        .unwrap();
        let producer = worker_ownership::filesystem_owner_for_tests(&owners, task.id()).unwrap();
        producer.close().unwrap();
        producers.push(producer);
    }
    let (stop, persistence) = writer(handle.clone());
    let settled = tokio::time::timeout(
        Duration::from_secs(10),
        retire_agent_provider_workers(
            &mut workers,
            &handle,
            &drains,
            &mut owners,
            &mut ProviderCleanupFailures::default(),
        ),
    )
    .await;
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    drop(finish(&handle, actor).await);
    assert!(settled.unwrap().is_ok());
    assert!(owners.is_empty());
    for producer in &producers {
        assert!(
            producer
                .refusal_acknowledgement()
                .unwrap()
                .unwrap()
                .acknowledges(producer)
        );
        assert!(producer.joined_outcome().is_none());
    }
    let tools = store
        .inspect(&conversation)
        .unwrap()
        .into_iter()
        .filter(|row| row.role == mez_agent::transcript::TranscriptRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 2);
    assert!(tools[0].content.contains("filesystem-0"));
    assert!(tools[1].content.contains("filesystem-1"));
    assert!(!root.0.join("native-files/f0").exists());
    assert!(!root.0.join("native-files/f1").exists());
}
