/// Verifies that an explicitly empty provider model list still falls back to
/// the provider's built-in code-defined catalog. This protects minimal configs
/// that clear `providers.openai.models` from losing all local model selection
/// when live provider catalog access is unavailable.
#[tokio::test]
async fn runtime_agent_shell_model_list_uses_code_defaults_when_config_models_empty() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = []\ndefault_model = \"gpt-5.5\"\n"
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

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    for model in [
        "★ gpt-5.5",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.3-codex",
        "gpt-5.3-codex-spark",
        "gpt-5.2",
    ] {
        assert!(model_list.contains(model), "{model_list}");
    }
    assert!(!model_list.contains("codex-mini-latest"), "{model_list}");
    assert!(model_list.contains("| config |"), "{model_list}");
}

/// Verifies that live provider model catalogs take precedence over configured
/// fallback models. The configured `providers.openai.models` list should keep
/// the command useful when the provider cannot be reached, but it must not
/// override a successfully populated provider catalog.
#[tokio::test]
async fn runtime_agent_shell_model_list_uses_provider_catalog_over_configured_models() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"configured-only\"]\ndefault_model = \"configured-only\"\n"
                .to_string(),
        }])
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![crate::agent::ProviderModelInfo {
            id: "provider-only".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string(), "high".to_string()],
        }],
        vec!["low".to_string(), "high".to_string()],
    );
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    assert!(
        model_list.contains("| openai | provider-only |"),
        "{model_list}"
    );
    assert!(!model_list.contains("configured-only"), "{model_list}");
    assert!(model_list.contains("| provider |"), "{model_list}");
}

/// Verifies that ChatGPT browser/device credentials do not trigger a fabricated
/// Codex model-catalog HTTP request. The runtime should skip that unsupported
/// live catalog path and fall back to configured provider models without
/// surfacing an OpenAI 400-class provider error in the agent prompt.
#[tokio::test]
async fn runtime_agent_shell_model_list_skips_browser_auth_catalog_request() {
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
    let root = temp_root("runtime-model-list-chatgpt");
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_provider_credential(
            "default",
            crate::auth::OpenAiProviderCredential {
                api_key: "chatgpt-access-token".to_string(),
                refresh_token: Some("refresh-token".to_string()),
                account_id: Some("acct_123".to_string()),
                organization_id: None,
                token_expires_at: Some("12345".to_string()),
            },
            &credential_store,
        )
        .unwrap();
    service.set_auth_store(auth_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    assert!(model_list.contains(r#""kind":"display""#), "{model_list}");
    assert!(
        model_list.contains("**Provider catalog unavailable:** `browser-auth-catalog-unsupported`"),
        "{model_list}"
    );
    assert!(!model_list.contains("status 400"), "{model_list}");
    assert!(!model_list.contains("Models API returned"), "{model_list}");
    assert!(
        model_list.contains("| openai | ★ gpt-5.5 |"),
        "{model_list}"
    );
    assert!(model_list.contains("| openai | gpt-5.4 |"), "{model_list}");
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
    assert_eq!(
        pending[0]
            .model_profile
            .provider_options
            .get("reasoning_effort")
            .map(String::as_str),
        Some("high")
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
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .extend([
            crate::agent::ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                label: "old minified assistant context for pane %1".to_string(),
                content: format!("minified-context:{}", "x".repeat(200 * 1024)),
            },
            crate::agent::ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                label: "transcript assistant entry 2 for pane %1".to_string(),
                content: "Recommended next tasks:\n1. Document the model picker.\n2. Clean up stale quota UI.\n3. Implement multi-file runtime auto-sizing.".to_string(),
            },
            crate::agent::ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                label: "previous tool output for pane %1".to_string(),
                content: "tool-only output should not reach the router".to_string(),
            },
            crate::agent::ContextBlock {
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
        crate::agent::ModelInteractionKind::AutoSizing
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
        message.role == crate::agent::ModelMessageRole::User
            && message.source == ContextSourceKind::UserInstruction
            && message.content.contains("implement this")
    }));
    assert!(requests[0].messages.iter().any(|message| {
        message.role == crate::agent::ModelMessageRole::Assistant
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
        crate::agent::ModelInteractionKind::CapabilityDecision
    );
    assert_eq!(requests[1].model, "gpt-5.5");
    assert_eq!(requests[1].reasoning_effort.as_deref(), Some("high"));
    assert_eq!(executions[0].request.model, "gpt-5.5");
    assert_eq!(
        executions[0].request.reasoning_effort.as_deref(),
        Some("high")
    );
    let router_usage_key = crate::agent::ModelTokenUsageKey::new("runtime-batch", "gpt-router");
    assert_eq!(
        executions[0]
            .routing_token_usage_by_model
            .get(&router_usage_key)
            .copied(),
        Some(crate::agent::ModelTokenUsage {
            input_tokens: 90,
            output_tokens: 10,
            reasoning_tokens: 3,
            cached_input_tokens: Some(30),
        })
    );
    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"auto-sizing-token-status","method":"agent/shell/command","params":{"idempotency_key":"auto-sizing-token-status","input":"/status"}}"#,
        &primary,
    );
    assert!(
        status.contains("| Provider tokens | 2 models; see Provider Token Usage |"),
        "{status}"
    );
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

/// Builds the synthetic model response used by compaction completion tests.
fn runtime_test_compaction_response(summary: &str) -> crate::agent::ModelResponse {
    crate::agent::ModelResponse {
        provider: "test".to_string(),
        model: "gpt-compact-test".to_string(),
        raw_text: summary.to_string(),
        usage: Default::default(),
        quota_usage: Vec::new(),
        action_batch: None,
        provider_transcript_events: Vec::new(),
    }
}

/// Applies a queued `/compact` provider result without leaving the actor path.
fn complete_runtime_test_compaction(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    summary: &str,
) {
    let task = service
        .pending_agent_compaction_tasks
        .remove(pane_id)
        .expect("queued compaction task");
    service
        .claimed_agent_compaction_tasks
        .insert(pane_id.to_string(), task);
    assert!(
        service
            .apply_agent_compaction_completed_event(
                pane_id,
                runtime_test_compaction_response(summary)
            )
            .unwrap()
    );
}

/// Verifies that `/compact` converts the active conversation transcript into a
/// bounded pane-scoped memory record, retains a raw recent transcript tail, and
/// feeds both into the next prompt context. This keeps context pressure
/// handling from silently dropping recent exact referents.
#[test]
fn runtime_agent_shell_compact_summarizes_transcript_into_memory_context() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-test"]
default_model = "gpt-compact-test"
[model_profiles.compact-test]
provider = "openai"
model = "gpt-compact-test"
context_window_tokens = 4500
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact"));
    for sequence in 1..=12 {
        let (role, content) = match sequence {
            1 => (
                crate::transcript::TranscriptRole::User,
                format!("summarize release plan {}", "summary-word ".repeat(28)),
            ),
            2 => (
                crate::transcript::TranscriptRole::Tool,
                format!(
                    "api_key sk-secret should be hidden {}",
                    "secret-word ".repeat(28)
                ),
            ),
            3 => (
                crate::transcript::TranscriptRole::Assistant,
                format!(
                    "release plan summary is ready {}",
                    "release-word ".repeat(28)
                ),
            ),
            _ if sequence % 2 == 0 => (
                crate::transcript::TranscriptRole::User,
                format!("filler user turn {sequence} {}", "user-word ".repeat(28)),
            ),
            _ => (
                crate::transcript::TranscriptRole::Assistant,
                format!(
                    "filler assistant turn {sequence} {}",
                    "assistant-word ".repeat(28)
                ),
            ),
        };
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: "as1".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 80).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as1", 12)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact","method":"agent/shell/command","params":{"idempotency_key":"compact","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains(r#""kind":"mutated""#), "{compact}");
    assert!(compact.contains(r#""command":"compact""#), "{compact}");
    assert!(compact.contains("state=queued"), "{compact}");
    assert!(
        compact.contains("previous_transcript_entries=12"),
        "{compact}"
    );
    assert!(compact.contains("summarized_entries=6"), "{compact}");
    assert!(compact.contains("source=model-compact"), "{compact}");
    assert!(!compact.contains("requires_runtime"), "{compact}");
    assert!(service.agent_compacting_panes.contains_key("%1"));
    assert!(service.pending_agent_compaction_tasks.contains_key("%1"));

    complete_runtime_test_compaction(&mut service, "%1", "summarize release plan\n[redacted]");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: compacting conversation summary"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent: compacted conversation summary"),
        "{pane_text}"
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        6
    );
    let compacted = service
        .memory_records()
        .into_iter()
        .find(|record| record.id == "compact-as1")
        .expect("compacted memory record");
    assert!(
        compacted.content.contains("summarize release plan"),
        "{}",
        compacted.content
    );
    assert!(
        compacted.content.contains("[redacted]"),
        "{}",
        compacted.content
    );
    assert!(
        !compacted.content.contains("sk-secret"),
        "{}",
        compacted.content
    );

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-prompt","method":"agent/shell/command","params":{"idempotency_key":"compact-prompt","input":"continue after compaction"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::Memory
            && block.label.contains("compact-as1")
            && block.content.contains("summarize release plan")
    }));
    assert!(context.blocks.iter().all(|block| {
        !matches!(
            block.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
        ) || !block.content.contains("release plan summary is ready")
    }));
    assert!(context.blocks.iter().all(|block| {
        !matches!(
            block.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
        ) || !block.content.contains("sk-secret")
    }));
    assert!(context.blocks.iter().all(|block| {
        !matches!(
            block.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
        ) || !block.content.contains("summarize release plan")
    }));
}

/// Verifies prompt submission does not run fallback context accounting before
/// appending prompt-derived state.
///
/// Provider responses and provider context-limit errors are the source of truth
/// for context-size handling, so prompt submission must start the turn even when
/// a local estimate would have crossed the model window.
#[test]
fn runtime_agent_prompt_does_not_preflight_compact_before_context_append() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-preflight-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-preflight-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-preflight-test"]
default_model = "gpt-compact-preflight-test"
[model_profiles.compact-preflight-test]
provider = "openai"
model = "gpt-compact-preflight-test"
context_window_tokens = 1024
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-preflight"));
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "as-preflight".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::Assistant,
            turn_id: "turn-previous".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!("large prior context {}", "context-pressure ".repeat(900)),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 80).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-preflight", 1)
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "continue with the next item")
        .unwrap();

    assert!(response.contains(r#""state":"running""#), "{response}");
    assert!(
        !response.contains(r#""kind":"requires_runtime""#),
        "{response}"
    );
    assert_eq!(service.agent_turn_ledger.turns().len(), 1);
    assert_eq!(
        transcript_store.prompt_history("as-preflight").unwrap(),
        vec!["continue with the next item".to_string()]
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("continue with the next item"),
        "{pane_text}"
    );
}

/// Verifies active-turn provider continuations do not run fallback context
/// accounting before request assembly.
///
/// Runtime-owned action results and steering can append context after the turn
/// has started. The continuation path should still send the exact assembled
/// request first and rely on provider context-limit recovery if the provider
/// rejects it.
#[test]
fn runtime_agent_turn_sends_active_context_before_provider_limit_feedback() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-active-turn-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "compact-active-turn-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.compact-active-turn-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 64000
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-active-turn-compact","input":"continue with gathered evidence"}}"#,
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
            label: "synthetic in-turn action result".to_string(),
            content: format!(
                "turn-context-pressure- {}",
                "context-pressure ".repeat(10_000)
            ),
        });
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
            provider_transcript_events: Vec::new(),
        },
        last_request: RefCell::new(None),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("compact-active-turn-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let request = provider.last_request.borrow().clone().unwrap();
    let request_text = request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request_text.contains("[synthetic in-turn action result]"),
        "{request_text}"
    );
    assert!(
        request_text.contains("turn-context-pressure-"),
        "{request_text}"
    );
    assert!(
        !request_text.contains("[context compacted]"),
        "{request_text}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("agent: compacted active turn context"),
        "{pane_text}"
    );
}

/// Verifies provider context-limit API errors trigger active-turn compaction
/// and retry before the turn is failed.
///
/// The proactive threshold path can miss provider-specific tokenization or
/// hidden request overhead. When the provider rejects the request anyway, the
/// runtime must compact the stored active-turn context before retrying so the
/// same oversized payload is not sent again.
#[test]
fn runtime_provider_context_limit_error_compacts_context_and_retries() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-context-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-context-limit-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-context-limit-test]
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-context-limit-recovery","input":"continue with the large observation"}}"#,
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
            label: "synthetic provider-rejected action result".to_string(),
            content: format!("provider-context-limit- {}", "cp ".repeat(10_000)),
        });
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeContextLimitThenSuccessProvider {
        requests: RefCell::new(Vec::new()),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("provider-context-limit-test")
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
        first_request_text.contains("provider-context-limit-"),
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
        second_request_text.contains("provider-context-limit-"),
        "{second_request_text}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider rejected context as too large; compacted active turn context"),
        "{pane_text}"
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

/// Verifies provider output-limit incomplete responses trigger compact retry
/// guidance and max-output escalation without compacting active-turn context.
///
/// Output exhaustion means the provider accepted the input but cut generation
/// off, so the recovery path should ask for a smaller complete response rather
/// than discarding context.
#[test]
fn runtime_provider_output_limit_error_guides_compact_retry_without_compaction() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-output-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-output-limit-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-output-limit-test]
provider = "runtime-batch"
model = "test"
max_output_tokens = 4096
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
        r#"{"jsonrpc":"2.0","id":"agent-output-limit-recovery","method":"agent/shell/command","params":{"idempotency_key":"agent-output-limit-recovery","input":"continue with the current implementation"}}"#,
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
            label: "synthetic retained action result".to_string(),
            content: "output-limit-retained-context".to_string(),
        });
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeOutputLimitThenSuccessProvider {
        requests: RefCell::new(Vec::new()),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("provider-output-limit-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].max_output_tokens, Some(4096));
    assert_eq!(requests[1].max_output_tokens, Some(16_384));
    let second_request_text = requests[1]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("output-limit-retained-context"),
        "{second_request_text}"
    );
    assert!(
        second_request_text.contains("[ephemeral provider output-limit retry]"),
        "{second_request_text}"
    );
    assert!(
        second_request_text.contains("one complete compact MAAP batch"),
        "{second_request_text}"
    );
    assert!(
        !second_request_text.contains("[context compacted]"),
        "{second_request_text}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider response hit output limit; retrying compactly"),
        "{pane_text}"
    );
}

/// Verifies routing context-limit recovery budgets against the smallest
/// possible main-provider target before a router decision has been stored.
///
/// A turn may start with a large default profile while the router is still able
/// to choose a smaller target profile for the first normal request. Provider
/// context-limit recovery must therefore compact against the minimum target
/// window until the synthesized per-turn profile exists.
#[test]
fn runtime_routing_context_limit_recovery_uses_minimum_target_window() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "routing-context-limit-recovery".to_string(),
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
models = ["gpt-router", "gpt-default", "gpt-small", "gpt-medium", "gpt-large"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"
reasoning_profile = "medium"
context_window_tokens = 100000

[model_profiles.router]
provider = "runtime-batch"
model = "gpt-router"
reasoning_profile = "low"
context_window_tokens = 2000

[model_profiles.small]
provider = "runtime-batch"
model = "gpt-small"
reasoning_profile = "medium"
context_window_tokens = 40000

[model_profiles.medium]
provider = "runtime-batch"
model = "gpt-medium"
reasoning_profile = "medium"
context_window_tokens = 100000

[model_profiles.large]
provider = "runtime-batch"
model = "gpt-large"
reasoning_profile = "high"
context_window_tokens = 100000
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
        r#"{"jsonrpc":"2.0","id":"agent-auto-context-limit","method":"agent/shell/command","params":{"idempotency_key":"agent-auto-context-limit","input":"continue with the current findings"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let default_profile = service
        .provider_registry()
        .resolve_profile("default")
        .unwrap();
    service
        .agent_turn_model_profiles
        .insert("turn-1".to_string(), default_profile);
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "synthetic routing action result".to_string(),
            content: format!(
                "routing-context-pressure- {}",
                "context-pressure ".repeat(50_000)
            ),
        });
    let error = MezError::invalid_state(
        "OpenAI Responses API returned status 400: context length exceeded",
    )
    .with_provider_failure_json(
        r#"{"status_code":400,"error":{"message":"maximum context length exceeded","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
    );

    let recovered = service
        .recover_agent_provider_context_limit_failure(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            &error,
            1,
        )
        .unwrap();

    assert!(recovered);
    let stored_context = service
        .agent_turn_contexts
        .get("turn-1")
        .unwrap()
        .blocks
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(stored_context.contains("[context compacted]"));
    let stored_blocks = &service.agent_turn_contexts.get("turn-1").unwrap().blocks;
    assert!(stored_blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.label == "synthetic routing action result"
            && block.content.contains("routing-context-pressure-")
    }));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider rejected context as too large; compacted active turn context"),
        "{pane_text}"
    );
}

/// Verifies provider context receives only the active conversation compaction
/// memory automatically.
///
/// Generic session memory should not be injected into every provider request
/// once transcript replay and compaction summaries already represent the active
/// conversation.
#[test]
fn runtime_agent_context_injects_only_active_compact_memory() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as1", 0)
        .unwrap();
    service
        .upsert_session_memory(MemoryRecord {
            id: "runtime-note".to_string(),
            scope: crate::memory::MemoryScope::Session {
                session_id: service.session().id.to_string(),
            },
            created_at_unix_seconds: 1,
            updated_at_unix_seconds: 1,
            source: crate::memory::MemorySource::User,
            priority: 255,
            content: "generic memory should not be automatic context".to_string(),
            explicit_sensitive_consent: false,
        })
        .unwrap();
    service
        .upsert_session_memory(MemoryRecord {
            id: "compact-other".to_string(),
            scope: crate::memory::MemoryScope::Pane {
                session_id: service.session().id.to_string(),
                pane_id: "%1".to_string(),
            },
            created_at_unix_seconds: 2,
            updated_at_unix_seconds: 2,
            source: crate::memory::MemorySource::Agent,
            priority: 255,
            content: "other compaction should not leak".to_string(),
            explicit_sensitive_consent: false,
        })
        .unwrap();
    service
        .upsert_session_memory(MemoryRecord {
            id: "compact-as1".to_string(),
            scope: crate::memory::MemoryScope::Pane {
                session_id: service.session().id.to_string(),
                pane_id: "%1".to_string(),
            },
            created_at_unix_seconds: 3,
            updated_at_unix_seconds: 3,
            source: crate::memory::MemorySource::Agent,
            priority: 128,
            content: "active compact summary".to_string(),
            explicit_sensitive_consent: false,
        })
        .unwrap();

    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    let memory_blocks = context
        .blocks
        .iter()
        .filter(|block| block.source == ContextSourceKind::Memory)
        .collect::<Vec<_>>();

    assert_eq!(memory_blocks.len(), 2, "{memory_blocks:?}");
    assert!(
        memory_blocks
            .iter()
            .any(|block| block.label == "conversation compaction notice"
                && block.content.contains("Conversation compaction occurred")),
        "{memory_blocks:?}"
    );
    assert!(
        memory_blocks
            .iter()
            .any(|block| block.label.contains("compact-as1")
                && block.content.contains("active compact summary")),
        "{memory_blocks:?}"
    );
    assert!(
        context
            .blocks
            .iter()
            .all(|block| !block.content.contains("generic memory"))
    );
    assert!(
        context
            .blocks
            .iter()
            .all(|block| !block.content.contains("other compaction"))
    );
}

/// Verifies explicit `$skill` prompt syntax loads the selected skill into the
/// next turn context and appends trailing prompt text as skill-specific
/// semantic context. The raw prompt remains present so the user's latest input
/// is still the visible turn instruction.
#[test]
fn runtime_agent_context_explicit_skill_prompt_loads_skill_context() {
    let config_root = temp_root("runtime-skill-context");
    let skill_dir = config_root.join("skills/review");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Review workflow\n---\n\nCheck tests and risks.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(config_root);

    let context = service
        .agent_context_for_pane_prompt("%1", "$review focus src/lib.rs", 0)
        .unwrap();
    let skill_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill review")
        .expect("missing explicit skill context block");
    let prompt_block = context
        .blocks
        .iter()
        .find(|block| block.label == "user prompt")
        .expect("missing raw user prompt block");

    assert_eq!(skill_block.source, ContextSourceKind::UserInstruction);
    assert!(skill_block.content.contains("name: review"));
    assert!(skill_block.content.contains("Check tests and risks."));
    assert!(
        skill_block
            .content
            .contains("## Additional context\n\nfocus src/lib.rs")
    );
    assert_eq!(prompt_block.content, "$review focus src/lib.rs");
}

/// Verifies explicit `$create-skill` prompt syntax loads the built-in skill
/// authoring workflow even when no user or project skills have been installed.
/// This keeps the built-in workflow available as normal skill context instead
/// of requiring a separate command or bootstrap file.
#[test]
fn runtime_agent_context_builtin_create_skill_prompt_loads_builtin_context() {
    let mut service = test_runtime_service();

    let context = service
        .agent_context_for_pane_prompt(
            "%1",
            "$create-skill create a project skill for release notes",
            0,
        )
        .unwrap();
    let skill_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill create-skill")
        .expect("missing explicit built-in skill context block");

    assert_eq!(skill_block.source, ContextSourceKind::UserInstruction);
    assert!(skill_block.content.contains("Source: builtin"));
    assert!(skill_block.content.contains("name: create-skill"));
    assert!(skill_block.content.contains("Project scope:"));
    assert!(
        skill_block
            .content
            .contains("Invocation state: this skill is already loaded"),
        "{}",
        skill_block.content
    );
    assert!(
        skill_block
            .content
            .contains("## Additional context\n\ncreate a project skill for release notes")
    );
    let invocation_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill invocation create-skill")
        .expect("missing explicit skill invocation block");
    assert_eq!(invocation_block.source, ContextSourceKind::LocalMessage);
    assert!(
        invocation_block
            .content
            .contains("The selected skill context has already been loaded above"),
        "{}",
        invocation_block.content
    );
}

/// Verifies `$mez-config` includes live schema guidance and current config.
///
/// The config skill should not force the model to rediscover basic setting
/// names before making a config mutation. Its invocation context therefore
/// includes the annotated schema, concrete theme color slots, reset operation,
/// and the pane's current effective config snapshot.
#[test]
fn runtime_agent_context_builtin_mez_config_prompt_includes_current_config() {
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
        .agent_context_for_pane_prompt("%1", "$mez-config set the prompt color", 0)
        .unwrap();
    let skill_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill mez-config")
        .expect("missing explicit mez-config skill context block");

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

/// Verifies persisted skill payloads are not replayed into later model context.
///
/// This covers both newly compact skill-action transcripts and legacy
/// transcripts that may already contain an expanded `SKILL.md` body from an
/// earlier build. The next ordinary prompt should see the raw user request and
/// assistant/tool evidence, not stale skill workflow instructions.
#[test]
fn runtime_agent_context_omits_persisted_skill_payloads_from_replay() {
    let transcript_root = temp_root("runtime-skill-transcript-replay");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
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
    for (sequence, role, content) in [
        (
            1,
            crate::transcript::TranscriptRole::User,
            "# Skill: review\n\nSource: project\nPath: skills/review/SKILL.md\n\nInvocation state: this skill is already loaded for the current turn.\n\nReview workflow body.",
        ),
        (
            2,
            crate::transcript::TranscriptRole::Tool,
            "action_id=skill-1 action_type=call_skill status=Succeeded\ncontent:\n# Skill: review\n\nReview workflow body.",
        ),
        (
            3,
            crate::transcript::TranscriptRole::Tool,
            "action_id=catalog-1 action_type=request_skills status=Succeeded\ncontent:\nAvailable skills:\n- review (project) - Review workflow body.",
        ),
        (
            4,
            crate::transcript::TranscriptRole::User,
            "$review focus src/lib.rs",
        ),
        (
            5,
            crate::transcript::TranscriptRole::Assistant,
            "I reviewed the requested area.",
        ),
    ] {
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: conversation_id.clone(),
                sequence,
                created_at_unix_seconds: 100,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 5)
        .unwrap();

    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    let replayed = context
        .blocks
        .iter()
        .filter(|block| {
            matches!(
                block.source,
                ContextSourceKind::TranscriptUser
                    | ContextSourceKind::TranscriptAssistant
                    | ContextSourceKind::TranscriptTool
            )
        })
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    assert!(replayed.contains("$review focus src/lib.rs"), "{replayed}");
    assert!(
        replayed.contains("I reviewed the requested area."),
        "{replayed}"
    );
    assert!(!replayed.contains("# Skill:"), "{replayed}");
    assert!(!replayed.contains("Review workflow body"), "{replayed}");
    assert!(!replayed.contains("Available skills:"), "{replayed}");
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies explicit `$skill` prompts do not allow a model to loop by loading
/// the same skill again.
///
/// A `$create-skill ...` prompt has already loaded the built-in skill into the
/// turn context. If the model responds with `call_skill(create-skill)` instead
/// of requesting a concrete execution capability, the strict request surface
/// should reject the action before runtime skill execution can start another
/// successful continuation.
#[test]
fn runtime_explicit_skill_prompt_rejects_redundant_call_skill_loop() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "$create-skill create a review skill")
        .unwrap();
    service
        .pending_agent_provider_tasks
        .remove(&started.turn_id);
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "load create skill again".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "load skill authoring context".to_string(),
                thought: None,
                turn_id: started.turn_id.clone(),
                agent_id: started.agent_id.clone(),
                actions: vec![crate::agent::AgentAction {
                    id: "skill-loop".to_string(),
                    rationale: "load the create-skill workflow".to_string(),
                    payload: crate::agent::AgentActionPayload::CallSkill {
                        name: "create-skill".to_string(),
                        additional_context: Some("create a review skill".to_string()),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(
        !service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == started.turn_id)
    );
    assert!(
        execution
            .response
            .raw_text
            .contains("maap action type call_skill is not allowed"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution
            .request
            .allowed_actions
            .action_type_names()
            .contains(&"request_capability")
    );
    assert!(
        !execution
            .request
            .allowed_actions
            .action_type_names()
            .contains(&"call_skill")
    );
    assert!(execution.action_results.is_empty());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("maap_validation_error"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies explicit `$skill` prompts do not need an additional skill catalog
/// lookup before acting on the already-loaded workflow.
///
/// The model-facing action surface suppresses `request_skills` once a full
/// skill body is in context. A provider that still emits the forbidden lookup
/// is rejected at MAAP validation rather than handed to the runtime skill
/// executor as another recoverable lookup.
#[test]
fn runtime_explicit_skill_prompt_rejects_redundant_skill_catalog_lookup() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "$create-skill create a review skill")
        .unwrap();
    service
        .pending_agent_provider_tasks
        .remove(&started.turn_id);
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "request skills again".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "check available skill workflows".to_string(),
                thought: None,
                turn_id: started.turn_id.clone(),
                agent_id: started.agent_id.clone(),
                actions: vec![crate::agent::AgentAction {
                    id: "skill-catalog-loop".to_string(),
                    rationale: "check available skill workflows".to_string(),
                    payload: crate::agent::AgentActionPayload::RequestSkills,
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(
        execution
            .response
            .raw_text
            .contains("maap action type request_skills is not allowed"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        !execution
            .request
            .allowed_actions
            .action_type_names()
            .contains(&"request_skills")
    );
    assert!(execution.action_results.is_empty());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/list-skills` displays the effective pane skill catalog with the
/// same `$skill` invocation syntax accepted by explicit skill prompts. This
/// gives users a discoverable way to see and select available workflows before
/// submitting a prompt.
#[test]
fn runtime_agent_shell_list_skills_displays_effective_catalog() {
    let config_root = temp_root("runtime-list-skills");
    let skill_dir = config_root.join("skills/review");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Review workflow\n---\n\nCheck tests and risks.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "/list-skills")
        .unwrap();

    assert!(response.contains("## Skills"), "{response}");
    assert!(response.contains("Start a prompt with `$`"), "{response}");
    assert!(
        response.contains("`$<skill-name> [additional context]`"),
        "{response}"
    );
    assert!(
        response
            .contains("| `$create-skill` | builtin | Create or modify concise Mezzanine skills"),
        "{response}"
    );
    assert!(
        response.contains("| `$review` | user | Review workflow |"),
        "{response}"
    );
}

/// Verifies `/list-skills` shows the built-in skill-authoring workflow when the
/// current pane has no user or trusted-project skills. This makes skill
/// creation discoverable before any external skill directories exist.
#[test]
fn runtime_agent_shell_list_skills_reports_builtin_catalog_without_external_skills() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(temp_root("runtime-list-skills-empty"));

    let response = service
        .execute_agent_shell_command(&primary, "/list-skills")
        .unwrap();

    assert!(
        response
            .contains("| `$create-skill` | builtin | Create or modify concise Mezzanine skills"),
        "{response}"
    );
    assert!(
        !response.contains("No skills are currently available."),
        "{response}"
    );
    assert!(response.contains("Start a prompt with `$`"), "{response}");
}

/// Verifies overlapping compaction attempts are rejected before they can start
/// another model request for the same pane.
#[test]
fn runtime_agent_shell_compact_rejects_overlapping_pane_compaction() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.agent_compacting_panes.insert("%1".to_string(), 1);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-overlap","method":"agent/shell/command","params":{"idempotency_key":"compact-overlap","input":"/compact"}}"#,
        &primary,
    );

    assert!(response.contains("already compacting"), "{response}");
}

/// Verifies compaction keeps only a bounded raw transcript tail when the active
/// conversation is larger than the exact-reference window.
///
/// The compact memory can summarize older entries, but the next turn needs the
/// recent tail verbatim for prompts like "implement the first item". Older raw
/// messages should not remain in transcript replay after compaction.
#[test]
fn runtime_agent_shell_compact_retains_bounded_recent_transcript_tail() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-tail-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-tail-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-tail-test"]
default_model = "gpt-compact-tail-test"
[model_profiles.compact-tail-test]
provider = "openai"
model = "gpt-compact-tail-test"
context_window_tokens = 5000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-tail"));
    for sequence in 1..=12 {
        let (role, content) = match sequence {
            1 => (
                crate::transcript::TranscriptRole::User,
                format!(
                    "old raw marker should be summary only {}",
                    "old-word ".repeat(28)
                ),
            ),
            8 => (
                crate::transcript::TranscriptRole::Assistant,
                format!(
                    "Recent targets:\n1. Preserve raw tail after compaction.\n2. Keep memory summary. {}",
                    "recent-word ".repeat(28)
                ),
            ),
            _ if sequence % 2 == 0 => (
                crate::transcript::TranscriptRole::User,
                format!("filler user turn {sequence} {}", "tail-user ".repeat(28)),
            ),
            _ => (
                crate::transcript::TranscriptRole::Assistant,
                format!(
                    "filler assistant turn {sequence} {}",
                    "tail-assistant ".repeat(28)
                ),
            ),
        };
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: "as-tail".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-tail", 12)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-tail","method":"agent/shell/command","params":{"idempotency_key":"compact-tail","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains("state=queued"), "{compact}");
    assert!(compact.contains("summarized_entries=5"), "{compact}");
    complete_runtime_test_compaction(&mut service, "%1", "old raw marker should be summary only");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        7
    );

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-tail-prompt","method":"agent/shell/command","params":{"idempotency_key":"compact-tail-prompt","input":"Implement the first item"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    let compaction_notice = context
        .blocks
        .iter()
        .find(|block| block.label == "conversation compaction notice")
        .expect("compaction notice should be model-visible after /compact");
    assert!(
        compaction_notice
            .content
            .contains("Older durable transcript entries were summarized"),
        "{compaction_notice:?}"
    );
    let transcript_context = context
        .blocks
        .iter()
        .filter(|block| {
            matches!(
                block.source,
                ContextSourceKind::Transcript
                    | ContextSourceKind::TranscriptUser
                    | ContextSourceKind::TranscriptAssistant
                    | ContextSourceKind::TranscriptTool
            )
        })
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        transcript_context.contains("1. Preserve raw tail after compaction."),
        "{transcript_context}"
    );
    assert!(
        !transcript_context.contains("old raw marker should be summary only"),
        "{transcript_context}"
    );
}

/// Verifies explicit `/compact` is forced even when the entire transcript fits
/// inside the normal retained-tail budget.
///
/// The user command is a direct request to compact now, so it must summarize at
/// least one active durable entry instead of returning a budget-based no-op.
#[test]
fn runtime_agent_shell_compact_forces_summary_when_under_context_budget() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-forced-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-forced-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-forced-test"]
default_model = "gpt-compact-forced-test"
[model_profiles.compact-forced-test]
provider = "openai"
model = "gpt-compact-forced-test"
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-forced"));
    for sequence in 1..=3 {
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: "as-forced".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: crate::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("forced compact marker {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-forced", 3)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-forced","method":"agent/shell/command","params":{"idempotency_key":"compact-forced","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains(r#""kind":"mutated""#), "{compact}");
    assert!(compact.contains("state=queued"), "{compact}");
    assert!(compact.contains("summarized_entries=1"), "{compact}");
    assert!(
        !compact.contains("within-retained-context-tail"),
        "{compact}"
    );
    complete_runtime_test_compaction(&mut service, "%1", "forced compact marker 1");
    let compacted = service
        .memory_records()
        .into_iter()
        .find(|record| record.id == "compact-as-forced")
        .expect("compacted memory record");
    assert!(
        compacted.content.contains("forced compact marker 1"),
        "{}",
        compacted.content
    );
}

/// Verifies that `/compact` is explicit when there is no transcript content to
/// compact and that the empty path does not create a misleading memory record.
#[test]
fn runtime_agent_shell_compact_reports_empty_transcript_without_memory() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-empty","method":"agent/shell/command","params":{"idempotency_key":"compact-empty","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains(r#""kind":"display""#), "{compact}");
    assert!(compact.contains(r#""command":"compact""#), "{compact}");
    assert!(
        compact.contains("compacted=false reason=no-transcript-entries"),
        "{compact}"
    );
    assert!(compact.contains("source=model-compact"), "{compact}");
    assert!(service.memory_records().is_empty());
}

/// Verifies that `/personality` mutates live pane-scoped agent preferences and
/// that those preferences are appended to the next prompt context. This makes
/// the slash command affect provider input instead of only acknowledging a
/// runtime placeholder.
#[test]
fn runtime_agent_shell_personality_feeds_prompt_context() {
    let mut service = test_runtime_service();
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

    let personality = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"personality","method":"agent/shell/command","params":{"idempotency_key":"personality","input":"/personality concise"}}"#,
        &primary,
    );
    assert!(personality.contains(r#""kind":"mutated""#), "{personality}");
    assert!(
        personality.contains(r#""command":"personality""#),
        "{personality}"
    );
    assert!(personality.contains("style=concise"), "{personality}");
    assert!(
        personality.contains("source=runtime-personality"),
        "{personality}"
    );
    assert!(!personality.contains("requires_runtime"), "{personality}");

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"preference-prompt","method":"agent/shell/command","params":{"idempotency_key":"preference-prompt","input":"prepare work"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.label == "agent shell personality"
                && block.content.contains("concise"))
    );
}

/// Verifies that saved agent conversations can be listed, resumed into the
/// current pane, exposed to prompt context, and forked while keeping readline
/// prompt history available through the shared prompt-history file.
#[test]
fn runtime_agent_shell_resume_and_fork_manage_saved_conversations() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-fork"));
    let cwd = temp_root("runtime-agent-resume-cwd");
    fs::create_dir_all(&cwd).unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::System,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: format!("cwd={}", cwd.display()),
        })
        .unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 2,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append_prompt_history("saved", "find files")
        .unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-latest".to_string(),
            agent_id: "agent-%8".to_string(),
            pane_id: "%8".to_string(),
            content: "latest prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append_presentation(&crate::transcript::AgentPresentationEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 3,
            pane_id: "%9".to_string(),
            turn_id: Some("turn-old".to_string()),
            terminal_width: 80,
            style_names: vec!["assistant".to_string(), "status".to_string()],
            display_lines: vec![
                "agent> rendered saved response".to_string(),
                "agent: rendered saved status".to_string(),
            ],
            copy_lines: vec![
                "agent> copy saved response".to_string(),
                "agent: copy saved status".to_string(),
            ],
            ansi_text: Some(
                "\r▐ agent> rendered saved response\r\n▐ agent: rendered saved status\r\n▐ ansi-only replay marker\r\n"
                    .to_string(),
            ),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let picker = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-list","method":"agent/shell/command","params":{"idempotency_key":"resume-list","input":"/resume"}}"#,
        &primary,
    );
    assert!(picker.contains("mez-agent:/resume%20saved"), "{picker}");
    assert!(picker.contains("mez-agent:/resume%20latest"), "{picker}");

    let latest = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-latest","method":"agent/shell/command","params":{"idempotency_key":"resume-latest","input":"/resume --latest"}}"#,
        &primary,
    );
    assert!(latest.contains("conversation_id=latest"), "{latest}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("latest")
    );

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume","method":"agent/shell/command","params":{"idempotency_key":"resume","input":"/resume saved"}}"#,
        &primary,
    );
    assert!(resumed.contains("conversation_id=saved"), "{resumed}");
    assert_eq!(
        service.pane_current_working_directory("%1").as_deref(),
        Some(cwd.as_path())
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("saved")
    );
    let resumed_pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        resumed_pane_text.contains("rendered sa") && resumed_pane_text.contains("ved response"),
        "{resumed_pane_text}"
    );
    assert!(
        resumed_pane_text.contains("agent: rendered sa")
            && resumed_pane_text.contains("ved status"),
        "{resumed_pane_text}"
    );
    assert!(
        resumed_pane_text.contains("ansi-only") && resumed_pane_text.contains("arker"),
        "{resumed_pane_text}"
    );
    assert!(
        !resumed_pane_text.contains("Resumed Agent Session"),
        "{resumed_pane_text}"
    );
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .history(),
        &[
            String::from("find files"),
            String::from("/resume"),
            String::from("/resume --latest"),
            String::from("/resume saved")
        ]
    );
    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == crate::agent::ContextSourceKind::TranscriptUser
            && block.content.contains("saved prompt")
    }));

    let forked = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"fork","method":"agent/shell/command","params":{"idempotency_key":"fork","input":"/fork saved-fork"}}"#,
        &primary,
    );
    assert!(forked.contains("source=saved"), "{forked}");
    assert!(forked.contains("conversation_id=saved-fork"), "{forked}");
    assert!(forked.contains("source_pane=%1"), "{forked}");
    assert_eq!(transcript_store.inspect("saved-fork").unwrap().len(), 2);
    assert_eq!(
        transcript_store.inspect_presentation("saved-fork").unwrap()[0].display_lines[0],
        "agent> rendered saved response"
    );
    let forked_pane = service
        .agent_shell_store()
        .sessions()
        .find(|session| session.session_id == "saved-fork")
        .map(|session| session.pane_id.clone())
        .expect("forked conversation should be bound to a pane");
    assert_ne!(forked_pane, "%1");
    assert_eq!(
        transcript_store.prompt_history("saved-fork").unwrap(),
        vec![
            String::from("find files"),
            String::from("/resume"),
            String::from("/resume --latest"),
            String::from("/resume saved"),
            String::from("/fork saved-fork")
        ]
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("saved")
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(&forked_pane)
            .map(|session| session.session_id.as_str()),
        Some("saved-fork")
    );
    assert_eq!(
        service
            .agent_prompt_inputs
            .get(&forked_pane)
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume saved"
    );
    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(cwd);
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
        vec![String::from("agent> visual-only pane replay")]
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

/// Verifies active agent shell metadata survives a daemon-style restart for the
/// same Mezzanine session without replaying a prompt or requiring a snapshot.
#[test]
fn runtime_restores_active_agent_session_metadata_for_same_session() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-active-restore"));
    let cwd = temp_root("runtime-agent-active-restore-cwd");
    fs::create_dir_all(&cwd).unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "saved restart context".to_string(),
        })
        .unwrap();
    transcript_store
        .append_prompt_history("saved", "remember this")
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .pane_current_working_directories
        .insert("%1".to_string(), cwd.clone());

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-resume","method":"agent/shell/command","params":{"idempotency_key":"restore-resume","input":"/resume saved"}}"#,
        &primary,
    );
    assert!(resumed.contains("conversation_id=saved"), "{resumed}");
    let saved_token_usage_key = crate::agent::ModelTokenUsageKey::new("openai", "gpt-5.5");
    let saved_token_usage = crate::agent::ModelTokenUsage {
        input_tokens: 321,
        output_tokens: 45,
        reasoning_tokens: 12,
        cached_input_tokens: Some(123),
    };
    service.record_agent_provider_token_usage("%1", saved_token_usage);
    let routing = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-routing","method":"agent/shell/command","params":{"idempotency_key":"restore-routing","input":"/routing on"}}"#,
        &primary,
    );
    assert!(routing.contains("enabled=true"), "{routing}");
    let approval = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-approval","method":"agent/shell/command","params":{"idempotency_key":"restore-approval","input":"/approval full-access"}}"#,
        &primary,
    );
    assert!(approval.contains("requested=full-access"), "{approval}");
    let personality = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-personality","method":"agent/shell/command","params":{"idempotency_key":"restore-personality","input":"/personality concise"}}"#,
        &primary,
    );
    assert!(personality.contains("style=concise"), "{personality}");
    let log_level = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-log-level","method":"agent/shell/command","params":{"idempotency_key":"restore-log-level","input":"/log-level trace"}}"#,
        &primary,
    );
    assert!(log_level.contains("now trace"), "{log_level}");
    let saved_metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(saved_metadata.len(), 1);
    assert_eq!(
        saved_metadata[0].working_directory.as_deref(),
        Some(cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        saved_metadata[0].token_usage,
        saved_token_usage
    );
    assert_eq!(
        saved_metadata[0]
            .token_usage_by_model
            .get(&saved_token_usage_key),
        Some(&saved_token_usage)
    );
    assert_eq!(saved_metadata[0].routing_enabled, Some(true));
    assert_eq!(
        saved_metadata[0].approval_policy.as_deref(),
        Some("full-access")
    );

    let mut restored = test_runtime_service();
    restored.session.id = service.session().id.clone();
    restored.set_agent_transcript_store(transcript_store.clone());
    let restored_count = restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    let restored_session = restored.agent_shell_store().get("%1").unwrap();
    assert_eq!(restored_count, 1);
    assert_eq!(restored_session.session_id, "saved");
    assert_eq!(restored_session.visibility, AgentShellVisibility::Visible);
    assert_eq!(restored_session.transcript_entries, 1);
    assert_eq!(restored_session.log_level, AgentLogLevel::Trace);
    assert_eq!(
        restored
            .agent_token_usage_by_conversation
            .get("saved")
            .and_then(|usage_by_model| usage_by_model.get(&saved_token_usage_key))
            .copied(),
        Some(saved_token_usage)
    );
    assert_eq!(
        restored.agent_routing_overrides.get("%1").copied(),
        Some(true)
    );
    assert_eq!(
        restored.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        restored.pane_current_working_directory("%1").as_deref(),
        Some(cwd.as_path())
    );
    assert_eq!(
        restored.agent_response_styles.get("%1").map(String::as_str),
        Some("concise")
    );
    assert_eq!(
        restored
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .history(),
        &[
            String::from("remember this"),
            String::from("/resume saved"),
            String::from("/routing on"),
            String::from("/approval full-access"),
            String::from("/personality concise"),
            String::from("/log-level trace"),
        ]
    );
    let context = restored
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == crate::agent::ContextSourceKind::TranscriptUser
            && block.content.contains("saved restart context")
    }));
    let _ = fs::remove_dir_all(cwd);
}

/// Verifies `/resume` reloads saved provider token totals for the rebound
/// conversation.
///
/// Active-session metadata is the durable source for pane-level provider
/// accounting. A manual resume path must hydrate the same in-memory usage map
/// as daemon startup restore so `/status` does not reset token counts to zero.
#[test]
fn runtime_resume_restores_provider_token_usage_from_session_metadata() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-tokens"));
    let mut service = test_runtime_service();
    let mezzanine_session_id = service.session().id.as_str().to_string();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved-tokens".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "resume with prior token totals".to_string(),
        })
        .unwrap();
    let saved_token_usage_key = crate::agent::ModelTokenUsageKey::new("openai", "gpt-saved");
    let saved_token_usage = crate::agent::ModelTokenUsage {
        input_tokens: 900,
        output_tokens: 80,
        reasoning_tokens: 33,
        cached_input_tokens: Some(450),
    };
    transcript_store
        .save_agent_session_metadata(
            &mezzanine_session_id,
            &[crate::transcript::AgentSessionMetadata {
                mezzanine_session_id: mezzanine_session_id.clone(),
                pane_id: "%1".to_string(),
                conversation_id: "saved-tokens".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                routing_enabled: Some(true),
                approval_policy: Some("full-access".to_string()),
                working_directory: None,
                project_root: None,
                context_usage: Some("42%".to_string()),
                token_usage: saved_token_usage,
                token_usage_by_model: std::collections::BTreeMap::from([(
                    saved_token_usage_key.clone(),
                    saved_token_usage,
                )]),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-tokens","method":"agent/shell/command","params":{"idempotency_key":"resume-tokens","input":"/resume saved-tokens"}}"#,
        &primary,
    );
    assert!(
        resumed.contains("conversation_id=saved-tokens"),
        "{resumed}"
    );
    assert_eq!(
        service.agent_routing_overrides.get("%1").copied(),
        Some(true)
    );
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        service
            .agent_context_usage_by_conversation
            .get("saved-tokens")
            .map(String::as_str),
        Some("42%")
    );
    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-token-status","method":"agent/shell/command","params":{"idempotency_key":"resume-token-status","input":"/status"}}"#,
        &primary,
    );

    assert!(
        status.contains(
            "| Provider tokens | gpt-saved via openai: input=450 (+ 450 cached) cache_hit=50.00% output=80 reasoning=33 total=980 |"
        ),
        "{status}"
    );
    assert!(
        status.contains("| openai | gpt-saved | 450 | 450 | 80 | 33 | 50.00% |"),
        "{status}"
    );
    let restored_metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(restored_metadata.len(), 1, "{restored_metadata:#?}");
    let restored_metadata = &restored_metadata[0];
    assert_eq!(
        restored_metadata.conversation_id,
        "saved-tokens",
        "{restored_metadata:#?}"
    );
    assert_eq!(
        restored_metadata.token_usage_by_model,
        std::collections::BTreeMap::from([(
            saved_token_usage_key.clone(),
            saved_token_usage,
        )]),
        "{restored_metadata:#?}"
    );

    let (_, mut profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    profile.provider = "openai".to_string();
    profile.model = "gpt-saved".to_string();
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 5,
            cached_input_tokens: Some(25),
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 5,
            cached_input_tokens: Some(25),
        },
        Some(&profile),
    );
    let resumed_status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-token-status-after-usage","method":"agent/shell/command","params":{"idempotency_key":"resume-token-status-after-usage","input":"/status"}}"#,
        &primary,
    );
    assert!(
        resumed_status.contains("| openai | gpt-saved | 525 | 475 | 100 | 38 | 47.50% |"),
        "{resumed_status}"
    );
    let resumed_metadata = transcript_store
        .load_agent_session_metadata(&mezzanine_session_id)
        .unwrap();
    assert_eq!(resumed_metadata.len(), 1, "{resumed_metadata:#?}");
    assert_eq!(
        resumed_metadata[0].token_usage_by_model,
        std::collections::BTreeMap::from([(
            saved_token_usage_key,
            crate::agent::ModelTokenUsage {
                input_tokens: 1000,
                output_tokens: 100,
                reasoning_tokens: 38,
                cached_input_tokens: Some(475),
            },
        )]),
        "{resumed_metadata:#?}"
    );
}

/// Verifies legacy agent-session metadata cannot narrow a configured elevated
/// approval default during restore.
///
/// Older checkpoints recorded the effective policy even when it only came from
/// the default configuration. Restoring those rows must not silently preempt a
/// user's newer `permissions.approval_policy = "full-access"` configuration.
#[test]
fn runtime_agent_session_restore_does_not_narrow_configured_approval_default() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-approval-default"));
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[permissions]\napproval_policy = \"full-access\"\n".to_string(),
        }])
        .unwrap();
    let mezzanine_session_id = service.session().id.as_str().to_string();
    transcript_store
        .save_agent_session_metadata(
            &mezzanine_session_id,
            &[crate::transcript::AgentSessionMetadata {
                mezzanine_session_id: mezzanine_session_id.clone(),
                pane_id: "%1".to_string(),
                conversation_id: "legacy-ask".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                routing_enabled: None,
                approval_policy: Some("ask".to_string()),
                working_directory: None,
                project_root: None,
                context_usage: None,
                token_usage: Default::default(),
                token_usage_by_model: Default::default(),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());

    let restored = service
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    assert_eq!(restored, 1);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    let rewritten = transcript_store
        .load_agent_session_metadata(&mezzanine_session_id)
        .unwrap();
    assert_eq!(rewritten.len(), 1);
    assert_eq!(rewritten[0].approval_policy, None);
}

/// Verifies active agent metadata from a different Mezzanine session id does
/// not auto-bind a fresh runtime pane to a stale conversation.
#[test]
fn runtime_does_not_restore_agent_metadata_for_other_sessions() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-other-session"));
    transcript_store
        .save_agent_session_metadata(
            "$foreign",
            &[crate::transcript::AgentSessionMetadata {
                mezzanine_session_id: "$foreign".to_string(),
                pane_id: "%1".to_string(),
                conversation_id: "foreign".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                routing_enabled: None,
                approval_policy: None,
                working_directory: None,
                project_root: None,
                context_usage: None,
                token_usage: Default::default(),
                token_usage_by_model: Default::default(),
            }],
        )
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store);

    let restored = service
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    assert_eq!(restored, 0);
    assert!(service.agent_shell_store().get("%1").is_none());
}

/// Verifies crash-recovered active metadata never resumes a previously running
/// turn automatically; it restores the conversation and records the turn as
/// interrupted so retry requires a fresh user action.
#[test]
fn runtime_restored_agent_metadata_marks_running_turn_interrupted() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-active-interrupted"));
    service.set_agent_transcript_store(transcript_store.clone());
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
        .start_agent_turn(crate::agent::AgentTurnRecord {
            turn_id: "turn-running-restore".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 200,
            policy_profile: "runtime".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,
        })
        .unwrap();
    assert_eq!(
        transcript_store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap()[0]
            .running_turn_id
            .as_deref(),
        Some("turn-running-restore")
    );

    let mut restored = test_runtime_service();
    restored.session.id = service.session().id.clone();
    restored.set_agent_transcript_store(transcript_store);
    let restored_count = restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    let restored_session = restored.agent_shell_store().get("%1").unwrap();
    let restored_turn = restored
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-running-restore")
        .unwrap();
    assert_eq!(restored_count, 1);
    assert_eq!(restored_session.session_id, conversation_id);
    assert_eq!(restored_session.running_turn_id, None);
    assert_eq!(restored_turn.state, AgentTurnState::Interrupted);
}

/// Verifies that `/fork` returns a concrete runtime diagnostic when no
/// transcript store is attached instead of falling back to a generic
/// runtime-required placeholder.
#[test]
fn runtime_agent_shell_fork_reports_missing_transcript_store() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"fork-missing-store","method":"agent/shell/command","params":{"idempotency_key":"fork-missing-store","input":"/fork branch"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"fork""#), "{response}");
    assert!(
        response.contains("forked=false reason=transcript-store-unavailable"),
        "{response}"
    );
    assert!(response.contains("source=runtime-fork"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies runtime provider execution completes running prompt turn.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_provider_execution_completes_running_prompt_turn() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-provider-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeEchoProvider,
            ModelProfile {
                provider: "runtime-echo".to_string(),
                model: "echo-model".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert!(execution.final_turn);
    assert_eq!(execution.response.raw_text, "done");
    assert!(
        execution
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("summarize the pane"))
    );
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(AgentTurnState::Completed)
    );
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    let assistant_entry = entries
        .iter()
        .find(|entry| entry.role == crate::transcript::TranscriptRole::Assistant)
        .expect("assistant transcript entry should be persisted");
    assert!(
        assistant_entry
            .content
            .contains("thinking: test action batch rationale")
    );
    assert!(
        assistant_entry
            .content
            .contains("thinking: finish the turn")
    );
    assert!(assistant_entry.content.ends_with("done"));
    assert!(
        entries
            .iter()
            .any(|entry| entry.content.contains("summarize the pane"))
    );
    assert_eq!(
        service.pane_transcript_refs.get("%1"),
        Some(&vec![format!("transcript:%1:{conversation_id}")])
    );
    let tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(tasks.contains(r#""state":"completed""#), "{tasks}");
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(
        audit.contains(r#""event_type":"external_integration""#),
        "{audit}"
    );
    assert!(audit.contains(r#""action":"provider_request""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(audit.contains(r#""provider":"runtime-echo""#), "{audit}");
    assert!(audit.contains(r#""model":"echo-model""#), "{audit}");
    assert!(audit.contains(r#""turn_id":"turn-1""#), "{audit}");

    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies progress `say` messages become a bounded current-turn context ledger
/// before provider continuation.
///
/// Progress `say` text is user-visible but easy for the model to paraphrase in a
/// later action batch. The runtime keeps the recent entries as turn-volatile
/// local context so the next provider request can suppress redundant updates
/// without moving that changing text into the cache-stable prefix.
#[test]
fn runtime_progress_say_context_ledger_reaches_provider_continuation() {
    let mut service = test_runtime_service();
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
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-progress-ledger","input":"fix the repeated progress updates"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "progress".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "record the first sequence point".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-progress".to_string(),
                    rationale: "tell the user the owner changed".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Progress,
                        text: "The redundant updates are coming from repeated progress says."
                            .to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let first_execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first_execution.terminal_state, AgentTurnState::Running);
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    let ledger_block = context
        .blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::LocalMessage
                && block.label == "current-turn progress say ledger"
        })
        .expect("progress say ledger should be active turn context");
    assert_eq!(
        ledger_block.cache_policy(),
        crate::agent::ContextCachePolicy::Ineligible
    );
    assert!(
        ledger_block
            .content
            .contains("already emitted during the current turn"),
        "{}",
        ledger_block.content
    );
    assert!(
        ledger_block
            .content
            .contains("progress_say: The redundant updates are coming"),
        "{}",
        ledger_block.content
    );

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
            provider_transcript_events: Vec::new(),
        },
        last_request: RefCell::new(None),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();

    assert_eq!(executions.len(), 1);
    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::LocalMessage
            && message
                .content
                .contains("[current-turn progress say ledger]")
            && message.content.contains("It is not a user request")
            && message
                .content
                .contains("progress_say: The redundant updates are coming")
    }));
    assert!(!service.agent_turn_contexts.contains_key("turn-1"));
}

/// Verifies successive shell commands add a soft implementation-pressure hint.
///
/// Repeated successful shell inspection can keep a long turn localizing the
/// same owner instead of implementing the next phase. The runtime should nudge
/// the next provider continuation after the configured threshold while keeping
/// the hint volatile and advisory rather than failing the shell action.
#[test]
fn runtime_implementation_pressure_context_reaches_provider_continuation() {
    let mut service = test_runtime_service();
    service.agent_implementation_pressure_after_shell_actions = 2;
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
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-implementation-pressure","input":"finish the backlog fixes"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let shell_action = crate::agent::AgentAction {
        id: "inspect".to_string(),
        rationale: "read current owner".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Inspect owner".to_string(),
            command: "sed -n '1,80p' src/runtime/mod.rs".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.record_shell_dispatch_success(
        "turn-1",
        "sed -n '1,80p' src/runtime/mod.rs",
        &shell_action,
    );
    assert!(
        !service
            .agent_turn_contexts
            .get("turn-1")
            .unwrap()
            .blocks
            .iter()
            .any(|block| block.label == "implementation pressure")
    );

    service.record_shell_dispatch_success(
        "turn-1",
        "sed -n '80,160p' src/runtime/mod.rs",
        &shell_action,
    );
    let pressure_block = service
        .agent_turn_contexts
        .get("turn-1")
        .unwrap()
        .blocks
        .iter()
        .find(|block| block.label == "implementation pressure")
        .expect("implementation pressure should be active turn context");
    assert_eq!(
        pressure_block.cache_policy(),
        crate::agent::ContextCachePolicy::Ineligible
    );
    assert!(
        pressure_block
            .content
            .contains("2 consecutive successful shell_command actions"),
        "{}",
        pressure_block.content
    );
    assert!(
        pressure_block
            .content
            .contains("Prefer the next implementation, validation, or final-report action now"),
        "{}",
        pressure_block.content
    );

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
            provider_transcript_events: Vec::new(),
        },
        last_request: RefCell::new(None),
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &second_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::LocalMessage
            && message.content.contains("[implementation pressure]")
            && message
                .content
                .contains("Use another shell_command only for one named missing fact")
    }));
}

/// Verifies implementation-pressure hints clear after a successful patch.
///
/// The pressure hint is meant to move read-only shell inspection toward
/// implementation. Once the model actually emits a semantic patch action, the
/// shell streak should reset so future continuation context does not keep
/// pressuring a turn that has already moved into implementation.
#[test]
fn runtime_implementation_pressure_resets_after_apply_patch_success() {
    let mut service = test_runtime_service();
    service.agent_implementation_pressure_after_shell_actions = 1;
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-implementation-pressure-reset","input":"patch the file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let shell_action = crate::agent::AgentAction {
        id: "inspect".to_string(),
        rationale: "read current owner".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Inspect owner".to_string(),
            command: "git diff -- src/runtime/mod.rs".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.record_shell_dispatch_success("turn-1", "git diff -- src/runtime/mod.rs", &shell_action);
    assert!(
        service
            .agent_turn_contexts
            .get("turn-1")
            .unwrap()
            .blocks
            .iter()
            .any(|block| block.label == "implementation pressure")
    );

    let patch_action = crate::agent::AgentAction {
        id: "patch".to_string(),
        rationale: "apply implementation".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/runtime/mod.rs\n@@\n context\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    service.record_shell_dispatch_success("turn-1", "mez apply-patch write", &patch_action);

    assert!(
        !service
            .agent_turn_contexts
            .get("turn-1")
            .unwrap()
            .blocks
            .iter()
            .any(|block| block.label == "implementation pressure")
    );
}

/// Verifies runtime suppresses repeated progress `say` updates during a turn.
///
/// Progress messages are user-visible sequence points. When a later provider
/// batch paraphrases an already displayed owner or diagnosis, the duplicate
/// should not be rendered, copied, retained in assistant context, or added back
/// into the progress ledger. The action still succeeds with compact feedback so
/// the model can continue without entering a correction loop.
#[test]
fn runtime_agent_suppresses_redundant_progress_say_updates() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-redundant-progress","input":"fix the selector"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first_progress = "The selector bug is in the real resume pager path.";
    let first_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "progress".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "record the owner".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-progress-1".to_string(),
                    rationale: "tell the user the selector owner".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Progress,
                        text: first_progress.to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let first_execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first_execution.terminal_state, AgentTurnState::Running);

    let duplicate_progress = "The surviving selector bug is still in the real resume pager path.";
    let final_text = "The fix is complete.";
    let second_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: duplicate_progress.to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    crate::agent::AgentAction {
                        id: "say-progress-2".to_string(),
                        rationale: "repeat the selector owner".to_string(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Progress,
                            text: duplicate_progress.to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    crate::agent::AgentAction {
                        id: "say-final".to_string(),
                        rationale: "finish the reply".to_string(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Final,
                            text: final_text.to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                ],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].terminal_state, AgentTurnState::Completed);

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains(first_progress), "{pane_text}");
    assert!(!pane_text.contains(duplicate_progress), "{pane_text}");
    assert!(pane_text.contains(final_text), "{pane_text}");
    assert!(
        executions[0].action_results.iter().any(|result| {
            result.action_id == "say-progress-2"
                && result
                    .structured_content_json
                    .as_deref()
                    .is_some_and(|content| content.contains("suppressed_duplicate_progress"))
        }),
        "{:?}",
        executions[0].action_results
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies batch thoughts are durable context notes, not normal-mode pane
/// chatter.
///
/// A model can emit a longer `thought` when that note should help future turns,
/// but normal users should not see that long-form internal context in routine
/// logs. Verbose-or-higher logs still render it as `thinking:` text for
/// diagnostics.
#[test]
fn runtime_batch_thought_is_hidden_until_verbose_logging() {
    fn pane_text_after_thought_response(level: AgentLogLevel) -> String {
        let mut service = test_runtime_service();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(80, 10).unwrap(), 30).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert("%1".to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .agent_shell_store_mut()
            .set_log_level("%1", level)
            .unwrap();
        let start = service.dispatch_runtime_control_body(
            r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-thought-display","input":"respond with durable context"}}"#,
            &primary,
        );
        assert!(start.contains(r#""state":"running""#), "{start}");
        let provider = RuntimeBatchProvider {
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "done".to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "respond with the final message".to_string(),
                    thought: Some(
                        "The durable note should only be visible in verbose logs.".to_string(),
                    ),
                    turn_id: "turn-1".to_string(),
                    agent_id: "agent-%1".to_string(),
                    actions: vec![crate::agent::AgentAction {
                        id: "say-final".to_string(),
                        rationale: String::new(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Final,
                            text: "Done.".to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    }],
                    final_turn: true,
                }),
                provider_transcript_events: Vec::new(),
            },
        };
        service
            .execute_agent_turn_with_provider(
                "turn-1",
                &provider,
                runtime_model_profile("runtime-batch", "test"),
            )
            .unwrap();
        let pane_text = service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        service.pane_processes_mut().terminate_all().unwrap();
        pane_text
    }

    let normal_text = pane_text_after_thought_response(AgentLogLevel::Normal);
    assert!(
        normal_text.contains("thinking: respond with the final message"),
        "{normal_text}"
    );
    assert!(!normal_text.contains("durable note"), "{normal_text}");
    assert!(normal_text.contains("Done."), "{normal_text}");

    let verbose_text = pane_text_after_thought_response(AgentLogLevel::Verbose);
    assert!(
        verbose_text.contains("thinking: respond with the final message"),
        "{verbose_text}"
    );
    assert!(
        verbose_text.contains("thinking: The durable note should only be visible"),
        "{verbose_text}"
    );
    assert!(verbose_text.contains("Done."), "{verbose_text}");
}

/// Verifies runtime treats a same-pane prompt submitted mid-turn as steering.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_prompt_during_running_turn_becomes_steering_context() {
    let mut service = test_runtime_service();
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

    let first = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-1","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn-1","input":"first prompt"}}"#,
        &primary,
    );
    assert!(first.contains(r#""state":"running""#), "{first}");
    let second = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-2","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn-2","input":"second prompt"}}"#,
        &primary,
    );
    assert!(second.contains(r#""kind":"mutated""#), "{second}");
    assert!(second.contains(r#""command":"prompt""#), "{second}");
    assert!(second.contains("injected_user_input=true"), "{second}");
    assert_eq!(service.agent_turn_ledger.turns().len(), 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    let provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: runtime_say_response("turn-1", "Acknowledged.", true),
        last_request: RefCell::new(None),
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let request = provider.last_request.borrow().clone().unwrap();
    let request_context = request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request_context.contains("second prompt"),
        "{request_context}"
    );
    assert!(
        request_context.contains("[user steering input during active turn]"),
        "{request_context}"
    );
    assert!(
        !service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-2")
    );
}

/// Verifies that the live runtime scheduler applies the starvation-bound
/// fairness rule after a running turn finishes: a queued runnable turn from a
/// different agent starts before a same-agent follow-up when capacity is one.
#[test]
fn runtime_scheduler_prefers_other_runnable_agent_after_completion() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane2 = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    for pane in ["%1", pane2.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert(pane.to_string(), screen);
    }

    service.start_agent_prompt_turn("%1", "first").unwrap();
    service.start_agent_prompt_turn("%1", "second").unwrap();
    service
        .start_agent_prompt_turn(pane2.as_str(), "third")
        .unwrap();
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 2);

    service.agent_scheduler_mut().complete("turn-1").unwrap();
    service
        .finish_agent_turn("%1", "turn-1", AgentTurnState::Completed)
        .unwrap();

    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-3"]
    );
    assert_eq!(
        service
            .agent_scheduler()
            .queued_turns()
            .map(|queued| queued.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-2"]
    );
}

/// Verifies terminal failures without a pane-local running shell marker still
/// drain scheduler capacity.
///
/// Some runtime failure paths settle a turn after its pane shell session was
/// already detached or removed. Those paths still release a global scheduler
/// slot, so they must immediately start queued independent work instead of
/// leaving it parked until unrelated input arrives.
#[test]
fn runtime_no_shell_session_provider_failure_starts_queued_turn() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    let pane2 = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    for pane in ["%1", pane2.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert(pane.to_string(), screen);
    }

    service.start_agent_prompt_turn("%1", "first").unwrap();
    service
        .start_agent_prompt_turn(pane2.as_str(), "second")
        .unwrap();
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);
    service.agent_shell_store_mut().remove_session("%1");

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeBatchFailingProvider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-2"]
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get(pane2.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-2")
    );
}

/// Verifies joined child completion drains the scheduler when other joined
/// children are queued behind a low concurrency limit.
///
/// A blocked parent releases its global scheduler slot while it waits for
/// joined subagents. When the first running child finishes, the next queued
/// child must start immediately so the parent is not left waiting for a child
/// turn that is ready but never launched.
#[test]
fn runtime_joined_child_completion_starts_next_queued_child() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    let child_one_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let child_two_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Horizontal)
        .unwrap();
    for pane in ["%1", child_one_pane.as_str(), child_two_pane.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(24, 5).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert(pane.to_string(), screen);
    }

    let parent = service.start_agent_prompt_turn("%1", "parent").unwrap();
    let child_one = service
        .start_agent_prompt_turn(child_one_pane.as_str(), "child one")
        .unwrap();
    let child_two = service
        .start_agent_prompt_turn(child_two_pane.as_str(), "child two")
        .unwrap();
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let spawn_one = runtime_spawn_agent_action("spawn-one", "child one");
    let spawn_two = runtime_spawn_agent_action("spawn-two", "child two");
    service.agent_turn_executions.insert(
        parent.turn_id.clone(),
        crate::agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&parent.turn_id, &parent.agent_id),
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "spawn children".to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    thought: None,
                    turn_id: parent.turn_id.clone(),
                    agent_id: parent.agent_id.clone(),
                    actions: vec![spawn_one.clone(), spawn_two.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![
                crate::agent::ActionResult::running(
                    &parent_turn,
                    &spawn_one,
                    vec!["waiting for child one".to_string()],
                    None,
                ),
                crate::agent::ActionResult::running(
                    &parent_turn,
                    &spawn_two,
                    vec!["waiting for child two".to_string()],
                    None,
                ),
            ],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.joined_subagent_dependencies.insert(
        child_one.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: parent.turn_id.clone(),
            parent_action_id: "spawn-one".to_string(),
            child_turn_id: child_one.turn_id.clone(),
            child_agent_id: child_one.agent_id.clone(),
            child_display_name: Some("child one".to_string()),
        },
    );
    service.joined_subagent_dependencies.insert(
        child_two.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: parent.turn_id.clone(),
            parent_action_id: "spawn-two".to_string(),
            child_turn_id: child_two.turn_id.clone(),
            child_agent_id: child_two.agent_id.clone(),
            child_display_name: Some("child two".to_string()),
        },
    );
    service.pending_agent_provider_tasks.remove(&parent.turn_id);
    service
        .agent_scheduler_mut()
        .block_running(&parent.turn_id)
        .unwrap();
    service
        .agent_turn_ledger
        .finish_turn(&parent.turn_id, AgentTurnState::Blocked)
        .unwrap();
    service.start_ready_agent_turns().unwrap();
    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec![child_one.turn_id.as_str()]
    );
    assert_eq!(
        service
            .agent_scheduler()
            .queued_turns()
            .map(|queued| queued.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec![child_two.turn_id.as_str()]
    );

    let child_provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &child_one.turn_id,
            &child_one.agent_id,
            "child one done",
            true,
        ),
    };
    service
        .execute_agent_turn_with_provider(
            &child_one.turn_id,
            &child_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec![child_two.turn_id.as_str()]
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert!(
        !service
            .joined_subagent_dependencies
            .contains_key(&child_one.turn_id)
    );
    assert!(
        service
            .joined_subagent_dependencies
            .contains_key(&child_two.turn_id)
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Blocked)
    );
}

/// Verifies a stale running `spawn_agent` result without a live joined child is
/// not treated as a runtime progress path.
///
/// The recovery loop must be able to fail or repair an orphaned parent turn
/// instead of considering any running `spawn_agent` result sufficient evidence
/// that a child can still complete.
#[test]
fn runtime_stale_joined_spawn_result_is_unreachable_progress() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 5).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    let parent = service.start_agent_prompt_turn("%1", "parent").unwrap();
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let spawn = runtime_spawn_agent_action("spawn-stale", "missing child");
    service.agent_turn_executions.insert(
        parent.turn_id.clone(),
        crate::agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&parent.turn_id, &parent.agent_id),
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "spawn child".to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    thought: None,
                    turn_id: parent.turn_id.clone(),
                    agent_id: parent.agent_id.clone(),
                    actions: vec![spawn.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![crate::agent::ActionResult::running(
                &parent_turn,
                &spawn,
                vec!["waiting for missing child".to_string()],
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.pending_agent_provider_tasks.remove(&parent.turn_id);

    assert!(service.unreachable_running_agent_turn_timer_needed());
    assert_eq!(service.reconcile_agent_runtime_progress_paths().unwrap(), 1);
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    assert!(!service.agent_turn_executions.contains_key(&parent.turn_id));
}

/// Verifies runtime provider failure persists and finishes turn.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_provider_failure_persists_and_finishes_turn() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-failure-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-provider-failure-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-fail","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeFailingProvider,
            ModelProfile {
                provider: "runtime-fail".to_string(),
                model: "failing-model".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
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
    assert!(entries.iter().any(|entry| {
        entry.role == crate::transcript::TranscriptRole::Assistant
            && entry.content.contains("provider_error")
    }));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    assert!(audit.contains(r#""provider":"runtime-fail""#), "{audit}");
    assert!(audit.contains(r#""model":"failing-model""#), "{audit}");
    assert!(audit.contains(r#""error_kind":"invalid_state""#), "{audit}");
    assert!(
        audit.contains(r#""error_message":"provider API request failed""#),
        "{audit}"
    );
    let failed_audit_record = audit
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|record| record["outcome"] == "failed")
        .unwrap();
    let provider_failure_json = failed_audit_record["metadata"]["provider_failure_json"]
        .as_str()
        .unwrap();
    let provider_failure: serde_json::Value = serde_json::from_str(provider_failure_json).unwrap();
    assert_eq!(provider_failure["status_code"], 400);
    assert_eq!(
        provider_failure["error"]["message"],
        "stream must be set to true"
    );
    assert_eq!(provider_failure["error"]["type"], "invalid_request_error");
    assert_eq!(
        provider_failure["error"]["code"],
        "missing_required_parameter"
    );
    assert!(
        failed_audit_record["metadata"]["provider_failure_json_bytes"]
            .as_str()
            .is_some_and(|value| value.parse::<usize>().unwrap() > 0),
        "{failed_audit_record}"
    );
    assert!(
        failed_audit_record["metadata"]["provider_failure_json_sha256"]
            .as_str()
            .is_some_and(|value| value.len() == 64),
        "{failed_audit_record}"
    );
    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies that provider errors carrying malformed raw output preserve that
/// output in the failed assistant transcript entry. This covers provider-native
/// MAAP parse failures that happen before the provider can build a
/// `ModelResponse`.
#[test]
fn runtime_provider_parse_failure_persists_raw_provider_text() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-parse-failure-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-provider-parse-failure-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-parse-fail","input":"produce malformed maap"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeProviderRawTextFailingProvider,
            ModelProfile {
                provider: "runtime-raw-fail".to_string(),
                model: "failing-model".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.role == crate::transcript::TranscriptRole::Assistant
            && entry
                .content
                .contains("{\"protocol\":\"maap/1\",\"actions\":[]}")
            && entry.content.contains("provider_error")
            && entry.content.contains("missing turn_id")
    }));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    assert!(audit.contains(r#""provider_raw_text_bytes":"#), "{audit}");
    assert!(audit.contains(r#""provider_raw_text_sha256":"#), "{audit}");
    assert!(audit.contains(r#""provider_failure_json":"#), "{audit}");
    assert!(audit.contains(r#"malformed_model_output"#), "{audit}");
    assert!(
        !audit.contains(r#""protocol":"maap/1","actions":[]"#),
        "{audit}"
    );
    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}
