//! Actual ordinary/raw/native producer sequencing through the runtime and actor.

use super::native_receive::{accept, scene, writer};
use super::*;
use crate::runtime::{PersistenceEvent, RuntimeSideEffect};
use mez_agent::transcript::{TranscriptContextEvent, TranscriptEntry, TranscriptRole};

/// An ordinary row already owns its assigned sequence while queued and after
/// dispatch. Native issuance waits through both states, then chooses its batch
/// after the real append and exact original completion retire that ownership.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_waits_for_queued_and_inflight_ordinary_rows() {
    let fixture = Fixture::new(true);
    let (_, mut actor, recipient, turn) = scene(&fixture).await;
    accept(&mut actor, &recipient, &turn, "native-after-ordinary-owner").await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert!(actor.service.pending_native_input_work().is_some());
    let store = actor.service.agent_transcript_store().unwrap().clone();
    let row = TranscriptEntry {
        conversation_id: turn.conversation_id.clone(),
        sequence: 1,
        created_at_unix_seconds: turn.started_at_unix_seconds,
        role: TranscriptRole::System,
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        pane_id: turn.pane_id.clone(),
        content: "ordinary canonical row assigned before native dispatch".into(),
    };
    let path = store.transcript_path(&turn.conversation_id).unwrap();
    actor
        .service
        .queue_transcript_for_tests(RuntimeSideEffect::PersistTranscriptEntries {
            store: store.clone(),
            path: path.clone(),
            entries: vec![row.clone()],
        });
    assert!(
        actor.service.pending_native_input_work().is_none(),
        "queued ordinary rows already own sequence1"
    );
    let effects = actor
        .service
        .drain_transcript_persistence_transition()
        .side_effects;
    assert!(
        actor.service.pending_native_input_work().is_none(),
        "dispatch cannot release in-flight raw ownership"
    );
    let mut wrote = false;
    for effect in effects {
        if let RuntimeSideEffect::PersistTranscriptEntries {
            store,
            entries,
            path,
        } = effect
        {
            let original = entries.clone();
            let bytes = tokio::task::spawn_blocking(move || store.append_many(&entries))
                .await
                .unwrap()
                .unwrap();
            actor
                .service
                .apply_persistence_transition(PersistenceEvent::TranscriptCompleted {
                    conversation_id: turn.conversation_id.clone(),
                    first_sequence: original[0].sequence,
                    entries: original,
                    path,
                    bytes,
                })
                .unwrap();
            wrote = true;
        }
    }
    assert!(wrote);
    assert!(actor.service.pending_native_input_work().is_some());
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    let rows = store.inspect(&turn.conversation_id).unwrap();
    assert_eq!(rows.first(), Some(&row));
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(
                TranscriptContextEvent::from_transcript_content(&row.content),
                Some(TranscriptContextEvent::NativePeerMessage { .. })
            ))
            .count(),
        1
    );
}

/// Stop retains its original chronology while native first-plan publication is
/// failing. It must not claim a stale bookkeeping read behind the held lease.
/// Actual root recovery, exact ACK and ordinary persistence then drain once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopped_bookkeeping_waits_for_issued_native_first_plan() {
    let fixture = Fixture::new(true);
    let (handle, mut actor, recipient, turn) = scene(&fixture).await;
    accept(&mut actor, &recipient, &turn, "native-issued-before-stop").await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    actor.fail_custody_receipt_publication_for_tests(
        crate::storage::transcript::CustodyReceiptPublicationFault::BeforeStage,
    );
    actor.poll_message_custody_for_tests();
    assert!(matches!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputRetryAt(_)
    ));
    assert!(
        actor
            .service
            .ensure_runtime_message_transfers_settled_for_restore()
            .is_err(),
        "actual issued native custody fences runtime replacement"
    );
    let primary = actor.service.session().clients()[0].id.clone();
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(
        actor.service.claim_bookkeeping_candidates().is_empty(),
        "an issued canonical producer fences competing source reads"
    );
    let store = actor.service.agent_transcript_store().unwrap().clone();
    let runner = tokio::spawn(actor.run());
    let (stop, persistence) = writer(&handle);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            handle.lifecycle_state().await.unwrap();
            if fixture
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 1
                && store.inspect(&turn.conversation_id).is_ok_and(|rows| {
                    rows.iter().any(|row| {
                        matches!(
                            TranscriptContextEvent::from_transcript_content(&row.content),
                            Some(TranscriptContextEvent::InterruptedTurn { .. })
                        )
                    })
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.store(true, std::sync::atomic::Ordering::Release);
    persistence.await.unwrap();
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert!(exited.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exited
            .service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Interrupted
    );
    let rows = exited
        .service
        .agent_transcript_store()
        .unwrap()
        .inspect(&turn.conversation_id)
        .unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(
                TranscriptContextEvent::from_transcript_content(&row.content),
                Some(TranscriptContextEvent::NativePeerMessage { .. })
            ))
            .count(),
        1
    );
    let peer = rows
        .iter()
        .position(|row| {
            matches!(
                TranscriptContextEvent::from_transcript_content(&row.content),
                Some(TranscriptContextEvent::NativePeerMessage { .. })
            )
        })
        .unwrap();
    let interruptions = rows
        .iter()
        .enumerate()
        .filter_map(|(position, row)| {
            matches!(
                TranscriptContextEvent::from_transcript_content(&row.content),
                Some(TranscriptContextEvent::InterruptedTurn { .. })
            )
            .then_some(position)
        })
        .collect::<Vec<_>>();
    assert_eq!(interruptions.len(), 1);
    assert!(interruptions[0] > peer);
}
