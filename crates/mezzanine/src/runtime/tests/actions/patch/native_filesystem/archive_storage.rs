//! Actual actor and root producer gates for original physical archive evidence.
//!
//! These tests run the descriptor worker and ordinary ordered persistence service.
//! A logical source, a physical prefix seal and producer release remain distinct.
use super::async_producer::ActorFixture;
use super::*;
use crate::host::async_runtime::native_filesystem::{
    claim_and_start_native_filesystem_producer, prepare_joined_native_filesystem_archive,
    seal_joined_native_filesystem_archive,
};
use crate::host::async_runtime::{
    AsyncRuntimeSideEffectServiceConfig, run_async_persistence_side_effect_service,
};
use crate::runtime::native_filesystem::NativeFilesystemArchiveAcceptance;
use crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer;
use std::time::Duration;

/// Waits for actual off-actor bookkeeping rather than manufacturing admission.
async fn checked(
    actor: &ActorFixture,
    producer: &NativeFilesystemProducer,
) -> NativeFilesystemArchiveAcceptance {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(source) = prepare_joined_native_filesystem_archive(&actor.handle, producer)
                .await
                .unwrap()
            {
                return source;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap()
}

/// Drives the real durable journal/append/settle lane before physical verification.
async fn persist(actor: &ActorFixture) {
    let report = run_async_persistence_side_effect_service(
        &actor.handle,
        AsyncRuntimeSideEffectServiceConfig {
            max_polls: 2,
            drain_limit: 64,
            idle_interval: Duration::from_millis(1),
        },
        |polls, _| polls >= 2,
    )
    .await
    .unwrap();
    assert_eq!(report.failed, 0);
}

/// Saved logical admission cannot substitute for physical rows or observed join.
/// The real writer unblocks the original root-held verifier; a lookalike producer
/// with identical attempt labels cannot register that source. Lost storage replies
/// leave the actor's actual gate and raw outcome retained and continuation blocked.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_physical_archive_requires_real_writer_and_root_join() {
    let mut fixture = Fixture::new();
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(fixture.root.join("archive")));
    fixture.service.persistence.enable_transcript_adapter();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let source = checked(&actor, &producer).await;
    let lookalike = NativeFilesystemProducer::new(attempt.clone());
    assert!(lookalike.register_archive_worker(&source).is_err());
    assert!(
        actor
            .handle
            .observe_native_filesystem_archive_storage(source.clone())
            .await
            .unwrap()
            .is_none()
    );
    let receipt = producer.register_archive_worker(&source).unwrap().unwrap();
    assert!(receipt.observe().await.unwrap().is_none());
    assert!(!receipt.observed().unwrap().unwrap());
    persist(&actor).await;
    let settled = seal_joined_native_filesystem_archive(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert!(receipt.observed().unwrap().unwrap());
    assert!(
        actor
            .handle
            .validate_native_filesystem_archive_settlement(settled.clone())
            .await
            .unwrap()
    );
    let mut lost = Box::pin(
        actor
            .handle
            .observe_native_filesystem_archive_storage(source),
    );
    assert!(futures_util::poll!(lost.as_mut()).is_pending());
    drop(lost);
    let mut fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .native_filesystem_archive_settlement_is_current(&settled)
    );
    assert!(Arc::ptr_eq(
        &producer.observe().await.unwrap().unwrap(),
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Original no-save policy has an explicit storage destination and never starts
/// a fake synchronization worker, while original raw/group custody remains held.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_physical_archive_no_save_has_no_worker() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let settled = seal_joined_native_filesystem_archive(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert!(producer.archive_worker().unwrap().is_none());
    let mut fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .native_filesystem_archive_settlement_is_current(&settled)
    );
    assert!(
        fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .is_some()
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Corrupting the real original RAW prefix after ordinary persistence fails the
/// genuine verifier. Repeated observers retain the first failure and original
/// raw effect instead of minting storage acceptance or replaying the mutation.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_physical_archive_corruption_preserves_first_error() {
    let mut fixture = Fixture::new();
    let store = AgentTranscriptStore::new(fixture.root.join("archive"));
    let path = store
        .transcript_path(&fixture.turn.conversation_id)
        .unwrap();
    fixture.service.set_agent_transcript_store(store.clone());
    fixture.service.persistence.enable_transcript_adapter();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let source = checked(&actor, &producer).await;
    let receipt = producer.register_archive_worker(&source).unwrap().unwrap();
    persist(&actor).await;
    fs::write(path, b"corrupt original prefix\n").unwrap();
    let first = seal_joined_native_filesystem_archive(&actor.handle, &producer)
        .await
        .unwrap_err();
    assert!(receipt.observed().unwrap().is_err());
    let second = seal_joined_native_filesystem_archive(&actor.handle, &producer)
        .await
        .unwrap_err();
    assert_eq!(first.message(), second.message());
    let fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .is_some()
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Cancellation of the bridge observer cannot detach the actual physical worker
/// from root/actor ownership. A replacement observer collects the same join and
/// can accept only its original seal, without another descriptor mutation.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_physical_archive_cancelled_bridge_retains_join() {
    let mut fixture = Fixture::new();
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(fixture.root.join("archive")));
    fixture.service.persistence.enable_transcript_adapter();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let source = checked(&actor, &producer).await;
    let receipt = producer.register_archive_worker(&source).unwrap().unwrap();
    persist(&actor).await;
    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    receipt.block_for_tests(entered.clone(), release.clone());
    let observer = tokio::spawn({
        let handle = actor.handle.clone();
        let producer = producer.clone();
        async move { seal_joined_native_filesystem_archive(&handle, &producer).await }
    });
    tokio::task::spawn_blocking(move || entered.wait())
        .await
        .unwrap();
    observer.abort();
    let cancelled = observer.await;
    let replaceable = receipt.close_for_replacement();
    release.wait();
    assert!(cancelled.unwrap_err().is_cancelled());
    assert!(!replaceable.unwrap());
    let settled = seal_joined_native_filesystem_archive(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(producer.archive_worker().unwrap().unwrap().1, receipt);
    assert!(receipt.close_for_replacement().unwrap());
    let mut fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .native_filesystem_archive_settlement_is_current(&settled)
    );
    assert!(
        fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .is_some()
    );
}
