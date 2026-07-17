//! Provider-independent model-request assembly from canonical context.
//!
//! This module owns context-to-message projection, repository-guidance
//! placement, provider transcript preservation, MCP summary recovery, prompt
//! cache identity extraction, default action surfaces, and provider-specific
//! request defaults. Product code supplies stable turn identity and prompt
//! assets without exposing runtime records or filesystem access.

use crate::{
    AgentContext, AgentPromptAssetSource, AgentPromptProfile, AgentRequestAssemblyResult,
    AllowedActionSet, ContextBlock, ContextPlacement, ContextSourceKind, McpPromptServer,
    McpPromptSummary, McpPromptTool, McpPromptUnavailableServer, ModelInteractionKind,
    ModelMessage, ModelMessageRole, ModelProfile, ModelRequest, ProviderTranscriptEvent,
    assemble_agent_system_prompt, constrain_skill_actions_for_loaded_context,
    model_context_block_header, validate_model_profile_request,
};

/// Stable product identity required to assemble one provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelRequestIdentity<'a> {
    /// Active turn identifier.
    pub turn_id: &'a str,
    /// Active agent identifier.
    pub agent_id: &'a str,
    /// Pane identifier used by the prompt profile.
    pub pane_id: &'a str,
}

/// Assembles one complete provider request from canonical model context.
pub fn assemble_model_request_from_context(
    profile: &ModelProfile,
    identity: ModelRequestIdentity<'_>,
    context: &AgentContext,
    prompt_assets: &impl AgentPromptAssetSource,
) -> AgentRequestAssemblyResult<ModelRequest> {
    validate_model_profile_request(profile, identity.turn_id)?;

    let blocks = context.blocks.clone();
    let repository_instruction_blocks = blocks
        .iter()
        .filter(|block| block.source == ContextSourceKind::ProjectGuidance)
        .map(|block| block.content.clone())
        .collect::<Vec<_>>();
    let is_deepseek = profile.provider == "deepseek";
    let repo_instructions_for_prompt = if is_deepseek && !repository_instruction_blocks.is_empty() {
        vec![deepseek_repository_instructions_system_prompt_pointer()]
    } else {
        repository_instruction_blocks.clone()
    };
    let prompt_profile = AgentPromptProfile::default_for(identity.agent_id, identity.pane_id)
        .with_provider(&profile.provider)
        .with_mcp_summary(mcp_prompt_summary_from_context_blocks(&blocks));
    let mut messages = Vec::with_capacity(blocks.len() + 1);
    messages.push(ModelMessage {
        role: ModelMessageRole::System,
        source: ContextSourceKind::System,
        placement: ContextPlacement::StablePrefix,
        content: assemble_agent_system_prompt(
            &prompt_profile,
            &repo_instructions_for_prompt,
            prompt_assets,
        )?,
    });
    if is_deepseek && !repository_instruction_blocks.is_empty() {
        messages.push(deepseek_repository_instructions_message(
            &repository_instruction_blocks,
        ));
    }
    let mut ordered_blocks = blocks.iter().enumerate().collect::<Vec<_>>();
    ordered_blocks.sort_by_key(|(index, block)| (block.cache_disposition(), *index));
    for (_, block) in ordered_blocks {
        if ProviderTranscriptEvent::from_transcript_content(&block.content).is_some() {
            messages.push(ModelMessage {
                role: ModelMessageRole::System,
                source: block.source,
                placement: block.placement,
                content: block.content.clone(),
            });
            continue;
        }
        if block.source == ContextSourceKind::ProjectGuidance {
            continue;
        }
        if block.source == ContextSourceKind::Configuration
            && matches!(
                block.label.as_str(),
                "session identity" | "pane identity" | "prompt cache lineage"
            )
        {
            continue;
        }
        messages.push(ModelMessage {
            role: role_for_context_source(block.source),
            source: block.source,
            placement: block.placement,
            content: format!("{}{}", model_context_block_header(block), block.content),
        });
    }
    let mut request = ModelRequest {
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        reasoning_effort: profile
            .reasoning_profile
            .clone()
            .or_else(|| profile.provider_options.get("reasoning_effort").cloned()),
        thinking_enabled: profile.thinking_enabled(),
        latency_preference: profile.latency_preference.clone(),
        prompt_cache_retention: profile
            .provider_options
            .get("prompt_cache_retention")
            .cloned(),
        max_output_tokens: profile.max_output_tokens(),
        temperature: profile
            .temperature()
            .map(|value| value.to_string())
            .or_else(|| {
                if is_deepseek {
                    Some("0.5".to_string())
                } else {
                    None
                }
            }),
        prompt_cache_session_id: prompt_cache_session_id_from_blocks(&blocks),
        prompt_cache_lineage_id: prompt_cache_lineage_id_from_blocks(&blocks),
        turn_id: identity.turn_id.to_string(),
        agent_id: identity.agent_id.to_string(),
        available_mcp_tools: Vec::new(),
        memory_actions_enabled: profile
            .provider_options
            .get("memory_actions_enabled")
            .is_some_and(|value| value == "true"),
        issue_actions_enabled: profile
            .provider_options
            .get("issue_actions_enabled")
            .is_none_or(|value| value != "false"),
        interaction_kind: ModelInteractionKind::CapabilityDecision,
        allowed_actions: AllowedActionSet::capability_decision(),
        stop: is_deepseek.then(|| vec!["\n}".to_string()]),
        messages,
    };
    constrain_skill_actions_for_loaded_context(&mut request);
    Ok(request)
}

/// Maps context provenance to provider-neutral message roles.
pub fn role_for_context_source(source: ContextSourceKind) -> ModelMessageRole {
    match source {
        ContextSourceKind::System => ModelMessageRole::System,
        ContextSourceKind::DeveloperInstruction
        | ContextSourceKind::Policy
        | ContextSourceKind::Configuration
        | ContextSourceKind::RuntimeHint
        | ContextSourceKind::EvidenceLedger
        | ContextSourceKind::CommittedEvidence
        | ContextSourceKind::RoutedHandoff => ModelMessageRole::Developer,
        ContextSourceKind::ActionResult | ContextSourceKind::TranscriptTool => {
            ModelMessageRole::Tool
        }
        ContextSourceKind::TranscriptAssistant => ModelMessageRole::Assistant,
        ContextSourceKind::UserInstruction
        | ContextSourceKind::SkillInstruction
        | ContextSourceKind::LocalMessage
        | ContextSourceKind::ProjectGuidance
        | ContextSourceKind::Memory
        | ContextSourceKind::Transcript
        | ContextSourceKind::TranscriptUser => ModelMessageRole::User,
    }
}

/// Recovers an MCP prompt summary from the canonical integration block.
fn mcp_prompt_summary_from_context_blocks(blocks: &[ContextBlock]) -> McpPromptSummary {
    let Some(block) = blocks.iter().find(|block| {
        matches!(
            block.source,
            ContextSourceKind::Configuration | ContextSourceKind::RuntimeHint
        ) && block.label == "mcp integrations"
    }) else {
        return empty_mcp_prompt_summary();
    };
    let mut unavailable_servers = Vec::new();
    let mut available_servers = Vec::new();
    let mut available_tools = Vec::new();
    for line in block.content.lines() {
        if mcp_availability_counts_from_line(line).is_some() {
            continue;
        }
        if let Some(server) = mcp_available_server_from_line(line) {
            available_servers.push(server);
            continue;
        }
        if let Some(tool) = mcp_available_tool_from_line(line) {
            available_tools.push(tool);
            continue;
        }
        if let Some(server) = mcp_unavailable_server_from_line(line) {
            unavailable_servers.push(server);
        }
    }
    McpPromptSummary {
        available_servers,
        available_tools,
        unavailable_servers,
    }
}

/// Returns an empty MCP prompt summary.
fn empty_mcp_prompt_summary() -> McpPromptSummary {
    McpPromptSummary {
        available_servers: Vec::new(),
        available_tools: Vec::new(),
        unavailable_servers: Vec::new(),
    }
}

/// Parses the MCP availability count line emitted by the product adapter.
fn mcp_availability_counts_from_line(line: &str) -> Option<(usize, usize)> {
    Some((
        mcp_usize_field(line, "available_servers=")?,
        mcp_usize_field(line, "available_tools=")?,
    ))
}

/// Parses one unsigned integer field from an MCP context line.
fn mcp_usize_field(line: &str, prefix: &str) -> Option<usize> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(prefix))
        .and_then(|value| value.parse::<usize>().ok())
}

/// Parses one unavailable-server context line.
fn mcp_unavailable_server_from_line(line: &str) -> Option<McpPromptUnavailableServer> {
    line.strip_prefix("unavailable_server=")?;
    Some(McpPromptUnavailableServer {
        server_id: mcp_raw_field(line, "unavailable_server=")?.to_string(),
        purpose: mcp_quoted_field(line, "purpose=").unwrap_or_default(),
        usage_instructions: mcp_quoted_field(line, "usage_instructions=").unwrap_or_default(),
        reason: mcp_quoted_field(line, "reason=").unwrap_or_default(),
        retryable: mcp_raw_field(line, "retryable=").is_some_and(|value| value == "true"),
    })
}

/// Parses one available-server context line.
fn mcp_available_server_from_line(line: &str) -> Option<McpPromptServer> {
    if !line.starts_with("server=") || !line.contains("status=available") {
        return None;
    }
    Some(McpPromptServer {
        server_id: mcp_raw_field(line, "server=")?.to_string(),
        display_name: mcp_quoted_field(line, "name=").unwrap_or_default(),
        purpose: mcp_quoted_field(line, "purpose=").unwrap_or_default(),
        usage_instructions: mcp_quoted_field(line, "usage_instructions=").unwrap_or_default(),
        tool_count: mcp_usize_field(line, "tools=").unwrap_or(0),
        approval_required_tool_count: 0,
    })
}

/// Parses one available-tool context line.
fn mcp_available_tool_from_line(line: &str) -> Option<McpPromptTool> {
    if !line.starts_with("available_tool=") || !line.contains("callable=true") {
        return None;
    }
    let combined = mcp_raw_field(line, "available_tool=")?;
    let (server_id, tool_name) = combined.split_once('/')?;
    if server_id.is_empty() || tool_name.is_empty() {
        return None;
    }
    Some(McpPromptTool {
        server_id: server_id.to_string(),
        tool_name: tool_name.to_string(),
        description: mcp_quoted_field(line, "description=").unwrap_or_default(),
        approval_required: false,
        input_schema_json: "{}".to_string(),
    })
}

/// Parses one whitespace-delimited raw MCP field.
fn mcp_raw_field<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(prefix))
        .filter(|value| !value.is_empty())
}

/// Parses one JSON-style quoted MCP field.
fn mcp_quoted_field(line: &str, prefix: &str) -> Option<String> {
    let start = line.find(prefix)? + prefix.len();
    let quoted = line.get(start..)?.trim_start().strip_prefix('"')?;
    let mut escaped = false;
    for (index, character) in quoted.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            let literal = format!("\"{}\"", &quoted[..index]);
            return serde_json::from_str::<String>(&literal).ok();
        }
    }
    None
}

/// Extracts the live Mezzanine session id from hidden identity context.
fn prompt_cache_session_id_from_blocks(blocks: &[ContextBlock]) -> Option<String> {
    blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::Configuration && block.label == "session identity"
        })
        .and_then(|block| {
            block
                .content
                .split_whitespace()
                .find_map(|field| field.strip_prefix("session_id="))
        })
        .filter(|session_id| !session_id.trim().is_empty())
        .map(ToOwned::to_owned)
}

/// Extracts stable prompt-cache lineage from hidden metadata.
fn prompt_cache_lineage_id_from_blocks(blocks: &[ContextBlock]) -> Option<String> {
    blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::Configuration
                && block.label == "prompt cache lineage"
        })
        .map(|block| block.content.trim())
        .filter(|lineage_id| !lineage_id.is_empty())
        .map(ToOwned::to_owned)
}

/// Returns the system-prompt pointer used for DeepSeek repository guidance.
fn deepseek_repository_instructions_system_prompt_pointer() -> String {
    "DeepSeek provider note: active repository instructions are provided in a dedicated user message immediately after this system prompt. Treat that block as the authoritative repository instruction contents for this turn; do not reread repository instruction files merely because the full text is reinforced outside section 3.".to_string()
}

/// Builds the fixed-position repository-guidance message used by DeepSeek.
fn deepseek_repository_instructions_message(
    repository_instruction_blocks: &[String],
) -> ModelMessage {
    let mut content = String::from("Active repository instructions:\n\n");
    for block in repository_instruction_blocks {
        content.push_str(block);
        content.push_str("\n\n");
    }
    ModelMessage {
        role: ModelMessageRole::User,
        source: ContextSourceKind::ProjectGuidance,
        placement: ContextPlacement::StablePrefix,
        content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic prompt source for assembly tests that do not require product
    /// embedded Markdown assets.
    struct TestPromptAssets;

    impl AgentPromptAssetSource for TestPromptAssets {
        fn system_fragment<'a>(&'a self, path: &str) -> crate::AgentPromptResult<&'a str> {
            Ok(match path {
                "identity.md" => "profile {profile_name} version {profile_version}",
                "repository_instructions.md" => "repository contract",
                "subagents.md" => "subagent contract",
                "mcp.md" => "mcp contract",
                _ => "generic contract",
            })
        }

        fn provider_fragment<'a>(&'a self, _path: &str) -> crate::AgentPromptResult<&'a str> {
            Ok("provider contract")
        }
    }

    /// Verifies runtime-injected MCP context rebuilds the structured summary
    /// consumed by lower prompt assembly.
    #[test]
    fn mcp_prompt_summary_accepts_runtime_hint_blocks() {
        let summary = mcp_prompt_summary_from_context_blocks(&[ContextBlock {
            source: ContextSourceKind::RuntimeHint,
            placement: crate::ContextPlacement::EphemeralTail,
            label: "mcp integrations".to_string(),
            content: concat!(
                "available_servers=1 available_tools=1\n",
                "server=test status=available route=mcp_call name=\"Test\" purpose=\"purpose\" usage_instructions=\"usage\" tools=1\n",
                "available_tool=test/run route=mcp_call callable=true description=\"Run tool\"\n",
                "unavailable_server=offline purpose=\"offline purpose\" usage_instructions=\"offline usage\" retryable=true reason=\"auth failed\"\n",
            )
            .to_string(),
        }]);

        assert_eq!(summary.available_servers[0].server_id, "test");
        assert_eq!(summary.available_tools[0].tool_name, "run");
        assert_eq!(summary.unavailable_servers[0].server_id, "offline");
        assert_eq!(summary.unavailable_servers[0].purpose, "offline purpose");
        assert_eq!(
            summary.unavailable_servers[0].usage_instructions,
            "offline usage"
        );
        assert_eq!(summary.unavailable_servers[0].reason, "auth failed");
        assert!(summary.unavailable_servers[0].retryable);
    }

    /// Verifies unrelated context does not fabricate MCP availability.
    #[test]
    fn mcp_prompt_summary_ignores_non_mcp_blocks() {
        let summary = mcp_prompt_summary_from_context_blocks(&[ContextBlock {
            source: ContextSourceKind::Configuration,
            placement: crate::ContextPlacement::StablePrefix,
            label: "session identity".to_string(),
            content: "session_id=test-session".to_string(),
        }]);

        assert!(summary.available_servers.is_empty());
        assert!(summary.available_tools.is_empty());
        assert!(summary.unavailable_servers.is_empty());
    }

    /// Verifies lower request assembly preserves hidden provider events and
    /// extracts cache identity without exposing hidden blocks as messages.
    #[test]
    fn model_request_assembly_preserves_hidden_context_contracts() {
        let event = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call-1".to_string(),
            content: "result".to_string(),
        }
        .to_transcript_content();
        let context = AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::Configuration,
                placement: crate::ContextPlacement::StablePrefix,
                label: "session identity".to_string(),
                content: "session_id=session-1".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "provider event".to_string(),
                content: event.clone(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "continue".to_string(),
            },
        ])
        .unwrap();
        let request = assemble_model_request_from_context(
            &model_profile("deepseek"),
            ModelRequestIdentity {
                turn_id: "turn-1",
                agent_id: "agent-1",
                pane_id: "%1",
            },
            &context,
            &TestPromptAssets,
        )
        .unwrap();

        assert_eq!(
            request.prompt_cache_session_id.as_deref(),
            Some("session-1")
        );
        assert!(
            request
                .messages
                .iter()
                .any(|message| message.content == event)
        );
        assert!(
            !request
                .messages
                .iter()
                .any(|message| message.content.contains("session_id="))
        );
    }

    /// Builds one minimal profile for lower request-assembly tests.
    fn model_profile(provider: &str) -> ModelProfile {
        ModelProfile {
            provider: provider.to_string(),
            model: "test-model".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        }
    }
}

#[cfg(test)]
#[path = "context_assembly/tests/policy.rs"]
mod policy_tests;
