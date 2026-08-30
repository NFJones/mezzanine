//! Subagent, macro, shell transaction, hook, profile, and overlay state records.

use super::{ActionStatus, HookExecutionPlan, PaneId, RuntimeHookPipelineBlock, Size, WindowId};
use crate::host::terminal::PaneAgentStatusField;
use mez_agent::LocalActionPlan;
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

/// Product-specialized mux overlay carrying record-browser and live sources.
pub(crate) type RuntimeDisplayOverlay =
    mez_mux::overlay::DisplayOverlay<RuntimeRecordBrowserOverlaySource, RuntimeLiveOverlaySource>;

/// Product-owned source and schedule retained for one live pager overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeLiveOverlaySource {
    /// Typed status builder used to refresh the overlay without command replay.
    pub source: RuntimeLiveOverlaySourceKind,
    /// Refresh cadence for this source.
    pub refresh_interval_ms: u64,
    /// Earliest actor clock time when this source should next be rebuilt.
    pub next_due_ms: u64,
}

/// Typed product status builders supported by live pager overlays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeLiveOverlaySourceKind {
    /// Exact-client Iroh connection status.
    IrohStatus {
        /// Client whose authenticated connection details may be displayed.
        client_id: String,
    },
    /// Pane-bound agent status.
    AgentStatus {
        /// Pane that opened the status pager.
        pane_id: String,
        /// Whether durable extended status was requested.
        extended: bool,
    },
}

/// Product-specialized record-browser overlay state.
pub(crate) type RuntimeRecordBrowserOverlayState =
    mez_mux::overlay::RecordBrowserOverlayState<RuntimeRecordBrowserOverlaySource>;

/// Product-specialized preserved record-browser frame.
pub(crate) type RuntimeRecordBrowserOverlayFrame =
    mez_mux::overlay::RecordBrowserOverlayFrame<RuntimeRecordBrowserOverlaySource>;

/// Query context retained for one backend-specific record-browser overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeRecordBrowserOverlaySource {
    /// Live session-wide pending approval queue.
    Approvals,
    /// Durable saved agent conversations displayed by bare `/resume`.
    SavedSessions {
        /// Current directory filter; `None` displays all saved conversations.
        directory: Option<String>,
        /// Active pane directory restored when the all-sessions view is toggled off.
        default_directory: Option<String>,
        /// Active or archived lifecycle partition displayed by the browser.
        lifecycle: crate::storage::transcript::SavedSessionLifecycleFilter,
        /// Whether delegated child conversations are included in discovery.
        include_subagents: bool,
        /// Optional backend search over UUID and bounded session metadata.
        search: Option<String>,
        /// Keyset anchor selecting the current bounded catalog page.
        anchor: Option<crate::storage::transcript::SavedSessionPageAnchor>,
        /// Maximum catalog rows retained by the current browser page.
        limit: usize,
    },
    /// Configured personality profiles selectable for one pane.
    Personalities {
        /// Pane whose effective personality is displayed and changed.
        pane_id: String,
    },
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
        /// Whether an implicit active-work filter excludes resolved records.
        active_only: bool,
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
    /// Whether approval grants one exact unsandboxed retry for this action.
    pub(crate) sandbox_bypass_after_approval: bool,
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
    /// Typed pane input payload sent after the transaction start marker.
    ///
    /// Large generated command bodies are streamed after the wrapper receiver
    /// starts. The retained delivery preserves priority, pacing, negotiated
    /// acknowledgement support, and transaction identity across PTY owners.
    pub(crate) pending_input_payload: Option<mez_mux::process::ShellInputDelivery>,
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

/// Retains one ambiguous sandbox payload failure while a bounded internal
/// model assessment determines whether an approval prompt is appropriate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeSandboxFailureAssessment {
    /// Exact backend that established the failed payload.
    pub(crate) backend: crate::runtime::SandboxBackend,
    /// Exact action whose sandboxed payload exited non-zero.
    pub(crate) action_id: String,
    /// Settled transaction marker retained for ordinary fallback settlement.
    pub(crate) marker: String,
    /// Original transaction evidence, including bounded command output.
    pub(crate) transaction: RunningShellTransactionRef,
    /// Trusted backend-reported payload exit code.
    pub(crate) exit_code: i32,
    /// Dedicated structured provider request built from bounded evidence.
    pub(crate) request: crate::runtime::ModelRequest,
}

/// Redacted lifecycle facts retained for one approved sandbox fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeSandboxFallbackAudit {
    /// Exact sandbox backend from which the retry originated.
    pub(crate) backend: crate::runtime::SandboxBackend,
    /// Stable classification or pre-payload failure reason.
    pub(crate) reason: String,
    /// Trusted proof or bounded model rationale, hashed before audit output.
    pub(crate) proof: String,
    /// Whether the sandboxed payload may already have produced effects.
    pub(crate) partial_effect_warning: bool,
    /// Primary client that approved the exact retry, when decided.
    pub(crate) approving_client_id: Option<String>,
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

/// Cache identity for one request-scoped pane environment evidence result.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RuntimeEnvironmentEvidenceCacheKey {
    /// Pane whose active process environment supplies the values.
    pub(crate) pane_id: String,
    /// Stable bootstrap identity of the pane environment.
    pub(crate) environment_signature: String,
    /// Configuration generation that selected the requested names.
    pub(crate) config_generation: u64,
    /// Exact turn whose action requested these values.
    pub(crate) turn_id: String,
    /// Exact action whose launch may consume these values.
    pub(crate) action_id: String,
    /// Exact validated variable-name request.
    pub(crate) request: mez_agent::shell::PaneEnvironmentRequest,
}

/// Tracks a shell-backed `apply_patch` action across batched read phases.
///
/// Large patch read snapshots can exceed a pane PTY capture budget when every
/// touched path is read in one transaction. The runtime keeps this state while
/// dispatching one read transaction per path and then builds the verified write
/// phase from the accumulated snapshot outputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeApplyPatchBatchState {
    /// Filesystem boundary selected from the action's live sandbox mode.
    pub(crate) path_boundary: mez_agent::semantic_patch_planning::ApplyPatchPathBoundary,
    /// Paths that still need read-phase snapshots.
    pub(crate) remaining_paths: Vec<String>,
    /// Path owned by the currently running one-file read transaction.
    pub(crate) current_path: Option<String>,
    /// Number of clean snapshot retries already dispatched for the current path.
    pub(crate) current_path_read_retries: u8,
    /// Full transport bytes captured for the currently running read-phase batch.
    ///
    /// Pane previews stay size-bounded for display, but write-phase planning
    /// still needs the complete snapshot payload bytes so large read batches can
    /// be verified after preview text truncates or normalizes lossy UTF-8.
    pub(crate) current_read_transport: Vec<u8>,
    /// Decoded read-phase outputs that completed without transport truncation.
    pub(crate) read_outputs: Vec<String>,
}

/// Retains one generated `apply_patch` phase while a pre-shell hook runs.
///
/// Generated read retries and write phases are concrete pane-shell commands.
/// Retaining their exact plan ensures hook completion resumes that command
/// through the ordinary final authorization boundary rather than rebuilding or
/// bypassing the phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePendingApplyPatchPhase {
    /// Exact generated plan awaiting the pre-shell hook or final dispatch.
    pub(crate) plan: LocalActionPlan,
    /// Filesystem boundary embedded in the generated plan.
    pub(crate) path_boundary: mez_agent::semantic_patch_planning::ApplyPatchPathBoundary,
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
    /// Runtime-owned blocking external editor using the pane foreground PTY.
    ExternalEditor {
        /// Opaque editor-session identity.
        session_id: String,
        /// Completion nonce retained outside pane output.
        completion_nonce: String,
    },
    /// Readiness probe that resumes one primary client's prompt-editor request.
    AgentPromptEditorReadinessProbe {
        /// Attached primary client that requested external prompt editing.
        primary_client_id: String,
    },
    /// Stateful configured hook executed in the focused pane shell.
    FocusedShellHook,
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
    /// Syntax-neutral probe that discovers the active pane shell before
    /// dialect-specific bootstrap source is rendered.
    ShellIdentityProbe {
        /// Primary pane process fenced when the probe was registered.
        primary_process_id: u32,
        /// Shell-interaction generation fenced when the probe was registered.
        interaction_generation: u64,
    },
    /// Internal read-only canonical path-resolution transaction.
    PathResolution {
        /// Cache identity captured before the transaction was dispatched.
        cache_key: RuntimePathResolutionCacheKey,
        /// Every turn/action pair awaiting this exact resolved authority.
        /// Provider-only resolutions retain an empty collection.
        waiters: Vec<(String, String)>,
    },
    /// Internal request-scoped pane environment evidence transaction.
    EnvironmentEvidence {
        /// Exact cache identity captured before pane dispatch.
        cache_key: RuntimeEnvironmentEvidenceCacheKey,
        /// Every turn/action pair awaiting this evidence.
        waiters: Vec<(String, String)>,
    },
    /// Internal Bubblewrap runtime-profile capability probe.
    BubblewrapCapabilityProbe {
        /// Pending action that initiated the probe.
        action_id: String,
        /// Every turn/action pair awaiting this exact probe, including the
        /// initiating action. Each terminal probe path settles every waiter.
        waiters: Vec<(String, String)>,
        /// Exact capability identity captured before pane dispatch.
        cache_key: Box<crate::security::sandbox::BubblewrapCapabilityCacheKey>,
        /// Exact deterministic probe plan whose output must be validated.
        probe_plan: crate::security::sandbox::BubblewrapCapabilityProbePlan,
    },
    /// Internal Seatbelt runtime-profile capability probe.
    SeatbeltCapabilityProbe {
        /// Pending action that initiated the probe.
        action_id: String,
        /// Every turn/action pair awaiting this exact probe, including the
        /// initiating action. Each terminal probe path settles every waiter.
        waiters: Vec<(String, String)>,
        /// Exact capability identity captured before pane dispatch.
        cache_key: Box<crate::security::sandbox::SeatbeltCapabilityCacheKey>,
        /// Exact deterministic probe plan whose output must be validated.
        probe_plan: crate::security::sandbox::SeatbeltCapabilityProbePlan,
    },
}

/// Timer-visible kind for a live shell transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeShellTransactionTimerKind {
    /// Agent shell command action timeout.
    AgentAction,
    /// External editor transaction timeout family (normally unbounded).
    ExternalEditor,
    /// Readiness probe timeout.
    ReadinessProbe,
    /// Pane bootstrap transaction or completion-certification timeout.
    Bootstrap,
    /// Pane-shell canonical path-resolution timeout.
    PathResolution,
    /// Pane environment evidence timeout.
    EnvironmentEvidence,
    /// Bubblewrap runtime-profile capability probe timeout.
    BubblewrapCapabilityProbe,
    /// Seatbelt runtime-profile capability probe timeout.
    SeatbeltCapabilityProbe,
    /// Focused-shell hook marker timeout.
    FocusedShellHook,
}

/// Timer-visible snapshot of live shell work, including post-transaction
/// certification that still gates bootstrap settlement.
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
    /// Digest of the exact concrete shell phase guarded by the hook.
    pub(crate) phase_command_sha256: String,
}

/// One blocking program hook whose async result gates a stored shell action.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PendingProgramHookContinuation {
    /// Turn that owns the shell action waiting on the hook result.
    pub(crate) turn_id: String,
    /// Action to resume or deny after the hook result is known.
    pub(crate) action_id: String,
    /// Digest of the exact concrete shell phase guarded by the hook.
    pub(crate) phase_command_sha256: String,
    /// Configured hook identity used to reject duplicate dispatches.
    pub(crate) hook_id: String,
}

impl PendingProgramHookContinuation {
    /// Builds a pending identity from the established shell continuation.
    pub(crate) fn new(
        continuation: &PendingFocusedShellHookContinuation,
        hook_id: impl Into<String>,
    ) -> Self {
        Self {
            turn_id: continuation.turn_id.clone(),
            action_id: continuation.action_id.clone(),
            phase_command_sha256: continuation.phase_command_sha256.clone(),
            hook_id: hook_id.into(),
        }
    }

    /// Projects the pending identity back to shell-action continuation state.
    pub(crate) fn shell_continuation(&self) -> PendingFocusedShellHookContinuation {
        PendingFocusedShellHookContinuation {
            turn_id: self.turn_id.clone(),
            action_id: self.action_id.clone(),
            phase_command_sha256: self.phase_command_sha256.clone(),
        }
    }
}

/// Completed pre-shell hook identity for a running action.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RuntimeAgentPreShellHookCompletion {
    /// Turn whose pending action ran the hook.
    pub(crate) turn_id: String,
    /// Shell action guarded by the hook.
    pub(crate) action_id: String,
    /// Digest of the exact concrete shell phase guarded by the hook.
    pub(crate) phase_command_sha256: String,
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
