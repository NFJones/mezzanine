//! Runtime observations retain source/costs without minting execution fences.

use super::*;
use crate::{ContextBlockRecord, ImportedExecutionEvent};

/// An observation is admitted through the public live append boundary while an
/// independent assistant group keeps its original pending source owner. The
/// observation supplies no closure handle and cannot retire that execution.
#[test]
fn replay_observation_keeps_existing_execution_protection_and_cached_record() {
    let mut source = owner();
    let native = ContextExecutionGroupId::new("actual-pending-assistant").unwrap();
    let frontier = source
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "await the actual owner",
            native.clone(),
        )])
        .unwrap()[0];
    let receipt = source.capture_execution_owner(&native, frontier).unwrap();
    let before = source.clone();
    let observation = ContextExecutionGroupId::new("runtime-observation").unwrap();
    let mut body = source.into_owned_context();
    let sequence = body
        .append_evidence_event(
            ContextSourceKind::CommittedEvidence,
            "runtime counters",
            "one actual provider call",
            observation.clone(),
            None,
            true,
        )
        .unwrap();
    let record = body
        .chronology()
        .iter()
        .find(|event| event.sequence() == sequence)
        .unwrap()
        .record();
    assert!(body.execution_group_is_pending(&native));
    assert!(!body.execution_group_is_pending(&observation));
    assert!(body.replay_execution_append_owner(&observation).is_none());
    let mut source = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body,
    )
    .unwrap();
    source.validate_source_extension(&before).unwrap();
    assert!(
        source
            .capture_execution_owner(&observation, sequence)
            .is_err()
    );
    assert_eq!(
        source
            .context()
            .chronology()
            .iter()
            .find(|event| event.sequence() == sequence)
            .unwrap()
            .record(),
        record
    );
    assert_eq!(
        source
            .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
            .unwrap(),
        ReplayCompactionPreparation::NoWork(ReplayCompactionNoWork::ProtectedPrefix)
    );
    source.finish_execution_group(&receipt).unwrap();
    let plan = plan(&source);
    assert!(
        plan.summary_input()
            .iter()
            .any(|event| event.record() == record)
    );
    source
        .publish_compaction(
            plan,
            ReplayHeadPublication::Summary("observed provider counters".into()),
        )
        .unwrap();
}

/// Checked cold arrivals retain the producer's exact observation record and
/// estimate, but cannot mint an action/native source owner. Orphaned action or
/// tool evidence still fails atomically through that same public import port.
#[test]
fn replay_observation_import_preserves_accounting_and_rejects_orphaned_tool_evidence() {
    let group = ContextExecutionGroupId::new("imported-observation").unwrap();
    let block = ContextBlock::evidence_event(
        ContextSourceKind::CommittedEvidence,
        "runtime counters",
        "one actual provider call",
    );
    let record = ContextBlockRecord::allocate(&block).unwrap();
    let imported = ImportedExecutionEvent::new(block.clone(), group.clone(), 1, None)
        .unwrap()
        .with_record(record)
        .unwrap();
    let mut body = owner().into_owned_context();
    body.append_imported_replay_arrivals(vec![block], &[(0, record)], &[imported], &[])
        .unwrap();
    assert!(!body.execution_group_is_pending(&group));
    assert!(body.replay_execution_append_owner(&group).is_none());
    let source = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body.clone(),
    )
    .unwrap();
    assert_eq!(source.context().chronology()[0].record(), record);
    assert!(
        source
            .capture_execution_owner(&group, source.context().chronology()[0].sequence())
            .is_err()
    );
    for kind in [
        ContextSourceKind::ActionResult,
        ContextSourceKind::TranscriptTool,
    ] {
        let before = body.clone();
        let block = ContextBlock::evidence_event(
            kind,
            "orphaned result",
            "must keep its real assistant owner",
        );
        let record = ContextBlockRecord::allocate(&block).unwrap();
        let imported = ImportedExecutionEvent::new(
            block.clone(),
            ContextExecutionGroupId::new("missing-assistant").unwrap(),
            1,
            None,
        )
        .unwrap()
        .with_record(record)
        .unwrap();
        assert!(
            body.append_imported_replay_arrivals(vec![block], &[(0, record)], &[imported], &[])
                .is_err()
        );
        assert_eq!(body, before);
    }
}
