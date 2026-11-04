//! Canonical receipt bytes and original backing, without transport authority.

use super::*;
use mez_agent::transcript::{
    TranscriptContextEvent, TranscriptNativeInputEvent, TranscriptNativeInputSource,
};
use std::os::unix::fs::DirBuilderExt;

/// Builds descriptive canonical input data for filesystem-owner tests only.
/// These row fields do not create a message, native binding or ACK capability.
pub(super) fn input(fixture: &Fixture) -> TranscriptEntry {
    let mut row = fixture.row.clone();
    row.role = TranscriptRole::System;
    let source = TranscriptNativeInputSource {
        version: 1,
        store_id: [1; 16],
        source_sha256: [2; 32],
        message_sequence: 1,
        binding_index: 0,
        recipient: row.agent_id.clone(),
        registered_after_sequence: 0,
        native_owner: serde_json::from_value(
            serde_json::json!({"conversation_id":"original", "incarnation":"1".repeat(64)}),
        )
        .unwrap(),
        offered_at_ms: 1,
    };
    let event = TranscriptNativeInputEvent::new(
        source,
        2,
        "native input".into(),
        "untrusted peer data".into(),
    )
    .unwrap();
    row.content = TranscriptContextEvent::NativePeerMessage {
        event: Box::new(event),
    }
    .to_transcript_content();
    row
}

/// Sharing physical ownership with terminal handoff must preserve the existing
/// native v3 serialized source/plan, filename and digest. Decode into the former
/// concrete plan schema to compare exact bytes independently of the new enum.
#[test]
fn canonical_native_receipt_preserves_original_v3_framing_and_digest() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct OriginalPlan {
        version: u32,
        source: TranscriptNativeInputSource,
        before: CustodyTranscriptPlan,
        input: CustodyTranscriptPlan,
    }
    let fixture = Fixture::new();
    let mut row = input(&fixture);
    let lease = fixture
        .store
        .custody_lease("original", false, None)
        .unwrap();
    let plan = lease
        .prepare_custody_append(&mut Vec::new(), &mut row)
        .unwrap();
    let bytes = serde_json::to_vec(&plan).unwrap();
    let original: OriginalPlan = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(serde_json::to_vec(&original).unwrap(), bytes);
    let owner = lease.publish_append_receipt(plan.clone(), &[row]).unwrap();
    let occurrence = plan.source().occurrence();
    let path = fixture.root.join(".append-receipts").join(format!(
        "original-native-{}.json",
        occurrence
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ));
    let data: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(data["version"], 3);
    assert!(data.get("terminal_handoff").is_none());
    let rows = data["rows"].as_array().unwrap();
    let mut rows_hash = Sha256::new();
    for row in rows {
        let row = row.as_str().unwrap();
        rows_hash.update((row.len() as u64).to_be_bytes());
        rows_hash.update(row.as_bytes());
    }
    let rows_digest = rows_hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut digest = Sha256::new();
    digest.update(b"mezzanine-native-transcript-append-v3\0");
    digest.update(rows_digest.as_bytes());
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
    assert_eq!(
        data["digest"],
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    owner.revalidate().unwrap();
}

/// Ordinary startup recovery recognizes the native receipt but leaves its
/// input/retirement to the original-root owner. Prefix bytes precede the final
/// row, and the exact retained image reconciles a partial native append.
#[test]
fn canonical_native_receipt_retains_prefix_and_partial_row_for_original_owner() {
    let fixture = Fixture::new();
    let mut native = input(&fixture);
    let mut user = fixture.row.clone();
    user.role = TranscriptRole::System;
    user.content = TranscriptContextEvent::user_event(1, "user prompt", "actual preceding request")
        .unwrap()
        .to_transcript_content();
    let mut prefix = vec![user];
    let lease = fixture
        .store
        .custody_lease("original", false, None)
        .unwrap();
    let plan = lease
        .prepare_custody_append(&mut prefix, &mut native)
        .unwrap();
    let rows = prefix
        .iter()
        .cloned()
        .chain(std::iter::once(native.clone()))
        .collect::<Vec<_>>();
    let owner = lease.publish_append_receipt(plan.clone(), &rows).unwrap();
    lease.append_custody_prefix(&plan, &rows).unwrap();
    assert_eq!(fixture.store.inspect("original").unwrap().len(), 2);
    drop(lease);
    fixture.store.recover_append_receipts().unwrap();
    assert_eq!(fixture.store.inspect("original").unwrap().len(), 2);
    assert!(
        fixture
            .store
            .has_unsettled_append_receipts("original")
            .unwrap()
    );
    owner.revalidate().unwrap();
    let mut foreign = fixture.row.clone();
    foreign.sequence = 3;
    foreign.content = "foreign ordinary writer must not occupy native position".into();
    assert!(fixture.store.append(&foreign).is_err());
    assert!(fixture.store.append_many(&[foreign.clone()]).is_err());
    assert!(fixture.store.accept_append_receipt(&[foreign], 19).is_err());
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
    let encoded = encoded_row(&native).unwrap();
    let mut file = OpenOptions::new().append(true).open(&lease.path).unwrap();
    file.write_all(&encoded[..encoded.len() / 2]).unwrap();
    file.sync_all().unwrap();
    lease
        .append_custody_receipt(&plan, &owner.rows().unwrap())
        .unwrap();
    lease
        .append_custody_receipt(&plan, &owner.rows().unwrap())
        .unwrap();
    let committed = fixture.store.inspect("original").unwrap();
    assert_eq!(committed.len(), 3);
    assert_eq!(committed[1], rows[0]);
    assert_eq!(committed[2], native);
    // A data-only filesystem control never claims the original MMP c1; the
    // native recovery owner therefore remains present for root-boundary tests.
    assert!(owner.revalidate().is_ok());
}

/// A copied receipt is not permission to create input in a replacement
/// conversation. Its original physical plan remains usable only after that
/// original backing is restored; missing backing never becomes an empty file.
#[test]
fn canonical_native_receipt_refuses_missing_or_replacement_backing() {
    let fixture = Fixture::new();
    let mut native = input(&fixture);
    let mut prefix = Vec::new();
    let lease = fixture
        .store
        .custody_lease("original", false, None)
        .unwrap();
    let plan = lease
        .prepare_custody_append(&mut prefix, &mut native)
        .unwrap();
    let rows = vec![native];
    let owner = lease.publish_append_receipt(plan.clone(), &rows).unwrap();
    drop(lease);
    let original = fixture.root.join("original");
    let retained = fixture.root.join("retained-original");
    fs::rename(&original, &retained).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&original).unwrap();
    let occurrence = plan.source().occurrence();
    assert!(
        fixture
            .store
            .custody_lease(
                "original",
                false,
                Some((
                    crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
                    &occurrence
                ))
            )
            .is_err()
    );
    assert!(!original.join("history.tsv").exists());
    File::create(original.join("history.tsv")).unwrap();
    let replacement = fixture
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
        replacement
            .append_custody_receipt(&plan, &owner.rows().unwrap())
            .is_err()
    );
    assert_eq!(fs::metadata(original.join("history.tsv")).unwrap().len(), 0);
    drop(replacement);
    fs::remove_dir_all(&original).unwrap();
    fs::rename(&retained, &original).unwrap();
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
    lease
        .append_custody_receipt(&plan, &owner.rows().unwrap())
        .unwrap();
    assert_eq!(fixture.store.inspect("original").unwrap().len(), 2);
}

/// A preceding row may have reached canonical storage through ordinary
/// bookkeeping before the native worker gets its lease. The same immutable
/// chronology identity must keep its earlier position instead of duplicating.
#[test]
fn canonical_native_prefix_deduplicates_validated_original_chronology() {
    let fixture = Fixture::new();
    let mut user = fixture.row.clone();
    user.role = TranscriptRole::System;
    user.sequence = 2;
    user.content = TranscriptContextEvent::user_event(1, "user prompt", "actual prior request")
        .unwrap()
        .to_transcript_content();
    fixture.store.append(&user).unwrap();
    let mut duplicate = user.clone();
    duplicate.sequence = 99;
    duplicate.created_at_unix_seconds = 99;
    let mut prefix = vec![duplicate];
    let mut native = input(&fixture);
    let lease = fixture
        .store
        .custody_lease("original", false, None)
        .unwrap();
    let plan = lease
        .prepare_custody_append(&mut prefix, &mut native)
        .unwrap();
    assert!(prefix.is_empty());
    assert_eq!(native.sequence, 3);
    let owner = lease
        .publish_append_receipt(plan.clone(), &[native.clone()])
        .unwrap();
    lease
        .append_custody_prefix(&plan, &owner.rows().unwrap())
        .unwrap();
    lease
        .append_custody_receipt(&plan, &owner.rows().unwrap())
        .unwrap();
    let rows = fixture.store.inspect("original").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1], user);
    assert_eq!(rows[2], native);
}

/// Damaged canonical publication must refuse a special file without waiting
/// for an external writer. A real FIFO exercises the actual open boundary; the
/// fallback opens both ends only to release a regressed blocking implementation
/// so this regression reports failure instead of hanging the whole test process.
#[test]
fn canonical_native_receipt_lookup_refuses_fifo_without_blocking() {
    use std::os::unix::ffi::OsStrExt;
    use std::time::Duration;
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
    drop(lease);
    let path = fs::read_dir(fixture.root.join(".append-receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::remove_file(&path).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the owned CString is a valid NUL-terminated pathname throughout
    // this call; mkfifo does not retain the pointer.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let store = fixture.store.clone();
    let (send, receive) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        send.send(
            store
                .custody_append_receipt(
                    "original",
                    crate::storage::transcript::custody::CustodyReceiptKind::NativeInput,
                    &occurrence,
                )
                .is_err(),
        )
        .unwrap();
    });
    let observed = receive.recv_timeout(Duration::from_secs(1));
    let release = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path)
        .unwrap();
    worker.join().unwrap();
    drop((release, owner));
    assert_eq!(
        observed.unwrap(),
        true,
        "special files must refuse without a writer"
    );
}
