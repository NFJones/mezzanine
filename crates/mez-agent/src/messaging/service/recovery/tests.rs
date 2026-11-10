//! Trusted-port controls for canonical cold reconstruction and reservation life.

use super::*;

/// Owns fixture source bytes written by an actual lower acceptance, without
/// claiming that this in-memory test port proves physical store continuity.
#[derive(Default)]
struct Store {
    sources: Vec<Vec<u8>>,
    observer: Option<MessagePayloadObserver>,
    fail_after_first: bool,
    acknowledge_first: bool,
    next_sequence: u64,
    acknowledgements_override: Option<Vec<(u32, u64)>>,
    swallow_row_error: bool,
    terminal_observations: Vec<(u32, u64)>,
    terminal_consumptions: Vec<u32>,
}
impl MessageAcceptanceStore for Store {
    fn commit(
        &mut self,
        record: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        self.sources.push(serde_json::to_vec(record).unwrap());
        Ok(())
    }
}
impl MessageStorageRecoveryStore for Store {
    fn recover(&mut self, target: &mut MessageStorageRecovery) -> Result<u64> {
        let observer = target.payload_observer()?;
        assert_eq!(observer.snapshot()?.reserved_allocations, 1);
        assert_eq!(
            target.decode_limits()?.payload_bytes,
            observer.snapshot()?.reserved_payload_bytes
        );
        self.observer = Some(observer.clone());
        for (i, bytes) in self.sources.iter().enumerate() {
            let sequence = i as u64 + 1;
            let result = target.restore_original(
                MessageStorageRecoveredRecord {
                    sequence,
                    source_sha256: [sequence as u8; 32],
                    expired_receipt: false,
                    expiry_observed_at_ms: None,
                    terminal_observations: self
                        .terminal_observations
                        .iter()
                        .map(|(index, clock)| (*index, DeliveryTerminalReason::Expired, *clock))
                        .collect(),
                    terminal_releases: Vec::new(),
                    terminal_consumptions: self.terminal_consumptions.clone(),
                    horizon_observed_at_ms: None,
                },
                self.acknowledgements_override.clone().unwrap_or_else(|| {
                    (self.acknowledge_first && i == 0)
                        .then_some((0, 20))
                        .into_iter()
                        .collect()
                }),
                |limits| {
                    assert_eq!(observer.snapshot()?.reserved_allocations, 1);
                    assert_eq!(
                        observer.snapshot()?.reserved_payload_bytes,
                        limits.payload_bytes
                    );
                    decode_message_acceptance(bytes.as_slice(), limits)
                },
            );
            if self.swallow_row_error && result.is_err() {
                break;
            }
            result?;
            assert_eq!(observer.snapshot()?.live_allocations, 0);
            if self.fail_after_first {
                return Err(unavailable());
            }
        }
        Ok(self.next_sequence)
    }
}

/// Trusted-port consumption settles only actual original terminal slots and
/// supplies no elapsed-horizon clock. Product controls separately qualify real
/// h1/native evidence; this fixture tests the lower reconstruction contract.
#[test]
fn recovery_sender_consumption_does_not_mint_horizon_observation() {
    let (mut store, _) = originals();
    store.terminal_observations = vec![(0, 111)];
    store.terminal_consumptions = vec![0];
    let recovered = MessageService::recover_storage_custody(
        &MessageStorageScope::default(),
        Default::default(),
        1000,
        1024 * 1024,
        &mut store,
    )
    .unwrap();
    assert!(recovered.pending.is_empty());
    let row = &recovered.queued_by_sequence[&2];
    assert_eq!(*row.horizon_observed_at_ms.lock().unwrap(), None);
    assert!(!row.acknowledged.lock().unwrap()[0]);
    let terminal = row.terminal.lock().unwrap()[0].unwrap();
    assert!(terminal.released);
    assert_eq!(terminal.observed_at_ms, 111);
}

/// Duplicate or out-of-range consumed slots and consumption without an actual
/// terminal before-image refuse the entire candidate, freeing its decoder.
#[test]
fn recovery_sender_consumption_refuses_duplicate_or_missing_terminal_slots() {
    for (terminals, consumed) in [
        (vec![], vec![0]),
        (vec![(0, 111)], vec![0, 0]),
        (vec![(0, 111)], vec![1]),
    ] {
        let (mut store, _) = originals();
        store.terminal_observations = terminals;
        store.terminal_consumptions = consumed;
        assert!(
            MessageService::recover_storage_custody(
                &MessageStorageScope::default(),
                Default::default(),
                1000,
                1024 * 1024,
                &mut store
            )
            .is_err()
        );
        assert_eq!(
            store
                .observer
                .as_ref()
                .unwrap()
                .snapshot()
                .unwrap()
                .reserved_payload_bytes,
            0
        );
    }
}

/// A test-only trusted port supplies actual lower-encoded original metadata;
/// product tests separately prove physical retirement and original-store proof.
struct RetiredPort {
    source: Vec<u8>,
    acks: Vec<(u32, u64)>,
    swallow: bool,
    observer: Option<MessagePayloadObserver>,
}

impl MessageStorageRecoveryStore for RetiredPort {
    fn recover(&mut self, target: &mut MessageStorageRecovery) -> Result<u64> {
        self.observer = Some(target.payload_observer()?);
        let result = target.restore_retired_acknowledged(1, self.acks.clone(), |limits| {
            crate::messaging::decode_message_acceptance_metadata(self.source.as_slice(), limits)
        });
        if self.swallow {
            assert!(result.is_err());
            // A failed port cannot later publish even if it ignores the error;
            // the reader must not run again after sticky candidate refusal.
            assert!(
                target
                    .restore_retired_acknowledged(2, [], |_| panic!("failed port read again"))
                    .is_err()
            );
        } else {
            result?;
        }
        Ok(2)
    }
}

fn retired_port() -> RetiredPort {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "raw-sender", vec![]);
    let target = service.register_agent(None, None, "raw-target", vec![]);
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    let envelope = Envelope {
        protocol: MMP_PROTOCOL,
        id: "retired-original".into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id),
        correlation_id: None,
        ttl_ms: Some(100),
        content_type: "text/plain".into(),
        payload: "actual original payload".into(),
        extension_fields: vec![],
    };
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope,
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let write = service
        .begin_storage_acceptance_write(&ticket, &scope)
        .unwrap();
    let mut store = Store::default();
    assert!(matches!(
        write.persist(&mut store),
        MessageAcceptanceWriteOutcome::Committed(_)
    ));
    RetiredPort {
        source: store.sources.remove(0),
        acks: vec![(0, 10)],
        swallow: false,
        observer: None,
    }
}

/// Positive retired metadata advances only checked history, recreating no body,
/// identity/idempotency entry, offer or delivery obligation. Earlier offer clocks
/// remain valid rather than being silently raised to the acceptance clock.
#[test]
fn recovery_retired_acknowledged_never_recreates_mail() {
    for clock in [0, 10, 110] {
        let mut port = retired_port();
        port.acks = vec![(0, clock)];
        let recovered = MessageService::recover_storage_custody(
            &MessageStorageScope::default(),
            Default::default(),
            16,
            1024 * 1024,
            &mut port,
        )
        .unwrap();
        assert_eq!(recovered.next_sequence, 2);
        assert!(recovered.pending.is_empty());
        assert!(recovered.queued_by_sequence.is_empty());
        assert!(recovered.accepted_messages.is_empty());
        assert_eq!(
            port.observer.as_ref().unwrap().snapshot().unwrap(),
            MessagePayloadUsage::default()
        );
    }
}

/// Every exact positive slot must be present exactly once and within original
/// TTL. Native attribution and ignored local errors still refuse in full.
#[test]
fn recovery_retired_acknowledged_rejects_bad_slots_and_native_or_swallowed_error() {
    for (acks, native, swallow) in [
        (vec![], false, false),
        (vec![(0, 10), (0, 10)], false, false),
        (vec![(1, 10)], false, false),
        (vec![(0, 111)], false, false),
        (vec![(0, 10)], true, false),
        (vec![], false, true),
    ] {
        let mut port = retired_port();
        port.acks = acks;
        port.swallow = swallow;
        if native {
            port.source = originals().0.sources.remove(0);
        }
        assert!(
            MessageService::recover_storage_custody(
                &MessageStorageScope::default(),
                Default::default(),
                16,
                1024 * 1024,
                &mut port,
            )
            .is_err()
        );
        assert_eq!(
            port.observer.as_ref().unwrap().snapshot().unwrap(),
            MessagePayloadUsage::default()
        );
    }
}

/// Makes two actual source2 acceptances with the same frozen native recipient.
/// Their combined payload exceeds the single-payload cache budget.
fn originals() -> (Store, MessageNativeOwner) {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "native", vec![]);
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    let registration = service
        .capture_sender_registration(&target.agent_id)
        .unwrap();
    let owner = service
        .bind_native_conversation(
            &registration,
            "original",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    let mut store = Store {
        next_sequence: 3,
        ..Default::default()
    };
    for i in 1..=2 {
        let envelope = Envelope {
            protocol: MMP_PROTOCOL,
            id: format!("original-{i}"),
            message_type: "send".into(),
            time: "original".into(),
            sender: sender.clone(),
            recipient: Recipient::Agent(target.agent_id.clone()),
            correlation_id: Some("correlation".into()),
            ttl_ms: Some(100),
            content_type: "text/plain".into(),
            payload: "x".repeat(700_000),
            extension_fields: vec![],
        };
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                envelope,
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        let write = service
            .begin_storage_acceptance_write(&ticket, &scope)
            .unwrap();
        let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(&mut store) else {
            panic!("source must commit")
        };
        service.publish_acceptance(&commit).unwrap();
        service.release_stored_payload(i, &scope).unwrap();
    }
    (store, owner)
}

/// Every original becomes cold metadata, including immutable native attribution,
/// while only explicit ACK slots settle. Historical custody must not revive an
/// endpoint or become a new live acceptance event in the restored runtime.
#[test]
fn recovery_preserves_cold_originals_slots_native_ownership_and_allocator() {
    let (mut store, owner) = originals();
    store.acknowledge_first = true;
    let scope = MessageStorageScope::default();
    let mut recovered = MessageService::recover_storage_custody(
        &scope,
        Default::default(),
        1000,
        1024 * 1024,
        &mut store,
    )
    .unwrap();
    assert_eq!(recovered.next_sequence, 3);
    assert_eq!(recovered.acceptance_history_through, 2);
    assert!(recovered.registered.is_empty() && recovered.native_bindings.is_empty());
    assert_eq!(recovered.pending.len(), 1);
    assert!(recovered.pending.contains_key(&2));
    let row = &recovered.pending[&2];
    assert_eq!(row.envelope.payload_bytes, 700_000);
    assert_eq!(row.envelope.resident_bytes(), 0);
    assert!(row.envelope.resident().is_err());
    assert_eq!(row.envelope.storage_digest(&scope).unwrap(), [2; 32]);
    assert_eq!(row.accepted_at_ms, 10);
    assert_eq!(row.envelope.ttl_ms, Some(100));
    assert_eq!(row.envelope.correlation_id.as_deref(), Some("correlation"));
    assert_eq!(
        row.native_bindings.as_ref().unwrap().recipients[0].as_ref(),
        Some(&owner)
    );
    assert_eq!(*row.offered_at_ms.lock().unwrap(), vec![None]);
    let usage = recovered.storage_usage().unwrap().unwrap();
    assert_eq!(usage.pending_payload_bytes, 700_000);
    assert_eq!(usage.cache, MessagePayloadUsage::default());
    let original_sender = row.envelope.sender.agent_id.clone();
    let original_target = row.recipient_bindings[0].0.clone();
    let new = recovered.register_agent(None, None, "fresh", vec![]);
    assert_ne!(new.agent_id, original_sender);
    assert_ne!(new.agent_id, original_target);
}

/// A late trusted-store failure must release every reservation and withhold the
/// partly reconstructed service; a later qualified attempt can read the originals.
#[test]
fn failed_recovery_withholds_candidate_and_releases_decode_reservations() {
    let (mut store, _) = originals();
    store.fail_after_first = true;
    let scope = MessageStorageScope::default();
    assert!(
        MessageService::recover_storage_custody(
            &scope,
            Default::default(),
            1000,
            1024 * 1024,
            &mut store
        )
        .is_err()
    );
    assert_eq!(
        store.observer.as_ref().unwrap().snapshot().unwrap(),
        MessagePayloadUsage::default()
    );
    store.fail_after_first = false;
    let recovered = MessageService::recover_storage_custody(
        &scope,
        Default::default(),
        1000,
        1024 * 1024,
        &mut store,
    )
    .unwrap();
    assert_eq!(recovered.pending.len(), 2);
}

/// Destination custody caps cannot be enlarged by a verified original. Recovery
/// fails as a whole instead of evicting one unsettled original to fit the policy.
#[test]
fn narrowed_pending_policy_refuses_without_dropping_originals() {
    let (mut store, _) = originals();
    let limits = MessageStorageLimits {
        messages: 1,
        ..Default::default()
    };
    let error = MessageService::recover_storage_custody(
        &MessageStorageScope::default(),
        limits,
        1000,
        1024 * 1024,
        &mut store,
    )
    .unwrap_err();
    assert_eq!(error.kind(), MessageErrorKind::Capacity);
    assert_eq!(store.sources.len(), 2);
    assert_eq!(
        store.observer.as_ref().unwrap().snapshot().unwrap(),
        MessagePayloadUsage::default()
    );
}

/// The persistent allocator floor must cover every installed original. A port
/// cannot return an old floor just because the final replay projection evicted
/// the first decoded row or there is no current registration to expose it.
#[test]
fn allocator_below_original_history_withholds_the_candidate() {
    let (mut store, _) = originals();
    store.next_sequence = 2;
    assert!(
        MessageService::recover_storage_custody(
            &MessageStorageScope::default(),
            Default::default(),
            1,
            1024 * 1024,
            &mut store
        )
        .is_err()
    );
    assert_eq!(
        store.observer.as_ref().unwrap().snapshot().unwrap(),
        MessagePayloadUsage::default()
    );
}

/// Invalid original slot/clock evidence must poison the entire candidate even
/// if a faulty trusted adapter accidentally swallows the immediate row error.
/// None of these claims may settle mail or publish a partial recovered mailbox.
#[test]
fn invalid_ack_claims_poison_recovery_even_when_the_port_swallows_the_error() {
    let (mut store, _) = originals();
    store.swallow_row_error = true;
    for acknowledgements in [vec![(0, 20), (0, 20)], vec![(1, 20)], vec![(0, 111)]] {
        store.acknowledgements_override = Some(acknowledgements);
        assert!(
            MessageService::recover_storage_custody(
                &MessageStorageScope::default(),
                Default::default(),
                1000,
                1024 * 1024,
                &mut store
            )
            .is_err()
        );
        assert_eq!(
            store.observer.as_ref().unwrap().snapshot().unwrap(),
            MessagePayloadUsage::default()
        );
    }
}

/// Incremental reconstruction counters enforce the independent payload, slot
/// and per-recipient bounds without allocating a new full recipient index for
/// every original. Every refusal leaves the stored fixture sources intact.
#[test]
fn recovery_honors_each_independent_pending_and_recipient_ceiling() {
    let (mut store, _) = originals();
    for limits in [
        MessageStorageLimits {
            payload_bytes: 1024 * 1024,
            ..Default::default()
        },
        MessageStorageLimits {
            recipient_obligations: 1,
            ..Default::default()
        },
        MessageStorageLimits {
            per_recipient: 1,
            ..Default::default()
        },
        MessageStorageLimits {
            per_recipient_payload_bytes: 1024 * 1024,
            ..Default::default()
        },
    ] {
        let error = MessageService::recover_storage_custody(
            &MessageStorageScope::default(),
            limits,
            1000,
            1024 * 1024,
            &mut store,
        )
        .unwrap_err();
        assert_eq!(error.kind(), MessageErrorKind::Capacity);
        assert_eq!(store.sources.len(), 2);
        assert_eq!(
            store.observer.as_ref().unwrap().snapshot().unwrap(),
            MessagePayloadUsage::default()
        );
    }
}

/// Actual endpoint clocks may roll back before original acceptance. Preserve
/// trusted offered-zero and earlier selection clocks rather than inventing a
/// minimum clock, while keeping exact positive slot and original TTL checks.
#[test]
fn genuine_earlier_offer_clocks_and_offered_zero_survive_reconstruction() {
    let (mut store, _) = originals();
    for clock in [9, 0] {
        store.acknowledgements_override = Some(vec![(0, clock)]);
        let recovered = MessageService::recover_storage_custody(
            &MessageStorageScope::default(),
            Default::default(),
            1000,
            1024 * 1024,
            &mut store,
        )
        .unwrap();
        assert!(recovered.pending.is_empty());
        assert_eq!(
            *recovered.queued_by_sequence[&2]
                .offered_at_ms
                .lock()
                .unwrap(),
            vec![Some(clock)]
        );
        assert_eq!(
            *recovered.queued_by_sequence[&2]
                .acknowledged
                .lock()
                .unwrap(),
            vec![true]
        );
    }
}
