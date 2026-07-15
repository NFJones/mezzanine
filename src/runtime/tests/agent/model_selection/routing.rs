//! Runtime tests for agent model_selection routing behavior.

use super::*;

/// Verifies subagents inherit the live parent pane routing decision.
///
/// Auto-reasoning is a pane-local agent behavior, not just a global default.
/// Child agents should continue with the parent pane's effective setting so a
/// user does not have to re-toggle it after spawning helpers.
#[test]
fn runtime_subagent_routing_inherits_parent_pane_setting() {
    let mut service = test_runtime_service();
    service.agent_routing = false;
    service
        .agent_routing_overrides
        .insert("%1".to_string(), true);

    assert_eq!(
        service.inherited_routing_for_child_agent("agent-%1"),
        Some(true)
    );

    service.agent_routing_overrides.remove("%1");
    service.agent_routing = true;
    assert_eq!(
        service.inherited_routing_for_child_agent("agent-%1"),
        Some(true)
    );
}

///
/// Verifies subagents inherit the live parent pane auto-sizing configuration.
///
/// Auto-sizing uses pane-local model profile names for router and bucket
/// selection. Child agents must inherit that configuration with the parent
/// model profile so a DeepSeek parent pane does not spawn children that use the
/// global OpenAI sizing defaults.
#[test]
fn runtime_subagent_auto_sizing_inherits_parent_pane_setting() {
    let mut service = test_runtime_service();
    let mut parent_auto_sizing = service.agent_auto_sizing.clone();
    parent_auto_sizing.router_model_profile = "deepseek-fast".to_string();
    parent_auto_sizing.small_model_profile = "deepseek-fast".to_string();
    parent_auto_sizing.medium_model_profile = "deepseek-default".to_string();
    parent_auto_sizing.large_model_profile = "deepseek-default".to_string();
    parent_auto_sizing.allowed_reasoning_efforts = vec!["high".to_string(), "xhigh".to_string()];
    service
        .agent_auto_sizing_overrides
        .insert("%1".to_string(), parent_auto_sizing.clone());

    assert_eq!(
        service.inherited_auto_sizing_for_child_agent("agent-%1"),
        Some(parent_auto_sizing)
    );
}

/// Verifies that configured named model profiles populate the full
/// specification-facing profile fields and that configured fallback profiles
/// are filtered through safety, privacy, residency, and approval
/// characteristics before they can be offered after provider failure.
#[test]
fn runtime_applies_named_model_profile_fields_and_safe_fallbacks() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\", \"gpt-safe\", \"gpt-weak\", \"gpt-external\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\nlatency_preference = \"default\"\nmultimodal_required = true\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\nfallback_profiles = [\"safe\", \"weak\", \"external\"]\n[model_profiles.work.provider_options]\nreasoning_effort = \"high\"\n[model_profiles.safe]\nprovider = \"openai\"\nmodel = \"gpt-safe\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.weak]\nprovider = \"openai\"\nmodel = \"gpt-weak\"\nsafety_tier = \"medium\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.external]\nprovider = \"openai\"\nmodel = \"gpt-external\"\nsafety_tier = \"high\"\nprivacy_tier = \"external\"\nresidency = \"eu\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();

    let registry = service.provider_registry();
    let profile = registry.resolve_profile("work").unwrap();
    assert_eq!(profile.provider, "openai");
    assert_eq!(profile.model, "gpt-work");
    assert_eq!(profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(profile.latency_preference.as_deref(), Some("default"));
    assert!(profile.multimodal_required);
    assert_eq!(profile.safety_tier.as_deref(), Some("high"));
    assert_eq!(
        profile
            .provider_options
            .get("reasoning_effort")
            .map(String::as_str),
        Some("high")
    );
    assert_eq!(
        registry.safe_fallback_profiles("work").unwrap(),
        vec!["safe".to_string()]
    );
}

/// Verifies that provider failure reporting only offers configured fallback
/// profiles whose safety, privacy, residency, and approval characteristics are
/// non-weaker than the active model profile.
#[test]
fn runtime_provider_failure_reports_only_safe_model_fallbacks() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-safe-fallback-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"runtime-fail\"\ndefault_model_profile = \"work\"\n[providers.runtime-fail]\nkind = \"runtime-fail\"\napi = \"openai-responses\"\nmodels = [\"primary\", \"safe\", \"weak\"]\ndefault_model = \"primary\"\n[model_profiles.work]\nprovider = \"runtime-fail\"\nmodel = \"primary\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\nfallback_profiles = [\"safe\", \"weak\"]\n[model_profiles.safe]\nprovider = \"runtime-fail\"\nmodel = \"safe\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.weak]\nprovider = \"runtime-fail\"\nmodel = \"weak\"\nsafety_tier = \"medium\"\nprivacy_tier = \"external\"\nresidency = \"eu\"\napproval_policy = \"full-access\"\n"
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
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-safe-fallback","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.model_profile.as_str()),
        Some("work")
    );

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeFailingProvider,
            service.provider_registry().resolve_profile("work").unwrap(),
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    let failure = entries
        .iter()
        .find(|entry| {
            entry.role == crate::transcript::TranscriptRole::Assistant
                && entry.content.contains("provider_error")
        })
        .unwrap();
    assert!(failure.content.contains("safe_fallback_profiles: safe"));
    assert!(!failure.content.contains("weak"));
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies that changing reasoning from the pane-frame selector preserves the
/// active latency preference and keeps the latency pill visible.
///
/// Reasoning changes generate a new pane-scoped model profile. That generated
/// profile must carry forward the provider-visible latency selection so the
/// status bar does not lose its latency dropdown after the user changes only
/// the reasoning level.
#[test]
fn runtime_pane_agent_status_reasoning_preserves_latency_preference() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"low\"\nlatency_preference = \"fast\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"low\"\n"
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
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string(), "high".to_string()],
            context_window_tokens: Some(1_050_000),
            capabilities: Vec::new(),
        }],
        vec!["low".to_string(), "high".to_string()],
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
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
            },
        )
        .unwrap();
    let reasoning_items = service
        .pane_agent_status_selector
        .as_ref()
        .map(|selector| selector.items.clone())
        .unwrap_or_default();
    let high_index = reasoning_items
        .iter()
        .position(|item| item == "high")
        .expect("reasoning selector should include high");
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Reasoning,
                        item_index: high_index,
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

    let (_name, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(profile.latency_preference.as_deref(), Some("fast"));
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_latency.as_deref(), Some("fast"));

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
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
    assert!(
        service.pane_agent_status_selector.is_some(),
        "latency selector should remain available after reasoning changes"
    );
}

/// Verifies that `/routing` stores a pane-local override used by
/// subsequent turns without mutating the global configured default. This covers
/// the command surface for enabling, toggling, and inspecting automatic model
/// sizing.
#[test]
fn runtime_agent_shell_routing_command_sets_pane_override() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let enabled = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routing-on","method":"agent/shell/command","params":{"idempotency_key":"routing-on","input":"/routing on"}}"#,
        &primary,
    );

    assert!(enabled.contains(r#""kind":"mutated""#), "{enabled}");
    assert!(enabled.contains(r#""command":"routing""#), "{enabled}");
    assert!(enabled.contains("enabled=true"), "{enabled}");
    assert!(enabled.contains("default=false"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    assert_eq!(
        service.agent_routing_overrides.get("%1").copied(),
        Some(true)
    );

    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routing-status","method":"agent/shell/command","params":{"idempotency_key":"routing-status","input":"/routing status"}}"#,
        &primary,
    );
    assert!(status.contains(r#""kind":"display""#), "{status}");
    assert!(status.contains("enabled=true"), "{status}");
    assert!(status.contains("override_present=true"), "{status}");

    let toggled = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routing-toggle","method":"agent/shell/command","params":{"idempotency_key":"routing-toggle","input":"/routing toggle"}}"#,
        &primary,
    );
    assert!(toggled.contains(r#""kind":"mutated""#), "{toggled}");
    assert!(toggled.contains("enabled=false"), "{toggled}");
    assert!(toggled.contains("changed=true"), "{toggled}");
    assert_eq!(
        service.agent_routing_overrides.get("%1").copied(),
        Some(false)
    );
}

/// Verifies that routing runs an internal router request before
/// the turn provider request, applies the selected model and reasoning effort,
/// and keeps router prompt/response correspondence out of persisted model
/// context. Only the effective profile and bounded logs survive.
#[test]
fn runtime_agent_turn_routing_selects_profile_without_context_leak() {
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
default_provider = "runtime-batch"
default_model_profile = "default"
routing = true

[agents.auto_sizing]
router_model_profile = "router"
small_model_profile = "small"
medium_model_profile = "medium"
large_model_profile = "large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]
fallback_policy = "use-default-profile"

[providers.runtime-batch]
kind = "openai"
models = ["gpt-router", "gpt-default", "gpt-5.3-codex", "gpt-5.4", "gpt-5.5"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"
reasoning_profile = "medium"

[model_profiles.router]
provider = "runtime-batch"
model = "gpt-router"
reasoning_profile = "low"

[model_profiles.small]
provider = "runtime-batch"
model = "gpt-5.3-codex"
reasoning_profile = "medium"

[model_profiles.medium]
provider = "runtime-batch"
model = "gpt-5.4"
reasoning_profile = "medium"

[model_profiles.large]
provider = "runtime-batch"
model = "gpt-5.5"
reasoning_profile = "high"
"#
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

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"auto-sized-prompt","method":"agent/shell/command","params":{"idempotency_key":"auto-sized-prompt","input":"implement this"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let frame_context = service.terminal_frame_context();
    let pane_context = frame_context
        .panes
        .get("%1")
        .expect("routing pane context should exist");
    assert_eq!(pane_context.agent_status.as_deref(), Some("routing"));
    assert!(
        pane_context
            .agent_display_lines
            .iter()
            .any(|line| line.starts_with("routing (") && line.contains(" • esc to interrupt")),
        "{pane_context:?}"
    );
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .extend([
            mez_agent::ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                label: "old minified assistant context for pane %1".to_string(),
                content: format!("minified-context:{}", "x".repeat(200 * 1024)),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                label: "transcript assistant entry 2 for pane %1".to_string(),
                content: "Recommended next tasks:\n1. Document the model picker.\n2. Clean up stale quota UI.\n3. Implement multi-file runtime auto-sizing.".to_string(),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                label: "previous tool output for pane %1".to_string(),
                content: "tool-only output should not reach the router".to_string(),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::Policy,
                label: "policy context".to_string(),
                content: "policy-only context should not reach the router".to_string(),
            },
        ]);

    let provider = RuntimeAutoSizingProvider {
        requests: RefCell::new(Vec::new()),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&provider, 1)
        .unwrap();
    assert_eq!(executions.len(), 1);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].interaction_kind,
        mez_agent::ModelInteractionKind::AutoSizing
    );
    assert_eq!(requests[0].model, "gpt-router");
    assert!(requests[0].turn_id.ends_with(":auto-sizing"));
    let router_context = requests[0]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(router_context.contains("implement this"));
    assert!(router_context.contains("Implement multi-file runtime auto-sizing"));
    assert!(router_context.contains("Latest submitted task"));
    assert!(router_context.contains("Referential prompt detected"));
    assert!(router_context.contains("Do not choose small/low merely because"));
    assert!(router_context.contains("Model size reflects task scope"));
    assert!(router_context.contains("reasoning effort reflects the depth and complexity"));
    assert!(router_context.contains("Small models are only for chat"));
    assert!(router_context.contains("Planning, investigation, complex implementation"));
    assert!(router_context.contains("Never choose low reasoning for coding"));
    assert!(router_context.contains("do not return only a discovery plan"));
    assert!(router_context.contains("[truncated for auto-sizing router]"));
    assert!(
        router_context.len() < 180 * 1024,
        "router context should stay bounded independently of model-window fallback estimates"
    );
    assert!(requests[0].messages.iter().any(|message| {
        message.role == mez_agent::ModelMessageRole::User
            && message.source == ContextSourceKind::UserInstruction
            && message.content.contains("implement this")
    }));
    assert!(requests[0].messages.iter().any(|message| {
        message.role == mez_agent::ModelMessageRole::Assistant
            && message.source == ContextSourceKind::TranscriptAssistant
            && message
                .content
                .contains("Implement multi-file runtime auto-sizing")
    }));
    assert!(
        !router_context.contains("tool-only output should not reach the router"),
        "{router_context}"
    );
    assert!(
        !router_context.contains("policy-only context should not reach the router"),
        "{router_context}"
    );
    assert_eq!(
        requests[1].interaction_kind,
        mez_agent::ModelInteractionKind::CapabilityDecision
    );
    assert_eq!(requests[1].model, "gpt-5.5");
    assert_eq!(requests[1].reasoning_effort.as_deref(), Some("high"));
    assert_eq!(executions[0].request.model, "gpt-5.5");
    assert_eq!(
        executions[0].request.reasoning_effort.as_deref(),
        Some("high")
    );
    let router_usage_key = mez_agent::ModelTokenUsageKey::new("runtime-batch", "gpt-router");
    assert_eq!(
        executions[0]
            .routing_token_usage_by_model
            .get(&router_usage_key)
            .copied(),
        Some(mez_agent::ModelTokenUsage {
            input_tokens: 90,
            output_tokens: 10,
            reasoning_tokens: 3,
            cached_input_tokens: Some(30),
            cache_write_input_tokens: None,
        })
    );
    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"auto-sizing-token-status","method":"agent/shell/command","params":{"idempotency_key":"auto-sizing-token-status","input":"/status"}}"#,
        &primary,
    );
    assert!(
        status.contains("| Pane agent tokens | 2 models; see Pane Agent Token Usage |"),
        "{status}"
    );
    assert!(status.contains("### Mez Session Token Usage"), "{status}");
    assert!(
        status.contains("| runtime-batch | gpt-router | 60 | 30 | 10 | 3 | 33.33% |"),
        "{status}"
    );
    assert!(
        status.contains("| runtime-batch | gpt-5.5 | 100 | 50 | 40 | 12 | 33.33% |"),
        "{status}"
    );
    let normal_request_context = requests[1]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!normal_request_context.contains(":auto-sizing"));
    assert!(!normal_request_context.contains("multi-file feature work"));
}

/// Verifies that an inaccessible routing model fails the turn instead of
/// silently falling back to the default profile.
///
/// Router provider request failures usually mean the configured router model is
/// unavailable to the account or provider. The user needs that provider error
/// surfaced so they can choose a routing model they can access.
#[test]
fn runtime_agent_turn_routing_provider_error_fails_turn() {
    struct InaccessibleRouterProvider {
        requests: RefCell<Vec<mez_agent::ModelRequest>>,
    }

    impl ModelProvider for InaccessibleRouterProvider {
        fn provider_id(&self) -> &str {
            "runtime-batch"
        }

        fn send_request(
            &self,
            request: &mez_agent::ModelRequest,
        ) -> Result<mez_agent::ModelResponse> {
            self.requests.borrow_mut().push(request.clone());
            if request.interaction_kind == mez_agent::ModelInteractionKind::AutoSizing {
                return Err(MezError::invalid_state(
                    "OpenAI Responses API returned status 404: model `gpt-5.3-codex-spark` is not available",
                )
                .with_provider_failure_json(
                    r#"{"status_code":404,"error":{"message":"model `gpt-5.3-codex-spark` is not available","type":"invalid_request_error","code":"model_not_found"}}"#,
                ));
            }
            Ok(runtime_say_response(
                &request.turn_id,
                "unexpected normal response",
                true,
            ))
        }
    }

    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-routing-provider-fail-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
routing = true

[agents.auto_sizing]
router_model_profile = "router"
small_model_profile = "default"
medium_model_profile = "default"
large_model_profile = "default"
fallback_policy = "use-default-profile"

[providers.runtime-batch]
kind = "openai"
models = ["gpt-default", "gpt-5.3-codex-spark"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"
reasoning_profile = "medium"

[model_profiles.router]
provider = "runtime-batch"
model = "gpt-5.3-codex-spark"
reasoning_profile = "low"
"#
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
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"router-fail-prompt","method":"agent/shell/command","params":{"idempotency_key":"router-fail-prompt","input":"use routing"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let provider = InaccessibleRouterProvider {
        requests: RefCell::new(Vec::new()),
    };
    let error = service
        .poll_agent_provider_tasks_with_provider(&provider, 1)
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error
            .message()
            .contains("auto-sizing router request failed for profile `router`"),
        "{error}"
    );
    assert_eq!(provider.requests.borrow().len(), 1);
    assert_eq!(
        provider.requests.borrow()[0].interaction_kind,
        mez_agent::ModelInteractionKind::AutoSizing
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    let failure = entries
        .iter()
        .find(|entry| {
            entry.role == crate::transcript::TranscriptRole::Assistant
                && entry.content.contains("provider_error")
        })
        .unwrap();
    assert!(failure.content.contains("gpt-5.3-codex-spark"));
    assert!(service.pending_agent_provider_tasks().is_empty());
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies that live agent pane rendering writes a separate durable
/// presentation log and does not leak presentation-only text into future model
/// context.
#[test]
fn runtime_agent_presentation_persistence_stays_out_of_model_context() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-presentation"));
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "visual-only pane replay")
        .unwrap();

    let presentation = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(presentation.len(), 1);
    assert_eq!(presentation[0].style_names, vec!["assistant"]);
    assert_eq!(
        presentation[0].display_lines,
        vec![String::from("mez> visual-only pane replay")]
    );
    assert!(
        presentation[0]
            .ansi_text
            .as_deref()
            .is_some_and(|text| text.contains("visual-only pane replay"))
    );
    assert!(transcript_store.inspect(&conversation_id).is_err());
    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        context
            .blocks
            .iter()
            .all(|block| !block.content.contains("visual-only pane replay"))
    );
}

/// Verifies a shell command rejected before dispatch by pane readiness is fed
/// back to the model for correction.
///
/// `pane_not_ready` means the shell command never reached the pane shell. The
/// model should receive that readiness diagnostic and choose a different next
/// step instead of the turn failing immediately.
#[test]
fn runtime_shell_pane_not_ready_queues_model_self_correction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the pager styling")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let action = mez_agent::AgentAction {
        id: "shell-not-ready".to_string(),
        rationale: "inspect the render owner".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the render owner.".to_string(),
            command: "rg -n \"status pager\" src".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "pane_not_ready",
        "pane %1 is not ready for agent shell input: interactive-blocked",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "state": "not_ready",
            "readiness_state": "interactive-blocked",
            "command": "rg -n \"status pager\" src"
        })
        .to_string(),
    );
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "pane not ready".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "pane_not_ready_recovery",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result shell-not-ready shell_command failed]")
            && block.content.contains("interactive-blocked")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.content.contains("Shell-readiness recovery")
    }));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}
