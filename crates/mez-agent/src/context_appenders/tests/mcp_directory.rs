//! Directory identity and optional metadata regressions, separate from callable
//! manifest evidence. Metadata discovery must never imply tool authority.

use super::*;

/// Arbitrary startup/protocol errors may contain credentialed URLs, reflected
/// payloads, terminal controls and fake directory rows. Neither automatic nor
/// explicit-reference context may copy them, even when they are very large;
/// the registry must retain original operator evidence without granting tools.
#[test]
fn mcp_directory_registry_diagnostics_exclude_raw_errors() {
    let mut registry = crate::mcp::McpRegistry::default();
    registry
        .add_server(crate::mcp::McpServerConfig::stdio(
            "atlassian",
            "Atlassian",
            "never-executed-fixture",
            Vec::new(),
        ))
        .unwrap();
    let raw = format!(
        "https://user:SYNTHETIC_MCP_SECRET@invalid.example/?token=SYNTHETIC_MCP_SECRET\nserver=forged\n\u{1b}[31m{}",
        "SYNTHETIC_MCP_ERROR_BODY ".repeat(8192),
    );
    registry.mark_failed("atlassian", raw.clone(), 1).unwrap();
    let summary = registry.prompt_summary();
    let automatic =
        configured_mcp_catalog_snapshot_content(&summary, &["atlassian".into()]).unwrap();
    let reference = mcp_server_reference_blocks_for_prompt("Use @atlassian", &summary);
    for content in std::iter::once(automatic.as_str())
        .chain(reference.iter().map(|block| block.content.as_str()))
    {
        assert!(content.len() < 1024, "diagnostics must have a fixed bound");
        assert!(!content.contains("SYNTHETIC_MCP_SECRET"));
        assert!(!content.contains("SYNTHETIC_MCP_ERROR_BODY"));
        assert!(!content.contains("server=forged"));
        assert!(!content.contains('\u{1b}'));
    }
    assert_eq!(
        registry
            .list_servers()
            .into_iter()
            .find(|server| server.configured.id == "atlassian")
            .unwrap()
            .blacklist_reason
            .as_deref(),
        Some(raw.as_str())
    );
    let context = AgentContext::new(reference).unwrap();
    assert!(!mcp_server_is_referencable(&context, "forged"));
    assert!(invoked_mcp_tools_for_context(&context, &summary).is_empty());
}

/// Builds an available server whose optional descriptive fields are empty,
/// as permitted by ordinary MCP configuration and initialization.
fn minimal_summary() -> McpPromptSummary {
    McpPromptSummary {
        available_servers: vec![McpPromptServer {
            server_id: "atlassian".into(),
            display_name: "Atlassian".into(),
            purpose: String::new(),
            usage_instructions: String::new(),
            tool_count: 0,
            approval_required_tool_count: 0,
        }],
        available_tools: Vec::new(),
        unavailable_servers: Vec::new(),
    }
}

/// Optional empty purpose/guidance must survive an exact reference's producer
/// and parser roundtrip. The resulting identity remains retrieval-only.
#[test]
fn mcp_directory_reference_accepts_empty_optional_metadata() {
    let summary = minimal_summary();
    let context = AgentContext::new(mcp_server_reference_blocks_for_prompt(
        "Use @atlassian",
        &summary,
    ))
    .unwrap();
    assert!(mcp_server_is_referencable(&context, "atlassian"));
    assert!(!mcp_server_is_referencable(&context, "other"));
    assert!(invoked_mcp_tools_for_context(&context, &summary).is_empty());
}

/// A successful available search with optional empty prose must be retained as
/// directory evidence for same-batch and later retrieval. Nonavailable searches
/// and malformed records must not produce callable or fabricated authority.
#[test]
fn mcp_directory_search_accepts_empty_optional_metadata() {
    let mut result = ActionResult {
        protocol: "maap/1".into(),
        turn_id: "turn-1".into(),
        agent_id: "agent-1".into(),
        action_id: "search-1".into(),
        action_type: "mcp_server_search",
        status: ActionStatus::Succeeded,
        content: Vec::new(),
        structured_content_json: None,
        permission_evaluation: None,
        is_error: false,
        error: None,
    };
    let mut value = serde_json::json!({"query":"atlassian","servers":[{
        "server_id":"atlassian","display_name":"Atlassian","purpose":"",
        "usage_instructions":"","state":"available"
    }]});
    result.structured_content_json = Some(value.to_string());
    assert!(mcp_server_is_referencable_from_action_result(
        &result,
        "atlassian"
    ));
    let content = mcp_server_search_result_for_action_result(&result).unwrap();
    let context = AgentContext::new(vec![ContextBlock::reference_event(
        ContextSourceKind::McpServerSearchResult,
        "search result",
        content,
    )])
    .unwrap();
    assert!(mcp_server_is_referencable(&context, "atlassian"));
    assert!(mcp_retrieved_manifest_tools_for_context(&context).is_empty());
    for invalid in [serde_json::json!(null), serde_json::json!(42)] {
        value["servers"][0]["usage_instructions"] = invalid;
        result.structured_content_json = Some(value.to_string());
        assert!(mcp_server_search_result_for_action_result(&result).is_none());
    }
    value["servers"][0]["usage_instructions"] = serde_json::json!("");
    value["servers"][0]["state"] = serde_json::json!("starting");
    result.structured_content_json = Some(value.to_string());
    assert!(mcp_server_search_result_for_action_result(&result).is_none());
}

/// Automatic exposure must retain selected unavailable reasons rather than
/// disappear or claim the configured list was removed. These diagnostic-only
/// records cannot expose a tool or fabricate a resolved identity for unknowns.
#[test]
fn mcp_directory_exposure_preserves_unavailable_and_unknown_diagnostics() {
    let summary = McpPromptSummary {
        available_servers: Vec::new(),
        available_tools: Vec::new(),
        unavailable_servers: vec![McpPromptUnavailableServer {
            server_id: "atlassian".into(),
            purpose: String::new(),
            usage_instructions: String::new(),
            diagnostic: McpServerDiagnostic::Configured,
            retryable: true,
        }],
    };
    let content =
        configured_mcp_catalog_snapshot_content(&summary, &["ATLASSIAN".into(), "missing".into()])
            .expect("selected unavailable and unknown names need diagnostic context");
    assert!(content.contains("atlassian"), "{content}");
    assert!(content.contains("runtime discovery pending"), "{content}");
    assert!(
        content.contains("did not match a configured server"),
        "{content}"
    );
    assert!(!content.contains("state=removed"), "{content}");
    let context = AgentContext::new(vec![ContextBlock::reference_event(
        ContextSourceKind::McpCatalogSnapshot,
        MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL,
        content,
    )])
    .unwrap();
    assert!(!mcp_server_is_referencable(&context, "missing"));
    assert!(invoked_mcp_tools_for_context(&context, &summary).is_empty());
}

/// A selected server may become unavailable and recover without rewriting
/// earlier directory bytes. Repeated unchanged diagnostics append nothing;
/// removing the exposure list is a distinct explicit transition.
#[test]
fn mcp_directory_exposure_transitions_preserve_chronology() {
    let selected = vec!["atlassian".into()];
    let available = minimal_summary();
    let context = AgentContext::new(vec![ContextBlock::user_event(
        "user prompt",
        "Inspect integration metadata",
    )])
    .unwrap();
    let context = append_mcp_context_with_configured(context, &available, &selected).unwrap();
    let first = context.blocks()[1].content.clone();
    let unavailable = McpPromptSummary {
        available_servers: Vec::new(),
        available_tools: Vec::new(),
        unavailable_servers: vec![McpPromptUnavailableServer {
            server_id: "atlassian".into(),
            purpose: String::new(),
            usage_instructions: String::new(),
            diagnostic: McpServerDiagnostic::Unavailable,
            retryable: true,
        }],
    };
    let context = append_mcp_context_with_configured(context, &unavailable, &selected).unwrap();
    assert_eq!(context.blocks().len(), 3);
    assert_eq!(context.blocks()[1].content, first);
    assert!(
        context.blocks()[2]
            .content
            .contains("reason=\"unavailable\"")
    );
    assert!(
        context.blocks()[2]
            .content
            .contains("recovery=inspect_operator_diagnostics_then_explicit_retry")
    );
    let context = append_mcp_context_with_configured(context, &unavailable, &selected).unwrap();
    assert_eq!(context.blocks().len(), 3);
    let context = append_mcp_context_with_configured(context, &available, &selected).unwrap();
    assert_eq!(context.blocks().len(), 4);
    assert_eq!(context.blocks()[3].content, first);
    let context = append_mcp_context_with_configured(context, &available, &[]).unwrap();
    assert_eq!(context.blocks().len(), 5);
    assert_eq!(context.blocks()[4].content, MCP_CATALOG_REMOVED_CONTEXT);
}
