//! Actual original source/terminal capabilities with shared physical receipt
//! recovery. This is not the future canonical handoff root worker or consume
//! journal; writing a typed archive row alone must leave the outcome pending.

use super::*;
use crate::storage::transcript::{AgentTranscriptStore, CustodyReceiptPublicationFault};
use mez_agent::messaging::MessageNativeIncarnation;
use mez_agent::transcript::{
    TranscriptContextEvent, TranscriptEntry, TranscriptRole, TranscriptTerminalHandoffEvent,
    TranscriptTerminalHandoffSource,
};

mod canonical;
mod crash;
mod failures;

/// Real source/expiry/root setup shared by canonical and failure test owners.
/// Application projection is descriptive until the actual root journals it.
struct Scene {
    root: MmpCustodyRoot,
    service: MessageService,
    sender: SenderIdentity,
    registration: mez_agent::messaging::MessageSenderRegistration,
    source: mez_agent::messaging::MessageTerminalHandoffAnchor,
    transcript: AgentTranscriptStore,
    projection: Arc<crate::storage::mmp_custody::terminal_handoff::TerminalHandoffProjection>,
}
impl Scene {
    async fn new(fixture: &Fixture, probe: Arc<Probe>) -> Self {
        let mut root = fixture.root();
        assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
        Arc::get_mut(&mut root.0).unwrap().handoff_probe = Some(probe);
        let (mut service, sender, target, _) = fixture_service();
        root.configure_payload_spill(&mut service, Default::default())
            .unwrap();
        service
            .require_native_continuity(root.storage_scope())
            .unwrap();
        let registration = service
            .capture_sender_registration(&sender.agent_id)
            .unwrap();
        service
            .bind_native_conversation(
                &registration,
                "native-sender",
                &MessageNativeIncarnation::default(),
            )
            .unwrap();
        root.start_native_continuity(&service, &sender.agent_id)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::NativeContinuityAwaitingAdoption
        );
        root.adopt_native_continuity(&mut service).unwrap();
        super::expiry::accept(&root, &mut service, &sender, &target, 10, 5).await;
        let ticket = service
            .prepare_storage_expiry(root.storage_scope(), 16, usize::MAX)
            .unwrap()
            .unwrap();
        root.start_terminal(&mut service, &ticket).unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::TerminalAwaitingAdoption
        );
        root.adopt_terminal(&mut service).unwrap();
        drop(ticket);
        let outcome = service
            .delivery_outcomes_for(&sender.agent_id, 1)
            .unwrap()
            .remove(0);
        let source = service
            .capture_terminal_handoff(&sender.agent_id, &outcome)
            .unwrap();
        let transcript = AgentTranscriptStore::new(fixture.path.join("canonical"));
        let mut row = TranscriptEntry {
            conversation_id: "native-sender".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "actual-original-context".into(),
            agent_id: sender.agent_id.to_string(),
            pane_id: "original-pane".into(),
            content: "original prefix".into(),
        };
        transcript.append(&row).unwrap();
        row.role = TranscriptRole::System;
        let (label, content) = mez_agent::transcript::terminal_outcome_reference(&outcome).unwrap();
        row.content = content;
        let projection = Arc::new(
            crate::storage::mmp_custody::terminal_handoff::TerminalHandoffProjection::new(
                row,
                11,
                label,
                Vec::new(),
            )
            .unwrap(),
        );
        Self {
            root,
            service,
            sender,
            registration,
            source,
            transcript,
            projection,
        }
    }
    fn start(&mut self) {
        self.root
            .start_terminal_handoff(
                &mut self.service,
                self.source.clone(),
                self.transcript.clone(),
                Some(self.projection.clone()),
            )
            .unwrap();
    }
    /// Observes actual physical journal phases without claiming lower adoption.
    fn phases(fixture: &Fixture) -> (i64, i64, i64) {
        fixture.connection().query_row("SELECT (SELECT count(*) FROM records WHERE key LIKE 'u1:%'),(SELECT count(*) FROM records WHERE key LIKE 'h1:%'),generation FROM state", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap()
    }
}

/// Real original SQLite/native continuity, acceptance and t1 lead to actual
/// sender caps. A replaced epoch can neither materialize another live handoff
/// nor retarget its complete staged image. Qualified lookup retains old source
/// attribution, promotes only its own complete image and aborts only incomplete
/// pre-journal bytes. Neither path consumes the lower outcome, schedules a task
/// or grants the replacement ownership merely because a typed row was written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_handoff_actual_source_recovers_physical_stage_after_sender_replacement() {
    for complete in [false, true] {
        let fixture = Fixture::new(true);
        let root = fixture.root();
        assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
        let (mut service, sender, target, _) = fixture_service();
        root.configure_payload_spill(&mut service, Default::default())
            .unwrap();
        service
            .require_native_continuity(root.storage_scope())
            .unwrap();
        let registration = service
            .capture_sender_registration(&sender.agent_id)
            .unwrap();
        let original = service
            .bind_native_conversation(
                &registration,
                "native-sender",
                &MessageNativeIncarnation::default(),
            )
            .unwrap();
        root.start_native_continuity(&service, &sender.agent_id)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::NativeContinuityAwaitingAdoption
        );
        assert!(root.adopt_native_continuity(&mut service).unwrap());
        expiry::accept(&root, &mut service, &sender, &target, 10, 5).await;
        let ticket = service
            .prepare_storage_expiry(root.storage_scope(), 16, 8 * 1024 * 1024)
            .unwrap()
            .unwrap();
        root.start_terminal(&mut service, &ticket).unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::TerminalAwaitingAdoption
        );
        assert_eq!(root.adopt_terminal(&mut service).unwrap(), 1);
        drop(ticket);
        let outcome = service
            .delivery_outcomes_for(&sender.agent_id, 1)
            .unwrap()
            .remove(0);
        let source = service
            .capture_terminal_handoff(&sender.agent_id, &outcome)
            .unwrap();
        assert_eq!(source.native_owner(), &original);
        let store_id: Vec<u8> = fixture
            .connection()
            .query_row("SELECT store_id FROM state", [], |r| r.get(0))
            .unwrap();
        let store_id: [u8; 16] = store_id.try_into().unwrap();
        let occurrence = source.occurrence(&store_id);
        let event = TranscriptTerminalHandoffEvent::new(
            TranscriptTerminalHandoffSource::from_anchor(&source, store_id).unwrap(),
            11,
        )
        .unwrap();
        let transcript = AgentTranscriptStore::new(fixture.path.join("canonical"));
        let mut row = TranscriptEntry {
            conversation_id: "native-sender".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "actual-original-context".into(),
            agent_id: sender.agent_id.to_string(),
            pane_id: "original-pane".into(),
            content: "original prefix".into(),
        };
        transcript.append(&row).unwrap();
        let (owner, lease) = transcript
            .recover_custody_lease(&source, &store_id, &occurrence, None, true)
            .unwrap();
        assert!(owner.is_none());
        let lease = lease.unwrap();
        row.role = TranscriptRole::System;
        row.content = TranscriptContextEvent::TerminalHandoff {
            event: Box::new(event),
        }
        .to_transcript_content();
        let plan = lease
            .prepare_custody_append(&mut Vec::new(), &mut row)
            .unwrap();
        transcript.fail_custody_receipt_publication_for_tests(if complete {
            CustodyReceiptPublicationFault::Complete
        } else {
            CustodyReceiptPublicationFault::Partial
        });
        assert!(
            lease
                .publish_append_receipt(plan.clone(), &[row.clone()])
                .is_err()
        );
        drop(lease);
        drop(source);
        let replacement = service
            .bind_native_conversation(
                &registration,
                "native-sender",
                &MessageNativeIncarnation::default(),
            )
            .unwrap();
        assert_ne!(original, replacement);
        assert!(
            service
                .capture_terminal_handoff(&sender.agent_id, &outcome)
                .is_err()
        );
        let lookup = service
            .capture_terminal_handoff_lookup(outcome.sequence, 0)
            .unwrap();
        assert_eq!(lookup.native_owner(), &original);
        assert!(!lookup.is_live());
        assert!(!service.terminal_handoff_owner_is_current(&lookup));
        assert!(TranscriptTerminalHandoffSource::from_anchor(&lookup, store_id).is_err());
        transcript.recover_append_receipts().unwrap();
        assert_eq!(transcript.inspect("native-sender").unwrap().len(), 1);
        let (owner, lease) = transcript
            .recover_custody_lease(&lookup, &store_id, &occurrence, None, false)
            .unwrap();
        if complete {
            let owner = owner.unwrap();
            let lease = lease.unwrap();
            assert_eq!(owner.plan(), &plan);
            assert_eq!(owner.rows().unwrap(), vec![row.clone()]);
            lease
                .append_custody_receipt(owner.plan(), &owner.rows().unwrap())
                .unwrap();
            assert_eq!(lease.seal(plan.input()).unwrap(), row);
            // Physical tests retire explicitly; production must first qualify
            // its original canonical journal. No lower proof is minted here.
            owner.retire().unwrap();
        } else {
            assert!(owner.is_none());
            assert!(lease.is_none());
        }
        let (owner, lease) = transcript
            .recover_custody_lease(&lookup, &store_id, &occurrence, None, false)
            .unwrap();
        assert!(owner.is_none());
        assert!(lease.is_none());
        assert!(
            service
                .delivery_outcomes_for(&sender.agent_id, 1)
                .unwrap()
                .is_empty()
        );
        let retained = service
            .capture_terminal_handoff_lookup(outcome.sequence, 0)
            .unwrap();
        assert_eq!(retained.outcome(), outcome);
        assert!(!retained.terminal().released);
        assert_eq!(
            service.storage_usage().unwrap().unwrap().pending_messages,
            1
        );
        assert!(
            service
                .acknowledge_delivery_outcome(&sender.agent_id, &outcome)
                .is_err()
        );
        assert!(
            service
                .prepare_storage_horizon_release(root.storage_scope(), 600016, usize::MAX)
                .unwrap()
                .is_none()
        );
    }
}
