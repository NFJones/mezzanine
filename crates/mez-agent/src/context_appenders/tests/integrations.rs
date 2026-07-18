//! Model Context tests for integrations behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

/// Builds a prompt summary with one callable tool per configured server id.
fn mcp_summary_for_server_ids(server_ids: &[&str]) -> McpPromptSummary {
    McpPromptSummary {
        available_servers: server_ids
            .iter()
            .map(|server_id| McpPromptServer {
                server_id: (*server_id).to_string(),
                display_name: (*server_id).to_string(),
                purpose: "Test MCP server".to_string(),
                usage_instructions: "Use the matching tool.".to_string(),
                tool_count: 1,
                approval_required_tool_count: 0,
            })
            .collect(),
        available_tools: server_ids
            .iter()
            .map(|server_id| McpPromptTool {
                server_id: (*server_id).to_string(),
                tool_name: "lookup".to_string(),
                description: "Look up a test record".to_string(),
                approval_required: false,
                input_schema_json: r#"{"type":"object"}"#.to_string(),
            })
            .collect(),
        unavailable_servers: Vec::new(),
    }
}

/// Returns the generated request-local MCP block without relying on its index
/// relative to durable prompt chronology.
fn mcp_context_content(context: &AgentContext) -> &str {
    &context
        .blocks
        .iter()
        .find(|block| block.label == MCP_INTEGRATIONS_CONTEXT_LABEL)
        .expect("MCP live-state block should be present")
        .content
}

#[test]
/// Verifies dynamic-schema providers do not receive duplicate textual MCP
/// definitions while OpenAI Responses retains the complete late manifest its
/// cache-stable generic MCP action cannot express.
fn mcp_context_is_provider_aware_and_keeps_unavailability_diagnostics() {
    let context = AgentContext::new_durable(vec![ContextBlock::user_event(
        "user prompt",
        "use @GitHub_2 to inspect the issue",
    )])
    .unwrap();
    let summary = mcp_summary_for_server_ids(&["GitHub_2"]);

    let anthropic = append_mcp_context_for_provider(context.clone(), &summary, "anthropic")
        .expect("dynamic Anthropic tools should carry the manifest");
    let openai = append_mcp_context_for_provider(context, &summary, "openai")
        .expect("OpenAI Responses should receive its late manifest");

    assert!(
        anthropic
            .blocks
            .iter()
            .all(|block| block.label != MCP_INTEGRATIONS_CONTEXT_LABEL)
    );
    let openai_manifest = openai
        .blocks
        .iter()
        .find(|block| block.label == MCP_INTEGRATIONS_CONTEXT_LABEL)
        .unwrap();
    assert!(
        openai_manifest
            .content
            .contains("available_tool=GitHub_2/lookup")
    );
    assert!(openai_manifest.content.contains("input_schema="));
}

#[test]
/// Verifies explicit mentions preserve exact configured identifier casing and
/// expose the matching server tools to both prompt context and action schemas.
fn mcp_context_resolves_exact_mixed_case_configured_server_id() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "use @GitHub_2 to inspect the issue".to_string(),
    }])
    .unwrap();
    let summary = mcp_summary_for_server_ids(&["GitHub_2"]);

    let tools = invoked_mcp_tools_for_context(&context, &summary);
    let context = append_mcp_context(context, &summary).unwrap();

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].server_id, "GitHub_2");
    let content = mcp_context_content(&context);
    assert!(
        content.contains("server=GitHub_2 status=available route=mcp_call"),
        "{content}"
    );
}

#[test]
/// Verifies a case-insensitive mention resolves only when one configured
/// server has that spelling, while preserving the canonical configured id.
fn mcp_context_resolves_unambiguous_case_insensitive_server_id() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "use @github_2 to inspect the issue".to_string(),
    }])
    .unwrap();
    let summary = mcp_summary_for_server_ids(&["GitHub_2"]);

    let tools = invoked_mcp_tools_for_context(&context, &summary);

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].server_id, "GitHub_2");
}

#[test]
/// Verifies unresolved and case-ambiguous mentions produce bounded model
/// diagnostics without exposing tools from an arbitrarily selected server.
fn mcp_context_reports_unresolved_and_ambiguous_server_mentions() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "compare @missing with @GITHUB".to_string(),
    }])
    .unwrap();
    let summary = mcp_summary_for_server_ids(&["GitHub", "github"]);

    let tools = invoked_mcp_tools_for_context(&context, &summary);
    let context = append_mcp_context(context, &summary).unwrap();
    let content = mcp_context_content(&context);

    assert!(tools.is_empty());
    assert!(content.contains("unavailable_server=missing"), "{content}");
    assert!(
        content.contains("did not match a configured server"),
        "{content}"
    );
    assert!(content.contains("unavailable_server=GITHUB"), "{content}");
    assert!(content.contains("mention is ambiguous"), "{content}");
    assert!(!content.contains("available_tool="), "{content}");
}

#[test]
/// Verifies exact casing wins even when another configured id differs only by
/// case, so canonical selection never rejects an unambiguous exact mention.
fn mcp_context_prefers_exact_server_id_over_case_ambiguous_matches() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "use @GitHub now".to_string(),
    }])
    .unwrap();
    let summary = mcp_summary_for_server_ids(&["GitHub", "github"]);

    let tools = invoked_mcp_tools_for_context(&context, &summary);

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].server_id, "GitHub");
}

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
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "@gitlab GitLab issue and merge request operations".to_string(),
    }])
    .unwrap();
    let context = append_mcp_context(
        context,
        &McpPromptSummary {
            available_servers: vec![McpPromptServer {
                server_id: "gitlab".to_string(),
                display_name: "GitLab".to_string(),
                purpose: "GitLab issue and merge request operations".to_string(),
                usage_instructions: "Use for GitLab issue and merge request tasks.".to_string(),
                tool_count: 1,
                approval_required_tool_count: 0,
            }],
            available_tools: vec![McpPromptTool {
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

    let content = mcp_context_content(&context);
    assert!(
        content.contains("server=gitlab status=available route=mcp_call"),
        "{content}"
    );
    assert!(!content.contains("routing_match="), "{content}");
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
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "use @fs to choose the right tool".to_string(),
    }])
    .unwrap();
    let tools = (0..10)
        .map(|index| McpPromptTool {
            server_id: "fs".to_string(),
            tool_name: format!("tool_{index:02}"),
            description: format!("Tool {index} description"),
            approval_required: false,
            input_schema_json: r#"{"type":"object"}"#.to_string(),
        })
        .collect::<Vec<_>>();
    let context = append_mcp_context(
        context,
        &McpPromptSummary {
            available_servers: vec![McpPromptServer {
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
    let content = mcp_context_content(&context);

    for index in 0..10 {
        assert!(
            content.contains(&format!("available_tool=fs/tool_{index:02}")),
            "{content}"
        );
    }
    assert!(!content.contains("available_tool_inventory="), "{content}");
}

#[test]
/// Verifies an explicitly selected server exposes each complete tool contract
/// so cache-stable generic MCP actions can be constructed without a volatile
/// provider schema.
fn mcp_context_preserves_complete_selected_tool_schema_and_descriptions() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "use @catalog to inspect an item".to_string(),
    }])
    .unwrap();
    let long_description = format!("Catalog item lookup {}", "detail ".repeat(300));
    let schema = r#"{"type":"object","description":"Lookup request","properties":{"item":{"type":"object","description":"Item selector","properties":{"id":{"type":"string","description":"Stable item id","minLength":3},"tags":{"type":"array","description":"Optional tags","items":{"type":"string","enum":["featured","archived"]},"minItems":1}},"required":["id"],"additionalProperties":false}},"required":["item"],"additionalProperties":false}"#;
    let context = append_mcp_context(
        context,
        &McpPromptSummary {
            available_servers: vec![McpPromptServer {
                server_id: "catalog".to_string(),
                display_name: "Catalog".to_string(),
                purpose: "Look up catalog records".to_string(),
                usage_instructions: "Use item selectors for catalog operations.".to_string(),
                tool_count: 1,
                approval_required_tool_count: 0,
            }],
            available_tools: vec![McpPromptTool {
                server_id: "catalog".to_string(),
                tool_name: "lookup_item".to_string(),
                description: long_description.clone(),
                approval_required: false,
                input_schema_json: schema.to_string(),
            }],
            unavailable_servers: Vec::new(),
        },
    )
    .unwrap();
    let content = mcp_context_content(&context);

    assert!(
        content.contains("available_tool=catalog/lookup_item"),
        "{content}"
    );
    let canonical_schema = serde_json::from_str::<serde_json::Value>(schema)
        .and_then(|schema| serde_json::to_string(&schema))
        .unwrap();
    assert!(content.contains(&canonical_schema), "{content}");
    let normalized_long_description = long_description
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(content.contains(&normalized_long_description), "{content}");
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
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "call a tool".to_string(),
    }])
    .unwrap();
    let context = append_mcp_context(
        context,
        &McpPromptSummary {
            available_servers: vec![McpPromptServer {
                server_id: "fs".to_string(),
                display_name: "Filesystem".to_string(),
                purpose: "Read project files through MCP".to_string(),
                usage_instructions: "Use read_file only when the task needs file contents."
                    .to_string(),
                tool_count: 1,
                approval_required_tool_count: 1,
            }],
            available_tools: vec![McpPromptTool {
                server_id: "fs".to_string(),
                tool_name: "read_file".to_string(),
                description: "Read files".to_string(),
                approval_required: true,
                input_schema_json: r#"{\"type\":\"object\"}"#.to_string(),
            }],
            unavailable_servers: vec![crate::McpPromptUnavailableServer {
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
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "use @fs for the fs/read_file MCP tool".to_string(),
    }])
    .unwrap();
    let context = append_mcp_context(
        context,
        &McpPromptSummary {
            available_servers: vec![McpPromptServer {
                server_id: "fs".to_string(),
                display_name: "Filesystem".to_string(),
                purpose: "Read project files through MCP".to_string(),
                usage_instructions: "Use read_file only when the task needs file contents."
                    .to_string(),
                tool_count: 1,
                approval_required_tool_count: 1,
            }],
            available_tools: vec![McpPromptTool {
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

    let content = mcp_context_content(&context);
    assert!(content.contains("available_tool=fs/read_file"));
    assert!(content.contains("description=\"Read files from MCP\""));
}

#[test]
/// Verifies that refreshing MCP prompt context replaces the previous
/// integration block instead of appending another copy. Provider continuations
/// rebuild the runtime context repeatedly, so duplicated MCP summaries would
/// grow both memory use and prompt size during long turns.
fn mcp_context_refresh_replaces_previous_integration_block() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "call @fs and then @git".to_string(),
    }])
    .unwrap();
    let first = McpPromptSummary {
        available_servers: vec![McpPromptServer {
            server_id: "fs".to_string(),
            display_name: "Filesystem".to_string(),
            purpose: "Read project files through MCP".to_string(),
            usage_instructions: "Use read_file only when the task needs file contents.".to_string(),
            tool_count: 1,
            approval_required_tool_count: 1,
        }],
        available_tools: vec![McpPromptTool {
            server_id: "fs".to_string(),
            tool_name: "read_file".to_string(),
            description: "Read files".to_string(),
            approval_required: true,
            input_schema_json: r#"{"type":"object"}"#.to_string(),
        }],
        unavailable_servers: Vec::new(),
    };
    let second = McpPromptSummary {
        available_servers: vec![McpPromptServer {
            server_id: "git".to_string(),
            display_name: "Git".to_string(),
            purpose: "Read Git state through MCP".to_string(),
            usage_instructions: "Use status for Git state summaries.".to_string(),
            tool_count: 1,
            approval_required_tool_count: 0,
        }],
        available_tools: vec![McpPromptTool {
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
        placement: crate::ContextPlacement::ConversationAppend,
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

    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.source == ContextSourceKind::Memory
                && block.content == "api_key = sk-secret")
    );
}

#[test]
/// Verifies memory retrieved after the active prompt appends in priority order.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn memory_context_appends_after_active_context_in_priority_order() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
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
    let request = assemble_test_model_request(&context);

    assert_eq!(context.blocks[0].source, ContextSourceKind::UserInstruction);
    assert_eq!(context.blocks[0].content, "do the task");
    assert!(context.blocks[1].label.contains("high"));
    assert!(context.blocks[2].label.contains("low"));
    assert_eq!(request.messages[1].role, ModelMessageRole::User);
    assert_eq!(request.messages[2].role, ModelMessageRole::Context);
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
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "edit the file".to_string(),
    }])
    .unwrap();
    let context = append_permission_policy_context(context).unwrap();

    assert_eq!(context.blocks.len(), 1);
    assert_eq!(context.blocks[0].source, ContextSourceKind::UserInstruction);
    assert_eq!(context.blocks[0].content, "edit the file");
}
