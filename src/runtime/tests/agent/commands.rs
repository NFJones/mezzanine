//! Runtime tests for agent commands behavior.

use super::*;

/// Verifies recent message-log detail rows wrap to the pane width with an
/// indented continuation row.
///
/// The `show-messages` command renders diagnostics and lifecycle events in a
/// modal display. Long payloads should stay readable in narrow panes instead
/// of depending on host-terminal soft wrapping, and continuation rows should be
/// visually tied to the original log line.
#[test]
fn runtime_show_messages_wraps_logged_rows_with_indented_continuations() {
    let mut service = test_runtime_service_with_size(Size::new(48, 24).unwrap());
    service
        .append_runtime_diagnostic_event(
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu".to_string(),
        )
        .unwrap();

    let body = super::commands_support::runtime_show_messages_display(&service);
    let detail_lines = body.lines().skip(1).collect::<Vec<_>>();

    assert!(
        detail_lines.iter().any(|line| line.starts_with("    ")),
        "expected an indented continuation row in {body:?}"
    );
    assert!(
        detail_lines
            .iter()
            .all(|line| UnicodeWidthStr::width(*line) <= 48),
        "message rows should fit the pane width: {body:?}"
    );
}

/// Verifies runtime-wide metrics preserve provider token counters per model.
///
/// Aggregate token counts remain available for operational totals, but the
/// metrics display must also expose provider/model buckets so cost-oriented
/// readers do not have to infer mixed-model usage from one combined counter.
#[test]
fn runtime_show_metrics_reports_provider_tokens_by_model() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .integration
        .runtime_metrics_mut()
        .record_provider_token_usage(
            mez_agent::ModelTokenUsage {
                input_tokens: 120,
                output_tokens: 34,
                reasoning_tokens: 9,
                cached_input_tokens: Some(80),
                cache_write_input_tokens: None,
            },
            mez_agent::ModelTokenUsage {
                input_tokens: 120,
                output_tokens: 34,
                reasoning_tokens: 9,
                cached_input_tokens: Some(80),
                cache_write_input_tokens: None,
            },
            &mez_agent::ModelTokenUsageKey::new("openai", "gpt-fast"),
        );
    service
        .integration
        .runtime_metrics_mut()
        .record_provider_token_usage(
            mez_agent::ModelTokenUsage {
                input_tokens: 200,
                output_tokens: 50,
                reasoning_tokens: 20,
                cached_input_tokens: Some(100),
                cache_write_input_tokens: None,
            },
            mez_agent::ModelTokenUsage {
                input_tokens: 200,
                output_tokens: 50,
                reasoning_tokens: 20,
                cached_input_tokens: Some(100),
                cache_write_input_tokens: None,
            },
            &mez_agent::ModelTokenUsageKey::new("deepseek", "deepseek-chat"),
        );
    let mut request = runtime_model_request_fixture("turn-output-budget");
    request.max_output_tokens = Some(16_384);
    request.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::Developer,
        source: ContextSourceKind::Configuration,
        content: "[ephemeral provider output-limit retry] max_output_tokens=16384".to_string(),
    });
    service
        .integration
        .runtime_metrics_mut()
        .record_provider_request_shape(&request, None, false);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"show-metrics","method":"terminal/command","params":{"idempotency_key":"show-metrics","input":"show-metrics"}}"#,
        &primary,
    );

    assert!(
        response.contains("provider_input_tokens = 320"),
        "{response}"
    );
    assert!(
        response.contains(
            "last_provider_output_token_budget_source = temporary_output_limit_retry_override"
        ),
        "{response}"
    );
    assert!(
        response.contains("last_provider_output_token_budget_tokens = 16384"),
        "{response}"
    );
    assert!(
        response.contains("last_provider_output_limit_retry_override_tokens = 16384"),
        "{response}"
    );
    assert!(
        response.contains("last_provider_input_tokens = 200"),
        "{response}"
    );
    assert!(
        response.contains("last_provider_cached_input_tokens = 100"),
        "{response}"
    );
    assert!(
        response.contains("last_provider_cached_input_hit_ratio = 50.00%"),
        "{response}"
    );
    assert!(
        response.contains(
            "provider_cached_input_hit_ratio_basis_points: observations=2 min=5625 max=6667 average=6146.00"
        ),
        "{response}"
    );
    assert!(
        response.contains("[runtime provider tokens by model]"),
        "{response}"
    );
    assert!(
        response.contains(
            "provider_model_tokens[gpt-fast via openai] = provider=openai model=gpt-fast input=40 cached_input=80 output=34 reasoning=9 cache_hit=66.67% total=154"
        ),
        "{response}"
    );
    assert!(
        response.contains(
            "provider_model_tokens[deepseek-chat via deepseek] = provider=deepseek model=deepseek-chat input=100 cached_input=100 output=50 reasoning=20 cache_hit=50.00% total=250"
        ),
        "{response}"
    );
}

/// Verifies that the /latency slash command displays the current setting when
/// called without args and applies a pane-local override when given a valid
/// value.
#[test]
fn runtime_slash_command_latency_displays_and_applies_override() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"high\"\nlatency_preference = \"default\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![mez_agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["high".to_string()],
            context_window_tokens: Some(1_050_000),
            capabilities: Vec::new(),
        }],
        vec!["high".to_string()],
    );

    let status_outcome = service
        .execute_agent_shell_latency_command("%1", "/latency")
        .unwrap();
    let status_text = match status_outcome {
        super::AgentShellCommandOutcome::Display { body, .. } => body,
        other => panic!("expected Display outcome for /latency without args, got {other:?}"),
    };
    assert!(
        status_text.contains("latency_preference=default"),
        "status should show default: {status_text}"
    );

    let apply_outcome = service
        .execute_agent_shell_latency_command("%1", "/latency slow")
        .unwrap();
    let apply_text = match apply_outcome {
        super::AgentShellCommandOutcome::Mutated { body, .. } => body,
        other => panic!("expected Mutated outcome for /latency slow, got {other:?}"),
    };
    assert!(
        apply_text.contains("latency_preference=slow"),
        "outcome should show slow: {apply_text}"
    );

    let (_name, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(profile.latency_preference.as_deref(), Some("slow"));
}

/// Verifies that `/thinking` exposes DeepSeek's native thinking-mode toggle as
/// a pane-local model-profile override.
///
/// DeepSeek thinking and reasoning effort are separate provider controls: a
/// profile may retain its reasoning level while the operator disables thinking
/// to force strict MAAP tool calls. This test exercises the same control path
/// used by live agent-shell commands and confirms the resulting provider task
/// receives the generated profile.
#[test]
fn runtime_slash_command_thinking_displays_and_applies_deepseek_override() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"deepseek\"\ndefault_model_profile = \"default\"\n\n[providers.deepseek]\nkind = \"deepseek\"\nmodels = [\"deepseek-v4-pro\"]\ndefault_model = \"deepseek-v4-pro\"\n\n[model_profiles.default]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_profile = \"high\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"high\"\n"
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

    let status_outcome = service
        .execute_agent_shell_thinking_command("%1", "/thinking")
        .unwrap();
    let status_text = match status_outcome {
        super::AgentShellCommandOutcome::Display { body, .. } => body,
        other => panic!("expected Display outcome for /thinking without args, got {other:?}"),
    };
    assert!(status_text.contains("enabled=true"), "{status_text}");
    assert!(status_text.contains("explicit=false"), "{status_text}");

    let apply = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"thinking","method":"agent/shell/command","params":{"idempotency_key":"thinking","input":"/thinking off"}}"#,
        &primary,
    );
    assert!(apply.contains("source=runtime-thinking"), "{apply}");
    assert!(apply.contains("thinking=disabled"), "{apply}");
    assert!(apply.contains("changed=true"), "{apply}");

    let (_name, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(profile.thinking_enabled(), Some(false));
    assert_eq!(profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(
        service.model_profile_thinking_enabled(&profile),
        Some(false)
    );

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_thinking.as_deref(), Some("off"));

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"prompt","method":"agent/shell/command","params":{"idempotency_key":"prompt","input":"use the current DeepSeek thinking setting"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].model_profile.thinking_enabled(), Some(false));
}

/// Verifies unsupported providers reject `/thinking` instead of mutating
/// provider-neutral model profiles.
///
/// The thinking toggle is intentionally a provider capability, not a universal
/// model-profile field. OpenAI remains unaffected by the DeepSeek adapter's
/// compatibility controls, so the command should fail fast before creating a
/// runtime-generated profile for an unsupported provider.
#[test]
fn runtime_slash_command_thinking_rejects_unsupported_provider() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let error = service
        .execute_agent_shell_thinking_command("%1", "/thinking off")
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("does not support a thinking-mode toggle"),
        "{error}"
    );
}
