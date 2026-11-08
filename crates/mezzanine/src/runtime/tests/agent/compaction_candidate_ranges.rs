//! Canonical manual candidate/request parity and explicit legacy admission.
//!
//! Original selective layouts remain refusal controls. Successful cases use
//! actual typed source and receipts, preserving native custody, exact references
//! and complete wire cost through inline and worker preparation.

use super::compaction_candidate::CanonicalManualFixture;
use super::*;
use crate::integrations::agent::context::assemble_model_request;

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
            qualify_legacy_admission(name, prefix, absorbed, surviving, None, false, worker);
        }
    }
    for worker in [false, true] {
        for headless in [false, true] {
            qualify_canonical_candidate(headless, worker, None, false);
        }
    }
}

/// Calibrate with the real complete estimator, then admit an independent
/// fixture with very small headroom. Duplicating one exact retained reference
/// must exceed that same bound; the actual correct publication needs no repair.
#[test]
fn runtime_manual_candidate_retained_ranges_fit_without_shrink_retry() {
    for worker in [false, true] {
        qualify_legacy_admission("near-fit", false, false, 2, None, true, worker);
        let input = qualify_canonical_candidate(false, worker, None, false);
        let cap = ((input + 8) * 20).div_ceil(19);
        qualify_canonical_candidate(false, worker, Some(cap), true);
    }
}

/// Exact references and pending native source are real captured members, not
/// flat blocks inserted around a sealed history. NoStore starts without a head;
/// saved source carries one prior head that the new prefix must absorb once.
fn qualify_canonical_candidate(
    headless: bool,
    worker: bool,
    cap: Option<usize>,
    tight: bool,
) -> usize {
    let mut fixture = if headless {
        CanonicalManualFixture::headless(true)
    } else {
        CanonicalManualFixture::new(cap, 90, true)
    };
    let task = fixture.queue(worker);
    assert!(task.replay_source.is_some());
    assert!(fixture.service.agent_turn_ledger().turns().is_empty());
    let source = crate::test_support::compaction_source::rendered_source(&task).unwrap();
    assert!(source.contains("ELIGIBLE_OLD_PREFIX"));
    assert_eq!(source.contains("previous canonical head"), !headless);
    let before = fixture
        .store
        .load_replay_epoch(&fixture.conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .map(|(id, _)| id);
    assert_eq!(before.is_none(), headless);
    let candidate = fixture
        .service
        .manual_compaction_candidate_context(&task, "new prefix summary")
        .unwrap();
    assert_eq!(
        candidate
            .blocks()
            .iter()
            .filter(|block| block.source == ContextSourceKind::CompactionPrefix)
            .count(),
        1
    );
    assert!(!candidate.blocks().iter().any(|block| block.source
        == ContextSourceKind::CompactionSelective
        || block.content.contains("ELIGIBLE_OLD_PREFIX")
        || block.content.contains("COVERED_RAW_MUST_NOT_REENTER")));
    let references = candidate
        .chronology()
        .iter()
        .filter(|event| {
            matches!(
                event.block().source,
                ContextSourceKind::Memory | ContextSourceKind::PersistedContextDocument
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 2);
    for reference in &references {
        assert_eq!(reference.retention(), mez_agent::ContextRetention::Exact);
        assert!(reference.execution_group_id().is_none());
        assert!(!mez_agent::context_block_is_compaction_summary(
            reference.block()
        ));
        assert!(fixture.original.context().chronology().contains(reference));
    }
    assert!(candidate.execution_group_is_pending(&fixture.group));
    assert_eq!(
        candidate
            .chronology()
            .iter()
            .filter(|event| event.provider_owner() == Some(&fixture.owner))
            .count(),
        2
    );
    let (input, limit, _) = fixture
        .service
        .manual_compaction_candidate_size_for_tests(&task, candidate.clone())
        .unwrap();
    if tight {
        assert!(
            input <= limit && limit - input <= 16,
            "near-fit input={input} limit={limit}"
        );
        let mut inflated = candidate.clone();
        inflated
            .append_reference_event(
                references[0].block().source,
                references[0].block().label.clone(),
                references[0].block().content.clone(),
            )
            .unwrap();
        let (duplicated, _, _) = fixture
            .service
            .manual_compaction_candidate_size_for_tests(&task, inflated)
            .unwrap();
        assert!(
            duplicated > limit,
            "duplicate retained occurrence must overflow the real bound"
        );
    }
    assert_eq!(
        fixture
            .store
            .load_replay_epoch(&fixture.conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .map(|(id, _)| id),
        before
    );
    fixture.publish("new prefix summary");
    let published = fixture
        .service
        .agent_context_for_pane_prompt("%1", "Continue after conversation compaction.", 100)
        .unwrap()
        .with_metadata(candidate.metadata().clone());
    assert_eq!(candidate.blocks(), published.blocks());
    for (left, right) in candidate.chronology().iter().zip(published.chronology()) {
        assert_eq!(left.semantic_kind(), right.semantic_kind());
        assert_eq!(left.retention(), right.retention());
        assert_eq!(left.execution_group_id(), right.execution_group_id());
        assert_eq!(left.provider_owner(), right.provider_owner());
    }
    for reference in &references {
        assert!(published.chronology().contains(reference));
    }
    assert!(fixture.service.pending_agent_compaction_tasks().is_empty());
    assert!(fixture.service.pending_agent_provider_tasks().is_empty());
    assert!(fixture.service.agent_turn_ledger().turns().is_empty());
    if headless {
        assert!(
            fixture
                .store
                .load_replay_epoch(&fixture.conversation, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
    } else {
        let (_, cold) = AgentTranscriptStore::new(fixture.store.root())
            .load_replay_epoch(&fixture.conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(
            cold.context().chronology()[0].block().content,
            "new prefix summary"
        );
        for reference in &references {
            assert!(cold.context().chronology().contains(reference));
        }
    }
    let turn = AgentTurnRecord {
        turn_id: "candidate-range-parity".into(),
        conversation_id: fixture.conversation.clone(),
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
        assert_eq!(before, after, "complete parity for {provider}/{model}");
        assert_eq!(
            mez_agent::provider_request_input_estimate_from_body(&before),
            mez_agent::provider_request_input_estimate_from_body(&after)
        );
        assert_eq!(before.contains("call_retained"), provider == "openai");
        for record in &fixture.source_records {
            assert!(!before.contains(&String::from(record.occurrence_id())));
        }
    }
    input
}

/// Preserves the original physical range layouts. Selective formats refuse
/// before model construction; bare raw native metadata retains pending custody.
fn qualify_legacy_admission(
    name: &str,
    prefix: bool,
    absorbed: bool,
    surviving: usize,
    input_limit: Option<usize>,
    near_fit: bool,
    worker: bool,
) {
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
            .install_legacy_compaction_ranges_fixture(
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
    if !prefix && !absorbed && surviving == 0 {
        let context = service
            .agent_context_for_pane_prompt("%1", "inspect original raw native history", 100)
            .unwrap();
        let canonical = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&conversation).unwrap(),
            context,
        )
        .unwrap();
        assert!(
            canonical.context().execution_group_is_pending(&group),
            "canonical adoption cannot infer closure from bare native metadata"
        );
        assert_eq!(
            canonical
                .context()
                .chronology()
                .iter()
                .filter(|event| event.provider_owner() == Some(&owner))
                .count(),
            2
        );
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert_eq!(store.inspect(&conversation).unwrap(), rows);
        return;
    }
    let paths = [
        store.transcript_path(&conversation).unwrap(),
        store
            .root()
            .join(&conversation)
            .join("compaction-epoch.json"),
    ];
    let before = paths.each_ref().map(|path| std::fs::read(path).unwrap());
    if worker {
        service.use_agent_source_preparation_adapter();
    }
    let result = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    if worker {
        assert!(result.contains("state=preparing"), "{name}: {result}");
        let source = service.take_manual_compaction_preparations().pop().unwrap();
        let captured = source.execute_source();
        assert!(
            captured
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("normalization")
        );
        let error = service
            .complete_manual_compaction_preparation(&source, captured)
            .unwrap_err();
        assert!(
            error.to_string().contains("normalization"),
            "{name}: {error}"
        );
    } else {
        assert!(result.contains("normalization"), "{name}: {result}");
    }
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.take_manual_compaction_requests().is_empty());
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        paths.each_ref().map(|path| std::fs::read(path).unwrap()),
        before
    );
    assert_eq!(store.inspect(&conversation).unwrap(), rows);
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
