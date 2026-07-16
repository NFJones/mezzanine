//! Runtime agent provider dispatch and loop state records.
//!
//! This module owns data-only records used by provider-backed agent workers,
//! automatic sizing decisions, loop bookkeeping, compaction, and memory
//! generation. The runtime actor still owns behavior; these records simply
//! describe queued or claimed work across async boundaries.

use super::{
    AgentContext, AgentTurnRecord, DeepSeekChatCompletionsProvider, MemoryScope, ModelProfile,
    ModelRequest, OpenAiCompatibleChatCompletionsProvider, OpenAiResponsesProvider, PathScopes,
    PermissionPolicy, ReqwestProviderHttpTransport, RuntimeAutoSizingDispatch,
    SessionApprovalStore, SubagentScopeDeclaration,
};
use crate::integrations::agent::provider::{AnthropicMessagesProvider, ClaudeCodeProvider};
use mez_agent::McpPromptTool;

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

/// Tracks a provider task after the async actor has claimed it from the queue.
///
/// Provider workers run outside the serialized runtime actor. This record gives
/// the actor a finite lease it can enforce if the worker never submits a
/// completion or failure event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeAgentProviderClaim {
    /// Runtime turn owned by the claimed provider worker.
    pub turn_id: String,
    /// Agent identity that owns the turn.
    pub agent_id: String,
    /// Timer generation associated with the current claim lease.
    pub generation: u64,
    /// Unix timestamp, in milliseconds, when the provider task was claimed.
    pub claimed_at_unix_ms: u64,
    /// Maximum lease duration before the runtime fails the turn.
    pub timeout_ms: u64,
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
    /// Represents the Claude Code subprocess provider case.
    ///
    /// Callers use this variant for configured provider instances that rely on
    /// a local Claude Code session instead of direct API credentials.
    ClaudeCode(ClaudeCodeProvider),
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
            Self::ClaudeCode(provider) => provider.provider_id(),
            Self::OpenAiCompatible(provider) => provider.provider_id(),
        }
    }
}

/// Carries Runtime Agent Provider Dispatch state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct RuntimeAgentProviderDispatch {
    /// Stores the turn value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn: AgentTurnRecord,
    /// Stores the context value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub context: AgentContext,
    /// Stores the model profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model_profile: ModelProfile,
    /// Structured macro judge request awaiting model feedback instead of an
    /// ordinary MAAP action batch.
    pub macro_judge_request: Option<ModelRequest>,
    /// Optional automatic sizing context for the worker's first provider step.
    pub auto_sizing: Option<RuntimeAutoSizingDispatch>,
    /// Optional router provider for auto-sizing when different from the main
    /// turn provider. When set, auto-sizing requests use this provider.
    pub auto_sizing_provider: Option<RuntimeAgentProviderDispatchProvider>,
    /// Providers that may be selected by automatic sizing target profiles.
    ///
    /// The async provider worker runs outside the runtime actor after the
    /// router decision is known. Carrying target providers with the dispatch
    /// lets cross-provider routing use the selected profile instead of falling
    /// back to the originally active provider.
    pub auto_sizing_target_providers:
        std::collections::BTreeMap<String, RuntimeAgentProviderDispatchProvider>,
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

/// Runtime-owned state for one active `/loop` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentLoopState {
    /// Pane whose visible agent shell owns the loop.
    pub pane_id: String,
    /// Original user prompt supplied after `/loop`.
    pub original_prompt: String,
    /// Conversation preparation mode for loop-owned work turns.
    pub mode: RuntimeAgentLoopMode,
    /// Parent conversation id restored after fresh loop iterations and used as
    /// the fork source when applicable.
    pub parent_conversation_id: String,
    /// Durable parent transcript count to restore after ephemeral loop forks.
    pub parent_transcript_entries: u64,
    /// Prompt-cache lineage to retain while rebinding forked loop iterations.
    pub parent_prompt_cache_lineage_id: Option<String>,
    /// One-based work iteration currently being evaluated or executed.
    pub iteration: usize,
    /// Whether the current work iteration has emitted any semantic
    /// `apply_patch` action before settling.
    pub emitted_apply_patch: bool,
    /// Maximum number of work iterations allowed before the loop stops.
    pub max_iterations: usize,
    /// Parent macro action settled once when the controller terminates.
    pub completion: Option<RuntimeAgentLoopCompletion>,
}

/// Metadata attached to a loop-owned agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentLoopTurn {
    /// Pane whose active loop owns the turn.
    pub pane_id: String,
    /// Role this turn plays in the loop controller.
    pub kind: RuntimeAgentLoopTurnKind,
    /// One-based work iteration associated with this turn.
    pub iteration: usize,
}

/// Provider-backed conversation compaction queued outside the actor.
///
/// The actor keeps pane state serialized while provider I/O runs in a worker.
/// This task carries the deterministic request and transcript-retention
/// metadata needed to finish compaction once a model response returns.
#[derive(Debug, Clone)]
pub struct RuntimeAgentCompactionTask {
    /// Pane whose visible status should remain `compacting`.
    pub pane_id: String,
    /// Conversation being summarized.
    pub conversation_id: String,
    /// User-visible source such as `manual` or `auto`.
    pub source: String,
    /// Transcript entry count before compaction.
    pub transcript_entries: u64,
    /// Raw recent transcript entries to retain after summary insertion.
    pub retained_transcript_entries: u64,
    /// Durable entries supplied to the model compactor.
    pub summarized_entries: usize,
    /// Active model profile name used for the compactor request.
    pub model_profile_name: String,
    /// Active model profile copied for completion metadata.
    pub model_profile: ModelProfile,
    /// Provider request submitted by the async compaction worker.
    pub request: ModelRequest,
    /// Running turn to requeue after this compaction completes.
    pub resume_turn_id: Option<String>,
}

/// Claimed model compaction dispatch owned by an async provider worker.
#[derive(Debug, Clone)]
pub struct RuntimeAgentCompactionDispatch {
    /// Compaction task metadata and provider request.
    pub task: RuntimeAgentCompactionTask,
    /// Provider used to execute the compaction request.
    pub provider: RuntimeAgentProviderDispatchProvider,
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
