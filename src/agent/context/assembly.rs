//! Provider request assembly for agent context.
//!
//! This module owns the final context-to-provider-message projection. It keeps
//! repository-guidance embedding, prompt-cache metadata extraction, and the
//! default MAAP action surface out of the context type facade.

use super::super::{
    AgentPromptProfile, AgentTurnRecord, ProviderTranscriptEvent,
    build_agent_system_prompt_with_repository_instructions, role_for_source, validate_non_empty,
};
use super::evidence::prepare_model_context_blocks;
use super::skills::constrain_skill_actions_for_loaded_context;
use super::{
    AgentContext, AllowedActionSet, ContextBlock, ContextSourceKind,
    DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT, ModelInteractionKind, ModelMessage,
    ModelMessageRole, ModelProfile, ModelRequest, model_context_block_header,
};
use crate::error::Result;
use crate::mcp::{McpPromptServer, McpPromptSummary, McpPromptTool, McpPromptUnavailableServer};

/// Runs the assemble model request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn assemble_model_request(
    profile: &ModelProfile,
    turn: &AgentTurnRecord,
    context: &AgentContext,
) -> Result<ModelRequest> {
    assemble_model_request_with_retained_tail_percent(
        profile,
        turn,
        context,
        DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT,
    )
}

/// Assembles a provider request without request-local fallback compaction.
///
/// The retained-tail argument is accepted by runtime call sites that also drive
/// explicit compaction paths, but provider request assembly itself preserves the
/// current context exactly and lets provider feedback trigger recovery.
pub fn assemble_model_request_with_retained_tail_percent(
    profile: &ModelProfile,
    turn: &AgentTurnRecord,
    context: &AgentContext,
    _retained_tail_percent: usize,
) -> Result<ModelRequest> {
    validate_non_empty("model provider", &profile.provider)?;
    validate_non_empty("model", &profile.model)?;
    validate_non_empty("turn_id", &turn.turn_id)?;

    let blocks = prepare_model_context_blocks(context.blocks.clone());
    let repository_instruction_blocks = blocks
        .iter()
        .filter(|block| block.source == ContextSourceKind::ProjectGuidance)
        .map(|block| block.content.clone())
        .collect::<Vec<_>>();
    let is_deepseek = profile.provider.as_str() == "deepseek";
    let repo_instructions_for_prompt = if is_deepseek {
        Vec::new()
    } else {
        repository_instruction_blocks.clone()
    };
    let mut messages = Vec::with_capacity(context.blocks.len() + 1);
    messages.push(ModelMessage {
        role: ModelMessageRole::System,
        source: ContextSourceKind::System,
        content: build_agent_system_prompt_with_repository_instructions(
            &AgentPromptProfile::default_for(&turn.agent_id, &turn.pane_id)
                .with_provider(&profile.provider)
                .with_mcp_summary(mcp_prompt_summary_from_context_blocks(&blocks)),
            &repo_instructions_for_prompt,
        )?,
    });
    for block in &blocks {
        if ProviderTranscriptEvent::from_transcript_content(&block.content).is_some() {
            messages.push(ModelMessage {
                role: ModelMessageRole::System,
                source: block.source,
                content: block.content.clone(),
            });
            continue;
        }
        if matches!(block.source, ContextSourceKind::ProjectGuidance) {
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
            role: role_for_source(block.source),
            source: block.source,
            content: format!("{}{}", model_context_block_header(block), block.content),
        });
    }
    if is_deepseek && !repository_instruction_blocks.is_empty() {
        prepend_repository_instructions_to_first_user_message(
            &mut messages,
            &repository_instruction_blocks,
        );
    }

    let mut request = ModelRequest {
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        reasoning_effort: profile
            .provider_options
            .get("reasoning_effort")
            .cloned()
            .or_else(|| profile.reasoning_profile.clone()),
        thinking_enabled: profile.thinking_enabled(),
        latency_preference: profile.latency_preference.clone(),
        prompt_cache_retention: profile
            .provider_options
            .get("prompt_cache_retention")
            .cloned(),
        max_output_tokens: profile.max_output_tokens(),
        temperature: profile.temperature().map(|t| t.to_string()).or_else(|| {
            if is_deepseek {
                Some("0.5".to_string())
            } else {
                None
            }
        }),
        prompt_cache_session_id: prompt_cache_session_id_from_blocks(&blocks),
        prompt_cache_lineage_id: prompt_cache_lineage_id_from_blocks(&blocks),
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
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
        stop: if is_deepseek {
            Some(vec!["\n}".to_string()])
        } else {
            None
        },
        messages,
    };
    constrain_skill_actions_for_loaded_context(&mut request);
    Ok(request)
}

/// Recovers the MCP prompt summary from the canonical runtime context block.
///
/// Runtime appends a compact, sanitized `[mcp integrations]` block before
/// request assembly. The system prompt also needs those availability counts so
/// it does not contradict the integration manifest with a stale zero-tool
/// default.
fn mcp_prompt_summary_from_context_blocks(blocks: &[ContextBlock]) -> McpPromptSummary {
    let Some(block) = blocks.iter().find(|block| {
        block.source == ContextSourceKind::Configuration && block.label == "mcp integrations"
    }) else {
        return empty_mcp_prompt_summary();
    };
    let mut available_tool_count = 0usize;
    let mut unavailable_servers = Vec::new();
    let mut available_servers = Vec::new();
    let mut available_tools = Vec::new();
    for line in block.content.lines() {
        if let Some(counts) = mcp_availability_counts_from_line(line) {
            available_tool_count = counts.1;
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
    if available_tools.len() < available_tool_count {
        let missing = available_tool_count - available_tools.len();
        available_tools.extend(placeholder_mcp_tools(missing));
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

/// Parses the MCP availability count line emitted by `append_mcp_context`.
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

/// Parses one unavailable-server line emitted by `append_mcp_context`.
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

/// Parses one available-server manifest line emitted by `append_mcp_context`.
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

/// Parses one available-tool manifest line emitted by `append_mcp_context`.
fn mcp_available_tool_from_line(line: &str) -> Option<McpPromptTool> {
    if !line.starts_with("available_tool=") || !line.contains("callable=true") {
        return None;
    }
    let combined = mcp_raw_field(line, "available_tool=")?;
    let (server_id, tool_name) = combined.split_once('/')?;
    Some(McpPromptTool {
        server_id: server_id.to_string(),
        tool_name: tool_name.to_string(),
        description: mcp_quoted_field(line, "description=").unwrap_or_default(),
        approval_required: false,
        input_schema_json: "{}".to_string(),
    })
}

/// Parses one whitespace-delimited raw MCP context field.
fn mcp_raw_field<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(prefix))
        .filter(|value| !value.is_empty())
}

/// Parses one debug-quoted MCP context field.
fn mcp_quoted_field(line: &str, prefix: &str) -> Option<String> {
    let start = line.find(prefix)? + prefix.len();
    let quoted = line.get(start..)?.trim_start();
    let quoted = quoted.strip_prefix('"')?;
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

/// Builds placeholder available-tool entries for prompt count rendering.
fn placeholder_mcp_tools(count: usize) -> Vec<McpPromptTool> {
    (0..count)
        .map(|_| McpPromptTool {
            server_id: String::new(),
            tool_name: String::new(),
            description: String::new(),
            approval_required: false,
            input_schema_json: "{}".to_string(),
        })
        .collect()
}

/// Extracts the live Mezzanine session UUID from runtime identity context.
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

/// Extracts the stable prompt-cache lineage id from hidden runtime metadata.
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

/// Prepends discovered repository instruction content to the first user message.
///
/// DeepSeek models weight user messages more strongly than system prompts.
/// Moving repository guidance into the first user turn places it where the
/// model's attention is strongest, improving instruction adherence without
/// altering the contract for other providers.
fn prepend_repository_instructions_to_first_user_message(
    messages: &mut [ModelMessage],
    repository_instruction_blocks: &[String],
) {
    let Some(first_user) = messages
        .iter_mut()
        .find(|m| m.role == ModelMessageRole::User)
    else {
        return;
    };
    let mut new_content = String::new();
    new_content.push_str("Active repository instructions:\n\n");
    for block in repository_instruction_blocks {
        new_content.push_str(block);
        new_content.push_str("\n\n");
    }
    new_content.push_str("---\n\n");
    new_content.push_str(&first_user.content);
    first_user.content = new_content;
}
