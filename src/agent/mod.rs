//! Agent-harness primitives.
//!
//! This module contains testable pieces of the pane-bound agent harness before
//! the runtime owns a real model loop. It covers shell transaction wrapping,
//! environment bootstrap decisions, prompt/model request assembly, action
//! planning, and injectable execution boundaries for pane shell actions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use secrecy::{ExposeSecret, SecretString};

use crate::auth::AuthStore;
use crate::error::{MezError, Result};
use crate::mcp::{
    McpPromptSummary, McpPromptTool, McpRegistry, McpToolCallPlan, McpToolCallResponse,
};
use crate::permissions::{PathScopes, PermissionPolicy, RuleDecision, SessionApprovalStore};
use crate::transcript::{AgentTranscriptStore, TranscriptEntry, TranscriptRole};

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
/// Exposes provider-native transcript event helpers.
///
/// The nested module keeps hidden provider continuity payloads outside visible
/// transcript rendering while allowing compatible providers to replay them.
mod provider_transcript;
/// Exposes the readiness module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod readiness;
/// Exposes the semantic module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod semantic;
/// Exposes the session module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod session;
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
/// Exposes the turn module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod turn;

pub use actions::{
    AgentTurnExecution, AgentTurnRunner, AsyncMcpActionExecutor, McpActionExecutor,
    PaneShellExecutor, ShellExecutionOutput, ShellExecutionRequest, ShellReadObservation,
    ShellReadObservationKind, ShellReadRange, ShellTransportDecodeResult,
    ShellTransportDiagnostics, action_result_context_content,
    assistant_context_content_for_execution, decode_shell_output_transport,
    decode_shell_output_transport_with_diagnostics, discover_tools_through_pane_shell,
    execute_mcp_action_through_runtime, execute_mcp_action_through_runtime_async,
    execute_shell_action_through_pane, next_transcript_sequence, persist_turn_execution_transcript,
    postprocess_shell_action_success_output, shell_command_result_content,
    shell_command_structured_content_json, shell_read_observations_for_command,
    transcript_entries_for_execution,
};
pub use context::{
    AgentCapability, AgentContext, AllowedAction, AllowedActionSet, ContextBlock,
    ContextCachePolicy, ContextSourceKind, ContextStability, ModelContextCompactionReport,
    ModelInteractionKind, ModelMessage, ModelMessageRole, ModelProfile, ModelProfileOverrideSource,
    ModelProfileOverrides, ModelRequest, SelectedModelProfile, append_mcp_context,
    append_memory_context, append_permission_policy_context, append_project_guidance_context,
    append_scheduler_context, assemble_model_request,
    assemble_model_request_with_retained_tail_percent, compact_model_context_for_budget,
    compact_model_context_for_budget_with_retained_tail_percent,
    constrain_skill_actions_for_loaded_context, known_model_context_window_tokens,
    model_context_text_word_count, select_model_profile, set_project_guidance_context,
};
pub use maap::{
    AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE, AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
    AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, ActionContentBlock, ActionError, ActionResult,
    ActionStatus, AgentAction, AgentActionPayload, MaapBatch, SayStatus,
    agent_output_content_type_is_diff, agent_output_content_type_is_markdown,
    normalize_agent_output_content_type, parse_fenced_maap_action_batch,
    parse_fenced_maap_action_batch_for_turn, parse_maap_action_batch_json,
    parse_maap_action_batch_json_for_turn,
};
pub use network::{
    NetworkActionPlan, execute_network_action_with_transport_async, network_action_plan,
    network_action_structured_content_json, network_action_summary,
};
pub use prompt::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentPromptProfile,
    build_agent_system_prompt, build_agent_system_prompt_with_repository_instructions,
};
pub use provider::{
    AgentContextUsageSnapshot, AsyncModelProvider, AsyncProviderHttpTransport,
    CHATGPT_ACCOUNT_ID_HEADER, CHATGPT_RESPONSES_ENDPOINT, ChatCompletionsProvider,
    DEEPSEEK_CHAT_COMPLETIONS_ENDPOINT, DEFAULT_PROVIDER_MAX_RESPONSE_BYTES,
    DEFAULT_PROVIDER_TIMEOUT_MS, DeepSeekChatCompletionsProvider, ModelResponse, ModelTokenUsage,
    ModelTokenUsageKey, OPENAI_MAAP_FUNCTION_TOOL_NAME, OPENAI_MODELS_ENDPOINT,
    OPENAI_RESPONSES_ENDPOINT, OpenAiCompatibleChatCompletionsProvider,
    OpenAiPromptCacheDiagnostics, OpenAiResponsesProvider, ProviderCapabilities,
    ProviderHttpRequest, ProviderHttpResponse, ProviderModelCatalog, ProviderModelInfo,
    ProviderQuotaUsage, ReqwestProviderHttpTransport, build_deepseek_chat_completions_http_request,
    build_openai_models_http_request, build_openai_models_http_request_with_headers,
    build_openai_responses_http_request, build_openai_responses_http_request_with_headers,
    deepseek_provider_from_auth_store_with_provider_options,
    openai_compatible_provider_from_auth_store_with_provider_options,
    openai_default_reasoning_levels_for_model, openai_models_endpoint_for_responses_endpoint,
    openai_prompt_cache_diagnostics_for_request, openai_provider_from_auth_store_with_options,
    openai_provider_from_auth_store_with_provider_options, openai_responses_endpoint_for_base_url,
    openai_responses_request_body, parse_openai_models_http_body, parse_openai_responses_http_body,
    provider_quota_usage_from_headers,
};
#[cfg(test)]
pub use provider::{
    ModelProvider, ProviderHttpTransport, openai_provider_from_auth_store_with_transport,
};
pub(crate) use provider::{
    provider_error_invites_retry, provider_error_is_context_limit_exceeded,
    provider_error_is_output_limit_exceeded,
};
pub use provider_transcript::{PROVIDER_TRANSCRIPT_EVENT_MARKER, ProviderTranscriptEvent};
pub use readiness::{
    BootstrapDecision, PaneReadinessOverride, PaneReadinessOverrideStore, PaneReadinessState,
    ReadinessDecision, ReadinessOverrideRevocation, decide_bootstrap_before_user_prompt,
    readiness_decision,
};
pub use semantic::{
    ApplyPatchTransactionPhase, LocalActionPlan, action_is_local_shell_backed,
    apply_patch_error_plan, apply_patch_touched_paths, apply_patch_transaction_phase,
    apply_patch_write_plan_from_read_output, local_action_plan, local_action_summary,
    try_convert_unified_diff_to_mez_patch,
};
pub use session::{
    AgentLogLevel, AgentShellSession, AgentShellStore, AgentShellVisibility, AgentTurnState,
    AgentTurnTrigger,
};
pub use shell::{
    DEFAULT_BOOTSTRAP_TIMEOUT_MS, DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, EnvironmentSignature,
    MarkerToken, ShellClassification, ShellTransaction, ShellTransactionInput,
    ShellTransactionOutputTransport, ToolDiscoveryCache, ToolInventory,
    agent_subshell_enter_command, bootstrap_script, bootstrap_script_for_classification,
    fish_bootstrap_script, fish_quote, parse_bootstrap_env_output,
    readiness_probe_command_for_classification, shell_quote, tool_discovery_script,
};
pub(crate) use shell::{
    posix_shell_history_suppression_finish, posix_shell_history_suppression_start,
};
pub use slash::{
    AgentShellCommandOutcome, AgentShellRuntimeContext, SlashCommandEffect, SlashCommandInvocation,
    SlashCommandSpec, baseline_slash_commands, execute_agent_shell_command,
    execute_agent_shell_command_with_context, execute_agent_shell_command_with_mcp,
    execute_agent_shell_command_with_permissions, execute_agent_shell_command_with_runtime_context,
    parse_slash_command,
};
pub use turn::{AgentTurnLedger, AgentTurnRecord};

use actions::role_for_source;
use maap::{
    action_content_blocks_from_json_or_text, action_text_content_blocks, json_escape,
    string_array_json, validate_non_empty,
};
use session::{
    agent_shell_help_display, agent_shell_mcp_display, agent_shell_permissions_display,
    agent_shell_status_display,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
