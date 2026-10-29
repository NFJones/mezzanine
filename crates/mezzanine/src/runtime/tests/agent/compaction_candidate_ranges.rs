//! Prospective manual history versus actual prefix publication and replay.
//!
//! Real saved selective ranges and complete native groups feed the production
//! manual planner. The same frozen task is sized, completed and replayed. Exact
//! optional references are supplied at the same pre-history boundary on both
//! requests; they are not summary epochs or source-publication authority.

use super::*;
use crate::integrations::agent::context::assemble_model_request;
use mez_agent::{AgentContext, ContextPlacement, ContextRetention};

/// The actual planner must preserve candidate/published parity with no ranges,
/// an absorbed range, a surviving range, both, multiple surviving ranges and an
/// existing prefix. Native call/result ownership, protected reference metadata,
/// provider switching, model changes and complete serialized request estimates
/// must remain identical. One accepted summary must not enqueue a shrink retry.
/// Incomplete frozen prefixes and backwards cuts must reject without mutation.
#[test]
fn runtime_manual_candidate_ranges_match_complete_published_requests() {
    for (name, prefix, absorbed, surviving) in [
        ("none", false, false, 0),
        ("absorbed", false, true, 0),
        ("surviving", false, false, 1),
        ("mixed", false, true, 1),
        ("multiple-surviving", false, false, 2),
        ("prefix-mixed", true, true, 1),
    ] {
        for worker in [false, true] {
            qualify_candidate_ranges(name, prefix, absorbed, surviving, None, false, worker);
        }
    }
}

/// Two independently admitted fixtures establish and then use a narrow real
/// request allowance. The exact runtime estimator includes tools and controls.
/// Correct replay fits with small headroom; one duplicate retained range would
/// exceed it, yet actual completion publishes without a new provider/shrink job.
#[test]
fn runtime_manual_candidate_retained_ranges_fit_without_shrink_retry() {
    for worker in [false, true] {
        let input = qualify_candidate_ranges("near-fit", false, false, 2, None, true, worker);
        let configured_limit = ((input + 32) * 20).div_ceil(19);
        qualify_candidate_ranges(
            "near-fit",
            false,
            false,
            2,
            Some(configured_limit),
            true,
            worker,
        );
    }
}

/// Adds the same neutral optional memory and exact reference before history,
/// retaining all existing stable-slot and chronological metadata. A generic
/// UUID memory remains ordinary even though a controller summary also has UUIDs.
fn with_exact_references(mut context: AgentContext, memory_label: &str) -> AgentContext {
    let mut history = context.clone();
    history
        .retain_blocks_with_metadata(|block, _| {
            block.placement == ContextPlacement::ConversationAppend
        })
        .unwrap();
    context
        .retain_blocks_with_metadata(|block, _| block.placement == ContextPlacement::StablePrefix)
        .unwrap();
    context
        .append_reference_event(
            ContextSourceKind::Memory,
            memory_label,
            "ordinary optional memory",
        )
        .unwrap();
    context
        .append_reference_event(
            ContextSourceKind::PersistedContextDocument,
            "candidate exact reference",
            "preserve exact reference bytes",
        )
        .unwrap();
    context.append_imported_history(&history).unwrap();
    context
}

/// Runs one genuine frozen planner task through candidate and actual completion.
/// Turn grouping determines whether the first selective range is consumed;
/// large input allowance keeps later complete groups raw without timing tricks.
fn qualify_candidate_ranges(
    name: &str,
    prefix: bool,
    absorbed: bool,
    surviving: usize,
    input_limit: Option<usize>,
    near_fit: bool,
    worker: bool,
) -> usize {
    let mut service = test_runtime_service();
    let retention = if near_fit { 90 } else { 10 };
    let input_setting =
        input_limit.map_or_else(String::new, |limit| format!("max_input_tokens = {limit}\n"));
    service.replace_config_layers(vec![ConfigLayer {
        name: "candidate-ranges".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"candidate-ranges\"\ncompaction_raw_retention_percent = {retention}\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.candidate-ranges]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 128000\n{input_setting}"),
    }]).unwrap();
    let conversation = format!("candidate-ranges-{name}");
    let store = AgentTranscriptStore::new(temp_root(&conversation));
    let mut rows = Vec::new();
    if prefix {
        push_row(
            &mut rows,
            &conversation,
            TranscriptRole::Assistant,
            "old",
            "old prefix raw".into(),
        );
    }
    let offset = rows.len() as u64;
    push_row(
        &mut rows,
        &conversation,
        TranscriptRole::Assistant,
        "first",
        "first display".into(),
    );
    let mut ranges = Vec::new();
    for (index, turn) in [
        (0, if absorbed { "first" } else { "range-a" }),
        (1, "range-b"),
    ] {
        if index == 1 {
            push_row(
                &mut rows,
                &conversation,
                TranscriptRole::Assistant,
                "middle",
                "middle raw work".into(),
            );
        }
        let group = mez_agent::ContextExecutionGroupId::new(format!("range-{index}")).unwrap();
        let content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::TranscriptAssistant,
            "range answer",
            format!("range {index} raw work ").repeat(300),
            group,
            1,
            None,
        )
        .unwrap()
        .to_transcript_content();
        push_row(
            &mut rows,
            &conversation,
            TranscriptRole::System,
            turn,
            content,
        );
        if (index == 0 && (absorbed || surviving == 2)) || (index == 1 && surviving > 0) {
            let sequence = rows.len() as u64;
            ranges.push(crate::storage::transcript::AgentCompactionRange {
                first_sequence: sequence,
                through_sequence: sequence,
                summary: format!("RANGE_{index}_SUMMARY preserved facts ").repeat(50),
            });
        }
    }
    let group = mez_agent::ContextExecutionGroupId::new("retained-native-range-tail").unwrap();
    let owner = mez_agent::ProviderContinuityOwner::new(
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        "openai",
    )
    .unwrap();
    let call = mez_agent::ProviderTranscriptEvent::validated_openai_response_output(vec![
        serde_json::json!({
            "type":"function_call", "id":"fc_range_tail", "call_id":"call_range_tail",
            "name":"submit_maap_action_batch", "arguments":"{}"
        }),
    ])
    .unwrap()
    .to_transcript_content();
    let result = mez_agent::ProviderTranscriptEvent::OpenAiFunctionCallOutput {
        call_id: "call_range_tail".into(),
        output: "exact native range tail".into(),
    }
    .to_transcript_content();
    for (index, (source, label, content, provider_owner)) in [
        (
            ContextSourceKind::TranscriptAssistant,
            "tail assistant",
            "tail decision".to_string(),
            None,
        ),
        (
            ContextSourceKind::ActionResult,
            "tail result",
            "tail settled result".to_string(),
            None,
        ),
        (
            ContextSourceKind::TranscriptTool,
            "tail native",
            call,
            Some(owner.clone()),
        ),
        (
            ContextSourceKind::TranscriptTool,
            "tail native",
            result,
            Some(owner.clone()),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            source,
            label,
            content,
            group.clone(),
            index as u64 + 1,
            provider_owner,
        )
        .unwrap()
        .to_transcript_content();
        push_row(
            &mut rows,
            &conversation,
            TranscriptRole::System,
            "native-tail",
            content,
        );
    }
    for row in &rows {
        store.append(row).unwrap();
    }
    if prefix || !ranges.is_empty() {
        store
            .save_compaction_ranges(
                &conversation,
                offset,
                if prefix { "old prefix summary" } else { "" },
                ranges,
            )
            .unwrap();
    }
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, rows.len() as u64)
        .unwrap();
    if worker {
        service.use_manual_compaction_preparation_adapter();
    }
    let queued = service.dispatch_runtime_control_body(
        &format!(r#"{{"jsonrpc":"2.0","id":"{name}","method":"agent/shell/command","params":{{"idempotency_key":"{name}","input":"/compact"}}}}"#),
        &primary,
    );
    if worker {
        assert!(queued.contains("state=preparing"), "{name}: {queued}");
        let source = service.take_manual_compaction_preparations().pop().unwrap();
        let snapshot = source.execute_source();
        assert!(
            service
                .complete_manual_compaction_preparation(&source, snapshot)
                .unwrap()
        );
        let request = service.take_manual_compaction_requests().pop().unwrap();
        let rendered = request.execute_request();
        assert!(
            service
                .complete_manual_compaction_request(&request, rendered)
                .unwrap()
        );
    } else {
        assert!(queued.contains("state=queued"), "{name}: {queued}");
    }
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    let expected_boundary = offset + if absorbed { 2 } else { 1 };
    assert_eq!(
        task.compacted_through_sequence,
        Some(expected_boundary),
        "{name}: actual planner boundary"
    );
    assert_eq!(
        task.retained_transcript_entries,
        rows.iter()
            .filter(|row| row.sequence > expected_boundary)
            .count() as u64,
        "{name}: current retained counts include physical rows beneath ranges"
    );
    assert_eq!(
        task.candidate_context
            .as_ref()
            .unwrap()
            .blocks()
            .iter()
            .filter(|block| block.source == ContextSourceKind::CompactionSelective)
            .count(),
        0,
        "{name}: preparation already removed cached ranges; candidate summaries must come from rebuilt replay"
    );
    let memory_label = format!("memory {} (note)", mez_agent::memory::new_memory_uuid());
    task.candidate_context = Some(with_exact_references(
        task.candidate_context.take().unwrap(),
        &memory_label,
    ));
    let before_epoch = store.compaction_epoch(&conversation).unwrap();
    let mut incomplete = task.clone();
    assert!(!incomplete.frozen_compaction_rows.is_empty());
    incomplete.frozen_compaction_rows.remove(0);
    let rejected = service.manual_compaction_candidate_context(&incomplete, "new prefix summary");
    assert!(
        rejected.is_err(),
        "{name}: candidate accepted an incomplete frozen prefix that publication must reject"
    );
    assert_eq!(store.compaction_epoch(&conversation).unwrap(), before_epoch);
    if prefix {
        let mut invalid = task.clone();
        invalid.compacted_through_sequence = Some(offset - 1);
        let error = service
            .manual_compaction_candidate_context(&invalid, "new prefix summary")
            .unwrap_err();
        assert!(error.message().contains("cannot move backwards"), "{error}");
        assert_eq!(store.compaction_epoch(&conversation).unwrap(), before_epoch);
    }
    let candidate = service
        .manual_compaction_candidate_context(&task, "new prefix summary")
        .unwrap();
    assert_eq!(
        store.compaction_epoch(&conversation).unwrap(),
        before_epoch,
        "{name}: sizing must not publish"
    );
    let selective = candidate
        .blocks()
        .iter()
        .filter(|block| block.source == ContextSourceKind::CompactionSelective)
        .collect::<Vec<_>>();
    assert_eq!(
        selective.len(),
        surviving,
        "{name}: each retained range must come from rebuilt replay exactly once: {selective:?}"
    );
    assert_eq!(
        candidate
            .blocks()
            .iter()
            .filter(|block| block.source == ContextSourceKind::CompactionPrefix)
            .count(),
        1,
        "{name}"
    );
    let ordinary = candidate
        .chronology()
        .iter()
        .find(|event| event.block().label == memory_label)
        .unwrap();
    assert_eq!(ordinary.retention(), ContextRetention::Exact, "{name}");
    assert!(ordinary.execution_group_id().is_none());
    assert!(!mez_agent::context_block_is_compaction_summary(
        ordinary.block()
    ));
    assert_eq!(
        candidate
            .chronology()
            .iter()
            .filter(|event| event.provider_owner() == Some(&owner))
            .count(),
        2,
        "{name}"
    );
    let (input, safe_limit, _) = service
        .manual_compaction_candidate_size_for_tests(&task, candidate.clone())
        .unwrap();
    if near_fit && input_limit.is_some() {
        assert!(
            input <= safe_limit,
            "actual request must fit: input={input} limit={safe_limit}"
        );
        assert!(
            safe_limit - input <= 64,
            "fixture must stay near the actual limit"
        );
        let mut inflated = candidate.clone();
        let duplicate = (*selective[0]).clone();
        inflated
            .insert_typed_block(
                duplicate.clone(),
                duplicate.semantic_kind(),
                duplicate.retention(),
                duplicate.recoverable_for_compaction(),
            )
            .unwrap();
        let (inflated_input, _, _) = service
            .manual_compaction_candidate_size_for_tests(&task, inflated)
            .unwrap();
        assert!(
            inflated_input > safe_limit,
            "duplicate summary must fail the same real request bound"
        );
    }
    service.claim_agent_compaction_task_state("%1", task.clone());
    assert!(
        service
            .apply_agent_compaction_completed_event(
                "%1",
                runtime_test_compaction_response("new prefix summary")
            )
            .unwrap()
    );
    assert!(
        service.pending_agent_compaction_tasks().is_empty(),
        "{name}: no unnecessary shrink retry"
    );
    let published = with_exact_references(
        service
            .agent_context_for_pane_prompt("%1", "Continue after conversation compaction.", 100)
            .unwrap(),
        &memory_label,
    )
    .with_metadata(candidate.metadata().clone());
    assert_eq!(
        candidate.blocks(),
        published.blocks(),
        "{name}: complete canonical parity"
    );
    for (before, after) in candidate.chronology().iter().zip(published.chronology()) {
        assert_eq!(before.semantic_kind(), after.semantic_kind(), "{name}");
        assert_eq!(before.retention(), after.retention(), "{name}");
        assert_eq!(
            before.execution_group_id(),
            after.execution_group_id(),
            "{name}"
        );
        assert_eq!(before.provider_owner(), after.provider_owner(), "{name}");
    }
    let turn = AgentTurnRecord {
        turn_id: "candidate-range-parity".into(),
        conversation_id: conversation,
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        deadline_at_unix_millis: 0,
        policy_profile: "runtime".into(),
        model_profile: task.model_profile_name.clone(),
        parent_turn_id: None,
        state: AgentTurnState::Queued,
        cooperation_mode: None,
        initial_capability: None,
    };
    for (provider, model) in [
        ("openai", "test"),
        ("switched-openai", "test"),
        ("openai", "changed-model"),
    ] {
        let mut profile = task.model_profile.clone();
        profile.provider = provider.into();
        profile.model = model.into();
        let api = mez_agent::ProviderApiCompatibility::OpenAiResponses;
        let before = assemble_model_request(&profile, api, &turn, &candidate).unwrap();
        let after = assemble_model_request(&profile, api, &turn, &published).unwrap();
        let before = mez_agent::openai_responses_request_body(&before).unwrap();
        let after = mez_agent::openai_responses_request_body(&after).unwrap();
        assert_eq!(
            before, after,
            "{name}: complete wire parity for {provider}/{model}"
        );
        assert_eq!(
            mez_agent::provider_request_input_estimate_from_body(&before),
            mez_agent::provider_request_input_estimate_from_body(&after)
        );
        assert_eq!(
            before.contains("call_range_tail"),
            provider == "openai",
            "{name}"
        );
    }
    input
}

/// Assigns a contiguous physical source row in one task-generated conversation.
fn push_row(
    rows: &mut Vec<TranscriptEntry>,
    conversation: &str,
    role: TranscriptRole,
    turn: &str,
    content: String,
) {
    let sequence = rows.len() as u64 + 1;
    rows.push(TranscriptEntry {
        conversation_id: conversation.into(),
        sequence,
        created_at_unix_seconds: sequence,
        role,
        turn_id: turn.into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content,
    });
}
