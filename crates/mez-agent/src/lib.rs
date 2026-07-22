//! Provider-independent agent harness and agent protocol state machines.
//!
//! This crate owns model-facing request normalization, MAAP contracts, context
//! assembly, provider behavior, turn negotiation/recovery, action planning,
//! semantic patch planning, scheduler policy, and narrow product integration
//! ports. Product credentials, persistence, transports, process execution, and
//! UI remain behind ports implemented by the root package.

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
/// Provider-independent automatic model-sizing policy.
pub mod auto_sizing;
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
/// Provider-neutral immutable-context continuity diagnostics.
pub mod context_continuity;
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
/// Canonical storage-independent issue records and validation.
pub mod issues;
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
/// Provider-neutral model catalog construction and selection policy.
pub mod model_catalog;
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
/// Sensitive-content-free OpenAI request continuity diagnostics.
pub mod openai_continuity;
/// Provider-independent OpenAI Responses request construction.
pub mod openai_request;
/// Provider-independent OpenAI Responses API response parsing.
pub mod openai_response;
/// OpenAI request-specific MAAP schema construction.
pub mod openai_schema;
/// Provider-independent completion validation and failure recovery policy.
pub mod outcome;
/// Agent-facing permission identity contracts.
pub mod permissions;
/// Provider-independent progress and rationale de-duplication helpers.
pub mod progress;
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
pub mod routed_workflow;
pub mod routing;
/// Provider-independent ambiguous Bubblewrap failure assessment.
pub mod sandbox_assessment;
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
/// Provider-independent shell-wrapper filtering and observation cleanup.
pub mod shell_observation;
/// Structured shell-read observation extraction.
pub mod shell_read_observation;
/// Provider-independent shell-output transport decoding.
pub mod shell_transport;
/// Provider-independent agent skill contracts and parsing.
pub mod skill_workflow;
/// Dependency-neutral agent slash-command registry and parsing.
pub mod slash;
/// Provider-independent subagent cooperation and scope contracts.
pub mod subagent;
/// Provider-independent child-turn result shaping.
pub mod subagent_output;
/// Provider interaction and MAAP action-surface contracts.
pub mod surface;
/// Provider-independent transcript projection and persistence contracts.
pub mod transcript;
/// Provider-independent agent turn-ledger error contracts.
pub mod turn;
/// Provider-independent volatile turn activity and advisory guidance.
pub mod turn_activity;
/// Provider-independent agent turn records and ledger state machine.
pub mod turn_ledger;
/// Canonical provider-independent production turn orchestration.
pub mod turn_runner;

pub use accounting::{
    AgentContextUsageSnapshot, LatestModelRequestUsage, ModelTokenUsage, ModelTokenUsageKey,
    agent_context_usage_snapshot,
};
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
pub use auto_sizing::{
    AutoSizingConfig, AutoSizingDecision, AutoSizingDispatch, AutoSizingError, AutoSizingExecution,
    AutoSizingFallbackPolicy, AutoSizingResult, AutoSizingRoutingPolicy,
    AutoSizingRoutingSelection, AutoSizingSelection, AutoSizingTargetProfile,
    DEFAULT_AUTO_SIZING_FALLBACK_POLICY, DEFAULT_AUTO_SIZING_LARGE_PROFILE,
    DEFAULT_AUTO_SIZING_MEDIUM_PROFILE, DEFAULT_AUTO_SIZING_ROUTER_PROFILE,
    DEFAULT_AUTO_SIZING_SMALL_PROFILE, apply_auto_sizing_execution_profile,
    auto_sizing_fallback_selection, auto_sizing_minimum_context_profile,
    auto_sizing_reasoning_levels_for_profile, auto_sizing_request,
    auto_sizing_selection_from_response,
};
pub use config_change::{
    CONFIG_CHANGE_OPERATION_NAMES, CONFIG_CHANGE_SETTING_PATH_DESCRIPTION,
    CONFIG_CHANGE_VALUE_DESCRIPTION, ConfigChangeError, ConfigChangeMutationSignature,
    ConfigChangeOperation, ConfigChangeValue, config_change_string_value,
    normalize_config_change_operation, parse_config_change_value,
};
pub use context::{
    AgentContext, AgentContextError, AgentContextResult, AgentRequestAssemblyError,
    AgentRequestAssemblyErrorKind, AgentRequestAssemblyResult, ContextBlock, ContextBlockMetadata,
    ContextCachePolicy, ContextEventSequence, ContextExecutionGroupId, ContextPlacement,
    ContextRetention, ContextSemanticKind, ContextSourceKind, ContextStability, ConversationEvent,
    LiveStateBlock, ModelContextCompactionReport, ModelContextMetadata, ModelMessage,
    ModelMessageRole, ModelRequest, PreparedModelContext, ProviderContinuityOwner,
    StableContextBlock, StableContextSlotId, StableContextSourceFingerprint, TrustDomain,
    context_block_is_compaction_summary, context_placement_insertion_index,
    insert_context_block_by_placement, model_context_block_header,
    validate_context_placement_order, validate_context_required, validate_context_semantics,
};
pub use context_appenders::{
    append_mcp_context, append_mcp_context_for_provider, append_memory_context,
    append_permission_policy_context, append_project_guidance_context,
    invoked_mcp_tools_for_context, memory_context_blocks, set_project_guidance_context,
};
pub use context_assembly::{
    ModelRequestIdentity, assemble_model_request_from_context, role_for_context_block,
};
pub use context_compaction::{
    DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT, ModelContextCompactionPlan,
    apply_model_context_compaction_plan, compact_model_context_for_budget,
    compact_model_context_for_budget_at_consumed_sequence,
    compact_model_context_for_budget_with_retained_tail_percent, model_context_text_word_count,
    plan_model_context_compaction_at_consumed_sequence,
};
pub use context_continuity::{
    ContextBlockDiagnostics, ContextContinuityBreakReason, ContextContinuityDiagnostics,
    ContextContinuitySnapshot, ContextSemanticAggregate, ImmutableContextBlockDigest,
    context_continuity_diagnostics, context_continuity_diagnostics_for_interaction,
    context_continuity_snapshot,
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
    AsyncMcpActionExecutor, DEFAULT_AGENT_TURN_TIMEOUT_MS, LocalActionExecutor,
    LocalExecutionOutput, LocalExecutionProjectionError, LocalExecutionRequest,
    LocalExecutionTransport, McpActionExecutor, McpExecutionValidationError, PaneShellExecutor,
    ShellExecutionOutput, ShellExecutionRequest, action_content_blocks_from_json_or_text,
    agent_shell_timeout_ms, agent_turn_remaining_timeout_ms,
    local_execution_output_to_action_result, mcp_response_to_action_result,
    postprocess_local_shell_output, postprocess_shell_action_success_output,
    shell_command_result_content, validate_mcp_execution_request,
};
pub use execution_transcript::{
    AgentTurnExecution, assistant_context_content_for_execution, transcript_entries_for_execution,
};
pub use failure_summary::{
    AgentFailureSummaryNegotiation, AgentFailureSummaryProviderDecision,
    AgentFailureSummaryResponseDecision, FailureSummaryExecutionError,
    failure_summary_execution_from_response,
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
pub use local_action::{
    LocalActionKind, LocalActionPlan, LocalActionPlanningError, LocalActionPlanningResult,
    action_is_local_shell_backed, local_action_plan, local_action_summary,
};
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
    MacroContractError, MacroDefinition, MacroDiagnostic, MacroJudgeDecision, MacroJudgeOutcome,
    MacroManagedSubagent, MacroPromptInvocation, MacroRunPhase, MacroRunRegistration,
    MacroRunState, MacroRunStep, MacroSource, MacroStep, MacroStepTaskResult, MacroSummary,
    ParsedMacroDocument, is_valid_macro_name, macro_initial_step_prompt,
    macro_judge_decision_from_text, macro_judge_model_request, macro_judge_policy,
    macro_judge_task, macro_message_recipient_agent_id, macro_parent_orchestration_prompt,
    macro_run_state, macro_step_model_request, parse_macro_document, parse_macro_prompt_invocation,
    parse_macro_steps,
};
pub use mcp::{
    AgentShellMcpServerSummary, AgentShellMcpSummary, AgentShellMcpToolSummary,
    McpExecutionRequest, McpExecutionResponse, McpPromptServer, McpPromptSummary, McpPromptTool,
    McpPromptUnavailableServer,
};
pub use memory::{MemoryContextRecord, MemoryContextScope};
pub use memory_guardrail::MemoryActionBudget;
pub use model_catalog::{
    ModelAvailability, ModelCatalog, ModelCatalogCandidate, ModelCatalogEntry, ModelCatalogInput,
    ModelCatalogSelection, ModelCatalogSelectionError, ModelCatalogSelectionErrorKind,
    ModelCatalogSource, normalize_model_catalog_values,
};
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
    openai_stable_projection_material_for_request,
};
pub use openai_chat_completions::{
    ChatCompletionsResponseEnvelope, OpenAiChatCompletionsOptions, OpenAiChatCompletionsResponse,
    OpenAiChatCompletionsResponseError, openai_chat_completions_request_body,
    parse_chat_completions_response_envelope, parse_openai_chat_completions_response_body,
};
pub use openai_continuity::{
    OpenAiRequestContinuity, OpenAiRequestContinuitySnapshot, OpenAiRequestMessageDigest,
    compare_openai_request_continuity,
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
    AgentShellPermissionSummary, ApprovalPolicy, PermissionEvaluation, PermissionPlanning,
    PermissionPreset, RuleDecision,
};
pub use progress::{
    RationaleSuppression, normalize_progress_say_entry, normalize_rationale_entry,
    progress_say_entries_are_redundant, progress_say_entries_for_execution,
    progress_say_significant_tokens, progress_say_stem_token, progress_say_token_is_stopword,
    push_progress_say_token, rationale_entries_are_redundant, rationale_entries_for_execution,
    rationale_entry_repeats_existing, suppress_redundant_batch_rationale, truncate_context_entry,
};
pub use prompt::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentPromptAssetSource,
    AgentPromptError, AgentPromptErrorKind, AgentPromptProfile, AgentPromptResult,
    assemble_agent_system_prompt, validate_agent_prompt_required,
};
pub use provider::{
    ANTHROPIC_MESSAGES_API, CHATGPT_RESPONSES_ENDPOINT, DEEPSEEK_CHAT_COMPLETIONS_API,
    MAAP_ACTION_BATCH_TOOL_NAME, OPENAI_CHAT_COMPLETIONS_API, OPENAI_MODELS_ENDPOINT,
    OPENAI_RESPONSES_API, OPENAI_RESPONSES_ENDPOINT, OpenAiPromptCacheDiagnostics,
    OpenAiRenderedMessages, OpenAiRequestOptions, ProviderApiCompatibility,
    ProviderApiCompatibilityError, ProviderCapabilities, ProviderEndpointError,
    ProviderEndpointErrorKind, ProviderEndpointResult, ProviderModelCatalog,
    ProviderModelCatalogParseError, ProviderModelInfo, ProviderRequestAssemblyError,
    ProviderRequestAssemblyErrorKind, ProviderRequestAssemblyResult, ProviderResponseError,
    ProviderResponseErrorKind, ProviderResponseResult, known_model_context_window_tokens,
    known_provider_model_context_window_tokens, openai_auto_sizing_response_format,
    openai_current_action_result_entry_text, openai_current_user_prompt_entry_text,
    openai_default_reasoning_levels_for_model, openai_executed_result_entry_text,
    openai_historical_action_result_entry_text, openai_historical_user_prompt_entry_text,
    openai_macro_judge_response_format, openai_models_endpoint_for_responses_endpoint,
    openai_prompt_cache_diagnostics, openai_prompt_cache_key, openai_render_messages,
    openai_request_options, openai_responses_endpoint_for_base_url,
    openai_routed_handoff_response_format, openai_sandbox_failure_assessment_response_format,
    openai_service_tier_for_latency_preference, openai_stable_projection_material,
    parse_openai_models_http_body, parse_openai_models_http_body_with,
    provider_catalog_reasoning_levels, resolve_provider_api, validate_provider_request_required,
};
pub use provider_diagnostics::{
    ProviderMalformedOutputError, provider_error_detail, provider_failure_event_json,
    provider_failure_json, provider_malformed_output_error, provider_malformed_output_failure_json,
    provider_malformed_output_hint,
};
pub use provider_error::{
    DEFAULT_PROVIDER_RETRY_POLICY, ProviderErrorKind, ProviderErrorRetryClass, ProviderRetryPolicy,
    classify_provider_error_retry,
};
pub use provider_transcript::{PROVIDER_TRANSCRIPT_EVENT_MARKER, ProviderTranscriptEvent};
pub use quota::{ProviderQuotaUsage, provider_quota_usage_from_headers};
pub use readiness::{
    BootstrapDecision, PaneReadinessOverride, PaneReadinessOverrideStore, PaneReadinessState,
    ReadinessDecision, ReadinessError, ReadinessErrorKind, ReadinessOverrideRevocation,
    ReadinessResult, decide_bootstrap_before_user_prompt, readiness_decision,
};
pub use response_progress::ProviderResponseProgress;
pub use routing::{
    ModelPreset, PresetRegistry, ProviderConfig, ProviderRegistry, ProviderRoutingError,
    ProviderRoutingResult,
};
pub use sandbox_assessment::{
    SANDBOX_FAILURE_ASSESSMENT_OUTPUT_MAX_BYTES, SandboxFailureAssessment,
    SandboxFailureAssessmentClass, SandboxFailureAssessmentError, SandboxFailureAssessmentEvidence,
    sandbox_failure_assessment_from_text, sandbox_failure_assessment_request,
};
pub use scheduler::{
    AgentScheduler, DEFAULT_MAX_CONCURRENT_AGENTS, ProviderRetryDispatchResult,
    ProviderRetryEffect, ProviderRetryEvent, ProviderRetryRecovery, ProviderRetryRecoveryResult,
    ProviderRetryScheduler, ProviderRetryTransition, RunningWork, ScheduledWork, ScheduledWorkKind,
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
    ShellChildArgument, ShellChildLaunch, ShellClassification, ShellTransaction,
    ShellTransactionInput, ShellTransactionOutputTransport, ToolDiscoveryCache, ToolInventory,
    ToolProbe, agent_subshell_enter_command, bootstrap_script, bootstrap_script_for_classification,
    fish_bootstrap_script, fish_quote, fish_tool_discovery_script, parse_bootstrap_env_output,
    posix_shell_history_suppression_finish, posix_shell_history_suppression_start,
    readiness_probe_command_for_classification, shell_command_contains_unquoted_heredoc,
    shell_command_invokes_semantic_action, shell_quote, tool_discovery_script,
    validate_agent_authored_shell_command, validate_resolved_shell_path,
    validate_shell_marker_token,
};
pub use shell_read_observation::{
    ShellReadObservation, ShellReadObservationKind, ShellReadRange,
    shell_read_observations_for_command,
};
pub use shell_transport::{
    SHELL_OUTPUT_BASE64_BEGIN_MARKER, SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER,
    SHELL_OUTPUT_BASE64_END_MARKER, SHELL_STATUS_BASE64_BEGIN_MARKER,
    SHELL_STATUS_BASE64_END_MARKER, ShellStatusTransportError, ShellTransportDecodeResult,
    ShellTransportDiagnostics, decode_shell_output_transport,
    decode_shell_output_transport_with_diagnostics, decode_shell_status_transport,
};
pub use skill_workflow::{
    ParsedSkillDocument, SKILL_ADDITIONAL_CONTEXT_HEADING, SKILL_FILE_NAME, SkillActionContext,
    SkillActionPlan, SkillCatalog, SkillContractError, SkillDiagnostic, SkillDocument,
    SkillPromptInvocation, SkillSource, SkillSummary, parse_skill_document,
    parse_skill_prompt_invocation, plan_skill_action, skill_action_context_from_blocks,
    skill_context_text, skill_load_action_result, split_skill_front_matter,
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
    normalize_subagent_spawn_role, subagent_action_scope_violation,
};
pub use subagent_output::subagent_task_output_for_execution;
pub use surface::{AgentCapability, AllowedAction, AllowedActionSet, ModelInteractionKind};
pub use transcript::{
    TRANSCRIPT_CONTEXT_EVENT_MARKER, TranscriptContextEvent, TranscriptContractError,
    TranscriptEntry, TranscriptPersistence, TranscriptRole,
};
pub use turn::{
    AgentTurnLedgerError, AgentTurnLedgerErrorKind, AgentTurnLedgerResult, AgentTurnState,
    AgentTurnTrigger, validate_turn_required,
};
pub use turn_activity::{
    AgentNetworkActionHistory, AgentShellDispatchHistory, AgentTurnSteering,
    agent_turn_steering_context_content, shell_command_looks_like_validation,
};
pub use turn_ledger::{AgentTurnLedger, AgentTurnRecord};
pub use turn_runner::{
    AgentTurnEnvironment, AgentTurnProviderFailure, DEFAULT_MAAP_REPAIR_ATTEMPT_LIMIT,
    apply_model_request_control, run_agent_turn_async, select_model_interaction_kind,
};
