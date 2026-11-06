//! Persistent accounted records preserve semantic projection and origin while
//! keeping costs quarantined until the actual canonical source is restored.

use super::*;
use crate::{AgentContext, ContextBlock, ContextSourceKind};

/// Explicit admission is a distinct reserved fact; malformed or missing original
/// boundaries cannot downgrade into ordinary user text or an empty claim.
#[test]
fn accounted_task_admission_round_trips_boundary_and_rejects_invalid_identity() {
    let block = ContextBlock::user_event("user prompt", "actual new task");
    let record = ContextBlockRecord::allocate(&block).unwrap();
    let first = crate::ContextBlockOccurrenceId::allocate();
    let event =
        TranscriptContextEvent::task_admission(2048, first, &block.label, &block.content).unwrap();
    let encoded = TranscriptContextRecord::new(event.clone(), record, None)
        .unwrap()
        .to_transcript_content()
        .unwrap();
    let decoded = TranscriptContextRecord::from_transcript_content(&encoded)
        .unwrap()
        .unwrap();
    assert_eq!(decoded.event(), &event);
    assert_eq!(decoded.record(), record);
    let mut value: Value = serde_json::from_str(
        encoded
            .strip_prefix(ACCOUNTED_CONTEXT_RECORD_MARKER)
            .unwrap(),
    )
    .unwrap();
    for fault in [
        Value::Null,
        serde_json::json!("00000000-0000-0000-0000-000000000000"),
        serde_json::json!(17),
    ] {
        value["event"]["task_start"] = fault;
        assert!(
            TranscriptContextRecord::from_transcript_content(&format!(
                "{ACCOUNTED_CONTEXT_RECORD_MARKER}{value}"
            ))
            .is_err()
        );
    }
    assert!(
        TranscriptContextEvent::task_admission(0, first, &block.label, &block.content).is_none()
    );
}

/// Metadata lookup over covered no-snapshot records skips actual semantic
/// reconstruction. Empty snapshots remain full claims; unsupported framing,
/// formats, fields and carrier semantics cannot become an absence fallback.
#[test]
fn stable_snapshot_lookup_skips_only_explicit_noncarrier_semantics() {
    let block = ContextBlock::reference_event(
        ContextSourceKind::PeerMessage,
        "peer",
        "dense 雪 {}".repeat(4_000),
    );
    let semantic = TranscriptContextEvent::prompt_boundary_with_event_sequence(
        ContextSourceKind::PeerMessage,
        1,
        "peer",
        block.content.clone(),
    )
    .unwrap();
    let record = ContextBlockRecord::allocate(&block).unwrap();
    let no_snapshot = TranscriptContextRecord::new(semantic.clone(), record, None)
        .unwrap()
        .to_transcript_content()
        .unwrap();
    let before = SEMANTIC_DECODES.with(|count| count.get());
    for _ in 0..3 {
        assert!(
            TranscriptContextRecord::stable_snapshot_from_transcript_content(&no_snapshot)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(SEMANTIC_DECODES.with(|count| count.get()), before);
    assert!(
        TranscriptContextRecord::from_transcript_content(&no_snapshot)
            .unwrap()
            .is_some()
    );
    assert_eq!(SEMANTIC_DECODES.with(|count| count.get()), before + 1);
    let carrier = TranscriptContextRecord::new(semantic, record, Some(Vec::new())).unwrap();
    let encoded = carrier.to_transcript_content().unwrap();
    for _ in 0..3 {
        assert!(TranscriptContextRecord::has_stable_snapshot(&encoded).unwrap());
    }
    assert_eq!(SEMANTIC_DECODES.with(|count| count.get()), before + 1);
    assert_eq!(
        TranscriptContextRecord::stable_snapshot_from_transcript_content(&encoded).unwrap(),
        Some(carrier)
    );
    assert_eq!(SEMANTIC_DECODES.with(|count| count.get()), before + 2);
    let value: Value = serde_json::from_str(
        no_snapshot
            .strip_prefix(ACCOUNTED_CONTEXT_RECORD_MARKER)
            .unwrap(),
    )
    .unwrap();
    for fault in ["version", "record", "field"] {
        let mut invalid = value.clone();
        match fault {
            "version" => invalid["version"] = "future".into(),
            "record" => invalid["record"]["format_version"] = 99.into(),
            _ => invalid["unknown_authority"] = true.into(),
        }
        let text = format!("{ACCOUNTED_CONTEXT_RECORD_MARKER}{invalid}");
        assert!(
            TranscriptContextRecord::stable_snapshot_from_transcript_content(&text).is_err(),
            "{fault}"
        );
    }
    let mut invalid = value;
    invalid["event"] = serde_json::json!({"version":"future", "type":"unknown"});
    let text = format!("{ACCOUNTED_CONTEXT_RECORD_MARKER}{invalid}");
    assert!(
        TranscriptContextRecord::stable_snapshot_from_transcript_content(&text)
            .unwrap()
            .is_none()
    );
    assert!(TranscriptContextRecord::from_transcript_content(&text).is_err());
    invalid["stable"] = serde_json::json!([]);
    let text = format!("{ACCOUNTED_CONTEXT_RECORD_MARKER}{invalid}");
    assert!(TranscriptContextRecord::stable_snapshot_from_transcript_content(&text).is_err());
}

/// The new wrapper round-trips occurrence/cost, while the ordinary semantic
/// decoder sees exactly the original event. Canonical import reuses checked
/// data and leaves display/provider source unchanged.
#[test]
fn accounted_context_record_round_trip_keeps_semantic_content_separate() {
    let mut context = AgentContext::empty();
    context
        .append_user_event("prompt", "exact é😀 user content")
        .unwrap();
    let source = &context.chronology()[0];
    let semantic = TranscriptContextEvent::user_event(
        source.sequence().get(),
        source.block().label.clone(),
        source.block().content.clone(),
    )
    .unwrap();
    let record = TranscriptContextRecord::new(
        semantic.clone(),
        source.record(),
        Some(context.stable_block_records()),
    )
    .unwrap();
    let text = record.to_transcript_content().unwrap();
    assert_eq!(
        TranscriptContextEvent::from_transcript_content(&text),
        Some(semantic)
    );
    let restored = TranscriptContextRecord::from_transcript_content(&text)
        .unwrap()
        .unwrap();
    assert_eq!(record, restored);
    let imported = AgentContext::import_durable_blocks_with_records(
        context.blocks().to_vec(),
        &[(0, restored.record())],
        &[],
    )
    .unwrap();
    assert_eq!(context.block_records(), imported.block_records());
    assert_eq!(context.blocks(), imported.blocks());
    assert!(!format!("{record:?}").contains("exact"));
}

/// Future/malformed record envelopes fail explicitly rather than becoming
/// partial replay. Legacy semantic data stays readable but supplies no new
/// occurrence/cache claim. Unknown metadata cannot become semantic source.
#[test]
fn accounted_record_rejects_future_and_malformed_frames_without_legacy_upgrade_claim() {
    let block = ContextBlock::reference_event(ContextSourceKind::PeerMessage, "peer", "source");
    let semantic = TranscriptContextEvent::prompt_boundary_with_event_sequence(
        ContextSourceKind::PeerMessage,
        1,
        "peer",
        "source",
    )
    .unwrap();
    let legacy = semantic.to_transcript_content();
    assert!(
        TranscriptContextRecord::from_transcript_content(&legacy)
            .unwrap()
            .is_none()
    );
    let record = TranscriptContextRecord::new(
        semantic,
        ContextBlockRecord::allocate(&block).unwrap(),
        None,
    )
    .unwrap();
    let text = record.to_transcript_content().unwrap();
    for malformed in [
        text.replace("context-record/v1", "context-record/v2"),
        format!("{ACCOUNTED_CONTEXT_RECORD_MARKER}{{\"version\":\"{VERSION}\"}}"),
        text.replace("\"version\":", "\"extra\":true,\"version\":"),
    ] {
        assert!(TranscriptContextRecord::from_transcript_content(&malformed).is_err());
        assert!(TranscriptContextEvent::from_transcript_content(&malformed).is_none());
    }
}
