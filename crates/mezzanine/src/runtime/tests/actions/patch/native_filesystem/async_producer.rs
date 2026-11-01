//! Actual async authority and producer custody, independent of shell lowering.
//!
//! The real actor runs scope/lease requests and accepts original raw outcomes.
//! Controlled blocking fixtures additionally prove handle/claim custody across
//! lost observers and reservation races. These are not whole provider-service
//! retirement, selector cutover or canonical/durable settlement qualification.

use super::*;
use crate::host::async_runtime::native_filesystem::{
    claim_and_start_native_filesystem_producer, retain_joined_native_filesystem_outcome,
};
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeActorExit, AsyncRuntimeSessionActor,
    AsyncRuntimeSessionHandle,
};
use crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer;
use mez_agent::native_action::{NativePatchOutcome, NativePatchPhase};
use std::time::Duration;

/// Actor fixture retaining storage/process cleanup through its original owner.
pub(super) struct ActorFixture {
    /// Original private subtree and returned runtime service on normal exit.
    fixture: Option<Fixture>,
    /// Actual serialized actor request destination.
    pub(super) handle: AsyncRuntimeSessionHandle,
    /// Actual actor task, always observed during normal fixture retirement.
    actor: Option<tokio::task::JoinHandle<AsyncRuntimeActorExit>>,
}

impl ActorFixture {
    /// Transfers the real canonical slot into an actual asynchronously run actor.
    pub(super) fn new(mut fixture: Fixture) -> Self {
        let service = std::mem::replace(&mut fixture.service, test_runtime_service());
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        Self {
            fixture: Some(fixture),
            handle,
            actor: Some(tokio::spawn(actor.run())),
        }
    }

    /// Returns the original canonical fixture directory without changing ownership.
    fn root(&self) -> &std::path::Path {
        &self.fixture.as_ref().unwrap().root
    }

    /// Observes actor shutdown and restores actual service for process cleanup.
    pub(super) async fn finish(mut self) -> Fixture {
        self.handle.shutdown().await.unwrap();
        let exit = self.actor.take().unwrap().await.unwrap();
        self.fixture.as_mut().unwrap().service = exit.service;
        self.fixture.take().unwrap()
    }
}

impl Drop for ActorFixture {
    /// A failing test cancels its actual actor task; normal paths observe exit.
    fn drop(&mut self) {
        if let Some(actor) = &self.actor {
            actor.abort();
        }
    }
}

/// A root-retained owner drives real actor scope/lease RPC and actual descriptor
/// publication. Original raw result allocation is retained once in actor and
/// producer custody; rereading it never repeats the file mutation.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "current_thread")]
async fn async_actor_rpc_commits_and_retains_original_raw_outcome() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let outcome = retain_joined_native_filesystem_outcome(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(outcome.receipts.len(), 1);
    assert_eq!(
        fs::read(actor.root().join("file")).unwrap(),
        b"native data\n"
    );
    assert!(Arc::ptr_eq(
        &outcome,
        &producer.observe().await.unwrap().unwrap()
    ));
    assert!(
        !actor
            .handle
            .retain_native_filesystem_outcome(attempt.clone(), Arc::clone(&outcome))
            .await
            .unwrap()
    );
    let projected =
        crate::host::async_runtime::native_filesystem::project_joined_native_filesystem_facts(
            &actor.handle,
            &producer,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&projected, &outcome));
    assert!(
        !actor
            .handle
            .project_native_filesystem_completion(attempt.clone())
            .await
            .unwrap()
    );
    let fixture = actor.finish().await;
    assert!(Arc::ptr_eq(
        &outcome,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Succeeded
    );
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_accepted"
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
    assert!(
        fixture
            .service
            .retained_native_filesystem_producer(&attempt)
            .unwrap()
            .same(&producer)
    );
}

/// Dropping an enqueued claim receiver cannot lose an issued dispatch: the actor
/// installs it in the already shared producer. Retirement positively observes a
/// claimed never-started slot and retains its exact unexecuted raw outcome.
#[tokio::test(flavor = "current_thread")]
async fn lost_claim_reply_keeps_actual_dispatch_for_never_started_retirement() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let service = std::mem::replace(&mut fixture.service, test_runtime_service());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut claim = Box::pin(handle.claim_native_filesystem_producer(producer.clone()));
    assert!(futures_util::poll!(claim.as_mut()).is_pending());
    drop(claim);
    let actor = ActorFixture {
        fixture: Some(fixture),
        handle,
        actor: Some(tokio::spawn(actor.run())),
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while !producer.is_claimed().unwrap() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let outcome = producer.retire().await.unwrap().unwrap();
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(outcome.receipts.is_empty());
    assert!(!actor.root().join("file").exists());
    actor
        .handle
        .retain_native_filesystem_outcome(attempt.clone(), Arc::clone(&outcome))
        .await
        .unwrap();
    let fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .retained_native_filesystem_producer(&attempt)
            .unwrap()
            .same(&producer)
    );
    assert!(Arc::ptr_eq(
        &outcome,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
}

/// Closing root admission before the actor polls a claim must leave the actual
/// canonical attempt never issued. No blocking executor starts and the actor can
/// still transfer its original unissued metadata for later refusal settlement.
#[tokio::test(flavor = "current_thread")]
async fn closed_registration_rejects_later_claim_without_issuing_work() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    assert!(producer.close().unwrap().is_none());
    let actor = ActorFixture::new(fixture);
    assert!(
        !actor
            .handle
            .claim_native_filesystem_producer(producer.clone())
            .await
            .unwrap()
    );
    assert!(!producer.is_claimed().unwrap());
    assert!(producer.retire().await.unwrap().is_none());
    let mut fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .take_unissued_native_filesystem_owner(&attempt)
            .is_ok()
    );
    assert!(!fixture.root.join("file").exists());
}

/// Sends release on every exit so a failing custody assertion cannot strand a
/// deliberately blocked fixture thread. This does not manipulate actual files.
struct Release(std::sync::mpsc::Sender<()>);
impl Drop for Release {
    /// Unblocks only this fixture's owned receiver.
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// Typed no-mutation evidence returned by the controlled blocking producer.
fn unexecuted(
    dispatch: &crate::runtime::agent::native_filesystem::RuntimeNativeFilesystemDispatch,
) -> NativePatchWorkerOutcome {
    NativePatchWorkerOutcome {
        result: NativePatchOutcome {
            identity: dispatch.work.identity.clone(),
            phase: NativePatchPhase::Snapshot,
            certainty: NativeEffectCertainty::Unexecuted,
            confirmed_effects: Vec::new(),
            diagnostic: None,
        },
        receipts: Vec::new(),
        completed_operations: Vec::new(),
    }
}

/// Cancelling both an observer and a retirement future while a real blocking
/// task is gated cannot detach its handle. Releasing that exact producer yields
/// its original immutable outcome repeatedly without consuming or replaying it.
#[tokio::test(flavor = "current_thread")]
async fn cancelled_observers_preserve_actual_blocking_handle_and_outcome() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    assert!(
        fixture
            .service
            .claim_native_filesystem_producer(&producer)
            .unwrap()
    );
    let retained = fixture
        .service
        .retained_native_filesystem_producer(&attempt)
        .unwrap();
    assert!(retained.same(&producer));
    let (started, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let release = Release(release);
    assert!(
        producer
            .start(move |dispatch| {
                started.send(()).unwrap();
                wait.recv().unwrap();
                unexecuted(&dispatch)
            })
            .await
            .unwrap()
    );
    entered.await.unwrap();
    let mut observe = Box::pin(producer.observe());
    assert!(futures_util::poll!(observe.as_mut()).is_pending());
    drop(observe);
    let mut retire = Box::pin(producer.retire());
    assert!(futures_util::poll!(retire.as_mut()).is_pending());
    drop(retire);
    drop(producer);
    drop(release);
    let outcome = retained.observe().await.unwrap().unwrap();
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(Arc::ptr_eq(
        &outcome,
        &retained.observe().await.unwrap().unwrap()
    ));
    assert!(!fixture.root.join("file").exists());
}

/// Retirement closes admission after the actor has issued a real dispatch but
/// before its callback returns outside the admission lock. That late dispatch
/// is retained and cancelled, never launched; retirement waits for the actual
/// claim reservation rather than inventing a completed or absent worker.
#[tokio::test(flavor = "current_thread")]
async fn closing_during_claim_preserves_late_unstarted_dispatch_without_locking_cancel() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt);
    let service = std::mem::replace(&mut fixture.service, test_runtime_service());
    let (started, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let release = Release(release);
    let claiming = producer.clone();
    let claim = tokio::task::spawn_blocking(move || {
        let mut service = service;
        let accepted = claiming.claim_with(|attempt| {
            let dispatch = service.claim_native_filesystem_action(attempt)?;
            started.send(()).unwrap();
            wait.recv().unwrap();
            Ok(dispatch)
        });
        (service, accepted)
    });
    entered.await.unwrap();
    assert!(producer.close().unwrap().is_none());
    let mut retire = Box::pin(producer.retire());
    assert!(futures_util::poll!(retire.as_mut()).is_pending());
    drop(release);
    let (service, accepted) = claim.await.unwrap();
    fixture.service = service;
    assert!(!accepted.unwrap());
    let outcome = retire.await.unwrap().unwrap();
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(producer.is_claimed().unwrap());
    assert!(!fixture.root.join("file").exists());
}

/// Stop before actual actor validation fences the original queue immediately,
/// even while its producer claim callback is outside the admission lock. The
/// rejected original source remains available for no-dispatch settlement; no
/// dispatch, worker join or synthetic outcome may be invented by retirement.
#[tokio::test(flavor = "current_thread")]
async fn closing_before_actor_validation_refuses_claim_without_fabricating_outcome() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let service = std::mem::replace(&mut fixture.service, test_runtime_service());
    let (started, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let release = Release(release);
    let claiming = producer.clone();
    let claim = tokio::task::spawn_blocking(move || {
        let mut service = service;
        let accepted = claiming.claim_with(|attempt| {
            started.send(()).unwrap();
            wait.recv().unwrap();
            service.claim_native_filesystem_action(attempt)
        });
        (service, accepted)
    });
    entered.await.unwrap();
    assert!(producer.close().unwrap().is_none());
    let mut retire = Box::pin(producer.retire());
    assert!(futures_util::poll!(retire.as_mut()).is_pending());
    drop(release);
    let (service, accepted) = claim.await.unwrap();
    fixture.service = service;
    assert!(accepted.unwrap_err().message().contains("cancelled"));
    assert!(retire.await.unwrap().is_none());
    assert!(!producer.is_claimed().unwrap());
    assert!(producer.joined_outcome().is_none());
    assert!(
        fixture
            .service
            .native_filesystem_attempt_failure(&attempt)
            .is_some()
    );
    assert!(
        fixture
            .service
            .take_unissued_native_filesystem_owner(&attempt)
            .is_ok()
    );
    assert!(!fixture.root.join("file").exists());
}

/// An unwinding claim callback closes and releases its reservation rather than
/// leaving future retirement waiting forever or permitting a replacement start.
#[tokio::test(flavor = "current_thread")]
async fn claim_callback_unwind_closes_reservation_and_unblocks_retirement() {
    let mut fixture = Fixture::new();
    let producer = NativeFilesystemProducer::new(fixture.queue());
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        producer.claim_with(
            |_| -> crate::Result<
                Option<crate::runtime::agent::native_filesystem::RuntimeNativeFilesystemDispatch>,
            > { panic!("fixture claim unwind") },
        )
    }));
    assert!(panic.is_err());
    assert!(producer.retire().await.unwrap().is_none());
    assert!(
        !producer
            .claim_with(|_| panic!("closed claim must not run"))
            .unwrap()
    );
    assert!(!fixture.root.join("file").exists());
}

/// Actual actor RPC rejects unrelated paths, non-preparation phases and cancelled
/// original attempts. A valid preparation request still succeeds between those
/// negative controls; none of these snapshots can become a publication lease.
#[tokio::test(flavor = "current_thread")]
async fn typed_preparation_rpc_rejects_unrelated_phase_path_and_cancelled_owner() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let actor = ActorFixture::new(fixture);
    assert!(
        actor
            .handle
            .claim_native_filesystem_producer(producer.clone())
            .await
            .unwrap()
    );
    let (captured, identity) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let release = Release(release);
    assert!(
        producer
            .start(move |dispatch| {
                captured
                    .send(Arc::new(dispatch.work.identity.clone()))
                    .unwrap();
                wait.recv().unwrap();
                unexecuted(&dispatch)
            })
            .await
            .unwrap()
    );
    let identity = identity.await.unwrap();
    let path = actor.root().join("file").to_str().unwrap().to_owned();
    assert!(
        actor
            .handle
            .native_filesystem_scopes(
                attempt.clone(),
                Arc::clone(&identity),
                NativePatchPhase::Snapshot,
                "unrelated-target".into()
            )
            .await
            .is_err()
    );
    assert!(
        actor
            .handle
            .native_filesystem_scopes(
                attempt.clone(),
                Arc::clone(&identity),
                NativePatchPhase::Plan,
                path.clone()
            )
            .await
            .is_err()
    );
    assert!(
        actor
            .handle
            .native_filesystem_scopes(
                attempt.clone(),
                Arc::clone(&identity),
                NativePatchPhase::Snapshot,
                path.clone()
            )
            .await
            .is_ok()
    );
    producer.close().unwrap();
    assert!(
        actor
            .handle
            .native_filesystem_scopes(attempt, identity, NativePatchPhase::Snapshot, path)
            .await
            .is_err()
    );
    drop(release);
    let outcome = retain_joined_native_filesystem_outcome(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(!actor.root().join("file").exists());
    let _fixture = actor.finish().await;
}

/// An unexpected blocking producer panic cannot be relabelled never-started or
/// confirmed. The actual join is observed, unknown certainty retains original
/// attribution, and arbitrary panic text never becomes its public diagnostic.
#[tokio::test(flavor = "current_thread")]
async fn unexpected_producer_join_failure_retains_original_unknown_evidence() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    assert!(
        fixture
            .service
            .claim_native_filesystem_producer(&producer)
            .unwrap()
    );
    assert!(
        producer
            .start(|_| panic!("private native producer panic payload"))
            .await
            .unwrap()
    );
    let outcome = producer.observe().await.unwrap().unwrap();
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unknown);
    assert_eq!(outcome.result.identity.turn_id, fixture.turn.turn_id);
    assert_eq!(outcome.result.identity.action_id, fixture.action.id);
    assert!(
        !outcome
            .result
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("private native")
    );
    assert!(
        fixture
            .service
            .retain_native_filesystem_outcome(&attempt, Arc::clone(&outcome))
            .unwrap()
    );
    assert!(Arc::ptr_eq(
        &outcome,
        &producer.retire().await.unwrap().unwrap()
    ));
    assert!(!fixture.root.join("file").exists());
}

/// Holds the same progress-recorder capture used by the real worker behind a
/// controlled gate. Dropping the last external producer must cancel its fence
/// before release; a callback must not keep its own task owner alive. This is a
/// no-mutation lifecycle test, not evidence of observed join after owner loss.
#[tokio::test(flavor = "current_thread")]
async fn progress_callback_does_not_prevent_last_owner_cancellation() {
    let mut fixture = Fixture::new();
    let producer = NativeFilesystemProducer::new(fixture.queue());
    // Claim actual policy/fence work without registering an extra actor-held
    // producer; this leaves exactly the one external owner tested below.
    assert!(
        producer
            .claim_with(|attempt| fixture.service.claim_native_filesystem_action(attempt))
            .unwrap()
    );
    let recorder = producer.progress_recorder();
    let (started, entered) = tokio::sync::oneshot::channel();
    let (finished, observed) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let release = Release(release);
    assert!(
        producer
            .start(move |dispatch| {
                assert!(started.send(dispatch.fence.clone()).is_ok());
                wait.recv().unwrap();
                std::hint::black_box(&recorder);
                let cancelled = dispatch.fence.check().is_err();
                finished.send(cancelled).unwrap();
                unexecuted(&dispatch)
            })
            .await
            .unwrap()
    );
    let fence = entered.await.unwrap();
    drop(producer);
    let cancelled_while_blocked = fence.check().is_err();
    drop(release);
    let worker_observed_cancellation = observed.await.unwrap();
    assert!(
        cancelled_while_blocked,
        "progress callback retained its own producer owner"
    );
    assert!(worker_observed_cancellation);
    assert!(!fixture.root.join("file").exists());
}

/// First root enrollment is immutable even before issuance. A same-source
/// lookalike cannot replace it, and dropping that rejected allocation must not
/// cancel the actual original source. Genuine actor claim remains possible;
/// owned-dispatch retirement then proves unexecuted work without launching it.
#[tokio::test(flavor = "current_thread")]
async fn preclaim_enrollment_rejects_lookalike_without_revoking_original_source() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let original = NativeFilesystemProducer::new(attempt.clone());
    original.register_root().unwrap();
    assert!(attempt.root_producer().unwrap().same(&original));
    let lookalike = NativeFilesystemProducer::new(attempt.clone());
    assert!(lookalike.register_root().is_err());
    drop(lookalike);
    assert!(
        fixture
            .service
            .claim_native_filesystem_producer(&original)
            .unwrap()
    );
    let outcome = original.retire().await.unwrap().unwrap();
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(!fixture.root.join("file").exists());
}
