//! Runtime tests for agent context behavior.

use super::*;

/// Prepares one synthetic provider request from prompt chronology without
/// starting a scheduler-owned turn.
fn prepared_context_for_prompt(
    service: &RuntimeSessionService,
    context: mez_agent::AgentContext,
) -> mez_agent::PreparedModelContext {
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "turn-context-test".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        state: mez_agent::AgentTurnState::Running,
        initial_capability: None,
    };
    let mcp_summary = service.mcp_registry().prompt_summary();
    service
        .prepare_agent_turn_model_context(&turn, context, &mcp_summary)
        .unwrap()
        .0
}

/// Verifies latest execution-model cache reuse stays separate from cumulative
/// accounting and cannot be overwritten by auxiliary routing usage.
///
/// A cold history and router call can keep cumulative reuse low while the last
/// execution request is warm. `/status` must expose both labelled values and
/// retain the execution sample after auxiliary provider accounting arrives.
#[test]
fn runtime_status_separates_latest_and_cumulative_cache_reuse() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let profile = runtime_model_profile("openai", "gpt-execution");
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 1_000,
            output_tokens: 20,
            reasoning_tokens: 5,
            cached_input_tokens: Some(100),
            cache_write_input_tokens: None,
        },
        mez_agent::ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 10,
            reasoning_tokens: 2,
            cached_input_tokens: Some(90),
            cache_write_input_tokens: None,
        },
        Some(&profile),
    );
    service.record_agent_provider_token_usage_by_model(
        "%1",
        &std::collections::BTreeMap::from([(
            mez_agent::ModelTokenUsageKey::new("openai", "gpt-router"),
            mez_agent::ModelTokenUsage {
                input_tokens: 100,
                output_tokens: 1,
                reasoning_tokens: 0,
                cached_input_tokens: Some(0),
                cache_write_input_tokens: None,
            },
        )]),
    );

    let status = service.runtime_agent_status_display("%1").unwrap();

    assert!(
        status.contains("| Cumulative cache hit | 9.09% |"),
        "{status}"
    );
    assert!(
        status.contains(
            "| Latest request cache hit | 90.00% (gpt-execution via openai; cached_input=90 input=100) |"
        ),
        "{status}"
    );
    let latest = service
        .agent_latest_request_usage(&service.agent_shell_store().get("%1").unwrap().session_id)
        .unwrap();
    assert_eq!(latest.model.model, "gpt-execution");
    assert_eq!(latest.usage.cached_input_tokens, Some(90));
}

/// Verifies a cold request after compaction can be followed by a warm latest
/// request without erasing the cumulative cold-start cost.
///
/// Compaction intentionally resets provider cache warming. The next request's
/// explicit zero must remain distinguishable from unknown, and a subsequent
/// warm request must replace only the latest sample while cumulative accounting
/// continues to include both requests.
#[test]
fn runtime_cache_status_tracks_post_compaction_cold_then_warm_requests() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let profile = runtime_model_profile("openai", "gpt-execution");
    let cold = mez_agent::ModelTokenUsage {
        input_tokens: 100,
        output_tokens: 5,
        reasoning_tokens: 0,
        cached_input_tokens: Some(0),
        cache_write_input_tokens: None,
    };
    service.record_agent_provider_token_usage_with_profile("%1", cold, cold, Some(&profile));
    let cold_status = service.runtime_agent_status_display("%1").unwrap();
    assert!(
        cold_status.contains("| Latest request cache hit | 0.00%"),
        "{cold_status}"
    );

    let warm = mez_agent::ModelTokenUsage {
        cached_input_tokens: Some(90),
        ..cold
    };
    service.record_agent_provider_token_usage_with_profile("%1", warm, warm, Some(&profile));
    let warm_status = service.runtime_agent_status_display("%1").unwrap();

    assert!(
        warm_status.contains("| Cumulative cache hit | 45.00% |"),
        "{warm_status}"
    );
    assert!(
        warm_status.contains("| Latest request cache hit | 90.00%"),
        "{warm_status}"
    );
}

/// Verifies finalized provider requests feed sensitive-content-free immutable
/// continuity diagnostics into `/status`.
///
/// The first request establishes a new-turn baseline. Adding settled chronology
/// to the same turn must then report append-only growth, a common immutable
/// prefix, token estimates, and a stable projection digest.
#[test]
fn runtime_status_reports_provider_context_continuity_diagnostics() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect continuity")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let context = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .cloned()
        .unwrap();
    let (_, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    service.record_runtime_provider_request_shape_for_context(
        &profile,
        &turn,
        &context,
        &[],
        false,
        false,
    );
    let initial_status = service.runtime_agent_status_display("%1").unwrap();
    assert!(
        initial_status.contains("reason=new_turn"),
        "{initial_status}"
    );

    let mut appended = context;
    mez_agent::insert_context_block_by_placement(
        &mut appended.blocks,
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "settled result".to_string(),
            content: "deterministic evidence".to_string(),
        },
    );
    service.record_runtime_provider_request_shape_for_context(
        &profile,
        &turn,
        &appended,
        &[],
        false,
        false,
    );
    let status = service.runtime_agent_status_display("%1").unwrap();

    assert!(
        status.contains("reason=append_only") && status.contains("append_only=true"),
        "{status}"
    );
    assert!(
        status.contains("| Immutable projection | bytes="),
        "{status}"
    );
    assert!(status.contains("sha256="), "{status}");
    assert!(
        status.contains("| Common immutable prefix | blocks=1 tokens~"),
        "{status}"
    );
    let request =
        crate::integrations::agent::context::assemble_model_request(&profile, &turn, &appended)
            .unwrap();
    service
        .append_agent_trace_maap_request(&turn, &request)
        .unwrap();
    let trace = service.agent_pane_trace_log_text("%1").unwrap();
    assert!(trace.contains("\"context_continuity\""), "{trace}");
    assert!(
        trace.contains("\"break_reason\": \"append_only\""),
        "{trace}"
    );
    assert!(trace.contains("\"immutable_token_estimate\""), "{trace}");
}

/// Verifies that the pane frame reports only the latest provider-backed input
/// context percentage instead of replacing it with a local preflight estimate
/// while another turn is running. This keeps the status pill tied to the same
/// token accounting that the provider returns, while still allowing the runtime
/// to use internal byte estimates for compaction decisions separately.
#[test]
fn runtime_frame_context_reports_last_provider_context_usage() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\ncontext_window_tokens = 1000\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    let initial_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let initial_pane_context = initial_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(initial_pane_context.agent_context_usage, None);

    service.record_agent_provider_token_usage(
        &pane_id,
        mez_agent::ModelTokenUsage {
            input_tokens: 251,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
            cache_write_input_tokens: None,
        },
    );
    let recorded_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let recorded_pane_context = recorded_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(
        recorded_pane_context.agent_context_usage.as_deref(),
        Some("25%")
    );

    let (_, profile) = service
        .active_model_profile_for_pane(&pane_id, &format!("agent-{pane_id}"), None)
        .unwrap();
    service.record_agent_provider_token_usage_with_profile(
        &pane_id,
        mez_agent::ModelTokenUsage {
            input_tokens: 1_200,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(100),
            cache_write_input_tokens: None,
        },
        mez_agent::ModelTokenUsage {
            input_tokens: 251,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(80),
            cache_write_input_tokens: None,
        },
        Some(&profile),
    );
    let cumulative_recorded_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let cumulative_recorded_pane_context = cumulative_recorded_config
        .frame_context
        .panes
        .get(&pane_id)
        .unwrap();
    assert_eq!(
        cumulative_recorded_pane_context
            .agent_context_usage
            .as_deref(),
        Some("25%")
    );

    service.record_agent_provider_token_usage_with_profile(
        &pane_id,
        mez_agent::ModelTokenUsage {
            input_tokens: 1_500,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(100),
            cache_write_input_tokens: None,
        },
        mez_agent::ModelTokenUsage {
            input_tokens: 1_200,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(80),
            cache_write_input_tokens: None,
        },
        Some(&profile),
    );
    let saturated_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let saturated_pane_context = saturated_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(
        saturated_pane_context.agent_context_usage.as_deref(),
        Some("100%")
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-context-usage","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-context-usage","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(response.contains(r#""state":"running""#), "{response}");

    let running_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let running_pane_context = running_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(
        running_pane_context.agent_context_usage.as_deref(),
        Some("100%")
    );
}

/// Verifies provider context-window wording also triggers active-turn
/// compaction and retry before the turn is failed.
///
/// Some providers report the same rejection without the OpenAI-specific
/// `context_length_exceeded` code. Runtime recovery should still classify the
/// error as a context-limit failure when the diagnostic says the input exceeds
/// the model context window.
#[test]
fn runtime_provider_context_window_error_compacts_context_and_retries() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-context-window-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-context-window-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-context-window-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-context-window-recovery","input":"continue with the large observation"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    mez_agent::insert_context_block_by_placement(
        &mut context.blocks,
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "synthetic provider-context-window action result".to_string(),
            content: format!("provider-context-window- {}", "cw ".repeat(10_000)),
        },
    );
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeContextWindowErrorProvider {
        requests: RefCell::new(Vec::new()),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("provider-context-window-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2);
    let first_request_text = requests[0]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        first_request_text.contains("provider-context-window-"),
        "{first_request_text}"
    );
    let second_request_text = requests[1]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("[context compacted]"),
        "{second_request_text}"
    );
    assert!(
        second_request_text
            .contains("source=action_result label=synthetic provider-context-window action result"),
        "{second_request_text}"
    );
    assert!(
        !second_request_text.contains("cw cw cw"),
        "{second_request_text}"
    );
    let retry_notice = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        retry_notice
            .contains("provider rejected context as too large; compacted active turn context"),
        "{retry_notice}"
    );
}

/// Verifies provider preparation includes non-ready pane diagnostics without
/// persisting volatile readiness in durable chronology.
///
/// If the pane is already known `interactive-blocked`, the model needs that
/// runtime fact in context before it chooses shell actions. Without this hint,
/// the provider can only discover the blockage after the runtime rejects the
/// first shell batch.
#[test]
fn runtime_agent_context_reports_nonready_pane_readiness() {
    let mut service = test_runtime_service();
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let durable = service
        .agent_context_for_pane_prompt("%1", "inspect the status pager styling", 0)
        .unwrap();
    let prepared = prepared_context_for_prompt(&service, durable.clone());

    assert!(durable.validate_durable().is_ok());
    assert!(
        !durable
            .blocks
            .iter()
            .any(|block| { block.label == "pane identity" || block.label == "pane readiness" })
    );
    assert!(prepared.live_state().iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.label == "pane readiness"
            && block
                .content
                .contains("shell_command and apply_patch cannot execute")
    }));
}

/// Verifies prompt construction settles recoverable passive readiness before
/// provider preparation and keeps the ready state out of both context regions.
///
/// Post-shell recovery can briefly leave the pane at `prompt-candidate` even
/// though host process metadata already shows the primary shell back in the
/// foreground. Prompt construction should promote that stale passive state to
/// `ready` and omit the shell-not-ready hint.
#[test]
fn runtime_agent_context_settles_recoverable_prompt_candidate_readiness() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service.set_pane_readiness("%1", PaneReadinessState::PromptCandidate);

    let durable = service
        .agent_context_for_pane_prompt("%1", "inspect the repo status", 0)
        .unwrap();
    let prepared = prepared_context_for_prompt(&service, durable.clone());

    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Ready
    );
    assert!(durable.validate_durable().is_ok());
    assert!(
        !durable
            .blocks
            .iter()
            .any(|block| { block.label == "pane identity" || block.label == "pane readiness" })
    );
    assert!(
        !prepared
            .live_state()
            .iter()
            .any(|block| block.label == "pane readiness")
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies provider preparation keeps a request-local readiness warning when
/// no shell-state evidence can reconcile a passive non-ready pane.
///
/// A stale `busy` state should only settle away when host process metadata
/// proves the primary shell owns the foreground again. Without that evidence,
/// prompt context must continue warning that shell-backed actions may wait.
#[test]
fn runtime_agent_context_keeps_unconfirmed_busy_readiness_warning() {
    let mut service = test_runtime_service();
    service.set_pane_readiness("%1", PaneReadinessState::Busy);

    let durable = service
        .agent_context_for_pane_prompt("%1", "inspect the repo status", 0)
        .unwrap();
    let prepared = prepared_context_for_prompt(&service, durable.clone());

    assert_eq!(service.pane_readiness_state("%1"), PaneReadinessState::Busy);
    assert!(durable.validate_durable().is_ok());
    assert!(
        !durable
            .blocks
            .iter()
            .any(|block| { block.label == "pane identity" || block.label == "pane readiness" })
    );
    let block = prepared
        .live_state()
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::RuntimeHint && block.label == "pane readiness"
        })
        .unwrap();
    assert!(
        block
            .content
            .contains("may be delayed or rejected until Mezzanine confirms")
    );
}

/// Verifies `$mez-reference` includes live schema guidance, command indexes,
/// and a separately labeled invocation-time config snapshot.
///
/// The reference skill should not force the model to rediscover basic command
/// names or config setting names before operating Mezzanine. Its invocation
/// context therefore includes command indexes, the annotated schema, concrete
/// theme color slots, reset operation, and the pane's current effective config
/// snapshot. The snapshot is a conversation record rather than immutable skill
/// text so later settled `config_change` results can explicitly supersede it.
#[test]
fn runtime_agent_context_builtin_mez_reference_prompt_includes_current_config() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "default-user".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: crate::config::DEFAULT_CONFIG_TOML.to_string(),
        }])
        .unwrap();

    let context = service
        .agent_context_for_pane_prompt("%1", "$mez-reference set the prompt color", 0)
        .unwrap();
    let skill_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill mez-reference")
        .expect("missing explicit mez-reference skill context block");

    assert!(skill_block.content.contains("Terminal command index"));
    assert!(
        skill_block
            .content
            .contains("Agent shell slash command index")
    );
    assert!(
        skill_block
            .content
            .contains("Allowed operations: `set`, `unset`, `reset`")
    );
    assert!(skill_block.content.contains("theme.colors.agent_prompt_bg"));
    assert!(
        skill_block
            .content
            .contains("## Additional context\n\nset the prompt color"),
        "{}",
        skill_block.content
    );
    assert_eq!(
        skill_block.placement,
        mez_agent::ContextPlacement::ConversationAppend
    );
    let snapshot_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill mez-reference invocation-time config snapshot")
        .expect("missing explicit mez-reference config snapshot block");
    assert_eq!(
        snapshot_block.placement,
        mez_agent::ContextPlacement::ConversationAppend
    );
    assert!(
        snapshot_block
            .content
            .contains("Effective Mezzanine config snapshot at skill invocation time")
    );
    assert!(
        snapshot_block
            .content
            .contains("Later settled config_change results supersede this snapshot")
    );
    assert!(snapshot_block.content.contains("value path=theme.active"));
}
