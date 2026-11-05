//! Persistent accounted records preserve semantic projection and origin while
//! keeping costs quarantined until the actual canonical source is restored.

use super::*;
use crate::{AgentContext, ContextBlock, ContextSourceKind};

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
