//! Provider-independent agent harness and agent protocol state machines.
//!
//! This crate owns model-facing request normalization, MAAP contracts, context
//! assembly, provider behavior, turn negotiation/recovery, action planning,
//! semantic patch planning, scheduler policy, and narrow product integration
//! ports. Product credentials, persistence, transports, process execution, and
//! UI remain behind ports implemented by the root package.

use std::collections::BTreeSet;

/// Provider-neutral model token accounting contracts.
pub mod accounting;
/// Default provider-facing MAAP action-surface policy.
pub mod action_gates;
/// Provider-independent initial action-result planning.
pub mod action_planning;
/// Provider-independent MAAP action recovery policy.
pub mod action_recovery;
/// Provider-independent MAAP action-result contracts.
pub mod action_result;
/// Provider-independent action-result context and transcript rendering.
pub mod action_result_context;
/// Provider-independent agent-shell session error contracts.
pub mod agent_shell;
/// Provider-independent agent-shell session state and display policy.
pub mod agent_shell_session;
/// Provider-independent Anthropic Messages request shaping.
pub mod anthropic;
/// Provider authentication routing contracts.
pub mod auth;
/// Provider-independent Claude Code CLI policy.
pub mod claude_code;
/// Model-facing live configuration mutation contracts.
pub mod config_change;
/// Provider-independent agent context validation contracts.
pub mod context;
/// Provider-independent context insertion and replacement policy.
pub mod context_appenders;
/// Provider-independent model-request assembly from canonical context.
pub mod context_assembly;
/// Provider-independent model-context compaction and budgeting.
pub mod context_compaction;
/// Skill-related model-context action-surface constraints.
pub mod context_skills;
/// Provider-independent capability-continuation decisions.
pub mod continuation;
/// Provider-independent DeepSeek endpoint and protocol policy.
pub mod deepseek;
/// Provider-independent DeepSeek Chat Completions response parsing.
pub mod deepseek_response;
/// Provider-independent action execution ports and transport records.
pub mod execution;
/// Provider-independent agent execution transcript projection.
pub mod execution_transcript;
/// Provider-neutral terminal failure-summary progression.
pub mod failure_summary;
/// Provider-independent complete agent turn orchestration.
pub mod harness;
/// Provider-neutral HTTP request and response contracts.
pub mod http;
/// Dependency-neutral project instruction discovery records and parsing.
pub mod instructions;
/// Provider-independent local-action execution plans.
pub mod local_action;
/// Provider-independent MAAP action batches, parsing, and validation.
pub mod maap;
#[cfg(test)]
mod maap_protocol_tests;
/// Provider-independent agent macro contracts and parsing.
pub mod macro_workflow;
/// Dependency-neutral MCP prompt manifest records.
pub mod mcp;
/// Prompt-facing memory context contracts.
pub mod memory;
/// Per-turn persistent-memory action guardrails.
pub mod memory_guardrail;
/// Deterministic local-agent message protocol and delivery service state.
pub mod messaging;
/// Provider-independent model profile records and selection policy.
pub mod model_profile;
/// Provider-independent successful model response contract.
pub mod model_response;
/// Provider-independent network action planning and result shaping.
pub mod network_action;
/// OpenAI request rendering and prompt-cache diagnostics.
pub mod openai_cache;
/// Provider-independent OpenAI-compatible Chat Completions request shaping.
pub mod openai_chat_completions;
/// Provider-independent OpenAI Responses request construction.
pub mod openai_request;
/// Provider-independent OpenAI Responses API response parsing.
pub mod openai_response;
/// OpenAI request-specific MAAP schema construction.
pub mod openai_schema;
/// Agent-facing permission identity contracts.
pub mod permissions;
/// Provider-neutral prompt profile contracts.
pub mod prompt;
/// Provider-neutral API compatibility contracts.
pub mod provider;
/// Secret-safe provider failure diagnostic shaping.
pub mod provider_diagnostics;
/// Provider failure retry and recovery classification.
pub mod provider_error;
/// Provider-native transcript continuity contracts.
pub mod provider_transcript;
/// Dependency-neutral provider quota accounting contracts.
pub mod quota;
/// Provider-independent pane readiness state and override policy.
pub mod readiness;
/// Provider-response accounting across one agent turn.
pub mod response_progress;
/// Provider-independent agent scheduling policy and queue state.
pub mod scheduler;
/// Provider-neutral MAAP action-batch schema construction.
pub mod schema;
/// Deterministic semantic-patch parsing and path validation.
pub mod semantic_patch;
/// Deterministic semantic-patch matching and shell transaction planning.
pub mod semantic_patch_planning;
/// Provider-independent shell-source construction helpers.
pub mod shell;
/// Structured shell-read observation extraction.
pub mod shell_read_observation;
/// Provider-independent shell-output transport decoding.
pub mod shell_transport;
/// Dependency-neutral agent slash-command registry and parsing.
pub mod slash;
/// Provider-independent subagent cooperation and scope contracts.
pub mod subagent;
/// Provider interaction and MAAP action-surface contracts.
pub mod surface;
/// Provider-independent transcript projection and persistence contracts.
pub mod transcript;
/// Provider-independent agent turn-ledger error contracts.
pub mod turn;
/// Provider-independent agent turn records and ledger state machine.
pub mod turn_ledger;
/// Canonical provider-independent production turn orchestration.
pub mod turn_runner;

pub use accounting::{AgentContextUsageSnapshot, ModelTokenUsage, ModelTokenUsageKey};
pub use action_gates::apply_default_action_gates;
pub use action_planning::{
    ActionPlanningError, ActionPlanningInput, ActionPlanningResult, PlannedBatchActionResults,
    action_auto_allow_reason, action_supports_auto_allow, failed_turn_execution_without_batch,
    plan_action_result, plan_batch_action_results, plan_turn_execution_from_batch,
    say_action_structured_content_json, shell_action_structured_content_json,
};
pub use action_recovery::{
    ActionRecoveryError, ActionRecoveryResult, BatchContinuationError, BatchContinuationInput,
    BatchContinuationPlan, BatchContinuationRejection, BatchValidationFailure,
    capability_continuation_request, capability_requests_from_batch,
    disallowed_action_capability_continuation_request, maap_repair_request,
    mixed_capability_continuation_request, plan_batch_continuation, validate_batch_allowed_actions,
};
pub use action_result::{
    ActionContentBlock, ActionError, ActionResult, ActionResultContractError,
    ActionResultContractResult, ActionStatus, AgentActionResultIdentity, AgentTurnResultIdentity,
    action_text_content_blocks, turn_state_from_action_results,
};
pub use action_result_context::{action_result_context_content, action_result_transcript_content};
pub use agent_shell::{
    AgentShellSessionError, AgentShellSessionErrorKind, AgentShellSessionResult,
    validate_agent_shell_required,
};
pub use agent_shell_session::{
    AgentLogLevel, AgentShellSession, AgentShellStore, AgentShellVisibility,
    agent_shell_help_display, agent_shell_mcp_display, agent_shell_permissions_display,
    agent_shell_status_display, agent_shell_visibility_name, approval_policy_name,
    permission_preset_name,
};
pub use anthropic::{
    ANTHROPIC_MESSAGES_ENDPOINT, AnthropicMessagesOptions, AnthropicMessagesResponse,
    AnthropicResponseError, DEFAULT_ANTHROPIC_MAX_TOKENS, DEFAULT_ANTHROPIC_PROMPT_CACHING,
    DEFAULT_ANTHROPIC_VERSION, anthropic_messages_endpoint_for_base_url,
    anthropic_messages_request_body, anthropic_provider_failure_json,
    anthropic_request_requires_maap, parse_anthropic_messages_provider_body,
};
pub use auth::{ProviderAuthMetadata, ProviderCredentialKind, ProviderCredentialSource};
pub use claude_code::{
    CLAUDE_CODE_EMPTY_OUTPUT_RETRY_INSTRUCTION, CLAUDE_CODE_MAAP_RETRY_INSTRUCTION,
    CLAUDE_CODE_STRUCTURED_OUTPUT_TOOL, ClaudeCodeOutput, ClaudeCodeResponseError,
    ClaudeCodeResponseResult, ClaudeCodeSessionErrorKind, bound_claude_code_text,
    claude_code_auto_sizing_json_schema, claude_code_corrective_retry_instruction,
    claude_code_empty_output_error, claude_code_maap_json_schema,
    claude_code_macro_judge_json_schema, claude_code_prompt, claude_code_resume_prompt,
    claude_code_session_error_kind, claude_code_session_id, claude_code_system_prompt,
    parse_claude_code_json_output, parse_claude_code_maap_output, redact_claude_code_text,
    validate_claude_code_auto_sizing_output,
};
pub use config_change::{
    CONFIG_CHANGE_OPERATION_NAMES, CONFIG_CHANGE_SETTING_PATH_DESCRIPTION,
    CONFIG_CHANGE_VALUE_DESCRIPTION,
};
pub use context::{
    AgentContext, AgentContextError, AgentContextResult, AgentRequestAssemblyError,
    AgentRequestAssemblyErrorKind, AgentRequestAssemblyResult, ContextBlock, ContextCachePolicy,
    ContextSourceKind, ContextStability, ModelContextCompactionReport, ModelMessage,
    ModelMessageRole, ModelRequest, TrustDomain, model_context_block_header,
    validate_context_required,
};
pub use context_appenders::{
    append_mcp_context, append_memory_context, append_permission_policy_context,
    append_project_guidance_context, append_scheduler_context, invoked_mcp_tools_for_context,
    set_project_guidance_context,
};
pub use context_assembly::{
    ModelRequestIdentity, assemble_model_request_from_context, role_for_context_source,
};
pub use context_compaction::{
    DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT, compact_model_context_for_budget,
    compact_model_context_for_budget_with_retained_tail_percent, model_context_text_word_count,
};
pub use context_skills::constrain_skill_actions_for_loaded_context;
pub use continuation::{
    CapabilityAvailability, CapabilityDecision, CapabilityRequest, ProviderResponseAcceptance,
    accept_provider_response, continuation_surface, decide_capabilities,
};
pub use deepseek::{
    DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME, DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME,
    DEEPSEEK_CHAT_COMPLETIONS_ENDPOINT, DEEPSEEK_MODELS_ENDPOINT,
    DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME, DeepSeekMaapRequestStrategy,
    deepseek_chat_completions_endpoint_for_base_url,
    deepseek_chat_completions_request_body_with_strategy, deepseek_effective_stream,
    deepseek_maap_request_strategy, deepseek_models_endpoint_for_base_url,
    deepseek_should_retry_with_forced_maap,
};
pub use deepseek_response::{
    DeepSeekResponse, DeepSeekResponseError, DeepSeekResponseResult,
    deepseek_request_requires_maap, parse_deepseek_chat_completions_provider_body,
};
pub use execution::{
    AsyncMcpActionExecutor, LocalActionExecutor, LocalExecutionOutput, LocalExecutionRequest,
    LocalExecutionTransport, McpActionExecutor, PaneShellExecutor, ShellExecutionOutput,
    ShellExecutionRequest,
};
pub use execution_transcript::{
    AgentTurnExecution, assistant_context_content_for_execution, transcript_entries_for_execution,
};
pub use failure_summary::{
    AgentFailureSummaryNegotiation, AgentFailureSummaryProviderDecision,
    AgentFailureSummaryResponseDecision,
};
pub use harness::{
    AgentTurnNegotiation, AgentTurnProviderFailureDecision, AgentTurnRecoveryBudget,
    AgentTurnResponseDecision,
};
pub use http::{
    DEFAULT_PROVIDER_MAX_RESPONSE_BYTES, DEFAULT_PROVIDER_TIMEOUT_MS, ProviderHttpError,
    ProviderHttpErrorKind, ProviderHttpRequest, ProviderHttpResponse, ProviderHttpResult,
    ProviderSseTerminalDetector, SseEvent, SseParseError, parse_sse_events, parse_sse_events_with,
};
pub use local_action::{LocalActionKind, LocalActionPlan};
pub use maap::{
    AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE, AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
    AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, AgentAction, AgentActionPayload, MaapBatch,
    MaapContractError, MaapContractResult, MaapValidationContext, SayStatus,
    agent_output_content_type_is_diff, agent_output_content_type_is_markdown, is_valid_skill_name,
    normalize_agent_output_content_type, parse_fenced_maap_action_batch,
    parse_fenced_maap_action_batch_for_turn, parse_maap_action_batch_json,
    parse_maap_action_batch_json_for_turn,
};
pub use macro_workflow::{
    MACRO_FILE_NAME, MACRO_STEPS_HEADING, MAX_MACRO_FILE_BYTES, MAX_MACRO_STEPS, MacroCatalog,
    MacroContractError, MacroDefinition, MacroDiagnostic, MacroPromptInvocation, MacroSource,
    MacroStep, MacroSummary, ParsedMacroDocument, is_valid_macro_name, parse_macro_document,
    parse_macro_prompt_invocation, parse_macro_steps,
};
pub use mcp::{
    AgentShellMcpServerSummary, AgentShellMcpSummary, AgentShellMcpToolSummary,
    McpExecutionRequest, McpExecutionResponse, McpPromptServer, McpPromptSummary, McpPromptTool,
    McpPromptUnavailableServer,
};
pub use memory::{MemoryContextRecord, MemoryContextScope};
pub use memory_guardrail::MemoryActionBudget;
pub use model_profile::{
    ModelProfile, ModelProfileOverrideSource, ModelProfileOverrides, SelectedModelProfile,
    select_model_profile, validate_model_profile_request,
};
pub use model_response::ModelResponse;
pub use network_action::{
    NetworkActionPlan, NetworkActionPlanError, NetworkActionPlanResult, network_action_plan,
    network_action_structured_content_json, network_action_summary,
};
pub use openai_cache::{
    openai_prompt_cache_diagnostics_for_request,
    openai_prompt_cache_diagnostics_for_request_with_stream,
    openai_stable_prefix_material_for_request,
};
pub use openai_chat_completions::{
    ChatCompletionsResponseEnvelope, OpenAiChatCompletionsOptions, OpenAiChatCompletionsResponse,
    OpenAiChatCompletionsResponseError, openai_chat_completions_request_body,
    parse_chat_completions_response_envelope, parse_openai_chat_completions_response_body,
};
pub use openai_request::{
    openai_responses_request_body, openai_responses_request_body_with_stream,
};
pub use openai_response::{
    parse_openai_responses_http_body, parse_openai_responses_provider_body,
    parse_openai_responses_stream_body,
};
pub use openai_schema::openai_maap_current_action_batch_description;
pub use permissions::{
    AgentShellPermissionSummary, ApprovalPolicy, PermissionPlanning, PermissionPreset, RuleDecision,
};
pub use prompt::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentPromptAssetSource,
    AgentPromptError, AgentPromptErrorKind, AgentPromptProfile, AgentPromptResult,
    assemble_agent_system_prompt, validate_agent_prompt_required,
};
pub use provider::{
    ANTHROPIC_MESSAGES_API, CHATGPT_RESPONSES_ENDPOINT, CLAUDE_CODE_API,
    DEEPSEEK_CHAT_COMPLETIONS_API, MAAP_ACTION_BATCH_TOOL_NAME, OPENAI_CHAT_COMPLETIONS_API,
    OPENAI_MODELS_ENDPOINT, OPENAI_RESPONSES_API, OPENAI_RESPONSES_ENDPOINT,
    OpenAiPromptCacheDiagnostics, OpenAiRenderedMessages, OpenAiRequestOptions,
    ProviderApiCompatibility, ProviderApiCompatibilityError, ProviderCapabilities,
    ProviderEndpointError, ProviderEndpointErrorKind, ProviderEndpointResult, ProviderModelCatalog,
    ProviderModelCatalogParseError, ProviderModelInfo, ProviderRequestAssemblyError,
    ProviderRequestAssemblyErrorKind, ProviderRequestAssemblyResult, ProviderResponseError,
    ProviderResponseErrorKind, ProviderResponseResult, known_model_context_window_tokens,
    known_provider_model_context_window_tokens, openai_allowed_action_surface_message,
    openai_auto_sizing_response_format, openai_current_action_result_entry_text,
    openai_current_user_prompt_entry_text, openai_default_reasoning_levels_for_model,
    openai_executed_result_entry_text, openai_historical_action_result_entry_text,
    openai_historical_user_prompt_entry_text, openai_macro_judge_response_format,
    openai_models_endpoint_for_responses_endpoint, openai_prompt_cache_diagnostics,
    openai_prompt_cache_key, openai_render_messages, openai_request_options,
    openai_responses_endpoint_for_base_url, openai_service_tier_for_latency_preference,
    openai_stable_prefix_material, parse_openai_models_http_body_with,
    provider_catalog_reasoning_levels, resolve_provider_api, validate_provider_request_required,
};
pub use provider_diagnostics::{
    ProviderMalformedOutputError, provider_error_detail, provider_failure_event_json,
    provider_failure_json, provider_malformed_output_error, provider_malformed_output_failure_json,
    provider_malformed_output_hint,
};
pub use provider_error::{
    ProviderErrorKind, ProviderErrorRetryClass, classify_provider_error_retry,
};
pub use provider_transcript::{PROVIDER_TRANSCRIPT_EVENT_MARKER, ProviderTranscriptEvent};
pub use quota::{ProviderQuotaUsage, provider_quota_usage_from_headers};
pub use readiness::{
    BootstrapDecision, PaneReadinessOverride, PaneReadinessOverrideStore, PaneReadinessState,
    ReadinessDecision, ReadinessError, ReadinessErrorKind, ReadinessOverrideRevocation,
    ReadinessResult, decide_bootstrap_before_user_prompt, readiness_decision,
};
pub use response_progress::ProviderResponseProgress;
pub use scheduler::{
    AgentScheduler, DEFAULT_MAX_CONCURRENT_AGENTS, RunningWork, ScheduledWork, ScheduledWorkKind,
    SchedulerCancellation, SchedulerError, SchedulerErrorKind, SchedulerResult, SchedulerSnapshot,
    runnable_agent_ids,
};
pub use schema::{
    OpenAiMaapToolSurface, maap_action_batch_schema, maap_current_action_batch_description,
    maap_mcp_call_action_schema_for_tool, mcp_tool_manifest_for_description,
    normalize_openai_strict_schema,
};
pub use semantic_patch::{is_mez_patch_payload, validate_apply_patch_payload};
pub use shell::{
    AgentShellValidationError, AgentShellValidationErrorKind, AgentShellValidationResult,
    DEFAULT_BOOTSTRAP_TIMEOUT_MS, DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, EnvironmentSignature,
    MarkerToken, SHELL_OUTPUT_BASE64_MAX_RAW_BYTES, SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES,
    ShellClassification, ShellTransaction, ShellTransactionInput, ShellTransactionOutputTransport,
    ToolDiscoveryCache, ToolInventory, ToolProbe, agent_subshell_enter_command, bootstrap_script,
    bootstrap_script_for_classification, fish_bootstrap_script, fish_quote,
    fish_tool_discovery_script, parse_bootstrap_env_output, posix_shell_history_suppression_finish,
    posix_shell_history_suppression_start, readiness_probe_command_for_classification,
    shell_command_contains_unquoted_heredoc, shell_command_invokes_semantic_action, shell_quote,
    tool_discovery_script, validate_agent_authored_shell_command, validate_resolved_shell_path,
    validate_shell_marker_token,
};
pub use shell_read_observation::{
    ShellReadObservation, ShellReadObservationKind, ShellReadRange,
    shell_read_observations_for_command,
};
pub use shell_transport::{
    SHELL_OUTPUT_BASE64_BEGIN_MARKER, SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER,
    SHELL_OUTPUT_BASE64_END_MARKER, ShellTransportDecodeResult, ShellTransportDiagnostics,
    decode_shell_output_transport, decode_shell_output_transport_with_diagnostics,
};
pub use slash::{
    SlashCommandEffect, SlashCommandInvocation, SlashCommandParseError, SlashCommandSpec,
    baseline_slash_commands, parse_slash_command,
};
pub use subagent::{
    ActiveWriteScope, BuiltinSubagentRole, CooperationMode, DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
    DefaultSubagentScopeEnforcement, ScopeConflict, ScopeRegistry, SubagentContractError,
    SubagentContractErrorKind, SubagentContractResult, SubagentProfile, SubagentScopeDeclaration,
    SubagentScopeEnforcement, SubagentSpawnRequest, builtin_role_name, builtin_subagent_profiles,
};
pub use surface::{AgentCapability, AllowedAction, AllowedActionSet, ModelInteractionKind};
pub use transcript::{
    AgentTranscriptEntry, AgentTranscriptRole, TranscriptContractError, TranscriptPersistence,
};
pub use turn::{
    AgentTurnLedgerError, AgentTurnLedgerErrorKind, AgentTurnLedgerResult, AgentTurnState,
    AgentTurnTrigger, validate_turn_required,
};
pub use turn_ledger::{AgentTurnLedger, AgentTurnRecord};
pub use turn_runner::{
    AgentTurnEnvironment, AgentTurnProviderFailure, DEFAULT_MAAP_REPAIR_ATTEMPT_LIMIT,
    run_agent_turn_async,
};

/// Maximum number of issue records a model-authored query may request.
pub const MAX_ISSUE_QUERY_LIMIT: u64 = 200;

/// Borrowed fields used to validate one model-authored issue update.
#[derive(Debug, Clone, Copy)]
pub struct IssueUpdateValidation<'a> {
    /// Optional replacement issue kind.
    pub kind: Option<&'a str>,
    /// Optional replacement issue state.
    pub state: Option<&'a str>,
    /// Optional replacement title.
    pub title: Option<&'a str>,
    /// Optional replacement body.
    pub body: Option<&'a str>,
    /// Whether the existing body should be removed.
    pub clear_body: bool,
    /// Optional replacement progress notes.
    pub notes: Option<&'a str>,
    /// Whether the existing notes should be removed.
    pub clear_notes: bool,
    /// Optional replacement dependency identifiers.
    pub depends_on: Option<&'a [String]>,
    /// Whether existing dependencies should be removed.
    pub clear_depends_on: bool,
}

/// Borrowed fields used to validate one model-authored issue query.
#[derive(Debug, Clone, Copy)]
pub struct IssueQueryValidation<'a> {
    /// Optional issue kind filter.
    pub kind: Option<&'a str>,
    /// Optional issue state filter.
    pub state: Option<&'a str>,
    /// Optional title/body substring filter.
    pub text: Option<&'a str>,
    /// Optional maximum result count.
    pub limit: Option<u64>,
}

/// Validates the stable model-facing issue kind grammar.
pub fn validate_issue_kind(kind: &str) -> Result<(), String> {
    if matches!(kind, "defect" | "task") {
        Ok(())
    } else {
        Err("issue kind must be defect or task".to_string())
    }
}

/// Validates the stable model-facing issue state grammar.
pub fn validate_issue_state(state: &str) -> Result<(), String> {
    if matches!(state, "open" | "resolved") {
        Ok(())
    } else {
        Err("issue state must be open or resolved".to_string())
    }
}

/// Validates a model-authored issue title.
pub fn validate_issue_title(title: &str) -> Result<(), String> {
    validate_non_empty_single_line("issue title", title)
}

/// Validates optional model-authored issue body text.
pub fn validate_issue_body(body: Option<&str>) -> Result<(), String> {
    validate_optional_text("issue body", body)
}

/// Validates optional model-authored issue progress notes.
pub fn validate_issue_notes(notes: Option<&str>) -> Result<(), String> {
    validate_optional_text("issue notes", notes)
}

/// Validates model-authored dependency identifiers before product lookup.
pub fn validate_issue_dependency_ids(depends_on: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for dependency_id in depends_on {
        if dependency_id.trim().is_empty() || dependency_id.bytes().any(|byte| byte == 0) {
            return Err("issue dependency id must not be empty".to_string());
        }
        if !seen.insert(dependency_id.as_str()) {
            return Err("issue dependencies must not contain duplicates".to_string());
        }
    }
    Ok(())
}

/// Validates one model-authored issue update without depending on persistence types.
pub fn validate_issue_update(update: IssueUpdateValidation<'_>) -> Result<(), String> {
    let has_changes = update.kind.is_some()
        || update.state.is_some()
        || update.title.is_some()
        || update.body.is_some()
        || update.clear_body
        || update.notes.is_some()
        || update.clear_notes
        || update.depends_on.is_some()
        || update.clear_depends_on;
    if !has_changes {
        return Err("issue update requires at least one field to change".to_string());
    }
    if update.body.is_some() && update.clear_body {
        return Err("issue update cannot set and clear body".to_string());
    }
    if update.notes.is_some() && update.clear_notes {
        return Err("issue update cannot set and clear notes".to_string());
    }
    if update.depends_on.is_some() && update.clear_depends_on {
        return Err("issue update cannot set and clear dependencies".to_string());
    }
    if let Some(kind) = update.kind {
        validate_issue_kind(kind)?;
    }
    if let Some(state) = update.state {
        validate_issue_state(state)?;
    }
    if let Some(title) = update.title {
        validate_issue_title(title)?;
    }
    validate_issue_body(update.body)?;
    validate_issue_notes(update.notes)?;
    if let Some(depends_on) = update.depends_on {
        validate_issue_dependency_ids(depends_on)?;
    }
    Ok(())
}

/// Validates one model-authored issue query without depending on the issue store.
pub fn validate_issue_query(query: IssueQueryValidation<'_>) -> Result<(), String> {
    if let Some(kind) = query.kind {
        validate_issue_kind(kind)?;
    }
    if let Some(state) = query.state {
        validate_issue_state(state)?;
    }
    validate_optional_text("issue query text", query.text)?;
    if let Some(limit) = query.limit {
        if limit == 0 {
            return Err("issue query limit must be greater than zero".to_string());
        }
        if limit > MAX_ISSUE_QUERY_LIMIT {
            return Err(format!(
                "issue query limit must be less than or equal to {MAX_ISSUE_QUERY_LIMIT}"
            ));
        }
    }
    Ok(())
}

fn validate_optional_text(label: &str, value: Option<&str>) -> Result<(), String> {
    if value.is_some_and(|value| value.bytes().any(|byte| byte == 0)) {
        return Err(format!("{label} must not contain NUL bytes"));
    }
    Ok(())
}

fn validate_non_empty_single_line(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(format!("{label} must be a single line without NUL bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        IssueQueryValidation, IssueUpdateValidation, validate_issue_query, validate_issue_update,
    };

    #[test]
    /// Verifies valid model-authored issue updates and queries are accepted at
    /// the dependency-neutral agent protocol boundary.
    fn issue_action_validation_accepts_valid_fields() {
        let dependencies = vec!["issue-1".to_string()];
        validate_issue_update(IssueUpdateValidation {
            kind: Some("task"),
            state: Some("open"),
            title: Some("Implement agent contract"),
            body: None,
            clear_body: false,
            notes: Some("in progress"),
            clear_notes: false,
            depends_on: Some(&dependencies),
            clear_depends_on: false,
        })
        .unwrap();
        validate_issue_query(IssueQueryValidation {
            kind: Some("defect"),
            state: Some("resolved"),
            text: Some("agent"),
            limit: Some(200),
        })
        .unwrap();
    }

    #[test]
    /// Verifies model-authored issue validation rejects conflicting updates,
    /// duplicate dependencies, invalid grammar, and out-of-range queries.
    fn issue_action_validation_rejects_invalid_fields() {
        let duplicate_dependencies = vec!["issue-1".to_string(), "issue-1".to_string()];
        let error = validate_issue_update(IssueUpdateValidation {
            kind: None,
            state: None,
            title: None,
            body: Some("replacement"),
            clear_body: true,
            notes: None,
            clear_notes: false,
            depends_on: Some(&duplicate_dependencies),
            clear_depends_on: false,
        })
        .unwrap_err();
        assert!(error.contains("set and clear body"), "{error}");

        for query in [
            IssueQueryValidation {
                kind: Some("bug"),
                state: None,
                text: None,
                limit: None,
            },
            IssueQueryValidation {
                kind: None,
                state: Some("closed"),
                text: None,
                limit: None,
            },
            IssueQueryValidation {
                kind: None,
                state: None,
                text: Some("bad\0query"),
                limit: None,
            },
            IssueQueryValidation {
                kind: None,
                state: None,
                text: None,
                limit: Some(201),
            },
        ] {
            assert!(validate_issue_query(query).is_err());
        }
    }
}
