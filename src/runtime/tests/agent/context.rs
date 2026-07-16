//! Runtime tests for agent context behavior.

use super::*;

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
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "synthetic provider-context-window action result".to_string(),
            content: format!("provider-context-window- {}", "cw ".repeat(10_000)),
        });
    service.pending_agent_provider_tasks.remove("turn-1");
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
        second_request_text.contains("provider-context-window-"),
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

/// Verifies agent prompt context includes pane readiness diagnostics before the
/// model plans shell-backed work.
///
/// If the pane is already known `interactive-blocked`, the model needs that
/// runtime fact in context before it chooses shell actions. Without this hint,
/// the provider can only discover the blockage after the runtime rejects the
/// first shell batch.
#[test]
fn runtime_agent_context_reports_nonready_pane_readiness() {
    let mut service = test_runtime_service();
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let context = service
        .agent_context_for_pane_prompt("%1", "inspect the status pager styling", 0)
        .unwrap();

    assert!(context.blocks.iter().any(|block| {
        block.label == "pane identity"
            && block
                .content
                .contains("readiness_state=interactive-blocked")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.label == "pane readiness"
            && block
                .content
                .contains("shell_command and apply_patch cannot execute")
    }));
}

/// Verifies prompt construction settles recoverable passive readiness before it
/// becomes durable model guidance for the next provider turn.
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

    let context = service
        .agent_context_for_pane_prompt("%1", "inspect the repo status", 0)
        .unwrap();

    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Ready
    );
    assert!(context.blocks.iter().any(|block| {
        block.label == "pane identity" && block.content.contains("readiness_state=ready")
    }));
    assert!(!context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint && block.label == "pane readiness"
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies prompt construction keeps the readiness warning when no shell-state
/// evidence can reconcile a passive non-ready pane.
///
/// A stale `busy` state should only settle away when host process metadata
/// proves the primary shell owns the foreground again. Without that evidence,
/// prompt context must continue warning that shell-backed actions may wait.
#[test]
fn runtime_agent_context_keeps_unconfirmed_busy_readiness_warning() {
    let mut service = test_runtime_service();
    service.set_pane_readiness("%1", PaneReadinessState::Busy);

    let context = service
        .agent_context_for_pane_prompt("%1", "inspect the repo status", 0)
        .unwrap();

    assert_eq!(service.pane_readiness_state("%1"), PaneReadinessState::Busy);
    assert!(context.blocks.iter().any(|block| {
        block.label == "pane identity" && block.content.contains("readiness_state=busy")
    }));
    let block = context
        .blocks
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
/// and current config.
///
/// The reference skill should not force the model to rediscover basic command
/// names or config setting names before operating Mezzanine. Its invocation
/// context therefore includes command indexes, the annotated schema, concrete
/// theme color slots, reset operation, and the pane's current effective config
/// snapshot.
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
            .contains("## Current effective Mezzanine config")
    );
    assert!(skill_block.content.contains("value path=theme.active"));
    assert!(
        skill_block
            .content
            .contains("## Additional context\n\nset the prompt color"),
        "{}",
        skill_block.content
    );
}
