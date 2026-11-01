//! Actual provider-lane filesystem owners, independent of selector activation.
//!
//! An offline request fixture supplies the real canonical turn/context. No shell
//! lowering is invoked: opaque filesystem offers cross the actor provider lane,
//! actual descriptors mutate private files and the real ordered writer settles
//! original source. Root maps remain authoritative across lost monitors/recovery.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorExit, AsyncRuntimeSessionActor};
use crate::runtime::native_filesystem::NativeFilesystemProducer;
use crate::runtime::{RuntimeNativeFilesystemOffer, RuntimeSessionService};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Owns a private directory after the actual pane/native workers are reaped.
struct Root(std::path::PathBuf);
impl Drop for Root {
    /// Removes only this fixture's private files on normal completion or unwind.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Installs exact accepted assistant/action chronology without shell lowering.
/// Offers are actual actor-owned tokens, never reconstructed labels or rights.
async fn fixture(
    count: usize,
) -> (
    Root,
    RuntimeSessionService,
    Vec<RuntimeNativeFilesystemOffer>,
) {
    let (mut service, agent, turn_id) =
        Box::pin(prepared_fixture("qualify filesystem root ownership")).await;
    let base = service
        .agent_transcript_store()
        .unwrap()
        .root()
        .parent()
        .unwrap()
        .to_path_buf();
    let root = Root(base);
    let directory = root.0.join("native-files");
    std::fs::create_dir(&directory).unwrap();
    let mut layers = service.config_layers().to_vec();
    let scope = serde_json::to_string(directory.to_str().unwrap()).unwrap();
    layers[0].text = layers[0].text.replace(
        "[permissions]\n",
        &format!("[permissions]\nread_scopes=[{scope}]\nwrite_scopes=[{scope}]\n"),
    );
    service.replace_config_layers(layers).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    let dispatch = service
        .claim_configured_agent_provider_task(&agent, &turn_id)
        .unwrap()
        .unwrap();
    let provider = IncompleteProvider {
        requests: Mutex::new(Vec::new()),
        failures: 0,
        stream: false,
    };
    let (sender, _receiver) = tokio::sync::mpsc::channel(8);
    let observer = ProviderWireRequestObserver::new(
        &dispatch.turn.conversation_id,
        &dispatch.turn.pane_id,
        sender,
    );
    let mut execution = Box::pin(run_incomplete_fixture(
        &provider, &observer, &dispatch, 0, None,
    ))
    .await
    .unwrap();
    let actions: Vec<_> = (0..count)
        .map(|index| mez_agent::AgentAction {
            id: format!("filesystem-{index}"),
            payload: mez_agent::AgentActionPayload::ApplyPatch {
                patch: format!(
                    "*** Begin Patch\n*** Add File: {}\n+root-{index}\n*** End Patch",
                    directory.join(format!("f{index}")).display()
                ),
                strip: None,
            },
        })
        .collect();
    execution.response.action_batch = Some(mez_agent::MaapBatch {
        rationale: "root filesystem ownership".into(),
        actions: actions.clone(),
    });
    execution.action_results = actions
        .iter()
        .map(|action| mez_agent::ActionResult::running(&dispatch.turn, action, Vec::new(), None))
        .collect();
    execution.final_turn = false;
    execution.terminal_state = mez_agent::AgentTurnState::Running;
    service.remove_pending_agent_provider_task(&turn_id);
    service
        .append_agent_execution_chronology(&dispatch.turn, &execution)
        .unwrap();
    service
        .agent_turn_executions_mut()
        .insert(turn_id, execution.clone());
    service.use_transcript_effect_adapter();
    let mut offers = Vec::new();
    for action in &actions {
        offers.push(RuntimeNativeFilesystemOffer::new(
            service
                .queue_native_filesystem_action(&dispatch.turn, action, &execution)
                .unwrap(),
        ));
    }
    (root, service, offers)
}

/// Uses the real actor with an actual join owned through normal test teardown.
fn actor(
    service: RuntimeSessionService,
) -> (
    AsyncRuntimeSessionHandle,
    tokio::task::JoinHandle<AsyncRuntimeActorExit>,
) {
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    (handle, tokio::spawn(Box::pin(actor.run())))
}

/// Reaps the real pane after actor exit; no test treats task abortion as cleanup.
async fn finish(
    handle: &AsyncRuntimeSessionHandle,
    actor: tokio::task::JoinHandle<AsyncRuntimeActorExit>,
) -> RuntimeSessionService {
    handle.shutdown().await.unwrap();
    let mut exit = actor.await.unwrap();
    exit.service.terminate_all_pane_processes().unwrap();
    exit.service
}

/// Starts the actual ordered journal/append/settle worker, not a success bit.
fn writer(handle: AsyncRuntimeSessionHandle) -> (Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let task = tokio::spawn(Box::pin(async move {
        let report = crate::host::async_runtime::run_async_persistence_side_effect_service(
            &handle,
            crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
                max_polls: 10_000,
                drain_limit: 64,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| stopping.load(Ordering::Acquire),
        )
        .await
        .unwrap();
        assert_eq!(report.failed, 0);
    }));
    (stop, task)
}

/// Builds report counters without inventing any native execution observation.
fn report(handle: &AsyncRuntimeSessionHandle) -> AsyncAgentProviderPollReport {
    AsyncAgentProviderPollReport {
        polls: 0,
        executions: 0,
        idle_polls: 0,
        terminal_state: *handle.lifecycle_state_watcher().borrow(),
    }
}

/// Root registration exists before the first claim poll, then the real actor
/// lane, descriptor worker and writer produce an exact ack before root removal.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_registers_before_claim_and_removes_only_exact_ack() {
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    Box::pin(filesystem_ownership::recover_filesystem_workers(
        &handle,
        &mut workers,
        &mut owners,
        &drains,
    ))
    .await
    .unwrap();
    assert_eq!(owners.len(), 1);
    let id = *owners.keys().next().unwrap();
    let producer = worker_ownership::filesystem_owner_for_tests(&owners, id).unwrap();
    assert!(
        !producer.is_claimed().unwrap(),
        "claim polled before synchronous registration completed"
    );
    let (stop, persistence) = writer(handle.clone());
    let joined = tokio::time::timeout(Duration::from_secs(10), workers.join_next_with_id())
        .await
        .unwrap()
        .unwrap();
    record_owned_agent_provider_worker(joined, &handle, &mut report(&handle), &mut owners, &drains)
        .await
        .unwrap();
    assert!(owners.is_empty());
    assert!(
        producer
            .transfer_acknowledgement()
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    assert_eq!(
        std::fs::read(root.0.join("native-files/f0")).unwrap(),
        b"root-0\n"
    );
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    let service = finish(&handle, actor).await;
    assert!(
        service
            .retained_native_filesystem_producer(offers[0].attempt())
            .is_none()
    );
}

/// A cancelled monitor and cancelled cleanup observer leave actual recovery
/// handles and original producer in the map. Real writer completion permits
/// resumed retirement without dispatching the mutation a second time.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_retains_recovery_across_monitor_and_cleanup_cancellation() {
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let _ = handle
        .drain_agent_provider_dispatch_side_effects(64)
        .await
        .unwrap();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let task = filesystem_ownership::spawn_filesystem_action_worker(
        &mut workers,
        &mut owners,
        handle.clone(),
        offers[0].clone(),
    );
    let producer = worker_ownership::filesystem_owner_for_tests(&owners, task.id()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while producer.joined_outcome().is_none() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let joined = workers.join_next_with_id().await.unwrap();
    record_owned_agent_provider_worker(joined, &handle, &mut report(&handle), &mut owners, &drains)
        .await
        .unwrap();
    assert_eq!(owners.len(), 1);
    let mut failures = ProviderCleanupFailures::default();
    let mut cleanup = Box::pin(retire_agent_provider_workers(
        &mut workers,
        &handle,
        &drains,
        &mut owners,
        &mut failures,
    ));
    assert!(futures_util::poll!(cleanup.as_mut()).is_pending());
    drop(cleanup);
    assert_eq!(owners.len(), 1);
    assert!(producer.transfer_acknowledgement().unwrap().is_none());
    let (stop, persistence) = writer(handle.clone());
    tokio::time::timeout(
        Duration::from_secs(10),
        retire_agent_provider_workers(&mut workers, &handle, &drains, &mut owners, &mut failures),
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
    assert_eq!(
        std::fs::read(root.0.join("native-files/f0")).unwrap(),
        b"root-0\n"
    );
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    drop(finish(&handle, actor).await);
}

/// Monitor panic preserves its bounded fatal cause and original owner. Actual
/// native success is adopted once during recovery, never overwritten as cancel.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_monitor_panic_keeps_actual_producer_until_transfer() {
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let _ = handle
        .drain_agent_provider_dispatch_side_effects(64)
        .await
        .unwrap();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let producer = NativeFilesystemProducer::new(offers[0].attempt().clone());
    let executing = producer.clone();
    let context = handle.clone();
    let task = worker_ownership::spawn_owned_filesystem_agent_worker(
        &mut workers,
        &mut owners,
        producer.clone(),
        async move {
            assert!(crate::host::async_runtime::native_filesystem::claim_and_start_native_filesystem_producer(context, &executing).await?);
            executing.observe().await?;
            panic!("private filesystem monitor payload");
        },
    );
    let joined = workers.join_next_with_id().await.unwrap();
    let failure = record_owned_agent_provider_worker(
        joined,
        &handle,
        &mut report(&handle),
        &mut owners,
        &drains,
    )
    .await
    .unwrap_err();
    assert!(
        !failure
            .message()
            .contains("private filesystem monitor payload")
    );
    assert!(
        worker_ownership::filesystem_owner_for_tests(&owners, task.id())
            .unwrap()
            .same(&producer)
    );
    let (stop, persistence) = writer(handle.clone());
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
    assert_eq!(
        std::fs::read(root.0.join("native-files/f0")).unwrap(),
        b"root-0\n"
    );
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    drop(finish(&handle, actor).await);
}

/// Stop can fence an unpolled offer without an actor reply. A later producer
/// with the same real token cannot resurrect writes; missing unissued canonical
/// handoff stays explicit in root custody and cannot fabricate a completed ack.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_preclaim_stop_fences_actor_source_and_retains_metadata() {
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let _ = handle
        .drain_agent_provider_dispatch_side_effects(64)
        .await
        .unwrap();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let task = filesystem_ownership::spawn_filesystem_action_worker(
        &mut workers,
        &mut owners,
        handle.clone(),
        offers[0].clone(),
    );
    let producer = worker_ownership::filesystem_owner_for_tests(&owners, task.id()).unwrap();
    producer.close().unwrap();
    let joined = workers.join_next_with_id().await.unwrap();
    record_owned_agent_provider_worker(joined, &handle, &mut report(&handle), &mut owners, &drains)
        .await
        .unwrap();
    let error = retire_agent_provider_workers(
        &mut workers,
        &handle,
        &drains,
        &mut owners,
        &mut ProviderCleanupFailures::default(),
    )
    .await
    .unwrap_err();
    assert!(error.message().contains("unissued source"));
    assert_eq!(owners.len(), 1);
    assert!(producer.transfer_acknowledgement().unwrap().is_none());
    let alias = NativeFilesystemProducer::new(offers[0].attempt().clone());
    assert!(
        handle
            .claim_native_filesystem_producer(alias)
            .await
            .is_err()
    );
    assert!(!root.0.join("native-files/f0").exists());
    drop(finish(&handle, actor).await);
}

/// The supervisor stop prefix remains usable while the driver state mutex is
/// held. Registration racing after stop closes before the first claim gate and
/// does not require access to that async mutex or another actor request.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_stop_is_independent_of_driver_lock_and_fences_late_registration() {
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let _ = handle
        .drain_agent_provider_dispatch_side_effects(64)
        .await
        .unwrap();
    let retirement = AgentProviderServiceRetirement::new(handle.clone());
    let mut state = retirement.work.state.lock().await;
    retirement.request_stop();
    let ProviderServiceState {
        workers, owners, ..
    } = &mut *state;
    let task = filesystem_ownership::spawn_filesystem_action_worker(
        workers,
        owners,
        handle.clone(),
        offers[0].clone(),
    );
    let producer = worker_ownership::filesystem_owner_for_tests(owners, task.id()).unwrap();
    assert!(producer.is_closed().unwrap());
    assert!(!producer.is_claimed().unwrap());
    let alias = NativeFilesystemProducer::new(offers[0].attempt().clone());
    assert!(
        handle
            .claim_native_filesystem_producer(alias)
            .await
            .is_err()
    );
    assert!(!root.0.join("native-files/f0").exists());
    drop(state);
    let result = retirement.retire().await;
    assert!(result.is_err());
    assert_eq!(retirement.work.state.lock().await.owners.len(), 1);
    drop(finish(&handle, actor).await);
}

/// The second action can finish before the first. Its cancelled monitor must
/// not block root monitor draining on predecessor publication. All actual joins
/// and fact projection recover concurrently, with every sibling fenced first.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_sibling_recovery_does_not_deadlock_ordered_publication() {
    use crate::host::async_runtime::native_filesystem::execute_for_tests;
    let (root, service, offers) = Box::pin(fixture(2)).await;
    let (handle, actor) = actor(service);
    let _ = handle.recover_native_filesystem_producers().await.unwrap();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    /// Releases the controlled real blocking worker even on assertion unwind.
    struct Release(std::sync::mpsc::Sender<()>);
    impl Drop for Release {
        /// Unblocks only this worker's owned test checkpoint.
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let mut ready = Vec::new();
    let mut releases = Vec::new();
    let mut tasks = Vec::new();
    let mut producers = Vec::new();
    for (index, offer) in offers.iter().enumerate() {
        let producer = NativeFilesystemProducer::new(offer.attempt().clone());
        let executing = producer.clone();
        let context = handle.clone();
        let runtime = tokio::runtime::Handle::current();
        let recorder = producer.progress_recorder();
        let (entered, observed) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        ready.push(observed);
        releases.push(Release(release));
        tasks.push(worker_ownership::spawn_owned_filesystem_agent_worker(
            &mut workers,
            &mut owners,
            producer.clone(),
            async move {
                assert!(
                    context
                        .claim_native_filesystem_producer(executing.clone())
                        .await?
                );
                assert!(
                    executing
                        .start(move |dispatch| {
                            if index == 0 {
                                let actual =
                                    execute_for_tests(dispatch, context, runtime, recorder);
                                entered.send(()).unwrap();
                                wait.recv().unwrap();
                                actual
                            } else {
                                entered.send(()).unwrap();
                                wait.recv().unwrap();
                                execute_for_tests(dispatch, context, runtime, recorder)
                            }
                        })
                        .await?
                );
                std::future::pending::<Result<AsyncAgentProviderWorkerResult>>().await
            },
        ));
        producers.push(producer);
    }
    for observed in ready {
        tokio::time::timeout(Duration::from_secs(10), observed)
            .await
            .unwrap()
            .unwrap();
    }
    owners.filesystem_stops().request_stop();
    assert!(
        producers
            .iter()
            .all(|producer| producer.is_closed().unwrap())
    );
    tasks[1].abort();
    let joined_second = workers.join_next_with_id().await.unwrap();
    // The first native worker still holds its actual result behind the gate.
    // Returning promptly here is essential: recovery must not serially await
    // second-action publication before the first monitor can be retired.
    tokio::time::timeout(
        Duration::from_secs(5),
        record_owned_agent_provider_worker(
            joined_second,
            &handle,
            &mut report(&handle),
            &mut owners,
            &drains,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(owners.len(), 2);
    assert!(!root.0.join("native-files/f1").exists());
    let (stop, persistence) = writer(handle.clone());
    drop(releases);
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
    assert!(producers.iter().all(|producer| {
        producer
            .transfer_acknowledgement()
            .unwrap()
            .is_some_and(|ack| ack.acknowledges(producer))
    }));
    assert_eq!(
        std::fs::read(root.0.join("native-files/f0")).unwrap(),
        b"root-0\n"
    );
    assert!(!root.0.join("native-files/f1").exists());
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    drop(finish(&handle, actor).await);
}

/// Missing original delivery is recovered by the sole-driver startup request.
/// A timer cannot duplicate an already offered unclaimed descriptor, and recovery
/// returns the same real source rather than fabricating another transaction.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_recovers_lost_offer_without_timer_duplicate() {
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    handle
        .queue_runtime_side_effects(vec![RuntimeSideEffect::DispatchNativeFilesystemAction {
            offer: offers[0].clone(),
        }])
        .await
        .unwrap();
    let lost = handle
        .drain_agent_provider_dispatch_side_effects(64)
        .await
        .unwrap();
    assert_eq!(
        lost.iter()
            .filter(|effect| matches!(
                effect,
                RuntimeSideEffect::DispatchNativeFilesystemAction { .. }
            ))
            .count(),
        1
    );
    drop(lost);
    assert!(
        handle
            .queue_provider_poll_timer_if_needed(7, 1)
            .await
            .unwrap()
    );
    let timers = handle.drain_timer_side_effects(64).await.unwrap();
    let key = timers
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::ScheduleTimer { key, .. }
                if key.kind == crate::runtime::RuntimeTimerKind::ProviderPoll =>
            {
                Some(key)
            }
            _ => None,
        })
        .unwrap();
    let mut batch = crate::runtime::RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Timer(crate::runtime::TimerEvent {
        key,
        now_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    }));
    handle.submit_runtime_events(batch).await.unwrap();
    assert!(
        !handle
            .drain_agent_provider_dispatch_side_effects(64)
            .await
            .unwrap()
            .iter()
            .any(|effect| matches!(
                effect,
                RuntimeSideEffect::DispatchNativeFilesystemAction { .. }
            ))
    );
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    filesystem_ownership::recover_filesystem_workers(&handle, &mut workers, &mut owners, &drains)
        .await
        .unwrap();
    assert_eq!(owners.len(), 1);
    let id = *owners.keys().next().unwrap();
    let producer = worker_ownership::filesystem_owner_for_tests(&owners, id).unwrap();
    assert_eq!(producer.attempt(), offers[0].attempt());
    let (stop, persistence) = writer(handle.clone());
    let joined = tokio::time::timeout(Duration::from_secs(10), workers.join_next_with_id())
        .await
        .unwrap()
        .unwrap();
    record_owned_agent_provider_worker(joined, &handle, &mut report(&handle), &mut owners, &drains)
        .await
        .unwrap();
    assert!(owners.is_empty());
    assert_eq!(
        std::fs::read(root.0.join("native-files/f0")).unwrap(),
        b"root-0\n"
    );
    assert!(
        handle
            .recover_native_filesystem_producers()
            .await
            .unwrap()
            .is_empty()
    );
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    drop(finish(&handle, actor).await);
}

/// A replacement observer receives the actual previously registered producer
/// allocation and handle. Even after a lost monitor and stop, it adopts the
/// original real result once; reconstructing a producer would lose this proof.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_recovery_uses_registered_original_worker() {
    use crate::host::async_runtime::native_filesystem::execute_for_tests;
    let (root, service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(service);
    let _ = handle.recover_native_filesystem_producers().await.unwrap();
    let mut old_workers = JoinSet::new();
    let mut old_owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let producer = NativeFilesystemProducer::new(offers[0].attempt().clone());
    let executing = producer.clone();
    let context = handle.clone();
    let runtime = tokio::runtime::Handle::current();
    let recorder = producer.progress_recorder();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    /// Releases the real result checkpoint even if a later control fails.
    struct Release(std::sync::mpsc::Sender<()>);
    impl Drop for Release {
        /// The exact worker owns this receiver; no unrelated process is signalled.
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let release = Release(release);
    let old = worker_ownership::spawn_owned_filesystem_agent_worker(
        &mut old_workers,
        &mut old_owners,
        producer.clone(),
        async move {
            assert!(
                context
                    .claim_native_filesystem_producer(executing.clone())
                    .await?
            );
            assert!(
                executing
                    .start(move |dispatch| {
                        let actual = execute_for_tests(dispatch, context, runtime, recorder);
                        entered.send(()).unwrap();
                        wait.recv().unwrap();
                        actual
                    })
                    .await?
            );
            std::future::pending::<Result<AsyncAgentProviderWorkerResult>>().await
        },
    );
    ready.await.unwrap();
    old.abort();
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
    assert!(recovered.same(&producer));
    let (stop, persistence) = writer(handle.clone());
    drop(release);
    let joined = tokio::time::timeout(Duration::from_secs(10), workers.join_next_with_id())
        .await
        .unwrap()
        .unwrap();
    record_owned_agent_provider_worker(joined, &handle, &mut report(&handle), &mut owners, &drains)
        .await
        .unwrap();
    assert!(owners.is_empty());
    let old_join = old_workers.join_next_with_id().await.unwrap();
    record_owned_agent_provider_worker(
        old_join,
        &handle,
        &mut report(&handle),
        &mut old_owners,
        &drains,
    )
    .await
    .unwrap();
    assert!(old_owners.is_empty());
    assert!(
        producer
            .transfer_acknowledgement()
            .unwrap()
            .unwrap()
            .acknowledges(&recovered)
    );
    assert_eq!(
        std::fs::read(root.0.join("native-files/f0")).unwrap(),
        b"root-0\n"
    );
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    drop(finish(&handle, actor).await);
}

/// A genuinely cancelled recovery observer retains its original failure even
/// after another observer completes the exact original transfer. Both normal
/// draining and final retirement must release acknowledged root custody after
/// joining that failed observer, without replaying the committed mutation.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_late_ack_releases_failed_recovery_without_hiding_error() {
    for final_retirement in [false, true] {
        let (root, service, offers) = Box::pin(fixture(1)).await;
        let (handle, actor) = actor(service);
        let mut workers = JoinSet::new();
        let mut owners = AgentProviderWorkerOwners::default();
        let drains: ProviderObservationDrains = Default::default();
        let task = filesystem_ownership::spawn_filesystem_action_worker(
            &mut workers,
            &mut owners,
            handle.clone(),
            offers[0].clone(),
        );
        let producer = worker_ownership::filesystem_owner_for_tests(&owners, task.id()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while producer.joined_outcome().is_none() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let joined = workers.join_next_with_id().await.unwrap();
        record_owned_agent_provider_worker(
            joined,
            &handle,
            &mut report(&handle),
            &mut owners,
            &drains,
        )
        .await
        .unwrap();
        let recovery = worker_ownership::filesystem_recovery_for_tests(&owners, task.id()).unwrap();
        recovery.abort_recovery_for_tests().await;
        let original = recovery.observe_recovery(&handle).await.unwrap_err();
        assert!(original.message().contains("recovery observer failed"));
        assert!(producer.transfer_acknowledgement().unwrap().is_none());
        assert_eq!(owners.len(), 1);
        let (stop, persistence) = writer(handle.clone());
        tokio::time::timeout(
            Duration::from_secs(10),
            filesystem_ownership::retire_filesystem_owner(&handle, &producer),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            producer
                .transfer_acknowledgement()
                .unwrap()
                .unwrap()
                .acknowledges(&producer)
        );
        let error = if final_retirement {
            retire_agent_provider_workers(
                &mut workers,
                &handle,
                &drains,
                &mut owners,
                &mut ProviderCleanupFailures::default(),
            )
            .await
            .unwrap_err()
        } else {
            worker_ownership::drain_filesystem_recoveries(&handle, &mut owners)
                .await
                .unwrap_err()
        };
        assert!(error.message().contains("recovery observer failed"));
        assert!(owners.is_empty());
        assert_eq!(
            std::fs::read(root.0.join("native-files/f0")).unwrap(),
            b"root-0\n"
        );
        stop.store(true, Ordering::Release);
        persistence.await.unwrap();
        drop(finish(&handle, actor).await);
    }
}

/// The real provider builder and supervisor retain filesystem work through
/// cancellation of both the service wrapper and its outer supervision observer.
/// An independently owned real writer isolates provider custody from the pending
/// production persistence shutdown-order change. The actual descriptor result is
/// held after its commit, so stop cannot masquerade as an unexecuted action.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_supervisor_cancellation_retains_actual_driver_and_transfer() {
    use crate::host::async_runtime::native_filesystem::execute_for_tests;
    let (root, runtime_service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(runtime_service);
    let service = crate::host::async_runtime::provider::build_async_agent_provider_service(
        "filesystem-root-supervisor",
        handle.clone(),
        AsyncAgentProviderServiceConfig::new(1).unwrap(),
    )
    .unwrap();
    let retirement = service.agent_provider_retirement.as_ref().unwrap().clone();
    let producer = NativeFilesystemProducer::new(offers[0].attempt().clone());
    let executing = producer.clone();
    let context = handle.clone();
    let runtime = tokio::runtime::Handle::current();
    let recorder = producer.progress_recorder();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    /// Always releases this original blocking worker on fixture unwind.
    struct Release(std::sync::mpsc::Sender<()>);
    impl Drop for Release {
        /// No task other than the captured worker receives this signal.
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let release = Release(release);
    {
        let mut state = retirement.work.state.lock().await;
        let ProviderServiceState {
            workers, owners, ..
        } = &mut *state;
        worker_ownership::spawn_owned_filesystem_agent_worker(
            workers,
            owners,
            producer.clone(),
            async move {
                assert!(
                    context
                        .claim_native_filesystem_producer(executing.clone())
                        .await?
                );
                assert!(
                    executing
                        .start(move |dispatch| {
                            let actual = execute_for_tests(dispatch, context, runtime, recorder);
                            entered.send(()).unwrap();
                            gate.recv().unwrap();
                            actual
                        })
                        .await?
                );
                std::future::pending::<Result<AsyncAgentProviderWorkerResult>>().await
            },
        );
    }
    ready.await.unwrap();
    let (stop, persistence) = writer(handle.clone());
    let primary =
        crate::host::async_runtime::AsyncRuntimeService::new("filesystem-primary", async {
            std::future::pending::<()>().await;
            Ok(crate::host::async_runtime::AsyncRuntimeServiceExit::completed(0))
        });
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let mut supervision = tokio::spawn(
        crate::host::async_runtime::supervise_async_runtime_services(
            vec![service, primary],
            async {
                let _ = cancelled.await;
            },
        ),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !retirement.driver_started_for_tests() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !producer.is_closed().unwrap() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut supervision)
            .await
            .is_err()
    );
    supervision.abort();
    assert!(supervision.await.unwrap_err().is_cancelled());
    let mut cleanup = Box::pin(retirement.retire());
    assert!(futures_util::poll!(cleanup.as_mut()).is_pending());
    drop(cleanup);
    assert!(producer.transfer_acknowledgement().unwrap().is_none());
    drop(release);
    tokio::time::timeout(Duration::from_secs(10), retirement.retire())
        .await
        .unwrap()
        .unwrap();
    assert!(retirement.work.state.lock().await.owners.is_empty());
    assert!(
        producer
            .transfer_acknowledgement()
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    assert_eq!(
        std::fs::read(root.0.join("native-files/f0")).unwrap(),
        b"root-0\n"
    );
    stop.store(true, Ordering::Release);
    persistence.await.unwrap();
    drop(finish(&handle, actor).await);
}

/// Real supervised persistence remains available for final original source
/// emitted after native join. A rescue writer only reaps the pre-fix failure;
/// requiring it always fails this test after all genuine workers retire.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_supervised_persistence_outlives_native_producer_retirement() {
    Box::pin(qualify_supervised_filesystem_persistence(
        u64::MAX,
        PersistenceShutdownScenario::Normal,
    ))
    .await;
}

/// Finite normal polling releases the wrapper without terminating its actual
/// writer before held producer retirement creates the original final records.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_finite_persistence_budget_preserves_final_drain() {
    Box::pin(qualify_supervised_filesystem_persistence(
        1,
        PersistenceShutdownScenario::Normal,
    ))
    .await;
}

/// Cancelling supervision and both provider/writer cleanup observers conserves
/// the same actual handles; resumed retirement obtains the original exact ack.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_supervised_persistence_survives_cancelled_cleanup_observers() {
    Box::pin(qualify_supervised_filesystem_persistence(
        u64::MAX,
        PersistenceShutdownScenario::CancelledObservers,
    ))
    .await;
}

/// Actual writer task failure must observe native join, report unavailable
/// original custody without false ack, and allow original-source-only recovery.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_failed_persistence_retains_source_for_replacement_writer() {
    Box::pin(qualify_supervised_filesystem_persistence(
        u64::MAX,
        PersistenceShutdownScenario::WriterFailure,
    ))
    .await;
}

/// Writer failure after finite wrapper completion still requires an actual
/// retained join; readiness alone cannot certify unavailable or successful I/O.
#[tokio::test(flavor = "current_thread")]
async fn filesystem_root_failed_persistence_after_finite_wrapper_remains_observable() {
    Box::pin(qualify_supervised_filesystem_persistence(
        1,
        PersistenceShutdownScenario::WriterFailure,
    ))
    .await;
}

/// Test variation changes only the owned observation/failure boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PersistenceShutdownScenario {
    /// Observe ordinary supervised producer-before-writer retirement.
    Normal,
    /// Drop original outer and cleanup observers, then resume actual joins.
    CancelledObservers,
    /// Cancel the actual writer and recover with its original registered source.
    WriterFailure,
}

/// Shared actual provider/descriptor/writer fixture for five lifetime owners.
async fn qualify_supervised_filesystem_persistence(
    max_polls: u64,
    scenario: PersistenceShutdownScenario,
) {
    use crate::host::async_runtime::native_filesystem::execute_for_tests;
    let (root, runtime_service, offers) = Box::pin(fixture(1)).await;
    let (handle, actor) = actor(runtime_service);
    let service = crate::host::async_runtime::provider::build_async_agent_provider_service(
        "filesystem-root-supervisor",
        handle.clone(),
        AsyncAgentProviderServiceConfig::new(1).unwrap(),
    )
    .unwrap();
    let retirement = service.agent_provider_retirement.as_ref().unwrap().clone();
    let producer = NativeFilesystemProducer::new(offers[0].attempt().clone());
    let executing = producer.clone();
    let context = handle.clone();
    let runtime = tokio::runtime::Handle::current();
    let recorder = producer.progress_recorder();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    /// Always releases this original blocking worker on fixture unwind.
    struct Release(std::sync::mpsc::Sender<()>);
    impl Drop for Release {
        /// No task other than the captured worker receives this signal.
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let release = Release(release);
    {
        let mut state = retirement.work.state.lock().await;
        let ProviderServiceState {
            workers, owners, ..
        } = &mut *state;
        worker_ownership::spawn_owned_filesystem_agent_worker(
            workers,
            owners,
            producer.clone(),
            async move {
                assert!(
                    context
                        .claim_native_filesystem_producer(executing.clone())
                        .await?
                );
                assert!(
                    executing
                        .start(move |dispatch| {
                            let actual = execute_for_tests(dispatch, context, runtime, recorder);
                            entered.send(()).unwrap();
                            gate.recv().unwrap();
                            actual
                        })
                        .await?
                );
                std::future::pending::<Result<AsyncAgentProviderWorkerResult>>().await
            },
        );
    }
    ready.await.unwrap();
    let persistence = crate::host::async_runtime::build_async_persistence_side_effect_service(
        "filesystem-persistence",
        handle.clone(),
        crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
            max_polls,
            drain_limit: 64,
            idle_interval: Duration::from_millis(1),
        },
    )
    .unwrap();
    let writer_owner = persistence.persistence_retirement.as_ref().unwrap().clone();
    let primary =
        crate::host::async_runtime::AsyncRuntimeService::new("filesystem-primary", async {
            std::future::pending::<()>().await;
            Ok(crate::host::async_runtime::AsyncRuntimeServiceExit::completed(0))
        });
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let mut supervision = tokio::spawn(
        crate::host::async_runtime::supervise_async_runtime_services(
            vec![service, persistence, primary],
            async {
                let _ = cancelled.await;
            },
        ),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !retirement.driver_started_for_tests() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.send(()).unwrap();
    if scenario == PersistenceShutdownScenario::WriterFailure {
        writer_owner.abort_driver_for_tests();
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while !producer.is_closed().unwrap() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut supervision)
            .await
            .is_err()
    );
    let stranded = match scenario {
        PersistenceShutdownScenario::CancelledObservers => {
            supervision.abort();
            assert!(supervision.await.unwrap_err().is_cancelled());
            let mut writer_join = Box::pin(writer_owner.retire());
            assert!(futures_util::poll!(writer_join.as_mut()).is_pending());
            drop(writer_join);
            let mut provider_join = Box::pin(retirement.retire());
            assert!(futures_util::poll!(provider_join.as_mut()).is_pending());
            drop(provider_join);
            assert!(producer.transfer_acknowledgement().unwrap().is_none());
            drop(release);
            tokio::time::timeout(Duration::from_secs(10), retirement.retire())
                .await
                .unwrap()
                .unwrap();
            let actual = tokio::time::timeout(Duration::from_secs(10), writer_owner.retire())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(actual.failed, 0);
            assert!(actual.completed > 0);
            false
        }
        PersistenceShutdownScenario::WriterFailure => {
            drop(release);
            let error = tokio::time::timeout(Duration::from_secs(10), &mut supervision)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(error.message().contains("persistence"));
            assert!(producer.joined_outcome().is_some());
            assert!(producer.transfer_acknowledgement().unwrap().is_none());
            assert_eq!(retirement.work.state.lock().await.owners.len(), 1);
            // A replacement writer reconciles original claimed source. Recovery
            // receives the ACTUAL registered producer, never reruns the patch.
            let replacement =
                crate::host::async_runtime::provider::build_async_agent_provider_service(
                    "replacement-provider",
                    handle.clone(),
                    AsyncAgentProviderServiceConfig::new(1).unwrap(),
                )
                .unwrap();
            let replacement_writer =
                crate::host::async_runtime::build_async_persistence_side_effect_service(
                    "replacement-persistence",
                    handle.clone(),
                    crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
                        max_polls: u64::MAX,
                        drain_limit: 64,
                        idle_interval: Duration::from_millis(1),
                    },
                )
                .unwrap();
            let primary = crate::host::async_runtime::AsyncRuntimeService::new(
                "replacement-primary",
                async {
                    std::future::pending::<()>().await;
                    Ok(crate::host::async_runtime::AsyncRuntimeServiceExit::completed(0))
                },
            );
            let (cancel, cancelled) = tokio::sync::oneshot::channel();
            let recovered = tokio::spawn(
                crate::host::async_runtime::supervise_async_runtime_services(
                    vec![replacement, replacement_writer, primary],
                    async {
                        let _ = cancelled.await;
                    },
                ),
            );
            tokio::time::timeout(Duration::from_secs(10), async {
                while producer.transfer_acknowledgement().unwrap().is_none() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            cancel.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(10), recovered)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let mut state = retirement.work.state.lock().await;
            let error = worker_ownership::drain_filesystem_recoveries(&handle, &mut state.owners)
                .await
                .unwrap_err();
            assert!(error.message().contains("persistence driver"));
            false
        }
        PersistenceShutdownScenario::Normal => {
            drop(release);
            let completed = tokio::time::timeout(Duration::from_secs(10), &mut supervision).await;
            let stranded = completed.is_err();
            let result = match completed {
                Ok(result) => result,
                Err(_) => {
                    let (stop, rescue) = writer(handle.clone());
                    let result = tokio::time::timeout(Duration::from_secs(10), &mut supervision)
                        .await
                        .unwrap();
                    stop.store(true, Ordering::Release);
                    rescue.await.unwrap();
                    result
                }
            };
            let report = result.unwrap().unwrap();
            assert!(
                report
                    .services
                    .iter()
                    .find(|service| service.name == "filesystem-persistence")
                    .unwrap()
                    .exit
                    .work_units
                    > 0
            );
            stranded
        }
    };
    assert!(retirement.work.state.lock().await.owners.is_empty());
    assert!(
        producer
            .transfer_acknowledgement()
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    assert_eq!(
        std::fs::read(root.0.join("native-files/f0")).unwrap(),
        b"root-0\n"
    );
    drop(finish(&handle, actor).await);
    assert!(
        !stranded,
        "supervised persistence stopped before original producer transfer"
    );
}
