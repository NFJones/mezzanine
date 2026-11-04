//! Real canonical sender-handoff images and file boundaries. Descriptive rows
//! exercise physical custody only; no original-root handoff/consume is claimed.

use super::*;
use crate::storage::transcript::CustodyReceiptPublicationFault;
use mez_agent::messaging::DeliveryTerminalReason;
use mez_agent::transcript::{
    TranscriptContextEvent, TranscriptTerminalHandoffEvent, TranscriptTerminalHandoffSource,
};

/// Constructs only a descriptive neutral row, never a messaging capability.
fn outcome(fixture: &Fixture) -> TranscriptEntry {
    let mut row = fixture.row.clone();
    row.role = TranscriptRole::System;
    let source = TranscriptTerminalHandoffSource {
        terminal_handoff_version: 1,
        store_id: [1; 16],
        source_sha256: [2; 32],
        message_sequence: 7,
        binding_index: 0,
        message_id: "original accepted action".into(),
        sender: row.agent_id.clone(),
        sender_registered_after_sequence: 0,
        recipient: "unavailable-original-recipient".into(),
        registered_after_sequence: 2,
        native_owner: serde_json::from_value(serde_json::json!({
            "conversation_id": row.conversation_id, "incarnation": "1".repeat(64)
        }))
        .unwrap(),
        reason: DeliveryTerminalReason::Expired,
        observed_at_ms: 0,
    };
    let event = TranscriptTerminalHandoffEvent::new(source, 11).unwrap();
    row.content = TranscriptContextEvent::TerminalHandoff {
        event: Box::new(event),
    }
    .to_transcript_content();
    row
}

/// The same actual lease/image/prefix machinery retains a neutral sender fact
/// across partial append and generic startup recovery. Every ordinary producer
/// stays fenced until explicit original-owner retirement; no ACK is inferred.
#[test]
fn canonical_terminal_handoff_receipt_owns_exact_prefix_and_partial_final_row() {
    let fixture = Fixture::new();
    let mut row = outcome(&fixture);
    let mut preceding = fixture.row.clone();
    preceding.content = "actual preceding user chronology".into();
    let mut prefix = vec![preceding];
    let lease = fixture
        .store
        .custody_lease("original", false, None)
        .unwrap();
    let plan = lease.prepare_custody_append(&mut prefix, &mut row).unwrap();
    assert_eq!(plan.source().kind(), CustodyReceiptKind::TerminalHandoff);
    assert!(plan.source().native_input().is_none());
    prefix.push(row.clone());
    let owner = lease.publish_append_receipt(plan.clone(), &prefix).unwrap();
    let selected = plan.input().archive_sequence;
    assert_eq!(selected, 3);
    lease.append_custody_prefix(&plan, &prefix).unwrap();
    assert_eq!(fixture.store.inspect("original").unwrap().len(), 2);
    let bytes = encoded_row(&row).unwrap();
    let mut file = OpenOptions::new().append(true).open(&lease.path).unwrap();
    file.write_all(&bytes[..bytes.len() / 2]).unwrap();
    file.sync_all().unwrap();
    drop(lease);
    fixture.store.recover_append_receipts().unwrap();
    owner.revalidate().unwrap();
    let mut foreign = fixture.row.clone();
    foreign.sequence = selected;
    assert!(fixture.store.append(&foreign).is_err());
    assert!(fixture.store.accept_append_receipt(&[foreign], 19).is_err());
    assert!(fixture.store.fork("original", "foreign-fork", 2).is_err());
    assert!(fixture.store.delete("original").is_err());
    assert!(fixture.store.archive_session("original", 2).is_err());
    assert!(
        fixture
            .store
            .save_compaction_epoch("original", 1, "handoff pending")
            .is_err()
    );
    let occurrence = plan.source().occurrence();
    assert!(
        fixture
            .store
            .custody_append_receipt("original", CustodyReceiptKind::NativeInput, &occurrence)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .store
            .custody_lease(
                "original",
                false,
                Some((CustodyReceiptKind::NativeInput, &occurrence))
            )
            .is_err()
    );
    let lease = fixture
        .store
        .custody_lease(
            "original",
            false,
            Some((CustodyReceiptKind::TerminalHandoff, &occurrence)),
        )
        .unwrap();
    let restored = lease
        .lookup_append_receipt(CustodyReceiptKind::TerminalHandoff, &occurrence)
        .unwrap()
        .unwrap();
    assert_eq!(restored.plan(), &plan);
    assert_eq!(restored.rows().unwrap(), prefix);
    lease
        .append_custody_receipt(restored.plan(), &restored.rows().unwrap())
        .unwrap();
    lease
        .append_custody_receipt(restored.plan(), &restored.rows().unwrap())
        .unwrap();
    assert_eq!(lease.seal(plan.input()).unwrap(), row);
    assert_eq!(fixture.store.inspect("original").unwrap().len(), 3);
    restored.retire().unwrap();
    owner.retire().unwrap();
    drop(lease);
    assert!(
        !fixture
            .store
            .has_unsettled_append_receipts("original")
            .unwrap()
    );
}

/// Failures at all real image publication boundaries retain the exact selected
/// position. Complete stages can promote only their own plan/row; incomplete
/// stages are separately abortable before any canonical bytes. Generic recovery
/// must leave complete sender images for their original root just like inputs.
#[test]
fn canonical_terminal_handoff_publication_faults_reconcile_original_image() {
    use CustodyReceiptPublicationFault::*;
    for fault in [BeforeStage, Partial, Complete, Linked] {
        let fixture = Fixture::new();
        let mut row = outcome(&fixture);
        let lease = fixture
            .store
            .custody_lease("original", false, None)
            .unwrap();
        let plan = lease
            .prepare_custody_append(&mut Vec::new(), &mut row)
            .unwrap();
        let rows = vec![row.clone()];
        fixture
            .store
            .fail_custody_receipt_publication_for_tests(fault);
        assert!(lease.publish_append_receipt(plan.clone(), &rows).is_err());
        let occurrence = plan.source().occurrence();
        drop(lease);
        fixture.store.recover_append_receipts().unwrap();
        assert_eq!(fixture.store.inspect("original").unwrap().len(), 1);
        let lease = fixture
            .store
            .custody_lease(
                "original",
                false,
                Some((CustodyReceiptKind::TerminalHandoff, &occurrence)),
            )
            .unwrap();
        let owner = match fault {
            Complete => {
                let stage = fixture
                    .store
                    .custody_stage_locked(
                        "original",
                        CustodyReceiptKind::TerminalHandoff,
                        &occurrence,
                        &lease.lease,
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(stage.plan(), Some(&plan));
                lease
                    .check_custody_append_plan(stage.plan().unwrap(), &stage.rows().unwrap())
                    .unwrap();
                stage.publish(&fixture.store, &lease.lease).unwrap()
            }
            Partial => {
                let stage = fixture
                    .store
                    .custody_stage_locked(
                        "original",
                        CustodyReceiptKind::TerminalHandoff,
                        &occurrence,
                        &lease.lease,
                    )
                    .unwrap()
                    .unwrap();
                assert!(stage.plan().is_none());
                stage.abort_incomplete().unwrap();
                lease.publish_append_receipt(plan.clone(), &rows).unwrap()
            }
            BeforeStage | Linked => lease.publish_append_receipt(plan.clone(), &rows).unwrap(),
        };
        assert_eq!(owner.plan(), &plan);
        lease
            .append_custody_receipt(owner.plan(), &owner.rows().unwrap())
            .unwrap();
        assert_eq!(fixture.store.inspect("original").unwrap()[1], row);
        owner.retire().unwrap();
    }
}

/// Framing, source role and deterministic row all participate in identity.
/// Relabeling v4 as native v3, inserting both owners or changing original actor
/// cannot turn a sender fact into input custody or bypass digest validation.
#[test]
fn canonical_terminal_handoff_refuses_cross_role_version_and_row_substitution() {
    let fixture = Fixture::new();
    let mut row = outcome(&fixture);
    let lease = fixture
        .store
        .custody_lease("original", false, None)
        .unwrap();
    let plan = lease
        .prepare_custody_append(&mut Vec::new(), &mut row)
        .unwrap();
    let owner = lease
        .publish_append_receipt(plan.clone(), &[row.clone()])
        .unwrap();
    let occurrence = plan.source().occurrence();
    let receipt_path = fixture.root.join(".append-receipts").join(format!(
        "original-handoff-{}.json",
        occurrence
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ));
    let original_bytes = fs::read(&receipt_path).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&original_bytes).unwrap();
    assert_eq!(original["version"], 4);
    assert!(original.get("native_input").is_none());
    assert!(
        original["terminal_handoff"]["source"]
            .get("terminal_handoff_version")
            .is_some()
    );
    for mutate in [0, 1, 2] {
        let mut changed = original.clone();
        match mutate {
            0 => changed["version"] = serde_json::json!(3),
            1 => changed["native_input"] = changed["terminal_handoff"].clone(),
            _ => changed["terminal_handoff"]["source"]["observed_at_ms"] = serde_json::json!(1),
        }
        fs::write(&receipt_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            fixture
                .store
                .custody_append_receipt_locked(
                    "original",
                    CustodyReceiptKind::TerminalHandoff,
                    &occurrence,
                    &lease.lease
                )
                .is_err()
        );
        fs::write(&receipt_path, &original_bytes).unwrap();
    }
    let mut changed = row.clone();
    changed.agent_id = "unavailable-original-recipient".into();
    assert!(plan.validate_rows(&[changed]).is_err());
    let mut changed = row;
    changed.content = receipts::input(&fixture).content;
    assert!(plan.validate_rows(&[changed]).is_err());
    owner.revalidate().unwrap();
}

/// Retained first descriptors cannot be retargeted to equal-looking replacement
/// history or prefix bytes. A refused append leaves its canonical image pending.
#[test]
fn canonical_terminal_handoff_refuses_replacement_archive_and_changed_prefix() {
    for replace in [false, true] {
        let fixture = Fixture::new();
        let mut row = outcome(&fixture);
        let lease = fixture
            .store
            .custody_lease("original", false, None)
            .unwrap();
        let plan = lease
            .prepare_custody_append(&mut Vec::new(), &mut row)
            .unwrap();
        let owner = lease
            .publish_append_receipt(plan.clone(), &[row.clone()])
            .unwrap();
        let path = fixture.root.join("original/history.tsv");
        let original = fs::read(&path).unwrap();
        if replace {
            fs::rename(&path, path.with_extension("original")).unwrap();
            fs::write(&path, &original).unwrap();
        } else {
            let mut changed = original;
            changed[0] ^= 1;
            fs::write(&path, changed).unwrap();
        }
        assert!(lease.append_custody_receipt(&plan, &[row]).is_err());
        owner.revalidate().unwrap();
        assert!(
            fixture
                .store
                .has_unsettled_append_receipts_locked("original", &lease.lease)
                .unwrap()
        );
    }
}
