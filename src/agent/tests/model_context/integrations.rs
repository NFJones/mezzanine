//! Model Context tests for integrations behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies MCP context does not add routing hints even when the current user
/// request matches available server metadata.
///
/// Server purpose remains visible through the regular MCP manifest line, but
/// the removed routing-match mechanism must not add extra next-action hints
/// that can steer the model away from the normal MAAP action-selection rules.
fn mcp_context_does_not_emit_routing_match_for_verbatim_server_purpose() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        label: "user".to_string(),
        content: "@gitlab GitLab issue and merge request operations".to_string(),
    }])
    .unwrap();
    let context = append_mcp_context(
        context,
        &mez_agent::McpPromptSummary {
            available_servers: vec![mez_agent::McpPromptServer {
                server_id: "gitlab".to_string(),
                display_name: "GitLab".to_string(),
                purpose: "GitLab issue and merge request operations".to_string(),
                usage_instructions: "Use for GitLab issue and merge request tasks.".to_string(),
                tool_count: 1,
                approval_required_tool_count: 0,
            }],
            available_tools: vec![mez_agent::McpPromptTool {
                server_id: "gitlab".to_string(),
                tool_name: "get_issue".to_string(),
                description: "Read one issue".to_string(),
                approval_required: false,
                input_schema_json: r#"{"type":"object"}"#.to_string(),
            }],
            unavailable_servers: Vec::new(),
        },
    )
    .unwrap();

    assert!(
        context.blocks[0]
            .content
            .contains("server=gitlab status=available route=mcp_call"),
        "{}",
        context.blocks[0].content
    );
    assert!(
        !context.blocks[0].content.contains("routing_match="),
        "{}",
        context.blocks[0].content
    );
}

#[test]
/// Verifies explicit MCP context includes all requested server tools.
///
/// Explicit MCP server invocations are the point where the model receives the
/// concrete server manifest. A large server must not hide tools that fall after
/// the compact ordinary-context detail limit, because the requested server is
/// already known to be relevant for the turn.
fn mcp_context_includes_all_tools_for_explicit_server_invocation() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        label: "user".to_string(),
        content: "use @fs to choose the right tool".to_string(),
    }])
    .unwrap();
    let tools = (0..10)
        .map(|index| mez_agent::McpPromptTool {
            server_id: "fs".to_string(),
            tool_name: format!("tool_{index:02}"),
            description: format!("Tool {index} description"),
            approval_required: false,
            input_schema_json: r#"{"type":"object"}"#.to_string(),
        })
        .collect::<Vec<_>>();
    let context = append_mcp_context(
        context,
        &mez_agent::McpPromptSummary {
            available_servers: vec![mez_agent::McpPromptServer {
                server_id: "fs".to_string(),
                display_name: "Filesystem".to_string(),
                purpose: "Read project files through MCP".to_string(),
                usage_instructions: "Use when MCP-backed file access is requested.".to_string(),
                tool_count: tools.len(),
                approval_required_tool_count: 0,
            }],
            available_tools: tools,
            unavailable_servers: Vec::new(),
        },
    )
    .unwrap();
    let content = &context.blocks[0].content;

    for index in 0..10 {
        assert!(
            content.contains(&format!("available_tool=fs/tool_{index:02}")),
            "{content}"
        );
    }
    assert!(!content.contains("available_tool_inventory="), "{content}");
}

#[test]
/// Verifies configured MCP servers are not globally injected without `@server`.
///
/// Ordinary turns should not receive a server/tool catalog merely because MCP
/// servers are configured. Prompt-visible MCP details are injected only when the
/// current user prompt or loaded skill text names a server with `@<server-id>`.
fn mcp_context_omits_integrations_without_explicit_server_invocation() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        label: "user".to_string(),
        content: "call a tool".to_string(),
    }])
    .unwrap();
    let context = append_mcp_context(
        context,
        &mez_agent::McpPromptSummary {
            available_servers: vec![mez_agent::McpPromptServer {
                server_id: "fs".to_string(),
                display_name: "Filesystem".to_string(),
                purpose: "Read project files through MCP".to_string(),
                usage_instructions: "Use read_file only when the task needs file contents."
                    .to_string(),
                tool_count: 1,
                approval_required_tool_count: 1,
            }],
            available_tools: vec![mez_agent::McpPromptTool {
                server_id: "fs".to_string(),
                tool_name: "read_file".to_string(),
                description: "Read files".to_string(),
                approval_required: true,
                input_schema_json: r#"{\"type\":\"object\"}"#.to_string(),
            }],
            unavailable_servers: vec![mez_agent::McpPromptUnavailableServer {
                server_id: "gitlab".to_string(),
                purpose: "GitLab issue and merge request operations".to_string(),
                usage_instructions: "Use for GitLab issue and merge request tasks.".to_string(),
                reason: "authentication failed".to_string(),
                retryable: true,
            }],
        },
    )
    .unwrap();

    assert_eq!(context.blocks.len(), 1);
    assert_eq!(context.blocks[0].source, ContextSourceKind::UserInstruction);
    assert_eq!(context.blocks[0].content, "call a tool");
}

#[test]
/// Verifies that MCP tool descriptions are quoted and whitespace-normalized in
/// the prompt context so model-visible metadata stays readable and stable.
fn mcp_context_quotes_and_normalizes_tool_descriptions() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        label: "user".to_string(),
        content: "use @fs for the fs/read_file MCP tool".to_string(),
    }])
    .unwrap();
    let context = append_mcp_context(
        context,
        &mez_agent::McpPromptSummary {
            available_servers: vec![mez_agent::McpPromptServer {
                server_id: "fs".to_string(),
                display_name: "Filesystem".to_string(),
                purpose: "Read project files through MCP".to_string(),
                usage_instructions: "Use read_file only when the task needs file contents."
                    .to_string(),
                tool_count: 1,
                approval_required_tool_count: 1,
            }],
            available_tools: vec![mez_agent::McpPromptTool {
                server_id: "fs".to_string(),
                tool_name: "read_file".to_string(),
                description: "Read files\nfrom MCP".to_string(),
                approval_required: true,
                input_schema_json: r#"{"type":"object"}"#.to_string(),
            }],
            unavailable_servers: Vec::new(),
        },
    )
    .unwrap();

    assert!(
        context.blocks[0]
            .content
            .contains("available_tool=fs/read_file")
    );
    assert!(
        context.blocks[0]
            .content
            .contains("description=\"Read files from MCP\"")
    );
}

#[test]
/// Verifies that refreshing MCP prompt context replaces the previous
/// integration block instead of appending another copy. Provider continuations
/// rebuild the runtime context repeatedly, so duplicated MCP summaries would
/// grow both memory use and prompt size during long turns.
fn mcp_context_refresh_replaces_previous_integration_block() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        label: "user".to_string(),
        content: "call @fs and then @git".to_string(),
    }])
    .unwrap();
    let first = mez_agent::McpPromptSummary {
        available_servers: vec![mez_agent::McpPromptServer {
            server_id: "fs".to_string(),
            display_name: "Filesystem".to_string(),
            purpose: "Read project files through MCP".to_string(),
            usage_instructions: "Use read_file only when the task needs file contents.".to_string(),
            tool_count: 1,
            approval_required_tool_count: 1,
        }],
        available_tools: vec![mez_agent::McpPromptTool {
            server_id: "fs".to_string(),
            tool_name: "read_file".to_string(),
            description: "Read files".to_string(),
            approval_required: true,
            input_schema_json: r#"{"type":"object"}"#.to_string(),
        }],
        unavailable_servers: Vec::new(),
    };
    let second = mez_agent::McpPromptSummary {
        available_servers: vec![mez_agent::McpPromptServer {
            server_id: "git".to_string(),
            display_name: "Git".to_string(),
            purpose: "Read Git state through MCP".to_string(),
            usage_instructions: "Use status for Git state summaries.".to_string(),
            tool_count: 1,
            approval_required_tool_count: 0,
        }],
        available_tools: vec![mez_agent::McpPromptTool {
            server_id: "git".to_string(),
            tool_name: "status".to_string(),
            description: "Read status".to_string(),
            approval_required: false,
            input_schema_json: r#"{"type":"object"}"#.to_string(),
        }],
        unavailable_servers: Vec::new(),
    };

    let context = append_mcp_context(context, &first).unwrap();
    let context = append_mcp_context(context, &second).unwrap();
    let mcp_blocks = context
        .blocks
        .iter()
        .filter(|block| block.label == "mcp integrations")
        .collect::<Vec<_>>();

    assert_eq!(mcp_blocks.len(), 1);
    assert!(
        mcp_blocks[0]
            .content
            .contains("server=git status=available route=mcp_call")
    );
    assert!(
        !mcp_blocks[0]
            .content
            .contains("available_tool=fs/read_file")
    );
    assert!(
        mcp_blocks[0].content.contains("available_tool=git/status"),
        "{}",
        mcp_blocks[0].content
    );
}

#[test]
/// Verifies memory context accepts user-managed sensitive records.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn memory_context_accepts_sensitive_records_without_heuristic_rejection() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        label: "user".to_string(),
        content: "do the task".to_string(),
    }])
    .unwrap();
    let records = vec![MemoryContextRecord {
        id: "secret".to_string(),
        scope: MemoryContextScope::Global,
        updated_at_unix_seconds: 10,
        priority: 9,
        content: "api_key = sk-secret".to_string(),
    }];

    let context = append_memory_context(context, &records, 1).unwrap();

    assert_eq!(context.blocks[1].content, "api_key = sk-secret");
}

#[test]
/// Verifies memory context appends after active context in priority order.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn memory_context_appends_after_active_context_in_priority_order() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        label: "user".to_string(),
        content: "do the task".to_string(),
    }])
    .unwrap();
    let records = vec![
        MemoryContextRecord {
            id: "low".to_string(),
            scope: MemoryContextScope::Global,
            updated_at_unix_seconds: 10,
            priority: 1,
            content: "low priority".to_string(),
        },
        MemoryContextRecord {
            id: "high".to_string(),
            scope: MemoryContextScope::Pane {
                session_id: "$1".to_string(),
                pane_id: "%1".to_string(),
            },
            updated_at_unix_seconds: 20,
            priority: 9,
            content: "high priority".to_string(),
        },
    ];

    let context = append_memory_context(context, &records, 2).unwrap();
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &context,
    )
    .unwrap();

    assert_eq!(context.blocks[0].source, ContextSourceKind::UserInstruction);
    assert_eq!(context.blocks[1].source, ContextSourceKind::Memory);
    assert!(context.blocks[1].label.contains("high"));
    assert!(context.blocks[2].label.contains("low"));
    assert_eq!(request.messages[2].role, ModelMessageRole::User);
}

#[test]
/// Verifies model-facing context omits raw permission policy fields.
///
/// The runtime may combine a read-only preset label with a full-access approval
/// policy internally. The model should receive denials through action results
/// instead of raw fields that can make visible mutation actions look unavailable.
fn permission_context_is_not_model_visible() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        label: "user".to_string(),
        content: "edit the file".to_string(),
    }])
    .unwrap();
    let context = append_permission_policy_context(context).unwrap();

    assert_eq!(context.blocks.len(), 1);
    assert_eq!(context.blocks[0].source, ContextSourceKind::UserInstruction);
    assert_eq!(context.blocks[0].content, "edit the file");
}
