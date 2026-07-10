//! Runtime tests for agent model_selection frame context behavior.

use super::*;

/// Verifies that pane-frame runtime context falls back to provider-option
/// reasoning effort when the active profile omits the top-level reasoning
/// field. This keeps Anthropic-style profiles aligned with the
/// `agent.reasoning` status contract.
#[test]
fn runtime_frame_context_reports_provider_option_reasoning_effort() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"anthropic\"\ndefault_model_profile = \"work\"\n[providers.anthropic]\nkind = \"anthropic\"\napi = \"anthropic-messages\"\nmodels = [\"claude-fable-5\"]\ndefault_model = \"claude-fable-5\"\n[model_profiles.work]\nprovider = \"anthropic\"\nmodel = \"claude-fable-5\"\n[model_profiles.work.provider_options]\nreasoning_effort = \"high\"\n"
                .to_string(),
        }])
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

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.mode.as_deref(), Some("agent"));
    assert_eq!(pane_context.agent_name.as_deref(), Some("manager"));
    assert_eq!(pane_context.agent_status.as_deref(), Some("idle"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("claude-fable-5"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
}

/// Verifies that an active agent turn reports the provider model name rather
/// than the selected profile name in pane-frame metadata. The pane status area
/// is constrained, so showing the concrete provider model and keeping reasoning
/// in its own field preserves both accuracy and space.
#[test]
fn runtime_frame_context_reports_running_agent_provider_model_name() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\n"
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

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"frame-provider-model","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(response.contains(r#""state":"running""#), "{response}");

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_status.as_deref(), Some("thinking"));
    assert_eq!(pane_context.agent_name.as_deref(), Some("manager"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("gpt-work"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
    assert_eq!(pane_context.agent_context_usage, None);
    assert!(
        pane_context
            .agent_display_lines
            .iter()
            .any(|line| line.starts_with("thinking (") && line.contains(" • esc to interrupt")),
        "{pane_context:?}"
    );

    service
        .finish_agent_turn(&pane_id, "turn-1", AgentTurnState::Completed)
        .unwrap();
    let pane_text = service
        .pane_screen(&pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Worked for "), "{pane_text}");
}

/// Verifies that the agent frame context percentage uses the effective model
/// context-window denominator when a profile omits an explicit token count. This
/// protects the status area from reporting OpenAI GPT-5.5 usage against the
/// small local fallback window instead of the provider model's documented window.
#[test]
fn runtime_frame_context_uses_known_openai_model_context_window() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    service
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

    service.record_agent_provider_token_usage(
        &pane_id,
        crate::agent::ModelTokenUsage {
            input_tokens: 10_500,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
            cache_write_input_tokens: None,
        },
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_context_usage.as_deref(), Some("1%"));
}
