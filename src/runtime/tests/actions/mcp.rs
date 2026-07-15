//! Runtime tests for actions mcp behavior.

use super::*;

/// Verifies MCP tool calls log a compact normal-mode action line with the
/// invoked server, tool, and compact JSON arguments.
///
/// MCP actions do not execute through the pane shell, but operators still need
/// a first-class execution row that makes the tool target and arguments visible
/// without waiting for verbose mode or failure output.
#[test]
fn runtime_mcp_call_logs_styled_action_line_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "mcp-1".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::McpCall {
            server: "github".to_string(),
            tool: "search_issues".to_string(),
            arguments_json: r#"{ "query": "prompt cache", "limit": 5 }"#.to_string(),
        },
    };

    let emitted = service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    assert!(emitted);

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(pane_text.contains("agent: mcp call: github/search_issues"));
    assert!(pane_text.contains("args={"));
    assert!(pane_text.contains(r#""query":"prompt cache""#));
    assert!(pane_text.contains(r#""limit":5"#));
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: mcp call:"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let prefix_column = display_column_for_fragment(&action_line.text, "agent:");
    let action_column = display_column_for_fragment(&action_line.text, "mcp call");
    let argument_column = display_column_for_fragment(&action_line.text, "github/search_issues");
    let prefix_rendition = styled_line_rendition_at(action_line, prefix_column);
    let action_rendition = styled_line_rendition_at(action_line, action_column);
    let argument_rendition = styled_line_rendition_at(action_line, argument_column);
    assert_eq!(
        prefix_rendition.foreground,
        Some(theme.colors.agent_transcript_status.foreground)
    );
    assert!(prefix_rendition.dim);
    assert_eq!(
        action_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground)
    );
    assert!(action_rendition.bold);
    assert_ne!(
        argument_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground),
        "{action_line:?}"
    );
}

/// Verifies runtime control mcp list uses runtime owned registry.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_mcp_list_uses_runtime_owned_registry() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    service
        .mcp_registry_mut()
        .mark_available(
            "fs",
            vec![crate::mcp::McpToolState {
                server_id: String::new(),
                name: "read_file".to_string(),
                available: true,
                blacklisted: false,
                permission_required: true,
                effects: crate::mcp::McpToolEffects::none(),
                approval: crate::mcp::McpApprovalSetting::Inherit,
                description: "read a file".to_string(),
                input_schema_json: "{}".to_string(),
            }],
        )
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"mcp","method":"mcp/list","params":{}}"#,
        &primary,
    );

    assert!(response.contains(r#""id":"fs""#), "{response}");
    assert!(response.contains(r#""id":"fs:read_file""#), "{response}");

    let targeted = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"mcp-targeted","method":"mcp/list","params":{"target":{"default":true}}}"#,
        &primary,
    );
    assert!(targeted.contains(r#""id":"fs""#), "{targeted}");

    let missing_session = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"mcp-missing","method":"mcp/list","params":{"target":{"name":"elsewhere"}}}"#,
        &primary,
    );
    assert!(
        missing_session.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session}"
    );
}

/// Verifies that the synchronous MCP retry helper keeps async-only transports
/// blacklisted instead of exposing stale tools. Blacklisted servers stay hidden
/// from the model until an integration workflow with async runtime support can
/// rediscover them.
#[tokio::test]
async fn runtime_mcp_retry_control_keeps_async_only_blacklisted_server_hidden() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-mcp-retry");
    let script_path = root.join("mcp-fixture.sh");
    fs::write(&script_path, runtime_mcp_fixture_script(false)).unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [{}]\napproval = \"allow\"\ntool_timeout_ms = 1000\n",
                toml_string(script_path.to_string_lossy().as_ref())
            ),
        }])
        .unwrap();
    service.apply_runtime_config_layers_async().await.unwrap();
    service
        .mcp_registry_mut()
        .blacklist_for_session("fixture", "failed handshake")
        .unwrap();
    assert!(
        service
            .mcp_registry()
            .prompt_summary()
            .available_tools
            .is_empty()
    );
    let control_retry = service.retry_runtime_mcp_server("fixture").unwrap();

    assert!(
        control_retry.previous_status_name() == "blacklisted",
        "{control_retry:?}"
    );
    assert_eq!(
        control_retry.status_name(),
        "blacklisted",
        "{control_retry:?}"
    );
    assert!(!control_retry.rediscovered, "{control_retry:?}");
    assert!(
        control_retry
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("requires async runtime discovery")),
        "{control_retry:?}"
    );
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        crate::mcp::McpServerStatus::Blacklisted
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime applies permission and mcp state from config layers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_applies_permission_and_mcp_state_from_config_layers() {
    let mut service = test_runtime_service();
    let report = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[permissions]\napproval_policy = \"full-access\"\nbypass_mode = false\n[[permissions.command_rules]]\npattern = [\"cargo\", \"test\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"prefix\"\n[mcp_servers.fs]\nname = \"filesystem\"\ncommand = \"mcp-fs\"\nargs = [\"--root\", \".\"]\nenv_vars = [\"MEZ_TEST_MISSING_TOKEN\"]\n".to_string(),
        }])
        .unwrap();

    assert_eq!(report.applied_layers, vec!["primary".to_string()]);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert!(!service.permission_policy().approval_bypass());
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("cargo test --all-targets"),
        RuleDecision::Allow
    );
    assert_eq!(service.mcp_registry().list_servers().len(), 1);
    assert_eq!(
        service.mcp_registry().prompt_summary().unavailable_servers[0].server_id,
        "fs"
    );
    assert_eq!(report.providers_configured, 1);
    assert_eq!(report.model_profiles_configured, 7);
    assert_eq!(report.default_model_profile.as_deref(), Some("default"));
    let profile = service
        .provider_registry()
        .resolve_profile("default")
        .unwrap();
    assert_eq!(profile.provider, "openai");
    assert_eq!(profile.model, "gpt-5.6-sol");
    assert!(
        service
            .provider_registry()
            .resolve_profile("gpt-5.6-terra")
            .is_ok(),
        "built-in OpenAI model profiles should be available when no provider list is configured"
    );
}

/// Verifies `/list-mcp` completion includes configured MCP server ids supplied
/// by the live runtime registry. MCP server names are dynamic configuration
/// data, so they must not be limited to static slash-command candidates.
#[test]
fn runtime_agent_prompt_list_mcp_autocompletes_configured_server_id() {
    let mut service = test_runtime_service();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fixture",
            "Fixture MCP",
            "mcp-fixture",
            Vec::new(),
        ))
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/list-mcp fi".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/list-mcp fixture "
    );
}

/// Verifies `@server` completion includes configured MCP server ids supplied
/// by the live runtime registry. Prompt-local MCP server names use the same
/// dynamic candidate source as `/list-mcp` without requiring a slash command.
#[test]
fn runtime_agent_prompt_at_mcp_autocompletes_configured_server_id() {
    let mut service = test_runtime_service();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fixture",
            "Fixture MCP",
            "mcp-fixture",
            Vec::new(),
        ))
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"ask @fi".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "ask @fixture "
    );
}

/// Verifies synchronous `/list-mcp` inside an active Tokio runtime degrades
/// gracefully to configured registry state instead of surfacing the blocking
/// discovery error reserved for callers outside async execution.
#[tokio::test]
async fn runtime_agent_shell_list_mcp_inside_active_runtime_reports_configured_server() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fixture",
            "fixture",
            "mcp-fixture",
            Vec::new(),
        ))
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-mcp","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-active-runtime","input":"/list-mcp"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains("## MCP Servers"), "{response}");
    assert!(response.contains("Servers: 1"), "{response}");
    assert!(response.contains("Tools: 0"), "{response}");
    assert!(response.contains("### `fixture` - fixture"), "{response}");
    assert!(response.contains("- State: enabled"), "{response}");
    assert!(response.contains("- Status: configured"), "{response}");
    assert!(
        !response
            .contains("synchronous /list-mcp discovery cannot run inside an active async runtime"),
        "{response}"
    );
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        crate::mcp::McpServerStatus::Configured
    );
}

/// Verifies async runtime config application initializes MCP transports at
/// session-start time and records human-readable lifecycle status.
///
/// MCP tools need to be available before the first model request so the model
/// can choose `mcp_call` from concrete runtime context instead of treating the
/// server as an unknown integration. The event log also needs plain status
/// messages so operators can see when startup discovery begins and when MCP
/// servers are ready to field requests.
#[tokio::test]
async fn runtime_async_config_apply_initializes_mcp_and_logs_readable_status() {
    let mut service = test_runtime_service();
    let script = runtime_mcp_fixture_script(false);

    let report = service
        .replace_config_layers_async(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [\"-c\", {}]\napproval = \"allow\"\ntool_timeout_ms = 1000\n",
                toml_string(&script)
            ),
        }])
        .await
        .unwrap();

    assert_eq!(report.mcp_servers_configured, 1);
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        crate::mcp::McpServerStatus::Available
    );
    assert_eq!(
        service.mcp_registry().prompt_summary().available_tools[0].tool_name,
        "echo"
    );
    let payloads = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .into_iter()
        .map(|event| event.payload)
        .collect::<Vec<_>>();
    assert!(
        payloads.iter().any(|payload| {
            payload.contains(r#""phase":"started""#)
                && payload.contains("Starting MCP initialization for 1 configured server.")
        }),
        "{payloads:#?}"
    );
    assert!(
        payloads.iter().any(|payload| {
            payload.contains(r#""server_id":"fixture""#)
                && payload.contains(r#""status":"available""#)
                && payload.contains("MCP server fixture is ready to field requests")
        }),
        "{payloads:#?}"
    );
    assert!(
        payloads.iter().any(|payload| {
            payload.contains(r#""phase":"completed""#)
                && payload.contains(
                    "MCP initialization complete: 1 enabled server ready to field requests",
                )
        }),
        "{payloads:#?}"
    );
}

/// Verifies `/list-mcp` starts configured MCP transports after a synchronous
/// config load. Default startup paths apply configuration synchronously, so the
/// user-facing MCP listing must not require a separate config reload before the
/// server becomes available to the agent runtime.
#[test]
fn runtime_agent_shell_list_mcp_lazily_discovers_configured_server() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-list-mcp-lazy-discovery");
    let script_path = root.join("mcp-fixture.sh");
    fs::write(&script_path, runtime_mcp_fixture_script(false)).unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [{}]\napproval = \"allow\"\ntool_timeout_ms = 1000\n",
                toml_string(script_path.to_string_lossy().as_ref())
            ),
        }])
        .unwrap();
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        crate::mcp::McpServerStatus::Configured
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-mcp","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-lazy","input":"/list-mcp"}}"#,
        &primary,
    );

    assert!(response.contains("## MCP Servers"), "{response}");
    assert!(response.contains("Servers: 1"), "{response}");
    assert!(response.contains("Tools: 1"), "{response}");
    assert!(response.contains("### `fixture` - fixture"), "{response}");
    assert!(response.contains("- Status: available"), "{response}");
    assert!(response.contains("| `echo` | available |"), "{response}");
    assert_eq!(
        service.mcp_registry().prompt_summary().available_tools[0].tool_name,
        "echo"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime executes accepted stdio mcp action and audits call.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn runtime_executes_accepted_stdio_mcp_action_and_audits_call() {
    let mut service = test_runtime_service();
    let audit_root = temp_root("runtime-mcp-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    let script = runtime_mcp_fixture_script(false);
    service
        .replace_config_layers_async(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [\"-c\", {}]\napproval = \"allow\"\ntool_timeout_ms = 1000\n",
                toml_string(&script)
            ),
        }])
        .await
        .unwrap();
    assert_eq!(
        service.mcp_registry().prompt_summary().available_tools[0].tool_name,
        "echo"
    );
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-turn","input":"call @fixture echo tool"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "calling mcp".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "m1".to_string(),
                    rationale: "call mcp".to_string(),
                    payload: mez_agent::AgentActionPayload::McpCall {
                        server: "fixture".to_string(),
                        tool: "echo".to_string(),
                        arguments_json: r#"{"message":"hello"}"#.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider_async(
            "turn-1",
            &provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert!(
        execution.action_results[0]
            .content_text()
            .contains("hello from mcp")
    );
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(
        audit.contains(r#""event_type":"external_integration""#),
        "{audit}"
    );
    assert!(audit.contains(r#""action":"mcp_call""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(audit.contains(r#""server_id":"fixture""#), "{audit}");
    assert!(
        audit.contains(r#""arguments_json":"{\"message\":\"hello\"}""#),
        "{audit}"
    );
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies full-access mode satisfies MCP tool prompt approval while still
/// executing the call through the normal MCP registry and transport path.
///
/// This prevents `approval = "prompt"` MCP tools from creating blocked action
/// approvals after the user has explicitly selected full-access mode.
#[tokio::test]
async fn runtime_full_access_executes_prompt_stdio_mcp_action() {
    let mut service = test_runtime_service();
    service.permission_policy_mut().approval_policy = ApprovalPolicy::FullAccess;
    let script = runtime_mcp_fixture_script(false);
    service
        .replace_config_layers_async(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [\"-c\", {}]\napproval = \"prompt\"\ntool_timeout_ms = 1000\n",
                toml_string(&script)
            ),
        }])
        .await
        .unwrap();
    service.permission_policy_mut().approval_policy = ApprovalPolicy::FullAccess;
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-full-access","input":"call @fixture echo tool"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "calling mcp".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "m1".to_string(),
                    rationale: "call mcp".to_string(),
                    payload: mez_agent::AgentActionPayload::McpCall {
                        server: "fixture".to_string(),
                        tool: "echo".to_string(),
                        arguments_json: r#"{"message":"hello"}"#.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider_async(
            "turn-1",
            &provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert!(service.blocked_approvals().pending().is_empty());
    assert!(
        execution.action_results[0]
            .content_text()
            .contains("hello from mcp")
    );
}

/// Verifies runtime nonfinal mcp action queues provider continuation.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn runtime_nonfinal_mcp_action_queues_provider_continuation() {
    let mut service = test_runtime_service();
    let script = runtime_mcp_fixture_script(false);
    service
        .replace_config_layers_async(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [\"-c\", {}]\napproval = \"allow\"\ntool_timeout_ms = 1000\n",
                toml_string(&script)
            ),
        }])
        .await
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
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-nonfinal","input":"call @fixture echo and continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "calling mcp".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "m1".to_string(),
                    rationale: "call mcp".to_string(),
                    payload: mez_agent::AgentActionPayload::McpCall {
                        server: "fixture".to_string(),
                        tool: "echo".to_string(),
                        arguments_json: r#"{"message":"hello"}"#.to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider_async(
            "turn-1",
            &first_provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn_id, "turn-1");

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
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
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("[action_result m1 mcp_call succeeded]")
    }));
}
