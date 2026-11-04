//! Exact native ACKs across real checkpoint, private context and factory recovery.
//!
//! A later original is genuinely selected and committed through the root while
//! the earlier slot remains pending. Snapshot bits describe those actual facts;
//! only the original store and private context can restore them. No task/provider
//! capability is manufactured from the saved cursor, row or private epoch bytes.

use super::*;
use crate::storage::mmp_custody::native_input::NativeInputProjection;
use mez_agent::transcript::TranscriptContextEvent;
use std::sync::Arc;

/// Commits only seq2 to a real canonical context/archive, then persists its exact
/// ACK independently of cursor0. Optional actual private epoch replacement occurs
/// after canonical commit and before ACK, retaining the old proof's original slot.
pub(super) async fn commit_later_original(
    service: &mut RuntimeSessionService,
    root: &MmpCustodyRoot,
    replace_incarnation: bool,
) {
    let lookup = service
        .message_service()
        .capture_native_input_lookup(2, 0)
        .unwrap();
    let transcripts = service.agent_transcript_store().unwrap().clone();
    root.start_native_input(
        service.message_service(),
        lookup.clone(),
        transcripts.clone(),
        None,
    )
    .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert!(
        root.adopt_native_input(service.message_service())
            .unwrap()
            .into_commit()
            .is_none()
    );
    for sequence in [1, 2] {
        assert!(
            root.start_payload_load(service.message_service_mut(), sequence)
                .unwrap()
        );
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::PayloadLoadAwaitingAdoption
        );
        assert!(
            root.publish_payload_load(service.message_service_mut())
                .unwrap()
        );
    }
    // The single-source native selector intentionally chooses the first slot.
    // Obtain a genuine two-message offer instead, then commit only its later
    // source to exercise the exact ACK boundary independently of actor order.
    let batch = service
        .message_service()
        .receive_subscribed(lookup.recipient(), 101, 2)
        .unwrap();
    assert_eq!(
        batch
            .messages
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let source = service
        .message_service()
        .capture_native_input_anchor(lookup.recipient(), 2)
        .unwrap();
    let envelope = source.resident_envelope().unwrap();
    let label = "actual later original selected before earlier pending slot".to_owned();
    let mut context = mez_agent::AgentContext::empty();
    context
        .append_peer_message_event(label.clone(), envelope.payload.clone())
        .unwrap();
    let projection = Arc::new(
        NativeInputProjection::new(
            TranscriptEntry {
                conversation_id: source.native_owner().conversation_id().into(),
                sequence: 1,
                created_at_unix_seconds: 101,
                role: TranscriptRole::System,
                turn_id: "actual-later-native-context".into(),
                agent_id: source.recipient().to_string(),
                pane_id: "%1".into(),
                content: envelope.payload.clone(),
            },
            context.event_sequence_high_water_mark(),
            label,
        )
        .unwrap(),
    );
    root.start_native_input(
        service.message_service(),
        source.clone(),
        transcripts,
        Some(projection.clone()),
    )
    .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    let proof = root
        .adopt_native_input(service.message_service())
        .unwrap()
        .into_commit()
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_append("%1", &[projection.committed_row(&proof).unwrap()])
        .unwrap();
    if replace_incarnation {
        let mut shell = service.agent_shell_store().get("%1").unwrap().clone();
        shell.native_message_incarnation = Default::default();
        service
            .agent_shell_store_mut()
            .restore_session("%1", shell)
            .unwrap();
        service.refresh_native_message_bindings().unwrap();
        root.start_native_continuity(service.message_service(), source.recipient())
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::NativeContinuityAwaitingAdoption
        );
        assert!(
            root.adopt_native_continuity(service.message_service_mut())
                .unwrap()
        );
    }
    service.checkpoint_agent_session_metadata().unwrap();
    let ack = service
        .message_service_mut()
        .prepare_native_input_acknowledgement(&proof, 512 * 1024)
        .unwrap()
        .unwrap();
    root.start_acknowledgement(service.message_service_mut(), &ack)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    root.adopt_acknowledgement(service.message_service_mut())
        .unwrap();
    assert_eq!(
        service
            .message_service()
            .subscription(source.recipient())
            .unwrap()
            .last_sequence,
        0
    );
    assert!(
        service
            .message_service_mut()
            .prepare_native_input_acknowledgement(&proof, 512 * 1024)
            .unwrap()
            .is_none()
    );
    for sequence in [1, 2] {
        service
            .message_service_mut()
            .release_stored_payload(sequence, root.storage_scope())
            .unwrap();
    }
}

/// The actual checkpoint retains an exact later positive above cursor0 and the
/// original earlier obligation. Structural validation alone grants no ACK rights.
fn assert_exact_checkpoint(fixture: &Fixture) {
    let snapshot = fixture.payload.message_state.as_ref().unwrap();
    snapshot.validate_storage_snapshot().unwrap();
    assert_eq!(snapshot.subscriptions.len(), 1);
    assert_eq!(snapshot.subscriptions[0].last_sequence, 0);
    let sources = &snapshot.storage_snapshot.as_ref().unwrap().sources;
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].sequence, 1);
    assert_eq!(sources[0].acknowledged, vec![false]);
    assert!(sources[0].pending);
    assert_eq!(sources[1].sequence, 2);
    assert_eq!(sources[1].acknowledged, vec![true]);
    assert!(!sources[1].pending);
}

/// The real factory restores per-slot facts from k1/source history before any
/// actor/provider work. A later ACK cannot skip the earlier pending occurrence;
/// an old native ACK cannot become a replacement incarnation's cursor advance.
async fn assert_factory_exact_ack(replace_incarnation: bool) {
    let fixture = Fixture::build(
        true,
        Original::NativeAckSnapshot {
            replace_incarnation,
        },
    )
    .await;
    assert_exact_checkpoint(&fixture);
    let native = PaneProcessRetirementOwner::default();
    let mut request = fixture.request();
    request.config.layers = vec![native_input::config()];
    let mut runtime = SessionFactory::create_with_native_custody(request, native, None)
        .await
        .unwrap();
    let handle = runtime.handle().actor().clone();
    let actor = runtime.actor.take().unwrap();
    assert!(actor.native_turns_for_tests().is_empty());
    assert_eq!(
        actor
            .native_subscription_for_tests(&fixture.native)
            .unwrap()
            .last_sequence,
        0
    );
    let runner = tokio::spawn(actor.run());
    let database = root_path(&fixture.config, &fixture.payload.session_id).join("custody.sqlite");
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        if replace_incarnation {
            for _ in 0..8 {
                handle.lifecycle_state().await.unwrap();
            }
        } else {
            loop {
                handle.lifecycle_state().await.unwrap();
                let db = rusqlite::Connection::open(&database).unwrap();
                if db
                    .query_row(
                        "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap()
                    == 2
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    })
    .await;
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    exited.service.terminate_all_pane_processes().unwrap();
    assert!(settled.is_ok(), "actual earlier original did not settle");
    assert_eq!(
        exited
            .service
            .message_service()
            .subscription(&fixture.native)
            .unwrap()
            .last_sequence,
        if replace_incarnation { 0 } else { 2 }
    );
    assert_eq!(
        exited
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        if replace_incarnation { 1 } else { 0 }
    );
    assert_eq!(
        exited
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation
            .snapshot_identity(),
        fixture.epoch
    );
    assert!(
        exited
            .service
            .message_service()
            .capture_native_input_lookup(2, 0)
            .is_err()
    );
    assert!(
        exited
            .service
            .message_service()
            .registered_identity(&fixture.raw)
            .is_none()
    );
    if replace_incarnation {
        let earlier = exited
            .service
            .message_service()
            .capture_native_input_lookup(1, 0)
            .unwrap();
        assert!(
            !exited
                .service
                .message_service()
                .native_input_can_receive(&earlier, 103)
        );
        assert!(exited.service.pending_agent_provider_tasks().is_empty());
    }
    let cid = exited
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let turns = exited.service.agent_turn_ledger().turns();
    assert_eq!(turns.len(), usize::from(!replace_incarnation));
    assert!(
        turns
            .iter()
            .all(|turn| turn.trigger == mez_agent::AgentTurnTrigger::LocalMessage)
    );
    let rows = exited
        .service
        .agent_transcript_store()
        .unwrap()
        .inspect(&cid)
        .unwrap();
    let mut inputs = rows
        .iter()
        .filter_map(
            |row| match TranscriptContextEvent::from_transcript_content(&row.content) {
                Some(TranscriptContextEvent::NativePeerMessage { event }) => {
                    Some(event.source.message_sequence)
                }
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    inputs.sort_unstable();
    assert_eq!(
        inputs,
        if replace_incarnation {
            vec![2]
        } else {
            vec![1, 2]
        }
    );
    let db = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        if replace_incarnation { 1 } else { 2 }
    );
    drop(runtime);
}

/// Restart preserves ACK2 while leaving slot1 pending; actual actor canonical
/// commit/ACK1 then advances through both explicit positives without replaying2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_out_of_order_exact_ack_snapshot() {
    assert_factory_exact_ack(false).await;
}

/// A genuinely fresh private incarnation at the same CID keeps cursor0 and
/// cannot receive old slot1 or inherit work from the old slot2 canonical ACK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_old_incarnation_ack_without_rebinding_cursor() {
    assert_factory_exact_ack(true).await;
}

/// Saved positive bits with real source hashes still cannot replace the exact
/// original ACK journal. Missing actual k1 withholds factory publication while
/// preserving the accepted originals and already committed canonical input.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_refuses_exact_ack_snapshot_without_original_ack_journal() {
    let fixture = Fixture::build(
        true,
        Original::NativeAckSnapshot {
            replace_incarnation: false,
        },
    )
    .await;
    assert_exact_checkpoint(&fixture);
    let database = root_path(&fixture.config, &fixture.payload.session_id).join("custody.sqlite");
    let db = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        db.execute("DELETE FROM records WHERE key LIKE 'k1:%'", [])
            .unwrap(),
        1
    );
    let mut request = fixture.request();
    request.config.layers = vec![native_input::config()];
    assert!(
        SessionFactory::create_with_native_custody(
            request,
            PaneProcessRetirementOwner::default(),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 'c1:%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 'a1:%:commit'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
}
