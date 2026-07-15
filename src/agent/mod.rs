//! Agent-harness primitives.
//!
//! This module contains testable pieces of the pane-bound agent harness before
//! the runtime owns a real model loop. It covers shell transaction wrapping,
//! environment bootstrap decisions, prompt/model request assembly, action
//! planning, and injectable execution boundaries for pane shell actions.

use std::collections::BTreeMap;
use std::path::Path;

use secrecy::{ExposeSecret, SecretString};

use crate::error::{MezError, Result};
use mez_agent::{McpPromptTool, ModelProfile, ModelResponse};

/// Exposes the actions module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod actions;
/// Exposes the context module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod context;
/// Exposes the maap module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod maap;
/// Exposes the network module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod network;
/// Exposes the prompt module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod prompt;
/// Exposes the provider module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod provider;
/// Exposes the semantic module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod semantic;
/// Exposes the shell module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod shell;
/// Exposes the slash module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod slash;
pub use actions::{
    AgentTurnExecution, AgentTurnRunner, AsyncMcpActionExecutor, LocalActionExecutor,
    LocalExecutionOutput, LocalExecutionRequest, LocalExecutionTransport, McpActionExecutor,
    PaneShellExecutor, PaneShellLocalExecutor, ShellExecutionOutput, ShellExecutionRequest,
    ShellTransportDecodeResult, ShellTransportDiagnostics, assistant_context_content_for_execution,
    decode_shell_output_transport, decode_shell_output_transport_with_diagnostics,
    discover_tools_through_pane_shell, execute_local_action, execute_mcp_action_through_runtime,
    execute_mcp_action_through_runtime_async, execute_shell_action_through_pane,
    next_transcript_sequence, persist_turn_execution_transcript,
    postprocess_shell_action_success_output, shell_command_result_content,
    shell_command_structured_content_json, transcript_entries_for_execution,
};
pub use context::assemble_model_request;
pub(crate) use maap::MaapBatchProductValidation;
use maap::{action_content_blocks_from_json_or_text, json_escape, validate_non_empty};
pub use maap::{
    parse_fenced_maap_action_batch, parse_fenced_maap_action_batch_for_turn,
    parse_maap_action_batch_json, parse_maap_action_batch_json_for_turn,
};
#[cfg(test)]
use mez_agent::AgentCapability;
use mez_agent::action_text_content_blocks;
use mez_agent::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentContext, AgentLogLevel,
    AgentShellStore, AgentShellVisibility, AgentTranscriptEntry, AgentTurnLedger, AgentTurnRecord,
    AgentTurnState, AllowedActionSet, ContextSourceKind, LocalActionPlan, MaapBatch,
    McpExecutionRequest, McpExecutionResponse, ModelInteractionKind, ModelMessage,
    ModelMessageRole, ModelRequest, ModelTokenUsage, ProviderHttpRequest, ProviderHttpResponse,
    SayStatus, TranscriptPersistence, agent_shell_help_display, agent_shell_mcp_display,
    agent_shell_permissions_display, agent_shell_status_display,
};
pub use network::{
    execute_network_action_with_transport_async, network_action_structured_content_json,
};
pub use prompt::{
    build_agent_system_prompt, build_agent_system_prompt_with_repository_instructions,
};
pub use provider::{
    AnthropicMessagesProvider, AsyncModelProvider, AsyncProviderHttpTransport,
    CHATGPT_ACCOUNT_ID_HEADER, ChatCompletionsProvider, ClaudeCodeProvider,
    DeepSeekChatCompletionsProvider, OpenAiCompatibleChatCompletionsProvider,
    OpenAiResponsesProvider, ReqwestProviderHttpTransport,
    anthropic_provider_from_auth_store_with_provider_options,
    build_deepseek_chat_completions_http_request, build_openai_models_http_request,
    build_openai_models_http_request_with_headers, build_openai_responses_http_request,
    build_openai_responses_http_request_with_headers,
    deepseek_chat_completions_provider_from_auth_store_with_provider_options,
    deepseek_provider_from_auth_store_with_provider_options, effective_provider_api,
    openai_compatible_provider_from_auth_store_with_provider_options,
    openai_provider_from_auth_store_with_options,
    openai_provider_from_auth_store_with_provider_options,
    openai_responses_provider_from_auth_store_with_provider_options, parse_openai_models_http_body,
};
#[cfg(test)]
pub use provider::{
    ModelProvider, ProviderHttpTransport, openai_provider_from_auth_store_with_transport,
};
pub(crate) use provider::{
    provider_error_retry_class, provider_error_retry_class_from_parts,
    provider_event_error_from_parts, provider_event_error_kind,
};
pub use semantic::{
    ApplyPatchTransactionPhase, action_is_local_shell_backed, apply_patch_error_plan,
    apply_patch_read_plan_for_paths, apply_patch_touched_paths, apply_patch_transaction_phase,
    apply_patch_write_plan_from_read_output, apply_patch_write_plan_from_read_outputs,
    local_action_plan, local_action_summary,
};
pub use shell::{
    DEFAULT_BOOTSTRAP_TIMEOUT_MS, DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, EnvironmentSignature,
    MarkerToken, ShellClassification, ShellTransaction, ShellTransactionInput,
    ShellTransactionOutputTransport, ToolDiscoveryCache, ToolInventory,
    agent_subshell_enter_command, bootstrap_script, bootstrap_script_for_classification,
    fish_bootstrap_script, fish_quote, parse_bootstrap_env_output,
    readiness_probe_command_for_classification, tool_discovery_script,
};
pub(crate) use shell::{
    posix_shell_history_suppression_finish, posix_shell_history_suppression_start,
};
pub use slash::{
    AgentShellCommandOutcome, AgentShellRuntimeContext, execute_agent_shell_command,
    execute_agent_shell_command_with_context, execute_agent_shell_command_with_mcp,
    execute_agent_shell_command_with_permissions, execute_agent_shell_command_with_runtime_context,
    parse_slash_command,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
