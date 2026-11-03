//! Native shell/source binding through the real runtime and SQLite worker.
//!
//! Reusable pane/agent IDs are held constant while actual conversation bindings
//! change. No provider, saved bearer or fabricated storage receipt is involved.

use super::*;
use crate::runtime::RuntimeSessionService;
use mez_agent::messaging::{MessageAcceptanceDecodeLimits, MessageNativeOwner};
use mez_core::ids::PaneId;

/// Constructs a real native shell under the actual fixture layout and enables
/// the same internal fresh-root policy as the actor before capturing ownership.
async fn native_runtime(
    fixture: &Fixture,
) -> (
    MmpCustodyRoot,
    RuntimeSessionService,
    SenderIdentity,
    SenderIdentity,
) {
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let target = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1"),
            "agent",
            &["agent-harness"],
            10,
        )
        .unwrap();
    service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap();
    let sender = service
        .message_service_mut()
        .register_agent(None, None, "raw", vec![]);
    root.configure_payload_spill(service.message_service_mut(), Default::default())
        .unwrap();
    service.refresh_native_message_bindings().unwrap();
    (root, service, sender, target)
}

/// Observes only an actual runtime-qualified association, without granting any
/// sender/application permission from the returned descriptive fields.
fn native_owner(service: &RuntimeSessionService, target: &SenderIdentity) -> MessageNativeOwner {
    let messages = service.message_service();
    let token = messages
        .capture_sender_registration(&target.agent_id)
        .unwrap();
    messages
        .native_owner_for_registration(
            &token,
            &service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .native_message_incarnation,
        )
        .unwrap()
        .clone()
}

/// Reads verified quarantined original bytes from the generated store for
/// assertions only; this secondary forensic connection never writes or adopts.
fn disk_owner(fixture: &Fixture, seq: u64) -> MessageNativeOwner {
    let store = MmpCustodyStore::open(&fixture.path, "original-session").unwrap();
    let (source, _, _) = store
        .read_accepted_source(
            seq,
            MessageAcceptanceDecodeLimits {
                payload_bytes: 1024 * 1024,
                auxiliary_bytes: 8 * 1024 * 1024,
                recipient_count: 40_000,
                recipient_bytes: 8 * 1024 * 1024,
            },
        )
        .unwrap();
    assert!(source.has_native_binding_record());
    source.native_recipient(0).unwrap().clone()
}

/// A stale checkpoint can name X at the same registration floor used by Y's
/// later accepted original. The actual source freezes Y before I/O; returning
/// the live pane to X while that worker is gated cannot rewrite it or expose
/// Y's mail to X. This establishes source attribution, not cold restart adoption.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_native_source_preserves_new_binding_against_stale_checkpoint_labels() {
    let fixture = Fixture::new(false);
    let (root, mut service, sender, target) = native_runtime(&fixture).await;
    let original_shell = service.agent_shell_store().get("%1").unwrap().clone();
    let checkpoint = service.message_service().snapshot_state().unwrap();
    let owner_x = native_owner(&service, &target);
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "conversation-y", 0)
        .unwrap();
    service.refresh_native_message_bindings().unwrap();
    let owner_y = native_owner(&service, &target);
    assert_ne!(owner_x, owner_y);
    assert_eq!(service.message_service().next_message_sequence(), 1);
    assert_eq!(
        checkpoint
            .registration_floors
            .as_ref()
            .unwrap()
            .iter()
            .find(|r| r.recipient == target.agent_id.as_str())
            .unwrap()
            .registered_after_sequence,
        0
    );
    let mut original = envelope(&sender, &target, "owned-by-y");
    original.ttl_ms = None;
    let ticket = service
        .message_service_mut()
        .prepare_acceptance(
            &sender.agent_id,
            original,
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(service.message_service_mut(), &ticket, None)
        .unwrap();
    fixture.probe.started.notified().await;
    service
        .agent_shell_store_mut()
        .restore_session("%1", original_shell)
        .unwrap();
    service.refresh_native_message_bindings().unwrap();
    assert_eq!(native_owner(&service, &target), owner_x);
    fixture.probe.release();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    root.adopt(service.message_service_mut(), None).unwrap();
    assert_eq!(disk_owner(&fixture, ticket.sequence()), owner_y);
    assert!(
        service
            .message_service()
            .receive_subscribed(&target.agent_id, 101, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(
        service
            .message_service()
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
}

/// Independent same-conversation resume changes the actual shell incarnation,
/// while exact transaction rollback restores its original one. Both paths use
/// real shell operations and the same unchanged MMP registration and source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_independent_resume_cannot_inherit_native_mail_but_exact_rollback_can() {
    let fixture = Fixture::new(true);
    let (root, mut service, sender, target) = native_runtime(&fixture).await;
    let original_shell = service.agent_shell_store().get("%1").unwrap().clone();
    let original_owner = native_owner(&service, &target);
    let mut original = envelope(&sender, &target, "original-native");
    original.ttl_ms = None;
    let ticket = service
        .message_service_mut()
        .prepare_acceptance(
            &sender.agent_id,
            original,
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(service.message_service_mut(), &ticket, None)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    root.adopt(service.message_service_mut(), None).unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", &original_shell.session_id, 0)
        .unwrap();
    service.refresh_native_message_bindings().unwrap();
    assert_ne!(native_owner(&service, &target), original_owner);
    assert!(
        service
            .message_service()
            .receive_subscribed(&target.agent_id, 101, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(
        service.message_service().next_message_sequence(),
        ticket.sequence() + 1
    );
    service
        .agent_shell_store_mut()
        .restore_session("%1", original_shell)
        .unwrap();
    service.refresh_native_message_bindings().unwrap();
    assert_eq!(native_owner(&service, &target), original_owner);
    assert_eq!(
        service
            .message_service()
            .receive_subscribed(&target.agent_id, 102, 1)
            .unwrap()
            .messages[0]
            .envelope
            .id,
        "original-native"
    );
    assert_eq!(disk_owner(&fixture, ticket.sequence()), original_owner);
}

/// Temporary ephemeral work suspends its actual parent's native delivery. It
/// cannot consume or retire the parent's stored original; exact restoration
/// reopens the same source without copying it into a temporary conversation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_ephemeral_binding_suspends_parent_mail_without_retiring_it() {
    let fixture = Fixture::new(true);
    let (root, mut service, sender, target) = native_runtime(&fixture).await;
    let original_shell = service.agent_shell_store().get("%1").unwrap().clone();
    let original_owner = native_owner(&service, &target);
    let mut original = envelope(&sender, &target, "parent-mail");
    original.ttl_ms = None;
    let ticket = service
        .message_service_mut()
        .prepare_acceptance(
            &sender.agent_id,
            original,
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(service.message_service_mut(), &ticket, None)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    root.adopt(service.message_service_mut(), None).unwrap();
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage("%1", "temporary", 0, None)
        .unwrap();
    service.refresh_native_message_bindings().unwrap();
    assert_eq!(native_owner(&service, &target), original_owner);
    assert!(
        service
            .message_service()
            .receive_subscribed(&target.agent_id, 101, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(
        service
            .message_service_mut()
            .settle_delivery_outcomes_at(102)
            .unwrap(),
        0
    );
    assert_eq!(
        service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    service
        .agent_shell_store_mut()
        .restore_session("%1", original_shell)
        .unwrap();
    service.refresh_native_message_bindings().unwrap();
    assert_eq!(
        service
            .message_service()
            .receive_subscribed(&target.agent_id, 103, 1)
            .unwrap()
            .messages[0]
            .envelope
            .id,
        "parent-mail"
    );
}

/// A parent replacement may occur before the next native association refresh.
/// Neither its predecessor's cached conversation nor an unrelated transcript
/// seed proves the new durable parent. Such first-observed ephemeral work stays
/// unavailable until the actual parent returns; accepted ownership is never
/// inferred from history ancestry or stale public registration labels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ephemeral_first_observation_cannot_inherit_stale_parent_or_transcript_seed() {
    for keep_previous in [true, false] {
        let fixture = Fixture::new(true);
        let (root, mut service, sender, target) = native_runtime(&fixture).await;
        if !keep_previous {
            service
                .message_service_mut()
                .reconcile_native_conversations(&[])
                .unwrap();
        }
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", "new-parent", 0)
            .unwrap();
        let parent = service.agent_shell_store().get("%1").unwrap().clone();
        service
            .agent_shell_store_mut()
            .bind_ephemeral_conversation_with_lineage_and_transcript_source(
                "%1",
                "temporary",
                0,
                None,
                Some("unrelated-history-seed".into()),
                0,
            )
            .unwrap();
        service.refresh_native_message_bindings().unwrap();
        service.refresh_native_message_bindings().unwrap();
        assert_eq!(
            native_owner(&service, &target).conversation_id(),
            "temporary",
            "first-observed ephemeral binding must not invent its durable parent"
        );
        let mut mail = envelope(&sender, &target, "wait-for-real-parent");
        mail.ttl_ms = None;
        assert!(
            service
                .message_service_mut()
                .prepare_acceptance(
                    &sender.agent_id,
                    mail.clone(),
                    MessageScope::Session,
                    100,
                    4096,
                    None
                )
                .is_err()
        );
        assert_eq!(service.message_service().next_message_sequence(), 1);
        assert_eq!(fixture.disk(), (0, 0));
        service
            .agent_shell_store_mut()
            .restore_session("%1", parent)
            .unwrap();
        service.refresh_native_message_bindings().unwrap();
        assert_eq!(
            native_owner(&service, &target).conversation_id(),
            "new-parent"
        );
        let ticket = service
            .message_service_mut()
            .prepare_acceptance(
                &sender.agent_id,
                mail,
                MessageScope::Session,
                100,
                4096,
                None,
            )
            .unwrap();
        root.start(service.message_service_mut(), &ticket, None)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::AwaitingAdoption
        );
        root.adopt(service.message_service_mut(), None).unwrap();
        assert_eq!(
            disk_owner(&fixture, ticket.sequence()).conversation_id(),
            "new-parent"
        );
    }
}
