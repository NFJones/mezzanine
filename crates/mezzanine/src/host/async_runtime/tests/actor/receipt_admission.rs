//! Native-free receipt admission race coverage through the actor persistence FIFO.
//!
//! Only the in-memory session/actor and real temporary-file persistence worker
//! run here. The isolated projection constructor disables editor/environment
//! discovery and global terminal policy; no pane process, provider, external
//! action, socket listener or other effect service is started. Every channel
//! wait is bounded and all workers finish before temporary-store cleanup.

use super::super::*;
use crate::test_support::runtime::SessionFixture;
use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

/// Builds one valid immutable receipt row without provider or live pane state.
fn row(conversation_id: &str) -> TranscriptEntry {
    TranscriptEntry {
        conversation_id: conversation_id.into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content: "exact accepted row".into(),
    }
}

/// A service-owned A append holds the global FIFO while its receipt is being
/// admitted. Force B's independent durable receipt to retire after A enumerates
/// it; an unrelated sentinel queued behind A must stay fenced before release,
/// then the real worker must commit A once and drain that sentinel in order.
/// No explicit failed-admission recovery, FIFO bypass or effect replay is used.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_receipt_retirement_does_not_stall_unrelated_persistence() {
    let root = std::env::temp_dir().join(format!(
        "mez-receipt-admission-fifo-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let a = row("conversation-a");
    let b = row("conversation-b");
    store
        .accept_append_receipt(std::slice::from_ref(&b), 1)
        .unwrap();
    store.append(&b).unwrap();
    let mut service = RuntimeSessionService::for_agent_presentation_projection(
        SessionFixture::new().build(),
        root.join("unused.sock"),
        1,
        crate::runtime::RuntimePresentationSettings::default(),
        1_000,
        100,
    )
    .unwrap();
    assert!(service.pane_processes().is_empty());
    assert!(service.pending_agent_provider_tasks().is_empty());
    service.queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
        path: store.transcript_path(&a.conversation_id).unwrap(),
        store: store.clone(),
        entries: vec![a.clone()],
    });
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    store.gate_next_receipt_enumeration_for_tests(observed_tx, resume_rx);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let sentinel = root.join("unrelated-audit.jsonl");
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
                    bytes: b"unrelated persistence\n".to_vec(),
                    mode: PersistenceWriteMode::Append,
                }])
                .await
                .unwrap(),
            1
        );
        assert!(handle.drain_persistence_claims(8).await.unwrap().is_empty());
        assert!(!sentinel.exists());
        assert!(!store.transcript_path(&a.conversation_id).unwrap().exists());
        tokio::task::spawn_blocking({
            let root = root.clone();
            let b = b.clone();
            move || AgentTranscriptStore::new(root).settle_append_receipt(&[b], 1)
        })
        .await
        .unwrap()
        .unwrap();
        let mut receipt_watcher = handle.side_effect_delivery_watcher();
        resume_tx.send(()).unwrap();

        // Worker startup can retry failed service-owned admission. Prove the
        // original admission created A's exact receipt before invoking any
        // retry-capable worker, otherwise historical ENOENT could self-repair.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store.pending_append_receipts().unwrap() == vec![vec![a.clone()]] {
                    break;
                }
                receipt_watcher.changed().await.unwrap();
            }
        })
        .await
        .expect("original A receipt admission must succeed without recovery");

        let mut observed_a_before_sentinel = false;
        let report = tokio::time::timeout(
            Duration::from_secs(5),
            run_async_persistence_side_effect_service(
                &handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 10_000,
                    drain_limit: 1,
                    idle_interval: Duration::from_millis(1),
                },
                |_, _| {
                    if sentinel.exists() {
                        true
                    } else {
                        observed_a_before_sentinel |= store.inspect(&a.conversation_id).is_ok();
                        false
                    }
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            observed_a_before_sentinel,
            "A must commit before the later sentinel"
        );
        assert_eq!(report.drained, 2);
        assert_eq!(report.completed, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"unrelated persistence\n"
        );
        assert_eq!(store.inspect(&a.conversation_id).unwrap(), vec![a.clone()]);
        assert_eq!(store.inspect(&b.conversation_id).unwrap(), vec![b.clone()]);
        assert!(store.pending_append_receipts().unwrap().is_empty());
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
