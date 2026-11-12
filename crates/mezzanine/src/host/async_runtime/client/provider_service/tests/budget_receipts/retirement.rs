//! Exercise the supervisor's retained work state with a real native receipt,
//! concrete adapter, canceled producer and actual actor handoff. The final
//! assertion follows exact fixture cleanup so RED cannot strand owned work.

use super::super::super::service_retirement::AgentProviderServiceRetirement;
use super::*;

#[derive(Clone, Copy)]
enum Fault {
    None,
    Storage,
    MonitorStorage,
    SettlementCancellation,
    CompletionCancellation,
    CheckpointCancellation,
}

#[tokio::test]
async fn budget_retirement_storage_failure_retains_original_until_explicit_recovery() {
    qualify_retirement(Fault::None).await;
    qualify_retirement(Fault::Storage).await;
}

#[tokio::test]
async fn budget_retirement_canceled_monitor_keeps_failed_journal_after_claim_settlement() {
    qualify_retirement(Fault::MonitorStorage).await;
}

#[tokio::test]
async fn budget_retirement_canceled_actor_settlement_preserves_observed_refusal() {
    qualify_retirement(Fault::SettlementCancellation).await;
}

#[tokio::test]
async fn budget_retirement_canceled_completion_wait_keeps_original_drain() {
    qualify_retirement(Fault::CompletionCancellation).await;
}

#[tokio::test]
async fn budget_retirement_canceled_checkpoint_keeps_actual_writer_and_source() {
    qualify_retirement(Fault::CheckpointCancellation).await;
}

/// A finished old observer cannot remove a replacement with the same map key.
/// No provider or actor execution is needed for this identity-only guard.
#[tokio::test]
async fn budget_retirement_old_observer_cannot_release_same_key_successor() {
    let drains: ProviderObservationDrains = Default::default();
    let key = worker_ownership::ProviderObservationOwner::Inference("fixture".into(), 7);
    let producer = tokio::spawn(std::future::pending::<()>());
    let (first, completed) = tokio::sync::oneshot::channel();
    let original = ProviderObservationDrain {
        abort: producer.abort_handle(),
        completed: completed.into(),
        pending: Default::default(),
        reported: None,
    };
    first.send(()).unwrap();
    original.completed.observe().await;
    original.completed.observe().await;
    let (second, completed) = tokio::sync::oneshot::channel();
    let successor = ProviderObservationDrain {
        completed: completed.into(),
        ..original.clone()
    };
    drains.lock().unwrap().insert(key.clone(), successor);
    worker_ownership::release_observation_drain(&drains, &key, &original);
    let kept = drains.lock().unwrap().len() == 1;
    producer.abort();
    assert!(producer.await.unwrap_err().is_cancelled());
    drop(second);
    assert!(kept, "old cleanup removed the replacement journal");
}

async fn qualify_retirement(fault: Fault) {
    let Case {
        service,
        dispatch,
        issued,
        factory,
        store,
        member,
        mut root,
    } = case(BudgetAttemptPurpose::Execution).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(Transport {
        usage: serde_json::json!({"prompt_tokens":11,"completion_tokens":3}),
        calls: calls.clone(),
    })
    .unwrap()
    .with_provider_id("local-chat")
    .unwrap();
    let request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    let pending = PendingProviderWireObservations::default();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
        .with_pending_observations(pending.clone())
        .with_budget_receipts(factory.clone())
        .unwrap();
    let weak_factory = Arc::downgrade(&factory);
    drop(factory);
    let (completed_sender, completed) = tokio::sync::oneshot::channel();
    let producer = ProviderProducer::new(tokio::spawn(async move {
        let _completion = ProviderCompletionOnDrop(Some(completed_sender));
        ObservedAsyncModelProvider::new(&provider, &observer, ProviderRequestPurpose::Execution)
            .send_request_async(&request)
            .await
    }));
    let observation = receiver.recv().await.unwrap();
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let retirement = Arc::new(AgentProviderServiceRetirement::new(handle.clone()));
    let writer =
        crate::host::async_runtime::persistence_retirement::PersistenceServiceRetirement::new(
            handle.clone(),
            Default::default(),
        );
    retirement.retain_persistence_dependency(&writer).unwrap();
    let dependencies = retirement
        .work
        .state
        .lock()
        .await
        .owners
        .persistence_dependencies();
    let drains = retirement.work.state.lock().await.drains.clone();
    if matches!(fault, Fault::MonitorStorage | Fault::SettlementCancellation) {
        let mut state = retirement.work.state.lock().await;
        let service_retirement::ProviderServiceState {
            workers, owners, ..
        } = &mut *state;
        spawn_owned_agent_provider_worker(
            workers,
            owners,
            Some(ProviderWorkerOwner::inference(
                AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap(),
                &dispatch,
            )),
            std::future::pending(),
        );
    }
    let key =
        worker_ownership::ProviderObservationOwner::Inference(dispatch.turn.turn_id.clone(), 7);
    drains.lock().unwrap().insert(
        key.clone(),
        ProviderObservationDrain {
            abort: producer.abort_handle(),
            completed: ProviderProducerCompletion::from(completed).with_join(producer.retirement()),
            pending,
            reported: None,
        },
    );
    if matches!(fault, Fault::SettlementCancellation) {
        // Keep the real actor unpolled so the original failure event cannot
        // answer yet. The receipt mutex proves its read-only reconciliation
        // has finished before we cancel at that actor-settlement await.
        let connection = rusqlite::Connection::open(store.path()).unwrap();
        connection.execute_batch("CREATE TRIGGER refuse_settlement_receipt BEFORE INSERT ON budget_receipts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        let work = issued
            .registered()
            .freeze_wire_receipt(&observation)
            .unwrap();
        let owner = work.commit_owner();
        assert!(owner.commit().await.is_err());
        let completion = drains.lock().unwrap().get(&key).unwrap().completed.clone();
        let original = retirement.clone();
        let attempt = tokio::spawn(async move { original.retire().await });
        while !producer.is_finished() {
            tokio::task::yield_now().await;
        }
        completion.observe().await;
        assert!(owner.commit().await.is_err());
        assert!(
            !attempt.is_finished(),
            "actor settlement must still be pending"
        );
        attempt.abort();
        assert!(attempt.await.unwrap_err().is_cancelled());
        connection
            .execute_batch("DROP TRIGGER refuse_settlement_receipt;")
            .unwrap();
        assert!(owner.reconcile().await.unwrap().is_none());
        owner.retry_after_absence().await.unwrap();
    }
    let client = async {
        let mut retained = true;
        let mut original_error = true;
        if matches!(fault, Fault::SettlementCancellation) {
            retained = drains.lock().unwrap().len() == 1 && weak_factory.upgrade().is_some();
            retained &= dependencies.lock().unwrap().len() == 1;
            original_error = retirement.retire().await.is_err();
        } else if matches!(fault, Fault::Storage | Fault::MonitorStorage) {
            let connection = rusqlite::Connection::open(store.path()).unwrap();
            connection.execute_batch("CREATE TRIGGER refuse_retired_receipt BEFORE INSERT ON budget_receipts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
            original_error = retirement.retire().await.is_err();
            retained = drains.lock().unwrap().len() == 1 && weak_factory.upgrade().is_some();
            // The same failure stays bounded and cannot discard the original.
            assert!(retirement.retire().await.is_err());
            retained &= drains.lock().unwrap().len() == 1;
            retained &= dependencies.lock().unwrap().len() == 1;
            assert_eq!(rows(&store, "budget_receipts"), 0);
            assert!(
                handle
                    .agent_turn_is_running(&dispatch.turn.turn_id)
                    .await
                    .is_ok()
            );
            connection
                .execute_batch("DROP TRIGGER refuse_retired_receipt;")
                .unwrap();
            // Explicit recovery is bookkeeping only, never another inference.
            let work = issued
                .registered()
                .freeze_wire_receipt(&observation)
                .unwrap();
            let owner = work.commit_owner();
            assert!(owner.reconcile().await.unwrap().is_none());
            owner.retry_after_absence().await.unwrap();
            // Successful late handoff does not rewrite the original failure.
            assert!(retirement.retire().await.is_err());
        } else if matches!(
            fault,
            Fault::CompletionCancellation | Fault::CheckpointCancellation
        ) {
            let work = issued
                .registered()
                .freeze_wire_receipt(&observation)
                .unwrap();
            let owner = work.commit_owner();
            let mut gate = None;
            if matches!(fault, Fault::CheckpointCancellation) {
                let original = owner.clone();
                let execute = work.clone();
                let (started, ready) = tokio::sync::oneshot::channel();
                let (release, blocked) = std::sync::mpsc::channel();
                let waiter = tokio::spawn(async move {
                    original
                        .commit_with_for_test(move || {
                            let _ = started.send(());
                            blocked.recv().map_err(|_| {
                                MezError::invalid_state("retirement fixture released")
                            })?;
                            execute.execute()
                        })
                        .await
                });
                ready.await.unwrap();
                waiter.abort();
                assert!(waiter.await.unwrap_err().is_cancelled());
                gate = Some(release);
            }
            {
                let mut attempt = Box::pin(retirement.retire());
                assert!(futures_util::poll!(&mut attempt).is_pending());
                if matches!(fault, Fault::CheckpointCancellation) {
                    // Observe actual producer destruction before proving the
                    // second pending poll is the held receipt writer's wait.
                    while !producer.is_finished() {
                        tokio::task::yield_now().await;
                    }
                    assert!(futures_util::poll!(&mut attempt).is_pending());
                }
            }
            retained = drains.lock().unwrap().len() == 1 && weak_factory.upgrade().is_some();
            retained &= dependencies.lock().unwrap().len() == 1;
            if let Some(release) = gate {
                release.send(()).unwrap();
            }
            retirement.retire().await.unwrap();
            // Also observe the original writer if RED lost the journal owner.
            owner.commit().await.unwrap();
        } else {
            retirement.retire().await.unwrap();
        }
        assert!(producer.result().await.unwrap().unwrap_err().is_cancelled());
        let empty = drains.lock().unwrap().is_empty();
        let released = weak_factory.upgrade().is_none();
        assert!(dependencies.lock().unwrap().is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(rows(&store, "budget_receipts"), 1);
        assert_eq!(
            rows(&store, "budget_admissions"),
            1,
            "journal release is not exposure retirement"
        );
        assert_eq!(
            store.budget_account(&member.root).unwrap().known,
            UsageTotals {
                input: 11,
                output: 3
            }
        );
        handle.shutdown().await.unwrap();
        (retained, original_error, empty, released)
    };
    let ((retained, original_error, empty, released), mut exit) = tokio::join!(client, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    drop((
        exit,
        retirement,
        drains,
        dispatch,
        issued,
        handle,
        writer,
        dependencies,
    ));
    root.cleanup();
    assert!(
        retained,
        "final retirement discarded the original receipt journal"
    );
    assert!(
        original_error && empty && released,
        "retirement lost failure or exact handoff state"
    );
}
