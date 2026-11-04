//! Native application ownership must survive storage without becoming authority.

use super::*;
use std::collections::BTreeMap;

/// Actual lower-issued writes serialize their immutable original, without fake
/// receipts. Product tests separately exercise the SQLite/root worker boundary.
#[derive(Default)]
struct Store(BTreeMap<u64, Vec<u8>>);
impl MessageAcceptanceStore for Store {
    fn commit(
        &mut self,
        record: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        self.0
            .insert(record.sequence(), serde_json::to_vec(record).unwrap());
        Ok(())
    }
}
impl MessagePayloadLoadStore for Store {
    fn load(&mut self, request: &MessagePayloadLoadWrite) -> Result<MessageAcceptanceSource> {
        decode_message_acceptance(
            self.0[&request.sequence()].as_slice(),
            request.decode_limits(),
        )
    }
}

/// Lower state-machine port; product controls separately prove actual SQLite
/// persistence, root ownership and joined completion before marker publication.
impl MessageNativeContinuityStore for Store {
    fn commit(&mut self, write: &MessageNativeContinuityWrite) -> Result<()> {
        write.snapshot().validate()
    }
}

/// Enabling the product barrier with retained originals hides native input and
/// duplicate success until exact journal adoption. An existing genuine offer
/// can still reserve its source-bound ACK, allowing custody to free resources.
#[test]
fn native_barrier_withholds_input_and_duplicate_but_allows_original_ack() {
    let mut s = Scene::new();
    let envelope = s.mail("barrier-original", 10);
    let write = s.write("barrier-original", 10);
    s.publish(write);
    assert_eq!(
        s.service
            .receive_subscribed(&s.target.agent_id, 11, 1)
            .unwrap()
            .messages
            .len(),
        1
    );
    s.service.require_native_continuity(&s.scope).unwrap();
    assert!(
        s.service
            .receive_subscribed(&s.target.agent_id, 11, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(
        s.service
            .storage_qualified_duplicate(
                &s.sender.agent_id,
                &envelope,
                &s.scope,
                MessageScope::Session,
                11
            )
            .is_err()
    );
    let ack = s
        .service
        .prepare_acknowledgement(&s.target.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    s.service.cancel_acknowledgement(&ack).unwrap();
    let request = s
        .service
        .prepare_native_continuity(&s.scope, &s.target.agent_id)
        .unwrap();
    let verified = request.write().persist(&mut s.store).unwrap();
    assert!(request.finish(&mut s.service, verified).unwrap());
    assert_eq!(
        s.service
            .receive_subscribed(&s.target.agent_id, 12, 1)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert!(
        s.service
            .storage_qualified_duplicate(
                &s.sender.agent_id,
                &envelope,
                &s.scope,
                MessageScope::Session,
                12
            )
            .unwrap()
            .unwrap()
            .accepted
    );
}

/// Immutable identity reconciliation may retire a namespace before the product
/// refreshes its actual shell view. A known stale native entry must stay behind
/// the barrier, rather than temporarily becoming an unbound/raw endpoint with
/// no pending journal work. Same epoch/CID/floor cannot repair that authority gap.
#[test]
fn stale_known_native_namespace_cannot_fall_back_to_unbound_admission() {
    let mut s = Scene::new();
    s.service.require_native_continuity(&s.scope).unwrap();
    let original = s
        .service
        .prepare_native_continuity(&s.scope, &s.target.agent_id)
        .unwrap();
    let verified = original.write().persist(&mut s.store).unwrap();
    assert!(original.finish(&mut s.service, verified).unwrap());
    s.target.window_id = Some(WindowId::opaque("filled-window").unwrap());
    s.service
        .ensure_agent_identity(s.target.clone(), 12)
        .unwrap();
    let before = s.service.storage_usage().unwrap().unwrap();
    assert_eq!(
        s.service.pending_native_continuity(),
        Some(&s.target.agent_id)
    );
    assert!(!s.service.native_delivery_is_available(&s.target.agent_id));
    assert!(
        s.service
            .prepare_acceptance(
                &s.sender.agent_id,
                s.mail("stale-native", 10),
                MessageScope::Session,
                13,
                4096,
                None
            )
            .is_err()
    );
    assert_eq!(s.service.next_message_sequence(), 1);
    assert_eq!(s.service.storage_usage().unwrap().unwrap(), before);
    assert!(
        s.service
            .prepare_native_continuity(&s.scope, &s.target.agent_id)
            .is_err()
    );
    let current = s
        .service
        .capture_sender_registration(&s.target.agent_id)
        .unwrap();
    s.service
        .bind_native_conversation(&current, "original-conversation", &s.incarnation)
        .unwrap();
    let fresh = s
        .service
        .prepare_native_continuity(&s.scope, &s.target.agent_id)
        .unwrap();
    let verified = fresh.write().persist(&mut s.store).unwrap();
    assert!(fresh.finish(&mut s.service, verified).unwrap());
    let write = s.write("after-current-native", 10);
    assert_eq!(write.sequence(), 1);
}

/// One fresh original storage owner with a real registration and native shell
/// identity. Public ID/floor stay unchanged throughout independent rebindings.
struct Scene {
    service: MessageService,
    scope: MessageStorageScope,
    sender: SenderIdentity,
    target: SenderIdentity,
    registration: MessageSenderRegistration,
    incarnation: MessageNativeIncarnation,
    store: Store,
}
impl Scene {
    fn new() -> Self {
        let mut service = MessageService::with_limits(1, 1024);
        let sender = service.register_agent(None, None, "sender", vec![]);
        let target = service.register_agent(None, None, "native", vec![]);
        service.subscribe(&target.agent_id).unwrap();
        let registration = service
            .capture_sender_registration(&target.agent_id)
            .unwrap();
        let incarnation = MessageNativeIncarnation::default();
        let scope = MessageStorageScope::default();
        service
            .enable_storage_custody(
                &scope,
                MessageStorageLimits {
                    cache_payload_bytes: 64,
                    ..Default::default()
                },
            )
            .unwrap();
        service
            .bind_native_conversation(&registration, "original-conversation", &incarnation)
            .unwrap();
        Self {
            service,
            scope,
            sender,
            target,
            registration,
            incarnation,
            store: Store::default(),
        }
    }
    fn mail(&self, id: &str, bytes: usize) -> Envelope {
        Envelope {
            protocol: "mmp/1",
            id: id.into(),
            message_type: "send".into(),
            time: "original".into(),
            sender: self.sender.clone(),
            recipient: Recipient::Agent(self.target.agent_id.clone()),
            correlation_id: None,
            ttl_ms: None,
            content_type: "text/plain".into(),
            payload: "x".repeat(bytes),
            extension_fields: vec![],
        }
    }
    fn write(&mut self, id: &str, bytes: usize) -> MessageAcceptanceWrite {
        let mail = self.mail(id, bytes);
        let ticket = self
            .service
            .prepare_acceptance(
                &self.sender.agent_id,
                mail,
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        self.service
            .begin_storage_acceptance_write(&ticket, &self.scope)
            .unwrap()
    }
    fn publish(&mut self, write: MessageAcceptanceWrite) -> Delivery {
        let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(&mut self.store)
        else {
            panic!("original test port must commit")
        };
        self.service.publish_acceptance(&commit).unwrap().0
    }
    fn source(&self, seq: u64) -> MessageAcceptanceSource {
        decode_message_acceptance(self.store.0[&seq].as_slice(), decode_limits()).unwrap()
    }
}
fn decode_limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024,
        auxiliary_bytes: 1024 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}

/// A source predating native attribution has unknown ownership, even when a
/// recovered current endpoint has equal public IDs. The trusted guard must keep
/// both native input and original-sender rights closed for that legacy source.
#[test]
fn guarded_native_binding_cannot_authorize_unknown_legacy_source() {
    let mut s = Scene::new();
    let write = s.write("legacy-unknown", 10);
    s.publish(write);
    // Simulate the legacy row produced by the schema1 decoder, not a new send.
    let (key, accepted) = s.service.accepted_messages.drain().next().unwrap();
    let AcceptedMessage {
        storage_scope,
        source,
        delivery,
    } = accepted;
    drop(source);
    let mut row = s.service.queue.pop_front().unwrap();
    s.service.pending.remove(&row.sequence);
    s.service.queued_by_sequence.remove(&row.sequence);
    Arc::get_mut(&mut row).unwrap().native_bindings = None;
    s.service.accepted_messages.insert(
        key,
        AcceptedMessage {
            storage_scope,
            source: row.clone(),
            delivery,
        },
    );
    s.service.pending.insert(row.sequence, row.clone());
    s.service
        .queued_by_sequence
        .insert(row.sequence, row.clone());
    s.service.queue.push_front(row);
    let row = s.service.queue.front().unwrap();
    assert!(
        s.service
            .native_recipient_is_current(row, &s.target.agent_id)
    );
    s.service.require_native_continuity(&s.scope).unwrap();
    let request = s
        .service
        .prepare_native_continuity(&s.scope, &s.target.agent_id)
        .unwrap();
    let verified = request.write().persist(&mut s.store).unwrap();
    request.finish(&mut s.service, verified).unwrap();
    let row = s.service.queue.front().unwrap();
    assert!(
        !s.service
            .native_recipient_is_current(row, &s.target.agent_id)
    );
    assert!(
        s.service
            .receive_subscribed(&s.target.agent_id, 11, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    let sender = s
        .service
        .capture_sender_registration(&s.sender.agent_id)
        .unwrap();
    s.service
        .bind_native_conversation(
            &sender,
            "sender-context",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert!(
        !s.service
            .native_sender_is_current(s.service.queue.front().unwrap())
    );
}

/// Equal public floors and conversation text do not identify an independent
/// native binding. Exact refresh and rollback reproduce only their own owner;
/// none of these observations allocates an MMP occurrence.
#[test]
fn native_incarnation_changes_without_message_sequence_allocation() {
    let mut s = Scene::new();
    let original = s
        .service
        .bind_native_conversation(&s.registration, "original-conversation", &s.incarnation)
        .unwrap();
    assert_eq!(
        original,
        s.service
            .bind_native_conversation(
                &s.registration,
                "original-conversation",
                &s.incarnation.clone()
            )
            .unwrap()
    );
    let replacement = s
        .service
        .bind_native_conversation(
            &s.registration,
            "original-conversation",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert_ne!(original, replacement);
    assert_eq!(
        original,
        s.service
            .bind_native_conversation(&s.registration, "original-conversation", &s.incarnation)
            .unwrap()
    );
    assert_eq!(s.service.last_sequence(), 0);
    assert_eq!(s.service.registration_floors[&s.target.agent_id], 0);
    s.service.retire_agent_identity(&s.target.agent_id);
    s.service
        .ensure_agent_identity(s.target.clone(), 11)
        .unwrap();
    assert!(
        s.service
            .bind_native_conversation(&s.registration, "original-conversation", &s.incarnation)
            .is_err()
    );
    let current = s
        .service
        .capture_sender_registration(&s.target.agent_id)
        .unwrap();
    assert_ne!(
        original,
        s.service
            .bind_native_conversation(&current, "original-conversation", &s.incarnation)
            .unwrap()
    );
}

/// Changing the actual application owner while physical input is outstanding
/// cannot rewrite the original source. Positive storage publication retains A's
/// exact attribution; B receives none of A's mail despite matching public ID.
#[test]
fn native_source_freezes_before_storage_completion_and_fences_replacement() {
    let mut s = Scene::new();
    let write = s.write("frozen", 10);
    let original = s
        .service
        .bind_native_conversation(&s.registration, "original-conversation", &s.incarnation)
        .unwrap();
    s.service
        .bind_native_conversation(
            &s.registration,
            "replacement",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    let delivery = s.publish(write);
    let source = s.source(delivery.sequence);
    assert!(source.has_native_binding_record());
    assert!(source.native_sender().is_none());
    assert_eq!(source.native_recipient(0), Some(&original));
    assert_eq!(source.envelope().payload, "x".repeat(10));
    assert!(
        s.service
            .receive_subscribed(&s.target.agent_id, 11, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(
        s.service.snapshot_state().is_err(),
        "schema9 must not discard native binding metadata"
    );
    assert_eq!(s.service.settle_delivery_outcomes_at(11).unwrap(), 1);
    assert_eq!(
        s.service
            .delivery_outcomes_for(&s.sender.agent_id, 100)
            .unwrap()
            .len(),
        1
    );
    let sender_registration = s
        .service
        .capture_sender_registration(&s.sender.agent_id)
        .unwrap();
    s.service
        .bind_native_conversation(
            &sender_registration,
            "new-sender",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert!(
        s.service
            .delivery_outcomes_for(&s.sender.agent_id, 100)
            .unwrap()
            .is_empty()
    );
}

/// A changed application binding blocks old mail, while an exact runtime
/// rollback restores its original owner without inventing a new incarnation or
/// retaining an unbounded history map. No terminal observation intervenes here.
#[test]
fn independent_resume_cannot_inherit_mail_but_exact_rollback_can() {
    let mut s = Scene::new();
    let write = s.write("original", 10);
    s.publish(write);
    s.service
        .bind_native_conversation(
            &s.registration,
            "original-conversation",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert!(
        s.service
            .receive_subscribed(&s.target.agent_id, 11, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    s.service
        .bind_native_conversation(&s.registration, "original-conversation", &s.incarnation)
        .unwrap();
    assert_eq!(
        s.service
            .receive_subscribed(&s.target.agent_id, 12, 1)
            .unwrap()
            .messages[0]
            .envelope
            .id,
        "original"
    );
}

/// A temporary ephemeral child cannot consume parent mail or classify it as
/// retired. Resuming the same actual parent binding releases the original
/// pending prefix, independently of unchanged public registration floors.
#[test]
fn suspended_native_binding_preserves_parent_obligation() {
    let mut s = Scene::new();
    let write = s.write("parent", 10);
    s.publish(write);
    let mut binding = MessageNativeRegistration {
        registration: s.registration.clone(),
        incarnation: s.incarnation.clone(),
        conversation_id: "original-conversation".into(),
        admission_available: true,
        delivery_available: false,
    };
    s.service
        .reconcile_native_conversations(&[binding])
        .unwrap();
    assert!(
        s.service
            .receive_subscribed(&s.target.agent_id, 11, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    let incoming = s.write("incoming-while-parent-paused", 10);
    s.publish(incoming);
    assert_eq!(s.service.settle_delivery_outcomes_at(12).unwrap(), 0);
    assert_eq!(
        s.service.storage_usage().unwrap().unwrap().pending_messages,
        2
    );
    binding = MessageNativeRegistration {
        registration: s.registration.clone(),
        incarnation: s.incarnation.clone(),
        conversation_id: "original-conversation".into(),
        admission_available: true,
        delivery_available: true,
    };
    s.service
        .reconcile_native_conversations(&[binding])
        .unwrap();
    assert_eq!(
        s.service
            .receive_subscribed(&s.target.agent_id, 13, 1)
            .unwrap()
            .messages[0]
            .envelope
            .id,
        "parent"
    );
}

/// Malformed or oversized complete product observations fail atomically, leaving
/// the original native owner and delivery rights unchanged.
#[test]
fn native_reconciliation_rejects_invalid_candidates_before_mutation() {
    let mut s = Scene::new();
    let write = s.write("original", 10);
    s.publish(write);
    let invalid = MessageNativeRegistration {
        registration: s.registration.clone(),
        incarnation: MessageNativeIncarnation::default(),
        conversation_id: "x".repeat(257),
        admission_available: true,
        delivery_available: true,
    };
    assert!(
        s.service
            .reconcile_native_conversations(&[invalid])
            .is_err()
    );
    assert_eq!(
        s.service
            .receive_subscribed(&s.target.agent_id, 11, 1)
            .unwrap()
            .messages[0]
            .envelope
            .id,
        "original"
    );
}

/// Cache eviction/reload verifies the complete source, including original native
/// ownership, while sharing the unchanged small canonical payload budget.
#[test]
fn native_binding_survives_original_source_reload() {
    let mut s = Scene::new();
    let write = s.write("cold", 40);
    let first = s.publish(write);
    let write = s.write("hot", 40);
    s.publish(write);
    let original = s
        .source(first.sequence)
        .native_recipient(0)
        .unwrap()
        .clone();
    let write = s
        .service
        .begin_payload_load(first.sequence, &s.scope)
        .unwrap()
        .unwrap();
    let mut loaded = write.load(&mut s.store);
    assert!(s.service.publish_payload_load(&mut loaded).unwrap());
    assert_eq!(
        s.source(first.sequence).native_recipient(0),
        Some(&original)
    );
    assert_eq!(
        s.service
            .receive_subscribed(&s.target.agent_id, 11, 1)
            .unwrap()
            .messages[0]
            .envelope
            .id,
        "cold"
    );
    assert!(
        s.service
            .payload_observer()
            .unwrap()
            .snapshot()
            .unwrap()
            .live_payload_bytes
            <= 64
    );
}

/// The bounded canonical decoder recognizes schema2's exact field grammar and
/// rejects future formats, invalid incarnations and mismatched slot counts.
/// A native record cannot be decoded using an envelope-only metadata allowance.
#[test]
fn native_source_decoder_rejects_future_malformed_and_unbudgeted_metadata() {
    let mut s = Scene::new();
    let write = s.write("original", 10);
    s.publish(write);
    let encoded = String::from_utf8(s.store.0[&1].clone()).unwrap();
    assert!(encoded.starts_with("{\"schema_version\":2,"));
    let id = s
        .source(1)
        .native_recipient(0)
        .unwrap()
        .incarnation()
        .to_string();
    for bad in [
        encoded.replacen("\"schema_version\":2", "\"schema_version\":3", 1),
        encoded.replace(&id, &"0".repeat(64)),
        encoded.replace("\"recipients\":[{", "\"recipients\":[null,{"),
        encoded.replace(
            "\"conversation_id\":\"original-conversation\"",
            &format!("\"conversation_id\":\"{}\"", "x".repeat(257)),
        ),
    ] {
        assert!(decode_message_acceptance(bad.as_bytes(), decode_limits()).is_err());
    }
    let row = &s.service.queued_by_sequence[&1];
    let mut limits = decode_limits();
    limits.auxiliary_bytes = auxiliary::retained_envelope_bytes(&row.envelope);
    assert!(decode_message_acceptance(encoded.as_bytes(), limits).is_err());
}

/// A well-formed but different stored native owner is not the qualified original.
/// Failed reload retains its allocation reservation and cannot publish changed
/// application attribution or expose a fabricated replacement source.
#[test]
fn native_reload_refuses_changed_valid_binding_and_keeps_reservation() {
    let mut s = Scene::new();
    let write = s.write("original", 40);
    let first = s.publish(write);
    let original = s
        .source(first.sequence)
        .native_recipient(0)
        .unwrap()
        .clone();
    s.service
        .release_stored_payload(first.sequence, &s.scope)
        .unwrap();
    let encoded = String::from_utf8(s.store.0[&first.sequence].clone()).unwrap();
    let mut forged = original.incarnation().as_bytes().to_vec();
    forged[0] = if forged[0] == b'a' { b'b' } else { b'a' };
    let forged = String::from_utf8(forged).unwrap();
    s.store.0.insert(
        first.sequence,
        encoded
            .replace(original.incarnation(), &forged)
            .into_bytes(),
    );
    let write = s
        .service
        .begin_payload_load(first.sequence, &s.scope)
        .unwrap()
        .unwrap();
    let mut result = write.load(&mut s.store);
    assert!(s.service.publish_payload_load(&mut result).is_err());
    assert_eq!(
        s.service
            .native_owner_for_registration(&s.registration, &s.incarnation),
        Some(&original)
    );
    assert!(
        s.service
            .receive_subscribed(&s.target.agent_id, 11, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    let usage = s.service.payload_observer().unwrap().snapshot().unwrap();
    assert_eq!(
        (usage.live_payload_bytes, usage.reserved_payload_bytes),
        (0, 40)
    );
}

/// Shell identity follows actual application binding transitions, independently
/// of equal conversation text. Failed validation changes nothing; ephemeral
/// child work and exact rollback retain the durable parent's incarnation.
#[test]
fn actual_shell_binding_rotates_only_on_successful_independent_durable_bind() {
    let mut shells = crate::AgentShellStore::default();
    let original = shells.enter_or_resume("%1").unwrap().clone();
    assert_eq!(
        shells
            .enter_or_resume("%1")
            .unwrap()
            .native_message_incarnation,
        original.native_message_incarnation
    );
    assert!(
        shells
            .bind_conversation_with_lineage("%1", &original.session_id, 0, Some(String::new()))
            .is_err()
    );
    assert_eq!(shells.get("%1").unwrap(), &original);
    shells
        .bind_ephemeral_conversation_with_lineage("%1", "temporary", 0, None)
        .unwrap();
    assert_eq!(
        shells.get("%1").unwrap().native_message_incarnation,
        original.native_message_incarnation
    );
    shells.restore_session("%1", original.clone()).unwrap();
    assert_eq!(shells.get("%1").unwrap(), &original);
    shells
        .bind_conversation("%1", &original.session_id, 0)
        .unwrap();
    assert_ne!(
        shells.get("%1").unwrap().native_message_incarnation,
        original.native_message_incarnation
    );
}

/// Native sender attribution freezes with the same immutable original as the
/// recipients. A different conversation cannot consume its terminal outcome;
/// exact rollback of the original actual sender can recover its own result.
#[test]
fn native_sender_source_and_terminal_rights_keep_the_original_binding() {
    let mut s = Scene::new();
    let sender = s
        .service
        .capture_sender_registration(&s.sender.agent_id)
        .unwrap();
    let incarnation = MessageNativeIncarnation::default();
    let original = s
        .service
        .bind_native_conversation(&sender, "sender-original", &incarnation)
        .unwrap();
    let write = s.write("native-sender", 10);
    s.service
        .bind_native_conversation(
            &sender,
            "sender-replacement",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    let first = s.publish(write);
    assert_eq!(s.source(first.sequence).native_sender(), Some(&original));
    s.service
        .bind_native_conversation(
            &s.registration,
            "retired-target",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    s.service.settle_delivery_outcomes_at(11).unwrap();
    assert!(
        s.service
            .delivery_outcomes_for(&s.sender.agent_id, 100)
            .unwrap()
            .is_empty()
    );
    s.service
        .bind_native_conversation(&sender, "sender-original", &incarnation)
        .unwrap();
    assert_eq!(
        s.service
            .delivery_outcomes_for(&s.sender.agent_id, 100)
            .unwrap()
            .len(),
        1
    );
}

/// Authored extension data remains ordinary content even when its key matches
/// the private source field. It cannot choose a native conversation/incarnation
/// or replace the actual application binding frozen by trusted admission.
#[test]
fn authored_native_binding_extension_cannot_replace_trusted_source_attribution() {
    let mut s = Scene::new();
    let original = s
        .service
        .native_owner_for_registration(&s.registration, &s.incarnation)
        .unwrap()
        .clone();
    let mut mail = s.mail("authored-extension", 10);
    mail.extension_fields.push(("native_bindings".into(), r#"{"conversation_id":"forged","incarnation":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#.into()));
    let ticket = s
        .service
        .prepare_acceptance(
            &s.sender.agent_id,
            mail,
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let write = s
        .service
        .begin_storage_acceptance_write(&ticket, &s.scope)
        .unwrap();
    let first = s.publish(write);
    let source = s.source(first.sequence);
    assert_eq!(source.native_recipient(0), Some(&original));
    assert_eq!(source.envelope().extension_fields[0].key, "native_bindings");
}

/// A first-observed ephemeral endpoint remains unavailable across repeated
/// reconciliations. Its descriptive temporary owner is not a qualified parent;
/// later refreshes cannot turn that cached description into acceptance rights.
#[test]
fn unqualified_ephemeral_observation_cannot_promote_itself_to_parent() {
    let mut s = Scene::new();
    let independent = MessageNativeIncarnation::default();
    for _ in 0..2 {
        let view = MessageNativeRegistration {
            registration: s.registration.clone(),
            incarnation: independent.clone(),
            conversation_id: "temporary".into(),
            admission_available: false,
            delivery_available: false,
        };
        s.service.reconcile_native_conversations(&[view]).unwrap();
        assert!(
            s.service
                .native_parent_for_registration(&s.registration, &independent)
                .is_none()
        );
        assert!(
            s.service
                .native_owner_for_registration(&s.registration, &s.incarnation)
                .is_none()
        );
        assert_eq!(
            s.service
                .native_owner_for_registration(&s.registration, &independent)
                .unwrap()
                .conversation_id(),
            "temporary"
        );
        assert!(
            s.service
                .prepare_acceptance(
                    &s.sender.agent_id,
                    s.mail("unqualified", 10),
                    MessageScope::Session,
                    10,
                    4096,
                    None
                )
                .is_err()
        );
        assert_eq!(s.service.next_message_sequence(), 1);
    }
}

/// Native owner strings count together with headers, frozen slots and worker
/// headroom before a new source or sequence is retained. A fitting payload alone
/// cannot bypass the combined metadata ceiling through a large native audience.
#[test]
fn native_audience_metadata_refuses_before_sequence_or_cache_allocation() {
    let mut service = MessageService::with_limits(1, 1024);
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(
            &scope,
            MessageStorageLimits {
                metadata_bytes: 1024 * 1024,
                ..Default::default()
            },
        )
        .unwrap();
    let sender = service.register_agent(None, None, "sender", vec![]);
    for _ in 0..200 {
        let target = service.register_agent(None, None, "native", vec![]);
        let registration = service
            .capture_sender_registration(&target.agent_id)
            .unwrap();
        service
            .bind_native_conversation(
                &registration,
                &"c".repeat(256),
                &MessageNativeIncarnation::default(),
            )
            .unwrap();
    }
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "over-metadata".into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: sender.clone(),
        recipient: Recipient::Role("native".into()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "x".into(),
        extension_fields: vec![],
    };
    let error = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope,
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap_err();
    assert_eq!(error.kind(), MessageErrorKind::Capacity);
    assert_eq!(service.next_message_sequence(), 1);
    assert!(!service.has_unpublished_acceptance());
    assert_eq!(
        service.payload_observer().unwrap().snapshot().unwrap(),
        MessagePayloadUsage::default()
    );
    assert!(service.storage_usage().unwrap().unwrap().metadata_bytes <= 1024 * 1024);
}

/// Stored idempotency belongs to the original native application incarnation,
/// just like sender outcome access. An independent binding of the SAME public
/// conversation cannot claim an earlier accepted receipt merely by resubmitting
/// byte-identical input under an unchanged MMP registration and sequence floor.
#[test]
fn native_sender_rebinding_cannot_claim_original_storage_duplicate_receipt() {
    let mut s = Scene::new();
    let sender = s
        .service
        .capture_sender_registration(&s.sender.agent_id)
        .unwrap();
    let original = MessageNativeIncarnation::default();
    s.service
        .bind_native_conversation(&sender, "sender-conversation", &original)
        .unwrap();
    let envelope = s.mail("native-original-receipt", 32);
    let write = s.write("native-original-receipt", 32);
    let accepted = s.publish(write);
    assert_eq!(
        s.service
            .storage_qualified_duplicate(
                &s.sender.agent_id,
                &envelope,
                &s.scope,
                MessageScope::Session,
                11
            )
            .unwrap()
            .unwrap(),
        accepted
    );
    s.service
        .bind_native_conversation(
            &sender,
            "sender-conversation",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    let result = s.service.storage_qualified_duplicate(
        &s.sender.agent_id,
        &envelope,
        &s.scope,
        MessageScope::Session,
        12,
    );
    assert!(
        result.is_err(),
        "an independently rebound native sender cannot inherit its predecessor's accepted receipt: {result:?}"
    );
    s.service
        .bind_native_conversation(&sender, "sender-conversation", &original)
        .unwrap();
    assert_eq!(
        s.service
            .storage_qualified_duplicate(
                &s.sender.agent_id,
                &envelope,
                &s.scope,
                MessageScope::Session,
                13
            )
            .unwrap()
            .unwrap(),
        accepted
    );
}

/// The test port completes the already-prepared transport decision. Actual
/// canonical application commit remains the caller's prerequisite; this fixture
/// exercises only how its frozen ACK interacts with later native bindings.
impl MessageAcknowledgementStore for Store {
    fn commit(&mut self, _: &MessageAcknowledgementWrite) -> Result<()> {
        Ok(())
    }
}

/// ACK progress from a different native incarnation must never conceal earlier
/// unread work when the actual original shell is restored. Test both an already
/// published ACK and its late publication after exact rollback; positive slots
/// for the other binding stay committed and cannot be delivered again.
#[test]
fn native_binding_rollback_preserves_unread_mail_across_foreign_cursor_progress() {
    for publish_before_rollback in [false, true] {
        let mut s = Scene::new();
        let original = s.incarnation.clone();
        let first = s.write("original-binding-unread", 32);
        s.publish(first);
        let other = MessageNativeIncarnation::default();
        s.service
            .bind_native_conversation(&s.registration, "original-conversation", &other)
            .unwrap();
        let second = s.write("other-binding-committed", 32);
        s.publish(second);
        let selected = s
            .service
            .receive_subscribed(&s.target.agent_id, 11, 8)
            .unwrap();
        assert_eq!(selected.messages.len(), 1);
        assert_eq!(selected.messages[0].sequence, 2);
        drop(selected);
        let ticket = s
            .service
            .prepare_acknowledgement(&s.target.agent_id, 2, 512 * 1024)
            .unwrap()
            .unwrap();
        let write = s
            .service
            .begin_storage_acknowledgement_write(&ticket, &s.scope)
            .unwrap();
        let MessageAcknowledgementWriteOutcome::Committed(commit) = write.persist(&mut s.store)
        else {
            panic!("test transport commit must succeed")
        };
        if publish_before_rollback {
            s.service.publish_acknowledgement(&commit).unwrap();
        }
        s.service
            .bind_native_conversation(&s.registration, "original-conversation", &original)
            .unwrap();
        if !publish_before_rollback {
            s.service.publish_acknowledgement(&commit).unwrap();
        }
        let own = s
            .service
            .receive_subscribed(&s.target.agent_id, 12, 8)
            .unwrap();
        assert_eq!(
            own.messages.len(),
            1,
            "foreign cursor progress cannot hide the restored original binding's unread work, publish-before={publish_before_rollback}"
        );
        assert_eq!(own.messages[0].sequence, 1);
        drop(own);
        s.service
            .bind_native_conversation(&s.registration, "original-conversation", &other)
            .unwrap();
        assert!(
            s.service
                .receive_subscribed(&s.target.agent_id, 13, 8)
                .unwrap()
                .messages
                .is_empty()
        );
    }
}
