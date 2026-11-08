//! Idle mail waits for ordinary terminal source after a genuine typed rewrite.

use super::*;
use mez_agent::messaging::{Envelope, MessageScope, Recipient};

/// Checked candidate acceptance alone cannot admit peer history when terminal
/// compaction still owns an append receipt. Preserve the original envelope and
/// cursor until the exact source/handoff rows commit, then receive it once.
/// Stale and duplicate append completions must not settle the pending marker.
#[test]
fn runtime_peer_receive_defers_terminal_compaction_handoff_receipt() {
    let (mut service, store, turn_id) = typed_source::pending();
    typed_source::queue(&mut service, &turn_id, true);
    typed_source::complete(&mut service, &turn_id, "RECEIVE_HANDOFF_SUMMARY");
    crate::test_support::compaction_source::complete_task(
        &mut service,
        &turn_id,
        "done",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let history = work.execute();
    assert!(
        service
            .complete_bookkeeping_candidate(work, history)
            .unwrap()
    );
    assert!(service.persistence.bookkeeping_pending(&conversation));
    assert!(service.claim_bookkeeping_candidates().is_empty());
    let now = crate::runtime::current_unix_millis();
    let recipient = service
        .ensure_runtime_message_identity(
            "agent-%1",
            mez_core::ids::PaneId::opaque("%1"),
            "agent",
            &[],
            now,
        )
        .unwrap();
    service
        .message_service_mut()
        .subscribe_from_retained_start(&recipient.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-handoff-sender", None, "agent", &[], now)
        .unwrap();
    let delivery = service
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "receive-handoff-envelope".into(),
                message_type: "send".into(),
                time: format!("runtime:{now}"),
                sender: sender.clone(),
                recipient: Recipient::Agent(recipient.agent_id.clone()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".into(),
                payload: "original peer after compaction".into(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now,
        )
        .unwrap();
    let transition = service.apply_peer_message_delivery_timer(now, 1).unwrap();
    assert!(!transition.applied);
    assert!(matches!(
        transition.side_effects.as_slice(),
        [RuntimeSideEffect::ScheduleTimer {
            delay_ms: 1_000,
            ..
        }]
    ));
    assert_eq!(
        service
            .message_service_mut()
            .subscription(&recipient.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    let effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    let mut receipts = Vec::new();
    for effect in effects {
        if let RuntimeSideEffect::PersistTranscriptEntries {
            store: writer,
            batch: entries,
            path,
        } = effect
        {
            writer.fail_transcript_append_after_first();
            let failure = writer
                .commit_accounted_transcript_batch(&entries, 1)
                .unwrap_err();
            assert!(
                service
                    .apply_persistence_transition(
                        crate::runtime::PersistenceEvent::TranscriptFailed {
                            conversation_id: conversation.clone(),
                            first_sequence: entries.first_sequence(),
                            batch: entries.clone(),
                            path: path.clone(),
                            error: failure.to_string(),
                            retryable: failure.kind() == crate::MezErrorKind::Io,
                        }
                    )
                    .unwrap()
                    .applied
            );
            assert!(service.persistence.bookkeeping_pending(&conversation));
            assert!(service.claim_bookkeeping_candidates().is_empty());
            let bytes = writer
                .commit_accounted_transcript_batch(&entries, 1)
                .unwrap();
            receipts.push(crate::runtime::PersistenceEvent::TranscriptCompleted {
                conversation_id: conversation.clone(),
                first_sequence: entries.first_sequence(),
                batch: entries,
                path,
                bytes,
            });
        }
    }
    assert!(!receipts.is_empty());
    let mut stale = receipts.last().unwrap().clone();
    if let crate::runtime::PersistenceEvent::TranscriptCompleted { first_sequence, .. } = &mut stale
    {
        *first_sequence += 100;
    }
    assert!(!service.apply_persistence_transition(stale).unwrap().applied);
    assert!(service.persistence.bookkeeping_pending(&conversation));
    let mut foreign = receipts.last().unwrap().clone();
    if let crate::runtime::PersistenceEvent::TranscriptCompleted { path, .. } = &mut foreign {
        *path = store.root().join("foreign-owner");
    }
    assert!(
        !service
            .apply_persistence_transition(foreign)
            .unwrap()
            .applied
    );
    assert!(service.persistence.bookkeeping_pending(&conversation));
    let mut changed = receipts.last().unwrap().clone();
    if let crate::runtime::PersistenceEvent::TranscriptCompleted { batch, .. } = &mut changed {
        let mut rows = batch.entries().to_vec();
        rows[0].role = mez_agent::TranscriptRole::User;
        rows[0].content = "different valid proposal".into();
        *batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
    }
    assert!(
        !service
            .apply_persistence_transition(changed)
            .unwrap()
            .applied
    );
    assert!(service.persistence.bookkeeping_pending(&conversation));
    for receipt in receipts {
        assert!(
            service
                .apply_persistence_transition(receipt.clone())
                .unwrap()
                .applied
        );
        assert!(
            !service
                .apply_persistence_transition(receipt)
                .unwrap()
                .applied
        );
    }
    assert!(!service.persistence.bookkeeping_pending(&conversation));
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "RECEIVE_HANDOFF_SUMMARY"
    );
    assert_eq!(
        service.deliver_pending_runtime_agent_messages(now).unwrap(),
        1
    );
    assert_eq!(
        service.deliver_pending_runtime_agent_messages(now).unwrap(),
        0
    );
    assert_eq!(
        service
            .message_service_mut()
            .subscription(&recipient.agent_id)
            .unwrap()
            .last_sequence,
        delivery.sequence
    );
    service.terminate_all_pane_processes().unwrap();
}
