//! Real actor transfers of original checked storage and actual joined producers.
//!
//! Root custody is removable only with the opaque original acknowledgement.
//! Lost replies and later source changes cannot replay completed transfers;
//! stale uncommitted source, failures, fake owners and unrelated siblings remain fenced.

use super::archive_storage::{checked, persist};
use super::async_producer::ActorFixture;
use super::*;
use crate::host::async_runtime::native_filesystem::{
    claim_and_start_native_filesystem_producer, seal_joined_native_filesystem_archive,
    transfer_joined_native_filesystem_producer,
};
use crate::runtime::native_filesystem::NativeFilesystemArchiveSettlement;
use crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer;

/// Produces actual canonical and storage evidence without transferring ownership.
async fn prepared(
    saved: bool,
) -> (
    ActorFixture,
    NativeFilesystemProducer,
    NativeFilesystemArchiveSettlement,
) {
    let mut fixture = Fixture::new();
    if saved {
        fixture
            .service
            .set_agent_transcript_store(AgentTranscriptStore::new(fixture.root.join("archive")));
        fixture.service.persistence.enable_transcript_adapter();
    }
    let producer = NativeFilesystemProducer::new(fixture.queue());
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    checked(&actor, &producer).await;
    if saved {
        persist(&actor).await;
    }
    let storage = seal_joined_native_filesystem_archive(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    (actor, producer, storage)
}

/// The actor retains its historical acknowledgement before sending its reply.
/// A lost receiver can retry the same transfer, while equal attempt labels and a
/// foreign actor cannot adopt it. Both original actor owners disappear exactly once.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_transfer_lost_reply_and_original_allocation() {
    let (actor, producer, storage) = prepared(false).await;
    let alias = NativeFilesystemProducer::new(producer.attempt().clone());
    assert!(
        actor
            .handle
            .transfer_native_filesystem_producer(alias.clone(), storage.clone())
            .await
            .is_err()
    );
    let mut lost = Box::pin(
        actor
            .handle
            .transfer_native_filesystem_producer(producer.clone(), storage.clone()),
    );
    assert!(futures_util::poll!(lost.as_mut()).is_pending());
    drop(lost);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while producer.transfer_acknowledgement().unwrap().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lost first reply must leave the actual committed acknowledgement");
    let ack = actor
        .handle
        .transfer_native_filesystem_producer(producer.clone(), storage.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(ack.acknowledges(&producer));
    assert!(!ack.acknowledges(&alias));
    let foreign = ActorFixture::new(Fixture::new());
    assert!(
        foreign
            .handle
            .transfer_native_filesystem_producer(producer.clone(), storage.clone())
            .await
            .is_err()
    );
    drop(foreign.finish().await);
    let mut fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .retained_native_filesystem_producer(producer.attempt())
            .is_none()
    );
    assert!(
        fixture
            .service
            .retained_native_filesystem_outcome(producer.attempt())
            .is_none()
    );
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(producer.attempt())
            .is_err()
    );
    assert!(
        fixture
            .service
            .transfer_native_filesystem_producer(&producer, &storage)
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    assert!(
        fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Saved storage and the actual observed native join transfer together. The
/// cached ack survives later canonical changes and actor shutdown; retaining it
/// outside the producer must not create a producer/ack self-custody cycle.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_transfer_saved_ack_is_historical_without_cycle() {
    let (actor, producer, storage) = prepared(true).await;
    let handle = actor.handle.clone();
    let ack = transfer_joined_native_filesystem_producer(&handle, &producer)
        .await
        .unwrap()
        .unwrap();
    let raw = producer.observe().await.unwrap().unwrap();
    let recorder = producer.progress_recorder();
    let mut fixture = actor.finish().await;
    assert!(
        !fixture
            .service
            .native_filesystem_archive_settlement_is_current(&storage)
    );
    fixture
        .service
        .agent_turn_contexts_mut()
        .get_mut(&fixture.turn.turn_id)
        .unwrap()
        .append_user_event("steering", "AFTER_COMPLETED_TRANSFER")
        .unwrap();
    assert!(
        fixture
            .service
            .transfer_native_filesystem_producer(&producer, &storage)
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    assert!(
        transfer_joined_native_filesystem_producer(&handle, &producer)
            .await
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    drop(producer);
    assert!(
        recorder
            .record_confirmed_progress(&raw.receipts[0])
            .unwrap()
            .is_none(),
        "completed ack retained its own producer"
    );
    drop(ack);
}

/// A source changed before the serialized transfer must be checked again. The
/// older physical seal cannot free either original actor owner or install an ack.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_transfer_rejects_stale_uncommitted_source() {
    let (actor, producer, storage) = prepared(true).await;
    let mut fixture = actor.finish().await;
    fixture
        .service
        .agent_turn_contexts_mut()
        .get_mut(&fixture.turn.turn_id)
        .unwrap()
        .append_user_event("steering", "BEFORE_TRANSFER_NEEDS_NEW_SOURCE")
        .unwrap();
    assert!(
        fixture
            .service
            .transfer_native_filesystem_producer(&producer, &storage)
            .unwrap()
            .is_none()
    );
    assert!(producer.transfer_acknowledgement().unwrap().is_none());
    assert!(
        fixture
            .service
            .retained_native_filesystem_producer(producer.attempt())
            .unwrap()
            .same(&producer)
    );
    assert!(
        fixture
            .service
            .retained_native_filesystem_outcome(producer.attempt())
            .is_some()
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// An entered required error after storage acceptance remains the first error.
/// An equivalent-looking second outcome cannot overwrite it or release custody.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_transfer_preserves_entered_required_error() {
    let (actor, producer, storage) = prepared(false).await;
    let raw = producer.observe().await.unwrap().unwrap();
    let mut fixture = actor.finish().await;
    let substituted = Arc::new(NativePatchWorkerOutcome {
        result: raw.result.clone(),
        receipts: Vec::new(),
        completed_operations: Vec::new(),
    });
    let first = fixture
        .service
        .retain_native_filesystem_outcome(producer.attempt(), substituted)
        .unwrap_err();
    let failure = fixture
        .service
        .transfer_native_filesystem_producer(&producer, &storage)
        .unwrap_err();
    assert_eq!(failure.message(), first.message());
    assert!(producer.transfer_acknowledgement().unwrap().is_none());
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(producer.attempt())
            .unwrap()
    ));
}

/// A retained typed raw result and explicit no-save storage are not proof of
/// actual producer join. Even after join, a different result allocation cannot
/// satisfy the original registry's retained outcome or mint a transfer ack.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_transfer_requires_actual_join_and_raw_allocation() {
    use mez_agent::native_action::{NativePatchOutcome, NativePatchPhase};
    let mut fixture = Fixture::new();
    let producer = NativeFilesystemProducer::new(fixture.queue());
    let actor = ActorFixture::new(fixture);
    assert!(
        actor
            .handle
            .claim_native_filesystem_producer(producer.clone())
            .await
            .unwrap()
    );
    let (capture, captured) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    /// Releases the controlled actual worker even if an assertion unwinds.
    struct Release(std::sync::mpsc::Sender<()>);
    impl Drop for Release {
        /// Unblocks the actual controlled worker on normal exit or unwind.
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let release = Release(release);
    assert!(
        producer
            .start(move |dispatch| {
                let result = NativePatchOutcome {
                    identity: dispatch.work.identity.clone(),
                    phase: NativePatchPhase::Snapshot,
                    certainty: NativeEffectCertainty::Unexecuted,
                    confirmed_effects: Vec::new(),
                    diagnostic: None,
                };
                assert!(
                    capture
                        .send(Arc::new(NativePatchWorkerOutcome {
                            result: result.clone(),
                            receipts: Vec::new(),
                            completed_operations: Vec::new(),
                        }))
                        .is_ok()
                );
                wait.recv().unwrap();
                NativePatchWorkerOutcome {
                    result,
                    receipts: Vec::new(),
                    completed_operations: Vec::new(),
                }
            })
            .await
            .unwrap()
    );
    let early = captured.await.unwrap();
    actor
        .handle
        .retain_native_filesystem_outcome(producer.attempt().clone(), early.clone())
        .await
        .unwrap();
    actor
        .handle
        .project_native_filesystem_completion(producer.attempt().clone())
        .await
        .unwrap();
    let source = actor
        .handle
        .prepare_native_filesystem_archive(producer.attempt().clone())
        .await
        .unwrap()
        .unwrap();
    let storage = actor
        .handle
        .observe_native_filesystem_archive_storage(source)
        .await
        .unwrap()
        .unwrap();
    assert!(
        actor
            .handle
            .transfer_native_filesystem_producer(producer.clone(), storage.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert!(producer.transfer_acknowledgement().unwrap().is_none());
    drop(release);
    let joined = producer.observe().await.unwrap().unwrap();
    assert!(!Arc::ptr_eq(&early, &joined));
    assert!(
        actor
            .handle
            .transfer_native_filesystem_producer(producer.clone(), storage)
            .await
            .is_err()
    );
    let fixture = actor.finish().await;
    assert!(Arc::ptr_eq(
        &early,
        &fixture
            .service
            .retained_native_filesystem_outcome(producer.attempt())
            .unwrap()
    ));
}

/// Completing the first original producer cannot settle its queued sibling or
/// grant group continuation. The ack cannot be borrowed by the sibling owner.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_transfer_keeps_unsettled_sibling_fenced() {
    let mut fixture = Fixture::with_actions(false, |root| {
        ["first", "second"]
            .into_iter()
            .map(|id| mez_agent::AgentAction {
                id: id.into(),
                payload: mez_agent::AgentActionPayload::ApplyPatch {
                    patch: format!(
                        "*** Begin Patch\n*** Add File: {}\n+{id}\n*** End Patch",
                        root.join(id).display()
                    ),
                    strip: None,
                },
            })
            .collect()
    });
    let first = fixture.queue();
    let second_action = fixture
        .execution
        .response
        .action_batch
        .as_ref()
        .unwrap()
        .actions[1]
        .clone();
    let second = fixture
        .service
        .queue_native_filesystem_action(&fixture.turn, &second_action, &fixture.execution)
        .unwrap();
    let producer = NativeFilesystemProducer::new(first);
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let ack = transfer_joined_native_filesystem_producer(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert!(!ack.acknowledges(&NativeFilesystemProducer::new(second.clone())));
    let mut fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&second)
            .unwrap()
            .is_none()
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// A real descriptor commit can precede an unexpected worker panic. Recover the
/// actual captured endpoint into the unknown outcome, without promoting whole
/// operation completion or certainty, before canonical archive and transfer.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_transfer_preserves_confirmed_prefix_after_worker_panic() {
    use crate::host::async_runtime::native_filesystem::{
        execute_for_tests, retain_joined_native_filesystem_outcome,
    };
    let mut fixture = Fixture::with_patch(false, |root| {
        let mut patch = "*** Begin Patch\n".to_owned();
        for index in 0..65 {
            patch.push_str(&format!(
                "*** Add File: {}\n+entry-{index}\n",
                root.join(format!("f{index}")).display()
            ));
        }
        patch.push_str("*** End Patch");
        patch
    });
    let path = fixture.root.join("f64");
    let producer = NativeFilesystemProducer::new(fixture.queue());
    let actor = ActorFixture::new(fixture);
    assert!(
        actor
            .handle
            .claim_native_filesystem_producer(producer.clone())
            .await
            .unwrap()
    );
    let handle = actor.handle.clone();
    let runtime = tokio::runtime::Handle::current();
    let recorder = producer.progress_recorder();
    assert!(
        producer
            .start(move |dispatch| {
                let _actual = execute_for_tests(dispatch, handle, runtime, recorder);
                panic!("controlled panic after actual descriptor result");
            })
            .await
            .unwrap()
    );
    let raw = retain_joined_native_filesystem_outcome(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"entry-64\n");
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Unknown);
    assert_eq!(
        raw.receipts.len(),
        65,
        "actual confirmed endpoint vanished after genuine worker panic"
    );
    assert_eq!(
        producer
            .progress_snapshot()
            .unwrap()
            .confirmed()
            .unwrap()
            .len(),
        64
    );
    assert!(raw.completed_operations.is_empty());
    assert!(raw.result.confirmed_effects.is_empty());
    let ack = transfer_joined_native_filesystem_producer(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert!(ack.acknowledges(&producer));
    let fixture = actor.finish().await;
    let result = &fixture
        .service
        .agent_turn_executions()
        .get(&fixture.turn.turn_id)
        .unwrap()
        .action_results[0];
    let metadata: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_ref().unwrap()).unwrap();
    assert_eq!(
        metadata["confirmed_endpoints"].as_array().unwrap().len(),
        65
    );
    assert_eq!(metadata["effect_certainty"], "unknown");
    assert!(
        metadata["completed_operations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// Even a genuinely joined worker cannot transfer if its returned unknown raw
/// outcome omits a recorded actual post-commit endpoint. Keep both original
/// owners and first failure rather than treating an empty result as no effects.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_transfer_refuses_raw_that_omits_captured_confirmation() {
    use crate::host::async_runtime::native_filesystem::{
        execute_for_tests, retain_joined_native_filesystem_outcome,
    };
    let mut fixture = Fixture::new();
    let path = fixture.root.join("file");
    let producer = NativeFilesystemProducer::new(fixture.queue());
    let actor = ActorFixture::new(fixture);
    assert!(
        actor
            .handle
            .claim_native_filesystem_producer(producer.clone())
            .await
            .unwrap()
    );
    let handle = actor.handle.clone();
    let runtime = tokio::runtime::Handle::current();
    let recorder = producer.progress_recorder();
    assert!(
        producer
            .start(move |dispatch| {
                let mut actual = execute_for_tests(dispatch, handle, runtime, recorder);
                actual.receipts.clear();
                actual.completed_operations.clear();
                actual.result.certainty = NativeEffectCertainty::Unknown;
                actual.result.confirmed_effects.clear();
                actual.result.diagnostic =
                    Some("deliberate negative-control omission of actual endpoint".into());
                actual
            })
            .await
            .unwrap()
    );
    let raw = retain_joined_native_filesystem_outcome(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert!(raw.receipts.is_empty());
    assert_eq!(fs::read(path).unwrap(), b"native data\n");
    assert_eq!(
        producer
            .progress_snapshot()
            .unwrap()
            .confirmed()
            .unwrap()
            .len(),
        1
    );
    let failure = transfer_joined_native_filesystem_producer(&actor.handle, &producer)
        .await
        .unwrap_err();
    assert!(
        failure
            .message()
            .contains("omits retained confirmed endpoints")
    );
    assert!(producer.transfer_acknowledgement().unwrap().is_none());
    let fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .retained_native_filesystem_producer(producer.attempt())
            .unwrap()
            .same(&producer)
    );
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(producer.attempt())
            .unwrap()
    ));
    assert_eq!(
        fixture
            .service
            .ensure_completion_required_gates_ready(&fixture.turn.turn_id)
            .unwrap_err()
            .message(),
        failure.message()
    );
}

/// Late real facts after forced retirement must transfer from original saved
/// backing even if the current store was replaced. Run the actual new actor,
/// checked read and writer; keep the group's original first failure unchanged.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_transfer_late_failed_backing_keeps_original_saved_target() {
    use crate::host::async_runtime::native_filesystem::retain_joined_native_filesystem_outcome;
    let mut fixture = Fixture::new();
    let original = AgentTranscriptStore::new(fixture.root.join("original-archive"));
    let replacement = AgentTranscriptStore::new(fixture.root.join("replacement-archive"));
    let conversation_id = fixture.turn.conversation_id.clone();
    fixture.service.set_agent_transcript_store(original.clone());
    fixture.service.persistence.enable_transcript_adapter();
    let producer = NativeFilesystemProducer::new(fixture.queue());
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let raw = retain_joined_native_filesystem_outcome(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    let mut fixture = actor.finish().await;
    let first = fixture
        .service
        .prepare_completion_forced_retirement(&fixture.turn)
        .unwrap_err();
    fixture
        .service
        .set_agent_transcript_store(replacement.clone());
    let actor = ActorFixture::new(fixture);
    checked(&actor, &producer).await;
    // The real writer may finish its claimed append, then report the retained
    // group error from generic event ingress. Preserve that error and require
    // independent actual read/sync evidence instead of treating it as storage.
    let writer = crate::host::async_runtime::run_async_persistence_side_effect_service(
        &actor.handle,
        crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
            max_polls: 2,
            drain_limit: 64,
            idle_interval: std::time::Duration::from_millis(1),
        },
        |polls, _| polls >= 2,
    )
    .await;
    match writer {
        Ok(report) => assert_eq!(report.failed, 0),
        Err(error) => assert_eq!(error.message(), first.message()),
    }
    assert!(
        original
            .inspect(&conversation_id)
            .unwrap()
            .iter()
            .any(|row| row
                .content
                .contains("Native patch completed; 1 confirmed file endpoint(s)."))
    );
    let ack = transfer_joined_native_filesystem_producer(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert!(ack.acknowledges(&producer));
    assert!(Arc::ptr_eq(&raw, &producer.joined_outcome().unwrap()));
    let fixture = actor.finish().await;
    assert_eq!(
        fixture
            .service
            .ensure_completion_required_gates_ready(&fixture.turn.turn_id)
            .unwrap_err()
            .message(),
        first.message()
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
    assert!(
        fixture
            .service
            .retained_native_filesystem_producer(producer.attempt())
            .is_none()
    );
    assert!(
        original
            .inspect(&conversation_id)
            .unwrap()
            .iter()
            .any(|row| row
                .content
                .contains("Native patch completed; 1 confirmed file endpoint(s)."))
    );
    match replacement.inspect(&conversation_id) {
        Ok(rows) => assert!(!rows.iter().any(|row| {
            row.content
                .contains("Native patch completed; 1 confirmed file endpoint(s).")
        })),
        Err(error) => assert_eq!(error.kind(), crate::error::MezErrorKind::NotFound),
    }
}
