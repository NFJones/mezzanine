//! Actual actor end/lookup/terminal scheduling without native reinjection.

use super::*;
use mez_agent::messaging::DeliveryTerminalReason;

/// A real TTL-free tracked native send survives removal of its exact endpoint.
/// The ordinary actor persists o1, resolves canonical absence and commits d1
/// through the common terminal worker. No peer context, delivery ACK, replacement
/// task or provider continuation is created by the terminal observation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_actor_settles_actual_ended_native_slot_without_input_or_ack() {
    let fixture = Fixture::new(true);
    let (_, mut actor, recipient, turn) = native_receive::scene(&fixture).await;
    native_receive::accept(&mut actor, &recipient, &turn, "ended-native-original").await;
    let tasks = actor.service.agent_turn_ledger().turns().len();
    let providers = actor.service.pending_agent_provider_tasks();
    assert_eq!(
        actor
            .service
            .agent_transcript_store()
            .unwrap()
            .inspect(&turn.conversation_id)
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::NotFound
    );
    assert!(
        actor
            .service
            .message_service_mut()
            .retire_agent_identity(&recipient)
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndAwaitingAdoption
    );
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(
        actor
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), tasks);
    assert_eq!(actor.service.pending_agent_provider_tasks(), providers);
    assert!(!actor.service.has_pending_native_input());
    assert!(
        actor.service.agent_turn_contexts()[&turn.turn_id]
            .blocks()
            .iter()
            .all(|block| block.source != mez_agent::ContextSourceKind::PeerMessage)
    );
    assert_eq!(
        actor
            .service
            .agent_transcript_store()
            .unwrap()
            .inspect(&turn.conversation_id)
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::NotFound
    );
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let history = store
        .fold_custody_history(mez_agent::messaging::MessageAcceptanceDecodeLimits {
            payload_bytes: 1024 * 1024,
            auxiliary_bytes: 512 * 1024,
            recipient_count: 4096,
            recipient_bytes: 1024 * 1024,
        })
        .unwrap();
    assert_eq!(history.terminal_observations.len(), 1);
    assert_eq!(
        history.terminal_observations[0].reason,
        DeliveryTerminalReason::RecipientRetired
    );
    assert!(history.acknowledgements.is_empty());
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT count(*) FROM records WHERE key LIKE 'c1:%' OR key LIKE 'k1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
