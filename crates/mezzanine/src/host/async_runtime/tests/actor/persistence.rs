//! Async-runtime tests owned by persistence behavior.

use super::super::*;
use crate::host::async_runtime::AsyncRuntimeSessionHandle;
use crate::runtime::RuntimeRegistryUpdatePlan;
use crate::security::project::{ProjectTrustStore, TrustDecision};

/// Waits for the off-actor startup receipt callback rather than assuming the
/// constructor's queued transcript has already passed its durable gate.
async fn wait_for_startup_transcript_receipts(
    handle: &AsyncRuntimeSessionHandle,
    store: &AgentTranscriptStore,
    count: usize,
) {
    let mut watcher = handle.side_effect_delivery_watcher();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if store.pending_append_receipts().unwrap().len() == count {
                break;
            }
            watcher.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

/// Startup holds service-owned transcript work until its receipt has synced;
/// a replacement store can recover it without a persistence worker drain.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_construction_journals_before_worker_drain() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-actor-initial-receipt-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "initial-receipt".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted before actor".to_string(),
    };
    let mut service = test_service_with_event_log();
    service.queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
        path: store.transcript_path(&row.conversation_id).unwrap(),
        store: store.clone(),
        entries: vec![row.clone()],
    });
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    assert!(store.pending_append_receipts().unwrap().is_empty());
    let client = async {
        wait_for_startup_transcript_receipts(&handle, &store, 1).await;
        assert_eq!(
            store.pending_append_receipts().unwrap(),
            vec![vec![row.clone()]]
        );
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    AgentTranscriptStore::new(root.clone())
        .recover_append_receipts()
        .unwrap();
    assert_eq!(store.inspect(&row.conversation_id).unwrap(), vec![row]);
    let _ = std::fs::remove_dir_all(root);
}

/// Direct actor admission must journal the accepted append before replying,
/// even when the persistence worker has not drained it.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_transcript_admission_journals_before_reply() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-actor-preworker-receipt-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "preworker-test".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted before worker".to_string(),
    };
    let path = store.transcript_path(&row.conversation_id).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();
    let client = async {
        assert_eq!(
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                    store: store.clone(),
                    path,
                    entries: vec![row.clone()],
                }])
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store.pending_append_receipts().unwrap(),
            vec![vec![row.clone()]]
        );
        assert!(
            !store
                .transcript_path(&row.conversation_id)
                .unwrap()
                .exists()
        );
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// Cancelling a caller after actor enqueue must not cancel the actor-owned
/// receipt sync or strand its held persistence claim.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_cancelled_transcript_producer_does_not_strand_claim() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-actor-cancelled-receipt-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "cancelled-receipt".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted".to_string(),
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();
    let client = async {
        let mut watcher = handle.side_effect_delivery_watcher();
        let producer_handle = handle.clone();
        let producer_store = store.clone();
        let producer_row = row.clone();
        let cancelled = tokio::spawn(async move {
            producer_handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                    path: producer_store
                        .transcript_path(&producer_row.conversation_id)
                        .unwrap(),
                    store: producer_store,
                    entries: vec![producer_row],
                }])
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), watcher.changed())
            .await
            .unwrap()
            .unwrap();
        cancelled.abort();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store.pending_append_receipts().unwrap() == vec![vec![row.clone()]] {
                    break;
                }
                watcher.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(handle.drain_persistence_claims(1).await.unwrap().len(), 1);
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// A terminal step that drains service-owned transcript work must not reply
/// before the exact append receipt is durable.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_terminal_step_journals_service_transcript_before_reply() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-terminal-step-receipt-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "terminal-step-receipt".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted through terminal step".to_string(),
    };
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, mut actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    actor
        .service
        .queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
            path: store.transcript_path(&row.conversation_id).unwrap(),
            store: store.clone(),
            entries: vec![row.clone()],
        });
    let client = async {
        handle
            .apply_attached_terminal_step_plan(
                primary,
                AttachedTerminalClientStepPlan {
                    actions: Vec::new(),
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            store.pending_append_receipts().unwrap(),
            vec![vec![row.clone()]]
        );
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// Event ingress must not acknowledge service-owned transcript rows until
/// their exact receipt is durable, even without a persistence worker drain.
/// A later non-transcript control frame cannot acknowledge an earlier frame
/// whose service-owned append receipt was rejected.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn async_actor_multiframe_control_rejects_earlier_failed_receipt() {
    use crate::control::encode_control_body;
    use crate::storage::snapshot::SnapshotRepository;
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!(
        "mez-multiframe-receipt-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let outside = root.with_extension("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.join(".append-receipts")).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "multiframe-receipt".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "must not acknowledge".to_string(),
    };
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, mut actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    actor
        .service
        .queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
            path: store.transcript_path(&row.conversation_id).unwrap(),
            store: store.clone(),
            entries: vec![row],
        });
    let client = async {
        let mut input = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"first","method":"session/get","params":{}}"#,
        );
        input.extend_from_slice(&encode_control_body(
            r#"{"jsonrpc":"2.0","id":"last","method":"snapshot/list","params":{}}"#,
        ));
        assert!(
            handle
                .handle_control_input_for_connection_with_snapshots(
                    input,
                    4096,
                    ControlConnectionState::trusted_existing_client(primary),
                    SnapshotRepository::new(root.join("snapshots")),
                )
                .await
                .is_err()
        );
        assert!(handle.drain_persistence_claims(1).await.unwrap().is_empty());
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

/// Receipt completion must not await a full interactive lane while holding
/// the actor: urgent shutdown remains serviceable until continuation admission.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_receipt_continuation_does_not_block_urgent_shutdown() {
    use crate::control::encode_control_body;
    use crate::host::async_runtime::actor_types::{
        AsyncRuntimeRequest, AsyncRuntimeRequestEnvelope, TranscriptReceiptReply,
    };
    use crate::storage::snapshot::SnapshotRepository;

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .config(AsyncRuntimeActorConfig {
            command_buffer: 4,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
        .unwrap();
    let sender = actor.sender.clone();
    let interactive_permit = sender
        .interactive_admission
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let mut receipt_completion = handle.side_effect_delivery_watcher();
    receipt_completion.borrow_and_update();
    let (reply, _response) = tokio::sync::oneshot::channel();
    let continuation = AsyncRuntimeRequest::HandleControlInputWithSnapshots {
        input: encode_control_body(
            r#"{"jsonrpc":"2.0","id":"next","method":"session/get","params":{}}"#,
        ),
        output_prefix: Vec::new(),
        consumed_prefix: 0,
        record_metrics: false,
        max_content_length: 4096,
        connection: ControlConnectionState::new(true, true),
        snapshots: SnapshotRepository::new(std::env::temp_dir().join("mez-full-lane-continuation")),
        reply,
    };
    sender
        .send(AsyncRuntimeRequestEnvelope::new(
            AsyncRuntimeRequest::CompleteTranscriptReceipts {
                results: Vec::new(),
                reply: TranscriptReceiptReply::ControlContinuation(Box::new(continuation)),
            },
        ))
        .await
        .unwrap();
    let client = async {
        // Receipt completion publishes this revision before dispatching the
        // continuation. Keep the interactive permit held across shutdown.
        tokio::time::timeout(Duration::from_secs(5), receipt_completion.changed())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), handle.shutdown())
            .await
            .unwrap()
            .unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    drop(interactive_permit);
}

/// Event ingress must not acknowledge service-owned transcript rows until
/// their exact receipt is durable, even without a persistence worker drain.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_event_admission_journals_service_transcript_before_reply() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-actor-event-receipt-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "event-receipt".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted through event".to_string(),
    };
    let (handle, mut actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();
    actor
        .service
        .queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
            path: store.transcript_path(&row.conversation_id).unwrap(),
            store: store.clone(),
            entries: vec![row.clone()],
        });
    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptFailed {
                conversation_id: row.conversation_id.clone(),
                first_sequence: row.sequence,
                entries: vec![row.clone()],
                path: store.transcript_path(&row.conversation_id).unwrap(),
                error: "unclaimed".to_string(),
                retryable: true,
            },
        ));
        handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(
            store.pending_append_receipts().unwrap(),
            vec![vec![row.clone()]]
        );
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// Consecutive accepted batches cannot overtake one another while receipt
/// writes run off-actor; both replies require durable recovery evidence.
/// A rejected startup receipt retains the service-owned sequence and can be
/// resynced when the persistence worker explicitly recovers after repair.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn async_actor_recovers_service_receipt_after_admission_failure() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!(
        "mez-service-receipt-retry-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let outside = root.with_extension("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.join(".append-receipts")).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "service-receipt-retry".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "retained after failed admission".to_string(),
    };
    let mut service = test_service_with_event_log();
    service.queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
        path: store.transcript_path(&row.conversation_id).unwrap(),
        store: store.clone(),
        entries: vec![row.clone()],
    });
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        let mut watcher = handle.side_effect_delivery_watcher();
        watcher.borrow_and_update();
        tokio::time::timeout(Duration::from_secs(5), watcher.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(handle.drain_persistence_claims(1).await.unwrap().is_empty());
        std::fs::remove_file(root.join(".append-receipts")).unwrap();
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 1);
        assert_eq!(
            store.pending_append_receipts().unwrap(),
            vec![vec![row.clone()]]
        );
        assert_eq!(handle.drain_persistence_claims(1).await.unwrap().len(), 1);
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    AgentTranscriptStore::new(root.clone())
        .recover_append_receipts()
        .unwrap();
    assert_eq!(store.inspect(&row.conversation_id).unwrap(), vec![row]);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

/// Consecutive accepted batches cannot overtake one another while receipt
/// writes run off-actor; both replies require durable recovery evidence.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_journals_consecutive_transcript_admissions_in_order() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-actor-ordered-receipts-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let rows = (1..=2)
        .map(|sequence| TranscriptEntry {
            conversation_id: "ordered-receipts".to_string(),
            sequence,
            created_at_unix_seconds: sequence,
            role: TranscriptRole::User,
            turn_id: format!("turn-{sequence}"),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!("row {sequence}"),
        })
        .collect::<Vec<_>>();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();
    let client = async {
        let first =
            handle.queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                path: store.transcript_path("ordered-receipts").unwrap(),
                store: store.clone(),
                entries: vec![rows[0].clone()],
            }]);
        let second =
            handle.queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                path: store.transcript_path("ordered-receipts").unwrap(),
                store: store.clone(),
                entries: vec![rows[1].clone()],
            }]);
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.unwrap(), 1);
        assert_eq!(second.unwrap(), 1);
        assert_eq!(
            store.pending_append_receipts().unwrap(),
            vec![vec![rows[0].clone()], vec![rows[1].clone()]]
        );
        assert!(!store.transcript_path("ordered-receipts").unwrap().exists());
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// A replacement worker must recover the exact claimed batch ahead of later
/// queued writes, then retire it only after its matching completion.
/// A rejected receipt directory must fail the producer reply and leave no
/// transcript effect for the persistence worker to execute.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn async_actor_rejects_unjournaled_transcript_admission() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!(
        "mez-actor-rejected-receipt-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let outside = root.with_extension("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.join(".append-receipts")).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "rejected-receipt".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "do not append".to_string(),
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();
    let client = async {
        assert!(
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                    path: store.transcript_path(&row.conversation_id).unwrap(),
                    store: store.clone(),
                    entries: vec![row.clone()],
                }])
                .await
                .is_err()
        );
        assert!(handle.drain_persistence_claims(1).await.unwrap().is_empty());
        assert!(
            !store
                .transcript_path(&row.conversation_id)
                .unwrap()
                .exists()
        );
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

/// An invalid later receipt rejects the producer submission without losing
/// the earlier durable receipt or preventing its exact worker claim.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_mixed_receipt_failure_preserves_accepted_prefix() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-actor-mixed-receipts-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let rows = (1..=2)
        .map(|sequence| TranscriptEntry {
            conversation_id: "mixed-receipts".to_string(),
            sequence,
            created_at_unix_seconds: sequence,
            role: TranscriptRole::User,
            turn_id: format!("turn-{sequence}"),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!("row {sequence}"),
        })
        .collect::<Vec<_>>();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();
    let client = async {
        assert!(
            handle
                .queue_runtime_side_effects(vec![
                    RuntimeSideEffect::PersistTranscriptEntries {
                        path: store.transcript_path("mixed-receipts").unwrap(),
                        store: store.clone(),
                        entries: vec![rows[0].clone()],
                    },
                    RuntimeSideEffect::PersistTranscriptEntries {
                        path: store.transcript_path("mixed-receipts").unwrap(),
                        store: store.clone(),
                        entries: vec![rows[1].clone(), rows[0].clone()],
                    },
                ])
                .await
                .is_err()
        );
        assert_eq!(
            store.pending_append_receipts().unwrap(),
            vec![vec![rows[0].clone()]]
        );
        let claims = handle.drain_persistence_claims(2).await.unwrap();
        assert_eq!(claims.len(), 1);
        assert!(
            matches!(&claims[0].0, RuntimeSideEffect::PersistTranscriptEntries { entries, .. } if entries == &vec![rows[0].clone()])
        );
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    AgentTranscriptStore::new(root.clone())
        .recover_append_receipts()
        .unwrap();
    assert_eq!(
        store.inspect("mixed-receipts").unwrap(),
        vec![rows[0].clone()]
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A replacement worker must recover the exact claimed batch ahead of later
/// queued writes, then retire it only after its matching completion.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_recovers_unacknowledged_transcript_before_later_work() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-async-transcript-restart-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let path = store.transcript_path("restart-test").unwrap();
    let row = TranscriptEntry {
        conversation_id: "restart-test".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted".to_string(),
    };
    let mut service = test_service_with_event_log();
    service.queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
        store: store.clone(),
        path: path.clone(),
        entries: vec![row.clone()],
    });
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        wait_for_startup_transcript_receipts(&handle, &store, 1).await;
        let mut unclaimed_failure = RuntimeEventBatch::new();
        unclaimed_failure.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptFailed {
                conversation_id: row.conversation_id.clone(),
                first_sequence: row.sequence,
                entries: vec![row.clone()],
                path: path.clone(),
                error: "unclaimed failure".to_string(),
                retryable: true,
            },
        ));
        assert_eq!(
            handle
                .submit_runtime_events(unclaimed_failure)
                .await
                .unwrap()
                .applied,
            0
        );
        let mut unclaimed_permanent_failure = RuntimeEventBatch::new();
        unclaimed_permanent_failure.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptFailed {
                conversation_id: row.conversation_id.clone(),
                first_sequence: row.sequence,
                entries: vec![row.clone()],
                path: path.clone(),
                error: "unclaimed permanent failure".to_string(),
                retryable: false,
            },
        ));
        assert_eq!(
            handle
                .submit_runtime_events(unclaimed_permanent_failure)
                .await
                .unwrap()
                .applied,
            0
        );
        let mut queued_only = RuntimeEventBatch::new();
        queued_only.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptCompleted {
                conversation_id: row.conversation_id.clone(),
                first_sequence: row.sequence,
                entries: vec![row.clone()],
                path: path.clone(),
                bytes: 1,
            },
        ));
        assert_eq!(
            handle
                .submit_runtime_events(queued_only)
                .await
                .unwrap()
                .applied,
            0
        );
        let claimed = handle.drain_persistence_side_effects(1).await.unwrap();
        assert_eq!(claimed.len(), 1);
        let mut wrong_path = RuntimeEventBatch::new();
        wrong_path.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptCompleted {
                conversation_id: row.conversation_id.clone(),
                first_sequence: row.sequence,
                entries: vec![row.clone()],
                path: path.with_extension("wrong"),
                bytes: 1,
            },
        ));
        assert_eq!(
            handle
                .submit_runtime_events(wrong_path)
                .await
                .unwrap()
                .applied,
            0
        );
        let mut wrong_path_failure = RuntimeEventBatch::new();
        wrong_path_failure.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptFailed {
                conversation_id: row.conversation_id.clone(),
                first_sequence: row.sequence,
                entries: vec![row.clone()],
                path: path.with_extension("wrong"),
                error: "foreign destination".to_string(),
                retryable: false,
            },
        ));
        assert_eq!(
            handle
                .submit_runtime_events(wrong_path_failure)
                .await
                .unwrap()
                .applied,
            0
        );
        store.append_many(std::slice::from_ref(&row)).unwrap();
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                store: store.clone(),
                path: path.clone(),
                entries: vec![TranscriptEntry {
                    sequence: 2,
                    ..row.clone()
                }],
            }])
            .await
            .unwrap();
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 1);
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 0);
        let replay = handle.drain_persistence_side_effects(2).await.unwrap();
        assert_eq!(replay.len(), 2);
        assert!(
            matches!(&replay[0], RuntimeSideEffect::PersistTranscriptEntries { entries, .. } if entries == &vec![row.clone()])
        );
        assert!(
            matches!(&replay[1], RuntimeSideEffect::PersistTranscriptEntries { entries, .. } if entries[0].sequence == 2)
        );
        store.append_many(std::slice::from_ref(&row)).unwrap();
        let mut malformed = RuntimeEventBatch::new();
        malformed.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptCompleted {
                conversation_id: row.conversation_id.clone(),
                first_sequence: 2,
                entries: vec![row.clone()],
                path: path.clone(),
                bytes: 0,
            },
        ));
        handle.submit_runtime_events(malformed).await.unwrap();
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 2);
        let mut events = RuntimeEventBatch::new();
        events.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptCompleted {
                conversation_id: row.conversation_id.clone(),
                first_sequence: 1,
                entries: vec![row.clone()],
                path,
                bytes: 0,
            },
        ));
        handle.submit_runtime_events(events).await.unwrap();
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 0);
        let remaining = handle.drain_persistence_side_effects(2).await.unwrap();
        assert!(
            matches!(remaining.as_slice(), [RuntimeSideEffect::PersistTranscriptEntries { entries, .. }] if entries[0].sequence == 2)
        );
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// A recovered claim retains its own queue position ahead of a later identical
/// append; settling the old claim cannot discard the new batch.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_recovered_claim_preserves_later_identical_append() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-recovered-identical-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "recovered-identical".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted".to_string(),
    };
    let path = store.transcript_path(&row.conversation_id).unwrap();
    let mut service = test_service_with_event_log();
    service.queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
        store: store.clone(),
        path: path.clone(),
        entries: vec![row.clone()],
    });
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        wait_for_startup_transcript_receipts(&handle, &store, 1).await;
        assert_eq!(
            handle
                .drain_persistence_side_effects(1)
                .await
                .unwrap()
                .len(),
            1
        );
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                store: store.clone(),
                path: path.clone(),
                entries: vec![row.clone()],
            }])
            .await
            .unwrap();
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 1);
        let replay = handle.drain_persistence_side_effects(1).await.unwrap();
        assert!(
            matches!(replay.as_slice(), [RuntimeSideEffect::PersistTranscriptEntries { entries, .. }] if entries == &vec![row.clone()])
        );
        store.append_many(std::slice::from_ref(&row)).unwrap();
        let mut events = RuntimeEventBatch::new();
        events.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptCompleted {
                conversation_id: row.conversation_id.clone(),
                first_sequence: row.sequence,
                entries: vec![row.clone()],
                path,
                bytes: 0,
            },
        ));
        assert_eq!(
            handle.submit_runtime_events(events).await.unwrap().applied,
            1
        );
        let remaining = handle.drain_persistence_side_effects(1).await.unwrap();
        assert!(
            matches!(remaining.as_slice(), [RuntimeSideEffect::PersistTranscriptEntries { entries, .. }] if entries == &vec![row.clone()])
        );
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// Settling a claimed batch must leave a later identical unclaimed batch in
/// the ordered persistence route.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_completion_preserves_queued_identical_transcript() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-transcript-queued-identical-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "queued-identical".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted".to_string(),
    };
    let path = store.transcript_path(&row.conversation_id).unwrap();
    let mut service = test_service_with_event_log();
    service.queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
        store: store.clone(),
        path: path.clone(),
        entries: vec![row.clone()],
    });
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        wait_for_startup_transcript_receipts(&handle, &store, 1).await;
        assert_eq!(
            handle
                .drain_persistence_side_effects(1)
                .await
                .unwrap()
                .len(),
            1
        );
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                store: store.clone(),
                path: path.clone(),
                entries: vec![row.clone()],
            }])
            .await
            .unwrap();
        store.append_many(std::slice::from_ref(&row)).unwrap();
        let mut events = RuntimeEventBatch::new();
        events.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptCompleted {
                conversation_id: row.conversation_id.clone(),
                first_sequence: row.sequence,
                entries: vec![row.clone()],
                path,
                bytes: 0,
            },
        ));
        assert_eq!(
            handle.submit_runtime_events(events).await.unwrap().applied,
            1
        );
        let remaining = handle.drain_persistence_side_effects(1).await.unwrap();
        assert!(
            matches!(remaining.as_slice(), [RuntimeSideEffect::PersistTranscriptEntries { entries, .. }] if entries == &vec![row.clone()])
        );
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 1);
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// Two fresh identical claims each need a replacement-worker replay, while a
/// second recovery request must not add another copy of either claim.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_recovers_two_fresh_identical_transcript_claims() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-two-identical-claims-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "two-identical-claims".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted".to_string(),
    };
    let effect = RuntimeSideEffect::PersistTranscriptEntries {
        path: store.transcript_path(&row.conversation_id).unwrap(),
        store: store.clone(),
        entries: vec![row.clone()],
    };
    let mut service = test_service_with_event_log();
    service.queue_transcript_for_tests(effect.clone());
    service.queue_transcript_for_tests(effect.clone());
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        wait_for_startup_transcript_receipts(&handle, &store, 2).await;
        let claimed = handle.drain_persistence_claims(2).await.unwrap();
        assert_eq!(claimed.len(), 2);
        let first_id = claimed[0].1.unwrap();
        let second_id = claimed[1].1.unwrap();
        assert_ne!(first_id, second_id);
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 2);
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 0);
        store.append_many(std::slice::from_ref(&row)).unwrap();
        let mut events = RuntimeEventBatch::new();
        events.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptClaim {
                claim_id: first_id,
                outcome: Box::new(crate::runtime::PersistenceEvent::TranscriptCompleted {
                    conversation_id: row.conversation_id.clone(),
                    first_sequence: row.sequence,
                    entries: vec![row.clone()],
                    path: store.transcript_path(&row.conversation_id).unwrap(),
                    bytes: 0,
                }),
            },
        ));
        for invalid_id in [0, first_id.wrapping_add(second_id).wrapping_add(1)] {
            let mut invalid = events.clone();
            if let RuntimeEvent::Persistence(crate::runtime::PersistenceEvent::TranscriptClaim {
                claim_id,
                ..
            }) = &mut invalid.events[0]
            {
                *claim_id = invalid_id;
            }
            assert_eq!(
                handle.submit_runtime_events(invalid).await.unwrap().applied,
                0
            );
        }
        let stale_completion = events.clone();
        assert_eq!(
            handle.submit_runtime_events(events).await.unwrap().applied,
            1
        );
        assert_eq!(
            handle
                .submit_runtime_events(stale_completion)
                .await
                .unwrap()
                .applied,
            0,
            "a duplicate outcome must not settle the other identical claim"
        );
        let replay = handle.drain_persistence_claims(2).await.unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].1, Some(second_id));
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 1);
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// A live worker failure must requeue its exact claimed write before later
/// reserved sequences rather than waiting for a worker restart.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_requeues_failed_transcript_before_later_work() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-async-transcript-failed-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let path = store.transcript_path("failure-test").unwrap();
    let first = TranscriptEntry {
        conversation_id: "failure-test".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted".to_string(),
    };
    let second = TranscriptEntry {
        sequence: 2,
        ..first.clone()
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();
    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                store: store.clone(),
                path: path.clone(),
                entries: vec![first.clone()],
            }])
            .await
            .unwrap();
        assert_eq!(
            handle
                .drain_persistence_side_effects(1)
                .await
                .unwrap()
                .len(),
            1
        );
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                store: store.clone(),
                path: path.clone(),
                entries: vec![second.clone()],
            }])
            .await
            .unwrap();
        let mut events = RuntimeEventBatch::new();
        events.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptFailed {
                conversation_id: first.conversation_id.clone(),
                first_sequence: 1,
                entries: vec![first.clone()],
                path: path.clone(),
                error: "transient".to_string(),
                retryable: true,
            },
        ));
        handle.submit_runtime_events(events).await.unwrap();
        let replay = handle.drain_persistence_side_effects(2).await.unwrap();
        assert!(matches!(replay.as_slice(),
            [RuntimeSideEffect::PersistTranscriptEntries { entries: old, .. }, RuntimeSideEffect::PersistTranscriptEntries { entries: new, .. }]
            if old == &vec![first] && new == &vec![second]));
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// A permanent conflict remains visible as an uncertain claim but cannot
/// immediately replay the same incompatible append through the live worker.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_does_not_requeue_permanent_transcript_failure() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-transcript-permanent-failure-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "permanent-failure".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted".to_string(),
    };
    let path = store.transcript_path(&row.conversation_id).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();
    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistTranscriptEntries {
                store,
                path: path.clone(),
                entries: vec![row.clone()],
            }])
            .await
            .unwrap();
        assert_eq!(
            handle
                .drain_persistence_side_effects(1)
                .await
                .unwrap()
                .len(),
            1
        );
        let mut events = RuntimeEventBatch::new();
        events.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptFailed {
                conversation_id: row.conversation_id.clone(),
                first_sequence: 1,
                entries: vec![row],
                path,
                error: "conflicting durable row".to_string(),
                retryable: false,
            },
        ));
        handle.submit_runtime_events(events).await.unwrap();
        assert!(
            handle
                .drain_persistence_side_effects(1)
                .await
                .unwrap()
                .is_empty()
        );
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// Permanently rejecting one of two identical claimed appends must not suppress
/// the other claim's replacement-worker replay.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_permanent_failure_preserves_other_identical_claim() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    let root = std::env::temp_dir().join(format!(
        "mez-identical-permanent-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let row = TranscriptEntry {
        conversation_id: "identical-permanent".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: "accepted".to_string(),
    };
    let path = store.transcript_path(&row.conversation_id).unwrap();
    let effect = RuntimeSideEffect::PersistTranscriptEntries {
        store: store.clone(),
        path: path.clone(),
        entries: vec![row.clone()],
    };
    let mut service = test_service_with_event_log();
    service.queue_transcript_for_tests(effect.clone());
    service.queue_transcript_for_tests(effect);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        wait_for_startup_transcript_receipts(&handle, &store, 2).await;
        let claims = handle.drain_persistence_claims(2).await.unwrap();
        assert_eq!(claims.len(), 2);
        let first_id = claims[0].1.unwrap();
        let second_id = claims[1].1.unwrap();
        assert_ne!(first_id, second_id);
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 2);
        let mut events = RuntimeEventBatch::new();
        events.push(RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptClaim {
                claim_id: first_id,
                outcome: Box::new(crate::runtime::PersistenceEvent::TranscriptFailed {
                    conversation_id: row.conversation_id.clone(),
                    first_sequence: row.sequence,
                    entries: vec![row.clone()],
                    path,
                    error: "permanent conflict".to_string(),
                    retryable: false,
                }),
            },
        ));
        let duplicate_failure = events.clone();
        let mut foreign_failure = events.clone();
        if let RuntimeEvent::Persistence(crate::runtime::PersistenceEvent::TranscriptClaim {
            claim_id,
            ..
        }) = &mut foreign_failure.events[0]
        {
            *claim_id = second_id.wrapping_add(1);
        }
        assert_eq!(
            handle
                .submit_runtime_events(foreign_failure)
                .await
                .unwrap()
                .applied,
            0
        );
        handle.submit_runtime_events(events).await.unwrap();
        assert_eq!(
            handle
                .submit_runtime_events(duplicate_failure)
                .await
                .unwrap()
                .applied,
            0
        );
        assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 0);
        let replay = handle.drain_persistence_claims(2).await.unwrap();
        assert!(
            matches!(replay.as_slice(), [(RuntimeSideEffect::PersistTranscriptEntries { entries, .. }, Some(id))] if entries == &vec![row.clone()] && *id == second_id)
        );
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    let _ = std::fs::remove_dir_all(root);
}

/// Permanent transcript failures stay blocked when another claim is retried,
/// regardless of which failure event arrives first in an actor batch.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_mixed_transcript_failures_do_not_replay_permanent_claim() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

    for permanent_first in [true, false] {
        let root = std::env::temp_dir().join(format!(
            "mez-transcript-mixed-failure-{}-{permanent_first}",
            std::process::id()
        ));
        let store = AgentTranscriptStore::new(root.clone());
        let first = TranscriptEntry {
            conversation_id: "mixed-failure".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "original".to_string(),
        };
        let second = TranscriptEntry {
            sequence: 2,
            ..first.clone()
        };
        let path = store.transcript_path(&first.conversation_id).unwrap();
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
            .build()
            .unwrap();
        let client = async {
            handle
                .queue_runtime_side_effects(
                    [first.clone(), second.clone()]
                        .into_iter()
                        .map(|row| RuntimeSideEffect::PersistTranscriptEntries {
                            store: store.clone(),
                            path: path.clone(),
                            entries: vec![row],
                        })
                        .collect(),
                )
                .await
                .unwrap();
            assert_eq!(
                handle
                    .drain_persistence_side_effects(2)
                    .await
                    .unwrap()
                    .len(),
                2
            );
            let failure = |row: TranscriptEntry, retryable| {
                RuntimeEvent::Persistence(crate::runtime::PersistenceEvent::TranscriptFailed {
                    conversation_id: row.conversation_id.clone(),
                    first_sequence: row.sequence,
                    entries: vec![row],
                    path: path.clone(),
                    error: "test failure".to_string(),
                    retryable,
                })
            };
            let mut events = RuntimeEventBatch::new();
            if permanent_first {
                events.push(failure(first.clone(), false));
                events.push(failure(second.clone(), true));
            } else {
                events.push(failure(second.clone(), true));
                events.push(failure(first.clone(), false));
            }
            handle.submit_runtime_events(events).await.unwrap();
            let replay = handle.drain_persistence_side_effects(4).await.unwrap();
            assert!(
                matches!(replay.as_slice(), [RuntimeSideEffect::PersistTranscriptEntries { entries, .. }] if entries == &vec![second.clone()])
            );
            assert_eq!(handle.recover_claimed_transcripts().await.unwrap(), 1);
            let recovered = handle.drain_persistence_side_effects(4).await.unwrap();
            assert!(
                matches!(recovered.as_slice(), [RuntimeSideEffect::PersistTranscriptEntries { entries, .. }] if entries == &vec![second.clone()])
            );
            handle.shutdown().await.unwrap();
        };
        let ((), _) = tokio::join!(client, actor.run());
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Verifies durable persistence work has independent admission from the
/// bounded transient side-effect queue. A persistence burst must remain
/// drainable rather than causing its already-applied producer to fail.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_admits_persistence_backlog_beyond_transient_queue_capacity() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-backlog-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
        .unwrap();

    let client = async {
        let effects = (0..5)
            .map(|index| RuntimeSideEffect::Persist {
                target: PersistenceTarget::AuditLog,
                path: root.join("audit.jsonl"),
                bytes: format!("{{\"index\":{index}}}\n").into_bytes(),
                mode: PersistenceWriteMode::Append,
            })
            .collect::<Vec<_>>();
        assert_eq!(handle.queue_runtime_side_effects(effects).await.unwrap(), 5);

        let drained = handle.drain_persistence_side_effects(8).await.unwrap();
        assert_eq!(drained.len(), 5);
        assert!(drained.iter().all(|effect| matches!(
            effect,
            RuntimeSideEffect::Persist {
                target: PersistenceTarget::AuditLog,
                ..
            }
        )));
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 5);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 5);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that registry persistence side effects are coalesced before queue
/// capacity is checked. Registry writes describe the latest discoverable
/// session state, so a burst only needs the newest pending update for that
/// session rather than a queue entry per intermediate state.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_coalesces_registry_persistence_before_capacity_check() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-coalesce-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    service.set_session_registry(registry.clone());
    let update = service.registry_update_plan();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
        .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::PersistRegistry {
                    registry: registry.clone(),
                    update: update.clone(),
                },
                RuntimeSideEffect::PersistRegistry {
                    registry: registry.clone(),
                    update: update.clone(),
                },
                RuntimeSideEffect::PersistRegistry { registry, update },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 3);

        let effects = handle.drain_persistence_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1);
        assert!(
            matches!(effects[0], RuntimeSideEffect::PersistRegistry { .. }),
            "{effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 1);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 1);
    assert_eq!(exit.metrics.render_invalidations_coalesced, 2);
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that a registry update arriving after the prior update has entered
/// the routed persistence lane replaces that pending work. This keeps a busy
/// provider stream from filling the shared effect queue with stale snapshots.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_coalesces_routed_registry_persistence_across_enqueue_calls() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-routed-registry-coalesce-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    service.set_session_registry(registry.clone());
    let initial_update = service.registry_update_plan();
    let RuntimeRegistryUpdatePlan::Upsert(record) = &initial_update else {
        panic!("live test service must create a registry upsert");
    };
    let expected_session_id = record.session_id.clone();
    let replacement_update = RuntimeRegistryUpdatePlan::Remove {
        session_id: expected_session_id.clone(),
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 1,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
        .unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistRegistry {
                registry: registry.clone(),
                update: initial_update,
            }])
            .await
            .unwrap();
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::PersistRegistry {
                registry,
                update: replacement_update.clone(),
            }])
            .await
            .unwrap();

        let effects = handle.drain_persistence_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects.as_slice(),
            [RuntimeSideEffect::PersistRegistry {
                update: RuntimeRegistryUpdatePlan::Remove { session_id },
                ..
            }] if session_id == &expected_session_id
        ));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 1);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 1);
    assert_eq!(exit.metrics.render_invalidations_coalesced, 1);
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that raw pane-output bursts do not enqueue registry persistence.
/// Pane output changes the terminal screen and event stream, but it does not
/// change the discoverable session registry record; persisting after every PTY
/// read can overflow the bounded side-effect queue during high-volume output.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_does_not_persist_registry_for_pane_output_bursts() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-output-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service.set_session_registry(registry);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
        .unwrap();

    let client = async {
        for index in 0..128 {
            let mut batch = RuntimeEventBatch::new();
            batch.push(RuntimeEvent::Pane(PaneEvent::Output {
                pane_id: "%1".to_string(),
                bytes: format!("burst-output-{index}\n").into_bytes(),
            }));
            let report = handle.submit_runtime_events(batch).await.unwrap();
            assert_eq!(report.accepted, 1);
            assert_eq!(report.applied, 1);
            assert_eq!(report.side_effects, 1);
        }

        let persistence = handle.drain_persistence_side_effects(8).await.unwrap();
        assert!(
            persistence.is_empty(),
            "pane output should not queue registry persistence: {persistence:?}"
        );
        let render = handle.drain_render_side_effects(8).await.unwrap();
        assert_eq!(
            render,
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that actor-applied runtime events refresh the session registry
/// by queuing a persistence-worker side effect rather than writing from inside
/// the actor. Daemon discovery must see sessions whose state changes through
/// typed events, and persistence-completion diagnostics must not recursively
/// enqueue another registry write.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_persists_registry_after_applied_runtime_events() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-event-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    service.set_session_registry(registry.clone());
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Process(ProcessEvent::Spawned {
            pane_id: "%1".to_string(),
            pid: Some(42),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 3,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
        assert_eq!(registry.list().unwrap().len(), 1);
        assert!(
            handle
                .drain_persistence_side_effects(8)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that audit records created through actor-owned runtime commands are
/// queued for the persistence worker instead of written from inside the actor.
/// The command still mutates policy state immediately, while the audit JSONL
/// append is drained through the target-specific persistence path.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_audit_writes_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-audit-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let audit_path = root.join("audit.jsonl");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_audit_log(crate::security::audit::AuditLog::new(
        crate::security::audit::AuditConfig {
            enabled: true,
            path: audit_path.clone(),
            hash_chain: true,
            required: true,
        },
    ));
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "/approval full-access".to_string())
            .await
            .unwrap();
        assert!(response.contains("changed=true"), "{response}");
        assert!(!audit_path.exists());

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);

        let audit = std::fs::read_to_string(&audit_path).unwrap();
        assert!(audit.contains(r#""event_type":"permission""#), "{audit}");
        assert!(
            audit.contains(r#""permission_id":"permissions.approval_policy""#),
            "{audit}"
        );
        assert!(audit.contains(r#""hash":"#), "{audit}");
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"audit_log""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that file-backed pane pipes start and append output through the
/// persistence worker in the async actor path. This keeps both `pipe-pane -o`
/// setup and later pane-output application from blocking on file I/O while
/// preserving the existing user behavior.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_file_pane_pipe_writes_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-pane-pipe-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("pane.log");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let started = handle
            .execute_terminal_command(primary, format!("pipe-pane -o {}", path.display()))
            .await
            .unwrap();
        assert!(started.contains("pipe=started"), "{started}");
        assert!(
            !path.exists(),
            "async actor should not create file-backed pipe output before persistence worker drains"
        );

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-async\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("pipe-async")
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"pane_pipe""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that file-backed pane pipe persistence failures stop the active
/// pipe through the actor. A failed async append otherwise leaves runtime state
/// believing that pane output is still being captured even though subsequent
/// writes will continue to fail.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_stops_file_pane_pipe_after_persistence_failure() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-pane-pipe-failed-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("pane.log");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let started = service
        .execute_terminal_command(&primary, &format!("pipe-pane -o {}", path.display()))
        .unwrap();
    assert!(started.contains("pipe=started"), "{started}");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-fail\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 0);
        assert_eq!(persistence.failed, 1);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"pane_pipe""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pipe":"stopped""#)
            && event.payload.contains(r#""reason":"persistence-failed""#)
    }));
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that command-backed pane pipes are checked by actor-owned timers
/// after accepted pane output. The command writer can fail after `write_output`
/// has already accepted bytes into its bounded queue; bounded health-timer
/// polling makes that asynchronous failure visible and stops the active pipe
/// without requiring a later pane-output write or an explicit `pipe-pane --stop`.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_stops_command_pane_pipe_after_health_timer() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-command-pane-pipe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("pipe-command.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nhead -c 1 >/dev/null\nsleep 0.02\nexit 7\n",
    )
    .unwrap();
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let started = service
        .execute_terminal_command(&primary, &format!("pipe-pane /bin/sh {}", script.display()))
        .unwrap();
    assert!(started.contains("pipe=started"), "{started}");
    assert!(started.contains("mode=command"), "{started}");
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-command-health\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

        let timers = tokio::time::timeout(
            Duration::from_secs(2),
            run_async_runtime_timer_side_effect_service(
                &handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 20,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                },
                1_000,
                |polls, _| polls >= 20,
            ),
        )
        .await
        .expect("command pane-pipe health checks should complete within two seconds")
        .unwrap();
        assert!(timers.drained >= 1, "{timers:?}");
        assert!(timers.fired >= 1, "{timers:?}");
        assert!(timers.submitted_events >= 1, "{timers:?}");
        assert!(timers.applied_events >= 1, "{timers:?}");
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pipe":"stopped""#)
            && event.payload.contains(r#""mode":"command""#)
            && event.payload.contains(r#""reason":"command-failed""#)
    }));
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that actor-owned command-backed pane pipes receive a health timer
/// as soon as the terminal command starts the pipe and reschedule that health
/// check while the pipe command is still active. This protects command pipe
/// lifecycle cleanup from depending on unrelated pane output and keeps quick
/// exits or deferred startup failures discoverable through timer ingress.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_actor_schedules_command_pane_pipe_health_after_start() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let started = handle
            .execute_terminal_command(primary.clone(), "pipe-pane cat >/dev/null".to_string())
            .await
            .unwrap();
        assert!(started.contains("pipe=started"), "{started}");
        assert!(started.contains("mode=command"), "{started}");

        let effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1, "{effects:?}");
        let first_key = match &effects[0] {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
                assert_eq!(key.kind, RuntimeTimerKind::PanePipeHealth);
                assert_eq!(key.owner_id, "%1");
                assert_eq!(*delay_ms, 50);
                key.clone()
            }
            other => panic!("expected pane-pipe health timer, got {other:?}"),
        };

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: first_key.clone(),
            now_ms: 1_060,
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 0);
        assert_eq!(report.side_effects, 1);

        let effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1, "{effects:?}");
        let second_key = match &effects[0] {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
                assert_eq!(key.kind, RuntimeTimerKind::PanePipeHealth);
                assert_eq!(key.owner_id, "%1");
                assert_eq!(*delay_ms, 50);
                key.clone()
            }
            other => panic!("expected rescheduled pane-pipe health timer, got {other:?}"),
        };
        assert!(second_key.generation > first_key.generation);

        let stopped = handle
            .execute_terminal_command(primary, "pipe-pane --stop".to_string())
            .await
            .unwrap();
        assert!(stopped.contains("pipe=stopped"), "{stopped}");
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
}

/// Verifies that project-scoped approval persistence updates runtime policy
/// immediately while deferring the project config file write to the persistence
/// worker. This covers the approval workflow's config-producing path and
/// prevents actor-owned control requests from writing `.mezzanine/config.toml`
/// inline.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_project_approval_config_to_persistence_worker() {
    use crate::control::{decode_control_frame, encode_control_body};

    let root = std::env::temp_dir().join(format!(
        "mez-async-project-approval-config-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let project_config = root.join(".mezzanine/config.toml");
    let mut trust_store = ProjectTrustStore::default();
    trust_store
        .decide_at(
            root.clone(),
            TrustDecision::Trusted,
            Some(root.join(".git")),
            1,
        )
        .unwrap();
    let mut service = test_service();
    service.set_project_trust_store(trust_store, None);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 10)
        .unwrap();
    let started = service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .apply_pane_foreground_process_event(
            started.pane_id.clone(),
            "cat",
            started.primary_pid,
            Some(root.to_string_lossy().to_string()),
        )
        .unwrap();
    let approval_id = service
        .queue_blocked_approval(mez_agent::permissions::BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "mez-test-command --flag".to_string(),
            declared_effects: vec!["unknown command effects".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: mez_agent::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();
    let deny_id = service
        .queue_blocked_approval(mez_agent::permissions::BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "mez-test-command --delete".to_string(),
            declared_effects: vec!["unknown command effects".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: mez_agent::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"allow-project","method":"approval/decide","params":{{"approval_id":"{}","decision":"approve","scope":{{"persistence":"project"}},"idempotency_key":"allow-project"}}}}"#,
            approval_id
        ));
        let result = handle
            .handle_control_input_for_connection(
                input,
                4096,
                ControlConnectionState::trusted_existing_client(primary.clone()),
            )
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""state":"approved""#), "{body}");
        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"deny-project","method":"approval/decide","params":{{"approval_id":"{}","decision":"disapprove","scope":{{"persistence":"project"}},"idempotency_key":"deny-project"}}}}"#,
            deny_id
        ));
        let result = handle
            .handle_control_input_for_connection(
                input,
                4096,
                ControlConnectionState::trusted_existing_client(primary),
            )
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""state":"disapproved""#), "{body}");
        assert!(
            !project_config.exists(),
            "async actor should not write project config before persistence worker drains"
        );

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 2);
        assert_eq!(persistence.completed, 2);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        let config_text = std::fs::read_to_string(&project_config).unwrap();
        assert!(config_text.contains(r#"match = "exact_sha256""#));
        assert!(config_text.contains(r#"decision = "allow""#));
        assert!(config_text.contains(r#"decision = "deny""#));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --flag"),
        mez_agent::RuleDecision::Allow
    );
    assert_eq!(
        exit.service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --delete"),
        mez_agent::RuleDecision::Forbid
    );
    assert!(exit.commands_processed >= 4);
    exit.service.terminate_all_pane_processes().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that runtime `config/set` requests which target the user-private
/// config file update actor-owned runtime configuration immediately while
/// deferring the actual file replacement to the async persistence worker. This
/// prevents actor-owned control requests from performing inline config writes
/// while preserving the user-visible live reload semantics of the command.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_user_config_mutation_to_persistence_worker() {
    use crate::control::{decode_control_frame, encode_control_body};

    let root = std::env::temp_dir().join(format!(
        "mez-async-user-config-mutation-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let config_root = root.join("config");
    let config_path = config_root.join("config.toml");
    std::fs::create_dir_all(&config_root).unwrap();
    std::fs::write(&config_path, "[history]\nlines = 10\n").unwrap();

    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 10)
        .unwrap();
    service.set_config_root(config_root);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(config_path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: std::fs::read_to_string(&config_path).unwrap(),
        }])
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let config_path_json = serde_json::to_string(&config_path.to_string_lossy()).unwrap();
        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"user-config-set","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"user","path":{config_path_json}}},"idempotency_key":"user-config-set"}}}}"#
        ));
        let result = handle
            .handle_control_input_for_connection(
                input,
                4096,
                ControlConnectionState::trusted_existing_client(primary),
            )
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""applied":true"#), "{body}");
        assert!(body.contains(r#""persisted":true"#), "{body}");
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("lines = 10"),
            "async actor should not replace the user config file before the persistence worker drains"
        );

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("lines = 7")
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.service.terminal_history_limit(), 7);
    assert!(exit.commands_processed >= 3);
    let _ = std::fs::remove_dir_all(root);
}
