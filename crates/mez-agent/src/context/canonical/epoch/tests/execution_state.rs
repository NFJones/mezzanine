//! Pure tests supply original producer state at the documented qualified replay
//! port. Product storage must independently establish that qualification; these
//! fixtures grant no execution, original live handle or persistence receipt.

use super::*;
use serde_json::{Value, json};

fn execution() -> (
    ReplayEpochContext,
    ContextExecutionGroupId,
    ReplayExecutionOwner,
) {
    let mut source = owner();
    let group = ContextExecutionGroupId::new("original-execution").unwrap();
    let frontier = source
        .append_execution_events([ContextConversationAppend::assistant(
            "response",
            "original assistant source",
            group.clone(),
        )])
        .unwrap()[0];
    let receipt = source.capture_execution_owner(&group, frontier).unwrap();
    (source, group, receipt)
}

fn cold(source: &ReplayEpochContext) -> ReplayEpochContext {
    ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        source.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap()
}

#[test]
fn execution_state_cold_identity_never_restores_original_live_handle_or_closure_proof() {
    let (mut source, group, receipt) = execution();
    let pending = cold(&source);
    assert_eq!(
        pending.execution_states().unwrap(),
        source.execution_states().unwrap()
    );
    assert!(pending.clone().finish_execution_group(&receipt).is_err());
    source.finish_execution_group(&receipt).unwrap();
    let closed = source.execution_states().unwrap().pop().unwrap();
    assert!(!closed.is_pending());
    let mut descriptive = cold(&source);
    assert!(descriptive.execution_states().unwrap().is_empty());
    assert!(!descriptive.restore_execution_state(&closed).unwrap());
    assert!(descriptive.validate_source_extension(&pending).is_err());
    assert!(source.validate_source_extension(&pending).is_ok());
    let mut restored = pending.clone();
    assert!(restored.restore_execution_state(&closed).unwrap());
    assert!(!restored.context().execution_group_is_pending(&group));
    assert!(restored.validate_source_extension(&pending).is_ok());
    assert!(!restored.execution_group_finished(&receipt));
}

#[test]
fn execution_state_binds_only_complete_original_imported_members() {
    let (mut source, group, receipt) = execution();
    source.finish_execution_group(&receipt).unwrap();
    let state = source.execution_states().unwrap().pop().unwrap();
    let original = source.context().chronology()[0].clone();
    let mut body = owner().into_owned_context();
    let imported =
        crate::ImportedExecutionEvent::new(original.block().clone(), group.clone(), 1, None)
            .unwrap()
            .with_record(original.record())
            .unwrap();
    body.append_imported_replay_arrivals(
        vec![original.block().clone()],
        &[(0, original.record())],
        &[imported],
        &[(0, original.sequence())],
    )
    .unwrap();
    let mut restored =
        ReplayEpochContext::from_owned_context(source.conversation_id().clone(), body).unwrap();
    assert!(restored.execution_states().unwrap().is_empty());
    assert!(restored.context().execution_group_is_pending(&group));
    assert!(restored.restore_execution_state(&state).unwrap());
    assert_eq!(restored.execution_states().unwrap(), vec![state.clone()]);
    assert_eq!(
        restored.context().chronology(),
        source.context().chronology()
    );
    let before = restored.clone();
    let mut forged = serde_json::to_value(&state).unwrap();
    forged["members"][0][0] = json!(ContextBlockOccurrenceId::allocate());
    let forged = serde_json::from_value(forged).unwrap();
    assert!(restored.restore_execution_state(&forged).is_err());
    assert_eq!(restored, before);
}

#[test]
fn execution_state_old_closure_cannot_release_cold_renewed_interval_or_later_source() {
    let (mut source, group, receipt) = execution();
    source.finish_execution_group(&receipt).unwrap();
    let old = source.execution_states().unwrap().pop().unwrap();
    let mut body = source.into_owned_context();
    body.retain_pending_execution_group(&group).unwrap();
    let mut renewed = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body,
    )
    .unwrap();
    let current = renewed.execution_states().unwrap().pop().unwrap();
    assert_eq!(current.generation(), old.generation() + 1);
    let mut reopened = cold(&renewed);
    let before = reopened.clone();
    assert!(!reopened.restore_execution_state(&old).unwrap());
    assert_eq!(reopened, before);
    let mut conflict = serde_json::to_value(&old).unwrap();
    conflict["interval"]["generation"] = json!(current.generation());
    let conflict = serde_json::from_value(conflict).unwrap();
    assert!(reopened.restore_execution_state(&conflict).is_err());
    assert_eq!(reopened, before);
    let mut recycled = serde_json::to_value(&current).unwrap();
    recycled["interval"]["generation"] = json!(current.generation() + 1);
    recycled["pending"] = json!(false);
    let recycled = serde_json::from_value(recycled).unwrap();
    assert!(reopened.restore_execution_state(&recycled).is_err());
    assert_eq!(reopened, before);
    let frontier = renewed.group_frontier(&group).unwrap();
    let new_receipt = renewed.capture_execution_owner(&group, frontier).unwrap();
    renewed.finish_execution_group(&new_receipt).unwrap();
    let closed = renewed.execution_states().unwrap().pop().unwrap();
    assert!(reopened.restore_execution_state(&closed).unwrap());
    // A delayed pending snapshot from the same interval cannot reopen a closure.
    assert!(!reopened.restore_execution_state(&current).unwrap());
    reopened
        .append_execution_events([ContextConversationAppend::assistant(
            "later",
            "new original group source",
            group.clone(),
        )])
        .unwrap();
    assert!(!reopened.restore_execution_state(&closed).unwrap());
    assert!(reopened.context().execution_group_is_pending(&group));
}

#[test]
fn execution_state_native_closure_requires_original_complete_pairing() {
    let mut source = owner();
    let group = ContextExecutionGroupId::new("native-pending").unwrap();
    let provider = crate::ProviderContinuityOwner::new(
        crate::ProviderApiCompatibility::OpenAiChatCompletions,
        "fixture",
    )
    .unwrap();
    let call = crate::ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call("fixture".into(), "original call".into(), vec![json!({"id":"call","type":"function","function":{"name":crate::MAAP_ACTION_BATCH_TOOL_NAME,"arguments":"{}"}})]).unwrap();
    source
        .append_execution_events([ContextConversationAppend::provider_assistant(
            "native declaration",
            call.to_transcript_content(),
            group.clone(),
            provider,
        )])
        .unwrap();
    let pending = source.execution_states().unwrap().pop().unwrap();
    let mut claimed = serde_json::to_value(pending).unwrap();
    claimed["pending"] = json!(false);
    let claimed = serde_json::from_value(claimed).unwrap();
    let before = source.clone();
    assert!(source.restore_execution_state(&claimed).is_err());
    assert_eq!(source, before);
}

#[test]
fn execution_state_checkpoint_format_is_explicit_and_legacy_intervals_stay_unknown() {
    let (mut source, group, receipt) = execution();
    let earlier_pending = source.execution_states().unwrap().pop().unwrap();
    source.finish_execution_group(&receipt).unwrap();
    let closed = source.execution_states().unwrap().pop().unwrap();
    let encoded = serde_json::to_value(source.checkpoint().unwrap()).unwrap();
    assert_eq!(encoded["version"], json!(5));
    for (pointer, value) in [
        ("/execution_intervals", Value::Null),
        ("/execution_intervals/0/1/generation", json!(0)),
        ("/execution_intervals/0/1/identity", Value::Null),
        ("/execution_intervals/0/0", json!("absent-group")),
    ] {
        let mut broken = encoded.clone();
        *broken.pointer_mut(pointer).unwrap() = value;
        let checkpoint = serde_json::from_value(broken).unwrap();
        assert!(
            ReplayEpochContext::from_checkpoint(
                source.conversation_id().clone(),
                checkpoint,
                AgentContext::empty()
            )
            .is_err(),
            "{pointer}"
        );
    }
    for version in [1, 2] {
        let mut legacy = encoded.clone();
        legacy["version"] = json!(version);
        legacy.as_object_mut().unwrap().remove("compaction_reentry");
        legacy
            .as_object_mut()
            .unwrap()
            .remove("execution_intervals");
        if version == 1 {
            legacy
                .as_object_mut()
                .unwrap()
                .remove("retired_mcp_retrievals");
        }
        let checkpoint = serde_json::from_value(legacy).unwrap();
        let mut restored = ReplayEpochContext::from_checkpoint(
            source.conversation_id().clone(),
            checkpoint,
            AgentContext::empty(),
        )
        .unwrap();
        assert!(restored.execution_states().unwrap().is_empty());
        let original = restored.clone();
        assert!(restored.restore_execution_state(&earlier_pending).is_err());
        assert_eq!(restored, original);
        let mut body = restored.clone().into_owned_context();
        let before = body.clone();
        assert!(body.retain_pending_execution_group(&group).is_err());
        assert_eq!(body, before);
        // Explicit original-source qualification binds descriptive ancestry;
        // the cold closed state still cannot export a live closure witness.
        assert!(restored.restore_execution_state(&closed).unwrap());
        assert!(restored.execution_states().unwrap().is_empty());
        let mut body = restored.into_owned_context();
        body.retain_pending_execution_group(&group).unwrap();
        let restored =
            ReplayEpochContext::from_owned_context(source.conversation_id().clone(), body).unwrap();
        assert_eq!(restored.execution_states().unwrap()[0].generation(), 2);
    }
}

#[test]
fn execution_state_source_interval_cannot_rewind_and_renewal_cannot_wrap() {
    let (mut source, group, receipt) = execution();
    source.finish_execution_group(&receipt).unwrap();
    let mut body = source.into_owned_context();
    body.retain_pending_execution_group(&group).unwrap();
    let source = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body,
    )
    .unwrap();
    let checkpoint = serde_json::to_value(source.checkpoint().unwrap()).unwrap();
    assert!(cold(&source).validate_source_extension(&source).is_ok());
    let mut rewind = checkpoint.clone();
    rewind["execution_intervals"][0][1]["generation"] = json!(1);
    let rewind = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        serde_json::from_value(rewind).unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(rewind.validate_source_extension(&source).is_err());
    let mut exhausted = checkpoint;
    exhausted["execution_intervals"][0][1]["generation"] = json!(u64::MAX);
    exhausted["pending_execution_groups"] = json!([]);
    let exhausted = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        serde_json::from_value(exhausted).unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    let mut body = exhausted.into_owned_context();
    let before = body.clone();
    assert!(body.retain_pending_execution_group(&group).is_err());
    assert_eq!(body, before);
}

#[test]
fn execution_state_imported_renewal_uses_original_producer_identity() {
    let (mut source, group, receipt) = execution();
    source.finish_execution_group(&receipt).unwrap();
    let old_closed = source.execution_states().unwrap().pop().unwrap();
    let parent = cold(&source);
    source
        .append_execution_events([ContextConversationAppend::assistant(
            "later response",
            "new source after original closure",
            group.clone(),
        )])
        .unwrap();
    let original_pending = source.execution_states().unwrap().pop().unwrap();
    let event = source.context().chronology().last().unwrap().clone();
    let mut body = parent.clone().into_owned_context();
    let metadata =
        crate::ImportedExecutionEvent::new(event.block().clone(), group.clone(), 2, None)
            .unwrap()
            .with_record(event.record())
            .unwrap();
    body.append_imported_replay_arrivals(
        vec![event.block().clone()],
        &[(0, event.record())],
        &[metadata],
        &[(0, event.sequence())],
    )
    .unwrap();
    let mut restored =
        ReplayEpochContext::from_owned_context(source.conversation_id().clone(), body).unwrap();
    // The provisional import survives restart without claiming a producer ID.
    assert!(restored.execution_states().unwrap().is_empty());
    assert!(restored.validate_source_extension(&parent).is_ok());
    restored = cold(&restored);
    assert!(restored.execution_states().unwrap().is_empty());
    assert!(restored.validate_source_extension(&parent).is_ok());
    let mut stale_full = serde_json::to_value(&original_pending).unwrap();
    stale_full["interval"] = serde_json::to_value(&old_closed).unwrap()["interval"].clone();
    stale_full["pending"] = json!(false);
    let stale_full = serde_json::from_value(stale_full).unwrap();
    let before = restored.clone();
    assert!(!restored.restore_execution_state(&stale_full).unwrap());
    assert_eq!(restored, before);
    let live = restored
        .capture_execution_owner(&group, event.sequence())
        .unwrap();
    assert!(restored.clone().finish_execution_group(&live).is_ok());
    assert!(restored.restore_execution_state(&original_pending).unwrap());
    // Qualifying descriptive identity changes neither the admitted source nor
    // this actual protection interval; it must preserve its captured live fence.
    let mut verified = restored.clone();
    verified.finish_execution_group(&live).unwrap();
    let verified_closed = verified.execution_states().unwrap().pop().unwrap();
    assert_eq!(restored.execution_states().unwrap(), vec![original_pending]);
    assert_eq!(
        restored.context().chronology(),
        source.context().chronology()
    );
    assert!(!restored.restore_execution_state(&old_closed).unwrap());
    assert!(restored.context().execution_group_is_pending(&group));
    let receipt = source
        .capture_execution_owner(&group, event.sequence())
        .unwrap();
    source.finish_execution_group(&receipt).unwrap();
    let closed = source.execution_states().unwrap().pop().unwrap();
    assert_eq!(verified_closed, closed);
    assert!(restored.restore_execution_state(&closed).unwrap());
    assert!(!restored.context().execution_group_is_pending(&group));
}
