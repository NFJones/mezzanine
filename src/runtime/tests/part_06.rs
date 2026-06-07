/// Verifies that batch-level MAAP validation failures are recorded as failed
/// agent turns with the provider's raw text and the validation diagnostic in
/// the persisted assistant transcript entry. This guards against treating
/// malformed provider output as an opaque provider failure after a response has
/// already been parsed into a `ModelResponse`.
#[test]
fn runtime_maap_validation_failure_persists_provider_response_detail() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-maap-validation-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-maap-validation-audit");
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
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "state",
            "state",
            "mcp-state",
            Vec::new(),
        ))
        .unwrap();
    service
        .mcp_registry_mut()
        .mark_available(
            "state",
            vec![crate::mcp::McpToolState {
                server_id: String::new(),
                name: "list".to_string(),
                available: true,
                blacklisted: false,
                permission_required: false,
                effects: crate::mcp::McpToolEffects::none(),
                approval: crate::mcp::McpApprovalSetting::Allow,
                description: "list state".to_string(),
                input_schema_json: "{}".to_string(),
            }],
        )
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-maap-validation-fail","input":"call unavailable tool"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "bad maap action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "mcp-1".to_string(),
                    rationale: "call missing tool".to_string(),
                    payload: crate::agent::AgentActionPayload::McpCall {
                        server: "missing".to_string(),
                        tool: "read".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
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
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.role == crate::transcript::TranscriptRole::Assistant
            && entry.content.contains("bad maap action")
            && entry.content.contains("maap_validation_error")
            && entry.content.contains("unavailable server")
    }));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    assert!(audit.contains(r#""provider_raw_text_bytes":"#), "{audit}");
    assert!(audit.contains(r#""provider_failure_json":"#), "{audit}");
    let failed_audit_record = audit
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|record| record["outcome"] == "failed")
        .unwrap();
    let failure_json = failed_audit_record["metadata"]["provider_failure_json"]
        .as_str()
        .unwrap();
    let failure: serde_json::Value = serde_json::from_str(failure_json).unwrap();
    assert_eq!(failure["type"], "agent_turn_execution_failure");
    assert_eq!(failure["stage"], "maap_validation");
    assert_eq!(failure["response"]["action_batch_present"], true);
    assert_eq!(failure["response"]["action_count"], 1);
    assert!(
        failure["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unavailable server")),
        "{failure}"
    );
    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-turn","input":"call echo tool"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "calling mcp".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "m1".to_string(),
                    rationale: "call mcp".to_string(),
                    payload: crate::agent::AgentActionPayload::McpCall {
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
    assert!(execution.request.messages.iter().any(|message| {
        message.source == ContextSourceKind::Configuration
            && message.content.contains("[mcp integrations]")
            && message.content.contains("available_tool=fixture/echo")
    }));
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
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies runtime memory actions append audit records that name the action
/// and preserve compact argument metadata without storing raw freeform text.
///
/// This regression keeps memory search and store behavior aligned with other
/// runtime-owned action families so operators can diagnose what executed from
/// the audit log alone.
#[test]
fn runtime_executes_memory_actions_and_audits_action_arguments() {
    let mut service = test_runtime_service();
    let audit_root = temp_root("runtime-memory-audit");
    let audit_path = audit_root.join("audit.jsonl");
    let config_root = temp_root("runtime-memory-action-config");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    service.set_config_root(config_root.clone());
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
    let store = crate::memory::PersistentMemoryStore::under_config_root(&config_root);
    store
        .upsert(crate::memory::MemoryRecord::new_with_defaults(
            "seed-memory".to_string(),
            crate::memory::MemoryScope::Project {
                root: crate::project::discover_project_root(&std::env::current_dir().unwrap())
                    .to_string_lossy()
                    .into_owned(),
            },
            1,
            1,
            crate::memory::MemorySource::User,
            50,
            "prompt cache details".to_string(),
        ))
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-memory-turn","input":"use memory"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let search = crate::agent::AgentAction {
        id: "mem-search".to_string(),
        rationale: "search memory".to_string(),
        payload: crate::agent::AgentActionPayload::MemorySearch {
            query: "prompt cache".to_string(),
            limit: Some(3),
        },
    };
    let store_action = crate::agent::AgentAction {
        id: "mem-store".to_string(),
        rationale: "store memory".to_string(),
        payload: crate::agent::AgentActionPayload::MemoryStore {
            kind: "preference".to_string(),
            priority: Some(80),
            scope: Some("project".to_string()),
            keywords: vec!["prompt".to_string(), "cache".to_string()],
            content: "remember prompt cache".to_string(),
            expires_in_days: Some(7),
        },
    };
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "using memory".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![search.clone(), store_action.clone()],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![
            crate::agent::ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                action_id: search.id.clone(),
                action_type: "memory_search",
                status: ActionStatus::Running,
                content: Vec::new(),
                structured_content_json: None,
                is_error: false,
                error: None,
            },
            crate::agent::ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                action_id: store_action.id.clone(),
                action_type: "memory_store",
                status: ActionStatus::Running,
                content: Vec::new(),
                structured_content_json: None,
                is_error: false,
                error: None,
            },
        ],
        final_turn: true,
        terminal_state: AgentTurnState::Running,
    };

    service
        .execute_running_memory_actions_for_turn(&turn, &mut execution)
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert_eq!(execution.action_results[1].status, ActionStatus::Succeeded);

    let records = fs::read_to_string(&audit_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|record| record["action"] == "runtime_memory_action")
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2, "{records:?}");

    let search_record = records
        .iter()
        .find(|record| record["metadata"]["action_id"] == "mem-search")
        .unwrap();
    assert_eq!(search_record["metadata"]["action_type"], "memory_search");
    assert_eq!(search_record["metadata"]["limit"], "3");
    assert_eq!(search_record["metadata"]["query_bytes"], "12");
    assert!(search_record["metadata"].get("query_sha256").is_some());

    let store_record = records
        .iter()
        .find(|record| record["metadata"]["action_id"] == "mem-store")
        .unwrap();
    assert_eq!(store_record["metadata"]["action_type"], "memory_store");
    assert_eq!(store_record["metadata"]["kind"], "preference");
    assert_eq!(store_record["metadata"]["priority"], "80");
    assert_eq!(store_record["metadata"]["scope"], "project");
    assert_eq!(store_record["metadata"]["keyword_count"], "2");
    assert_eq!(store_record["metadata"]["content_bytes"], "21");
    assert_eq!(store_record["metadata"]["expires_in_days"], "7");
    assert!(store_record["metadata"].get("content_sha256").is_some());
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-full-access","input":"call echo tool"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "calling mcp".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "m1".to_string(),
                    rationale: "call mcp".to_string(),
                    payload: crate::agent::AgentActionPayload::McpCall {
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

/// Runs the execute runtime send message action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn execute_runtime_send_message_action(
    content_type: &str,
    payload: &str,
) -> (
    RuntimeSessionService,
    crate::agent::AgentTurnExecution,
    AgentId,
) {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let target_agent = AgentId::opaque("agent-%2").unwrap();
    service
        .message_service_mut()
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: target_agent.clone(),
                pane_id: None,
                window_id: None,
                role: Some("worker".to_string()),
                capabilities: Vec::new(),
            },
            0,
        )
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service.pane_screens.get_mut("%1").unwrap().feed(b"ready\n");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-message-turn","input":"send local message"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "send message".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "msg-1".to_string(),
                    rationale: "coordinate with another local agent".to_string(),
                    payload: crate::agent::AgentActionPayload::SendMessage {
                        recipient: "agent:agent-%2".to_string(),
                        content_type: content_type.to_string(),
                        payload: payload.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
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
        .unwrap();

    (service, execution, target_agent)
}

/// Verifies that MAAP `send_message` still reaches the shared message queue
/// when its media metadata is valid. This protects the accepted text path while
/// invalid media handling is tightened to match MMP transport validation.
#[test]
fn runtime_executes_send_message_action_through_message_service() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("text/plain; charset=utf-8", "hello worker");

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap_or_default()
            .contains(r#""delivery_status":"accepted""#)
    );
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "text/plain; charset=utf-8");
    assert_eq!(messages[0].payload, "hello worker");
}

/// Verifies that MAAP `send_message` canonicalizes the common model-emitted
/// `text/plain` shorthand before MMP delivery. The transport endpoint remains
/// strict, but model-produced coordination messages should not fail a subagent
/// turn when the payload is otherwise valid UTF-8 text.
#[test]
fn runtime_canonicalizes_send_message_text_plain_alias() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("text/plain", "hello worker");

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "text/plain; charset=utf-8");
    assert_eq!(messages[0].payload, "hello worker");
}

/// Verifies that MAAP `send_message` uses the same text, JSON, and binary
/// payload metadata validation as the MMP transport endpoint. Rejected actions
/// must not enqueue messages because the agent-facing action result is the
/// durable protocol feedback for the failed local delivery.
#[test]
fn runtime_rejects_send_message_action_with_invalid_mmp_payload_metadata() {
    let cases = [
        (
            "text/markdown",
            "hello worker",
            "MMP text payloads require content_type text/plain; charset=utf-8",
        ),
        (
            "application/json",
            "not-json",
            "MMP JSON payload must be valid JSON",
        ),
        (
            "application/octet-stream",
            "AQID",
            "MMP binary payloads require payload_encoding base64",
        ),
    ];

    for (content_type, payload, expected_message) in cases {
        let (service, execution, target_agent) =
            execute_runtime_send_message_action(content_type, payload);

        assert_eq!(execution.terminal_state, AgentTurnState::Running);
        let result = &execution.action_results[0];
        assert_eq!(result.status, ActionStatus::Failed);
        assert!(result.is_error);
        assert_eq!(
            result.error.as_ref().map(|error| error.code.as_str()),
            Some("invalid_message_payload")
        );
        assert_eq!(
            result.error.as_ref().map(|error| error.message.as_str()),
            Some(expected_message)
        );
        let structured = result.structured_content_json.as_deref().unwrap();
        assert!(structured.contains(r#""delivery_status":"rejected""#));
        assert!(structured.contains(r#""code":"invalid_params""#));
        assert!(structured.contains(expected_message), "{structured}");
        assert!(
            service
                .message_service()
                .receive_for(&target_agent, u64::MAX)
                .is_empty()
        );
        assert!(
            service
                .pending_agent_provider_tasks()
                .iter()
                .any(|task| task.turn_id == "turn-1")
        );
        let context = service.agent_turn_contexts.get("turn-1").unwrap();
        assert!(context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::ActionResult
                && block
                    .content
                    .contains("[action_result msg-1 send_message failed]")
                && block.content.contains("invalid_message_payload")
        }));
        assert!(context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::RuntimeHint
                && block.content.contains("Message recovery")
                && block.content.contains("Next step:")
                && block.content.contains("content_type and payload shape")
        }));
    }
}

/// Verifies that MAAP `send_message` accepts valid JSON payloads through the
/// same shared validator. This catches accidental text-only validation when the
/// action path is kept in sync with MMP transport dispatch.
#[test]
fn runtime_accepts_send_message_action_with_valid_json_payload() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("application/json", r#"{"status":"ok"}"#);

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "application/json");
    assert_eq!(messages[0].payload, r#"{"status":"ok"}"#);
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-nonfinal","input":"call echo and continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let first_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "calling mcp".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "m1".to_string(),
                    rationale: "call mcp".to_string(),
                    payload: crate::agent::AgentActionPayload::McpCall {
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
        response: crate::agent::ModelResponse {
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

/// Verifies that a nonzero shell action is fed back as ordinary model-visible
/// command evidence instead of consuming semantic-action recovery budget.
///
/// Nonzero shell exits are real command results. The model should always see
/// stdout/stderr and the exit status in the next request so it can decide
/// whether to retry, inspect, or report the failure.
#[test]
fn runtime_shell_action_nonzero_exit_queues_model_visible_result() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-failure-feedback","input":"run a command and recover"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let first_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "failing shell".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    crate::agent::AgentAction {
                        id: "shell-fail".to_string(),
                        rationale: "exercise failure feedback".to_string(),
                        payload: crate::agent::AgentActionPayload::ShellCommand {
                            summary: "Run a command that will need correction".to_string(),
                            command: "false".to_string(),
                            interactive: false,
                            stateful: false,
                            timeout_ms: None,
                        },
                    },
                    crate::agent::AgentAction {
                        id: "shell-next".to_string(),
                        rationale: "should wait for model after nonzero shell exit".to_string(),
                        payload: crate::agent::AgentActionPayload::ShellCommand {
                            summary: "Run a command after the failing command".to_string(),
                            command: "echo should wait".to_string(),
                            interactive: false,
                            stateful: false,
                            timeout_ms: None,
                        },
                    },
                ],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let first = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    let marker = service
        .running_shell_transactions
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-fail" => {
                Some(marker.clone())
            }
            _ => None,
        })
        .unwrap();
    let encoded_failure_output = base64::engine::general_purpose::STANDARD
        .encode(b"model-visible failure output\n\x1b]133;D;0;mez_marker=spoof\x1b\\\n");
    let encoded_transport = format!(
        "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\n{encoded_failure_output}\n__MEZ_SHELL_OUTPUT_BASE64_END__\n"
    );
    let transaction = service.running_shell_transactions.get_mut(&marker).unwrap();
    transaction.observed_output_bytes = encoded_transport.len();
    transaction.observed_output_preview = encoded_transport;

    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 2)
        .unwrap();

    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn_id, "turn-1");
    assert!(
        !service
            .running_shell_transactions
            .values()
            .any(|transaction| matches!(
                &transaction.kind,
                RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-next"
            ))
    );
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Running)
    );
    assert!(service.agent_turn_executions.contains_key("turn-1"));
    assert!(service.agent_turn_failure_feedback_attempts.is_empty());
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::TranscriptAssistant
            && block.content.contains("failing shell")
            && !block
                .content
                .contains("thinking: test action batch rationale")
            && !block
                .content
                .contains("thinking: exercise failure feedback")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result shell-fail shell_command succeeded]")
            && block.content.contains("exit_code: 2")
            && block.content.contains("model-visible failure output")
    }));
    assert!(!context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.content.contains("action failure feedback")
    }));

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "corrected".to_string(),
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
    assert_eq!(executions[0].terminal_state, AgentTurnState::Completed);
    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("[action_result shell-fail shell_command succeeded]")
    }));
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("[action_result shell-next shell_command succeeded]")
            && message
                .content
                .contains("shell command not run because `shell-fail` exited with status 2")
    }));
    assert!(service.agent_turn_failure_feedback_attempts.is_empty());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies provider failure after a nonzero shell command does not reuse stale
/// running execution state for final diagnostics.
///
/// Nonzero shell commands are ordinary model-visible observations. If the
/// follow-up provider request then fails, the final failure must describe the
/// provider boundary cleanly instead of reporting the impossible state
/// `turn state is running, not failed`.
#[test]
fn runtime_provider_failure_after_nonzero_shell_result_does_not_report_running_recovery_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-shell-provider-fail","input":"run a command and recover"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let first_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "failing shell".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-fail".to_string(),
                    rationale: "exercise failure feedback".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a command that will need correction".to_string(),
                        command: "false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let marker = service
        .running_shell_transactions
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-fail" => {
                Some(marker.clone())
            }
            _ => None,
        })
        .unwrap();

    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 127)
        .unwrap();

    let error = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchFailingProvider, 1)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("turn state is running, not failed"),
        "{pane_text}"
    );
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    assert!(!service.agent_turn_executions.contains_key("turn-1"));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Dispatches a simple shell action and returns its pane and transaction marker.
///
/// Protocol-invariant tests need a real runtime-owned shell transaction so the
/// strict start-marker bookkeeping is populated the same way it is in normal
/// agent execution.
fn dispatch_protocol_test_shell_action(
    service: &mut RuntimeSessionService,
    primary: &crate::ids::ClientId,
    action_id: &str,
) -> (String, String) {
    mark_test_pane_ready(service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{{"idempotency_key":"agent-protocol-{action_id}","input":"run a shell command"}}}}"#
        ),
        primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: action_id.to_string(),
                    rationale: "run a shell command".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a command".to_string(),
                        command: "true".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
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
    let marker = service
        .running_shell_transactions
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction {
                action_id: candidate,
            } if candidate == action_id => Some(marker.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        service
            .shell_transaction_require_start_markers
            .contains(&marker)
    );
    ("%1".to_string(), marker)
}

/// Verifies mismatched shell-transaction markers fail the live action promptly.
///
/// A terminal OSC marker can be malformed, delayed, or spoofed. The runtime must
/// validate marker metadata against the retained transaction state and fail the
/// action instead of leaving the turn to wait for a later timeout.
#[test]
fn runtime_shell_transaction_metadata_mismatch_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-marker-mismatch","input":"run a command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "run a shell command".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a command".to_string(),
                        command: "true".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
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
    let marker = service
        .running_shell_transactions
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-1" => {
                Some(marker.clone())
            }
            _ => None,
        })
        .unwrap();

    let observed = service
        .observe_agent_shell_transaction_end("%2", &marker, "turn-1", "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(!service.running_shell_transactions.contains_key(&marker));
    assert!(
        !service
            .shell_transaction_require_start_markers
            .contains(&marker)
    );
    assert!(!service.shell_transaction_started_markers.contains(&marker));
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text
            .contains("shell transaction marker metadata does not match runtime dispatch state"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies a duplicate start marker fails the live shell action.
///
/// The wrapper start marker is the handoff boundary for deferred command
/// payloads. Seeing it twice for one marker means the in-band control stream is
/// no longer well framed, so the action should fail instead of waiting for a
/// later timeout.
#[test]
fn runtime_shell_transaction_duplicate_start_marker_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (pane_id, marker) =
        dispatch_protocol_test_shell_action(&mut service, &primary, "shell-duplicate-start");

    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();
    assert!(service.shell_transaction_started_markers.contains(&marker));
    let observed = service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(!service.running_shell_transactions.contains_key(&marker));
    assert!(
        !service
            .shell_transaction_require_start_markers
            .contains(&marker)
    );
    assert!(!service.shell_transaction_started_markers.contains(&marker));
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("shell transaction emitted a duplicate start marker"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies an end marker before the start marker fails the live shell action.
///
/// Runtime-dispatched wrappers must emit a start marker before any end marker.
/// An end marker first means the parser missed a control boundary or command
/// output spoofed the frame, either of which should fail fast with diagnostics.
#[test]
fn runtime_shell_transaction_end_before_start_marker_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (pane_id, marker) =
        dispatch_protocol_test_shell_action(&mut service, &primary, "shell-end-before-start");

    let observed = service
        .observe_agent_shell_transaction_end(&pane_id, &marker, "turn-1", "agent-%1", &pane_id, 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(!service.running_shell_transactions.contains_key(&marker));
    assert!(
        !service
            .shell_transaction_require_start_markers
            .contains(&marker)
    );
    assert!(!service.shell_transaction_started_markers.contains(&marker));
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("shell transaction end marker arrived before the start marker"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies async pane write failures settle shell-backed file actions.
///
/// File mutations are sent through the pane shell as generated transactions. If
/// the async pane worker cannot write that transaction input, the action must
/// become a failed action result and queue model recovery instead of remaining
/// in the running-transaction table forever.
#[test]
fn runtime_pane_write_failure_fails_running_file_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-write-failure","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "write file".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "patch-fail".to_string(),
                    rationale: "write a note".to_string(),
                    payload: crate::agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Add File: note.txt\n+note\n*** End Patch"
                            .to_string(),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let first = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    assert_eq!(service.drain_deferred_pane_inputs().len(), 1);
    assert!(
        service
            .running_shell_transactions
            .values()
            .any(|transaction| matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "patch-fail"
            ))
    );

    assert!(
        service
            .apply_pane_write_failure_event(&pane_id, "synthetic PTY write failure")
            .unwrap()
    );

    assert!(
        service
            .running_shell_transactions
            .values()
            .all(|transaction| !matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { .. }
            ))
    );
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    assert!(!service.agent_turn_executions.contains_key("turn-1"));
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-fail apply_patch failed]")
            && block.content.contains("pane input write failed")
    }));

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies shell transaction payload bytes are deferred until the wrapper
/// receiver emits its start marker.
///
/// Large generated file-action scripts must not be sent as part of the initial
/// shell wrapper. Waiting for the start marker proves the shell has reached the
/// read loop that treats following bytes as payload data instead of shell
/// source.
#[test]
fn runtime_shell_transaction_start_streams_deferred_payload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-stream-payload","input":"run command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-stream".to_string(),
                    rationale: "run payload command".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run payload command".to_string(),
                        command: "printf '%s\\n' payload-marker".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let deferred_wrapper = service.drain_deferred_pane_inputs();
    assert_eq!(deferred_wrapper.len(), 1);
    let wrapper_text = String::from_utf8_lossy(&deferred_wrapper[0].bytes);
    assert!(wrapper_text.contains("__mez_tx_"), "{wrapper_text}");
    assert!(!wrapper_text.contains("payload-marker"), "{wrapper_text}");
    let (marker, transaction) = service
        .running_shell_transactions
        .iter()
        .find(|(_, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "shell-stream"
            )
        })
        .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
        .unwrap();
    assert!(transaction.pending_input_payload.is_some());

    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();

    let deferred_payload = service.drain_deferred_pane_inputs();
    assert_eq!(deferred_payload.len(), 1);
    let payload_text = String::from_utf8_lossy(&deferred_payload[0].bytes);
    let encoded = payload_text
        .lines()
        .take_while(|line| !line.starts_with("__MEZ_COMMAND_PAYLOAD_END_"))
        .collect::<String>();
    let decoded = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert!(decoded.contains("payload-marker"), "{decoded}");
    assert!(
        service
            .running_shell_transactions
            .get(&marker)
            .unwrap()
            .pending_input_payload
            .is_none()
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies pending payload handoff uses a short start-marker deadline.
///
/// Non-stateful shell actions wait for an OSC start marker before sending the
/// encoded command body. If that marker is lost or the wrapper never reaches
/// the receiver loop, the transaction should time out quickly instead of
/// occupying the pane until the full command timeout expires.
#[test]
fn runtime_shell_transaction_pending_payload_uses_short_start_timer() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1".to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-start".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-1".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "grep -n needle file.txt".to_string(),
            started_at_unix_ms: 1_000,
            timeout_ms: Some(10 * 60 * 1000),
            pending_input_payload: Some(b"payload\n".to_vec()),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let timer = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == "marker-start")
        .unwrap();

    assert_eq!(timer.timeout_ms, 30_000);

    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            "marker-start",
            "turn-1",
            "agent-%1",
            &pane_id,
        )
        .unwrap();
    let timer = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == "marker-start")
        .unwrap();
    assert_eq!(timer.timeout_ms, 10 * 60 * 1000);
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies runtime shell dispatch honors per-action shell timeouts.
///
/// The MAAP parser and semantic lowering preserve `timeout_ms`; the runtime
/// must carry that bound into the live shell transaction instead of replacing it
/// with the enclosing turn's full timeout budget.
#[test]
fn runtime_shell_command_dispatch_uses_action_timeout() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-timeout","input":"run bounded grep"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-timeout".to_string(),
                    rationale: "run a bounded command".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run bounded grep".to_string(),
                        command: "grep -n needle file.txt".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(1500),
                    },
                }],
                final_turn: false,
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
    let transaction = service
        .running_shell_transactions
        .values()
        .find(|transaction| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "shell-timeout"
            )
        })
        .unwrap();

    assert_eq!(transaction.timeout_ms, Some(1500));
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies timed-out shell actions receive bounded model recovery.
///
/// A file mutation can time out if the pane PTY stops accepting the generated
/// shell transaction. Treating timeout action results as non-recoverable leaves
/// the turn failed even though the model can choose a smaller or different
/// mutation strategy after seeing the timeout diagnostic.
#[test]
fn runtime_shell_action_timeout_queues_model_self_correction() {
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
        .start_agent_prompt_turn("%1", "write a file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = crate::agent::AgentAction {
        id: "patch-timeout".to_string(),
        rationale: "write a file through the pane shell".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: note.txt\n+hello\n*** End Patch".to_string(),
            strip: None,
        },
    };
    let timed_out = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::TimedOut,
        "shell_timeout",
        "shell command timed out after 30000 ms",
    )
    .unwrap();
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "write file timed out".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
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
        action_results: vec![timed_out],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "shell_timeout_recovery",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-timeout apply_patch timed_out]")
            && block
                .content
                .contains("shell command timed out after 30000 ms")
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies an `apply_patch` validation failure is eligible for model
/// correction.
///
/// Malformed Mezzanine patch payloads are model-correctable input errors and
/// must not end the turn before the model sees the failed action result.
#[test]
fn runtime_apply_patch_invalid_params_queues_model_self_correction() {
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
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = crate::agent::AgentAction {
        id: "patch-invalid".to_string(),
        rationale: "apply an invalid patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch".to_string(),
            strip: None,
        },
    };

    let mut failed = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "invalid_params",
        "apply_patch requires Mezzanine patch blocks starting with *** Begin Patch; use shell_command with git apply for raw unified diffs",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "state": "dispatch_failed",
            "stage": "local_action_plan",
            "error": {
                "kind": "invalid_params",
                "message": "apply_patch requires Mezzanine patch blocks starting with *** Begin Patch; use shell_command with git apply for raw unified diffs"
            }
        })
        .to_string(),
    );
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "invalid patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
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
            "apply_patch_validation_failed",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    assert_eq!(
        service
            .agent_turn_failure_feedback_attempts
            .values()
            .copied()
            .collect::<Vec<_>>(),
        vec![1]
    );
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-invalid apply_patch failed]")
            && block.content.contains("Mezzanine patch blocks starting")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.content.contains("action failure feedback")
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
    assert!(!pane_text.contains("Failed after"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
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

    let action = crate::agent::AgentAction {
        id: "shell-not-ready".to_string(),
        rationale: "inspect the render owner".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the render owner.".to_string(),
            command: "rg -n \"status pager\" src".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut failed = crate::agent::ActionResult::failed(
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
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "pane not ready".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
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

/// Verifies a pre-dispatch pane-readiness failure stops the current shell batch
/// after the first failed action.
///
/// Later shell siblings were never sent to the pane, so the runtime should
/// preserve them as untouched running siblings for same-turn correction rather
/// than failing every remaining shell action with the same readiness error.
#[test]
fn runtime_pane_not_ready_stops_shell_batch_after_first_failure() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);
    let turn = crate::agent::AgentTurnRecord {
        turn_id: "turn-pane-not-ready".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "runtime".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        state: AgentTurnState::Running,
    };
    service.agent_turn_ledger.start_turn(turn.clone()).unwrap();
    let first = crate::agent::AgentAction {
        id: "shell-a".to_string(),
        rationale: "inspect owner one".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Inspect owner one.".to_string(),
            command: "rg -n \"status pager\" src".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let second = crate::agent::AgentAction {
        id: "shell-b".to_string(),
        rationale: "inspect owner two".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Inspect owner two.".to_string(),
            command: "sed -n '1,120p' src/runtime/render/mod.rs".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "inspect with shell".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![first.clone(), second.clone()],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![
            crate::agent::ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                action_id: first.id.clone(),
                action_type: "shell_command",
                status: ActionStatus::Running,
                content: Vec::new(),
                structured_content_json: None,
                is_error: false,
                error: None,
            },
            crate::agent::ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                action_id: second.id.clone(),
                action_type: "shell_command",
                status: ActionStatus::Running,
                content: Vec::new(),
                structured_content_json: None,
                is_error: false,
                error: None,
            },
        ],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    service
        .agent_turn_executions
        .insert(turn.turn_id.clone(), execution);
    service.agent_turn_contexts.insert(
        turn.turn_id.clone(),
        crate::agent::AgentContext::new(vec![crate::agent::ContextBlock {
            source: ContextSourceKind::Configuration,
            label: "test context".to_string(),
            content: "present".to_string(),
        }])
        .unwrap(),
    );
    let execution = service
        .dispatch_stored_running_shell_actions(&turn.turn_id)
        .unwrap()
        .expect("execution should still be present");

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(
        execution.action_results[0]
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("pane_not_ready")
    );
    assert_eq!(execution.action_results[1].status, ActionStatus::Running);
    assert!(!execution.action_results[1].is_error);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: pane %1 is not ready for agent shell input: interactive-blocked"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("Inspect owner two."), "{pane_text}");
}

/// Verifies pre-execution `apply_patch` transport failures are model
/// correctable.
///
/// A pane input write timeout means the runtime could not deliver the generated
/// write command, not that the user request is impossible. The model should
/// receive bounded correction feedback so it can retry with a smaller or
/// different file action instead of failing through immediately.
#[test]
fn runtime_apply_patch_pane_input_failure_queues_model_self_correction() {
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
        .start_agent_prompt_turn("%1", "write the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = crate::agent::AgentAction {
        id: "patch-transport".to_string(),
        rationale: "write a source file".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: src/generated.rs\n+content\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let failed = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "pane_input_write_failed",
        "pane input write failed while sending shell action",
    )
    .unwrap();
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "write transport failure".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
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
            "apply_patch_transport_failed",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-transport apply_patch failed]")
            && block.content.contains("pane_input_write_failed")
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `apply_patch` hunk mismatches receive specific recovery guidance.
///
/// A generic "action failed" continuation is not enough for patch hunk
/// mismatches because replaying the same patch will deterministically fail.
/// The model should be steered to inspect the current file and generate a fresh
/// Mezzanine patch block instead.
#[test]
fn runtime_apply_patch_hunk_mismatch_recovery_guides_context_refresh() {
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
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = crate::agent::AgentAction {
        id: "patch-hunk".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch:
                "*** Begin Patch\n*** Update File: src/driver/mod.rs\n@@\n-old\n+new\n*** End Patch"
                    .to_string(),
            strip: None,
        },
    };
    let mut failed = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "command": "\"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\" \"$MEZ_PATCH\"",
            "terminal_observation": {
                "exit_code": 1,
                "combined_output_preview": "apply_patch: hunk did not match: src/driver/mod.rs\napply_patch: patch failed",
                "combined_output_bytes": 91,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "hunk mismatch patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
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
            "apply_patch_hunk_mismatch",
        )
        .unwrap();

    assert!(queued);
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    let feedback = context
        .blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::RuntimeHint
                && block.label == "action failure feedback"
        })
        .expect("feedback block should be present");
    assert!(!feedback.content.contains("attempt="), "{}", feedback.content);
    assert!(
        feedback.content.contains("Mutation-evidence rule"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("no successful mutation has occurred"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("Reads, git status, and git diff after a failed mutation"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("the current file/diff shows that state"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("Do not retry substantially the same patch"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("Next step: first inspect the affected path(s) with a bounded shell_command"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("reported line number(s)"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("not necessarily a stale-file condition"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("fresh Mezzanine"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("emit a smaller fresh Mezzanine"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("src/driver/mod.rs"),
        "{}",
        feedback.content
    );
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("apply_patch: hunk did not match: src/driver/mod.rs")
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
    assert!(pane_text.contains("(patch hunk mismatch)"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies real `apply_patch` write-phase hunk failures enter model recovery.
///
/// `apply_patch` runs through a read transaction followed by a generated write
/// transaction. Direct recovery-unit tests do not prove the shell-transaction
/// observer routes write-phase hunk mismatches back into the correction loop,
/// so this covers the user-visible path that emits the final patch diagnostic.
#[test]
fn runtime_apply_patch_write_phase_hunk_mismatch_queues_model_recovery() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-patch-write-phase-recovery","input":"patch the file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "patch response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "patch-write".to_string(),
                    rationale: "apply a source patch".to_string(),
                    payload: crate::agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Update File: tests/standard_config_consumer_test.rs\n@@\n-old\n+new\n*** End Patch"
                            .to_string(),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(service.running_shell_transactions.len(), 1);
    let marker = service
        .running_shell_transactions
        .keys()
        .next()
        .cloned()
        .unwrap();
    let transaction = service.running_shell_transactions.get_mut(&marker).unwrap();
    transaction.command = "# __MEZ_APPLY_PATCH_WRITE_PHASE__".to_string();
    transaction.observed_output_preview =
        "apply_patch: hunk did not match: tests/standard_config_consumer_test.rs\n\
         apply_patch: exact hunk context was not found in the current file"
            .to_string();
    transaction.observed_output_bytes = transaction.observed_output_preview.len();

    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 1)
        .unwrap();

    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Running)
    );
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    let feedback = context
        .blocks
        .iter()
        .rev()
        .find(|block| {
            block.source == ContextSourceKind::RuntimeHint
                && block.label == "action failure feedback"
        })
        .expect("feedback block should be present");
    assert!(
        feedback.content.contains("Apply-patch recovery"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("tests/standard_config_consumer_test.rs"),
        "{}",
        feedback.content
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    assert!(pane_text.contains("(patch hunk mismatch)"), "{pane_text}");
    assert!(!pane_text.contains("recovery unavailable"), "{pane_text}");
    let copy_response = service
        .execute_agent_shell_command(&primary, "/copy-patches buffer failed-patches")
        .unwrap();
    assert!(
        copy_response.contains(r#""command":"copy-patches""#),
        "{copy_response}"
    );
    assert!(copy_response.contains("patches=written"), "{copy_response}");
    assert!(
        copy_response.contains("destination=buffer"),
        "{copy_response}"
    );
    let failed_patches = service.paste_buffers.get("failed-patches").unwrap();
    assert!(
        failed_patches.contains("patch 1: turn=turn-1 action=patch-write status=failed"),
        "{failed_patches}"
    );
    assert!(
        failed_patches
            .contains("apply_patch: hunk did not match: tests/standard_config_consumer_test.rs"),
        "{failed_patches}"
    );
    assert!(
        failed_patches.contains("*** Update File: tests/standard_config_consumer_test.rs"),
        "{failed_patches}"
    );
    assert!(failed_patches.contains("-old"), "{failed_patches}");
    assert!(failed_patches.contains("+new"), "{failed_patches}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies repeated identical `apply_patch` hunk mismatches stay unbounded
/// and omit retry-budget noise.
///
/// Provider wording and generated action ids can vary while the model repeats
/// the same bad patch. `apply_patch` recovery should still track repeated
/// identical failures for guidance, but it must not consume the generic
/// bounded retry budget or surface `(attempt/max)` status text.
#[test]
fn runtime_apply_patch_hunk_mismatch_recovery_is_unbounded_and_hides_retry_budget() {
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
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let build_execution = |raw_text: &str, action_id: &str| {
        let action = crate::agent::AgentAction {
            id: action_id.to_string(),
            rationale: "apply a source patch".to_string(),
            payload: crate::agent::AgentActionPayload::ApplyPatch {
                patch:
                    "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch"
                        .to_string(),
                strip: None,
            },
        };
        let mut failed = crate::agent::ActionResult::failed(
            &turn,
            &action,
            ActionStatus::Failed,
            "shell_command_failed",
            "shell command exited with status 1",
        )
        .unwrap();
        failed.structured_content_json = Some(
            serde_json::json!({
                "command": "apply_patch",
                "terminal_observation": {
                    "exit_code": 1,
                    "combined_output_preview": "apply_patch: hunk did not match: src/main.rs\napply_patch: patch failed",
                    "combined_output_bytes": 75,
                    "output_truncated": false
                }
            })
            .to_string(),
        );
        crate::agent::AgentTurnExecution {
            request: runtime_model_request_fixture(&turn.turn_id),
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: raw_text.to_string(),
                usage: Default::default(),
            latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
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
        }
    };

    for index in 0..8 {
        let action_id = format!("patch-{index}");
        let raw_text = if index % 2 == 0 {
            "first provider wording"
        } else {
            "different provider wording"
        };
        let mut execution = build_execution(raw_text, &action_id);
        assert!(
            service
                .queue_agent_failure_feedback_for_correction(
                    &turn,
                    &mut execution,
                    "apply_patch_hunk_mismatch",
                )
                .unwrap()
        );
    }

    assert_eq!(
        service
            .agent_turn_failure_feedback_attempts
            .values()
            .copied()
            .collect::<Vec<_>>(),
        vec![8]
    );
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    let feedback = context
        .blocks
        .iter()
        .rev()
        .find(|block| {
            block.source == ContextSourceKind::RuntimeHint
                && block.label == "action failure feedback"
        })
        .expect("second feedback block should be present");
    assert!(
        !feedback.content.contains("attempt="),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("Repeated apply-patch recovery: the same failure signature repeated."),
        "{}",
        feedback.content
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover (patch hunk mismatch)"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("/5"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies unsafe `apply_patch` paths receive CWD-relative recovery guidance.
///
/// Mezzanine patch headers are intentionally restricted to paths relative to
/// the pane current working directory. When a model emits an absolute path, the
/// corrective continuation should include the rejected path, the best-known CWD,
/// and a clear note that this restriction is specific to `apply_patch` headers.
#[test]
fn runtime_apply_patch_unsafe_path_recovery_guides_relative_headers() {
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
    service.pane_current_working_directories.insert(
        "%1".to_string(),
        PathBuf::from("/home/neil/Documents/repos/chimera"),
    );
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let unsafe_path = "/home/neil/Documents/repos/chimera/src/conf/document.rs";
    let action = crate::agent::AgentAction {
        id: "patch-absolute".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: format!(
                "*** Begin Patch\n*** Update File: {unsafe_path}\n@@\n-old\n+new\n*** End Patch"
            ),
            strip: None,
        },
    };
    let mut failed = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "command": "\"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\" \"$MEZ_PATCH\"",
            "terminal_observation": {
                "exit_code": 1,
                "combined_output_preview": format!("apply_patch: unsafe patch path: {unsafe_path}\n"),
                "combined_output_bytes": 96,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "absolute path patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
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
            "apply_patch_unsafe_path",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    let feedback = context
        .blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::RuntimeHint
                && block.label == "action failure feedback"
        })
        .expect("feedback block should be present");
    assert!(
        feedback.content.contains("unsafe patch path"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains(unsafe_path),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("Current pane working directory: /home/neil/Documents/repos/chimera"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("relative to the pane current working directory"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("`src/conf/document.rs`"),
        "{}",
        feedback.content
    );
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("apply_patch: unsafe patch path: /home/neil/Documents/repos/chimera/src/conf/document.rs")
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies agent-authored heredoc shell commands fail before pane dispatch.
///
/// MAAP validation rejects heredocs before runtime execution. This protects the
/// pane from receiving an unterminated shell construct and ensures that a fixed
/// provider response surfaces a repairable diagnostic instead of attempting to
/// execute the invalid command.
#[test]
fn runtime_shell_command_heredoc_is_rejected_before_pane_dispatch() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-heredoc-feedback","input":"write a file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "heredoc shell".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-heredoc".to_string(),
                    rationale: "write a file with a heredoc".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Write a file with a heredoc".to_string(),
                        command: "cat > /tmp/mez-heredoc.rs <<'EOF'\nfn main() {}\nEOF".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(service.running_shell_transactions.is_empty());
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(
        execution
            .response
            .raw_text
            .contains("maap_validation_error"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution
            .response
            .raw_text
            .contains("heredoc redirection is disabled"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution.response.raw_text.contains("apply_patch"),
        "{}",
        execution.response.raw_text
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("MEZ_COMMAND_"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies network research failure feedback is scoped per action batch and
/// that mixed successful results are sent back with the failures.
///
/// Broken documentation links and 404s are normal web-research evidence. A
/// previous single turn-wide failure-feedback budget let an earlier bad URL
/// consume budget for a later batch of different URLs. The network budget should
/// instead be per batch and controlled by the configured action-failure limit.
#[test]
fn runtime_network_action_failures_get_additional_model_feedback_budget() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-network-failure-feedback","input":"research docs"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let success_action = crate::agent::AgentAction {
        id: "fetch-good".to_string(),
        rationale: "capture one usable source".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/ok".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let failed_action = crate::agent::AgentAction {
        id: "fetch-missing".to_string(),
        rationale: "try a moved source".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/missing".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let mut failed_result = crate::agent::ActionResult::failed(
        &turn,
        &failed_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404",
    )
    .unwrap();
    failed_result.structured_content_json = Some(
        serde_json::json!({
            "kind": "fetch_url",
            "response": {
                "url": "https://example.test/missing",
                "status_code": 404
            }
        })
        .to_string(),
    );
    let mut execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            stop: None,
            prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::NetworkFetch,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                content: "research docs".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "mixed network fetches".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![success_action.clone(), failed_action.clone()],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![
            crate::agent::ActionResult::succeeded(
                &turn,
                &success_action,
                vec!["usable source body".to_string()],
                None,
            ),
            failed_result,
        ],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };
    let previous_key = "turn-1:previous-network-batch".to_string();
    service
        .agent_turn_failure_feedback_attempts
        .insert(previous_key.clone(), 3);
    service
        .present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)
        .unwrap();
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent warning: URL fetch failed (HTTP 404)"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("model received the response detail")
            && pane_text.contains("for recovery"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("https://example.test/missing"),
        "{pane_text}"
    );

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "network_research_failed_action",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(
        service
            .agent_turn_failure_feedback_attempts
            .get(&previous_key)
            .copied(),
        Some(3)
    );
    let mut attempt_values = service
        .agent_turn_failure_feedback_attempts
        .values()
        .copied()
        .collect::<Vec<_>>();
    attempt_values.sort_unstable();
    assert_eq!(attempt_values, vec![1, 3]);
    assert!(service.pending_agent_provider_tasks.contains("turn-1"));
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result fetch-good fetch_url succeeded]")
            && block.content.contains("usable source body")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result fetch-missing fetch_url failed]")
            && block.content.contains("network request returned HTTP 404")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint && block.content.contains("attempt=1 max=5")
    }));
    assert!(context.blocks.iter().all(|block| {
        block.source != ContextSourceKind::RuntimeHint
            || !block.content.contains("Mutation-evidence rule")
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies provider-worker network results are applied without actor-side HTTP.
///
/// A large research turn can return many already-settled `fetch_url` results
/// from the async provider worker. The runtime actor must present and audit
/// those results, then queue model recovery for failed fetches, without trying
/// to run the network requests again while applying the provider completion.
#[tokio::test]
async fn runtime_provider_completion_records_preexecuted_network_results_before_recovery() {
    let mut service = test_runtime_service();
    let audit_root = temp_root("runtime-preexecuted-network-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 160)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-preexecuted-network-results","input":"research provider docs"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let success_action = crate::agent::AgentAction {
        id: "fetch-ok".to_string(),
        rationale: "fetch an available provider document".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/ok".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let failed_action = crate::agent::AgentAction {
        id: "fetch-404".to_string(),
        rationale: "fetch a provider document that moved".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/missing".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let success_result = crate::agent::ActionResult::succeeded(
        &turn,
        &success_action,
        vec!["provider document body".to_string()],
        Some(
            crate::agent::network_action_structured_content_json(
                &success_action,
                serde_json::Value::Null,
                serde_json::json!({
                    "url": "https://example.test/ok",
                    "status_code": 200,
                    "body_bytes": 22,
                    "returned_bytes": 22,
                    "requested_max_bytes": null,
                    "max_bytes": 16384,
                    "hard_max_bytes": 262144,
                    "truncated": false
                }),
            )
            .unwrap(),
        ),
    );
    let mut failed_result = crate::agent::ActionResult::failed(
        &turn,
        &failed_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404",
    )
    .unwrap();
    failed_result.structured_content_json = Some(
        crate::agent::network_action_structured_content_json(
            &failed_action,
            serde_json::Value::Null,
            serde_json::json!({
                "url": "https://example.test/missing",
                "status_code": 404,
                "body_bytes": 0
            }),
        )
        .unwrap(),
    );
    let mut request = runtime_model_request_fixture("turn-1");
    request.provider = "runtime-batch".to_string();
    request.model = "test".to_string();
    request.agent_id = turn.agent_id.clone();
    request.allowed_actions =
        crate::agent::AllowedActionSet::for_capability(crate::agent::AgentCapability::NetworkFetch);
    request.messages = vec![crate::agent::ModelMessage {
        role: crate::agent::ModelMessageRole::User,
        source: ContextSourceKind::UserInstruction,
        content: "research provider docs".to_string(),
    }];
    let execution = crate::agent::AgentTurnExecution {
        request,
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "provider docs fetch batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "fetch provider documentation sources".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![success_action, failed_action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![success_result, failed_result],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let applied = service
        .apply_agent_provider_completed_event(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();

    assert!(applied);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-1")
    );
    assert!(!service.agent_turn_executions.contains_key("turn-1"));
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content.contains("[action_result fetch-ok fetch_url succeeded]")
            && block.content.contains("provider document body")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content.contains("[action_result fetch-404 fetch_url failed]")
            && block.content.contains("network request returned HTTP 404")
    }));
    let history = service
        .agent_turn_network_action_history
        .get("turn-1")
        .unwrap();
    assert_eq!(history.requests.len(), 2);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: fetch url: https://example.test/ok"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("provider document body"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent warning: URL fetch failed (HTTP 404)"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("model received the response detail")
            && pane_text.contains("for recovery"),
        "{pane_text}"
    );
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"external_integration""#), "{audit}");
    assert!(audit.contains(r#""action_id":"fetch-ok""#), "{audit}");
    assert!(audit.contains(r#""action_id":"fetch-404""#), "{audit}");
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies failure-feedback accounting is per failed action, not per batch.
///
/// A single model response may contain multiple correctable action failures.
/// Each failed action should receive its own bounded retry counter so one bad
/// action does not amortize away another action's correction opportunity.
#[test]
fn runtime_action_failure_retry_budget_is_per_failed_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-per-action-retry","input":"research docs"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let first_action = crate::agent::AgentAction {
        id: "fetch-first".to_string(),
        rationale: "try first source".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/first".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let second_action = crate::agent::AgentAction {
        id: "fetch-second".to_string(),
        rationale: "try second source".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/second".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let first_result = crate::agent::ActionResult::failed(
        &turn,
        &first_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404 for first source",
    )
    .unwrap();
    let second_result = crate::agent::ActionResult::failed(
        &turn,
        &second_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404 for second source",
    )
    .unwrap();
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "two failed network fetches".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![first_action, second_action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![first_result, second_result],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "network_research_failed_actions",
        )
        .unwrap();

    assert!(queued);
    let mut attempt_values = service
        .agent_turn_failure_feedback_attempts
        .values()
        .copied()
        .collect::<Vec<_>>();
    attempt_values.sort_unstable();
    assert_eq!(attempt_values, vec![1, 1]);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that intentionally terminal model actions do not use the automatic
/// failure-feedback path. Cancellations and denials represent user or policy
/// boundaries rather than correctable execution evidence, so they must end the
/// turn without queuing another provider request.
#[test]
fn runtime_cancelled_action_does_not_queue_failure_feedback() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-cancel-no-feedback","input":"stop"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "abort".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "abort-1".to_string(),
                    rationale: "abort the turn".to_string(),
                    payload: crate::agent::AgentActionPayload::Abort {
                        reason: "cannot continue".to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.agent_turn_failure_feedback_attempts.is_empty());
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies unrecovered `apply_patch` failures render their captured terminal
/// diagnostic when the turn is actually ending failed.
///
/// While the model still has a recovery attempt, normal logging does not need
/// to show the patch stderr/stdout. Once recovery is unavailable or exhausted,
/// the user needs enough final context to understand why the patch action
/// failed, so the renderer should surface the bounded terminal observation
/// before the failed-turn footer.
#[test]
fn runtime_unrecovered_apply_patch_failure_logs_terminal_observation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-unrecovered-patch-failure","input":"patch the file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .expect("started turn should be recorded");
    let action = crate::agent::AgentAction {
        id: "patch-fail".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let mut result = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    result.structured_content_json = Some(
        serde_json::json!({
            "kind": "apply_patch",
            "terminal_observation": {
                "combined_output_preview": "\n\n∙ MEZ_PATCH=$(mktemp) || exit 1\n∙ printf %s '*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch' > \"$MEZ_PATCH\"\n∙ \"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\" \"$MEZ_PATCH\"\nMEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\napply_patch: hunk did not match: src/lib.rs\napply_patch: patch failed\n",
                "combined_output_bytes": 298,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "failed patch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![action],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![result],
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };
    service
        .agent_turn_executions
        .insert("turn-1".to_string(), execution);

    service
        .finish_agent_turn("%1", "turn-1", AgentTurnState::Failed)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_flat = pane_text.replace("▐ ", "").replace('\n', "");
    assert!(
        pane_text_flat.contains(
            "failed; recovery unavailable: no model-correction continuation was queued after the apply_patch failure"
        ),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("apply_patch: hunk did not match: src/lib.rs"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("MEZ_RESTORE_NOUNSET_NOW"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("[mez: failure output truncated for pane display]"),
        "{pane_text}"
    );
    assert!(pane_text.contains("Failed after"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies unrecovered failures explain when recovery is unavailable because
/// a sibling action has not settled.
///
/// The runtime cannot feed a partial batch back to the model without risking a
/// correction prompt that ignores still-running or blocked actions. The final
/// failure line should make that blocker explicit instead of using a bare
/// "recovery unavailable" suffix.
#[test]
fn runtime_unrecovered_failure_with_pending_sibling_explains_blocker() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch and inspect")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let patch_action = crate::agent::AgentAction {
        id: "patch-fail".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let read_action = crate::agent::AgentAction {
        id: "read-pending".to_string(),
        rationale: "read the target file".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Read the target file".to_string(),
            command: "sed -n '1,120p' src/lib.rs".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut failed = crate::agent::ActionResult::failed(
        &turn,
        &patch_action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "kind": "apply_patch",
            "terminal_observation": {
                "combined_output_preview": "apply_patch: hunk did not match: src/lib.rs",
                "combined_output_bytes": 44,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let pending = crate::agent::ActionResult::running(
        &turn,
        &read_action,
        vec!["local action accepted for pane execution".to_string()],
        None,
    );
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "partial batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![patch_action, read_action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![failed, pending],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };
    service
        .agent_turn_executions
        .insert(turn.turn_id.clone(), execution);

    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Failed)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("recovery unavailable: action result(s) are still pend"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("read-pending shell_command running no_error_code"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies unrecovered failures explain when the failed result is outside the
/// model-correction path.
///
/// Policy/user-boundary outcomes must not be retried by the model. The final
/// failure line should still identify the non-correctable result so the user
/// can distinguish that boundary from a missing retry loop.
#[test]
fn runtime_unrecovered_non_correctable_failure_explains_boundary() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "write the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = crate::agent::AgentAction {
        id: "patch-denied".to_string(),
        rationale: "write a source file".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let denied = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Denied,
        "approval_denied",
        "user denied the action",
    )
    .unwrap();
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "denied write".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
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
        action_results: vec![denied],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };
    service
        .agent_turn_executions
        .insert(turn.turn_id.clone(), execution);

    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Failed)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("recovery unavailable: no model-correctable"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("patch-denied apply_patch denied"),
        "{pane_text}"
    );
    assert!(pane_text.contains("approval_denied"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies unrecovered `apply_patch` failures do not expose shell-wrapper
/// fragments when no actionable diagnostic survived capture.
///
/// Some failed patch commands can echo a partially quoted generated command as
/// isolated glyphs or words after the shell wrapper has already been stripped.
/// Those fragments are confusing to users and do not help model recovery, so a
/// final failed turn should prefer a concise generic diagnostic when no real
/// `apply_patch:` or error line is available.
#[test]
fn runtime_unrecovered_apply_patch_failure_uses_generic_line_for_fragments() {
    let action = crate::agent::AgentAction {
        id: "patch-fragment".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let lines = super::agent::runtime_unrecovered_failure_output_lines(
        &action,
        "\n∙\nb\ngal(&mut\ncomma\nd\nS\ne\nu\nl\nE\nR\nMEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n",
    );

    assert_eq!(
        lines,
        vec![
            "apply_patch failed without an actionable patch diagnostic. Next step: inspect the current target file with a bounded shell_command, then retry with a smaller fresh Mezzanine *** Begin Patch block."
                .to_string()
        ]
    );
}

/// Verifies runtime event fanout batches all ready event notifications for one
/// connection into a single sink write while still advancing the per-connection
/// delivery cursor for each visible event.
///
/// High-volume event streams can otherwise perform one write per event per
/// connection, so this regression protects the fanout path from regressing to
/// O(events × clients) write calls when a bounded replay batch is ready.
#[test]
fn runtime_event_fanout_batches_frames_per_connection() {
    struct RecordingEventSink {
        frames: Vec<(String, Vec<u8>)>,
    }

    impl crate::runtime::RuntimeEventFanoutSink for RecordingEventSink {
        fn send_frame(&mut self, connection_id: &str, frame: &[u8]) -> crate::Result<()> {
            self.frames
                .push((connection_id.to_string(), frame.to_vec()));
            Ok(())
        }
    }

    let mut event_log = crate::event::EventLog::new(8, 1024).unwrap();
    event_log
        .append(
            crate::event::EventKind::Diagnostic,
            Some("session".to_string()),
            crate::event::EventVisibility::SessionView,
            r#"{"message":"first"}"#,
        )
        .unwrap();
    event_log
        .append(
            crate::event::EventKind::Diagnostic,
            Some("session".to_string()),
            crate::event::EventVisibility::SessionView,
            r#"{"message":"second"}"#,
        )
        .unwrap();

    let mut connections = crate::runtime::RuntimeEventConnectionTable::default();
    connections
        .attach("event-connection", crate::event::EventAudience::Primary, true, 0)
        .unwrap();
    let wakeups = connections.wakeups(Some(&event_log), 10);

    let mut sink = RecordingEventSink { frames: Vec::new() };
    let delivered = crate::runtime::flush_runtime_event_wakeups(
        &mut connections,
        &wakeups,
        &mut sink,
    )
    .unwrap();

    assert_eq!(delivered, 2);
    assert_eq!(sink.frames.len(), 1);
    assert_eq!(sink.frames[0].0, "event-connection");
    let batched = String::from_utf8(sink.frames[0].1.clone()).unwrap();
    assert!(batched.contains(r#""message":"first""#), "{batched}");
    assert!(batched.contains(r#""message":"second""#), "{batched}");
    assert!(connections.wakeups(Some(&event_log), 10).is_empty());
}
