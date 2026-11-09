//! Provider failure settlement must not need a valid replacement request.
//! These controls retain actual task admission and cleanup; an unusable profile
//! injects a deterministic assembly refusal without corrupting private context.

use super::*;

fn fixture() -> (PathBuf, RuntimeSessionService, mez_core::ids::ClientId) {
    let root = temp_root("provider-failure-containment");
    let mut service = test_runtime_service();
    service.set_config_root(root.clone());
    service.set_agent_transcript_store(AgentTranscriptStore::new(root.join("transcripts")));
    service.replace_config_layers(vec![ConfigLayer {
        name: "failure-containment".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\nshell_mode=\"pane\"\nrouting=false\ndefault_provider=\"runtime-echo\"\ndefault_model_profile=\"test\"\n[providers.runtime-echo]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.test]\nprovider=\"runtime-echo\"\nmodel=\"test\"\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    (root, service, primary)
}

/// A typed semantic refusal must never be converted into a new synthetic
/// execution, even when the current source could assemble successfully. A
/// required audit failure is retained as secondary evidence, not suppressed.
#[test]
fn runtime_provider_failure_retains_typed_context_refusal_and_audit_failure() {
    for audit_failure in [false, true] {
        let (root, mut service, primary) = fixture();
        let turn = service
            .start_agent_prompt_turn("%1", "original retained source")
            .unwrap();
        let original = service.agent_turn_contexts()[&turn.turn_id].clone();
        let rejected = mez_agent::AgentContext::new(vec![
            ContextBlock::user_event("user prompt", "active task"),
            ContextBlock::task_prelude(
                ContextSourceKind::SkillInstruction,
                "unowned prelude",
                "late instructions",
            ),
        ])
        .unwrap_err();
        let cause = MezError::from(rejected);
        if audit_failure {
            let blocker = root.join("audit-parent");
            fs::write(&blocker, "not a directory").unwrap();
            service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
                enabled: true,
                required: true,
                hash_chain: false,
                path: blocker.join("audit.jsonl"),
            }));
        }
        service
            .fail_configured_agent_provider_task(&turn.turn_id, &cause)
            .unwrap();
        let retained = service
            .provider_failure_source_for_tests(&turn.turn_id)
            .unwrap();
        assert_eq!(retained.0, &original);
        assert!(retained.1.is_none());
        assert_eq!(service.agent_turn_contexts()[&turn.turn_id], original);
        assert!(!service.agent_turn_executions().contains_key(&turn.turn_id));
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        let refusal = service.ensure_provider_failure_pane_open("%1").unwrap_err();
        assert!(refusal.message().contains(cause.message()));
        if audit_failure {
            assert!(refusal.message().contains("audit"));
        }
        assert!(
            service
                .execute_agent_shell_command(&primary, "must not become steering")
                .is_err()
        );
        assert!(service.stop_agent_turn_for_pane("%1").is_err());
        for command in ["/new", "/clear"] {
            let error = service
                .execute_agent_shell_command(&primary, command)
                .unwrap_err();
            assert!(error.message().contains(cause.message()));
        }
        assert_eq!(service.agent_turn_contexts()[&turn.turn_id], original);
        service
            .fail_configured_agent_provider_task(
                &turn.turn_id,
                &MezError::invalid_state("duplicate must not replace cause"),
            )
            .unwrap();
        assert_eq!(
            service
                .ensure_provider_failure_pane_open("%1")
                .unwrap_err()
                .message(),
            refusal.message()
        );
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }
}

/// A causal request error remains visible even when its reporting path cannot
/// assemble a request. The original source stays intact, no inference is
/// retried, and the refusal releases capacity for another pane.
#[test]
fn runtime_provider_failure_contains_unassemblable_failure_report() {
    let (root, mut service, primary) = fixture();
    let turn = service
        .start_agent_prompt_turn("%1", "retain original failed input")
        .unwrap();
    let original = service.agent_turn_contexts()[&turn.turn_id].clone();
    service.set_agent_turn_model_profile(&turn.turn_id, runtime_model_profile("runtime-echo", ""));
    let cause = MezError::invalid_args("context semantic violation: injected request refusal");
    service
        .fail_configured_agent_provider_task(&turn.turn_id, &cause)
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert_eq!(service.agent_turn_contexts()[&turn.turn_id], original);
    assert!(
        !service.agent_turn_executions().contains_key(&turn.turn_id),
        "a refused request is not a synthetic execution"
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(
        service
            .agent_scheduler()
            .running_turns()
            .all(|work| work.turn_id != turn.turn_id)
    );
    assert!(
        service
            .agent_scheduler()
            .blocked_turns()
            .any(|work| work.turn_id == turn.turn_id)
    );
    let trace = service.agent_pane_trace_log_text("%1").unwrap();
    assert!(trace.contains("injected request refusal"));
    assert!(trace.contains("model"));
    // Repeated delivery cannot re-enter settlement or replace original custody.
    service
        .fail_configured_agent_provider_task(&turn.turn_id, &cause)
        .unwrap();
    assert_eq!(service.agent_turn_contexts()[&turn.turn_id], original);

    let other = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap()
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&other)
        .unwrap();
    let next = service
        .start_agent_prompt_turn(&other, "unaffected work")
        .unwrap();
    let result = service
        .execute_agent_turn_with_provider(
            &next.turn_id,
            &RuntimeEchoProvider,
            runtime_model_profile("runtime-echo", "test"),
        )
        .unwrap();
    assert_eq!(result.terminal_state, AgentTurnState::Completed);
    assert_eq!(service.agent_turn_contexts()[&turn.turn_id], original);
    drop(service);
    fs::remove_dir_all(root).unwrap();
}

/// Normal provider failure still publishes a causal failure through the
/// original checked bookkeeping path and retires the turn normally.
#[test]
fn runtime_provider_failure_settles_valid_context_normally() {
    let (root, mut service, _) = fixture();
    let turn = service
        .start_agent_prompt_turn("%1", "ordinary failure input")
        .unwrap();
    service
        .fail_configured_agent_provider_task(
            &turn.turn_id,
            &MezError::invalid_args("ordinary local refusal"),
        )
        .unwrap();
    for work in service.claim_bookkeeping_candidates() {
        let outcome = work.execute();
        assert!(
            service
                .complete_bookkeeping_candidate(work, outcome)
                .unwrap()
        );
    }
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Failed
    );
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn.turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let rows = service
        .agent_transcript_store()
        .unwrap()
        .inspect(&conversation)
        .unwrap();
    assert!(
        rows.iter()
            .any(|row| row.content.contains("ordinary failure input"))
    );
    assert!(
        rows.iter()
            .any(|row| row.content.contains("ordinary local refusal"))
    );
    drop(service);
    fs::remove_dir_all(root).unwrap();
}
