//! Real staging files and mutation fences; no transport authority is claimed.

use super::receipts::input;
use super::*;
use crate::storage::transcript::CustodyReceiptPublicationFault;

/// Creates only a physical candidate and injects failure after its real bytes
/// reach the selected boundary. Original-source qualification is tested at the
/// MMP root/factory boundary, not inferred from these descriptive row fields.
fn staged(
    fixture: &Fixture,
    fault: CustodyReceiptPublicationFault,
) -> (super::super::CustodyAppendPlan, Vec<TranscriptEntry>) {
    let mut native = input(fixture);
    let lease = fixture
        .store
        .custody_lease("original", false, None)
        .unwrap();
    let plan = lease
        .prepare_custody_append(&mut Vec::new(), &mut native)
        .unwrap();
    fixture
        .store
        .fail_custody_receipt_publication_for_tests(fault);
    let rows = vec![native];
    assert!(lease.publish_append_receipt(plan.clone(), &rows).is_err());
    (plan, rows)
}

/// Complete unpublished images fence ordinary producers and survive generic
/// recovery. Original physical qualification can promote the same file and
/// finish its exact row without moving the selected sequence.
#[test]
fn complete_native_stage_fences_ordinary_writers_and_preserves_exact_image() {
    let fixture = Fixture::new();
    let (plan, rows) = staged(&fixture, CustodyReceiptPublicationFault::Complete);
    assert!(
        fixture
            .store
            .has_unsettled_append_receipts("original")
            .unwrap()
    );
    fixture.store.recover_append_receipts().unwrap();
    assert_eq!(fixture.store.inspect("original").unwrap().len(), 1);
    let mut foreign = fixture.row.clone();
    foreign.sequence = 2;
    assert!(fixture.store.append_many(&[foreign.clone()]).is_err());
    assert!(fixture.store.accept_append_receipt(&[foreign], 9).is_err());
    assert!(fixture.store.fork("original", "fork-target", 2).is_err());
    assert!(fixture.store.delete_entry("original", 1).is_err());
    assert!(fixture.store.delete("original").is_err());
    assert!(fixture.store.archive_session("original", 2).is_err());
    assert!(
        fixture
            .store
            .save_compaction_epoch("original", 1, "pending native source")
            .is_err()
    );
    let original_rows = fixture.store.inspect("original").unwrap();
    let expected = crate::storage::transcript::TranscriptPrefixExpectation::capture(
        "original",
        original_rows.iter(),
    )
    .unwrap();
    assert!(
        fixture
            .store
            .seal_transcript_prefix(&expected)
            .unwrap()
            .is_none()
    );
    assert_eq!(original_rows.len(), 1);
    assert!(!fixture.root.join("fork-target").exists());
    assert!(
        fixture
            .store
            .compaction_epoch("original")
            .unwrap()
            .is_none()
    );
    let occurrence = plan.source().occurrence();
    let lease = fixture
        .store
        .custody_lease(
            "original",
            false,
            Some((
                crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
                &occurrence,
            )),
        )
        .unwrap();
    let stage = fixture
        .store
        .custody_stage_locked(
            "original",
            crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
            &occurrence,
            &lease.lease,
        )
        .unwrap()
        .unwrap();
    assert_eq!(stage.plan(), Some(&plan));
    assert_eq!(stage.rows().unwrap(), rows);
    lease.check_custody_append_plan(&plan, &rows).unwrap();
    let owner = stage.publish(&fixture.store, &lease.lease).unwrap();
    lease
        .append_custody_receipt(&plan, &owner.rows().unwrap())
        .unwrap();
    assert_eq!(fixture.store.inspect("original").unwrap()[1], rows[0]);
}

/// Incomplete JSON owns no committed canonical row. Removing this actual
/// candidate preserves the archive and releases only its ordinary write fence;
/// the real root test separately proves accepted-source/journal qualification.
#[test]
fn partial_native_stage_abort_preserves_archive_without_committing_input() {
    let fixture = Fixture::new();
    let (plan, _) = staged(&fixture, CustodyReceiptPublicationFault::Partial);
    let occurrence = plan.source().occurrence();
    assert!(
        fixture
            .store
            .has_unsettled_append_receipts("original")
            .unwrap()
    );
    let lease = fixture
        .store
        .custody_lease(
            "original",
            false,
            Some((
                crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
                &occurrence,
            )),
        )
        .unwrap();
    let stage = fixture
        .store
        .custody_stage_locked(
            "original",
            crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
            &occurrence,
            &lease.lease,
        )
        .unwrap()
        .unwrap();
    assert!(stage.plan().is_none());
    stage.abort_incomplete().unwrap();
    drop(lease);
    assert!(
        !fixture
            .store
            .has_unsettled_append_receipts("original")
            .unwrap()
    );
    assert_eq!(fixture.store.inspect("original").unwrap().len(), 1);
}

/// Complete but damaged data is not an interrupted producer. Qualification
/// refuses without deleting or truncating its owner, preventing silent loss.
#[test]
fn corrupt_complete_native_stage_is_preserved_and_refused() {
    let fixture = Fixture::new();
    let (plan, _) = staged(&fixture, CustodyReceiptPublicationFault::Complete);
    let path = fs::read_dir(fixture.root.join(".append-receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["digest"] = "damaged complete checksum".into();
    let changed = serde_json::to_vec(&value).unwrap();
    fs::write(&path, &changed).unwrap();
    let occurrence = plan.source().occurrence();
    let lease = fixture
        .store
        .custody_lease(
            "original",
            false,
            Some((
                crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
                &occurrence,
            )),
        )
        .unwrap();
    assert!(
        fixture
            .store
            .custody_stage_locked(
                "original",
                crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
                &occurrence,
                &lease.lease
            )
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), changed);
    assert_eq!(fixture.store.inspect("original").unwrap().len(), 1);
}

/// A different temporary file alongside a published owner cannot be mistaken
/// for the second link of the same publication or stranded after retirement.
#[test]
fn published_native_receipt_refuses_conflicting_temporary_owner() {
    let fixture = Fixture::new();
    let mut native = input(&fixture);
    let lease = fixture
        .store
        .custody_lease("original", false, None)
        .unwrap();
    let plan = lease
        .prepare_custody_append(&mut Vec::new(), &mut native)
        .unwrap();
    let occurrence = plan.source().occurrence();
    let owner = lease.publish_append_receipt(plan, &[native]).unwrap();
    let path = fs::read_dir(fixture.root.join(".append-receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let temp = path.with_extension("json.tmp");
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)
        .unwrap()
        .write_all(b"{")
        .unwrap();
    assert!(
        fixture
            .store
            .custody_append_receipt_locked(
                "original",
                crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
                &occurrence,
                &lease.lease
            )
            .is_err()
    );
    assert_eq!(fs::read(&temp).unwrap(), b"{");
    assert!(owner.revalidate().is_err());
    assert!(path.exists());
    fs::remove_file(&temp).unwrap();
    owner.revalidate().unwrap();
    owner.retire().unwrap();
    owner.retire().unwrap();
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)
        .unwrap()
        .write_all(b"{")
        .unwrap();
    assert!(owner.retire().is_err());
    assert_eq!(fs::read(&temp).unwrap(), b"{");
}

/// A byte-identical replacement is still another physical owner. Promotion
/// must retain the selected inode and refuse without consuming its replacement.
#[test]
fn complete_native_stage_refuses_replaced_staging_object() {
    let fixture = Fixture::new();
    let (plan, _) = staged(&fixture, CustodyReceiptPublicationFault::Complete);
    let occurrence = plan.source().occurrence();
    let lease = fixture
        .store
        .custody_lease(
            "original",
            false,
            Some((
                crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
                &occurrence,
            )),
        )
        .unwrap();
    let stage = fixture
        .store
        .custody_stage_locked(
            "original",
            crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
            &occurrence,
            &lease.lease,
        )
        .unwrap()
        .unwrap();
    let path = fs::read_dir(fixture.root.join(".append-receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bytes = fs::read(&path).unwrap();
    let saved = path.with_extension("saved");
    fs::rename(&path, &saved).unwrap();
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    assert!(stage.publish(&fixture.store, &lease.lease).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read(&saved).unwrap(), bytes);
    assert!(!path.with_extension("").exists());
}
