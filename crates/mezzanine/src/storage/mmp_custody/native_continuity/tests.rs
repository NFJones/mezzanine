//! Native fact framing, capacity and allocator controls on real SQLite stores.

use mez_agent::messaging::{
    Envelope, MessageAcceptanceDecodeLimits, MessageAcceptanceWriteOutcome,
    MessageAcknowledgementWriteOutcome, MessageNativeContinuityRequest, MessageNativeIncarnation,
    MessageScope, MessageService, MessageStorageScope, Recipient,
};

use super::*;

/// Owns only a generated native-journal fixture directory.
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-native-journal-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
    fn open(&self) -> MmpCustodyStore {
        MmpCustodyStore::open(&self.0, "native-original").unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Captures actual lower registrations; only corruption tests modify DTO bytes.
fn capture(capabilities: Vec<String>) -> (MessageService, MessageNativeContinuityRequest) {
    capture_after(capabilities, 0)
}

/// Selects a distinct actual agent ID when two independent fixture runtimes
/// contribute facts to the same test store; default allocators both start at 1.
fn capture_after(
    capabilities: Vec<String>,
    preceding: usize,
) -> (MessageService, MessageNativeContinuityRequest) {
    let mut service = MessageService::default();
    for _ in 0..preceding {
        service.register_agent(None, None, "other", vec![]);
    }
    let identity = service.register_agent(None, None, "native", capabilities);
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    let registration = service
        .capture_sender_registration(&identity.agent_id)
        .unwrap();
    service
        .bind_native_conversation(
            &registration,
            "native-parent",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    let request = service
        .prepare_native_continuity(&scope, &identity.agent_id)
        .unwrap();
    (service, request)
}

fn limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}

/// Matching store UUID/owner and even the same physical database cannot make a
/// different connection's transaction cover this reader's SELECTs. Refuse the
/// mismatch before reading an absent or present native row outside its snapshot.
#[test]
fn native_reader_requires_its_actual_sqlite_snapshot_connection() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let other = fixture.open();
    let transaction = other.connection.unchecked_transaction().unwrap();
    let key = record_key("native-agent");
    assert!(
        store
            .read_native_continuity_in_snapshot(&transaction, &key)
            .is_err()
    );
    let own = store.connection.unchecked_transaction().unwrap();
    assert!(
        store
            .read_native_continuity_in_snapshot(&own, &key)
            .unwrap()
            .is_none()
    );
}

/// Checksums and valid JSON alone cannot qualify a native observation. Every
/// malformed record must refuse both individual read and whole-store inventory,
/// including the preimage check that precedes any attempted replacement.
#[test]
fn native_record_corruption_cannot_be_read_inventoried_or_overwritten() {
    let (_, request) = capture(vec![]);
    let write = request.write();
    let snapshot = write.snapshot();
    let key = record_key(&snapshot.native.agent_id);
    for fault in [
        "version",
        "allocation",
        "agent-key",
        "floor",
        "owner",
        "objective",
        "unknown",
    ] {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let mut data = serde_json::to_value(RecordRef {
            format_version: 1,
            store_id: &store.store_id,
            snapshot,
        })
        .unwrap();
        match fault {
            "version" => data["format_version"] = 2.into(),
            "allocation" => data["store_id"][0] = ((store.store_id[0] ^ 1) as u64).into(),
            "agent-key" => {
                data["snapshot"]["native"]["agent_id"] = "foreign-agent".into();
                data["snapshot"]["identity"]["agent_id"] = "foreign-agent".into();
            }
            "floor" => data["snapshot"]["registered_after_sequence"] = 1.into(),
            "owner" => data["snapshot"]["native"]["owner"]["incarnation"] = "0".repeat(64).into(),
            "objective" => data["snapshot"]["identity"]["objective"] = "mutable".into(),
            "unknown" => data["unknown"] = true.into(),
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&data).unwrap();
        store
            .commit(
                0,
                &[CustodyWrite {
                    key: &key,
                    previous: None,
                    value: Some(&bytes),
                }],
            )
            .unwrap();
        assert!(
            store
                .read_native_continuity(&snapshot.native.agent_id)
                .is_err(),
            "{fault}"
        );
        assert!(store.inventory_custody(limits()).is_err(), "{fault}");
        assert!(
            store
                .native_continuity_plan(&snapshot.native.agent_id)
                .is_err(),
            "{fault}"
        );
        assert_eq!(store.read(&key).unwrap().unwrap(), bytes);
    }
}

/// Many tiny JSON strings fit the encoded-row limit but exceed the conservative
/// decoded-container budget. Structural preflight must stop before owned serde
/// creates those entries, and a valid checksum must not hide the excess.
#[test]
fn native_record_structural_metadata_is_bounded_before_owned_decode() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (_, request) = capture(vec![]);
    let write = request.write();
    let snapshot = write.snapshot();
    let mut data = serde_json::to_value(RecordRef {
        format_version: 1,
        store_id: &store.store_id,
        snapshot,
    })
    .unwrap();
    data["snapshot"]["identity"]["capabilities"] = serde_json::json!(vec!["small"; 5000]);
    let bytes = serde_json::to_vec(&data).unwrap();
    assert!(bytes.len() < MAX_RECORD);
    assert!(preflight(&bytes).is_err());
    let key = record_key(&snapshot.native.agent_id);
    store
        .commit(
            0,
            &[CustodyWrite {
                key: &key,
                previous: None,
                value: Some(&bytes),
            }],
        )
        .unwrap();
    assert!(
        store
            .read_native_continuity(&snapshot.native.agent_id)
            .is_err()
    );
    assert!(store.inventory_custody(limits()).is_err());
}

/// A real actual-token writer can exceed the encoded ceiling through escaping
/// even when lower metadata fits. Refusal must precede any record, generation or
/// watermark change; it must not publish a live completion either.
#[test]
fn oversized_native_write_preserves_the_original_store_position() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (service, request) = capture(vec!["\"".repeat(130 * 1024)]);
    let write = request.write();
    let id = write.snapshot().native.agent_id.clone();
    let plan = store.native_continuity_plan(&id).unwrap();
    assert!(
        write
            .persist(&mut store.native_continuity_port(&plan))
            .is_err()
    );
    let state = store.current_state().unwrap();
    assert_eq!((state.generation, state.watermark), (0, 0));
    assert!(store.read_native_continuity(&id).unwrap().is_none());
    assert!(!service.native_continuity_is_recorded(&mez_core::ids::AgentId::opaque(&id).unwrap()));
}

/// Only the original last batch is reconcilable. A stale preimage/generation
/// must refuse after another row commits, even if native data remains equal.
#[test]
fn native_plan_reconciles_only_its_exact_original_last_batch() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (_, request) = capture(vec![]);
    let id = request.write().snapshot().native.agent_id.clone();
    let plan = store.native_continuity_plan(&id).unwrap();
    request
        .write()
        .persist(&mut store.native_continuity_port(&plan))
        .unwrap();
    request
        .write()
        .persist(&mut store.native_continuity_port(&plan))
        .unwrap();
    plan.validate_settled(&store, 0).unwrap();
    assert_eq!(store.current_state().unwrap().generation, 1);
    let stale = store.native_continuity_plan(&id).unwrap();
    store
        .commit(
            1,
            &[CustodyWrite {
                key: "later",
                previous: None,
                value: Some(b"later"),
            }],
        )
        .unwrap();
    assert!(
        request
            .write()
            .persist(&mut store.native_continuity_port(&plan))
            .is_err()
    );
    assert!(
        request
            .write()
            .persist(&mut store.native_continuity_port(&stale))
            .is_err()
    );
    assert!(plan.validate_settled(&store, 0).is_err());
    assert_eq!(store.current_state().unwrap().generation, 2);
}

/// The sequence watermark uses the full u64 domain independently of SQLite's
/// signed generation. Removing the last native observation cannot release its
/// floor, and reopening must preserve exhaustion without inventing messages.
#[test]
fn native_floor_full_u64_range_survives_row_removal_and_reopen() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (_, request) = capture(vec![]);
    let mut snapshot = request.write().snapshot().clone();
    snapshot.registered_after_sequence = u64::MAX;
    let key = record_key(&snapshot.native.agent_id);
    let bytes = serde_json::to_vec(&RecordRef {
        format_version: 1,
        store_id: &store.store_id,
        snapshot: &snapshot,
    })
    .unwrap();
    let transaction = store.connection.unchecked_transaction().unwrap();
    store
        .commit_in_transaction_with_floor(
            &transaction,
            0,
            &[CustodyWrite {
                key: &key,
                previous: None,
                value: Some(&bytes),
            }],
            u64::MAX,
        )
        .unwrap();
    store.finish_transaction(transaction).unwrap();
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.sequence_watermark, u64::MAX);
    assert!(inventory.occurrences.is_empty());
    let digest = Sha256::digest(&bytes);
    store
        .commit(
            1,
            &[CustodyWrite {
                key: &key,
                previous: Some(&digest),
                value: None,
            }],
        )
        .unwrap();
    drop(store);
    let mut store = fixture.open();
    assert_eq!(
        store
            .inventory_custody(limits())
            .unwrap()
            .sequence_watermark,
        u64::MAX
    );
    assert!(
        store
            .commit(
                2,
                &[CustodyWrite {
                    key: "a1:0000000000000001:stage",
                    previous: None,
                    value: Some(b"reused"),
                }]
            )
            .is_err()
    );
}

/// Persists a real accepted original and source-bound positive ACK. The native
/// writer under test has no authority to change either canonical record family.
fn seed_accepted_original_and_ack(store: &mut MmpCustodyStore) {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "protected-original".into(),
                message_type: "send".into(),
                time: "fixture".into(),
                sender: sender.clone(),
                recipient: Recipient::Agent(target.agent_id.clone()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain".into(),
                payload: "protected original payload".repeat(100),
                extension_fields: vec![],
            },
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let plan = store.acceptance_plan(ticket.sequence()).unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) = service
        .begin_acceptance_write(&ticket)
        .unwrap()
        .persist(&mut store.acceptance_port(&plan))
    else {
        panic!("actual source required")
    };
    service.publish_acceptance(&commit).unwrap();
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 20, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    let plan = store.acknowledgement_plan().unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) = service
        .begin_acknowledgement_write(&ticket)
        .unwrap()
        .persist(&mut store.acknowledgement_port(&plan))
    else {
        panic!("actual ACK required")
    };
    service.publish_acknowledgement(&commit).unwrap();
}

/// Captures every fixture row including the stored checksum, independently of
/// the production fingerprint. Refusal must roll all SQL effects back exactly.
fn rows(store: &MmpCustodyStore) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    store
        .connection
        .prepare("SELECT key,value,digest FROM records ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// INSERT and UPDATE triggers can preserve the intended n1 postimage and state
/// while deleting or rechecksumming unrelated accepted chunks, ACKs or native
/// facts. Such effects must roll back with the attempted journal write. Removing
/// the fault must let the same original plan succeed without losing prior mail.
#[test]
fn native_write_rolls_back_trigger_changes_to_unrelated_custody() {
    let mut escaped = Vec::new();
    for event in ["INSERT", "UPDATE"] {
        for target in ["source", "ack", "native"] {
            for mutation in ["delete", "rewrite"] {
                let fixture = Fixture::new();
                let mut store = fixture.open();
                seed_accepted_original_and_ack(&mut store);
                let (_, prior) = capture(vec![]);
                let prior_id = prior.write().snapshot().native.agent_id.clone();
                let prior_plan = store.native_continuity_plan(&prior_id).unwrap();
                prior
                    .write()
                    .persist(&mut store.native_continuity_port(&prior_plan))
                    .unwrap();
                let (_, request) = capture_after(vec![], 1);
                let id = request.write().snapshot().native.agent_id.clone();
                if event == "UPDATE" {
                    let plan = store.native_continuity_plan(&id).unwrap();
                    request
                        .write()
                        .persist(&mut store.native_continuity_port(&plan))
                        .unwrap();
                }
                let plan = store.native_continuity_plan(&id).unwrap();
                let key = record_key(&id);
                let before = rows(&store);
                let state = store.current_state().unwrap();
                let victim = before
                    .iter()
                    .find(|(k, _, _)| match target {
                        "source" => k.starts_with("a1:") && k.contains(":chunk:"),
                        "ack" => k.starts_with("k1:"),
                        "native" => *k == record_key(&prior_id),
                        _ => unreachable!(),
                    })
                    .unwrap();
                let effect = if mutation == "delete" {
                    format!("DELETE FROM records WHERE key='{}';", victim.0)
                } else {
                    let mut bytes = victim.1.clone();
                    if target == "native" {
                        let mut data: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                        data["snapshot"]["native"]["admission_available"] = false.into();
                        data["snapshot"]["native"]["delivery_available"] = false.into();
                        bytes = serde_json::to_vec(&data).unwrap();
                    } else {
                        bytes[0] ^= 1;
                    }
                    let hex = |b: &[u8]| b.iter().map(|v| format!("{v:02x}")).collect::<String>();
                    format!(
                        "UPDATE records SET value=X'{}',digest=X'{}' WHERE key='{}';",
                        hex(&bytes),
                        hex(&Sha256::digest(&bytes)),
                        victim.0
                    )
                };
                store.connection.execute_batch(&format!(
                    "CREATE TRIGGER attack AFTER {event} ON records WHEN NEW.key='{key}' BEGIN {effect} END;"
                )).unwrap();
                let result = request
                    .write()
                    .persist(&mut store.native_continuity_port(&plan));
                if result.is_ok() {
                    escaped.push(format!("{event}/{target}/{mutation}"));
                    continue;
                }
                assert_eq!(rows(&store), before, "{event}/{target}/{mutation}");
                let after = store.current_state().unwrap();
                assert_eq!(
                    (after.generation, after.watermark, after.batch),
                    (state.generation, state.watermark, state.batch)
                );
                assert_eq!(
                    store.inventory_custody(limits()).unwrap().occurrences.len(),
                    1
                );
                store
                    .connection
                    .execute_batch("DROP TRIGGER attack")
                    .unwrap();
                request
                    .write()
                    .persist(&mut store.native_continuity_port(&plan))
                    .unwrap();
                let healthy = store.inventory_custody(limits()).unwrap();
                assert_eq!(healthy.occurrences.len(), 1);
                assert_eq!(healthy.acknowledgement_keys.len(), 1);
                assert_eq!(healthy.native_continuity_keys.len(), 2);
            }
        }
    }
    assert!(
        escaped.is_empty(),
        "native trigger mutations committed: {escaped:?}"
    );
}
