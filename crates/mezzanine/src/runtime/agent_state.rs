//! Runtime agent provider dispatch and loop state records.
//!
//! This module owns data-only records used by provider-backed agent workers,
//! automatic sizing decisions, loop bookkeeping, compaction, and memory
//! generation. The runtime actor still owns behavior; these records simply
//! describe queued or claimed work across async boundaries.

use super::{
    AgentTurnExecution, AgentTurnRecord, DeepSeekChatCompletionsProvider, MemoryScope,
    ModelProfile, ModelRequest, OpenAiCompatibleChatCompletionsProvider, OpenAiResponsesProvider,
    PathScopes, PermissionPolicy, ReqwestProviderHttpTransport, RuntimeAutoSizingDispatch,
    SessionApprovalStore, SubagentScopeDeclaration,
};
use crate::integrations::agent::provider::AnthropicMessagesProvider;
use crate::runtime::processes::NativeShellContext;
use crate::storage::issues::IssueStore;
use crate::storage::memory::PersistentMemoryStore;
use mez_agent::{
    AutoSizingRoutingSelection, McpPromptTool, PreparedModelContext, ShellExecutionOutput,
    ShellExecutionRequest,
};

/// Carries Runtime Agent Provider Task state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentProviderTask {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the model profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model_profile: ModelProfile,
}

/// One approved external action transferred from the runtime actor to a worker.
///
/// The actor retains turn and execution ownership. The worker receives only
/// the immutable action inputs and, for MCP, the concrete server transport
/// needed to perform the call without borrowing serialized runtime state.
pub(crate) struct RuntimeApprovedExternalActionDispatch {
    /// Turn that owns the approved action.
    pub turn: AgentTurnRecord,
    /// Approved action to execute.
    pub action: mez_agent::AgentAction,
    /// Actor-issued generation that fences this worker's presentation updates.
    pub attempt: String,
    /// Exact attempt cancellation; retirement never authorizes remote replay.
    pub cancellation: crate::runtime::processes::native_cancellation::NativeActionCancellation,
    /// MCP-specific transport state when the action is an MCP call.
    pub mcp: Option<RuntimeApprovedMcpActionDispatch>,
}

/// MCP transport inputs moved into one approved-action worker.
pub(crate) struct RuntimeApprovedMcpActionDispatch {
    /// Validated tool-call plan retained from the actor-owned registry.
    pub plan: mez_agent::mcp::McpToolCallPlan,
    /// Exact live server transport moved out of actor state for the call.
    pub transport: super::RuntimeMcpTransport,
    /// Environment exposed to the configured MCP transport.
    pub environment: std::collections::BTreeMap<String, String>,
    /// Optional credential store used by authenticated HTTP MCP transports.
    pub auth_store: Option<crate::security::auth::AuthStore>,
}

/// Result returned by an approved external-action worker.
pub(crate) struct RuntimeApprovedExternalActionOutcome {
    /// Turn that owns the completed action.
    pub turn_id: String,
    /// Stable action identity within the turn.
    pub action_id: String,
    /// Exact actor-issued generation that produced this outcome.
    pub attempt: String,
    /// Settled result or typed execution failure.
    pub result: crate::error::Result<mez_agent::ActionResult>,
    /// MCP transport returned to actor ownership after the call.
    pub mcp_transport: Option<(String, super::RuntimeMcpTransport)>,
}

/// One bounded presentation update observed by an approved external worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeApprovedExternalActionProgress {
    /// Typed progress fenced by the claimed approved-external attempt.
    pub(crate) presentation: mez_agent::ActionPresentationProgress,
}

/// Immutable spawned-shell work transferred from the runtime actor to a worker.
///
/// The actor retains turn and action ownership while the worker receives only
/// the inferred process context and already-authorized execution request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeNativeShellDispatch {
    /// Turn that owns the native shell action.
    pub(crate) turn_id: String,
    /// Stable action identity within the turn.
    pub(crate) action_id: String,
    /// Exact transaction marker used to fence stale worker completions.
    pub(crate) marker: String,
    /// Monotonic actor/worker-owner cancellation for this exact dispatch.
    pub(crate) cancellation:
        crate::runtime::processes::native_cancellation::NativeActionCancellation,
    /// Inferred shell, environment, and working-directory context.
    pub(crate) context: NativeShellContext,
    /// Uncached backend-tagged capability proof run by the external worker.
    pub(crate) capability_probe: Option<crate::runtime::processes::NativeSandboxCapabilityProbe>,
    /// Whether the worker must stop after capability proof without launching
    /// the user workload.
    pub(crate) capability_probe_only: bool,
    /// Exact backend whose lifecycle evidence must be validated.
    pub(crate) sandbox_backend: Option<crate::runtime::SandboxBackend>,
    /// Managed-home lease retained through worker probe and workload execution.
    pub(crate) bubblewrap_activity_lease:
        Option<crate::runtime::processes::NativeBubblewrapActivityLease>,
    /// Seatbelt action/home/temp cleanup lease retained through execution.
    pub(crate) seatbelt_workload_lease: Option<crate::security::sandbox::SeatbeltWorkloadLease>,
    /// Fully materialized spawned-shell request.
    pub(crate) request: ShellExecutionRequest,
    /// Actor-selected semantic purpose, never inferred from command text.
    pub(crate) launch_reason: crate::runtime::processes::launch_accounting::NativeLaunchReason,
    /// Runtime wall-clock timestamp used by shell completion metrics.
    pub(crate) started_at_unix_ms: u64,
}

/// Typed spawned-shell failure safe to carry through runtime events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeNativeShellFailure {
    /// Stable runtime failure kind used in action-result metadata.
    pub(crate) kind: String,
    /// Human-readable worker failure diagnostic.
    pub(crate) message: String,
    /// Bounded lifecycle evidence; absent for failures unrelated to sandbox status.
    pub(crate) lifecycle: Option<crate::security::sandbox::SandboxLifecycleFailure>,
}

/// Bounded cumulative progress observed while a native shell worker is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeNativeShellWorkerProgress {
    /// Monotonically newer bounded shell-output snapshot.
    pub(crate) output: Option<(u64, String)>,
}

/// One executor presentation update carried from the native worker to the actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeNativeShellProgress {
    /// Typed presentation-only progress fenced by the claimed native attempt.
    pub(crate) presentation: mez_agent::ActionPresentationProgress,
}

/// Result returned by a native shell worker for actor-owned settlement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeNativeShellOutcome {
    /// Turn that owned the dispatched work.
    pub(crate) turn_id: String,
    /// Stable action identity within the turn.
    pub(crate) action_id: String,
    /// Exact marker copied from the claimed dispatch.
    pub(crate) marker: String,
    /// Command retained for audit, history, and apply-patch phase handling.
    pub(crate) command: String,
    /// Runtime wall-clock timestamp captured before worker dispatch.
    pub(crate) started_at_unix_ms: u64,
    /// Successful native capability proof returned for actor-owned caching.
    pub(crate) sandbox_capability: Option<Box<crate::security::sandbox::SandboxCapability>>,
    /// Whether this outcome represents only a capability preflight.
    pub(crate) capability_probe_only: bool,
    /// Direct launch evidence; None means unavailable, not zero launches.
    pub(crate) launch_counts:
        Option<Vec<crate::runtime::processes::launch_accounting::NativeLaunchCount>>,
    /// Normalized shell output or a typed worker failure.
    pub(crate) result: std::result::Result<ShellExecutionOutput, RuntimeNativeShellFailure>,
}

/// Tracks a provider task after the async actor has claimed it from the queue.
///
/// Provider workers run outside the serialized runtime actor. This record gives
/// the actor a finite lease it can enforce if the worker never submits a
/// completion or failure event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeAgentProviderClaim {
    /// Runtime turn owned by the claimed provider worker.
    pub turn_id: String,
    /// Frozen request attribution independent of subsequent pane state.
    pub accounting_origin: crate::storage::token_usage::AccountingOrigin,
    /// Immutable conversation that owns the claimed provider request.
    pub conversation_id: String,
    /// Agent identity that owns the turn.
    pub agent_id: String,
    /// Exact configured provider instance and API that own native continuity.
    pub provider_owner: Option<mez_agent::ProviderContinuityOwner>,
    /// Timer generation associated with the current claim lease.
    pub generation: u64,
    /// Unix timestamp, in milliseconds, when the provider task was claimed.
    pub claimed_at_unix_ms: u64,
    /// Maximum lease duration before the runtime fails the turn.
    pub timeout_ms: u64,
    /// Highest canonical event sequence consumed by the claimed request.
    pub context_event_high_water_mark: u64,
    /// Exact selected dependency generation, recipient index, canonical source
    /// sequence and digest present in the actual ordinary dispatch context.
    /// This volatile proof is never reconstructed from a returned worker body.
    pub request_wait_context: Option<(u64, usize, mez_agent::ContextEventSequence, [u8; 32])>,
    /// Canonical serialized OpenAI Responses body bytes for the claimed request.
    ///
    /// Non-Responses providers leave this unset. Context-limit recovery uses
    /// the value as a strict upper bound for the next provider retry.
    pub openai_request_bytes: Option<usize>,
    /// Streaming mode used to serialize the claimed OpenAI Responses request.
    pub openai_request_stream: Option<bool>,
}

/// Carries Runtime Agent Provider Dispatch Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub enum RuntimeAgentProviderDispatchProvider {
    /// Represents the Open Ai case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    OpenAi(OpenAiResponsesProvider<ReqwestProviderHttpTransport>),
    /// Represents the Deep Seek case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DeepSeek(DeepSeekChatCompletionsProvider<ReqwestProviderHttpTransport>),
    /// Represents the Anthropic Messages case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Anthropic(AnthropicMessagesProvider<ReqwestProviderHttpTransport>),
    /// Represents a named OpenAI-compatible Chat Completions provider.
    ///
    /// Callers use this variant for configured provider instances that share
    /// the Chat Completions wire contract without inheriting native OpenAI
    /// Responses semantics.
    OpenAiCompatible(OpenAiCompatibleChatCompletionsProvider<ReqwestProviderHttpTransport>),
}

impl RuntimeAgentProviderDispatchProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn provider_id(&self) -> &str {
        match self {
            Self::OpenAi(provider) => provider.provider_id(),
            Self::DeepSeek(provider) => provider.provider_id(),
            Self::Anthropic(provider) => provider.provider_id(),
            Self::OpenAiCompatible(provider) => provider.provider_id(),
        }
    }

    /// Returns the exact wire API implemented by this dispatched provider.
    pub(crate) fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        match self {
            Self::OpenAi(_) => mez_agent::ProviderApiCompatibility::OpenAiResponses,
            Self::DeepSeek(_) => mez_agent::ProviderApiCompatibility::DeepSeekChatCompletions,
            Self::Anthropic(_) => mez_agent::ProviderApiCompatibility::AnthropicMessages,
            Self::OpenAiCompatible(_) => mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
        }
    }

    /// Returns the wire streaming mode used to serialize one provider request.
    pub(crate) fn request_stream(&self, request: &ModelRequest) -> bool {
        match self {
            Self::OpenAi(provider) => provider.streams_responses(),
            Self::DeepSeek(provider) => provider.streams_request(request),
            Self::Anthropic(provider) => provider.streams_request(request),
            Self::OpenAiCompatible(provider) => provider.streams_request(request),
        }
    }
}

/// Carries Runtime Agent Provider Dispatch state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct RuntimeAgentProviderDispatch {
    /// Exact actor-assigned generation of the provider claim owning this dispatch.
    pub claim_generation: u64,
    /// Remaining finite error capacity captured by the actor retry owner.
    /// Canonical incomplete retries retain this policy and their exact request.
    pub turn_limits: mez_agent::AgentTurnLimits,
    /// Immutable accounting attribution; never resampled on response settlement.
    pub(crate) accounting_origin: crate::storage::token_usage::AccountingOrigin,
    /// Stores the turn value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn: AgentTurnRecord,
    /// Stores the context value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub context: PreparedModelContext,
    /// Accumulated action surface retained across provider-worker boundaries
    /// for this logical turn.
    pub allowed_actions: Option<mez_agent::AllowedActionSet>,
    /// Controller-selected exceptional interaction for this provider request.
    pub interaction_kind: Option<mez_agent::ModelInteractionKind>,
    /// Stores the model profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model_profile: ModelProfile,
    /// Structured macro judge request awaiting model feedback instead of an
    /// ordinary MAAP action batch.
    pub macro_judge_request: Option<ModelRequest>,
    /// Structured sandbox failure assessment request awaiting model
    /// attribution instead of an ordinary MAAP action batch.
    pub sandbox_failure_assessment_request: Option<ModelRequest>,
    /// Optional automatic sizing context for the worker's first provider step.
    pub auto_sizing: Option<RuntimeAutoSizingDispatch>,
    /// Optional router provider for auto-sizing when different from the main
    /// turn provider. When set, auto-sizing requests use this provider.
    pub auto_sizing_provider: Option<RuntimeAgentProviderDispatchProvider>,
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: RuntimeAgentProviderDispatchProvider,
    /// Stores the permission policy value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub permission_policy: PermissionPolicy,
    /// Whether prompting local actions may attempt Bubblewrap execution before
    /// requesting user approval.
    pub sandbox_first_local_prompts: bool,
    /// Pane-owned shell classification used for both permission analysis and
    /// eventual command rendering during this provider dispatch.
    pub shell_classification: String,
    /// Stores the session approvals value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_approvals: SessionApprovalStore,
    /// Stores the path scopes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub path_scopes: Option<PathScopes>,
    /// Stores the subagent scope value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub subagent_scope: Option<SubagentScopeDeclaration>,
    /// Macro and bridge child agent ids owned by this provider turn.
    ///
    /// Runtime macro and bridge sends stay ungated, so the planning adapter must
    /// know which recipients are runtime-owned orchestration targets rather than
    /// model-chosen peers.
    pub macro_bridge_recipients: Vec<String>,
    /// Stores the available mcp servers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub available_mcp_servers: Vec<String>,
    /// Stores the available mcp tools value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub available_mcp_tools: Vec<McpPromptTool>,
    /// Whether persistent memory actions are enabled for this provider turn.
    ///
    /// Async provider workers execute outside the runtime actor, so the live
    /// memory gate must be carried with the dispatch instead of recomputed from
    /// unavailable actor-owned state.
    pub memory_actions_enabled: bool,
    /// Whether local issue-tracking actions are enabled for this provider turn.
    pub issue_actions_enabled: bool,
    /// Optional `/loop` controller metadata for this provider turn.
    #[allow(
        dead_code,
        reason = "provider dispatch carries loop context across worker ownership"
    )]
    pub loop_turn: Option<RuntimeAgentLoopTurn>,
}

/// Result produced by one provider worker before actor-owned state mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeAgentProviderWorkerOutcome {
    /// A normal provider execution ready for runtime application.
    Execution(Box<AgentTurnExecution>),
    /// A routing decision ready for policy-specific application by the runtime actor.
    RoutingSelected(Box<AutoSizingRoutingSelection>),
}

/// Captured actor-owned identity for one persistence settlement generation.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeAgentProviderPersistenceOwner {
    /// Immutable originating turn, including agent, pane, and conversation identity.
    pub(crate) turn: AgentTurnRecord,
    /// Exact worker generation admitted for this owner.
    pub(crate) generation: u64,
}

impl RuntimeAgentProviderPersistenceOwner {
    /// Checks immutable identity without treating a reported state as authority.
    pub(crate) fn matches_turn(&self, turn: &AgentTurnRecord) -> bool {
        self.turn.turn_id == turn.turn_id
            && self.turn.agent_id == turn.agent_id
            && self.turn.pane_id == turn.pane_id
            && self.turn.conversation_id == turn.conversation_id
    }
}

/// Immutable actor-validated SQLite work for one provider completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentProviderPersistenceWork {
    /// Running turn that owns every action in the execution.
    pub turn: AgentTurnRecord,
    /// Exact persistence settlement generation for rejecting late worker results.
    pub generation: u64,
    /// Effective model profile selected for this execution.
    pub model_profile: ModelProfile,
    /// Provider identity retained for trace and failure reporting.
    pub provider_id: String,
    /// Provider execution whose running memory and issue results need settling.
    pub execution: AgentTurnExecution,
    /// Fork-only read phase; later persistence actions must wait for actor spawn settlement.
    pub(crate) fork_read: Option<crate::runtime::control::RuntimeSubagentForkReadWork>,
    /// Terminal bookkeeping read; actor settlement waits for checked chronology.
    pub(crate) bookkeeping_read: Option<crate::runtime::RuntimeBookkeepingTranscriptReadWork>,
    /// Whether persistent-memory actions were enabled for this execution.
    pub memory_enabled: bool,
    /// Persistent-memory repository when memory actions are available.
    pub memory_store: Option<PersistentMemoryStore>,
    /// Runtime-visible memory scopes for this turn.
    pub memory_scopes: Vec<MemoryScope>,
    /// Default retention applied to memory records without an explicit expiry.
    pub memory_default_ttl_days: u64,
    /// Whether local issue actions were enabled for this execution.
    pub issues_enabled: bool,
    /// Local issue repository when issue actions are available.
    pub issue_store: Option<IssueStore>,
    /// Stable project key used by every issue action in this batch.
    pub issue_project: String,
    /// Successful query fingerprints already observed during this logical turn.
    pub issue_query_freshness: std::collections::BTreeMap<String, String>,
    /// Non-persistence actions settled before the actor deferred SQLite work.
    pub actions_executed_before_persistence: usize,
    /// Terminal action results observed before the SQLite family boundary.
    pub settled_action_results_before_persistence: Vec<mez_agent::ActionResult>,
}

/// Actor-side metadata produced while settling provider persistence actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentProviderPersistenceOutcome {
    /// Running turn that owned the validated persistence work.
    pub turn: AgentTurnRecord,
    /// Exact persistence settlement generation for rejecting late worker results.
    pub generation: u64,
    /// Effective model profile selected for this execution.
    pub model_profile: ModelProfile,
    /// Provider identity retained for trace and failure reporting.
    pub provider_id: String,
    /// Original execution retained until actor-owned family boundaries apply results.
    pub execution: AgentTurnExecution,
    /// Captured fork owner returned for actor freshness validation.
    pub(crate) fork_read: Option<crate::runtime::control::RuntimeSubagentForkReadWork>,
    /// Checked fork history or bounded diagnostic; never authorizes worker-side spawning.
    pub(crate) fork_snapshot:
        Option<std::result::Result<crate::runtime::control::RuntimeSubagentForkSnapshot, String>>,
    /// Exact captured history owner for terminal bookkeeping acceptance.
    pub(crate) bookkeeping_read: Option<crate::runtime::RuntimeBookkeepingTranscriptReadWork>,
    /// Checked turn rows and the store's next sequence, prepared without live runtime state.
    pub(crate) bookkeeping_history: Option<(Vec<mez_agent::TranscriptEntry>, u64)>,
    /// Memory results keyed by their original execution result indexes.
    pub memory_results: Vec<(usize, mez_agent::ActionResult)>,
    /// Issue results keyed by their original execution result indexes.
    pub issue_results: Vec<(usize, mez_agent::ActionResult)>,
    /// Final successful issue-query fingerprints after ordered batch execution.
    pub issue_query_freshness: std::collections::BTreeMap<String, String>,
    /// Whether an issue mutation changed selector-visible records.
    pub issue_records_changed: bool,
    /// Non-persistence actions settled before the actor deferred SQLite work.
    pub actions_executed_before_persistence: usize,
    /// Terminal action results observed before the SQLite family boundary.
    pub settled_action_results_before_persistence: Vec<mez_agent::ActionResult>,
}

/// Identifies the role of one runtime turn owned by a `/loop` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeAgentLoopTurnKind {
    /// A normal work iteration that should attempt to satisfy the original prompt.
    Work,
}

/// Identifies how `/loop` prepares the pane conversation for each work turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeAgentLoopMode {
    /// Run each iteration in the pane's current conversation.
    ReuseCurrentConversation,
    /// Run each iteration in a fresh transcript fork of the parent conversation.
    ForkEachIteration,
    /// Run each iteration in a fresh empty conversation with no parent fork context.
    NewEachIteration,
}

/// Parent macro action waiting for one logical `/loop` controller result.
///
/// Loop work turns are transient and receive a new turn id for every
/// iteration. This record therefore lives on the controller so parent
/// completion cannot be lost when an intermediate iteration settles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentLoopCompletion {
    /// Parent macro orchestration turn waiting for the loop.
    pub parent_turn_id: String,
    /// Parent action that should receive the terminal loop result.
    pub parent_action_id: String,
    /// First loop work turn exposed as the logical macro step identifier.
    pub child_turn_id: String,
    /// Persistent macro child agent executing the loop.
    pub child_agent_id: String,
    /// Human-readable display name assigned to the macro child.
    pub child_display_name: Option<String>,
}

/// Original NoStore parent policy, independent of its copied work conversation.
/// This describes a saved source window only; it grants no execution rights.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeLoopNoStoreSource {
    pub(crate) conversation_id: Option<String>,
    pub(crate) entries: u64,
}

/// Runtime-owned state for one active `/loop` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentLoopState {
    /// Stable identity shared by every work turn in this logical loop.
    pub loop_id: String,
    /// Unix timestamp when the logical loop operation started.
    ///
    /// Each work iteration retains its own turn timestamp and deadline for
    /// execution policy, while user-facing elapsed time spans the full loop.
    pub operation_started_at_unix_seconds: u64,
    /// Pane whose visible agent shell invoked and presents the loop.
    pub invoking_pane_id: String,
    /// Pane that currently executes loop work turns.
    pub execution_pane_id: String,
    /// Original user prompt supplied after `/loop`.
    pub original_prompt: String,
    /// Optional semantic completion goal evaluated by the model after each
    /// work iteration instead of using the default patch-emission heuristic.
    pub goal: Option<String>,
    /// Conversation preparation mode for loop-owned work turns.
    pub mode: RuntimeAgentLoopMode,
    /// Parent conversation id restored after fresh loop iterations and used as
    /// the fork source when applicable.
    pub parent_conversation_id: String,
    /// Exact parent's immutable native workspace and instruction discovery,
    /// captured before ephemeral rebinding. This is contextual/launch data,
    /// never filesystem or cleanup authority.
    pub parent_native_directory: Option<mez_agent::AgentNativeWorkingDirectory>,
    /// Ephemeral conversation installed by this controller's latest successful
    /// work binding. History/source labels cannot substitute for this identity.
    pub work_conversation_id: Option<String>,
    /// Durable parent transcript count to restore after ephemeral loop forks.
    pub parent_transcript_entries: u64,
    /// Frozen logical archive frontier, independent of retained replay rows.
    pub parent_transcript_high_water: u64,
    /// Actual typed source captured before a forked NoStore iteration binds.
    /// This in-memory value is never reconstructed from the numeric frontier.
    pub(crate) parent_frozen_replay: Option<crate::runtime::control::RuntimeFrozenReplayHistory>,
    /// Original legacy projection for isolated iterations, with no invented
    /// typed head and no subsequent reads of the parent's later archive.
    pub(crate) parent_legacy_history: Option<crate::runtime::commands::RuntimeLoopHistory>,
    /// Restoring an unsaved parent must preserve its policy and original source.
    pub(crate) parent_no_store_source: Option<RuntimeLoopNoStoreSource>,
    /// Immutable action catalog captured from the durable parent conversation.
    ///
    /// Ephemeral loop iterations clear their pane-local catalog as a new
    /// session boundary. The controller retains this parent snapshot so
    /// restoring the durable conversation never recaptures live configuration.
    pub parent_allowed_actions: mez_agent::AllowedActionSet,
    /// Frozen parent identity restored without allocating an ephemeral name.
    pub parent_display_name: Option<String>,
    /// Prompt-cache lineage to retain while rebinding forked loop iterations.
    pub parent_prompt_cache_lineage_id: Option<String>,
    /// One-based work iteration currently being evaluated or executed.
    pub iteration: usize,
    /// Whether the current work iteration has emitted any semantic
    /// `apply_patch` action before settling.
    pub emitted_apply_patch: bool,
    /// Maximum number of work iterations allowed before the loop stops.
    pub max_iterations: usize,
    /// Routed parent turn that owns final handoff and presentation, when any.
    pub routed_parent_turn_id: Option<String>,
    /// Routed worker profile pinned across internal loop iterations, when any.
    pub routed_worker_profile: Option<ModelProfile>,
    /// Parent macro action settled once when the controller terminates.
    pub completion: Option<RuntimeAgentLoopCompletion>,
}

/// Metadata attached to a loop-owned agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentLoopTurn {
    /// Stable identity of the logical loop that owns this turn.
    pub loop_id: String,
    /// Pane that executes this loop-owned turn.
    pub pane_id: String,
    /// Role this turn plays in the loop controller.
    pub kind: RuntimeAgentLoopTurnKind,
    /// One-based work iteration associated with this turn.
    pub iteration: usize,
}

/// One loop continuation awaiting its predecessor turn's settlement.
///
/// Terminal-result delivery settles a loop-owned work turn before the pane
/// shell releases its running-turn slot. Fork and new iterations bind a fresh
/// ephemeral conversation, and that lower binding refuses to switch while a
/// turn is still running, so the controller records the pending iteration here
/// and the authoritative completion path performs the transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentLoopContinuation {
    /// Stable identity of the logical loop that owns the pending iteration.
    pub loop_id: String,
    /// Pane that will execute the pending loop work turn.
    pub pane_id: String,
    /// One-based iteration the pending work turn will execute.
    pub iteration: usize,
}

/// Result of consuming one terminal loop-owned work turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeAgentLoopSettlement {
    /// The terminal turn was not owned by a logical loop.
    NotOwned,
    /// Patch work scheduled the next iteration in the same logical loop.
    Continued,
    /// The logical loop terminated and released any parent completion join.
    Terminal {
        /// Parent macro completion retained until terminal result delivery.
        completion: Option<RuntimeAgentLoopCompletion>,
    },
}

/// Original publication store and imported source retained by one logical job.
/// An explicit missing store stays missing through retries and replacement.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeCompactionSourceOwner {
    /// Original storage handle, never looked up again from installation state.
    pub(crate) store: Option<crate::storage::transcript::AgentTranscriptStore>,
    /// Original history read that supplied imported canonical occurrences.
    pub(crate) history: Option<std::sync::Arc<crate::runtime::control::RuntimeAgentHistorySource>>,
}

/// Provider-backed conversation compaction queued outside the actor.
///
/// The actor keeps pane state serialized while provider I/O runs in a worker.
/// This task carries the deterministic request and transcript-retention
/// metadata needed to finish compaction once a model response returns.
#[derive(Debug, Clone)]
pub struct RuntimeAgentCompactionTask {
    /// Original source/store capture; once present even missing storage is frozen.
    pub(crate) source_owner: Option<std::sync::Arc<RuntimeCompactionSourceOwner>>,
    /// Exact canonical prefix frozen before rendering the actual provider input.
    pub(crate) replay_source: Option<std::sync::Arc<crate::runtime::RuntimeCompactionReplaySource>>,
    /// Unique queued/claimed task generation used to reject stale worker outcomes.
    pub task_generation: u64,
    /// Transport retries consumed by this logical auxiliary operation.
    pub transport_retry_attempts: u64,
    /// Pending supervised backoff; a held task cannot be dispatched or claimed.
    pub transport_retry_delay_ms: Option<u64>,
    /// Frozen project provenance of this model request, retained across retries.
    pub(crate) accounting_origin: crate::storage::token_usage::AccountingOrigin,
    /// Logical compaction epoch retained across recursive and retry generations.
    pub compaction_epoch: u64,
    /// Pane whose visible status should remain `compacting`.
    pub pane_id: String,
    /// Conversation being summarized.
    pub conversation_id: String,
    /// User-visible source such as `manual` or `auto`.
    pub source: String,
    /// Active physical raw-row count before compaction, including pending rows.
    pub transcript_entries: u64,
    /// Physical transcript rows represented by the selected compactor source.
    pub summarized_entries: usize,
    /// Active model profile name used for the compactor request.
    pub model_profile_name: String,
    /// Active model profile copied for completion metadata.
    pub model_profile: ModelProfile,
    /// Provider request submitted by the async compaction worker.
    pub request: ModelRequest,
    /// Running turn to requeue after this compaction completes.
    pub resume_turn_id: Option<String>,
    /// Resolved wire shape captured at claim for context-limit retry sizing.
    pub compaction_request_shape: Option<(
        mez_agent::ProviderApiCompatibility,
        std::collections::BTreeMap<String, String>,
        bool,
    )>,
}

/// Identifies why one active-turn context compaction was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeActiveTurnCompactionTrigger {
    /// Recovery after a provider authoritatively rejected the submitted request.
    ProviderContextLimit {
        /// Bounded provider retry attempt resumed after compaction.
        attempt: u32,
    },
    /// Proactive compaction after an execution response reports input at or
    /// above the configured threshold.
    ObservedInputLimit {
        /// Provider-reported total input tokens for the execution request.
        observed_input_tokens: u64,
        /// Explicit configured input-token cap.
        max_input_tokens: usize,
    },
}

/// Claimed model compaction dispatch owned by an async provider worker.
#[derive(Debug, Clone)]
pub struct RuntimeAgentCompactionDispatch {
    /// Compaction task metadata and provider request.
    pub task: RuntimeAgentCompactionTask,
    /// Provider used to execute the compaction request.
    pub provider: RuntimeAgentProviderDispatchProvider,
    /// Non-secret provider options used to estimate the exact retry request shape.
    pub provider_options: std::collections::BTreeMap<String, String>,
    /// Wire streaming choice used for the claimed request and its retry.
    pub stream: bool,
}

/// Provider-backed durable memory generation queued outside the actor.
///
/// The actor owns command validation and state mutation while provider I/O runs
/// in a worker. This task carries the deterministic request and memory metadata
/// needed to persist generated records once a model response returns.
#[derive(Debug, Clone)]
pub struct RuntimeAgentRememberTask {
    /// Pane whose visible status should remain `memorizing`.
    pub pane_id: String,
    /// Original conversation, independent of later pane rebinding.
    pub(crate) conversation_id: String,
    /// Frozen project provenance of this model request.
    pub(crate) accounting_origin: crate::storage::token_usage::AccountingOrigin,
    /// Stable observation namespace for this memory request.
    pub(crate) observation_id: String,
    /// Active model profile name used for the memory request.
    pub model_profile_name: String,
    /// Active model profile copied for completion metadata.
    pub model_profile: ModelProfile,
    /// Durable scope selected when the command was queued.
    pub scope: MemoryScope,
    /// Provider request submitted by the async memory worker.
    pub request: ModelRequest,
}

/// Claimed model memory dispatch owned by an async provider worker.
#[derive(Debug, Clone)]
pub struct RuntimeAgentRememberDispatch {
    /// Remember task metadata and provider request.
    pub task: RuntimeAgentRememberTask,
    /// Provider used to execute the memory-generation request.
    pub provider: RuntimeAgentProviderDispatchProvider,
}

/// Provider-backed generated session-title task queued outside the actor.
///
/// Title generation is a side channel: the actor owns admission, the attempt
/// limit, settlement, and cancellation while the bounded provider request runs
/// in an async worker. The task never references a turn, action, or transcript
/// entry, so generating a title cannot create or mutate turn machinery.
#[derive(Debug, Clone)]
pub struct RuntimeAgentSessionTitleTask {
    /// Exact pending dispatch, preserved on no-I/O rollback and renewed for retry.
    /// This is a source fence, never an accounting owner or execution authority.
    pub dispatch_id: String,
    /// Unique issued attempt; assigned only when a worker claims this request.
    pub attempt_id: String,
    /// Immutable origin captured with the frozen title request.
    pub(crate) accounting_origin: crate::storage::token_usage::AccountingOrigin,
    /// Original pane-root identity, preventing late expense from reaching a replacement.
    pub(crate) pane_process: Option<crate::runtime::processes::RuntimePaneProcessIdentity>,
    /// Conversation whose display title is being generated.
    pub conversation_id: String,
    /// Pane that owns the conversation, retained for status and trace output.
    pub pane_id: String,
    /// Agent identity copied into the bounded title request.
    pub agent_id: String,
    /// Model profile name selected for the title request.
    pub model_profile_name: String,
    /// Model profile used to build and report the bounded title request.
    pub model_profile: ModelProfile,
    /// Bounded, tool-free title request built by the mez-agent title builder.
    pub request: ModelRequest,
}

/// Claimed generated session-title dispatch owned by an async provider worker.
#[derive(Debug, Clone)]
pub struct RuntimeAgentSessionTitleDispatch {
    /// Title task metadata and bounded provider request.
    pub task: RuntimeAgentSessionTitleTask,
    /// Provider used to execute the title request.
    pub provider: RuntimeAgentProviderDispatchProvider,
}

/// One claimed generated session-title task together with its worker lease.
///
/// A claimed title task leaves the pending queue, so the lease is what makes a
/// lost worker observable: the provider poll timer reaps an expired claim, and an
/// expired claim would otherwise hold a concurrency slot for the life of the
/// process.
#[derive(Debug, Clone)]
pub struct RuntimeAgentSessionTitleClaim {
    /// Title task metadata and bounded provider request.
    pub task: RuntimeAgentSessionTitleTask,
    /// Unix milliseconds at which this claim was installed.
    pub claimed_at_unix_ms: u64,
    /// Worker lease applied to this claim.
    pub timeout_ms: u64,
}
