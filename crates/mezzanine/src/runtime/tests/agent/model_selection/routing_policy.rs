//! Runtime tests for model-routing selection and presentation policy.
//!
//! These tests cover pane-local routing inheritance, auto-sizing profile
//! application and fallback, provider failures, command overrides, status
//! presentation, context isolation, and readiness recovery at product seams.

use super::*;

/// Verifies subagents inherit the live parent pane routing decision.
///
/// Auto-reasoning is a pane-local agent behavior, not just a global default.
/// Child agents should continue with the parent pane's effective setting so a
/// user does not have to re-toggle it after spawning helpers.
#[test]
fn runtime_subagent_routing_inherits_parent_pane_setting() {
    let mut service = test_runtime_service();
    service.set_agent_default_routing(false);
    service.set_agent_routing_override("%1", Some(true));

    assert_eq!(
        service.inherited_routing_for_child_agent("agent-%1"),
        Some(true)
    );

    service.set_agent_routing_override("%1", None);
    service.set_agent_default_routing(true);
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
    let mut parent_auto_sizing = service.agent_auto_sizing().clone();
    parent_auto_sizing.router_model_profile = "deepseek-fast".to_string();
    parent_auto_sizing.small_model_profile = "deepseek-fast".to_string();
    parent_auto_sizing.medium_model_profile = "deepseek-default".to_string();
    parent_auto_sizing.large_model_profile = "deepseek-default".to_string();
    parent_auto_sizing.allowed_reasoning_efforts = vec!["high".to_string(), "xhigh".to_string()];
    service.set_agent_auto_sizing_override("%1", Some(parent_auto_sizing.clone()));

    assert_eq!(
        service.inherited_auto_sizing_for_child_agent("agent-%1"),
        Some(parent_auto_sizing)
    );
}

/// Verifies the `spawn_agent` schema contract matches the explicit-pair policy.
///
/// Recent model-config changes narrowed the DeepSeek preset to `low`, `high`,
/// and `xhigh`, but the static schema still offered `medium`, so a schema-led
/// spawn was rejected as a disallowed reasoning effort. Advertised sizes and
/// reasoning levels must come from the same policy data the runtime validates
/// against, and a level the policy rejects must not appear in the schema.
#[test]
fn runtime_spawn_agent_sizing_reflects_explicit_pair_policy() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "deepseek-sizing-policy".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"deepseek\"\ndefault_model_profile = \"deepseek-default\"\n[agents.auto_sizing]\nrouter_model_profile = \"deepseek-fast\"\nsmall_model_profile = \"deepseek-fast\"\nmedium_model_profile = \"deepseek-default\"\nlarge_model_profile = \"deepseek-default\"\nallowed_reasoning_efforts = [\"low\", \"high\", \"xhigh\"]\n[providers.deepseek]\nkind = \"deepseek\"\ndefault_model = \"deepseek-v4-pro\"\n[providers.deepseek.models.deepseek-v4-pro]\nid = \"deepseek-v4-pro\"\nreasoning_levels = [\"low\", \"high\", \"max\"]\n[model_profiles.deepseek-default]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_profile = \"high\"\n[model_profiles.deepseek-fast]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_profile = \"high\"\n"
                .to_string(),
        }])
        .unwrap();

    let sizing = service
        .runtime_spawn_agent_sizing_for_pane("%1")
        .expect("configured deepseek routing profiles should resolve");
    assert_eq!(sizing.sizes.len(), 3);
    for option in &sizing.sizes {
        assert!(
            !option
                .allowed_reasoning_efforts
                .contains(&"medium".to_string()),
            "medium is not configured for `{}`: {:?}",
            option.size,
            option.allowed_reasoning_efforts
        );
        for effort in &option.allowed_reasoning_efforts {
            service
                .runtime_explicit_auto_sizing_selection_for_pane("%1", &option.size, effort)
                .expect("advertised reasoning level should resolve for its size");
        }
    }
    assert!(
        service
            .runtime_explicit_auto_sizing_selection_for_pane("%1", "medium", "medium")
            .is_err()
    );

    let schema = mez_agent::maap_action_batch_schema(
        &mez_agent::AllowedActionSet::all_enabled().with_spawn_agent_sizing(sizing),
        &[],
    );
    let spawn = schema["properties"]["actions"]["items"]["anyOf"]
        .as_array()
        .and_then(|variants| {
            variants.iter().find(|variant| {
                variant["properties"]["type"]["enum"] == serde_json::json!(["spawn_agent"])
            })
        })
        .expect("spawn_agent schema variant");
    let enum_values = spawn["properties"]["reasoning_effort"]["enum"]
        .as_array()
        .expect("reasoning enum");
    assert!(!enum_values.contains(&serde_json::json!("medium")));
    assert!(enum_values.contains(&serde_json::json!("high")));
    let description = spawn["properties"]["reasoning_effort"]["description"]
        .as_str()
        .expect("reasoning description");
    assert!(
        description.contains("Configured allowed reasoning efforts by size"),
        "{description}"
    );
}

/// Verifies a frozen sizing catalog selects its captured execution profile
/// without consulting mutable live profile definitions after the snapshot.
#[test]
fn runtime_explicit_spawn_sizing_uses_frozen_execution_profile() {
    let service = test_runtime_service();
    let captured = runtime_model_profile("runtime-batch", "frozen-model");
    let catalog = mez_agent::AllowedActionSet::from_actions([
        mez_agent::AllowedAction::Say,
        mez_agent::AllowedAction::SpawnAgent,
    ])
    .with_spawn_agent_sizing(mez_agent::SpawnAgentSizing {
        sizes: vec![mez_agent::SpawnAgentSizeOption {
            size: "small".to_string(),
            profile_name: "removed-live-profile".to_string(),
            execution_profile: Some(captured.clone()),
            allowed_reasoning_efforts: vec!["high".to_string()],
        }],
    });

    let selection = service
        .runtime_explicit_auto_sizing_selection_from_catalog(&catalog, "small", "high")
        .expect("frozen sizing must not resolve the removed live profile");

    assert_eq!(selection.selected_profile_name, "removed-live-profile");
    assert_eq!(selection.selected_profile.provider, captured.provider);
    assert_eq!(selection.selected_profile.model, captured.model);
    assert_eq!(
        selection.selected_profile.reasoning_profile.as_deref(),
        Some("high")
    );
    let error = service
        .runtime_explicit_auto_sizing_selection_from_catalog(&catalog, "small", "low")
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("reasoning effort is not allowed for the frozen model size"),
        "{error}"
    );
}

/// Verifies legacy frozen sizing entries without execution profiles remain
/// readable while rejecting explicit selections, matching the schema's
/// null-only automatic-routing branch for those entries.
#[test]
fn runtime_legacy_spawn_sizing_rejects_explicit_selection() {
    let service = test_runtime_service();
    let captured = runtime_model_profile("runtime-batch", "frozen-large");
    let catalog = mez_agent::AllowedActionSet::from_actions([
        mez_agent::AllowedAction::Say,
        mez_agent::AllowedAction::SpawnAgent,
    ])
    .with_spawn_agent_sizing(mez_agent::SpawnAgentSizing {
        sizes: vec![
            mez_agent::SpawnAgentSizeOption {
                size: "small".to_string(),
                profile_name: "legacy-small".to_string(),
                execution_profile: None,
                allowed_reasoning_efforts: vec!["low".to_string()],
            },
            mez_agent::SpawnAgentSizeOption {
                size: "large".to_string(),
                profile_name: "frozen-large".to_string(),
                execution_profile: Some(captured.clone()),
                allowed_reasoning_efforts: vec!["high".to_string()],
            },
        ],
    });

    let error = service
        .runtime_explicit_auto_sizing_selection_from_catalog(&catalog, "small", "low")
        .unwrap_err();

    assert!(
        error.message().contains("lacks the execution profile"),
        "{error}"
    );
    let selection = service
        .runtime_explicit_auto_sizing_selection_from_catalog(&catalog, "large", "high")
        .expect("captured entries remain selectable beside legacy entries");
    assert_eq!(selection.selected_profile.provider, captured.provider);
    assert_eq!(selection.selected_profile.model, captured.model);
    assert_eq!(
        selection.selected_profile.reasoning_profile.as_deref(),
        Some("high")
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
    service.set_pane_screen("%1".to_string(), screen);
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
            .agent_turn_ledger()
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
            entry.role == mez_agent::transcript::TranscriptRole::Assistant
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
            reasoning_levels: Some(vec!["low".to_string(), "high".to_string()]),
            context_window_tokens: Some(1_050_000),
            max_input_tokens: None,
            max_output_tokens: None,
            capabilities: None,
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
        .pane_agent_status_selector()
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
        service.pane_agent_status_selector().is_some(),
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
    assert_eq!(service.agent_routing_override("%1"), Some(true));

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
    assert_eq!(service.agent_routing_override("%1"), Some(false));
}

/// Verifies `/routing policy` is pane-local unless `--global` is explicit.
///
/// Pane overrides must remain sparse and win over later global fallback
/// changes, malformed forms must be mutation-free, and delegated subagents
/// must continue to use in-place routing regardless of either root policy.
#[test]
fn runtime_agent_shell_routing_policy_scopes_changes_and_preserves_subagents() {
    let mut service = test_runtime_service();
    let config_root = temp_root("runtime-agent-shell-routing-policy");
    let _ = fs::remove_dir_all(&config_root);
    service.set_config_root(config_root.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let selected = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routing-policy","method":"agent/shell/command","params":{"idempotency_key":"routing-policy","input":"/routing policy subagent"}}"#,
        &primary,
    );
    assert!(selected.contains(r#""kind":"mutated""#), "{selected}");
    assert!(selected.contains("root_policy=subagent"), "{selected}");
    assert!(selected.contains("scope=pane"), "{selected}");
    assert_eq!(
        service.agent_root_routing_policy_override("%1"),
        Some(mez_agent::AutoSizingRoutingPolicy::Subagent)
    );
    assert!(!config_root.join("config.toml").exists());

    let root_turn = mez_agent::AgentTurnRecord {
        turn_id: "root-routing-policy".to_string(),
        conversation_id: "conversation-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        deadline_at_unix_millis: 0,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        initial_capability: None,
        state: mez_agent::AgentTurnState::Running,
    };
    assert_eq!(
        service.auto_sizing_routing_policy_for_turn(&root_turn),
        mez_agent::AutoSizingRoutingPolicy::Subagent
    );

    let other_root_turn = mez_agent::AgentTurnRecord {
        turn_id: "other-root-routing-policy".to_string(),
        conversation_id: "conversation-1".to_string(),
        agent_id: "agent-%2".to_string(),
        pane_id: "%2".to_string(),
        ..root_turn.clone()
    };
    assert_eq!(
        service.auto_sizing_routing_policy_for_turn(&other_root_turn),
        mez_agent::AutoSizingRoutingPolicy::Subagent
    );

    let global = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routing-policy-global","method":"agent/shell/command","params":{"idempotency_key":"routing-policy-global","input":"/routing policy --global in-place"}}"#,
        &primary,
    );
    assert!(global.contains(r#""kind":"mutated""#), "{global}");
    assert!(global.contains("scope=global"), "{global}");
    assert!(global.contains("root_policy=in-place"), "{global}");
    assert!(
        fs::read_to_string(config_root.join("config.toml"))
            .unwrap()
            .contains("root_routing_policy = \"in-place\"")
    );
    assert_eq!(
        service.auto_sizing_routing_policy_for_turn(&root_turn),
        mez_agent::AutoSizingRoutingPolicy::Subagent
    );
    assert_eq!(
        service.auto_sizing_routing_policy_for_turn(&other_root_turn),
        mez_agent::AutoSizingRoutingPolicy::InPlace
    );

    service.set_subagent_lineage(
        "agent-child",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "child".to_string(),
            terminal: false,
        },
    );
    let child_turn = mez_agent::AgentTurnRecord {
        agent_id: "agent-child".to_string(),
        ..root_turn.clone()
    };
    assert_eq!(
        service.auto_sizing_routing_policy_for_turn(&child_turn),
        mez_agent::AutoSizingRoutingPolicy::InPlace
    );

    let persisted_before_rejections = fs::read_to_string(config_root.join("config.toml")).unwrap();
    for (id, input) in [
        ("invalid", "/routing policy invalid"),
        ("missing", "/routing policy --global"),
        ("misplaced", "/routing policy in-place --global"),
        ("duplicate", "/routing policy --global --global in-place"),
        ("unknown", "/routing policy --session in-place"),
    ] {
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":"{id}","method":"agent/shell/command","params":{{"idempotency_key":"{id}","input":"{input}"}}}}"#
        );
        let rejected = service.dispatch_runtime_control_body(&request, &primary);
        assert!(rejected.contains("invalid_params"), "{rejected}");
    }
    assert_eq!(
        service.agent_root_routing_policy_override("%1"),
        Some(mez_agent::AutoSizingRoutingPolicy::Subagent)
    );
    assert_eq!(
        fs::read_to_string(config_root.join("config.toml")).unwrap(),
        persisted_before_rejections
    );

    service.cleanup_removed_pane_runtime_state("%1").unwrap();
    assert_eq!(service.agent_root_routing_policy_override("%1"), None);
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies routing applies the selected worker profile while forking the
/// managed child from parent conversation and tool history.
#[test]
fn runtime_agent_turn_routing_selects_profile_with_parent_context() {
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
    service.set_pane_screen("%1".to_string(), screen);
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
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    for block in [
            mez_agent::ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "old minified assistant context for pane %1".to_string(),
                content: format!("minified-context:{}", "x".repeat(200 * 1024)),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "transcript assistant entry 2 for pane %1".to_string(),
                content: "Recommended next tasks:\n1. Document the model picker.\n2. Clean up stale quota UI.\n3. Implement multi-file runtime auto-sizing.".to_string(),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "previous tool output for pane %1".to_string(),
                content: "tool-only output should not reach the router".to_string(),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::Policy,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "policy context".to_string(),
                content: "policy-only context should not reach the router".to_string(),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "active routed action result".to_string(),
                content: "action-result sentinel must reach the routed worker".to_string(),
            },
        ] {
        insert_test_context_block(context, block);
    }

    let provider = RuntimeAutoSizingProvider {
        requests: RefCell::new(Vec::new()),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&provider, 1)
        .unwrap();
    assert!(executions.is_empty());
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 1);
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
    assert!(router_context.contains("The following ordered user, assistant"));
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
    let workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("routing should create a managed worker workflow");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult,
        "{workflow:#?}"
    );
    assert_eq!(workflow.main_model_profile, "default");
    assert_eq!(workflow.worker_model_profile.as_deref(), Some("gpt-5.5"));
    assert_eq!(workflow.original_user_prompt, "implement this");
    let child_turn_id = workflow
        .child_turn_id
        .clone()
        .expect("managed worker turn should be queued");
    let child_profile = service
        .agent_turn_model_profile(&child_turn_id)
        .expect("managed worker profile should be pinned");
    assert_eq!(child_profile.model, "gpt-5.5");
    assert_eq!(child_profile.reasoning_profile.as_deref(), Some("high"));
    let child_context = service
        .agent_turn_contexts()
        .get(&child_turn_id)
        .expect("managed worker context should exist");
    assert_eq!(
        child_context
            .blocks()
            .iter()
            .filter(|block| block.content == "implement this")
            .count(),
        1
    );
    assert!(child_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::TranscriptAssistant
            && block
                .content
                .contains("Implement multi-file runtime auto-sizing")
    }));
    assert!(child_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::TranscriptTool
            && block.content == "tool-only output should not reach the router"
    }));
    assert!(child_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content == "action-result sentinel must reach the routed worker"
    }));
    assert!(child_context.validate_durable().is_ok());
    assert!(!child_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::Policy
            && block.content == "policy-only context should not reach the router"
    }));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(mez_agent::AgentTurnState::Blocked)
    );
    drop(requests);
    let waiting_tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routed-worker-wait","method":"agent/task/list","params":{"target":{"agent_id":"agent-%1"}}}"#,
        &primary,
    );
    assert!(
        waiting_tasks.contains(r#""state":"waiting""#),
        "{waiting_tasks}"
    );
    assert!(
        waiting_tasks.contains(r#""approval_ids":[]"#),
        "{waiting_tasks}"
    );
    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"auto-sizing-token-status","method":"agent/shell/command","params":{"idempotency_key":"auto-sizing-token-status","input":"/status"}}"#,
        &primary,
    );
    assert!(
        status.contains("| Pane agent tokens | gpt-router via runtime-batch:"),
        "{status}"
    );
    assert!(status.contains("### Mez Session Token Usage"), "{status}");
    assert!(
        status.contains("| runtime-batch | gpt-router | 60 | 30 | 10 | 3 | 33.33% |"),
        "{status}"
    );
    assert!(!status.contains("| runtime-batch | gpt-5.5 |"), "{status}");

    let completed_say_execution = |turn: &mez_agent::AgentTurnRecord, text: &str| {
        let action = mez_agent::AgentAction {
            id: format!("say-{}", turn.turn_id),

            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Final,
                text: text.to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        };
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "completed routed response".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::succeeded(
                turn,
                &action,
                vec![text.to_string()],
                None,
            )],
            final_turn: true,
            terminal_state: AgentTurnState::Completed,
        }
    };
    let completed_progress_and_final_execution =
        |turn: &mez_agent::AgentTurnRecord, progress: &str, final_text: &str| {
            let progress_action = mez_agent::AgentAction {
                id: format!("progress-{}", turn.turn_id),

                payload: mez_agent::AgentActionPayload::Say {
                    status: mez_agent::SayStatus::Progress,
                    text: progress.to_string(),
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                },
            };
            let final_action = mez_agent::AgentAction {
                id: format!("final-{}", turn.turn_id),

                payload: mez_agent::AgentActionPayload::Say {
                    status: mez_agent::SayStatus::Final,
                    text: final_text.to_string(),
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                },
            };
            mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
                response: mez_agent::ModelResponse {
                    provider: "runtime-batch".to_string(),
                    model: "test".to_string(),
                    raw_text: "completed routed response".to_string(),
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Default::default(),
                    action_batch: Some(mez_agent::MaapBatch {
                        rationale: "report progress and the final routed result".to_string(),

                        actions: vec![progress_action.clone(), final_action.clone()],
                    }),
                    provider_transcript_events: Vec::new(),
                },
                latest_response_usage: Default::default(),
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: vec![
                    mez_agent::ActionResult::succeeded(
                        turn,
                        &progress_action,
                        vec![progress.to_string()],
                        None,
                    ),
                    mez_agent::ActionResult::succeeded(
                        turn,
                        &final_action,
                        vec![final_text.to_string()],
                        None,
                    ),
                ],
                final_turn: true,
                terminal_state: AgentTurnState::Completed,
            }
        };
    let worker_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == child_turn_id)
        .cloned()
        .expect("managed worker turn should remain recorded");
    let exact_worker_result = "Implemented the routed fix and verified its regression test.";
    let worker_progress = "Still running the routed regression test.";
    let worker_context = service
        .agent_turn_contexts_mut()
        .get_mut(&worker_turn.turn_id)
        .expect("managed worker context should be available before completion");
    for block in [
        mez_agent::ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "live routed worker assistant context".to_string(),
            content: "live assistant sentinel must reach the routed handoff".to_string(),
        },
        mez_agent::ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "live routed worker action result".to_string(),
            content: "live action-result sentinel must reach the routed handoff".to_string(),
        },
    ] {
        insert_test_context_block(worker_context, block);
    }
    assert!(
        service
            .handle_routed_child_execution_result(
                &worker_turn,
                &completed_progress_and_final_execution(
                    &worker_turn,
                    worker_progress,
                    exact_worker_result,
                ),
            )
            .unwrap()
    );

    let workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("worker completion should advance the routed workflow");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForHandoff
    );
    assert_eq!(
        workflow.worker_final_result.as_deref(),
        Some(exact_worker_result)
    );
    assert_ne!(
        workflow.worker_final_result.as_deref(),
        Some(worker_progress)
    );
    let handoff_turn_id = workflow
        .child_turn_id
        .as_deref()
        .expect("worker completion should queue a handoff turn");
    let handoff_context = service
        .agent_turn_contexts()
        .get(handoff_turn_id)
        .expect("handoff context should be recorded");
    assert!(handoff_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::TranscriptAssistant
            && block.content == "live assistant sentinel must reach the routed handoff"
    }));
    assert!(handoff_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content == "live action-result sentinel must reach the routed handoff"
    }));
    assert!(handoff_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "routed worker exact final result"
            && block.content == exact_worker_result
    }));

    let handoff_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == handoff_turn_id)
        .cloned()
        .expect("handoff turn should remain recorded");
    assert!(
        service
            .handle_routed_child_execution_result(
                &handoff_turn,
                &completed_say_execution(&handoff_turn, "invalid handoff"),
            )
            .unwrap()
    );

    let workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("invalid handoff should retain the routed workflow");
    assert_eq!(workflow.handoff_repair_attempts, 1);
    let repair_turn_id = workflow
        .child_turn_id
        .as_deref()
        .expect("invalid handoff should queue one repair turn");
    let repair_context = service
        .agent_turn_contexts()
        .get(repair_turn_id)
        .expect("repair context should be recorded");
    assert_eq!(
        repair_context
            .blocks()
            .iter()
            .filter(|block| {
                block.source == ContextSourceKind::RoutedHandoff
                    && block.label == "routed worker exact final result"
                    && block.content == exact_worker_result
            })
            .count(),
        1
    );
    assert!(repair_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "invalid routed handoff output"
            && block.content == "invalid handoff"
    }));
    assert!(repair_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "routed handoff validation feedback"
            && block
                .content
                .contains("invalid routed handoff JSON: expected value")
    }));
    assert!(repair_context.validate_durable().is_ok());

    let repair_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == repair_turn_id)
        .cloned()
        .expect("repair turn should remain recorded");
    let valid_handoff = r#"{"version":1,"result_summary":"Routing fix complete","decisions":["preserve exact output"],"evidence":["regression passed"],"changes":["added routed result context"],"validation":["focused test"],"assumptions":[],"unresolved_risks":[],"follow_up_context":[]}"#;
    assert!(
        service
            .handle_routed_child_execution_result(
                &repair_turn,
                &completed_say_execution(&repair_turn, valid_handoff),
            )
            .unwrap()
    );

    let parent_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .expect("parent context should remain recorded");
    assert!(parent_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "routed worker exact final result"
            && block.content == exact_worker_result
    }));
    assert!(parent_context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "routed worker handoff context"
            && block
                .content
                .contains("\"result_summary\":\"Routing fix complete\"")
    }));
    assert_eq!(
        service
            .routed_workflow_for_tests("turn-1")
            .map(|workflow| workflow.phase.clone()),
        Some(mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForPresentation)
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .and_then(|turn| turn.initial_capability),
        Some(mez_agent::AgentCapability::RespondOnly),
        "routed presentation must expose a hard response-only action surface"
    );
    let frame_context = service.terminal_frame_context();
    let pane_context = frame_context
        .panes
        .get("%1")
        .expect("routed parent pane context should exist");
    assert_eq!(
        pane_context.agent_status.as_deref(),
        Some("thinking"),
        "routed parent presentation must not re-enter the routing status"
    );
    let parent_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .expect("parent turn should remain recorded");
    service
        .complete_running_agent_turn_and_start_ready(
            &parent_turn,
            AgentTurnState::Failed,
            "routed presentation failed",
        )
        .unwrap();
    let failed_workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("failed parent presentation should remain observable");
    assert_eq!(
        failed_workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ExplainingError
    );
    assert_eq!(
        failed_workflow.diagnostic.as_deref(),
        Some("routed parent presentation failed")
    );
    assert!(failed_workflow.error_explanation_attempted);
    service
        .complete_running_agent_turn_and_start_ready(
            &parent_turn,
            AgentTurnState::Failed,
            "routed error explanation failed",
        )
        .unwrap();
    assert!(service.routed_workflow_for_tests("turn-1").is_none());
    assert!(!service.has_active_routed_workflow("turn-1"));
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
    service.set_pane_screen("%1".to_string(), screen);
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
            .agent_turn_ledger()
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
            entry.role == mez_agent::transcript::TranscriptRole::Assistant
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
            .blocks()
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
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let action = mez_agent::AgentAction {
        id: "shell-not-ready".to_string(),

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
                rationale: "test action batch rationale".to_string(),

                actions: vec![action],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };
    service
        .append_agent_execution_chronology(&turn, &execution)
        .unwrap();

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "pane_not_ready_recovery",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let durable = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    let context = runtime_prepared_context_for_turn(&service, &turn.turn_id);
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result shell-not-ready shell_command failed]")
            && block.content.contains("interactive-blocked")
    }));
    assert!(!durable.blocks().iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.content.contains("Shell-readiness recovery")
    }));
    assert!(durable.validate_durable().is_ok());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Configures distinct auto-sizing targets whose models and reasoning levels
/// differ from the parent default, so explicit child sizing is observable.
///
/// The `deepseek-large` target configures `high` reasoning while allowing
/// `low`, which lets regressions distinguish a requested effort from the target
/// profile's own configured effort.
const EXPLICIT_SUBAGENT_SIZING_CONFIG: &str = "[agents]\ndefault_provider = \"deepseek\"\ndefault_model_profile = \"deepseek-default\"\nshell_mode = \"native\"\n\n[agents.auto_sizing]\nrouter_model_profile = \"deepseek-router\"\nsmall_model_profile = \"deepseek-small\"\nmedium_model_profile = \"deepseek-medium\"\nlarge_model_profile = \"deepseek-large\"\nallowed_reasoning_efforts = [\"low\", \"high\"]\n\n[permissions]\napproval_policy = \"ask\"\nsandbox = \"policy-only\"\n\n[providers.deepseek]\nkind = \"deepseek\"\ndefault_model = \"deepseek-v4-pro\"\n\n[providers.deepseek.models.deepseek-v4-flash]\nid = \"deepseek-v4-flash\"\nreasoning_levels = [\"low\", \"high\", \"max\"]\n\n[providers.deepseek.models.deepseek-v4-pro]\nid = \"deepseek-v4-pro\"\nreasoning_levels = [\"low\", \"high\", \"max\"]\n\n[providers.deepseek.models.deepseek-v4-max]\nid = \"deepseek-v4-max\"\nreasoning_levels = [\"low\", \"high\", \"max\"]\n\n[model_profiles.deepseek-default]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_profile = \"high\"\n\n[model_profiles.deepseek-router]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-flash\"\nreasoning_profile = \"low\"\n\n[model_profiles.deepseek-small]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-flash\"\nreasoning_profile = \"low\"\n\n[model_profiles.deepseek-medium]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_profile = \"low\"\n\n[model_profiles.deepseek-large]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-max\"\nreasoning_profile = \"high\"\n";

/// Verifies an explicit `spawn_agent` size/reasoning pair becomes the spawned
/// child's durable model identity instead of a single-turn override.
///
/// The frozen sizing catalog resolves `size` and `reasoning_effort` into one
/// execution profile, and the spawn response advertises that selection as the
/// child's model. Every later child turn must keep resolving the requested
/// model and reasoning level; reverting to the parent profile on the second
/// turn is the defect this regression pins.
#[test]
fn runtime_spawn_explicit_size_pair_governs_child_model_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let (parent_profile_name, parent_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(parent_profile_name, "deepseek-default");
    assert_eq!(parent_profile.model, "deepseek-v4-pro");

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: Some("large".to_string()),
                initial_reasoning_effort: Some("high".to_string()),
                task_prompt: "inspect the explicit subagent size policy".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let child_agent_id = spawned["agent"]["id"].as_str().unwrap().to_string();
    let child_pane_id = spawned["agent"]["pane_id"].as_str().unwrap().to_string();
    assert_eq!(
        spawned["agent"]["initial_model_size"].as_str(),
        Some("large")
    );
    assert_eq!(
        spawned["agent"]["initial_model_profile"].as_str(),
        Some("deepseek-large")
    );
    let (durable_profile_name, durable_profile) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_ne!(durable_profile_name, parent_profile_name);
    assert_eq!(durable_profile.model, "deepseek-v4-max");
    assert_eq!(
        spawned["agent"]["model_profile"].as_str(),
        Some(durable_profile_name.as_str()),
        "the spawn response must report the child's durable model profile"
    );
    let child_turn_id = spawned["turn"]["id"].as_str().unwrap().to_string();
    let child_turn_profile = service
        .agent_turn_model_profile(&child_turn_id)
        .expect("child initial turn profile should exist")
        .clone();
    assert_eq!(child_turn_profile.model, "deepseek-v4-max");
    assert_eq!(
        child_turn_profile.reasoning_profile.as_deref(),
        Some("high")
    );

    // A real follow-up child turn resolves the child's durable agent-scoped
    // profile instead of the initial turn selection, so the explicit pair must
    // be reflected there too.
    let follow_up = service
        .start_agent_prompt_turn(&child_pane_id, "continue the review")
        .unwrap();
    let follow_up_profile = service
        .agent_turn_model_profile(&follow_up.turn_id)
        .expect("follow-up child turn profile should exist")
        .clone();
    assert_eq!(
        (
            follow_up_profile.model.as_str(),
            follow_up_profile.reasoning_profile.as_deref(),
        ),
        ("deepseek-v4-max", Some("high")),
        "a later child turn must keep the explicit size pair, not the parent profile {:?}",
        (parent_profile_name.as_str(), parent_profile.model.as_str())
    );
    let (later_profile_name, later_profile) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_ne!(later_profile_name, parent_profile_name);
    assert_eq!(later_profile.model, "deepseek-v4-max");
    assert_eq!(later_profile.reasoning_profile.as_deref(), Some("high"));
    let follow_up_turn_profile_name = service
        .agent_turn_ledger()
        .turn(&follow_up.turn_id)
        .map(|turn| turn.model_profile.clone())
        .expect("follow-up child turn should be recorded");
    assert_eq!(later_profile_name, follow_up_turn_profile_name);
}

/// Spawns one explorer child with an explicit size/reasoning pair and returns
/// its agent id, pane id, and initial turn id.
///
/// The helper keeps the requested-effort regressions focused on profile
/// identity instead of repeating the full spawn request shape.
fn spawn_explicitly_sized_child(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
    size: &str,
    reasoning_effort: &str,
) -> (String, String, String) {
    let spawned = service
        .spawn_runtime_subagent(
            primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: Some(size.to_string()),
                initial_reasoning_effort: Some(reasoning_effort.to_string()),
                task_prompt: "review the requested reasoning level".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    (
        spawned["agent"]["id"].as_str().unwrap().to_string(),
        spawned["agent"]["pane_id"].as_str().unwrap().to_string(),
        spawned["turn"]["id"].as_str().unwrap().to_string(),
    )
}

/// Verifies a spawn-sized child's model identity is captured durably.
///
/// The child's agent-scoped profile used to live only in runtime memory, so a
/// restart silently changed the child's model and reasoning level. The identity
/// now lands on the child conversation's sidecar together with the selection
/// needed to re-materialize a runtime-generated definition, and an unrelated
/// pane must not inherit it.
#[test]
fn runtime_spawn_explicit_sizing_captures_durable_child_model_identity() {
    let transcript_store = crate::storage::transcript::AgentTranscriptStore::new(temp_root(
        "runtime-spawn-sized-child-identity",
    ));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing-identity".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let (child_agent_id, child_pane_id, _turn_id) =
        spawn_explicitly_sized_child(&mut service, &primary, "large", "high");
    let (effective_name, effective) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();

    let child_conversation_id = service
        .agent_shell_store()
        .get(&child_pane_id)
        .expect("the child pane owns an agent shell session")
        .session_id
        .clone();
    let primary_conversation_id = service
        .agent_shell_store()
        .get("%1")
        .expect("the primary pane owns an agent shell session")
        .session_id
        .clone();
    let (captured_name, captured_selection) = transcript_store
        .conversation_model_identity(&child_conversation_id)
        .unwrap()
        .expect("the child model identity must be persisted at spawn");
    assert_eq!(captured_name, effective_name);
    let captured_selection =
        captured_selection.expect("a runtime-generated profile carries its selection");
    assert_eq!(captured_selection.model, effective.model);
    assert_eq!(
        captured_selection.reasoning_profile.as_deref(),
        effective.reasoning_profile.as_deref()
    );
    assert_eq!(
        transcript_store
            .conversation_model_identity(&primary_conversation_id)
            .unwrap(),
        None,
        "an unrelated pane must not inherit the child model identity"
    );
}

/// Spawns one child with no explicit size/reasoning pair so its identity comes
/// from the role or the inherited parent resolution.
fn spawn_child_without_explicit_sizing(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
) -> (String, String, String) {
    let spawned = service
        .spawn_runtime_subagent(
            primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "inherit the parent model identity".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    (
        spawned["agent"]["id"].as_str().unwrap().to_string(),
        spawned["agent"]["pane_id"].as_str().unwrap().to_string(),
        spawned["turn"]["id"].as_str().unwrap().to_string(),
    )
}

/// Returns the persisted model identity of one pane's conversation.
fn persisted_model_identity(
    service: &RuntimeSessionService,
    store: &crate::storage::transcript::AgentTranscriptStore,
    pane_id: &str,
) -> (
    String,
    Option<crate::storage::transcript::AgentModelProfileSelection>,
) {
    let conversation_id = service
        .agent_shell_store()
        .get(pane_id)
        .expect("the pane owns an agent shell session")
        .session_id
        .clone();
    store
        .conversation_model_identity(&conversation_id)
        .unwrap()
        .expect("the pane conversation must carry a durable model identity")
}

/// Verifies a configured inherited profile is captured by name only.
///
/// The capture split must keep configuration authoritative: a child that
/// inherited a configured profile persists its name without a selection, so a
/// later removal of that profile from configuration degrades through the
/// documented fallback instead of being re-materialized from a stale definition.
#[test]
fn runtime_configured_child_model_identity_captures_name_only() {
    let transcript_store = crate::storage::transcript::AgentTranscriptStore::new(temp_root(
        "runtime-configured-child-identity",
    ));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing-configured".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .integration
        .model_profile_overrides_mut()
        .agent_profiles
        .insert("agent-%1".to_string(), "deepseek-small".to_string());

    let (child_agent_id, child_pane_id, _turn_id) =
        spawn_child_without_explicit_sizing(&mut service, &primary);
    assert_eq!(
        service
            .integration
            .model_profile_overrides()
            .agent_profiles
            .get(&child_agent_id)
            .map(String::as_str),
        Some("deepseek-small"),
        "the child must inherit the configured parent profile"
    );
    let (captured_name, captured_selection) =
        persisted_model_identity(&service, &transcript_store, &child_pane_id);
    assert_eq!(captured_name, "deepseek-small");
    assert_eq!(
        captured_selection, None,
        "a configured name must not carry a selection"
    );
}

/// Verifies an inherited runtime-generated identity keeps its selection.
///
/// A generated name inherited from an ancestor is still a runtime selection, so
/// the marker - not the spawn branch - must decide whether its definition is
/// captured; otherwise the child degrades on restart even though the runtime can
/// reproduce the identical profile.
#[test]
fn runtime_inherited_generated_identity_keeps_its_selection() {
    let transcript_store = crate::storage::transcript::AgentTranscriptStore::new(temp_root(
        "runtime-inherited-generated-identity",
    ));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing-inherited".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let (sized_agent_id, _sized_pane_id, _turn_id) =
        spawn_explicitly_sized_child(&mut service, &primary, "large", "high");
    let generated_name = service
        .integration
        .model_profile_overrides()
        .agent_profiles
        .get(&sized_agent_id)
        .cloned()
        .expect("the sized child owns a generated profile name");
    assert!(
        service
            .integration
            .model_profile_overrides()
            .runtime_generated_profiles
            .contains(&generated_name),
        "registering a runtime-generated name must mark it"
    );
    service
        .integration
        .model_profile_overrides_mut()
        .agent_profiles
        .insert("agent-%1".to_string(), generated_name.clone());

    let (inherited_agent_id, inherited_pane_id, _turn_id) =
        spawn_child_without_explicit_sizing(&mut service, &primary);
    assert_eq!(
        service
            .integration
            .model_profile_overrides()
            .agent_profiles
            .get(&inherited_agent_id)
            .map(String::as_str),
        Some(generated_name.as_str()),
        "the child must inherit the parent's generated name"
    );
    let (captured_name, captured_selection) =
        persisted_model_identity(&service, &transcript_store, &inherited_pane_id);
    assert_eq!(captured_name, generated_name);
    let captured_selection =
        captured_selection.expect("an inherited runtime-generated name must keep its selection");
    assert!(
        !captured_selection.model.is_empty(),
        "the captured selection must carry the effective model"
    );

    // Both halves are now exercised together: dropping every in-memory identity
    // fact must not change the identity a restart restores.
    let selection = captured_selection;
    {
        let overrides = service.integration.model_profile_overrides_mut();
        overrides.agent_profiles.clear();
        overrides.subagent_profiles.clear();
        overrides.runtime_generated_profiles.clear();
    }
    service
        .integration
        .provider_registry_mut()
        .profile_definitions
        .remove(&captured_name);
    service
        .integration
        .provider_registry_mut()
        .profiles
        .remove(&captured_name);

    service.restore_agent_model_profile_identity(
        &inherited_pane_id,
        &captured_name,
        Some(&selection),
    );
    let (effective_name, effective) = service
        .active_model_profile_for_pane(&inherited_pane_id, &inherited_agent_id, None)
        .unwrap();
    assert_eq!(effective_name, captured_name);
    assert_eq!(effective.model, selection.model);
    assert_eq!(
        effective.reasoning_profile.as_deref(),
        selection.reasoning_profile.as_deref()
    );
    assert!(
        service
            .integration
            .model_profile_overrides()
            .runtime_generated_profiles
            .contains(&captured_name),
        "the restored name must be marked as runtime-generated again"
    );
}

/// Verifies the durable identity captures the effective materialized profile.
///
/// Materialization merges provider-catalog model metadata into the effective
/// profile, and the model-catalog cache is populated only by the refresh command
/// paths, so a fresh daemon restores without it. Capturing the raw generated
/// definition therefore let a restore re-materialize a different option set - and
/// a different derived name - while still reporting success, silently changing
/// the model identity the child was created with.
#[test]
fn runtime_sized_child_identity_captures_catalog_materialized_options() {
    let transcript_store = crate::storage::transcript::AgentTranscriptStore::new(temp_root(
        "runtime-catalog-materialized-identity",
    ));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing-catalog".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();
    // Seed a live catalog whose model metadata materialization folds into the
    // effective profile but which the configured definition does not carry. The
    // seeded option exists only in the catalog, so the capture must take it from
    // materialization rather than from the definition.
    service.cache_provider_model_catalog_with_options_for_tests(
        "deepseek",
        vec![(
            "deepseek-v4-max".to_string(),
            std::collections::BTreeMap::from([(
                "captured_variant".to_string(),
                "effective".to_string(),
            )]),
        )],
        vec!["low".to_string(), "high".to_string()],
    );
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let (child_agent_id, child_pane_id, _turn_id) =
        spawn_explicitly_sized_child(&mut service, &primary, "large", "high");
    let (effective_name, effective) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert!(
        effective
            .provider_options
            .contains_key("model_reasoning_levels")
            || effective
                .provider_options
                .contains_key("model_capabilities"),
        "the seeded catalog must reach the effective profile: {effective:?}"
    );
    assert_eq!(
        effective
            .provider_options
            .get("captured_variant")
            .map(String::as_str),
        Some("effective"),
        "the seeded catalog option must reach the effective profile: {effective:?}"
    );

    let (captured_name, captured_selection) =
        persisted_model_identity(&service, &transcript_store, &child_pane_id);
    assert_eq!(captured_name, effective_name);
    let selection = captured_selection.expect("a generated identity carries its selection");
    assert_eq!(
        selection.provider_options, effective.provider_options,
        "the capture must describe the effective profile, not the raw definition"
    );

    // A restart drops the catalog cache and every in-memory identity fact.
    service
        .remove_cached_provider_model_catalog("deepseek")
        .unwrap();
    {
        let overrides = service.integration.model_profile_overrides_mut();
        overrides.agent_profiles.clear();
        overrides.subagent_profiles.clear();
        overrides.runtime_generated_profiles.clear();
    }
    service
        .integration
        .provider_registry_mut()
        .profile_definitions
        .remove(&captured_name);
    service
        .integration
        .provider_registry_mut()
        .profiles
        .remove(&captured_name);

    service.restore_agent_model_profile_identity(&child_pane_id, &captured_name, Some(&selection));
    let (restored_name, restored) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_eq!(
        restored_name, captured_name,
        "a restore must reproduce the captured identity name"
    );
    assert_eq!(
        restored.provider_options, effective.provider_options,
        "a restore must reproduce the effective profile options"
    );
}

/// Verifies an option-only identity drift is reported, not installed silently.
///
/// The derived name depends on which names are free rather than on the identity
/// alone, so a name comparison cannot catch a restored profile whose options
/// changed. A restore-side catalog that contributes an extra option is exactly
/// that case: the captured name still derives, but the profile no longer matches
/// what the child ran.
#[test]
fn runtime_restore_reports_option_only_identity_drift() {
    let transcript_store = crate::storage::transcript::AgentTranscriptStore::new(temp_root(
        "runtime-option-only-identity-drift",
    ));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing-drift".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "deepseek",
        vec![mez_agent::ProviderModelInfo {
            id: "deepseek-v4-max".to_string(),
            display_name: None,
            reasoning_levels: Some(vec!["low".to_string(), "high".to_string()]),
            context_window_tokens: None,
            max_input_tokens: None,
            max_output_tokens: None,
            capabilities: Some(vec!["tool_use".to_string()]),
        }],
        vec!["low".to_string(), "high".to_string()],
    );
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let (child_agent_id, child_pane_id, _turn_id) =
        spawn_explicitly_sized_child(&mut service, &primary, "large", "high");
    let (captured_name, captured_selection) =
        persisted_model_identity(&service, &transcript_store, &child_pane_id);
    let selection = captured_selection.expect("a generated identity carries its selection");

    // The restart happens after a catalog refresh that reports a metadata key the
    // child's capture does not carry at all, so re-materialization folds in an
    // option the child never ran with. (A changed *value* of a captured option
    // cannot drift: the captured options extend last during materialization.)
    service.cache_provider_model_catalog_for_tests(
        "deepseek",
        vec![mez_agent::ProviderModelInfo {
            id: "deepseek-v4-max".to_string(),
            display_name: None,
            reasoning_levels: Some(vec!["low".to_string(), "high".to_string()]),
            context_window_tokens: Some(128_000),
            max_input_tokens: None,
            max_output_tokens: None,
            capabilities: Some(vec!["tool_use".to_string()]),
        }],
        vec!["low".to_string(), "high".to_string()],
    );
    {
        let overrides = service.integration.model_profile_overrides_mut();
        overrides.agent_profiles.clear();
        overrides.subagent_profiles.clear();
        overrides.runtime_generated_profiles.clear();
    }
    service
        .integration
        .provider_registry_mut()
        .profile_definitions
        .remove(&captured_name);
    service
        .integration
        .provider_registry_mut()
        .profiles
        .remove(&captured_name);

    service.restore_agent_model_profile_identity(&child_pane_id, &captured_name, Some(&selection));
    assert_eq!(
        service
            .integration
            .model_profile_overrides()
            .agent_profiles
            .get(&child_agent_id)
            .map(String::as_str),
        Some(captured_name.as_str()),
        "the captured name is authoritative when it is free"
    );
    let degradations = service
        .event_log()
        .expect("the runtime test service owns an event log")
        .replay_for(&crate::protocol::event::EventAudience::AllPrimaries)
        .into_iter()
        .filter(|event| {
            event.kind == crate::runtime::EventKind::AgentStatus
                && event.payload.contains("\"model_profile\":\"degraded\"")
        })
        .map(|event| event.payload.clone())
        .collect::<Vec<_>>();
    assert!(
        degradations
            .iter()
            .any(|payload| payload.contains("provider_options")),
        "an option-only drift must name the differing field: {degradations:?}"
    );
}

/// Verifies a name configuration later defines loses its generated marker.
///
/// The marker asserts that this process generated the name. Once configuration
/// owns that name the marker must go, or a capture inherited from the configured
/// profile would later re-materialize silently instead of taking the
/// configuration-authoritative path that reports the loss.
#[test]
fn runtime_config_apply_clears_generated_marker_for_configured_names() {
    let transcript_store = crate::storage::transcript::AgentTranscriptStore::new(temp_root(
        "runtime-configured-name-marker",
    ));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing-marker".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let (child_agent_id, _child_pane_id, _turn_id) =
        spawn_explicitly_sized_child(&mut service, &primary, "large", "high");
    let generated_name = service
        .integration
        .model_profile_overrides()
        .agent_profiles
        .get(&child_agent_id)
        .cloned()
        .expect("the sized child owns a generated profile name");
    assert!(
        service
            .integration
            .model_profile_overrides()
            .runtime_generated_profiles
            .contains(&generated_name),
        "the generated name must be marked"
    );

    // A config apply that changes provider metadata without defining the name must
    // keep the marker: the reload path rebases the preserved generated definition
    // into the registry, and a rebased generated name is not configuration-owned.
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing-marker".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "{EXPLICIT_SUBAGENT_SIZING_CONFIG}\n[providers.deepseek.models.deepseek-v4-mini]\nid = \"deepseek-v4-mini\"\nreasoning_levels = [\"low\", \"high\"]\n"
            ),
        }])
        .unwrap();
    assert!(
        service
            .integration
            .model_profile_overrides()
            .runtime_generated_profiles
            .contains(&generated_name),
        "an apply that does not define the name must keep its runtime-generated marker"
    );

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing-marker".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "{EXPLICIT_SUBAGENT_SIZING_CONFIG}[model_profiles.\"{generated_name}\"]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-max\"\nreasoning_profile = \"high\"\n"
            ),
        }])
        .unwrap();
    assert!(
        !service
            .integration
            .model_profile_overrides()
            .runtime_generated_profiles
            .contains(&generated_name),
        "a configuration-owned name must not keep a runtime-generated marker"
    );
}

/// Verifies restored identities re-install the durable child profile, and that an
/// unresolvable one degrades to the documented fallback instead of silently
/// changing models.
#[test]
fn runtime_agent_model_identity_restore_reinstalls_or_degrades() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing-restore".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();

    service.restore_agent_model_profile_identity("%7", "deepseek-small", None);
    assert_eq!(
        service
            .integration
            .model_profile_overrides()
            .agent_profiles
            .get("agent-%7")
            .map(String::as_str),
        Some("deepseek-small"),
        "a name that still resolves must be re-installed"
    );

    let selection = crate::storage::transcript::AgentModelProfileSelection {
        provider: "missing-provider".to_string(),
        model: "missing-model".to_string(),
        reasoning_profile: Some("high".to_string()),
        latency_preference: None,
        provider_options: std::collections::BTreeMap::new(),
    };
    service.restore_agent_model_profile_identity(
        "%8",
        "generated-missing-profile",
        Some(&selection),
    );
    assert!(
        !service
            .integration
            .model_profile_overrides()
            .agent_profiles
            .contains_key("agent-%8"),
        "an unresolvable selection must not install an override"
    );
    service.restore_agent_model_profile_identity("%9", "generated-missing-profile", None);
    assert!(
        !service
            .integration
            .model_profile_overrides()
            .agent_profiles
            .contains_key("agent-%9"),
        "a missing name without a selection must not install an override"
    );

    let restorable = crate::storage::transcript::AgentModelProfileSelection {
        provider: "deepseek".to_string(),
        model: "deepseek-v4-flash".to_string(),
        reasoning_profile: Some("low".to_string()),
        latency_preference: None,
        provider_options: std::collections::BTreeMap::new(),
    };
    service.restore_agent_model_profile_identity(
        "%10",
        "runtime-generated:restored",
        Some(&restorable),
    );
    let restored_name = service
        .integration
        .model_profile_overrides()
        .agent_profiles
        .get("agent-%10")
        .cloned()
        .expect("a resolvable selection must be re-materialized and installed");
    let restored_profile = service
        .provider_registry()
        .profile(&restored_name)
        .expect("the re-materialized profile must resolve");
    assert_eq!(restored_profile.model, "deepseek-v4-flash");
    assert_eq!(restored_profile.reasoning_profile.as_deref(), Some("low"));

    let degraded_payloads = service
        .event_log()
        .expect("the runtime test service owns an event log")
        .replay_for(&crate::protocol::event::EventAudience::AllPrimaries)
        .into_iter()
        .filter(|event| {
            event.kind == crate::runtime::EventKind::AgentStatus
                && event.payload.contains("\"model_profile\":\"degraded\"")
        })
        .map(|event| event.payload.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        degraded_payloads.len(),
        3,
        "each degraded restore must report once, including the identity drift"
    );
    assert!(
        degraded_payloads.iter().any(|payload| payload
            .contains("\"profile\":\"generated-missing-profile\"")
            && payload.contains("profile no longer resolves")),
        "a missing name must report its profile and reason: {degraded_payloads:?}"
    );
    assert!(
        degraded_payloads
            .iter()
            .any(|payload| payload.contains("re-materialization failed")),
        "a failed re-materialization must report its reason: {degraded_payloads:?}"
    );
    assert!(
        degraded_payloads
            .iter()
            .any(|payload| payload.contains("re-materialized identity differs")),
        "a re-materialization that cannot reproduce the captured name must report the drift: \
         {degraded_payloads:?}"
    );
}

/// Verifies a durable child profile pins the requested reasoning level even
/// when it differs from the configured target profile's own reasoning level.
///
/// The generated profile name embeds the reasoning level, so two children that
/// request different efforts for the same size must resolve distinct profiles
/// with distinct reasoning levels instead of collapsing onto one identity.
#[test]
fn runtime_spawn_explicit_reasoning_pins_requested_effort() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-subagent-sizing-reasoning".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"gpt-default\"\nshell_mode = \"native\"\n\n[agents.auto_sizing]\nrouter_model_profile = \"gpt-router\"\nsmall_model_profile = \"gpt-small\"\nmedium_model_profile = \"gpt-medium\"\nlarge_model_profile = \"gpt-large\"\nallowed_reasoning_efforts = [\"low\", \"high\"]\n\n[permissions]\napproval_policy = \"ask\"\nsandbox = \"policy-only\"\n\n[providers.openai]\nkind = \"openai\"\napi = \"openai-responses\"\ndefault_model = \"gpt-5.6-terra\"\n\n[providers.openai.models.gpt-5-6-terra]\nid = \"gpt-5.6-terra\"\nreasoning_levels = [\"low\", \"medium\", \"high\", \"xhigh\"]\n\n[providers.openai.models.gpt-5-6-sol]\nid = \"gpt-5.6-sol\"\nreasoning_levels = [\"low\", \"medium\", \"high\", \"xhigh\"]\n\n[providers.openai.models.gpt-5-6-luna]\nid = \"gpt-5.6-luna\"\nreasoning_levels = [\"low\", \"medium\", \"high\", \"xhigh\"]\n\n[model_profiles.gpt-default]\nprovider = \"openai\"\nmodel = \"gpt-5.6-terra\"\nreasoning_profile = \"high\"\n\n[model_profiles.gpt-router]\nprovider = \"openai\"\nmodel = \"gpt-5.6-luna\"\nreasoning_profile = \"low\"\n\n[model_profiles.gpt-small]\nprovider = \"openai\"\nmodel = \"gpt-5.6-luna\"\nreasoning_profile = \"low\"\n\n[model_profiles.gpt-medium]\nprovider = \"openai\"\nmodel = \"gpt-5.6-terra\"\nreasoning_profile = \"medium\"\n\n[model_profiles.gpt-large]\nprovider = \"openai\"\nmodel = \"gpt-5.6-sol\"\nreasoning_profile = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let (high_agent_id, high_pane_id, high_turn_id) =
        spawn_explicitly_sized_child(&mut service, &primary, "large", "high");
    let (low_agent_id, low_pane_id, low_turn_id) =
        spawn_explicitly_sized_child(&mut service, &primary, "large", "low");

    let high_turn_profile = service
        .agent_turn_model_profile(&high_turn_id)
        .expect("high-reasoning child turn profile should exist")
        .clone();
    let low_turn_profile = service
        .agent_turn_model_profile(&low_turn_id)
        .expect("low-reasoning child turn profile should exist")
        .clone();
    assert_eq!(high_turn_profile.model, "gpt-5.6-sol");
    assert_eq!(high_turn_profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(low_turn_profile.model, "gpt-5.6-sol");
    assert_eq!(low_turn_profile.reasoning_profile.as_deref(), Some("low"));

    let (high_profile_name, high_profile) = service
        .active_model_profile_for_pane(&high_pane_id, &high_agent_id, None)
        .unwrap();
    let (low_profile_name, low_profile) = service
        .active_model_profile_for_pane(&low_pane_id, &low_agent_id, None)
        .unwrap();
    assert_eq!(high_profile.model, "gpt-5.6-sol");
    assert_eq!(high_profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(low_profile.model, "gpt-5.6-sol");
    assert_eq!(low_profile.reasoning_profile.as_deref(), Some("low"));
    assert_ne!(
        high_profile_name, low_profile_name,
        "different requested reasoning levels must not collapse onto one child identity"
    );
}

/// Verifies a subagent-scoped model-profile override governs a child's turn
/// resolution and that clearing it restores the child's durable identity.
///
/// `/model --scope subagent` stores overrides under the child's agent id, so
/// every turn path that resolves that child must consult the scope instead of
/// silently resolving the agent-, pane-, or default-scoped profile.
#[test]
fn runtime_subagent_scope_override_governs_child_turn_resolution() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "subagent-scope-resolution".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let (child_agent_id, child_pane_id, _turn_id) =
        spawn_explicitly_sized_child(&mut service, &primary, "large", "high");
    let (_, durable) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_eq!(durable.model, "deepseek-v4-max");
    assert_eq!(durable.reasoning_profile.as_deref(), Some("high"));

    service
        .integration
        .model_profile_overrides_mut()
        .subagent_profiles
        .insert(child_agent_id.clone(), "deepseek-small".to_string());
    let (scoped_name, scoped) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_eq!(scoped_name, "deepseek-small");
    assert_eq!(scoped.model, "deepseek-v4-flash");
    assert_eq!(scoped.reasoning_profile.as_deref(), Some("low"));

    service.stop_agent_turn_for_pane(&child_pane_id).unwrap();
    let turn = service
        .start_agent_prompt_turn(&child_pane_id, "review under the scoped profile")
        .unwrap();
    let turn_profile = service
        .agent_turn_model_profile(&turn.turn_id)
        .expect("scoped child turn profile should exist")
        .clone();
    assert_eq!(turn_profile.model, "deepseek-v4-flash");
    assert_eq!(turn_profile.reasoning_profile.as_deref(), Some("low"));

    service
        .integration
        .model_profile_overrides_mut()
        .subagent_profiles
        .remove(&child_agent_id);
    let (_, restored) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_eq!(restored.model, "deepseek-v4-max");
    assert_eq!(restored.reasoning_profile.as_deref(), Some("high"));
}

/// Verifies a nested child inherits its parent's effective model identity,
/// including a subagent-scoped override on that parent.
///
/// A child of an explicitly sized child must inherit the profile the parent
/// actually resolves — its subagent-scoped override — rather than the parent's
/// agent-scoped spawn identity, matching resolver precedence.
#[test]
fn runtime_nested_child_inherits_parent_effective_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "nested-inheritance".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let (parent_agent_id, parent_pane_id, _) =
        spawn_explicitly_sized_child(&mut service, &primary, "large", "high");
    let (parent_profile_name, parent_profile) = service
        .active_model_profile_for_pane(&parent_pane_id, &parent_agent_id, None)
        .unwrap();
    assert_eq!(parent_profile.model, "deepseek-v4-max");
    assert_eq!(
        service.inherited_model_profile_for_child_agent(&parent_agent_id),
        Some(parent_profile_name),
        "a child inherits the parent's own model identity"
    );
    service
        .integration
        .model_profile_overrides_mut()
        .subagent_profiles
        .insert(parent_agent_id.clone(), "deepseek-small".to_string());
    assert_eq!(
        service.inherited_model_profile_for_child_agent(&parent_agent_id),
        Some("deepseek-small".to_string()),
        "the parent's subagent-scoped override must be the inherited identity"
    );

    let nested = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: parent_agent_id.clone(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "inspect the nested inheritance".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: false,
            },
        )
        .unwrap();
    let nested = serde_json::from_str::<serde_json::Value>(&nested).unwrap();
    let nested_agent_id = nested["agent"]["id"].as_str().unwrap().to_string();
    let nested_pane_id = nested["agent"]["pane_id"].as_str().unwrap().to_string();
    let (nested_profile_name, nested_profile) = service
        .active_model_profile_for_pane(&nested_pane_id, &nested_agent_id, None)
        .unwrap();
    assert_eq!(nested_profile_name, "deepseek-small");
    assert_eq!(nested_profile.model, "deepseek-v4-flash");
    assert_eq!(nested_profile.reasoning_profile.as_deref(), Some("low"));
}

/// Verifies a promptless persistent spawn accepts an explicit size/reasoning
/// pair and applies it to the idle child before any turn exists.
///
/// An idle persistent child starts with its first peer-message turn, so the
/// spawn response must report the requested pair and the durable child profile,
/// and that first message-triggered turn must already run at the requested
/// model and reasoning level.
#[test]
fn runtime_idle_persistent_spawn_accepts_explicit_size_pair() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "runtime-idle-persistent-sizing",
    )));
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "idle-persistent-sizing".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: EXPLICIT_SUBAGENT_SIZING_CONFIG.to_string(),
        }])
        .unwrap();
    let _primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let parent_conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();

    let spawned = service
        .spawn_runtime_persistent_subagent_session_owned(
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: Some("large".to_string()),
                initial_reasoning_effort: Some("high".to_string()),
                task_prompt: String::new(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: false,
            },
            &parent_conversation_id,
            "Review repository issues on request",
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let child_agent_id = spawned["agent"]["id"].as_str().unwrap().to_string();
    let child_pane_id = spawned["agent"]["pane_id"].as_str().unwrap().to_string();
    assert_eq!(
        spawned["agent"]["initial_model_size"].as_str(),
        Some("large")
    );
    assert_eq!(
        spawned["agent"]["initial_reasoning_effort"].as_str(),
        Some("high")
    );
    assert_eq!(
        spawned["agent"]["initial_model_profile"].as_str(),
        Some("deepseek-large")
    );
    assert!(spawned["turn"].is_null());

    let (child_profile_name, child_profile) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_eq!(child_profile.model, "deepseek-v4-max");
    assert_eq!(child_profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(
        spawned["agent"]["model_profile"].as_str(),
        Some(child_profile_name.as_str())
    );

    // The idle child's first peer-message turn must already use the pair.
    let now_ms = crate::runtime::current_unix_millis();
    let parent_identity = service
        .ensure_runtime_message_identity("agent-%1", None, "agent", &[], now_ms)
        .unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &parent_identity.agent_id,
            mez_agent::messaging::Envelope {
                protocol: "mmp/1",
                id: "idle-persistent-review".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: parent_identity.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    mez_core::ids::AgentId::opaque(child_agent_id.clone()).unwrap(),
                ),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "review the pending issue".to_string(),
                extension_fields: Vec::new(),
            },
            mez_agent::messaging::MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    let child_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == child_agent_id)
        .cloned()
        .expect("idle persistent child should start one peer-message turn");
    let child_turn_profile = service
        .agent_turn_model_profile(&child_turn.turn_id)
        .expect("peer-message child turn profile should exist")
        .clone();
    assert_eq!(child_turn_profile.model, "deepseek-v4-max");
    assert_eq!(
        child_turn_profile.reasoning_profile.as_deref(),
        Some("high")
    );
}

/// Verifies a generated child profile keeps the supplied reasoning level even
/// when the base profile definition configures no reasoning level.
///
/// An explicit spawn selection always supplies a reasoning level, so the pin
/// must not depend on the base definition carrying one.
#[test]
fn runtime_generated_child_profile_pins_reasoning_without_base_reasoning() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "bare-reasoning-profile".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"bare-default\"\n\n[providers.openai]\nkind = \"openai\"\napi = \"openai-responses\"\ndefault_model = \"gpt-5.6-sol\"\n\n[model_profiles.bare-default]\nprovider = \"openai\"\nmodel = \"gpt-5.6-sol\"\n"
                .to_string(),
        }])
        .unwrap();

    let mut profile = service
        .provider_registry()
        .resolve_profile("bare-default")
        .expect("configured bare profile should resolve");
    assert!(profile.reasoning_profile.is_none());
    profile.reasoning_profile = Some("low".to_string());
    let generated = service
        .insert_runtime_generated_model_profile("bare-default", profile)
        .expect("generated child profile should register");
    let resolved = service
        .provider_registry()
        .resolve_profile(&generated)
        .expect("generated child profile should resolve");
    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.model, "gpt-5.6-sol");
    assert_eq!(resolved.reasoning_profile.as_deref(), Some("low"));
}

/// Verifies a spawn without an explicit size/reasoning pair keeps the existing
/// role-then-parent profile inheritance for every child turn.
///
/// Durable child sizing must not leak into ordinary spawns: a child that omits
/// the pair continues to resolve the inherited parent profile (or the global
/// default profile when the parent resolves the default) instead of acquiring a
/// generated profile of its own.
#[test]
fn runtime_spawn_without_explicit_size_keeps_inherited_child_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "inherited-subagent-profile".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"deepseek\"\ndefault_model_profile = \"deepseek-default\"\nshell_mode = \"native\"\n\n[permissions]\napproval_policy = \"ask\"\nsandbox = \"policy-only\"\n\n[providers.deepseek]\nkind = \"deepseek\"\ndefault_model = \"deepseek-v4-pro\"\n\n[providers.deepseek.models.deepseek-v4-pro]\nid = \"deepseek-v4-pro\"\nreasoning_levels = [\"low\", \"high\", \"max\"]\n\n[model_profiles.deepseek-default]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_profile = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let (parent_profile_name, parent_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(parent_profile_name, "deepseek-default");

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "inspect inherited child sizing".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let child_agent_id = spawned["agent"]["id"].as_str().unwrap().to_string();
    let child_pane_id = spawned["agent"]["pane_id"].as_str().unwrap().to_string();
    assert_eq!(
        spawned["agent"]["initial_model_size"],
        serde_json::Value::Null
    );
    let child_turn_id = spawned["turn"]["id"].as_str().unwrap().to_string();
    let child_turn_profile = service
        .agent_turn_model_profile(&child_turn_id)
        .expect("child initial turn profile should exist")
        .clone();
    assert_eq!(child_turn_profile.model, parent_profile.model);
    assert_eq!(
        child_turn_profile.reasoning_profile,
        parent_profile.reasoning_profile
    );
    let (child_profile_name, child_profile) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_eq!(child_profile_name, parent_profile_name);
    assert_eq!(child_profile.model, parent_profile.model);
}
