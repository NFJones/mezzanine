//! A control remains metadata, including when it describes an actual closure.

use super::*;
use crate::transcript::{TranscriptContextRecord, is_accounted_context_record};
use crate::{
    AgentContext, ContextConversationAppend, ContextExecutionGroupId, ReplayConversationId,
    ReplayEpochContext,
};

fn control() -> TranscriptExecutionStateRecord {
    let mut source = ReplayEpochContext::new(
        ReplayConversationId::new("control-codec").unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    let group = ContextExecutionGroupId::new("original-group").unwrap();
    let frontier = source
        .append_execution_events([ContextConversationAppend::assistant(
            "response",
            "original source",
            group.clone(),
        )])
        .unwrap()[0];
    let fence = source.capture_execution_owner(&group, frontier).unwrap();
    source.finish_execution_group(&fence).unwrap();
    TranscriptExecutionStateRecord::new(source.execution_states().unwrap().pop().unwrap()).unwrap()
}

#[test]
fn execution_control_codec_has_no_model_occurrence_or_stable_snapshot() {
    let original = control();
    let content = original.to_transcript_content().unwrap();
    assert!(is_accounted_context_record(&content));
    assert_eq!(
        TranscriptExecutionStateRecord::from_transcript_content(&content).unwrap(),
        Some(original)
    );
    assert!(
        TranscriptContextRecord::from_transcript_content(&content)
            .unwrap()
            .is_none()
    );
    assert!(!TranscriptContextRecord::has_stable_snapshot(&content).unwrap());
    assert!(
        TranscriptContextRecord::stable_snapshot_from_transcript_content(&content)
            .unwrap()
            .is_none()
    );
    assert!(!content.contains("\"record\""));
    assert!(!content.contains("\"stable\""));
}

#[test]
fn execution_control_codec_refuses_malformed_and_future_framing() {
    let content = control().to_transcript_content().unwrap();
    let wire: serde_json::Value =
        serde_json::from_str(content.strip_prefix(EXECUTION_STATE_RECORD_MARKER).unwrap()).unwrap();
    for (pointer, value) in [
        ("/version", serde_json::json!("future")),
        ("/state/interval/provisional", serde_json::json!(true)),
        ("/state/members", serde_json::json!([])),
    ] {
        let mut broken = wire.clone();
        *broken.pointer_mut(pointer).unwrap() = value;
        let broken = format!("{EXECUTION_STATE_RECORD_MARKER}{broken}");
        assert!(TranscriptExecutionStateRecord::from_transcript_content(&broken).is_err());
        assert!(TranscriptContextRecord::from_transcript_content(&broken).is_err());
        assert!(TranscriptContextRecord::has_stable_snapshot(&broken).is_err());
    }
    let future = content.replacen("/v2]", "/v3]", 1);
    assert!(TranscriptContextRecord::from_transcript_content(&future).is_err());
    let oversized = format!("{EXECUTION_STATE_RECORD_MARKER}{}", " ".repeat(MAX_BYTES));
    assert!(TranscriptExecutionStateRecord::from_transcript_content(&oversized).is_err());
    assert!(
        TranscriptExecutionStateRecord::from_transcript_content("ordinary text")
            .unwrap()
            .is_none()
    );
}
