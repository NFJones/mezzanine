//! Request-local auxiliary unwind boundaries and exact captured failure identity.
//! These controls exercise the same spawn/ownership and completion adapters as
//! production monitors; actor acceptance and process-abort qualification are
//! separate contracts, not inferred from these component tests.

use super::*;

/// Injects construction and polling panics independently for each auxiliary
/// family. The monitor-side identity survives and no successful completion or
/// arbitrary payload is published; an independently owned sibling still joins.
#[tokio::test]
async fn auxiliary_worker_panics_preserve_exact_failure_identity_and_sibling() {
    for construction_panic in [false, true] {
        for family in 0..3 {
            let sibling = tokio::spawn(async { 73 });
            let (worker, _owner) = spawn_auxiliary_provider_worker(move || {
                assert!(!construction_panic, "private-construction-payload");
                async {
                    tokio::task::yield_now().await;
                    panic!("private-poll-payload");
                    #[allow(unreachable_code)]
                    std::future::pending::<Result<mez_agent::ModelResponse>>().await
                }
            });
            let error = worker.await.unwrap_err();
            assert!(error.is_panic());
            assert_auxiliary_failure(family, error, "panic");
            assert_eq!(sibling.await.unwrap(), 73);
        }
    }
}

/// Cancels a genuinely pending owned request after its startup acknowledgement,
/// then observes its join rather than interpreting an abort request as completed
/// cleanup. Each adapter publishes cancellation against its captured owner only.
#[tokio::test]
async fn auxiliary_worker_cancellation_preserves_exact_failure_identity() {
    for family in 0..3 {
        let (ready, started) = tokio::sync::oneshot::channel();
        let (worker, owner) = spawn_auxiliary_provider_worker(move || async move {
            let _ = ready.send(());
            std::future::pending::<Result<mez_agent::ModelResponse>>().await
        });
        started.await.unwrap();
        drop(owner);
        let error = tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.is_cancelled());
        assert_auxiliary_failure(family, error, "cancelled");
    }
}

/// Verifies cancellation of the containing monitor requests destruction of its
/// child rather than detaching a live request. The acknowledgement is emitted
/// by the actual child destructor and does not stand in for actor settlement.
#[tokio::test]
async fn auxiliary_monitor_drop_destroys_its_pending_worker() {
    /// Acknowledges actual pending child destruction after monitor abort.
    struct Dropped(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let (ready, started) = tokio::sync::oneshot::channel();
    let (dropped, destroyed) = tokio::sync::oneshot::channel();
    let monitor = tokio::spawn(async move {
        let (worker, _owner) = spawn_auxiliary_provider_worker(move || async move {
            let _dropped = Dropped(Some(dropped));
            let _ = ready.send(());
            std::future::pending::<()>().await;
        });
        worker.await.unwrap();
    });
    started.await.unwrap();
    monitor.abort();
    assert!(monitor.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), destroyed)
        .await
        .unwrap()
        .unwrap();
}

/// Positive controls retain successful results and ordinary typed errors across
/// the new boundary. Optional title generation keeps its exact attempt and
/// normalized usage rather than treating every joined task as a failure.
#[tokio::test]
async fn auxiliary_worker_success_and_typed_failure_remain_distinct() {
    let (worker, _owner) = spawn_auxiliary_provider_worker(|| async { 41 });
    assert_eq!(worker.await.unwrap(), 41);
    let (worker, _owner) = spawn_auxiliary_provider_worker(|| async {
        Err::<mez_agent::ModelResponse, _>(MezError::invalid_args("typed failure"))
    });
    let (event, completed) = compaction_worker_event("pane-exact".into(), 19, worker.await);
    assert!(!completed);
    let RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed { kind, message, .. }) = event
    else {
        panic!("typed compaction failure");
    };
    assert_eq!(kind, "invalid_args");
    assert_eq!(message, "typed failure");
    let (worker, _owner) = spawn_auxiliary_provider_worker(|| async {
        (
            AgentSessionTitleOutcome::Generated("A bounded title".into()),
            mez_agent::ModelTokenUsage {
                input_tokens: 11,
                ..Default::default()
            },
        )
    });
    let (event, completed) = session_title_worker_event(
        "conversation-exact".into(),
        "attempt-exact".into(),
        worker.await,
    );
    assert!(!completed);
    let RuntimeEvent::AgentSessionTitle(AgentSessionTitleEvent::WorkerSettled {
        conversation_id,
        attempt_id,
        usage,
        outcome,
    }) = event
    else {
        panic!("title settlement");
    };
    assert_eq!(conversation_id, "conversation-exact");
    assert_eq!(attempt_id, "attempt-exact");
    assert_eq!(usage.input_tokens, 11);
    assert_eq!(
        outcome,
        AgentSessionTitleOutcome::Generated("A bounded title".into())
    );
}

/// Checks captured identities and bounded error vocabulary for one joined fault.
/// Unknown incurred usage remains unknown/default, with no invented response or
/// raw provider fields sourced from the panic payload.
fn assert_auxiliary_failure(family: usize, error: tokio::task::JoinError, category: &str) {
    let (event, completed) = match family {
        0 => compaction_worker_event("pane-exact".into(), 19, Err(error)),
        1 => remember_worker_event("pane-exact".into(), "observation-exact".into(), Err(error)),
        2 => session_title_worker_event(
            "conversation-exact".into(),
            "attempt-exact".into(),
            Err(error),
        ),
        _ => unreachable!(),
    };
    assert!(!completed);
    match event {
        RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed {
            pane_id,
            task_generation,
            kind,
            message,
            usage,
            provider_failure_json,
            provider_raw_text,
        }) => {
            assert_eq!(pane_id, "pane-exact");
            assert_eq!(task_generation, 19);
            assert_eq!(kind, "invalid_state");
            assert_eq!(message, format!("provider worker join failed: {category}"));
            assert_eq!(usage, Default::default());
            assert!(provider_failure_json.is_none() && provider_raw_text.is_none());
        }
        RuntimeEvent::AgentRemember(AgentRememberEvent::Failed {
            pane_id,
            observation_id,
            kind,
            message,
            usage,
            provider_failure_json,
            provider_raw_text,
        }) => {
            assert_eq!(pane_id, "pane-exact");
            assert_eq!(observation_id, "observation-exact");
            assert_eq!(kind, "invalid_state");
            assert_eq!(message, format!("provider worker join failed: {category}"));
            assert_eq!(usage, Default::default());
            assert!(provider_failure_json.is_none() && provider_raw_text.is_none());
        }
        RuntimeEvent::AgentSessionTitle(AgentSessionTitleEvent::WorkerSettled {
            conversation_id,
            attempt_id,
            usage,
            outcome,
        }) => {
            assert_eq!(conversation_id, "conversation-exact");
            assert_eq!(attempt_id, "attempt-exact");
            assert_eq!(usage, Default::default());
            assert_eq!(
                outcome,
                AgentSessionTitleOutcome::Rejected(format!("worker_{category}"))
            );
        }
        _ => panic!("exact auxiliary failure"),
    }
}
