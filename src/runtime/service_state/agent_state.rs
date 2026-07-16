//! Subagent, macro, shell transaction, hook, profile, and overlay state records.

use super::*;

/// Describes whether a parent turn waits for spawned subagents before it can
/// continue provider execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentWaitPolicy {
    /// Spawned subagents are joined: the parent waits for their task results.
    Join,
    /// Spawned subagents are detached: the parent can continue after spawn.
    Detach,
}

/// Tracks one macro-managed persistent child and the parent macro run that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct MacroManagedSubagent {
    /// Parent macro orchestration turn allowed to send step prompts to this child.
    pub parent_turn_id: String,
    /// Parent agent that owns the macro run.
    pub parent_agent_id: String,
    /// Macro name used for diagnostics and traceability.
    pub macro_name: String,
}

/// Describes the current runtime-owned phase for one active macro run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) enum MacroRunPhase {
    /// The runtime is preparing or retrying one step submission.
    DispatchingStep {
        /// Zero-based index of the step being dispatched.
        step_index: usize,
    },
    /// The runtime is waiting for the submitted child turn to finish.
    WaitingForStep {
        /// Zero-based index of the submitted step.
        step_index: usize,
        /// Child turn currently executing the step prompt.
        child_turn_id: String,
    },
    /// The runtime is asking the parent model to judge a completed step.
    WaitingForJudge {
        /// Zero-based index of the step being judged.
        step_index: usize,
    },
}

/// Stores the terminal task result for one completed macro child step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct MacroStepTaskResult {
    /// Whether the child step completed successfully.
    pub success: bool,
    /// Child task summary supplied through the subagent task result.
    pub summary: String,
    /// Child task output supplied through the subagent task result.
    pub output: String,
}

/// Runtime-validated outcome returned by the macro judge model request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime) enum MacroJudgeOutcome {
    /// Continue with the next scripted prompt unchanged.
    Continue,
    /// Continue with a validated adapted prompt for the next step.
    ContinueWithAdaptedPrompt,
    /// Retry the current step, optionally with a bounded adapted prompt.
    RetryCurrentStep,
    /// Stop the macro as failed with a user-visible explanation.
    StopFailure,
    /// Complete the macro successfully after the final required step.
    FinishSuccess,
}

impl std::str::FromStr for MacroJudgeOutcome {
    type Err = &'static str;

    /// Parses the stable wire value returned by the structured macro-judge
    /// provider response.
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "continue" => Ok(Self::Continue),
            "continue_with_adapted_prompt" => Ok(Self::ContinueWithAdaptedPrompt),
            "retry_current_step" => Ok(Self::RetryCurrentStep),
            "stop_failure" => Ok(Self::StopFailure),
            "finish_success" => Ok(Self::FinishSuccess),
            _ => Err("unsupported macro judge outcome"),
        }
    }
}

/// Stores one validated macro judge decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct MacroJudgeDecision {
    /// Outcome selected by the judge.
    pub outcome: MacroJudgeOutcome,
    /// Whether the judge accepted the completed step as successful.
    pub step_success: bool,
    /// Short model-supplied rationale retained for diagnostics.
    pub rationale: String,
    /// Optional adapted prompt used only for `ContinueWithAdaptedPrompt`.
    pub adapted_prompt: Option<String>,
    /// Optional user-visible failure message for `StopFailure`.
    pub user_message: Option<String>,
}

/// Stores one scripted macro step and its runtime submission metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct MacroRunStep {
    /// Zero-based index copied from the loaded macro definition.
    pub index: usize,
    /// Number of times this step has been submitted to the persistent worker.
    pub attempts: usize,
    /// Scripted prompt copied at run start so in-flight runs are stable.
    pub scripted_prompt: String,
    /// Prompt text submitted to the child, including invocation context.
    pub submitted_prompt: Option<String>,
    /// Child turn created for the submitted step, when one exists.
    pub child_turn_id: Option<String>,
    /// Terminal task result returned by the child step.
    pub task_result: Option<MacroStepTaskResult>,
    /// Runtime-validated judge decision for the completed step.
    pub judgment: Option<MacroJudgeDecision>,
}

/// Tracks explicit runtime state for a harness-owned macro run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct MacroRunState {
    /// Stable macro run identifier; currently equal to the parent turn id.
    pub run_id: String,
    /// Parent macro orchestration turn id.
    pub parent_turn_id: String,
    /// Parent agent that owns the macro run.
    pub parent_agent_id: String,
    /// Parent pane where the macro was invoked.
    pub parent_pane_id: String,
    /// Persistent child agent used for all macro steps.
    pub child_agent_id: String,
    /// Macro name copied from the loaded definition.
    pub macro_name: String,
    /// Macro description copied from the loaded definition.
    pub macro_description: String,
    /// Original user prompt that invoked the macro.
    pub invocation_prompt: String,
    /// User-supplied context after the macro token, if any.
    pub invocation_context: Option<String>,
    /// Ordered steps copied from the loaded definition at run start.
    pub steps: Vec<MacroRunStep>,
    /// Zero-based index of the current step.
    pub current_step: usize,
    /// Current runtime-owned macro run phase.
    pub phase: MacroRunPhase,
}

/// Tracks one spawned child turn that a parent turn is waiting to join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct JoinedSubagentDependency {
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
pub(in crate::runtime) struct RuntimeSubagentLineage {
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
pub(in crate::runtime) type RuntimeDisplayOverlay =
    mez_mux::overlay::DisplayOverlay<RuntimeRecordBrowserOverlaySource>;

/// Product-specialized record-browser overlay state.
pub(in crate::runtime) type RuntimeRecordBrowserOverlayState =
    mez_mux::overlay::RecordBrowserOverlayState<RuntimeRecordBrowserOverlaySource>;

/// Product-specialized preserved record-browser frame.
pub(in crate::runtime) type RuntimeRecordBrowserOverlayFrame =
    mez_mux::overlay::RecordBrowserOverlayFrame<RuntimeRecordBrowserOverlaySource>;

/// Query context retained for one backend-specific record-browser overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) enum RuntimeRecordBrowserOverlaySource {
    /// Issue browser filters and bounded result limit.
    Issues {
        /// Optional project glob filter; `None` means all projects.
        project_glob: Option<String>,
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

/// Pane-local drop-down selector for agent model and reasoning status pills.
///
/// The selector is actor-owned UI state: mouse routing receives cell hits from
/// the terminal client loop, while rendering uses this record to draw the
/// current list and highlight the row under the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimePaneAgentStatusSelector {
    /// Stable pane identity targeted by the selector.
    pub(in crate::runtime) pane_id: String,
    /// Pane index targeted by rendered mouse cells.
    pub(in crate::runtime) pane_index: usize,
    /// Status field being selected.
    pub(in crate::runtime) field: PaneAgentStatusField,
    /// Available values in display and selection order.
    pub(in crate::runtime) items: Vec<String>,
    /// Item currently highlighted by hover or initial active value.
    pub(in crate::runtime) active_index: usize,
    /// First item currently visible in the drop-down viewport.
    pub(in crate::runtime) scroll_offset: usize,
    /// Column of the source pill used to place the drop-down.
    pub(in crate::runtime) anchor_column: u16,
    /// Row of the source pill used to place the drop-down.
    pub(in crate::runtime) anchor_row: u16,
    /// Width of the source pill used as a minimum drop-down width.
    pub(in crate::runtime) anchor_width: u16,
}

/// Carries Pane Descriptor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct PaneDescriptor {
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) window_id: WindowId,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_id: PaneId,
    /// Stores the size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) size: Size,
}

/// Carries Blocked Agent Approval Ref state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct BlockedAgentApprovalRef {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) turn_id: String,
    /// Stores the action id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) action_id: String,
}

/// Carries Running Shell Transaction Ref state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RunningShellTransactionRef {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) turn_id: String,
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) kind: RunningShellTransactionKind,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_id: String,
    /// Stores the command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) command: String,
    /// Stores the started at unix ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) started_at_unix_ms: u64,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) timeout_ms: Option<u64>,
    /// Pane input payload that must be sent after the transaction start marker.
    ///
    /// Large generated command bodies are streamed after the wrapper receiver
    /// starts so they are consumed as data rather than parsed as shell source.
    pub(in crate::runtime) pending_input_payload: Option<Vec<u8>>,
    /// Stores the observed output bytes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) observed_output_bytes: usize,
    /// Stores the observed output preview value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) observed_output_preview: String,
    /// Stores the observed output truncated value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) observed_output_truncated: bool,
}

/// Tracks a shell-backed `apply_patch` action across batched read phases.
///
/// Large patch read snapshots can exceed a pane PTY capture budget when every
/// touched path is read in one transaction. The runtime keeps this state while
/// dispatching one read transaction per path and then builds the verified write
/// phase from the accumulated snapshot outputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeApplyPatchBatchState {
    /// Paths that still need read-phase snapshots.
    pub(in crate::runtime) remaining_paths: Vec<String>,
    /// Full transport bytes captured for the currently running read-phase batch.
    ///
    /// Pane previews stay size-bounded for display, but write-phase planning
    /// still needs the complete snapshot payload bytes so large read batches can
    /// be verified after preview text truncates or normalizes lossy UTF-8.
    pub(in crate::runtime) current_read_transport: Vec<u8>,
    /// Decoded read-phase outputs that completed without transport truncation.
    pub(in crate::runtime) read_outputs: Vec<String>,
}

/// Carries Running Shell Transaction Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) enum RunningShellTransactionKind {
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
pub(in crate::runtime) struct RuntimeShellTransactionActionFailure {
    /// Runtime action id for the MAAP shell command being failed.
    pub(in crate::runtime) action_id: String,
    /// Terminal action status to report to the MAAP action result.
    pub(in crate::runtime) status: ActionStatus,
    /// Stable machine-readable failure code for the action error object.
    pub(in crate::runtime) code: String,
    /// User-facing failure message rendered into the pane and transcript.
    pub(in crate::runtime) message: String,
    /// Whether the shell command itself was sent to the pane before failure.
    pub(in crate::runtime) sent_to_pane: bool,
    /// Structured timeout or observation data attached to the action result.
    pub(in crate::runtime) terminal_observation: serde_json::Value,
    /// Trace-level reason used for state-transition diagnostics.
    pub(in crate::runtime) trace_reason: String,
}

/// Carries Pending Focused Shell Hook Transaction state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct PendingFocusedShellHookTransaction {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_id: String,
    /// Stores the plan value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) plan: HookExecutionPlan,
    /// Stores the started at unix ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) started_at_unix_ms: u64,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) timeout_ms: u64,
    /// Stores the continuation value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) continuation: Option<PendingFocusedShellHookContinuation>,
}

/// Agent shell action suspended behind a blocking focused-shell pre-action hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct PendingFocusedShellHookContinuation {
    /// Turn that owns the shell action waiting on the hook result.
    pub(in crate::runtime) turn_id: String,
    /// Action to resume or deny after the hook result is known.
    pub(in crate::runtime) action_id: String,
}

/// Completed pre-shell hook identity for a running action.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::runtime) struct RuntimeAgentPreShellHookCompletion {
    /// Turn whose pending action ran the hook.
    pub(in crate::runtime) turn_id: String,
    /// Shell action guarded by the hook.
    pub(in crate::runtime) action_id: String,
    /// Hook that has already completed for this action.
    pub(in crate::runtime) hook_id: String,
}

/// Outcome of evaluating pre-action hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) enum RuntimeHookPipelineDecision {
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
pub(in crate::runtime) struct RuntimeModelProfileOverrideStore {
    /// Stores the session profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) session_profile: Option<String>,
    /// Stores the window profiles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) window_profiles: BTreeMap<String, String>,
    /// Stores the pane profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_profiles: BTreeMap<String, String>,
    /// Stores the agent profiles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_profiles: BTreeMap<String, String>,
    /// Stores the subagent profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) subagent_profiles: BTreeMap<String, String>,
}

/// User-defined pane personality profile.
///
/// Personality profiles are optional named overlays for pane-local agent
/// preferences. They never replace Mezzanine's built-in system prompt; instead
/// they append user-configured instructions and selected agent preferences.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeAgentPersonalityProfile {
    /// Stable profile id from configuration.
    pub(in crate::runtime) id: String,
    /// Optional human-readable profile name.
    pub(in crate::runtime) name: Option<String>,
    /// Optional system-level instruction text appended after Mezzanine's base
    /// system prompt.
    pub(in crate::runtime) system_prompt: Option<String>,
    /// Optional response style preference.
    pub(in crate::runtime) response_style: Option<String>,
    /// Optional model profile override.
    pub(in crate::runtime) model_profile: Option<String>,
    /// Optional planning-mode override.
    pub(in crate::runtime) planning_enabled: Option<bool>,
    /// Optional routing override.
    pub(in crate::runtime) routing_enabled: Option<bool>,
}

/// Carries Runtime Model Profile Override Scope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) enum RuntimeModelProfileOverrideScope {
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
