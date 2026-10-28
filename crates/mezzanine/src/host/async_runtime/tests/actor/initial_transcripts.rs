//! Initial fork admission crosses real actor and persistence-worker boundaries.
//!
//! The projection-only fixture starts no pane process or provider. A controlled
//! native receipt-enumeration gate blocks only the storage worker, while actor
//! requests remain live and ordered persistence stays held. Every ACK is backed
//! by real epoch/row publication and receipt retirement, including an empty tail.

use super::super::*;
use crate::runtime::PersistenceEvent;
use crate::storage::transcript::{AgentCompactionEpoch, TranscriptAppendBatch};
use crate::test_support::runtime::SessionFixture;

/// Constructs the same real service-owned metadata claim for both tests, with
/// an actual bound pane but no process, provider or effect service started.
fn initial_owner_service(
    store: &AgentTranscriptStore,
    batch: &TranscriptAppendBatch,
) -> RuntimeSessionService {
    let mut service = RuntimeSessionService::for_agent_presentation_projection(
        SessionFixture::new().build(),
        store.root().join("unused.sock"),
        1,
        crate::runtime::RuntimePresentationSettings::default(),
        1_000,
        100,
    )
    .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", batch.conversation_id(), 0)
        .unwrap();
    service.queue_transcript_for_tests(RuntimeSideEffect::PersistInitialTranscript {
        store: store.clone(),
        path: store.transcript_path(batch.conversation_id()).unwrap(),
        batch: batch.clone(),
    });
    service
}

/// Metadata-only initial ownership must survive service handoff, admission and
/// exact worker settlement. Its sidecar and empty archive are ready before the
/// worker is released, and a late duplicate generation cannot settle twice.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_initial_transcript_admission_publishes_empty_owner_before_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-initial-actor-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let epoch = AgentCompactionEpoch {
        version: 1,
        conversation_id: "initial-child".into(),
        through_sequence: 0,
        summary: "INITIAL_OWNER_SUMMARY".into(),
        ranges: vec![],
    };
    let batch = TranscriptAppendBatch::initial_projection(epoch.clone(), vec![]).unwrap();
    let path = store.transcript_path(batch.conversation_id()).unwrap();
    let service = initial_owner_service(&store, &batch);
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    // The enumeration barrier runs after opening an existing receipt directory.
    std::fs::create_dir_all(root.join(".append-receipts")).unwrap();
    store.gate_next_receipt_enumeration_for_tests(observed_tx, release_rx);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let sentinel = root.join("after-initial.jsonl");
    let client = async {
        tokio::task::spawn_blocking(move || observed_rx.recv_timeout(Duration::from_secs(5)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::Persist {
                    target: PersistenceTarget::AuditLog,
                    path: sentinel.clone(),
                    bytes: b"after initial\n".to_vec(),
                    mode: PersistenceWriteMode::Append,
                }])
                .await
                .unwrap(),
            1
        );
        assert!(handle.drain_persistence_claims(8).await.unwrap().is_empty());
        assert!(!path.exists());
        let mut watcher = handle.side_effect_delivery_watcher();
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store.pending_append_receipts().unwrap() == vec![Vec::new()] {
                    break;
                }
                watcher.changed().await.unwrap();
            }
        })
        .await
        .expect("initial admission must retain its metadata-only receipt");
        // Prove publication readiness before invoking retry-capable workers.
        assert_eq!(
            store.compaction_epoch(batch.conversation_id()).unwrap(),
            Some(epoch.clone())
        );
        assert!(store.inspect(batch.conversation_id()).unwrap().is_empty());
        let receipt_path = std::fs::read_dir(root.join(".append-receipts"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(receipt_path).unwrap()).unwrap();
        let claim_id = receipt["generation"].as_u64().unwrap();
        let report = tokio::time::timeout(
            Duration::from_secs(5),
            run_async_persistence_side_effect_service(
                &handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 10_000,
                    drain_limit: 1,
                    idle_interval: Duration::from_millis(1),
                },
                |_, _| sentinel.exists(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.completed, 2);
        assert_eq!(report.failed, 0);
        assert!(store.pending_append_receipts().unwrap().is_empty());
        let mut late = RuntimeEventBatch::new();
        late.push(RuntimeEvent::Persistence(
            PersistenceEvent::TranscriptClaim {
                claim_id,
                outcome: Box::new(PersistenceEvent::InitialTranscriptCompleted {
                    batch: batch.clone(),
                    path: path.clone(),
                }),
            },
        ));
        assert_eq!(handle.submit_runtime_events(late).await.unwrap().applied, 0);
        assert_eq!(
            store.compaction_epoch(batch.conversation_id()).unwrap(),
            Some(epoch.clone())
        );
        assert!(handle.drain_persistence_claims(8).await.unwrap().is_empty());
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pane_processes().is_empty());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    drop(exit);
    drop(handle);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

/// A changed durable epoch is an immutable authority conflict, not a retryable
/// transport failure. The worker must retain the receipt and actor startup
/// fence without hot-loop replay or overwriting the independently changed epoch.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_initial_transcript_conflict_keeps_owner_without_retry_loop() {
    let root = std::env::temp_dir().join(format!(
        "mez-initial-conflict-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let epoch = AgentCompactionEpoch {
        version: 1,
        conversation_id: "initial-child".into(),
        through_sequence: 0,
        summary: "INITIAL_OWNER_SUMMARY".into(),
        ranges: vec![],
    };
    let batch = TranscriptAppendBatch::initial_projection(epoch, vec![]).unwrap();
    let service = initial_owner_service(&store, &batch);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        let mut watcher = handle.side_effect_delivery_watcher();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store.pending_append_receipts().unwrap() == vec![Vec::new()] {
                    break;
                }
                watcher.changed().await.unwrap();
            }
        })
        .await
        .expect("initial publication must be admitted before mutation");
        // Claim without executing a worker, then let the sole replacement
        // service recover that known-idle claim. This makes first-poll work
        // readiness explicit rather than depending on admission message timing.
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                let claims = handle.drain_persistence_claims(1).await.unwrap();
                if !claims.is_empty() {
                    assert!(matches!(&claims[..],[(RuntimeSideEffect::PersistInitialTranscript {batch: owned,..},Some(_))] if owned == &batch));
                    break;
                }
                watcher.changed().await.unwrap();
            }
        }).await.expect("initial claim must leave its admission hold");
        let mut untagged = RuntimeEventBatch::new();
        untagged.push(RuntimeEvent::Persistence(
            PersistenceEvent::InitialTranscriptCompleted {
                batch: batch.clone(),
                path: store.transcript_path(batch.conversation_id()).unwrap(),
            },
        ));
        assert_eq!(
            handle
                .submit_runtime_events(untagged)
                .await
                .unwrap()
                .applied,
            0
        );
        store
            .save_compaction_epoch(batch.conversation_id(), 0, "INDEPENDENT_NEWER_AUTHORITY")
            .unwrap();
        let report = tokio::time::timeout(
            Duration::from_secs(5),
            run_async_persistence_side_effect_service(
                &handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 10_000,
                    drain_limit: 1,
                    idle_interval: Duration::from_millis(1),
                },
                |polls, _| polls >= 1,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.completed, 0);
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 0);
        assert!(handle.drain_persistence_claims(8).await.unwrap().is_empty());
        assert_eq!(
            store
                .compaction_epoch(batch.conversation_id())
                .unwrap()
                .unwrap()
                .summary,
            "INDEPENDENT_NEWER_AUTHORITY"
        );
        assert_eq!(store.pending_append_receipts().unwrap(), vec![Vec::new()]);
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.service.check_agent_history_admission("%1").is_err());
    assert!(exit.service.pane_processes().is_empty());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    drop(exit);
    drop(handle);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
