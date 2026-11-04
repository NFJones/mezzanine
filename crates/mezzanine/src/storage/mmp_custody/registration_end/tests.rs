//! Original SQLite end-journal inputs, integrity and allocator controls.

use super::*;
use mez_agent::messaging::{
    Envelope, MessageAcceptanceDecodeLimits, MessageNativeIncarnation,
    MessageRegistrationEndRequest, MessageScope, MessageService, MessageStorageScope, Recipient,
    SenderIdentity,
};

/// Owns only a generated private fixture, never actual user session state.
struct Fixture(std::path::PathBuf);
impl Fixture {
    /// Chooses a unique temporary private-store destination.
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-registration-end-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
    /// Opens the original concrete store under this fixture's owner label.
    fn open(&self) -> MmpCustodyStore {
        MmpCustodyStore::open(&self.0, "original-end").unwrap()
    }
}
impl Drop for Fixture {
    /// Deletes only this generated fixture after all its actual owners drop.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Real guarded native publication precedes each actual end in these controls.
fn scene(store: &mut MmpCustodyStore) -> (MessageService, MessageStorageScope, SenderIdentity) {
    let mut service = MessageService::default();
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    service.require_native_continuity(&scope).unwrap();
    let agent = service.register_agent(None, None, "native", vec![]);
    let registration = service
        .capture_sender_registration(&agent.agent_id)
        .unwrap();
    service
        .bind_native_conversation(
            &registration,
            "native-parent",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    let request = service
        .prepare_native_continuity(&scope, &agent.agent_id)
        .unwrap();
    let plan = store
        .native_continuity_plan(agent.agent_id.as_str())
        .unwrap();
    let verified = request
        .write()
        .persist(&mut store.native_continuity_port(&plan))
        .unwrap();
    assert!(request.finish(&mut service, verified).unwrap());
    (service, scope, agent)
}

/// Reads only bounded original sources; no fixture needs a paid provider.
fn limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}

/// Captures the actual first removal, without reconstructing it from n1 data.
fn retire(
    service: &mut MessageService,
    scope: &MessageStorageScope,
    agent: &SenderIdentity,
) -> MessageRegistrationEndRequest {
    assert!(service.retire_agent_identity(&agent.agent_id));
    service.prepare_registration_end(scope).unwrap().unwrap()
}

/// One explicit end is exactly one immutable original batch even after a lost
/// positive. Reading or journaling it changes no source, terminal or ACK slots.
#[test]
fn registration_end_journal_reconciles_one_original_without_terminal_authority() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (mut service, scope, agent) = scene(&mut store);
    let request = retire(&mut service, &scope, &agent);
    let plan = store
        .registration_end_plan(request.write().snapshot())
        .unwrap();
    drop(
        request
            .write()
            .persist(&mut store.registration_end_port(&plan))
            .unwrap(),
    );
    assert!(service.has_pending_registration_end().unwrap());
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.registration_end_keys.len(), 1);
    assert_eq!(inventory.native_lifetime_keys.len(), 1);
    assert!(
        inventory.occurrences.is_empty()
            && inventory.expiry_keys.is_empty()
            && inventory.acknowledgement_keys.is_empty()
    );
    assert_eq!(store.current_state().unwrap().generation, 2);
    let verified = request
        .write()
        .persist(&mut store.registration_end_port(&plan))
        .unwrap();
    plan.validate_settled(&store).unwrap();
    assert_eq!(store.current_state().unwrap().generation, 2);
    assert!(request.finish(&mut service, verified).unwrap());
    assert!(!service.has_pending_registration_end().unwrap());
    assert!(store.registration_end_plan(&plan.snapshot).is_err());
}

/// The cutoff includes allocated but cancelled originals, and must atomically
/// reserve those gaps without accepting a message or reusing its sequence.
#[test]
fn registration_end_journal_reserves_actual_cancelled_sequence_cutoff() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (mut service, scope, agent) = scene(&mut store);
    let raw = service.register_agent(None, None, "raw", vec![]);
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "cancelled".into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: agent.clone(),
        recipient: Recipient::Agent(raw.agent_id),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "never accepted".into(),
        extension_fields: vec![],
    };
    let ticket = service
        .prepare_acceptance(
            &agent.agent_id,
            envelope,
            MessageScope::Session,
            0,
            4096,
            None,
        )
        .unwrap();
    service.cancel_acceptance(&ticket).unwrap();
    let request = retire(&mut service, &scope, &agent);
    assert_eq!(request.write().snapshot().ended_after_sequence, 1);
    let plan = store
        .registration_end_plan(request.write().snapshot())
        .unwrap();
    let verified = request
        .write()
        .persist(&mut store.registration_end_port(&plan))
        .unwrap();
    request.finish(&mut service, verified).unwrap();
    plan.validate_settled(&store).unwrap();
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.sequence_watermark, 1);
    assert!(inventory.occurrences.is_empty());
    assert_eq!(service.next_message_sequence(), 2);
}

/// Rechecksummed malformed end facts fail whole inventory rather than being
/// ignored as no retirement. A known existing o1 cannot be overwritten into a
/// fresh plan, including corrupt agent attribution caught through native links.
#[test]
fn registration_end_corruption_refuses_inventory_and_fresh_overwrite() {
    for fault in [
        "version",
        "allocation",
        "generation",
        "same-generation",
        "zero",
        "namespace",
        "agent",
        "floor",
        "cutoff",
        "unknown",
    ] {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let (mut service, scope, agent) = scene(&mut store);
        let request = retire(&mut service, &scope, &agent);
        let snapshot = request.write().snapshot().clone();
        let plan = store.registration_end_plan(&snapshot).unwrap();
        request
            .write()
            .persist(&mut store.registration_end_port(&plan))
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&plan.bytes).unwrap();
        match fault {
            "version" => value["format_version"] = 2.into(),
            "allocation" => {
                value["store_id"][0] = (value["store_id"][0].as_u64().unwrap() ^ 1).into()
            }
            "generation" => value["generation"] = 3.into(),
            "same-generation" => value["generation"] = 1.into(),
            "zero" => value["generation"] = 0.into(),
            "namespace" => {
                value["snapshot"]["registration_namespace"][0] =
                    (value["snapshot"]["registration_namespace"][0]
                        .as_u64()
                        .unwrap()
                        ^ 1)
                    .into()
            }
            "agent" => value["snapshot"]["agent_id"] = "different-agent".into(),
            "floor" => value["snapshot"]["registered_after_sequence"] = 1.into(),
            "cutoff" => value["snapshot"]["ended_after_sequence"] = 1.into(),
            "unknown" => value["task_authority"] = true.into(),
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        store
            .connection
            .execute(
                "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                params![&bytes, &Sha256::digest(&bytes)[..], &plan.key],
            )
            .unwrap();
        assert!(store.inventory_custody(limits()).is_err(), "{fault}");
        assert!(store.registration_end_plan(&snapshot).is_err(), "{fault}");
        assert_eq!(store.read(&plan.key).unwrap().unwrap(), bytes);
    }
}

/// A trigger may neither delete original native provenance nor change the end
/// target with a correct checksum. The entire batch rolls back before positive;
/// repair then retries the same immutable plan exactly once.
#[test]
fn registration_end_sql_effects_preserve_original_targets_and_other_rows() {
    for target in ["other", "own"] {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let (mut service, scope, agent) = scene(&mut store);
        let request = retire(&mut service, &scope, &agent);
        let plan = store
            .registration_end_plan(request.write().snapshot())
            .unwrap();
        let before = store.inventory_custody(limits()).unwrap();
        let history_key = &before.native_lifetime_keys[0];
        let original_history = store.read(history_key).unwrap().unwrap();
        let effect = if target == "other" {
            format!("DELETE FROM records WHERE key='{history_key}';")
        } else {
            let mut value: serde_json::Value = serde_json::from_slice(&plan.bytes).unwrap();
            value["generation"] = 0.into();
            let bytes = serde_json::to_vec(&value).unwrap();
            format!(
                "UPDATE records SET value=X'{}',digest=X'{}' WHERE key='{}';",
                hex(&bytes),
                hex(&Sha256::digest(&bytes)),
                plan.key
            )
        };
        store.connection.execute_batch(&format!("CREATE TRIGGER end_attack AFTER INSERT ON records WHEN NEW.key='{}' BEGIN {effect} END;", plan.key)).unwrap();
        let error = request
            .write()
            .persist(&mut store.registration_end_port(&plan))
            .err()
            .unwrap();
        let expected = if target == "other" {
            "end SQL effects changed original rows"
        } else {
            "MMP custody committed postimage changed"
        };
        assert!(error.to_string().contains(expected), "{target}: {error}");
        assert_eq!(store.current_state().unwrap().generation, 1);
        assert_eq!(store.read(history_key).unwrap().unwrap(), original_history);
        assert!(store.read(&plan.key).unwrap().is_none());
        store
            .connection
            .execute_batch("DROP TRIGGER end_attack")
            .unwrap();
        let verified = request
            .write()
            .persist(&mut store.registration_end_port(&plan))
            .unwrap();
        request.finish(&mut service, verified).unwrap();
        assert_eq!(store.current_state().unwrap().generation, 2);
    }
}

/// Exact end evidence cannot revive its old namespace through a retained native
/// capture after removal. A new plan may not replace ended n1 with a late tuple.
#[test]
fn registration_end_blocks_late_native_publication_for_the_original_namespace() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (mut service, scope, agent) = scene(&mut store);
    let late = service
        .prepare_native_continuity(&scope, &agent.agent_id)
        .unwrap();
    let request = retire(&mut service, &scope, &agent);
    let plan = store
        .registration_end_plan(request.write().snapshot())
        .unwrap();
    request
        .write()
        .persist(&mut store.registration_end_port(&plan))
        .unwrap();
    let native_plan = store
        .native_continuity_plan(agent.agent_id.as_str())
        .unwrap();
    let error = late
        .write()
        .persist(&mut store.native_continuity_port(&native_plan))
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("captured native registration has ended"),
        "{error}"
    );
    assert_eq!(store.current_state().unwrap().generation, 2);
}

/// A second connection's transaction cannot qualify reads, even when it refers
/// to the same physical allocation. Exact snapshot ownership remains required.
#[test]
fn registration_end_reader_requires_actual_original_snapshot_connection() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (mut service, scope, agent) = scene(&mut store);
    let request = retire(&mut service, &scope, &agent);
    let key = record_key(&request.write().snapshot().registration_namespace);
    let other = fixture.open();
    let transaction = other.connection.unchecked_transaction().unwrap();
    assert!(
        store
            .read_registration_end_in_snapshot(&transaction, &key)
            .is_err()
    );
}

/// SQL fixture interpolation contains only generated hex record/checksum bytes.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
