//! Complete instruction snapshots and cache lineage at prompt boundaries.
//!
//! This leaf owns guidance ordering, all-scope inclusion and refresh identity.
//! Accepted provider epochs remain frozen while pre-request changes derive their
//! cache lineage from actual chronological snapshot bytes, not removed slots.

use super::*;

/// Builds one discovered repository-instruction fixture.
fn project_instruction(content: &str) -> mez_agent::instructions::DiscoveredInstructionFile {
    mez_agent::instructions::DiscoveredInstructionFile {
        path: "/repo/AGENTS.md".to_string(),
        scope_root: "/repo".to_string(),
        bytes: content.len(),
        truncated: false,
        content: content.to_string(),
    }
}

/// Verifies project-guidance discovery is an exact no-op until source content
/// changes before the first provider response, then appends a successor
/// task-prelude snapshot and records a new cache lineage diagnostic.
#[test]
fn runtime_project_guidance_refresh_preserves_or_rewrites_cache_lineage_by_fingerprint() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .set_pane_agent_instruction_files("%1", vec![project_instruction("Preserve chronology.")]);
    let started = service
        .start_agent_prompt_turn("%1", "inspect context ordering")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let original = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .clone();

    service
        .refresh_agent_turn_project_guidance_context(&turn)
        .unwrap();
    assert_eq!(
        service.agent_turn_contexts().get(&turn.turn_id).unwrap(),
        &original
    );

    service.set_pane_agent_instruction_files(
        "%1",
        vec![project_instruction(
            "Preserve chronology and causal ownership.",
        )],
    );
    service
        .refresh_agent_turn_project_guidance_context(&turn)
        .unwrap();
    let changed = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert_ne!(changed, &original);
    assert!(changed.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ProjectGuidance
            && block
                .content
                .contains("Preserve chronology and causal ownership.")
    }));
    assert_ne!(
        changed.metadata().prompt_cache_lineage_id,
        original.metadata().prompt_cache_lineage_id
    );
    assert_ne!(changed.chronology(), original.chronology());
    let trace = service.agent_pane_trace_log_text("%1").unwrap();
    assert!(
        trace.contains("context lineage changed") && trace.contains("project guidance"),
        "{trace}"
    );
}

/// Verifies project-guidance changes cannot rewrite front-loaded instructions
/// after the turn has accepted its first provider response.
///
/// The current turn keeps its original snapshot and cache lineage. A later
/// user turn will rebuild context from the newly discovered instruction file.
#[test]
fn runtime_project_guidance_refresh_defers_changes_during_active_cache_epoch() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .set_pane_agent_instruction_files("%1", vec![project_instruction("Preserve chronology.")]);
    let started = service
        .start_agent_prompt_turn("%1", "inspect context ordering")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .cloned()
        .unwrap();
    let original = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .cloned()
        .unwrap();
    let request = crate::integrations::agent::context::assemble_model_request(
        &runtime_model_profile("openai", "gpt-test"),
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &turn,
        &original,
    )
    .unwrap();
    service.agent_turn_executions_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: request.clone(),
            response: mez_agent::ModelResponse {
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
                raw_text: "accepted response".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: Default::default(),
            action_results: Vec::new(),
            final_turn: false,
            terminal_state: mez_agent::AgentTurnState::Running,
        },
    );
    let (prepared, _) = service
        .prepare_agent_turn_model_context(
            &turn,
            original.clone(),
            &service.mcp_registry().prompt_summary(),
            &runtime_model_profile("openai", "gpt-test"),
        )
        .unwrap();
    assert_eq!(prepared.previous_request(), Some(&request));

    service.set_pane_agent_instruction_files(
        "%1",
        vec![project_instruction("Changed instructions apply next turn.")],
    );
    service
        .refresh_agent_turn_project_guidance_context(&turn)
        .unwrap();

    assert_eq!(
        service.agent_turn_contexts().get(&turn.turn_id),
        Some(&original)
    );
    let trace = service.agent_pane_trace_log_text("%1").unwrap();
    assert!(
        trace.contains("project guidance refresh deferred")
            && trace.contains("active provider cache epoch"),
        "{trace}"
    );
}

/// Every applicable ancestor instruction file belongs in the next prompt.
/// Three scopes expose the old arbitrary two-file cap without depending on
/// native navigation adoption or filesystem discovery fixtures.
#[test]
fn runtime_project_guidance_includes_every_discovered_scope() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let files = [".", "./src", "./src/nested"]
        .into_iter()
        .enumerate()
        .map(
            |(index, scope)| mez_agent::instructions::DiscoveredInstructionFile {
                path: format!("{scope}/AGENTS.md"),
                scope_root: scope.to_string(),
                bytes: 16,
                truncated: false,
                content: format!("scope-rule-{index}"),
            },
        )
        .collect();
    service.set_pane_agent_instruction_files("%1", files);
    let started = service
        .start_agent_prompt_turn("%1", "inspect all scopes")
        .unwrap();
    let context = service.agent_turn_contexts().get(&started.turn_id).unwrap();
    let guidance = context
        .blocks()
        .iter()
        .find(|block| block.source == ContextSourceKind::ProjectGuidance)
        .unwrap();
    for index in 0..3 {
        assert!(
            guidance.content.contains(&format!("scope-rule-{index}")),
            "{}",
            guidance.content
        );
    }
}

/// Two alternative discoveries from one identical pre-request context must
/// receive distinct cache identities. Looking for a removed stable system slot
/// produces the same absent fingerprint for both and cannot qualify the data.
#[test]
fn runtime_project_guidance_lineage_uses_actual_snapshot_bytes() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_agent_instruction_files("%1", vec![project_instruction("original-rule")]);
    let started = service
        .start_agent_prompt_turn("%1", "inspect instruction identity")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    let original = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .clone();
    let mut lineages = Vec::new();
    for rule in ["alternative-rule-A", "alternative-rule-B"] {
        service
            .agent_turn_contexts_mut()
            .insert(turn.turn_id.clone(), original.clone());
        service.set_pane_agent_instruction_files("%1", vec![project_instruction(rule)]);
        service
            .refresh_agent_turn_project_guidance_context(&turn)
            .unwrap();
        let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
        let latest = context
            .chronology()
            .iter()
            .rev()
            .find(|event| event.block().source == ContextSourceKind::ProjectGuidance)
            .unwrap();
        assert!(latest.block().content.contains(rule));
        assert_ne!(
            context.metadata().prompt_cache_lineage_id,
            original.metadata().prompt_cache_lineage_id
        );
        lineages.push(context.metadata().prompt_cache_lineage_id.clone());
    }
    assert_ne!(lineages[0], lineages[1]);
}
