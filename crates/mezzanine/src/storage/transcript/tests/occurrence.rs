//! Occurrence evidence survives durable reconstruction without clock guesses.

use super::*;
use mez_core::presentation::{
    PresentationEntry, PresentationOccurrence, PresentationOccurrenceBasis,
};

/// Version-five steering receipts truly omitted occurrence. Their identity,
/// acceptance order, status and exact display must survive while missing time
/// remains unknown, not inherited from metadata/checkpoint or decode time.
#[test]
fn presentation_occurrence_legacy_v5_missing_steering_stamp_stays_unknown() {
    use mez_agent::transcript::{SteeringRecoveryReceipt, SteeringRecoveryStatus};
    let mut metadata = agent_session_metadata("$legacy", "legacy-steering");
    metadata.steering_recovery.push(SteeringRecoveryReceipt {
        id: "original".into(),
        acceptance_order: 1,
        occurrence: PresentationOccurrence::new(
            Some(1234),
            PresentationOccurrenceBasis::LocalAcceptance,
        ),
        turn_id: Some("turn".into()),
        event_sequence: Some(9),
        display: "exact\t雪\r\n\\source".into(),
        status: SteeringRecoveryStatus::Admitted(7),
    });
    let encoded = encode_agent_session_metadata(&metadata).unwrap();
    let (prefix, _) = encoded.rsplit_once('\t').unwrap();
    let mut rows = serde_json::to_value(&metadata.steering_recovery).unwrap();
    rows[0].as_object_mut().unwrap().remove("occurrence");
    let json = serde_json::to_string(&rows)
        .unwrap()
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    let legacy = format!(
        "{}\t{json}",
        prefix.replacen(
            "mez-agent-session-metadata/6",
            "mez-agent-session-metadata/5",
            1
        )
    );
    metadata.steering_recovery[0].occurrence = PresentationOccurrence::default();
    assert_eq!(decode_agent_session_metadata(&legacy).unwrap(), metadata);
}

/// The fork source uses exactly the established 64-entry/2-MiB transcript tail.
/// Oversized last rows must not turn partial bytes into a fabricated entry, and
/// record-count clipping retains original order/bytes and full catalog count.
#[test]
fn presentation_occurrence_fork_tail_matches_existing_bounded_reader() {
    use crate::storage::transcript::fallback::{Content, RECORD_BYTES, RECORD_LIMIT, Source};
    for large_last in [false, true] {
        let root = temp_root("occurrence-fork-tail");
        let store = AgentTranscriptStore::new(root.clone());
        for sequence in 1..=70 {
            let mut row = entry("original", sequence, TranscriptRole::User);
            if large_last && sequence == 70 {
                row.content = "x".repeat(RECORD_BYTES + 1);
            }
            store.append(&row).unwrap();
        }
        let expected = store
            .inspect_recent("original", RECORD_LIMIT, RECORD_BYTES as u64)
            .unwrap();
        assert_eq!(expected.len(), if large_last { 0 } else { RECORD_LIMIT });
        let forked = store.fork("original", "forked", 99).unwrap();
        assert_eq!(forked.entries, 70);
        let presentation = store.inspect_presentation("forked").unwrap();
        let source = Source::decode(presentation[0].source_text.as_deref().unwrap()).unwrap();
        let Content::Records {
            records,
            transcript_entries,
        } = source.content
        else {
            panic!("expected original records");
        };
        assert_eq!(transcript_entries, 70);
        assert_eq!(Source::entries(&records).unwrap(), expected);
        fs::remove_dir_all(root).unwrap();
    }
}

/// Fallback presentation source uses the existing reversible transcript codec,
/// including NUL/control escaping, and never admits changed record chronology,
/// unknown fields or a future envelope version as raw canonical display text.
#[test]
fn presentation_occurrence_fallback_source_preserves_codec_and_rejects_corruption() {
    use crate::storage::transcript::fallback::{Content, Source};
    let mut original = entry("original", 1, TranscriptRole::Tool);
    original.content = "exact\0\t\r\n雪\\ source".into();
    let source = Source::from_records(&[original.clone()]).unwrap();
    let encoded = source.encode().unwrap();
    let decoded = Source::decode(&encoded).unwrap();
    let Content::Records { records, .. } = decoded.content else {
        panic!("expected original records");
    };
    assert_eq!(Source::entries(&records).unwrap(), vec![original]);
    let mut corrupt: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    corrupt["version"] = 2.into();
    assert!(Source::decode(&corrupt.to_string()).is_err());
    corrupt["version"] = 1.into();
    corrupt["extra"] = true.into();
    assert!(Source::decode(&corrupt.to_string()).is_err());
    let mut corrupt: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    corrupt["content"]["records"] = serde_json::json!([records[0], records[0]]);
    corrupt["content"]["transcript_entries"] = 2.into();
    assert!(Source::decode(&corrupt.to_string()).is_err());
}

/// Presentation v3 retains exact millisecond evidence independently of record
/// creation. Older source-backed v2 carries only coarse legacy record evidence;
/// opaque rows have no proven entry, and conversion overflow stays unknown.
#[test]
fn presentation_occurrence_encoding_preserves_known_unknown_and_legacy_basis() {
    let mut row = presentation("conversation", 1);
    row.source_text = Some("exact\r\n雪\t".into());
    row.source_content_type = Some("text/plain; charset=utf-8".into());
    row.presentation = Some(PresentationEntry {
        id: "occurrence".into(),
        occurrence: PresentationOccurrence::new(
            Some(-1234),
            PresentationOccurrenceBasis::LocalObservation,
        ),
        first_row: true,
    });
    let encoded = row.encode().unwrap();
    assert!(encoded.starts_with("mez-agent-presentation/3\t"));
    assert_eq!(AgentPresentationEntry::decode(&encoded).unwrap(), row);
    let v2 = encoded.rsplit_once('\t').unwrap().0.replacen(
        "mez-agent-presentation/3",
        "mez-agent-presentation/2",
        1,
    );
    let decoded = AgentPresentationEntry::decode(&v2).unwrap();
    assert_eq!(decoded.source_text, row.source_text);
    assert_eq!(
        decoded.presentation.as_ref().unwrap().occurrence,
        PresentationOccurrence::legacy_record(21)
    );
    row.created_at_unix_seconds = u64::MAX;
    let encoded = row.encode().unwrap();
    let v2 = encoded.rsplit_once('\t').unwrap().0.replacen(
        "mez-agent-presentation/3",
        "mez-agent-presentation/2",
        1,
    );
    assert_eq!(
        AgentPresentationEntry::decode(&v2)
            .unwrap()
            .presentation
            .unwrap()
            .occurrence,
        PresentationOccurrence::default()
    );
    row.source_text = None;
    row.source_content_type = None;
    let encoded = row.encode().unwrap();
    let v2 = encoded.rsplit_once('\t').unwrap().0.replacen(
        "mez-agent-presentation/3",
        "mez-agent-presentation/2",
        1,
    );
    assert!(
        AgentPresentationEntry::decode(&v2)
            .unwrap()
            .presentation
            .is_none()
    );
}

/// Fork keeps its historical creation/catalog semantics while retaining the
/// original occurrence. Steering envelope ownership is rebound only at the
/// explicit fork boundary; receipt identity/status/display and copy stay exact.
#[test]
fn presentation_occurrence_fork_preserves_stamp_and_steering_owner() {
    let root = temp_root("occurrence-fork");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("source", 1, TranscriptRole::User))
        .unwrap();
    let occurrence =
        PresentationOccurrence::new(Some(1234), PresentationOccurrenceBasis::LocalAcceptance);
    let source = crate::storage::transcript::steering::Source {
        version: 1,
        conversation_id: "source".into(),
        receipt: mez_agent::transcript::SteeringRecoveryReceipt {
            id: "steering-occurrence".into(),
            acceptance_order: 1,
            occurrence,
            turn_id: Some("turn-1".into()),
            event_sequence: Some(1),
            display: "exact\r\n雪".into(),
            status: mez_agent::transcript::SteeringRecoveryStatus::NotSent,
        },
    };
    let mut row = presentation("source", 1);
    row.presentation = Some(PresentationEntry {
        id: source.receipt.id.clone(),
        occurrence,
        first_row: true,
    });
    row.source_text = Some(source.encode().unwrap());
    row.source_content_type = Some(crate::storage::transcript::steering::CONTENT_TYPE.into());
    store.append_presentation(&row).unwrap();
    let mut conflict = row.clone();
    conflict
        .presentation
        .as_mut()
        .unwrap()
        .occurrence
        .unix_millis = Some(9000);
    assert!(
        store.append_presentation_many(&[conflict]).is_err(),
        "outer stamp cannot contradict the receipt"
    );
    let mut conflict = row.clone();
    conflict.presentation.as_mut().unwrap().id = "changed-occurrence".into();
    assert!(
        store.append_presentation_many(&[conflict]).is_err(),
        "durable retry cannot replace immutable entry metadata"
    );
    store.fork("source", "forked", 99).unwrap();
    let forked = store.inspect_presentation("forked").unwrap();
    assert_eq!(forked.len(), 1);
    assert_eq!(forked[0].created_at_unix_seconds, 99);
    assert_eq!(forked[0].presentation, row.presentation);
    assert_eq!(forked[0].copy_lines, row.copy_lines);
    let decoded = crate::storage::transcript::steering::Source::decode(
        forked[0].source_text.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(decoded.conversation_id, "forked");
    assert_eq!(decoded.receipt, source.receipt);
    fs::remove_dir_all(root).unwrap();
}

/// Hundreds of independently accepted equal/backward-clock entries survive
/// concatenated zstd archive growth in sequence order. Storage never rewrites
/// occurrence evidence, grouped source/copy rows or identity while compacting.
#[test]
fn presentation_occurrence_compressed_growth_keeps_sequence_and_bytes() {
    let root = temp_root("occurrence-growth");
    let store = AgentTranscriptStore::new(root.clone())
        .with_presentation_compaction_threshold(1024)
        .unwrap();
    let mut expected = Vec::new();
    for sequence in 1..=205 {
        let mut row = compacting_presentation("history", sequence);
        row.presentation = Some(PresentationEntry {
            id: format!("entry-{sequence}"),
            occurrence: PresentationOccurrence::new(
                Some(if sequence % 2 == 0 { 1000 } else { -500 }),
                PresentationOccurrenceBasis::LocalAcceptance,
            ),
            first_row: true,
        });
        store.append_presentation(&row).unwrap();
        expected.push(row.normalized_for_agent_log_wrap());
    }
    assert!(
        store
            .presentation_compressed_path("history")
            .unwrap()
            .exists()
    );
    assert_eq!(store.inspect_presentation("history").unwrap(), expected);
    assert_eq!(store.next_presentation_sequence("history").unwrap(), 206);
    fs::remove_dir_all(root).unwrap();
}
