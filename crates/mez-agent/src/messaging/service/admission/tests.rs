//! Real lower-owner reservation/visibility tests with deterministic storage ports.

use super::*;

/// Payload acquisition precedes the issuance mutation. Even an unavailable
/// unpublished source must not install a worker attempt or bind a storage root
/// when no write input can be returned to the actual worker owner.
#[test]
fn absent_original_does_not_partially_issue_an_acceptance_write() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "absent-original"),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    service
        .admission
        .as_ref()
        .unwrap()
        .row
        .envelope
        .release_cache_for_test();
    let scope = MessageStorageScope::default();
    assert_eq!(
        service
            .begin_storage_acceptance_write(&ticket, &scope)
            .unwrap_err()
            .kind(),
        MessageErrorKind::InvalidState
    );
    let pending = service.admission.as_ref().unwrap();
    assert!(pending.attempt.is_none());
    assert!(pending.storage_scope.is_none());
    assert_eq!(pending.row.sequence, ticket.sequence());
}

/// A linear issued worker input explicitly pins its original allocation. Cache
/// release cannot invalidate its borrowed envelope or record serialization; no
/// second payload is reconstructed merely to keep the port API usable.
#[test]
fn original_write_pins_borrowed_bytes_across_cache_release() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "pinned-original"),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let row = &service.admission.as_ref().unwrap().row;
    row.envelope.release_cache_for_test();
    assert!(Arc::ptr_eq(
        &write.envelope,
        &row.envelope.resident().unwrap()
    ));
    assert_eq!(write.envelope().id, "pinned-original");
    assert!(serde_json::to_vec(&write).is_ok());
    drop(write);
    assert_eq!(row.envelope.resident_bytes(), 0);
    assert_eq!(
        row.envelope.resident().unwrap_err().kind(),
        MessageErrorKind::InvalidState
    );
}

/// Makes one immutable direct occurrence under an authenticated registered source.
fn mail(sender: &SenderIdentity, target: &AgentId, id: &str) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:admission".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: id.into(),
        extension_fields: Vec::new(),
    }
}
/// Synchronous trusted fixture port; success is an explicit store decision.
struct Store {
    calls: usize,
    uncertain: bool,
}
impl MessageAcceptanceStore for Store {
    /// Returns one deterministic joined result without hiding another worker.
    fn commit(
        &mut self,
        _record: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        self.calls += 1;
        if self.uncertain {
            self.uncertain = false;
            Err(MessageAcceptanceStoreError::Unknown(
                MessageError::invalid_state("lost commit reply"),
            ))
        } else {
            Ok(())
        }
    }
}
/// An unpublished original has no accepted receipt, delivery, offer or proof.
/// Physical completion alone still does not publish actor state; only exact
/// adoption exposes it once, and repeating the proof must not append mail again.
#[test]
fn original_is_invisible_until_qualified_storage_commit_is_adopted_once() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service
        .subscribe_from_retained_start(&target.agent_id)
        .unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "original"),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    assert!(
        service
            .snapshot_state()
            .unwrap()
            .accepted_messages
            .is_empty()
    );
    assert!(
        service
            .receive_subscribed(&target.agent_id, 2, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let mut store = Store {
        calls: 0,
        uncertain: false,
    };
    let MessageAcceptanceWriteOutcome::Committed(committed) = write.persist(&mut store) else {
        panic!("store should commit")
    };
    assert_eq!(store.calls, 1);
    assert!(
        service
            .receive_subscribed(&target.agent_id, 2, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    let (delivery, _) = service.publish_acceptance(&committed).unwrap();
    assert!(delivery.accepted);
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 2, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert_eq!(service.publish_acceptance(&committed).unwrap().0, delivery);
    assert_eq!(service.snapshot_state().unwrap().accepted_messages.len(), 1);
    assert!(
        Arc::ptr_eq(
            &service.accepted_messages.get("original").unwrap().source,
            service.pending.get(&delivery.sequence).unwrap()
        ),
        "storage adoption must install one canonical source for custody and dedup"
    );
}
/// A slow storage operation cannot monopolize recipient progress. An existing
/// original remains receivable/acknowledgeable, and a tail subscription created
/// while a new original is unpublished must not skip that later publication.
#[test]
fn pending_write_preserves_existing_ack_and_does_not_skip_new_tail_subscription() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service
        .subscribe_from_retained_start(&target.agent_id)
        .unwrap();
    let old = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "old"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "new"),
            MessageScope::Session,
            2,
            4096,
            None,
        )
        .unwrap();
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 3, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    service
        .advance_subscription(&target.agent_id, old.sequence)
        .unwrap();
    // Another already-eligible target subscribes from the actual accepted tail.
    service.subscriptions.remove(&target.agent_id);
    service.subscribe(&target.agent_id).unwrap();
    let mut store = Store {
        calls: 0,
        uncertain: false,
    };
    let MessageAcceptanceWriteOutcome::Committed(committed) = service
        .begin_acceptance_write(&ticket)
        .unwrap()
        .persist(&mut store)
    else {
        panic!("commit")
    };
    service.publish_acceptance(&committed).unwrap();
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 4, 10)
            .unwrap()
            .messages
            .iter()
            .map(|row| row.envelope.id.as_str())
            .collect::<Vec<_>>(),
        ["new"]
    );
}
/// A joined ambiguous result retains the same source and reservation. It cannot
/// be cancelled as unsent or issued concurrently, and successful reconciliation
/// uses the original occurrence rather than allocating a new send identity.
#[test]
fn ambiguous_write_is_not_unaccepted_and_reconciles_original_once() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "original"),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let original_attempt = service.acceptance_write_attempt(&ticket).unwrap();
    assert!(service.cancel_acceptance(&ticket).is_err());
    let mut store = Store {
        calls: 0,
        uncertain: true,
    };
    let MessageAcceptanceWriteOutcome::Unknown(unknown) = write.persist(&mut store) else {
        panic!("ambiguous")
    };
    assert!(service.begin_acceptance_write(&ticket).is_err());
    assert!(service.cancel_acceptance(&ticket).is_err());
    struct StaleJoin;
    impl MessageAcceptanceJoin for StaleJoin {
        /// A stale attempt must be refused before the join adapter is consulted.
        fn qualify_joined(&mut self, _: &MessageAcceptanceAttempt) -> Result<()> {
            panic!("stale attempt reached join qualifier")
        }
    }
    let retry = service.retry_acceptance_write(unknown).unwrap();
    assert!(
        !service
            .acceptance_write_attempt(&ticket)
            .unwrap()
            .same_attempt(&original_attempt)
    );
    assert!(
        service
            .recover_joined_acceptance_write(&original_attempt, &mut StaleJoin)
            .is_err()
    );
    let MessageAcceptanceWriteOutcome::Committed(committed) = retry.persist(&mut store) else {
        panic!("reconcile")
    };
    service.publish_acceptance(&committed).unwrap();
    assert_eq!(service.snapshot_state().unwrap().accepted_messages.len(), 1);
    assert_eq!(store.calls, 2);
}

/// Same public IDs or identical snapshot data cannot manufacture an allocation
/// belonging to another service. A released refusal cannot cancel a subsequent
/// original, and an unissued cancellation cannot later be sent to storage.
#[test]
fn foreign_and_stale_capabilities_cannot_publish_or_release_another_original() {
    struct Refuse;
    impl MessageAcceptanceStore for Refuse {
        /// Qualifies absence explicitly, without classifying a generic error.
        fn commit(
            &mut self,
            _: &MessageAcceptanceWrite,
        ) -> std::result::Result<(), MessageAcceptanceStoreError> {
            Err(MessageAcceptanceStoreError::NotCommitted(
                MessageError::new(MessageErrorKind::Capacity, "full"),
            ))
        }
    }
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let first = service
        .prepare_acceptance(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "first"),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    let mut foreign =
        MessageService::from_snapshot_state(&service.snapshot_state().unwrap()).unwrap();
    assert!(foreign.begin_acceptance_write(&first).is_err());
    assert!(foreign.cancel_acceptance(&first).is_err());
    let MessageAcceptanceWriteOutcome::NotCommitted(refused) = service
        .begin_acceptance_write(&first)
        .unwrap()
        .persist(&mut Refuse)
    else {
        panic!("refusal")
    };
    service.release_rejected_acceptance(&refused).unwrap();
    let second = service
        .prepare_acceptance(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "second"),
            MessageScope::Session,
            2,
            4096,
            None,
        )
        .unwrap();
    assert!(second.sequence() > first.sequence());
    assert!(service.release_rejected_acceptance(&refused).is_err());
    assert!(service.begin_acceptance_write(&first).is_err());
    service.cancel_acceptance(&second).unwrap();
    assert!(service.begin_acceptance_write(&second).is_err());
}

/// Reservation failures must precede allocator, history or acceptance mutation.
/// Pending capacity remains charged through issued storage, and ordinary new
/// sends cannot bypass the unpublished owner while accepted duplicates stay inert.
#[test]
fn budget_and_invalid_input_refusal_leave_source_unchanged_and_charge_inflight() {
    let limits = super::super::super::pending_limits::PendingDeliveryLimits {
        messages: 1,
        ..Default::default()
    };
    let mut service = MessageService::with_pending_limits(1, 4096, limits).unwrap();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let before = service.snapshot_state().unwrap();
    assert!(
        service
            .prepare_acceptance(
                &sender.agent_id,
                mail(&sender, &target.agent_id, "too-small-budget"),
                MessageScope::Session,
                1,
                0,
                None
            )
            .is_err()
    );
    let mut invalid = mail(&sender, &target.agent_id, "");
    invalid.time.clear();
    assert!(
        service
            .prepare_acceptance(
                &sender.agent_id,
                invalid,
                MessageScope::Session,
                1,
                4096,
                None
            )
            .is_err()
    );
    assert_eq!(service.snapshot_state().unwrap(), before);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "original"),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    let reserved = service.snapshot_state().unwrap();
    assert!(
        service
            .prepare_acceptance(
                &sender.agent_id,
                mail(&sender, &target.agent_id, "another"),
                MessageScope::Session,
                2,
                4096,
                None
            )
            .is_err()
    );
    assert!(
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(&sender, &target.agent_id, "bypass"),
                MessageScope::Session,
                2
            )
            .is_err()
    );
    assert_eq!(service.snapshot_state().unwrap(), reserved);
    let mut store = Store {
        calls: 0,
        uncertain: false,
    };
    let MessageAcceptanceWriteOutcome::Committed(committed) = service
        .begin_acceptance_write(&ticket)
        .unwrap()
        .persist(&mut store)
    else {
        panic!("commit")
    };
    let receipt = service.publish_acceptance(&committed).unwrap().0;
    assert!(
        service
            .prepare_acceptance(
                &sender.agent_id,
                mail(&sender, &target.agent_id, "capacity-still-held"),
                MessageScope::Session,
                3,
                4096,
                None
            )
            .is_err()
    );
    service
        .subscribe_from_retained_start(&target.agent_id)
        .unwrap();
    service.receive_subscribed(&target.agent_id, 4, 10).unwrap();
    service
        .advance_subscription(&target.agent_id, receipt.sequence)
        .unwrap();
    let next = service
        .prepare_acceptance(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "after-drain"),
            MessageScope::Session,
            5,
            4096,
            None,
        )
        .unwrap();
    // Late positive proof returns only its fact, not the evicted original bytes.
    assert_eq!(service.publish_acceptance(&committed).unwrap().0, receipt);
    assert!(
        !service
            .snapshot_state()
            .unwrap()
            .accepted_messages
            .iter()
            .any(|row| row.envelope.id == "original")
    );
    service.cancel_acceptance(&next).unwrap();
}

/// Retirement during storage does not relabel committed source as belonging to
/// a replacement. Original recipients stay frozen, late/reused registrations
/// cannot acquire mail, and an ended sender does not lose canonical provenance.
#[test]
fn retired_source_and_reused_target_keep_original_facts_without_rebinding() {
    for fresh in [false, true] {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "sender", vec![]);
        let target = service.register_agent(None, None, "target", vec![]);
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                mail(&sender, &target.agent_id, "original"),
                MessageScope::Session,
                1,
                4096,
                None,
            )
            .unwrap();
        let write = service.begin_acceptance_write(&ticket).unwrap();
        if fresh {
            service
                .retire_fresh_observational_identity(&sender.agent_id)
                .unwrap();
        } else {
            service.retire_agent_identity(&sender.agent_id);
        }
        service.retire_agent_identity(&target.agent_id);
        service.ensure_agent_identity(target.clone(), 2).unwrap();
        MessageService::from_snapshot_state(&service.snapshot_state().unwrap()).unwrap();
        let mut store = Store {
            calls: 0,
            uncertain: false,
        };
        let MessageAcceptanceWriteOutcome::Committed(committed) = write.persist(&mut store) else {
            panic!("commit")
        };
        service.publish_acceptance(&committed).unwrap();
        assert!(service.registered_identity(&sender.agent_id).is_none());
        service
            .subscribe_from_retained_start(&target.agent_id)
            .unwrap();
        assert!(
            service
                .receive_subscribed(&target.agent_id, 3, 10)
                .unwrap()
                .messages
                .is_empty()
        );
        let snapshot = service.snapshot_state().unwrap();
        assert_eq!(snapshot.accepted_messages.len(), 1);
        assert_eq!(
            snapshot.accepted_messages[0].envelope.sender.agent_id,
            sender.agent_id.as_str()
        );
        MessageService::from_snapshot_state(&snapshot).unwrap();
    }
}

/// A real separate thread may serialize and commit the frozen record while the
/// actor updates discovery. Borrowed encoding must retain original metadata,
/// raw payload/extension text, audience and eligible recipient intervals.
#[test]
fn off_actor_record_encoding_preserves_original_input_and_optional_proof() {
    struct Capture(
        Option<serde_json::Value>,
        std::sync::Arc<std::sync::Barrier>,
    );
    impl MessageAcceptanceStore for Capture {
        /// Captures the semantic record as a deterministic serialization probe.
        fn commit(
            &mut self,
            record: &MessageAcceptanceWrite,
        ) -> std::result::Result<(), MessageAcceptanceStoreError> {
            self.1.wait();
            self.1.wait();
            self.0 = Some(serde_json::to_value(record).unwrap());
            Ok(())
        }
    }
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let mut input = mail(&sender, &target.agent_id, "original");
    input.payload = "raw\r\nbody\n".into();
    input.extension_fields = vec![("opaque".into(), "{\"raw\":true}".into())];
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            input.clone(),
            MessageScope::Session,
            17,
            4096,
            Some(MessageAcceptanceReservation {
                metadata_bytes: 4096,
                recipients: 1,
                handoff_bytes_per_recipient: 0,
            }),
        )
        .unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let worker_gate = gate.clone();
    let worker = std::thread::spawn(move || {
        let mut store = Capture(None, worker_gate);
        let outcome = write.persist(&mut store);
        (outcome, store.0.unwrap())
    });
    gate.wait();
    service
        .update_agent_objective(&sender.agent_id, Some("changed after prepare"), 18)
        .unwrap();
    assert!(service.receive_for(&target.agent_id, 18).is_empty());
    gate.wait();
    let (outcome, json) = worker.join().unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["accepted_at_ms"], 17);
    assert_eq!(json["envelope"]["payload"], input.payload);
    assert_eq!(
        json["envelope"]["sender"]["objective"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["envelope"]["extension_fields"][0]["value_json"],
        "{\"raw\":true}"
    );
    assert_eq!(
        json["recipient_bindings"][0]["recipient"],
        target.agent_id.as_str()
    );
    let MessageAcceptanceWriteOutcome::Committed(committed) = outcome else {
        panic!("commit")
    };
    let (delivery, proof) = service.publish_acceptance(&committed).unwrap();
    assert_eq!(delivery.sequence, ticket.sequence());
    assert_eq!(proof.unwrap().sequence(), delivery.sequence);
    assert_eq!(service.receive_for(&target.agent_id, 19)[0], input);
    MessageService::from_snapshot_state(&service.snapshot_state().unwrap()).unwrap();
}

/// Eligible fanout is frozen before storage and cannot widen when another
/// matching worker registers. A request proof returned after publication refers
/// only to the original eligible registration, not current selector membership.
#[test]
fn prepared_fanout_never_acquires_a_late_matching_registration() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let original = service.register_agent(None, None, "worker", vec![]);
    let mut input = mail(&sender, &original.agent_id, "fanout");
    input.recipient = Recipient::Role("worker".into());
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            input,
            MessageScope::Session,
            1,
            4096,
            Some(MessageAcceptanceReservation {
                metadata_bytes: 4096,
                recipients: 1,
                handoff_bytes_per_recipient: 0,
            }),
        )
        .unwrap();
    assert!(
        service
            .acceptance_proof(&sender.agent_id, ticket.sequence())
            .is_none()
    );
    let late = service.register_agent(None, None, "worker", vec![]);
    for target in [&original, &late] {
        service
            .subscribe_from_retained_start(&target.agent_id)
            .unwrap();
    }
    assert!(
        service
            .advance_subscription(&original.agent_id, ticket.sequence())
            .is_err()
    );
    let mut store = Store {
        calls: 0,
        uncertain: false,
    };
    let MessageAcceptanceWriteOutcome::Committed(commit) = service
        .begin_acceptance_write(&ticket)
        .unwrap()
        .persist(&mut store)
    else {
        panic!("commit")
    };
    let (delivery, proof) = service.publish_acceptance(&commit).unwrap();
    assert_eq!(delivery.queued_recipients, 1);
    assert_eq!(
        proof.unwrap().recipients().collect::<Vec<_>>(),
        [&original.agent_id]
    );
    assert_eq!(
        service
            .receive_subscribed(&original.agent_id, 2, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert!(
        service
            .receive_subscribed(&late.agent_id, 2, 10)
            .unwrap()
            .messages
            .is_empty()
    );
}

/// Storage adapters may stream an expanded JSON payload through small buffers.
/// The lower encoding borrows original bytes and never requires a full encoded
/// frame allocation; a partial-consumption writer must receive the whole record.
#[test]
fn borrowed_record_streams_escaped_payload_through_bounded_write_chunks() {
    struct Sink {
        bytes: usize,
        max: usize,
    }
    impl std::io::Write for Sink {
        /// Accepts only one small chunk, forcing correct write-all continuation.
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let used = bytes.len().min(8192);
            self.bytes += used;
            self.max = self.max.max(used);
            Ok(used)
        }
        /// No hidden I/O is deferred after a successful fixture write.
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    struct Stream(Sink);
    impl MessageAcceptanceStore for Stream {
        /// Serializes directly into a bounded-consumption streaming sink.
        fn commit(
            &mut self,
            record: &MessageAcceptanceWrite,
        ) -> std::result::Result<(), MessageAcceptanceStoreError> {
            serde_json::to_writer(&mut self.0, record).unwrap();
            Ok(())
        }
    }
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let mut input = mail(&sender, &target.agent_id, "stream");
    input.payload = "\"\n".repeat(120_000);
    let payload_bytes = input.payload.len();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            input,
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    let mut store = Stream(Sink { bytes: 0, max: 0 });
    let outcome = service
        .begin_acceptance_write(&ticket)
        .unwrap()
        .persist(&mut store);
    assert!(matches!(
        outcome,
        MessageAcceptanceWriteOutcome::Committed(_)
    ));
    assert!(store.0.bytes > payload_bytes);
    assert!(store.0.max <= 8192);
    assert!(
        service
            .snapshot_state()
            .unwrap()
            .accepted_messages
            .is_empty(),
        "storage completion alone is not actor adoption"
    );
}

/// A worker can panic after a possibly committed operation and lose its linear
/// input. Actor escrow plus actual joined-thread observation must recover only
/// the same original; neither a live worker nor foreign/stale attempt qualifies.
#[test]
fn joined_worker_loss_recovers_original_without_assuming_noncommit() {
    struct Panic;
    impl MessageAcceptanceStore for Panic {
        /// Models a lost result after entering storage; absence is not proven.
        fn commit(
            &mut self,
            _: &MessageAcceptanceWrite,
        ) -> std::result::Result<(), MessageAcceptanceStoreError> {
            panic!("lost storage worker")
        }
    }
    struct Join {
        expected: MessageAcceptanceAttempt,
        observed: bool,
    }
    impl MessageAcceptanceJoin for Join {
        /// Checks the retained exact attempt and an actual observed thread join.
        fn qualify_joined(&mut self, attempt: &MessageAcceptanceAttempt) -> Result<()> {
            if self.observed && self.expected.same_attempt(attempt) {
                Ok(())
            } else {
                Err(MessageError::invalid_state("worker has not joined"))
            }
        }
    }
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "original"),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let attempt = service.acceptance_write_attempt(&ticket).unwrap();
    let mut joined = Join {
        expected: attempt.clone(),
        observed: false,
    };
    assert!(
        service
            .recover_joined_acceptance_write(&attempt, &mut joined)
            .is_err()
    );
    let worker = std::thread::spawn(move || write.persist(&mut Panic));
    assert!(worker.join().is_err());
    joined.observed = true;
    let recovered = service
        .recover_joined_acceptance_write(&attempt, &mut joined)
        .unwrap();
    assert_eq!(recovered.sequence(), ticket.sequence());
    assert!(
        service
            .recover_joined_acceptance_write(&attempt, &mut joined)
            .is_err(),
        "stale join cannot issue twice"
    );
    assert!(
        service.cancel_acceptance(&ticket).is_err(),
        "join does not prove unsent"
    );
    let mut store = Store {
        calls: 0,
        uncertain: false,
    };
    let MessageAcceptanceWriteOutcome::Committed(commit) = recovered.persist(&mut store) else {
        panic!("reconcile")
    };
    assert_eq!(
        service.publish_acceptance(&commit).unwrap().0.sequence,
        ticket.sequence()
    );
    assert_eq!(service.snapshot_state().unwrap().accepted_messages.len(), 1);
}
