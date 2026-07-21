//! Subagent, macro, shell transaction, hook, profile, and overlay state records.

use super::{ActionStatus, HookExecutionPlan, PaneId, RuntimeHookPipelineBlock, Size, WindowId};
use crate::host::terminal::PaneAgentStatusField;
use std::collections::BTreeMap;

/// Describes whether a parent turn waits for spawned subagents before it can
/// continue provider execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SubagentWaitPolicy {
    /// Spawned subagents are joined: the parent waits for their task results.
    #[default]
    Join,
    /// Spawned subagents are detached: the parent can continue after spawn.
    Detach,
}

/// Tracks one spawned child turn that a parent turn is waiting to join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JoinedSubagentDependency {
    /// Parent turn that emitted the MAAP `spawn_agent` action.
    pub parent_turn_id: String,
    /// Parent action that should receive the child task result.
    pub parent_action_id: String,
    /// Child turn created for the spawned subagent.
    pub child_turn_id: String,
    /// Child agent created for the spawned subagent.
    pub child_agent_id: String,
    /// Human-readable display name assigned to the child subagent.
    pub child_display_name: Option<String>,
}

/// Tracks runtime delegation lineage for an active spawned subagent.
///
/// Regular pane agents are roots at depth zero and therefore do not need stored
/// entries. Only active spawned children are tracked so width and depth limits
/// reflect currently running delegation state rather than historical turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeSubagentLineage {
    /// Direct parent agent that spawned this child.
    pub parent_agent_id: String,
    /// Root pane agent that owns this delegation tree.
    pub root_agent_id: String,
    /// Depth of this subagent below the root pane agent.
    pub depth: usize,
    /// Human-readable display name assigned while the subagent is active.
    pub display_name: String,
}

/// Product-specialized mux overlay carrying issue or memory refresh sources.
pub(crate) type RuntimeDisplayOverlay =
    mez_mux::overlay::DisplayOverlay<RuntimeRecordBrowserOverlaySource>;

/// Product-specialized record-browser overlay state.
pub(crate) type RuntimeRecordBrowserOverlayState =
    mez_mux::overlay::RecordBrowserOverlayState<RuntimeRecordBrowserOverlaySource>;

/// Product-specialized preserved record-browser frame.
pub(crate) type RuntimeRecordBrowserOverlayFrame =
    mez_mux::overlay::RecordBrowserOverlayFrame<RuntimeRecordBrowserOverlaySource>;

/// Query context retained for one backend-specific record-browser overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeRecordBrowserOverlaySource {
    /// Durable transcript backing the current pane's context browser.
    Context {
        /// Conversation whose entries are displayed and may be deleted.
        conversation_id: String,
        /// Pane that owns the active conversation.
        pane_id: String,
    },
    /// Issue browser filters and bounded result limit.
    Issues {
        /// Optional project glob filter; `None` means all projects.
        project_glob: Option<String>,
        /// Project glob restored when the all-projects browser view is toggled off.
        default_project_glob: Option<String>,
        /// Optional defect/task kind filter.
        kind: Option<mez_agent::issues::IssueKind>,
        /// Optional lifecycle state filter.
        state: Option<mez_agent::issues::IssueState>,
        /// Optional title/body text filter.
        text: Option<String>,
        /// Maximum number of displayed records.
        limit: usize,
    },
    /// Memory browser filters and bounded result limit.
    Memories {
        /// Optional exact memory scope; `None` means all scopes.
        scope: Option<mez_agent::memory::MemoryScope>,
        /// Scope restored when the all-scopes browser view is toggled off.
        default_scope: Option<mez_agent::memory::MemoryScope>,
        /// Optional memory kind filter.
        kind: Option<mez_agent::memory::MemoryKind>,
        /// Optional memory state filter.
        state: Option<mez_agent::memory::MemoryState>,
        /// Optional full-text query.
        text: Option<String>,
        /// Maximum number of displayed records.
        limit: usize,
    },
}

/// Pane-local mux selector specialized with product agent-status identity.
pub(crate) type RuntimePaneAgentStatusSelector =
    mez_mux::overlay::AnchoredSelector<PaneAgentStatusField>;

/// Carries Pane Descriptor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneDescriptor {
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) window_id: WindowId,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) pane_id: PaneId,
    /// Stores the size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) size: Size,
}

/// Carries Blocked Agent Approval Ref state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockedAgentApprovalRef {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) turn_id: String,
    /// Stores the action id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) action_id: String,
}

/// Carries Running Shell Transaction Ref state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunningShellTransactionRef {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) turn_id: String,
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) kind: RunningShellTransactionKind,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) pane_id: String,
    /// Stores the command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) command: String,
    /// Stores the started at unix ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) started_at_unix_ms: u64,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) timeout_ms: Option<u64>,
    /// Pane input payload that must be sent after the transaction start marker.
    ///
    /// Large generated command bodies are streamed after the wrapper receiver
    /// starts so they are consumed as data rather than parsed as shell source.
    pub(crate) pending_input_payload: Option<Vec<u8>>,
    /// Stores the observed output bytes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) observed_output_bytes: usize,
    /// Stores the observed output preview value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) observed_output_preview: String,
    /// Stores the observed output truncated value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) observed_output_truncated: bool,
}

/// Cache identity for pane-shell path authority resolution.
///
/// Environment and configuration generations are part of the identity so a
/// working-directory, remote-environment, or permission change cannot reuse
/// stale canonical path evidence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RuntimePathResolutionCacheKey {
    /// Pane whose shell environment performs the resolution.
    pub(crate) pane_id: String,
    /// Stable hash of the shell-observed pane environment.
    pub(crate) environment_signature: String,
    /// Configuration generation that supplied the requested authority.
    pub(crate) config_generation: u64,
    /// Exact bounded set of paths resolved by the pane shell.
    pub(crate) request: mez_agent::shell::PanePathResolutionRequest,
}

/// Tracks a shell-backed `apply_patch` action across batched read phases.
///
/// Large patch read snapshots can exceed a pane PTY capture budget when every
/// touched path is read in one transaction. The runtime keeps this state while
/// dispatching one read transaction per path and then builds the verified write
/// phase from the accumulated snapshot outputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeApplyPatchBatchState {
    /// Paths that still need read-phase snapshots.
    pub(crate) remaining_paths: Vec<String>,
    /// Full transport bytes captured for the currently running read-phase batch.
    ///
    /// Pane previews stay size-bounded for display, but write-phase planning
    /// still needs the complete snapshot payload bytes so large read batches can
    /// be verified after preview text truncates or normalizes lossy UTF-8.
    pub(crate) current_read_transport: Vec<u8>,
    /// Decoded read-phase outputs that completed without transport truncation.
    pub(crate) read_outputs: Vec<String>,
}

/// Carries Running Shell Transaction Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunningShellTransactionKind {
    /// Represents the Agent Action case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    AgentAction {
        /// Stores the action id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        action_id: String,
    },
    /// Represents the Readiness Probe case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ReadinessProbe,
    /// Represents the Bootstrap case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Bootstrap,
    /// Internal read-only canonical path-resolution transaction.
    PathResolution {
        /// Cache identity captured before the transaction was dispatched.
        cache_key: RuntimePathResolutionCacheKey,
        /// Pending action resumed or failed when action-specific resolution settles.
        action_id: Option<String>,
    },
    /// Internal Bubblewrap runtime-profile capability probe.
    BubblewrapCapabilityProbe {
        /// Pending action resumed or failed when the probe settles.
        action_id: String,
        /// Exact capability identity captured before pane dispatch.
        cache_key: crate::security::sandbox::BubblewrapCapabilityCacheKey,
        /// Exact deterministic probe plan whose output must be validated.
        probe_plan: crate::security::sandbox::BubblewrapCapabilityProbePlan,
    },
}

/// Timer-visible kind for a live shell transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeShellTransactionTimerKind {
    /// Agent shell command action timeout.
    AgentAction,
    /// Readiness probe timeout.
    ReadinessProbe,
    /// Pane bootstrap timeout.
    Bootstrap,
    /// Pane-shell canonical path-resolution timeout.
    PathResolution,
    /// Bubblewrap runtime-profile capability probe timeout.
    BubblewrapCapabilityProbe,
    /// Focused-shell hook marker timeout.
    FocusedShellHook,
}

/// Timer-visible snapshot of a live shell transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeShellTransactionTimerRef {
    /// Unique transaction marker used as the timer owner identity.
    pub marker: String,
    /// Timeout family to schedule.
    pub kind: RuntimeShellTransactionTimerKind,
    /// Unix timestamp in milliseconds when the transaction started.
    pub started_at_unix_ms: u64,
    /// Timeout duration in milliseconds.
    pub timeout_ms: u64,
}

/// Runtime-owned failure payload used to settle a shell action whose external
/// shell transaction could not complete normally.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuntimeShellTransactionActionFailure {
    /// Runtime action id for the MAAP shell command being failed.
    pub(crate) action_id: String,
    /// Terminal action status to report to the MAAP action result.
    pub(crate) status: ActionStatus,
    /// Stable machine-readable failure code for the action error object.
    pub(crate) code: String,
    /// User-facing failure message rendered into the pane and transcript.
    pub(crate) message: String,
    /// Whether the shell command itself was sent to the pane before failure.
    pub(crate) sent_to_pane: bool,
    /// Structured timeout or observation data attached to the action result.
    pub(crate) terminal_observation: serde_json::Value,
    /// Trace-level reason used for state-transition diagnostics.
    pub(crate) trace_reason: String,
}

/// Carries Pending Focused Shell Hook Transaction state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingFocusedShellHookTransaction {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) pane_id: String,
    /// Stores the plan value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) plan: HookExecutionPlan,
    /// Stores the started at unix ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) started_at_unix_ms: u64,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) timeout_ms: u64,
    /// Stores the continuation value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) continuation: Option<PendingFocusedShellHookContinuation>,
}

/// Agent shell action suspended behind a blocking focused-shell pre-action hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingFocusedShellHookContinuation {
    /// Turn that owns the shell action waiting on the hook result.
    pub(crate) turn_id: String,
    /// Action to resume or deny after the hook result is known.
    pub(crate) action_id: String,
}

/// Completed pre-shell hook identity for a running action.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RuntimeAgentPreShellHookCompletion {
    /// Turn whose pending action ran the hook.
    pub(crate) turn_id: String,
    /// Shell action guarded by the hook.
    pub(crate) action_id: String,
    /// Hook that has already completed for this action.
    pub(crate) hook_id: String,
}

/// Outcome of evaluating pre-action hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeHookPipelineDecision {
    /// No blocking hook prevented the caller from continuing immediately.
    Continue,
    /// A hook failure policy blocked the action.
    Block(RuntimeHookPipelineBlock),
    /// A focused-shell hook was queued and the caller must resume later.
    Pending,
}

/// Carries Runtime Model Profile Override Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimeModelProfileOverrideStore {
    /// Stores the session profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) session_profile: Option<String>,
    /// Stores the window profiles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) window_profiles: BTreeMap<String, String>,
    /// Stores the pane profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) pane_profiles: BTreeMap<String, String>,
    /// Stores the agent profiles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) agent_profiles: BTreeMap<String, String>,
    /// Stores the subagent profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) subagent_profiles: BTreeMap<String, String>,
}

/// User-defined pane personality profile.
///
/// Personality profiles are optional named overlays for pane-local agent
/// preferences. They never replace Mezzanine's built-in system prompt; instead
/// they append user-configured instructions and selected agent preferences.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimeAgentPersonalityProfile {
    /// Stable profile id from configuration.
    pub(crate) id: String,
    /// Optional human-readable profile name.
    pub(crate) name: Option<String>,
    /// Optional system-level instruction text appended after Mezzanine's base
    /// system prompt.
    pub(crate) system_prompt: Option<String>,
    /// Optional response style preference.
    pub(crate) response_style: Option<String>,
    /// Optional model profile override.
    pub(crate) model_profile: Option<String>,
    /// Optional planning-mode override.
    pub(crate) planning_enabled: Option<bool>,
    /// Optional routing override.
    pub(crate) routing_enabled: Option<bool>,
}

/// Carries Runtime Model Profile Override Scope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeModelProfileOverrideScope {
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session,
    /// Represents the Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Window(String),
    /// Represents the Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pane(String),
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent(String),
    /// Represents the Subagent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Subagent(String),
}
