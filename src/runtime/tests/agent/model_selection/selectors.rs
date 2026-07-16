//! Runtime tests for agent model_selection selectors behavior.

use super::*;

/// Verifies that runtime model-profile overrides feed both provider execution
/// and the live `agent/list` state surface. The selected profile must remain
/// visible while the turn is running and after the turn completes so clients do
/// not see the generic offline `default` placeholder for a live agent.
#[test]
fn runtime_agent_shell_model_command_overrides_pane_model_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"model","method":"agent/shell/command","params":{"idempotency_key":"model","input":"/model gpt-5.4"}}"#,
        &primary,
    );
    assert!(model.contains("scope=pane:%1"), "{model}");
    assert!(model.contains("profile=gpt-5.4"), "{model}");

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"prompt","method":"agent/shell/command","params":{"idempotency_key":"prompt","input":"use the selected model"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].model_profile.model, "gpt-5.4");
    let agents = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agents","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(agents.contains(r#""model_profile":"gpt-5.4""#), "{agents}");
    assert!(agents.contains(r#""last_turn_id":"turn-1""#), "{agents}");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeEchoProvider,
            ModelProfile {
                provider: "openai".to_string(),
                model: "gpt-5.4".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let completed_agents = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agents-after","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(
        completed_agents.contains(r#""model_profile":"gpt-5.4""#),
        "{completed_agents}"
    );
}

/// Verifies that clicking pane-frame model and reasoning status pills opens a
/// selector backed by the live provider catalog cache and applies the selected
/// value as a pane-scoped model override. This protects the mouse UI path from
/// drifting away from the `/model` command semantics that provider execution
/// already uses.
#[test]
fn runtime_pane_agent_status_selector_applies_model_and_reasoning() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"low\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"low\"\n"
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
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![mez_agent::ProviderModelInfo {
            id: "gpt-provider-only".to_string(),
            display_name: Some("Provider Only".to_string()),
            reasoning_levels: vec!["low".to_string(), "high".to_string()],
            context_window_tokens: Some(777_777),
            capabilities: Vec::new(),
        }],
        vec!["low".to_string(), "high".to_string()],
    );

    let open_model = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::OpenPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Model,
            },
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &open_model)
        .unwrap();
    let model_index = service
        .pane_agent_status_selector()
        .and_then(|selector| {
            selector
                .items
                .iter()
                .position(|item| item == "openai: gpt-provider-only")
        })
        .expect("model selector should include live provider catalog models");
    let select_model = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::SelectPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Model,
                item_index: model_index,
            },
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &select_model)
        .unwrap();
    let (_name, model_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(model_profile.model, "gpt-provider-only");
    assert_eq!(model_profile.reasoning_profile.as_deref(), Some("low"));
    assert_eq!(
        model_profile.provider_options.get("context_window_tokens"),
        Some(&"777777".to_string())
    );

    let open_reasoning = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::OpenPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Reasoning,
            },
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &open_reasoning)
        .unwrap();
    let reasoning_items = service
        .pane_agent_status_selector()
        .map(|selector| selector.items.clone())
        .unwrap_or_default();
    let reasoning_index = reasoning_items
        .iter()
        .position(|item| item == "high")
        .unwrap_or_else(|| {
            panic!(
                "reasoning selector should include configured provider reasoning levels: {reasoning_items:?}"
            )
        });
    let select_reasoning = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::SelectPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Reasoning,
                item_index: reasoning_index,
            },
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &select_reasoning)
        .unwrap();
    let (_name, reasoning_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(reasoning_profile.model, "gpt-provider-only");
    assert_eq!(reasoning_profile.reasoning_profile.as_deref(), Some("high"));
    assert!(service.pane_agent_status_selector().is_none());

    let open_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(open_report.view_refresh_required);
    assert!(!open_report.full_redraw_required);
    let full_access_index = service
        .pane_agent_status_selector()
        .and_then(|selector| selector.items.iter().position(|item| item == "full-access"))
        .expect("approval selector should include full-access");
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                        item_index: full_access_index,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let (_name, preserved_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(preserved_profile.model, "gpt-provider-only");
    assert_eq!(preserved_profile.reasoning_profile.as_deref(), Some("high"));
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(
        pane_context.agent_model.as_deref(),
        Some("gpt-provider-only")
    );
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
}

/// Verifies that the pane-frame model selector prepends configured presets and
/// applies preset-local automatic sizing without mutating the global sizing
/// defaults. This also protects the model pill contract by keeping the visible
/// model value sourced from the active concrete model after a preset choice
/// changes the pane profile.
#[test]
fn runtime_pane_model_selector_prepends_presets_and_applies_them_locally() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "openai"
default_model_profile = "default"

[agents.auto_sizing]
router_model_profile = "openai-router"
small_model_profile = "openai-small"
medium_model_profile = "openai-medium"
large_model_profile = "openai-large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]

[providers.openai]
kind = "openai"
models = ["gpt-5.5", "gpt-5.4"]
default_model = "gpt-5.5"

[providers.deepseek]
kind = "deepseek"
models = ["deepseek-v4-flash", "deepseek-v4"]
default_model = "deepseek-v4-flash"

[model_profiles.default]
provider = "openai"
model = "gpt-5.5"
reasoning_profile = "medium"

[model_profiles.openai-router]
provider = "openai"
model = "gpt-5.4"
reasoning_profile = "medium"

[model_profiles.openai-small]
provider = "openai"
model = "gpt-5.4"
reasoning_profile = "low"

[model_profiles.openai-medium]
provider = "openai"
model = "gpt-5.5"
reasoning_profile = "medium"

[model_profiles.openai-large]
provider = "openai"
model = "gpt-5.5"
reasoning_profile = "high"

[model_profiles.deepseek-fast]
provider = "deepseek"
model = "deepseek-v4-flash"
reasoning_profile = "high"
latency_preference = "fast"

[model_profiles.deepseek-default]
provider = "deepseek"
model = "deepseek-v4"
reasoning_profile = "xhigh"

[model_presets.openai]
default_model_profile = "default"
auto_sizing_router_model_profile = "openai-router"
auto_sizing_small_model_profile = "openai-small"
auto_sizing_medium_model_profile = "openai-medium"
auto_sizing_large_model_profile = "openai-large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]

[model_presets.deepseek]
default_model_profile = "deepseek-fast"
auto_sizing_router_model_profile = "deepseek-fast"
auto_sizing_small_model_profile = "deepseek-fast"
auto_sizing_medium_model_profile = "deepseek-default"
auto_sizing_large_model_profile = "deepseek-default"
allowed_reasoning_efforts = ["high", "xhigh"]
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

    let initial_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let initial_pane_context = initial_config.frame_context.panes.get("%1").unwrap();
    assert_eq!(initial_pane_context.agent_preset.as_deref(), Some("openai"));
    assert_eq!(initial_pane_context.agent_model.as_deref(), Some("gpt-5.5"));

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"model-selector-turn","method":"agent/shell/command","params":{"idempotency_key":"model-selector-turn","input":"check model"}}"#,
        &primary,
    );
    assert!(response.contains(r#""state":"running""#), "{response}");
    service
        .finish_agent_turn("%1", "turn-1", AgentTurnState::Completed)
        .unwrap();

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let selector = service
        .pane_agent_status_selector()
        .expect("model selector should open from the pane status field");
    assert_eq!(selector.field, PaneAgentStatusField::Model);
    assert_eq!(
        &selector.items[..2],
        ["preset: deepseek".to_string(), "preset: openai".to_string()]
    );
    assert_eq!(
        selector
            .items
            .get(selector.active_index)
            .map(String::as_str),
        Some("openai: gpt-5.5")
    );
    let deepseek_index = selector
        .items
        .iter()
        .position(|item| item == "preset: deepseek")
        .unwrap();

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                        item_index: deepseek_index,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(service.pane_agent_status_selector().is_none());
    let (_name, active_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(active_profile.provider, "deepseek");
    assert_eq!(active_profile.model, "deepseek-v4-flash");
    assert_eq!(
        service.agent_auto_sizing.router_model_profile, "openai-router",
        "preset selection must not mutate the global auto-sizing defaults"
    );
    let pane_auto_sizing = service.agent_auto_sizing_overrides.get("%1").unwrap();
    assert_eq!(pane_auto_sizing.router_model_profile, "deepseek-fast");
    assert_eq!(pane_auto_sizing.medium_model_profile, "deepseek-default");
    assert_eq!(
        pane_auto_sizing.allowed_reasoning_efforts,
        vec!["high".to_string(), "xhigh".to_string()]
    );

    let updated_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let updated_pane_context = updated_config.frame_context.panes.get("%1").unwrap();
    assert_eq!(
        updated_pane_context.agent_status.as_deref(),
        Some("completed"),
        "the completed turn remains the latest turn while the pill follows the selected profile"
    );
    assert_eq!(
        updated_pane_context.agent_model.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        updated_pane_context.agent_preset.as_deref(),
        Some("deepseek")
    );
}

/// Verifies that model presets validate every referenced auto-sizing profile at
/// config-load time. Without this guard, invalid preset groups can appear in
/// the selector and fail later during selection or automatic model sizing.
#[test]
fn runtime_model_presets_reject_unknown_auto_sizing_profile_references() {
    let mut service = test_runtime_service();
    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "openai"
default_model_profile = "default"

[providers.openai]
kind = "openai"
models = ["gpt-5.5"]
default_model = "gpt-5.5"

[model_profiles.default]
provider = "openai"
model = "gpt-5.5"
reasoning_profile = "medium"

[model_presets.openai]
default_model_profile = "default"
auto_sizing_router_model_profile = "missing-router"
"#
            .to_string(),
        }])
        .unwrap_err();

    assert!(
        error.message().contains(
            "model_presets.openai.auto_sizing_router_model_profile `missing-router` is not configured in model_profiles"
        ),
        "{error:?}"
    );
}

/// Verifies that `/model <model> <reasoning>` creates a pane-scoped runtime
/// model profile from the provider model catalog. This covers the direct model
/// selection UX without requiring users to predefine a named profile for every
/// model and reasoning combination they want to try.
#[test]
fn runtime_agent_shell_model_command_accepts_model_name_with_reasoning() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"model-reasoning","method":"agent/shell/command","params":{"idempotency_key":"model-reasoning","input":"/model gpt-5.4 high"}}"#,
        &primary,
    );
    assert!(model.contains(r#""kind":"mutated""#), "{model}");
    assert!(model.contains("scope=pane:%1"), "{model}");
    assert!(model.contains("profile=gpt-5.4:high"), "{model}");
    assert!(model.contains("model=gpt-5.4"), "{model}");
    assert!(model.contains("reasoning_profile=high"), "{model}");

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"prompt-reasoning","method":"agent/shell/command","params":{"idempotency_key":"prompt-reasoning","input":"use the selected model and reasoning"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].model_profile.model, "gpt-5.4");
    assert_eq!(
        pending[0].model_profile.reasoning_profile.as_deref(),
        Some("high")
    );
    assert!(
        !pending[0]
            .model_profile
            .provider_options
            .contains_key("reasoning_effort"),
        "/model reasoning selection should store only reasoning_profile"
    );
}

/// Verifies that `/model --routing` updates the auto-sizing router model.
///
/// The routing model is used for the internal sizing decision before the
/// main provider request. It should be configurable from the same command
/// surface as the primary pane model without changing the active pane model.
#[test]
fn runtime_agent_shell_model_command_sets_routing_model_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n[agents.auto_sizing]\nrouter_model_profile = \"router\"\nsmall_model_profile = \"default\"\nmedium_model_profile = \"default\"\nlarge_model_profile = \"default\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"medium\"\n[model_profiles.router]\nprovider = \"openai\"\nmodel = \"gpt-5.4-mini\"\nreasoning_profile = \"medium\"\n"
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

    let routing = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routing-model","method":"agent/shell/command","params":{"idempotency_key":"routing-model","input":"/model --routing gpt-5.4 high"}}"#,
        &primary,
    );
    assert!(routing.contains(r#""kind":"mutated""#), "{routing}");
    assert!(routing.contains("scope=routing"), "{routing}");
    assert!(routing.contains("profile=gpt-5.4:high"), "{routing}");
    assert!(routing.contains("model=gpt-5.4"), "{routing}");
    assert!(routing.contains("reasoning_profile=high"), "{routing}");
    assert_eq!(
        service.agent_auto_sizing.router_model_profile,
        "gpt-5.4:high"
    );

    let primary_status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"primary-model-status","method":"agent/shell/command","params":{"idempotency_key":"primary-model-status","input":"/model show"}}"#,
        &primary,
    );
    assert!(
        primary_status.contains("active_profile=default"),
        "{primary_status}"
    );
    let routing_status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routing-model-status","method":"agent/shell/command","params":{"idempotency_key":"routing-model-status","input":"/model --routing show"}}"#,
        &primary,
    );
    assert!(
        routing_status.contains("profile=gpt-5.4:high"),
        "{routing_status}"
    );
    assert!(
        routing_status.contains("active_model=gpt-5.5"),
        "{routing_status}"
    );
}
