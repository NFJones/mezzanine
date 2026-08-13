//! Runtime Processes implementation.
//!
//! This module owns the runtime processes boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.
mod bash_compat;
mod fish_compat;
mod layout;
pub(crate) mod output_filter;
mod pane_pipes;
mod startup;
mod transactions;
mod zsh_compat;

use mez_mux::presentation::{pane_content_size_for_geometry, rendered_window_body_size};

use super::{
    ActionContentBlock, ActionResult, ActionStatus, ActivePanePipe, AgentId, AgentTurnRecord,
    AgentTurnState, AuditActor, BTreeSet, ClipboardAuthorization, ClipboardDecision,
    CommandInvocation, CommandOutcome, EnvironmentSignature, EventKind, ExitedPaneProcess,
    HookEvent, HookExecutionResult, HookExecutionStatus, HookFailure, HookFailureKind, MezError,
    PaneDescriptor, PaneExitRecord, PaneExitStatus, PaneExitUpdate, PaneId, PaneOutputUpdate,
    PaneProcessManager, PaneProcessOutput, PaneProcessStart, PaneReadinessState, PaneResizeUpdate,
    PaneSizeSpec, Path, PathBuf, ReadinessOverrideRevocation, Result, RunningShellTransactionKind,
    RunningShellTransactionRef, RuntimeHookPipelineBlock, RuntimeLifecycleState,
    RuntimeSessionService, RuntimeShellTransactionActionFailure, RuntimeShellTransactionTimerKind,
    RuntimeShellTransactionTimerRef, SessionSnapshotPayload, ShellClassification, ShellTransaction,
    Size, SplitDirection, StoppedPanePipe, TerminalClipboardOperation, TerminalClipboardRequest,
    TerminalOscEvent, TerminalScreen, WindowId, current_unix_millis, current_unix_seconds,
    decode_shell_output_transport_with_diagnostics, execute_mark_pane_ready_command,
    focused_shell_pre_action_timeout_result, hook_execution_audit_record, json_escape,
    local_action_plan, new_window_pane_size, optional_i32_json, pane_environment_with_term,
    plan_terminal_clipboard_request, postprocess_shell_action_success_output,
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_name,
    runtime_execution_ready_for_provider_continuation, runtime_hook_event_name,
    runtime_hook_execution_status_name, runtime_marker_for_action,
    runtime_pane_readiness_state_name, runtime_post_shell_hook_payload,
    runtime_random_marker_token, shell_command_result_content, validate_pane_size,
};
use crate::host::terminal::parse_mez_shell_transaction_osc;
use crate::runtime::config::{PaneSpawnDirectoryPolicy, PaneSpawnPolicy, PaneSpawnViewPolicy};
use crate::runtime::service_state::ProgramOwnedPaneTitle;
use crate::runtime::{
    PaneEvent, PaneForegroundProcessObservation, PaneProcessInstance, PaneProcessIoEffect,
    ProcessEvent, RenderInvalidationReason, RuntimeSideEffect, RuntimeTransition,
};
use mez_agent::instructions::DiscoveredInstructionFile;
use mez_agent::semantic_patch_planning::{
    ApplyPatchTransactionPhase, apply_patch_transaction_phase,
};
use mez_agent::shell_observation::{
    agent_shell_transaction_bytes_before_end_marker, agent_shell_transaction_observation_bytes,
    find_byte_subsequence, latest_agent_shell_transaction_output_lines,
    mez_wrapper_echo_line_is_hidden, mez_wrapper_echo_line_is_possible_prefix,
    mez_wrapper_echo_line_visible_bytes, mez_wrapper_filter_bytes_may_contain_boilerplate,
    renderable_shell_transaction_bytes,
};
use mez_agent::{AgentActionPayload, ToolInventory};
use mez_agent::{
    DEFAULT_BOOTSTRAP_TIMEOUT_MS, SHELL_OUTPUT_BASE64_BEGIN_MARKER, SHELL_OUTPUT_BASE64_END_MARKER,
    SHELL_OUTPUT_BASE64_MAX_RAW_BYTES, bootstrap_script_for_classification,
    parse_bootstrap_env_output, readiness_probe_command_for_classification,
};
use mez_mux::process::PaneProcess;
use mez_terminal::TerminalStyledLine;

pub(crate) use transactions::{
    BubblewrapEnvironmentProfile, RUNTIME_APPLY_PATCH_SNAPSHOT_OBSERVATION_LIMIT_BYTES,
};
use transactions::{
    RUNTIME_HIDDEN_SHELL_RENDER_RETENTION_POLLS,
    RUNTIME_SHELL_WRAPPER_FILTER_COMMAND_LINE_LIMIT_BYTES,
    RUNTIME_SHELL_WRAPPER_FILTER_PENDING_LIMIT_BYTES,
    RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT,
    RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS, runtime_running_shell_transaction_kind_name,
};

/// Identifies one independently retained pane presentation surface.
// Dependency-gated refactor slices consume this type after the storage
// foundation lands, so it is intentionally dormant in non-test builds here.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PaneSurfaceKind {
    /// Terminal emulator state owned by the pane's shell or active process.
    Process,
    /// Product-authored log state owned by the pane's active agent conversation.
    Agent,
}

/// Conversation-bound terminal state for one pane's agent log.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct AgentPaneScreen {
    /// Conversation whose presentation entries own this screen.
    conversation_id: String,
    /// Independently retained terminal state for agent presentation.
    screen: TerminalScreen,
}

#[allow(dead_code)]
impl AgentPaneScreen {
    /// Returns the conversation that owns this agent presentation screen.
    pub(crate) fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    /// Returns the retained terminal screen for this agent conversation.
    pub(crate) fn screen(&self) -> &TerminalScreen {
        &self.screen
    }

    /// Returns mutable retained terminal state for agent presentation.
    pub(crate) fn screen_mut(&mut self) -> &mut TerminalScreen {
        &mut self.screen
    }
}

/// Runtime-owned identity for one process handle moved to an async adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DetachedPaneProcess {
    /// Primary process id retained for lifecycle diagnostics and exit fencing.
    primary_pid: u32,
    /// Monotonic generation assigned when adapter ownership begins.
    generation: u64,
}

/// Provenance for a non-primary shell boundary trusted for pane dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeCertifiedShellSource {
    /// A Mezzanine-owned agent subshell completed its registered bootstrap.
    AgentSubshellBootstrap,
}

impl RuntimeCertifiedShellSource {
    /// Returns the stable diagnostic name for this certification source.
    fn as_str(self) -> &'static str {
        match self {
            Self::AgentSubshellBootstrap => "agent-subshell-bootstrap",
        }
    }
}

/// Stable reason that an agent-subshell bootstrap could not certify a shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeAgentSubshellCertificationRejection {
    /// The registered bootstrap never produced start-boundary evidence.
    MissingStartEvidence,
    /// The pane primary process changed before certification settled.
    PrimaryProcessChanged,
    /// The shell-interaction generation changed before certification settled.
    InteractionGenerationChanged,
    /// The runtime could not observe the foreground process group.
    ForegroundProcessUnavailable,
    /// Start and completion boundaries reported different foreground groups.
    ForegroundProcessGroupChanged,
    /// The bootstrap transaction returned a non-zero status.
    TransactionFailed,
    /// Bootstrap output exceeded the bounded observation limit.
    OutputTruncated,
    /// Successful output did not contain a parseable environment signature.
    EnvironmentSignatureMissing,
    /// The exact completion observation did not settle before its runtime deadline.
    ForegroundObservationTimedOut,
}

impl RuntimeAgentSubshellCertificationRejection {
    /// Returns the stable machine-readable rejection code.
    fn as_str(self) -> &'static str {
        match self {
            Self::MissingStartEvidence => "missing_start_evidence",
            Self::PrimaryProcessChanged => "primary_process_changed",
            Self::InteractionGenerationChanged => "interaction_generation_changed",
            Self::ForegroundProcessUnavailable => "foreground_process_unavailable",
            Self::ForegroundProcessGroupChanged => "foreground_process_group_changed",
            Self::TransactionFailed => "transaction_failed",
            Self::OutputTruncated => "output_truncated",
            Self::EnvironmentSignatureMissing => "environment_signature_missing",
            Self::ForegroundObservationTimedOut => "foreground_observation_timed_out",
        }
    }
}

/// Typed result of settling one possible agent-subshell certification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeAgentSubshellCertificationOutcome {
    /// The completed bootstrap was not bound to an agent-subshell handoff.
    NotApplicable,
    /// Static proof passed and an adapter-owned fresh PTY observation is pending.
    Pending,
    /// Complete proof certified the persistent agent subshell.
    Certified,
    /// Proof was applicable but rejected for a stable reason.
    Rejected(RuntimeAgentSubshellCertificationRejection),
}

/// Stable reason that pane environment authority settled unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimePaneEnvironmentAuthorityUnavailableReason {
    /// Bootstrap completed successfully but did not publish an environment.
    EnvironmentSignatureMissing,
    /// Bootstrap output exceeded the bounded observation limit.
    BootstrapOutputTruncated,
    /// Bootstrap returned a non-zero status before publishing an environment.
    BootstrapTransactionFailed,
    /// Bootstrap exceeded its bounded runtime deadline.
    BootstrapTimedOut,
    /// Bootstrap input could not be delivered to the pane process.
    BootstrapWriteFailed,
    /// Bootstrap violated its registered shell protocol boundary.
    BootstrapProtocolViolation,
    /// Syntax-neutral shell identity discovery failed before bootstrap.
    ShellIdentityProbeFailed,
    /// Agent-subshell certification rejected the discovered environment.
    AgentSubshellCertification(RuntimeAgentSubshellCertificationRejection),
}

impl RuntimePaneEnvironmentAuthorityUnavailableReason {
    /// Returns a stable diagnostic that identifies the failed authority boundary.
    pub(crate) fn diagnostic(self) -> String {
        match self {
            Self::EnvironmentSignatureMissing => {
                "pane bootstrap completed without a parseable environment signature".to_string()
            }
            Self::BootstrapOutputTruncated => {
                "pane bootstrap output was truncated before environment certification".to_string()
            }
            Self::BootstrapTransactionFailed => {
                "pane bootstrap failed before environment certification".to_string()
            }
            Self::BootstrapTimedOut => {
                "pane bootstrap timed out before environment certification".to_string()
            }
            Self::BootstrapWriteFailed => {
                "pane bootstrap input failed before environment certification".to_string()
            }
            Self::BootstrapProtocolViolation => {
                "pane bootstrap protocol failed before environment certification".to_string()
            }
            Self::ShellIdentityProbeFailed => {
                "pane shell identity probe failed before environment certification".to_string()
            }
            Self::AgentSubshellCertification(reason) => format!(
                "pane agent-subshell bootstrap certification failed: {}",
                reason.as_str()
            ),
        }
    }
}

/// Current authority available for pane-relative provider and sandbox work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimePaneEnvironmentAuthority {
    /// A bounded bootstrap or certification owner is still discovering authority.
    Pending,
    /// A current bootstrap-derived environment signature is published.
    Certified,
    /// Discovery settled without usable authority for a stable reason.
    Unavailable(RuntimePaneEnvironmentAuthorityUnavailableReason),
    /// No bootstrap result or current signature exists for the pane.
    Unknown,
}

impl RuntimePaneEnvironmentAuthority {
    /// Returns an actionable failure for settled or unsupported authority states.
    pub(crate) fn failure_message(self) -> Option<String> {
        match self {
            Self::Pending | Self::Certified => None,
            Self::Unavailable(reason) => Some(reason.diagnostic()),
            Self::Unknown => Some(
                "pane environment authority is unavailable because bootstrap has not certified this pane"
                    .to_string(),
            ),
        }
    }
}

/// Certified non-primary shell identity for one live pane-process epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePaneCertifiedShellIdentity {
    /// Primary pane process that owned the certified child-shell handoff.
    primary_process_id: u32,
    /// Foreground process group proven by the registered bootstrap protocol.
    process_group_id: u32,
    /// Monotonic shell-interaction generation that fences stale identities.
    interaction_generation: u64,
    /// Environment discovered by the bootstrap that certified this boundary.
    environment_signature: EnvironmentSignature,
    /// Runtime-owned provenance for the certification.
    source: RuntimeCertifiedShellSource,
}

/// Syntax-neutral shell evidence collected before dialect-specific bootstrap.
///
/// This provisional identity is valid only for the exact primary process and
/// interaction generation that owned its probe. Successful certified
/// bootstrap replaces it with a complete environment-backed identity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePaneProbedShellIdentity {
    /// Primary pane process that received the identity probe.
    primary_process_id: u32,
    /// Shell-interaction generation that received the identity probe.
    interaction_generation: u64,
    /// Atomically paired shell path, classification, and version evidence.
    execution_identity: RuntimePaneShellExecutionIdentity,
}

/// Atomically validated shell identity used to render and execute one pane
/// transaction.
///
/// Path, classification, version evidence, process identity, and interaction
/// generation are kept together so callers cannot select syntax and an
/// executable from different pane epochs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePaneShellExecutionIdentity {
    /// Absolute shell path valid in the active pane environment.
    shell_path: PathBuf,
    /// Shell grammar selected for wrapper rendering and authorization.
    classification: ShellClassification,
    /// Bounded version evidence used to classify renamed executables.
    version_probe: Option<String>,
    /// Primary pane process fenced by this identity when available.
    primary_process_id: Option<u32>,
    /// Shell-interaction generation fenced by a certified child identity.
    interaction_generation: Option<u64>,
}

impl RuntimePaneShellExecutionIdentity {
    /// Returns the absolute executable path paired with this identity.
    pub(crate) fn shell_path(&self) -> &Path {
        &self.shell_path
    }

    /// Returns the shell grammar paired with this executable path.
    pub(crate) fn classification(&self) -> ShellClassification {
        self.classification
    }

    /// Returns bounded runtime version evidence, when available.
    #[cfg(test)]
    pub(crate) fn version_probe(&self) -> Option<&str> {
        self.version_probe.as_deref()
    }

    /// Returns the pane primary process fenced by this identity.
    #[cfg(test)]
    pub(crate) fn primary_process_id(&self) -> Option<u32> {
        self.primary_process_id
    }

    /// Returns the certified shell-interaction generation, when applicable.
    #[cfg(test)]
    pub(crate) fn interaction_generation(&self) -> Option<u64> {
        self.interaction_generation
    }
}

/// Builds one transaction identity from an atomically published pane
/// environment signature.
fn runtime_shell_execution_identity_from_signature(
    signature: &EnvironmentSignature,
    primary_process_id: Option<u32>,
    interaction_generation: Option<u64>,
) -> Result<RuntimePaneShellExecutionIdentity> {
    let shell_path = PathBuf::from(&signature.shell_path);
    mez_agent::validate_resolved_shell_path(&shell_path)
        .map_err(|error| MezError::invalid_state(error.message()))?;
    let version_probe = signature.shell_version.clone();
    let probed_classification =
        ShellClassification::classify_with_probe(&shell_path, version_probe.as_deref());
    if version_probe.is_some() && probed_classification != signature.shell_classification {
        return Err(MezError::invalid_state(
            "pane shell path, classification, and version evidence are inconsistent",
        ));
    }
    Ok(RuntimePaneShellExecutionIdentity {
        shell_path,
        classification: signature.shell_classification,
        version_probe,
        primary_process_id,
        interaction_generation,
    })
}

/// Pending runtime-owned handoff from the primary shell to an agent subshell.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePaneShellHandoff {
    /// Primary pane process that received the child-shell launch command.
    primary_process_id: u32,
    /// Shell-interaction generation assigned to this handoff.
    interaction_generation: u64,
    /// Exact bootstrap marker registered after the handoff command.
    bootstrap_marker: Option<String>,
    /// Registered bootstrap wrapper held until the child prompt is observed.
    deferred_bootstrap_wrapper: Option<String>,
}

/// Persistent shell receiver observed when a handoff bootstrap emitted its start marker.
///
/// Payload release occurs only after this evidence is captured, so isolated
/// transaction children cannot be promoted as the persistent shell identity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeBootstrapShellCertificationEvidence {
    /// Pane that emitted the registered bootstrap marker.
    pane_id: String,
    /// Primary process identity captured for lifecycle fencing.
    primary_process_id: u32,
    /// Persistent receiver's foreground process group observed at transaction start.
    process_group_id: Option<u32>,
    /// Shell-interaction generation associated with the bootstrap marker.
    interaction_generation: u64,
}

/// Bootstrap start boundary waiting on a fresh pane-worker observation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePendingAgentSubshellStartObservation {
    /// Exact adapter-owned pane process lifetime that must answer.
    instance: PaneProcessInstance,
    /// Correlation token required on the worker observation event.
    observation_id: String,
    /// Exact bootstrap marker whose receiver is waiting for its payload.
    marker: String,
}

/// Parsed bootstrap context withheld until shell certification succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePendingBootstrapEnvironment {
    /// Environment identity used to key path and tool authority.
    signature: EnvironmentSignature,
    /// Tool inventory discovered under the pending environment.
    tool_inventory: Option<ToolInventory>,
    /// Project instruction files discovered under the pending environment.
    instruction_files: Vec<DiscoveredInstructionFile>,
}

/// Agent-subshell certification waiting on a fresh pane-worker observation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePendingAgentSubshellCertification {
    /// Exact bootstrap marker that established the protocol boundaries.
    marker: String,
    /// Exact adapter-owned pane process lifetime that must answer.
    instance: PaneProcessInstance,
    /// Correlation token required on the worker observation event.
    observation_id: String,
    /// Start-boundary proof for the persistent receiver.
    evidence: RuntimeBootstrapShellCertificationEvidence,
    /// Parsed bootstrap context published only after certification.
    environment: RuntimePendingBootstrapEnvironment,
    /// Unix timestamp when runtime-owned completion certification began.
    started_at_unix_ms: u64,
    /// Maximum runtime wait for the exact correlated completion observation.
    timeout_ms: u64,
}

/// One recovery-owned foreground observation for a blocked shell dispatch.
///
/// This owner is deliberately distinct from bootstrap certification: it never
/// releases bootstrap payloads or writes pane input.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePendingShellDispatchRecoveryObservation {
    /// Exact adapter-owned pane process lifetime that must answer.
    instance: PaneProcessInstance,
    /// Correlation token required on the worker observation event.
    observation_id: String,
    /// Turn whose shell action remains undispatched.
    turn_id: String,
    /// Pending shell action guarded by this observation.
    action_id: String,
    /// Primary process identity fenced by the observation.
    primary_process_id: u32,
    /// Shell interaction generation fenced by the observation.
    interaction_generation: u64,
    /// Unix timestamp when this exact worker observation was requested.
    started_at_unix_ms: u64,
}

/// Owns live process metadata that is private to the pane process subsystem.
///
/// Detached process ids, observed foreground groups, and program-owned title
/// lifetimes change together with pane process events. Keeping them behind
/// this component prevents unrelated runtime leaves from mutating incomplete
/// process metadata.
#[derive(Debug, Default)]
pub(crate) struct RuntimeProcessComponent {
    /// Live terminal and shell settings applied to process state.
    settings: RuntimeProcessSettings,
    /// Live pane process handles and their PTY lifecycle manager.
    pane_processes: PaneProcessManager,
    /// Best-known current working directory for each pane process.
    pane_current_working_directories: std::collections::BTreeMap<String, PathBuf>,
    /// Latest readiness state observed for each pane shell.
    pane_readiness_states: std::collections::BTreeMap<String, PaneReadinessState>,
    /// Explicit readiness overrides and pending probe epochs.
    pane_readiness_overrides: mez_agent::PaneReadinessOverrideStore,
    /// Bootstrap-derived environment signatures keyed by pane id.
    pane_environment_signatures: std::collections::BTreeMap<String, EnvironmentSignature>,
    /// Managed passive Fish integration state keyed by pane process.
    pane_fish_compatibility:
        std::collections::BTreeMap<String, fish_compat::ManagedFishCompatibility>,
    /// Managed Bash private receiver state keyed by pane id.
    pane_bash_compatibility:
        std::collections::BTreeMap<String, bash_compat::ManagedBashCompatibility>,
    /// Managed zsh startup and history-authentication state keyed by pane id.
    pane_zsh_compatibility: std::collections::BTreeMap<String, zsh_compat::ManagedZshCompatibility>,
    /// Canonical path authority keyed by pane environment, config generation,
    /// and the exact bounded resolution request.
    pane_path_scopes: std::collections::BTreeMap<
        crate::runtime::RuntimePathResolutionCacheKey,
        mez_agent::permissions::PathScopes,
    >,
    /// Fail-closed resolver outcomes keyed by the same exact authority identity.
    pane_path_scope_failures:
        std::collections::BTreeMap<crate::runtime::RuntimePathResolutionCacheKey, String>,
    /// Protected pane-derived environment values keyed by exact request identity.
    pane_environment_evidence: std::collections::BTreeMap<
        crate::runtime::RuntimeEnvironmentEvidenceCacheKey,
        mez_agent::shell::PaneEnvironmentEvidence,
    >,
    /// Mapping-warning identities already retained in each pane log.
    pub(crate) sandbox_mapping_warnings_emitted: BTreeSet<String>,
    /// Successful Bubblewrap probes keyed by exact pane-environment and
    /// runtime-profile identity.
    pane_bubblewrap_capabilities: std::collections::BTreeMap<
        crate::security::sandbox::BubblewrapCapabilityCacheKey,
        crate::security::sandbox::BubblewrapCapability,
    >,
    /// Panes with an in-flight bootstrap transaction.
    pane_bootstrap_pending: BTreeSet<String>,
    /// Stable unavailable outcomes retained after bootstrap settlement.
    pane_environment_authority_failures:
        std::collections::BTreeMap<String, RuntimePaneEnvironmentAuthorityUnavailableReason>,
    /// Authoritative process terminal screen state keyed by pane id.
    process_pane_screens: std::collections::BTreeMap<String, TerminalScreen>,
    /// Conversation-bound agent log screen state keyed by pane id.
    agent_pane_screens: std::collections::BTreeMap<String, AgentPaneScreen>,
    /// Live shell transactions keyed by their OSC marker.
    running_shell_transactions: std::collections::BTreeMap<String, RunningShellTransactionRef>,
    /// Exact process instances holding each transaction's exclusive input lease.
    shell_transaction_input_leases: std::collections::BTreeMap<String, PaneProcessInstance>,
    /// Immutable bounded wrapper-filter commands keyed by transaction marker.
    ///
    /// Registration derives this descriptor once so steady-state PTY output
    /// filtering never scans a potentially multi-megabyte transaction command.
    shell_transaction_wrapper_filter_commands:
        std::collections::BTreeMap<String, std::sync::Arc<[String]>>,
    /// Markers whose runtime wrappers must emit start before completion.
    shell_transaction_require_start_markers: BTreeSet<String>,
    /// Markers whose mandatory wrapper start event has been observed.
    shell_transaction_started_markers: BTreeSet<String>,
    /// Fish transaction markers awaiting their receiver-ready admission event.
    shell_transaction_payload_receiver_ready_required: BTreeSet<String>,
    /// Incomplete mandatory start-marker bytes retained across PTY reads.
    shell_transaction_start_boundary_pending: std::collections::BTreeMap<String, Vec<u8>>,
    /// Incomplete transaction end-marker bytes retained across PTY reads.
    shell_transaction_end_boundary_pending: std::collections::BTreeMap<String, Vec<u8>>,
    /// Incomplete private control OSC bytes retained across PTY reads.
    shell_transaction_control_osc_pending: std::collections::BTreeMap<String, Vec<u8>>,
    /// Incomplete UTF-8 suffixes retained per shell transaction across PTY reads.
    shell_transaction_output_utf8_pending: std::collections::BTreeMap<String, Vec<u8>>,
    /// Remaining receiver acknowledgements owned by each active transaction.
    ///
    /// Counts are installed only when a negotiated deferred payload is
    /// released. Filtering is therefore marker-scoped and cannot suppress an
    /// ordinary child process record-separator byte after the payload drains.
    shell_transaction_receiver_acknowledgements: std::collections::BTreeMap<String, usize>,
    /// Managed Bash source stages retained until authenticated receiver admission.
    shell_receiver_pending_payloads: std::collections::BTreeMap<
        String,
        std::collections::VecDeque<mez_mux::process::ShellInputDelivery>,
    >,
    /// Transactions whose inner end marker cannot settle before receiver completion.
    shell_receiver_completion_required: BTreeSet<String>,
    /// Inner transaction-end metadata retained until the Bash callback completes.
    shell_receiver_pending_ends: std::collections::BTreeMap<String, (String, String, String, i32)>,
    /// Agent-action markers whose child launch uses the Bubblewrap backend.
    sandboxed_shell_transaction_markers: BTreeSet<String>,
    /// Shared managed-home activity locks retained for sandboxed workloads.
    managed_home_activity_locks: std::collections::BTreeMap<
        String,
        crate::security::sandbox::BubblewrapManagedHomeActivityLock,
    >,
    /// Active pane output pipes keyed by their source pane id.
    active_pane_pipes: std::collections::BTreeMap<String, ActivePanePipe>,
    /// Process identity for panes whose handles are adapter-owned.
    detached_pane_processes: std::collections::BTreeMap<String, DetachedPaneProcess>,
    /// Next monotonic adapter-owned process generation.
    next_detached_pane_generation: u64,
    /// Latest foreground process groups observed by pane workers.
    pane_foreground_process_groups: std::collections::BTreeMap<String, u32>,
    /// Certified non-primary shell identities keyed by pane id.
    pane_certified_shell_identities:
        std::collections::BTreeMap<String, RuntimePaneCertifiedShellIdentity>,
    /// Pre-bootstrap shell identities keyed by pane id and fenced by the
    /// current process and shell-interaction generation.
    pane_probed_shell_identities:
        std::collections::BTreeMap<String, RuntimePaneProbedShellIdentity>,
    /// Runtime-owned agent-subshell handoffs awaiting bootstrap proof.
    pane_shell_handoffs: std::collections::BTreeMap<String, RuntimePaneShellHandoff>,
    /// Bootstrap-start foreground evidence keyed by exact transaction marker.
    bootstrap_shell_certification_evidence:
        std::collections::BTreeMap<String, RuntimeBootstrapShellCertificationEvidence>,
    /// Bootstrap start boundaries awaiting fresh adapter-owned PTY metadata.
    pending_agent_subshell_start_observations:
        std::collections::BTreeMap<String, RuntimePendingAgentSubshellStartObservation>,
    /// Parsed bootstrap context awaiting a correlated pane-worker observation.
    pending_agent_subshell_certifications:
        std::collections::BTreeMap<String, RuntimePendingAgentSubshellCertification>,
    /// Recovery-owned fresh foreground observations for blocked shell actions.
    pending_shell_dispatch_recovery_observations:
        std::collections::BTreeMap<String, RuntimePendingShellDispatchRecoveryObservation>,
    /// Next opaque identity for a recovery-owned foreground observation.
    next_shell_dispatch_recovery_observation: u64,
    /// Latest actionable agent-subshell certification rejection per pane.
    pane_agent_subshell_certification_rejections:
        std::collections::BTreeMap<String, RuntimeAgentSubshellCertificationRejection>,
    /// Current shell-interaction generation keyed by pane id.
    pane_shell_interaction_generations: std::collections::BTreeMap<String, u64>,
    /// Next monotonic shell-interaction generation.
    next_shell_interaction_generation: u64,
    /// Program-owned pane title state keyed by pane id.
    program_owned_pane_titles: std::collections::BTreeMap<String, ProgramOwnedPaneTitle>,
    /// Full terminal parsers retained for visible shell transaction streams.
    pane_transaction_osc_screens: std::collections::BTreeMap<String, TerminalScreen>,
    /// Incomplete private shell-output frames retained across visible PTY reads.
    pane_shell_output_render_pending: std::collections::BTreeMap<String, Vec<u8>>,
    /// Partial wrapper-filter bytes keyed by pane id.
    pane_mez_wrapper_filter_pending: std::collections::BTreeMap<String, Vec<u8>>,
    /// Precomputed bounded wrapper-filter commands keyed by pane id.
    pane_mez_wrapper_filter_recent_commands:
        std::collections::BTreeMap<String, std::sync::Arc<[String]>>,
    /// Remaining wrapper-filter retention polls keyed by pane id.
    pane_mez_wrapper_filter_recent_polls: std::collections::BTreeMap<String, usize>,
    /// Remaining hidden-shell render retention polls keyed by pane id.
    pane_hidden_shell_render_recent_polls: std::collections::BTreeMap<String, usize>,
    /// Consecutive idle polls used to synchronize foreground titles.
    foreground_title_idle_sync_polls: usize,
    /// Terminal exit records retained for panes whose primary process ended.
    pane_exit_records: std::collections::BTreeMap<String, PaneExitRecord>,
    /// Panes whose process teardown has begun but is not yet fully reconciled.
    pane_closing: BTreeSet<String>,
    /// Test-only one-shot failure injected while interrupting a pane shell.
    #[cfg(test)]
    fail_next_pane_interrupt_write: bool,
    /// Test-only guard proving transaction ownership precedes pane delivery.
    #[cfg(test)]
    require_registered_transaction_on_next_write: bool,
}

/// Owns terminal configuration that controls pane process and screen behavior.
///
/// These values are parsed together during config application and must be
/// replaced together so newly spawned screens, existing history buffers, and
/// process environments observe one coherent settings generation.
#[derive(Debug, Clone)]
struct RuntimeProcessSettings {
    /// Maximum retained history lines for each pane screen.
    terminal_history_limit: usize,
    /// History lines removed in each overflow rotation batch.
    terminal_history_rotate_lines: usize,
    /// TERM value exported to pane processes and attached clients.
    terminal_term: String,
    /// Directory and initial-surface policies for ordinary pane creation.
    pane_spawn_policy: PaneSpawnPolicy,
    /// Emoji-width policy represented by the currently modeled pane screens.
    terminal_emoji_width: mez_terminal::TerminalEmojiWidth,
    /// Hidden shell output tail lines retained in action previews.
    terminal_shell_output_preview_lines: usize,
}

impl Default for RuntimeProcessSettings {
    fn default() -> Self {
        Self {
            terminal_history_limit: mez_terminal::DEFAULT_HISTORY_LIMIT,
            terminal_history_rotate_lines: mez_terminal::DEFAULT_HISTORY_ROTATE_LINES,
            terminal_term: mez_terminal::DEFAULT_PANE_TERM.to_string(),
            pane_spawn_policy: PaneSpawnPolicy::default(),
            terminal_emoji_width: mez_terminal::TerminalEmojiWidth::Wide,
            terminal_shell_output_preview_lines: 5,
        }
    }
}

impl RuntimeProcessComponent {
    /// Builds process ownership around the manager supplied by runtime construction.
    pub(crate) fn with_pane_processes(pane_processes: PaneProcessManager) -> Self {
        Self {
            pane_processes,
            ..Self::default()
        }
    }
}

impl RuntimeSessionService {
    /// Returns the number of active pane output pipes.
    pub(crate) fn active_pane_pipe_count(&self) -> usize {
        self.process.active_pane_pipes.len()
    }

    /// Registers one live shell transaction and its start-marker invariant.
    pub(crate) fn register_running_shell_transaction(
        &mut self,
        marker: String,
        transaction: RunningShellTransactionRef,
        require_start_marker: bool,
    ) {
        let filter_commands = if transaction.pending_input_payload.is_none() {
            self.remember_mez_wrapper_filter_command(&transaction.pane_id, &transaction.command)
        } else {
            std::sync::Arc::from(Vec::<String>::new())
        };
        self.process
            .shell_transaction_wrapper_filter_commands
            .insert(marker.clone(), filter_commands);
        self.process
            .running_shell_transactions
            .insert(marker.clone(), transaction);
        if let Some(instance) = self.adapter_owned_pane_process_instance(
            self.process
                .running_shell_transactions
                .get(&marker)
                .map_or("", |transaction| transaction.pane_id.as_str()),
        ) {
            self.process
                .shell_transaction_input_leases
                .insert(marker.clone(), instance.clone());
            self.persistence
                .queue_pane_input(RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: PaneProcessIoEffect::AcquireShellInputLease {
                        owner_id: marker.clone(),
                    },
                });
        }
        if require_start_marker {
            self.process
                .shell_transaction_require_start_markers
                .insert(marker);
        }
    }

    /// Requires a correlated Fish receiver-ready event before payload delivery.
    pub(crate) fn require_shell_transaction_payload_receiver_ready(&mut self, marker: &str) {
        self.process
            .shell_transaction_payload_receiver_ready_required
            .insert(marker.to_string());
    }

    /// Retains managed Bash source frames until the private receiver admits them.
    pub(crate) fn register_shell_receiver_payload(
        &mut self,
        marker: &str,
        payload: mez_mux::process::ShellInputDelivery,
    ) {
        self.process
            .shell_receiver_pending_payloads
            .entry(marker.to_string())
            .or_default()
            .push_back(payload);
        self.process
            .shell_receiver_completion_required
            .insert(marker.to_string());
    }

    /// Prepends one managed Bash source stage before already-retained work.
    pub(crate) fn prepend_shell_receiver_payload(
        &mut self,
        marker: &str,
        payload: mez_mux::process::ShellInputDelivery,
    ) {
        self.process
            .shell_receiver_pending_payloads
            .entry(marker.to_string())
            .or_default()
            .push_front(payload);
        self.process
            .shell_receiver_completion_required
            .insert(marker.to_string());
    }

    /// Reports whether an agent action has a live shell transaction.
    pub(crate) fn agent_action_has_running_shell_transaction(
        &self,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        self.process
            .running_shell_transactions
            .values()
            .any(|transaction| {
                transaction.turn_id == turn_id
                    && matches!(
                        &transaction.kind,
                        RunningShellTransactionKind::AgentAction {
                            action_id: running_action_id
                        } if running_action_id == action_id
                    )
            })
    }

    /// Reports whether a turn has any live agent-action shell transaction.
    pub(crate) fn turn_has_running_agent_action_shell_transaction(&self, turn_id: &str) -> bool {
        self.process
            .running_shell_transactions
            .values()
            .any(|transaction| {
                transaction.turn_id == turn_id
                    && matches!(
                        transaction.kind,
                        RunningShellTransactionKind::AgentAction { .. }
                    )
            })
    }

    /// Reports whether a turn has a live transaction of the requested kind.
    pub(crate) fn turn_has_running_shell_transaction_kind(
        &self,
        turn_id: &str,
        kind: &RunningShellTransactionKind,
    ) -> bool {
        self.process
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.turn_id == turn_id && &transaction.kind == kind)
    }

    /// Reports whether one pane has any live shell transaction.
    pub(crate) fn pane_has_running_shell_transaction(&self, pane_id: &str) -> bool {
        self.process
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == pane_id)
    }

    /// Reports whether a pending pane bootstrap has a bounded runtime progress
    /// owner capable of settling or timing out.
    pub(crate) fn pane_bootstrap_has_bounded_progress_owner(&self, pane_id: &str) -> bool {
        self.process
            .pending_agent_subshell_certifications
            .contains_key(pane_id)
            || self
                .process
                .running_shell_transactions
                .values()
                .any(|transaction| {
                    transaction.pane_id == pane_id
                        && transactions::runtime_shell_transaction_effective_timeout_ms(transaction)
                            .is_some()
                })
    }

    /// Reports whether one pane is awaiting correlated foreground-process
    /// certification for parsed bootstrap evidence.
    pub(crate) fn pane_agent_subshell_certification_is_pending(&self, pane_id: &str) -> bool {
        self.process
            .pending_agent_subshell_certifications
            .contains_key(pane_id)
    }

    /// Clears a recovery-owned foreground observation when its shell action no
    /// longer awaits foreground stabilization.
    pub(crate) fn clear_shell_dispatch_recovery_observations_for_action(
        &mut self,
        turn_id: &str,
        action_id: &str,
    ) {
        self.process
            .pending_shell_dispatch_recovery_observations
            .retain(|_, pending| pending.turn_id != turn_id || pending.action_id != action_id);
    }

    /// Clears every recovery-owned foreground observation for a settled turn.
    pub(crate) fn clear_shell_dispatch_recovery_observations_for_turn(&mut self, turn_id: &str) {
        self.process
            .pending_shell_dispatch_recovery_observations
            .retain(|_, pending| pending.turn_id != turn_id);
    }

    /// Returns marker and pane pairs for every live transaction in one turn.
    pub(crate) fn running_shell_transaction_targets_for_turn(
        &self,
        turn_id: &str,
    ) -> Vec<(String, String)> {
        self.process
            .running_shell_transactions
            .iter()
            .filter(|(_, transaction)| transaction.turn_id == turn_id)
            .map(|(marker, transaction)| (marker.clone(), transaction.pane_id.clone()))
            .collect()
    }

    /// Removes one live shell transaction by marker.
    pub(crate) fn remove_running_shell_transaction(
        &mut self,
        marker: &str,
    ) -> Option<RunningShellTransactionRef> {
        self.process.managed_home_activity_locks.remove(marker);
        if let Some(instance) = self.process.shell_transaction_input_leases.remove(marker) {
            self.persistence
                .queue_pane_input(RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: PaneProcessIoEffect::ReleaseShellInputLease {
                        owner_id: marker.to_string(),
                    },
                });
        }
        self.process
            .shell_transaction_wrapper_filter_commands
            .remove(marker);
        self.process
            .shell_transaction_receiver_acknowledgements
            .remove(marker);
        self.process
            .shell_transaction_output_utf8_pending
            .remove(marker);
        self.process
            .shell_transaction_start_boundary_pending
            .remove(marker);
        self.process
            .shell_transaction_end_boundary_pending
            .remove(marker);
        self.process
            .shell_transaction_control_osc_pending
            .remove(marker);
        self.process.running_shell_transactions.remove(marker)
    }

    /// Clears all live shell transactions and marker protocol state.
    pub(crate) fn clear_all_shell_transaction_state(&mut self) {
        self.process.running_shell_transactions.clear();
        self.process
            .shell_transaction_wrapper_filter_commands
            .clear();
        self.process.shell_transaction_require_start_markers.clear();
        self.process.shell_transaction_started_markers.clear();
        self.process
            .shell_transaction_start_boundary_pending
            .clear();
        self.process.shell_transaction_end_boundary_pending.clear();
        self.process.shell_transaction_control_osc_pending.clear();
        self.process.shell_transaction_output_utf8_pending.clear();
        self.process
            .shell_transaction_receiver_acknowledgements
            .clear();
        self.process.shell_receiver_pending_payloads.clear();
        self.process.shell_receiver_completion_required.clear();
        self.process.shell_receiver_pending_ends.clear();
        self.process.managed_home_activity_locks.clear();
    }

    /// Returns the active pane-screen history limit.
    pub(crate) fn terminal_history_limit(&self) -> usize {
        self.process.settings.terminal_history_limit
    }

    /// Returns the active pane-screen history rotation batch size.
    pub(crate) fn terminal_history_rotate_lines(&self) -> usize {
        self.process.settings.terminal_history_rotate_lines
    }

    /// Returns the TERM value exported to pane processes and clients.
    pub(crate) fn terminal_term(&self) -> &str {
        &self.process.settings.terminal_term
    }

    /// Applies one parsed generation of terminal process settings.
    pub(crate) fn apply_process_terminal_settings(
        &mut self,
        history_limit: usize,
        history_rotate_lines: usize,
        terminal_term: String,
        pane_spawn_policy: PaneSpawnPolicy,
        terminal_emoji_width: mez_terminal::TerminalEmojiWidth,
        shell_output_preview_lines: usize,
    ) -> Result<()> {
        self.configure_pane_screen_history(history_limit, history_rotate_lines)?;
        let emoji_width_changed =
            self.process.settings.terminal_emoji_width != terminal_emoji_width;
        self.process.settings = RuntimeProcessSettings {
            terminal_history_limit: history_limit,
            terminal_history_rotate_lines: history_rotate_lines,
            terminal_term,
            pane_spawn_policy,
            terminal_emoji_width,
            terminal_shell_output_preview_lines: shell_output_preview_lines,
        };
        mez_terminal::set_terminal_emoji_width(terminal_emoji_width);
        if emoji_width_changed {
            for screen in self.process.process_pane_screens.values_mut() {
                screen.rebuild_for_width_policy_change(terminal_emoji_width);
            }
            for agent_screen in self.process.agent_pane_screens.values_mut() {
                agent_screen
                    .screen_mut()
                    .rebuild_for_width_policy_change(terminal_emoji_width);
            }
            for screen in self.process.pane_transaction_osc_screens.values_mut() {
                screen.rebuild_for_width_policy_change(terminal_emoji_width);
            }
        }
        Ok(())
    }

    /// Returns all modeled pane screens for whole-layout presentation.
    pub(crate) fn process_pane_screens(
        &self,
    ) -> &std::collections::BTreeMap<String, TerminalScreen> {
        &self.process.process_pane_screens
    }

    /// Returns the authoritative process terminal screen for one pane.
    pub(crate) fn process_pane_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        self.process.process_pane_screens.get(pane_id)
    }

    /// Returns mutable authoritative process terminal state for one pane.
    pub(crate) fn process_pane_screen_mut(&mut self, pane_id: &str) -> Option<&mut TerminalScreen> {
        self.process.process_pane_screens.get_mut(pane_id)
    }

    /// Replaces the authoritative process terminal screen for one pane.
    #[allow(
        dead_code,
        reason = "explicit process-screen fixture API used by test targets"
    )]
    pub(crate) fn set_process_pane_screen(
        &mut self,
        pane_id: impl Into<String>,
        screen: TerminalScreen,
    ) {
        self.process
            .process_pane_screens
            .insert(pane_id.into(), screen);
    }

    /// Returns the conversation-bound agent screen state for one pane.
    #[allow(dead_code)]
    pub(crate) fn agent_pane_screen_state(&self, pane_id: &str) -> Option<&AgentPaneScreen> {
        self.process.agent_pane_screens.get(pane_id)
    }

    /// Returns the retained agent terminal screen for one pane.
    #[allow(dead_code)]
    pub(crate) fn agent_pane_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        self.agent_pane_screen_state(pane_id)
            .map(AgentPaneScreen::screen)
    }

    /// Returns mutable retained agent terminal state for one pane.
    #[allow(dead_code)]
    pub(crate) fn agent_pane_screen_mut(&mut self, pane_id: &str) -> Option<&mut TerminalScreen> {
        self.process
            .agent_pane_screens
            .get_mut(pane_id)
            .map(AgentPaneScreen::screen_mut)
    }

    /// Replaces one pane's retained agent screen with a conversation-bound value.
    pub(crate) fn set_agent_pane_screen(
        &mut self,
        pane_id: impl Into<String>,
        conversation_id: impl Into<String>,
        screen: TerminalScreen,
    ) {
        let pane_id = pane_id.into();
        self.clear_interaction_state_for_surface(&pane_id, PaneSurfaceKind::Agent);
        self.process.agent_pane_screens.insert(
            pane_id,
            AgentPaneScreen {
                conversation_id: conversation_id.into(),
                screen,
            },
        );
    }

    /// Removes one pane's retained agent screen during replacement rollback.
    pub(crate) fn remove_agent_pane_screen(&mut self, pane_id: &str) {
        self.clear_interaction_state_for_surface(pane_id, PaneSurfaceKind::Agent);
        self.process.agent_pane_screens.remove(pane_id);
    }

    /// Ensures one pane has an agent screen bound to the requested conversation.
    ///
    /// A conversation change replaces the prior screen instead of allowing
    /// delayed presentation from one session to contaminate another session.
    #[allow(dead_code)]
    pub(crate) fn ensure_agent_pane_screen(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        size: Size,
    ) -> Result<&mut TerminalScreen> {
        let replace = self
            .process
            .agent_pane_screens
            .get(pane_id)
            .is_none_or(|screen| screen.conversation_id() != conversation_id);
        if replace {
            let screen = TerminalScreen::new_with_history_config(
                size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?;
            self.clear_interaction_state_for_surface(pane_id, PaneSurfaceKind::Agent);
            self.process.agent_pane_screens.insert(
                pane_id.to_string(),
                AgentPaneScreen {
                    conversation_id: conversation_id.to_string(),
                    screen,
                },
            );
        }
        self.agent_pane_screen_mut(pane_id)
            .ok_or_else(|| MezError::invalid_state("agent pane screen was not initialized"))
    }

    /// Returns the presentation surface selected by pane-local agent visibility.
    #[allow(dead_code)]
    pub(crate) fn presented_pane_surface(&self, pane_id: &str) -> PaneSurfaceKind {
        if self
            .agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.visibility != super::AgentShellVisibility::Hidden)
        {
            PaneSurfaceKind::Agent
        } else {
            PaneSurfaceKind::Process
        }
    }

    /// Returns the retained screen selected for presentation in one pane.
    #[allow(dead_code)]
    pub(crate) fn presented_pane_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        match self.presented_pane_surface(pane_id) {
            PaneSurfaceKind::Process => self.process_pane_screen(pane_id),
            PaneSurfaceKind::Agent => {
                let session = self.agent_shell_store().get(pane_id)?;
                let screen = self.agent_pane_screen_state(pane_id)?;
                (screen.conversation_id() == session.session_id).then(|| screen.screen())
            }
        }
    }

    /// Returns mutable terminal state for the surface selected for presentation.
    pub(crate) fn presented_pane_screen_mut(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut TerminalScreen> {
        match self.presented_pane_surface(pane_id) {
            PaneSurfaceKind::Process => self.process_pane_screen_mut(pane_id),
            PaneSurfaceKind::Agent => {
                let conversation_id = self.agent_shell_store().get(pane_id)?.session_id.clone();
                let screen = self.process.agent_pane_screens.get_mut(pane_id)?;
                if screen.conversation_id() != conversation_id {
                    return None;
                }
                Some(screen.screen_mut())
            }
        }
    }

    /// Returns the displayed screen through the temporary compatibility surface.
    ///
    /// Process protocol callers must use `process_pane_screen`; dependent
    /// refactor slices migrate remaining interaction callers to explicit
    /// presented-surface accessors before this compatibility API is removed.
    #[cfg(test)]
    pub(crate) fn pane_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        self.presented_pane_screen(pane_id)
    }

    /// Returns mutable displayed state through the temporary compatibility API.
    #[cfg(test)]
    pub(crate) fn pane_screen_mut(&mut self, pane_id: &str) -> Option<&mut TerminalScreen> {
        self.presented_pane_screen_mut(pane_id)
    }

    /// Replaces process state through the temporary compatibility API.
    #[allow(
        dead_code,
        reason = "compatibility fixture API retained during screen migration"
    )]
    pub(crate) fn set_pane_screen(&mut self, pane_id: impl Into<String>, screen: TerminalScreen) {
        self.set_process_pane_screen(pane_id, screen);
    }

    /// Clears modeled terminal state when the live session is replaced.
    pub(crate) fn clear_pane_screens(&mut self) {
        self.process.process_pane_screens.clear();
        self.process.agent_pane_screens.clear();
    }

    /// Applies new history retention policy to every modeled pane screen.
    pub(crate) fn configure_pane_screen_history(
        &mut self,
        history_limit: usize,
        rotate_lines: usize,
    ) -> Result<()> {
        let mut process_copy_invalidations = Vec::new();
        for (pane_id, screen) in &mut self.process.process_pane_screens {
            let previous_history_len = screen.history().len();
            screen.set_history_limit(history_limit)?;
            screen.set_history_rotate_lines(rotate_lines)?;
            if screen.history().len() != previous_history_len {
                process_copy_invalidations.push(pane_id.clone());
            }
        }
        let mut agent_copy_invalidations = Vec::new();
        for (pane_id, agent_screen) in &mut self.process.agent_pane_screens {
            let previous_history_len = agent_screen.screen().history().len();
            agent_screen.screen_mut().set_history_limit(history_limit)?;
            agent_screen
                .screen_mut()
                .set_history_rotate_lines(rotate_lines)?;
            if agent_screen.screen().history().len() != previous_history_len {
                agent_copy_invalidations.push(pane_id.clone());
            }
        }
        for pane_id in process_copy_invalidations {
            self.clear_copy_state_for_surface(&pane_id, PaneSurfaceKind::Process);
        }
        for pane_id in agent_copy_invalidations {
            self.clear_copy_state_for_surface(&pane_id, PaneSurfaceKind::Agent);
        }
        Ok(())
    }

    /// Returns the last readiness state observed for a pane shell.
    pub(crate) fn pane_readiness_state(&self, pane_id: &str) -> PaneReadinessState {
        self.process
            .pane_readiness_states
            .get(pane_id)
            .copied()
            .unwrap_or(PaneReadinessState::Unknown)
    }

    /// Records the current readiness state for one pane shell.
    pub(crate) fn set_pane_readiness(&mut self, pane_id: &str, state: PaneReadinessState) {
        self.process
            .pane_readiness_states
            .insert(pane_id.to_string(), state);
    }

    /// Revokes readiness authority for one pane after a shell lifecycle event.
    pub(crate) fn revoke_pane_readiness_override(
        &mut self,
        pane_id: &str,
        reason: ReadinessOverrideRevocation,
    ) {
        self.process
            .pane_readiness_overrides
            .revoke(pane_id, reason);
    }

    /// Reports whether one pane still has a readiness probe in flight.
    pub(crate) fn pane_readiness_override_has_pending_probe(&self, pane_id: &str) -> bool {
        self.process
            .pane_readiness_overrides
            .has_pending_probe(pane_id)
    }

    /// Executes the readiness override command against process-owned state.
    pub(crate) fn execute_pane_readiness_override_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        invocation: &CommandInvocation,
        current_state: PaneReadinessState,
        current_epoch: u64,
    ) -> Result<CommandOutcome> {
        execute_mark_pane_ready_command(
            &self.session,
            primary_client_id,
            &mut self.process.pane_readiness_overrides,
            invocation,
            current_state,
            current_epoch,
            self.persistence.audit_log_mut(),
        )
    }

    /// Returns the bootstrap-derived environment signature for one pane.
    pub(crate) fn pane_environment_signature(
        &self,
        pane_id: &str,
    ) -> Option<&EnvironmentSignature> {
        self.process.pane_environment_signatures.get(pane_id)
    }

    /// Returns the typed authority state for pane-relative provider work.
    pub(crate) fn pane_environment_authority(
        &self,
        pane_id: &str,
    ) -> RuntimePaneEnvironmentAuthority {
        if self.process.pane_bootstrap_pending.contains(pane_id) {
            return RuntimePaneEnvironmentAuthority::Pending;
        }
        if self
            .process
            .pane_environment_signatures
            .contains_key(pane_id)
        {
            return RuntimePaneEnvironmentAuthority::Certified;
        }
        if let Some(reason) = self
            .process
            .pane_agent_subshell_certification_rejections
            .get(pane_id)
            .copied()
        {
            return RuntimePaneEnvironmentAuthority::Unavailable(
                RuntimePaneEnvironmentAuthorityUnavailableReason::AgentSubshellCertification(
                    reason,
                ),
            );
        }
        self.process
            .pane_environment_authority_failures
            .get(pane_id)
            .copied()
            .map(RuntimePaneEnvironmentAuthority::Unavailable)
            .unwrap_or(RuntimePaneEnvironmentAuthority::Unknown)
    }

    /// Records a settled bootstrap failure and invalidates signature-bound caches.
    pub(crate) fn mark_pane_environment_authority_unavailable(
        &mut self,
        pane_id: &str,
        reason: RuntimePaneEnvironmentAuthorityUnavailableReason,
    ) {
        self.process.pane_environment_signatures.remove(pane_id);
        self.process
            .pane_path_scopes
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_path_scope_failures
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_environment_evidence
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_bubblewrap_capabilities
            .retain(|key, _| key.pane_id != pane_id);
        self.clear_pane_agent_instruction_files(pane_id);
        self.process
            .pane_environment_authority_failures
            .insert(pane_id.to_string(), reason);
    }

    /// Clears a stale unavailable result when bounded discovery starts or succeeds.
    pub(crate) fn clear_pane_environment_authority_failure(&mut self, pane_id: &str) {
        self.process
            .pane_environment_authority_failures
            .remove(pane_id);
    }

    /// Reports whether one pane is still awaiting its one-shot environment
    /// bootstrap attempt.
    pub(crate) fn pane_bootstrap_is_pending(&self, pane_id: &str) -> bool {
        self.process.pane_bootstrap_pending.contains(pane_id)
    }

    /// Reports whether a pending bootstrap still needs current-epoch shell
    /// identity evidence before commands can safely enter a child shell.
    pub(crate) fn pane_bootstrap_awaits_shell_identity(&self, pane_id: &str) -> bool {
        self.process.pane_bootstrap_pending.contains(pane_id)
            && self
                .process
                .pane_shell_interaction_generations
                .contains_key(pane_id)
            && !self
                .process
                .pane_certified_shell_identities
                .contains_key(pane_id)
            && !self
                .process
                .pane_probed_shell_identities
                .contains_key(pane_id)
    }

    /// Clears pane readiness states and manual overrides for session replacement.
    pub(crate) fn clear_pane_readiness_state_and_overrides(&mut self) {
        self.process.pane_readiness_states.clear();
        self.process.pane_readiness_overrides = Default::default();
    }

    /// Records the best-known current working directory for one pane process.
    pub(crate) fn set_pane_current_working_directory(
        &mut self,
        pane_id: impl Into<String>,
        path: PathBuf,
    ) {
        self.process
            .pane_current_working_directories
            .insert(pane_id.into(), path);
    }

    /// Removes one pane's best-known working directory during rollback.
    pub(crate) fn remove_pane_current_working_directory(&mut self, pane_id: &str) {
        self.process
            .pane_current_working_directories
            .remove(pane_id);
    }

    /// Terminates every process still owned by the runtime.
    pub(crate) fn terminate_all_pane_processes(&mut self) -> Result<Vec<ExitedPaneProcess>> {
        Ok(self.process.pane_processes.terminate_all()?)
    }

    /// Returns the current output-activity sequence for one pane process.
    pub(crate) fn pane_process_output_activity_sequence(&self, pane_id: &str) -> Option<u64> {
        self.process
            .pane_processes
            .output_activity_sequence(pane_id)
    }

    /// Waits for a pane process to publish output after a known sequence.
    pub(crate) fn wait_for_pane_process_output_activity_after(
        &self,
        pane_id: &str,
        sequence: u64,
        timeout: std::time::Duration,
    ) -> Option<bool> {
        self.process
            .pane_processes
            .wait_for_output_activity_after(pane_id, sequence, timeout)
    }

    /// Returns the executable name observed for one live pane process.
    pub(crate) fn pane_process_name(&self, pane_id: &str) -> Option<String> {
        self.process.pane_processes.process_name(pane_id)
    }

    /// Returns pane ids currently tracked by the live process manager.
    pub(crate) fn tracked_runtime_pane_process_ids(&self) -> Vec<String> {
        self.process.pane_processes.tracked_pane_ids()
    }

    /// Clears visible and hidden shell transaction parser state on shutdown.
    pub(crate) fn clear_pane_transaction_parsers(&mut self) {
        self.process.pane_transaction_osc_screens.clear();
        self.process.pane_shell_output_render_pending.clear();
    }

    /// Clears pane exit and closing markers when the live session is replaced.
    pub(crate) fn clear_pane_process_lifecycle_tracking(&mut self) {
        self.process.pane_exit_records.clear();
        self.process.pane_closing.clear();
    }

    /// Returns the last observed exit status for a pane process.
    pub(crate) fn pane_exit_status(&self, pane_id: &str) -> Option<PaneExitStatus> {
        self.process
            .pane_exit_records
            .get(pane_id)
            .map(|record| record.exit_status)
    }

    /// Marks a pane as being in process teardown.
    pub(crate) fn mark_pane_closing(&mut self, pane_id: impl Into<String>) {
        self.process.pane_closing.insert(pane_id.into());
    }

    /// Reports whether a pane is already in process teardown.
    pub(crate) fn pane_is_closing(&self, pane_id: &str) -> bool {
        self.process.pane_closing.contains(pane_id)
    }
}

#[cfg(test)]
impl RuntimeSessionService {
    /// Installs one pane environment signature for path-resolution tests.
    pub(crate) fn set_pane_environment_signature_for_tests(
        &mut self,
        pane_id: impl Into<String>,
        signature: EnvironmentSignature,
    ) {
        let pane_id = pane_id.into();
        self.process.pane_bootstrap_pending.remove(&pane_id);
        self.process
            .pane_environment_authority_failures
            .remove(&pane_id);
        self.process
            .pane_environment_signatures
            .insert(pane_id, signature);
    }

    /// Installs one certification rejection for lifecycle and preflight tests.
    pub(crate) fn set_pane_agent_subshell_certification_rejection_for_tests(
        &mut self,
        pane_id: impl Into<String>,
        rejection: RuntimeAgentSubshellCertificationRejection,
    ) {
        self.process
            .pane_agent_subshell_certification_rejections
            .insert(pane_id.into(), rejection);
    }

    /// Advances one pane's interaction generation without clearing its
    /// certified identity so tests can prove stale epoch evidence fails closed.
    pub(crate) fn advance_pane_shell_interaction_generation_for_tests(&mut self, pane_id: &str) {
        self.process.next_shell_interaction_generation = self
            .process
            .next_shell_interaction_generation
            .saturating_add(1);
        self.process.pane_shell_interaction_generations.insert(
            pane_id.to_string(),
            self.process.next_shell_interaction_generation,
        );
    }

    /// Returns live shell transactions for integration-test observation.
    pub(crate) fn running_shell_transactions_for_tests(
        &self,
    ) -> &std::collections::BTreeMap<String, RunningShellTransactionRef> {
        &self.process.running_shell_transactions
    }

    /// Returns live shell transactions for process-fixture mutation.
    pub(crate) fn running_shell_transactions_mut_for_tests(
        &mut self,
    ) -> &mut std::collections::BTreeMap<String, RunningShellTransactionRef> {
        &mut self.process.running_shell_transactions
    }

    /// Reports whether one live agent-action transaction uses Bubblewrap.
    pub(crate) fn shell_transaction_is_sandboxed_for_tests(&self, marker: &str) -> bool {
        self.process
            .sandboxed_shell_transaction_markers
            .contains(marker)
    }

    /// Injects one failure while sending Ctrl-C to a pane shell.
    pub(crate) fn fail_next_pane_interrupt_write_for_tests(&mut self) {
        self.process.fail_next_pane_interrupt_write = true;
    }

    /// Reports whether a transaction still requires a start marker.
    pub(crate) fn shell_transaction_requires_start_marker_for_tests(&self, marker: &str) -> bool {
        self.process
            .shell_transaction_require_start_markers
            .contains(marker)
    }

    /// Reports whether a transaction start marker has been observed.
    pub(crate) fn shell_transaction_started_for_tests(&self, marker: &str) -> bool {
        self.process
            .shell_transaction_started_markers
            .contains(marker)
    }

    /// Installs the remaining private-receiver acknowledgement count for a test transaction.
    pub(crate) fn set_shell_transaction_receiver_acknowledgements_for_tests(
        &mut self,
        marker: &str,
        remaining: usize,
    ) {
        self.process
            .shell_transaction_receiver_acknowledgements
            .insert(marker.to_string(), remaining);
    }

    /// Installs a manual readiness override for a test epoch.
    pub(crate) fn mark_pane_readiness_override_for_tests(
        &mut self,
        pane_id: &str,
        epoch: u64,
        reason: &str,
        one_shot: bool,
    ) -> Result<()> {
        self.process
            .pane_readiness_overrides
            .mark_ready_for_epoch(pane_id, epoch, reason, one_shot)?;
        Ok(())
    }

    /// Reports whether a manual readiness override allows a test epoch.
    pub(crate) fn pane_readiness_override_allows_epoch_for_tests(
        &self,
        pane_id: &str,
        epoch: u64,
    ) -> bool {
        self.process
            .pane_readiness_overrides
            .allows_epoch(pane_id, epoch)
    }

    /// Reports whether bootstrap remains pending for a process fixture.
    pub(crate) fn pane_bootstrap_is_pending_for_tests(&self, pane_id: &str) -> bool {
        self.pane_bootstrap_is_pending(pane_id)
    }
    /// Returns the process manager for integration-test observation.
    pub(crate) fn pane_processes(&self) -> &PaneProcessManager {
        &self.process.pane_processes
    }

    /// Returns the process manager for test-only process-fixture mutation.
    pub(crate) fn pane_processes_mut(&mut self) -> &mut PaneProcessManager {
        &mut self.process.pane_processes
    }

    /// Returns mutable visible transaction parsers for a process fixture.
    pub(crate) fn pane_transaction_osc_screens_mut_for_tests(
        &mut self,
    ) -> &mut std::collections::BTreeMap<String, TerminalScreen> {
        &mut self.process.pane_transaction_osc_screens
    }

    /// Returns visible transaction parsers for process integration tests.
    pub(crate) fn pane_transaction_osc_screens_for_tests(
        &self,
    ) -> &std::collections::BTreeMap<String, TerminalScreen> {
        &self.process.pane_transaction_osc_screens
    }

    /// Requires the next test pane write to observe registered transaction ownership.
    pub(crate) fn require_registered_transaction_on_next_write_for_tests(&mut self) {
        self.process.require_registered_transaction_on_next_write = true;
    }

    /// Installs one pane exit status for presentation integration tests.
    pub(crate) fn set_pane_exit_status_for_tests(
        &mut self,
        pane_id: impl Into<String>,
        exit_status: PaneExitStatus,
    ) {
        self.process
            .pane_exit_records
            .insert(pane_id.into(), PaneExitRecord { exit_status });
    }
}

// Pane process lifecycle and PTY synchronization.

impl RuntimeSessionService {
    /// Returns one validated pane shell identity for transaction construction.
    ///
    /// Certified child shells must still match the live primary process,
    /// interaction generation, and published environment signature. Any stale
    /// or contradictory proof fails closed instead of falling back to the
    /// session-global executable with a pane-local dialect.
    pub(crate) fn shell_execution_identity_for_pane(
        &self,
        pane_id: &str,
    ) -> Result<RuntimePaneShellExecutionIdentity> {
        let primary_process_id = self.primary_pid_for_live_pane_process(pane_id);
        if let Some(certified) = self.process.pane_certified_shell_identities.get(pane_id) {
            let interaction_generation = self
                .process
                .pane_shell_interaction_generations
                .get(pane_id)
                .copied();
            let published = self.process.pane_environment_signatures.get(pane_id);
            if primary_process_id != Some(certified.primary_process_id)
                || interaction_generation != Some(certified.interaction_generation)
                || published != Some(&certified.environment_signature)
            {
                return Err(MezError::invalid_state(
                    "certified pane shell identity is stale for the current process or interaction epoch",
                ));
            }
            return runtime_shell_execution_identity_from_signature(
                &certified.environment_signature,
                primary_process_id,
                interaction_generation,
            );
        }

        if let Some(probed) = self.process.pane_probed_shell_identities.get(pane_id) {
            let interaction_generation = self
                .process
                .pane_shell_interaction_generations
                .get(pane_id)
                .copied();
            if primary_process_id != Some(probed.primary_process_id)
                || interaction_generation != Some(probed.interaction_generation)
            {
                return Err(MezError::invalid_state(
                    "probed pane shell identity is stale for the current process or interaction epoch",
                ));
            }
            return Ok(probed.execution_identity.clone());
        }

        if let Some(signature) = self.process.pane_environment_signatures.get(pane_id) {
            if self
                .process
                .pane_shell_interaction_generations
                .contains_key(pane_id)
            {
                return Err(MezError::invalid_state(
                    "pane shell environment is not certified for the current interaction epoch",
                ));
            }
            return runtime_shell_execution_identity_from_signature(
                signature,
                primary_process_id,
                None,
            );
        }

        if self
            .process
            .pane_shell_interaction_generations
            .contains_key(pane_id)
        {
            return Err(MezError::invalid_state(
                "pane shell identity has not been probed for the current interaction epoch",
            ));
        }

        let shell_path = self.session.shell.path().to_path_buf();
        mez_agent::validate_resolved_shell_path(&shell_path)
            .map_err(|error| MezError::invalid_state(error.message()))?;
        let version_probe = self.session.shell.version_probe().map(ToOwned::to_owned);
        let classification =
            ShellClassification::classify_with_probe(&shell_path, version_probe.as_deref());
        Ok(RuntimePaneShellExecutionIdentity {
            shell_path,
            classification,
            version_probe,
            primary_process_id,
            interaction_generation: None,
        })
    }

    /// Runs the shell classification for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn shell_classification_for_pane(&self, pane_id: &str) -> ShellClassification {
        self.shell_execution_identity_for_pane(pane_id)
            .map(|identity| identity.classification())
            .unwrap_or_else(|_| {
                ShellClassification::classify_with_probe(
                    self.session.shell.path(),
                    self.session.shell.version_probe(),
                )
            })
    }

    /// Adds pane-scoped shell compatibility state to one transaction.
    ///
    /// Managed Bash and zsh panes receive their startup-authenticated tokens.
    /// Other classifications retain their existing renderer behavior.
    pub(super) fn configure_shell_transaction_for_pane(
        &self,
        pane_id: &str,
        transaction: ShellTransaction,
    ) -> ShellTransaction {
        let transaction = self
            .process
            .pane_zsh_compatibility
            .get(pane_id)
            .map_or(transaction.clone(), |compatibility| {
                transaction.with_zsh_history_token(compatibility.token().clone())
            });
        let transaction = self
            .process
            .pane_bash_compatibility
            .get(pane_id)
            .map_or(transaction.clone(), |compatibility| {
                transaction.with_bash_receiver_token(compatibility.token().clone())
            });
        transaction.with_payload_receiver_acknowledgements(cfg!(target_os = "macos"))
    }

    /// Rejects generated shell input when a required managed transport is absent.
    pub(super) fn require_generated_shell_input(
        &self,
        input: &mez_agent::ShellTransactionInput,
    ) -> Result<()> {
        if input.is_empty() {
            return Err(MezError::invalid_state(
                "managed Bash private receiver is unavailable for generated shell input",
            ));
        }
        Ok(())
    }

    /// Returns the managed zsh history token installed for one pane.
    pub(super) fn zsh_history_token_for_pane(
        &self,
        pane_id: &str,
    ) -> Option<&mez_agent::MarkerToken> {
        self.process
            .pane_zsh_compatibility
            .get(pane_id)
            .map(zsh_compat::ManagedZshCompatibility::token)
    }

    /// Returns the managed Bash receiver rcfile for one pane when installed.
    pub(super) fn bash_receiver_rcfile_for_pane(&self, pane_id: &str) -> Option<&Path> {
        self.process
            .pane_bash_compatibility
            .get(pane_id)
            .map(bash_compat::ManagedBashCompatibility::rcfile)
    }

    /// Returns the pane-scoped token authenticating private Bash receiver events.
    pub(super) fn bash_receiver_token_for_pane(
        &self,
        pane_id: &str,
    ) -> Option<&mez_agent::MarkerToken> {
        self.process
            .pane_bash_compatibility
            .get(pane_id)
            .map(bash_compat::ManagedBashCompatibility::token)
    }

    /// Runs the poll pane processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn poll_pane_processes(&mut self) -> Result<Vec<PaneExitUpdate>> {
        self.require_live()?;
        let exited = self.process.pane_processes.poll_exited()?;
        let mut updates = Vec::new();

        for process in exited {
            updates.push(self.apply_exited_pane_process(process, true)?);
        }

        Ok(updates)
    }

    /// Applies one pane process exit event delivered by an async process watcher.
    ///
    /// The live polling path still removes recorded exits from
    /// `PaneProcessManager`; event-driven callers may not keep ownership there,
    /// so this method applies the session, event-log, registry, pane-pipe, and
    /// agent-turn cleanup without assuming the synchronous manager observed the
    /// exit first.
    pub fn apply_pane_process_exit_event(
        &mut self,
        pane_id: impl Into<String>,
        primary_pid: u32,
        exit_status: PaneExitStatus,
    ) -> Result<Option<PaneExitUpdate>> {
        self.require_live()?;
        let pane_id = pane_id.into();
        if self.find_pane_descriptor(&pane_id).is_none() {
            self.process.detached_pane_processes.remove(&pane_id);
            return Ok(None);
        }
        let live_primary_pid = self.primary_pid_for_live_pane_process(&pane_id);
        if primary_pid != 0
            && let Some(live_primary_pid) = live_primary_pid
            && live_primary_pid != primary_pid
        {
            return Ok(None);
        }
        let primary_pid = if primary_pid == 0 {
            live_primary_pid.unwrap_or(0)
        } else {
            primary_pid
        };
        self.process.detached_pane_processes.remove(&pane_id);
        self.apply_exited_pane_process(
            ExitedPaneProcess {
                pane_id: pane_id.clone(),
                primary_pid,
                status: exit_status,
            },
            false,
        )
        .map(Some)
    }

    /// Applies one process lifecycle failure delivered by an async watcher.
    ///
    /// This records a diagnostic pane event without closing the pane. The
    /// process may still be live when a watcher, wait task, resize operation, or
    /// write bridge fails, so lifecycle mutation remains the responsibility of a
    /// later explicit exit or termination event.
    pub fn apply_pane_process_failure_event(
        &mut self,
        pane_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        let error = error.into();
        let primary_pid = self
            .primary_pid_for_live_pane_process(descriptor.pane_id.as_str())
            .unwrap_or(0);
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"failed","error":"{}"}}"#,
                json_escape(descriptor.pane_id.as_str()),
                json_escape(descriptor.window_id.as_str()),
                primary_pid,
                json_escape(&error)
            ),
        )?;
        Ok(true)
    }

    /// Applies one process-spawn lifecycle event delivered by an async watcher.
    ///
    /// This is the event-driven equivalent of the post-spawn bookkeeping in
    /// `start_pane_process_with_start_directory`: it refreshes the pane's
    /// terminal screen state, marks readiness as unknown, queues bootstrap
    /// observation, and emits the replayable pane-start lifecycle event. The
    /// async process owner is responsible for retaining the live process handle.
    pub fn apply_pane_process_spawn_event(
        &mut self,
        pane_id: impl Into<String>,
        pid: Option<u32>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        let primary_pid = pid
            .or_else(|| {
                self.process
                    .pane_processes
                    .primary_pid(descriptor.pane_id.as_str())
            })
            .unwrap_or(0);
        self.process
            .pane_exit_records
            .remove(descriptor.pane_id.as_str());
        self.session
            .set_pane_live_state(descriptor.pane_id.as_str(), true)?;
        self.process.process_pane_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?,
        );
        self.process.pane_transaction_osc_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?,
        );
        self.process
            .pane_readiness_states
            .insert(descriptor.pane_id.to_string(), PaneReadinessState::Unknown);
        self.process
            .pane_bootstrap_pending
            .insert(descriptor.pane_id.to_string());

        let update = PaneProcessStart {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid,
            size: descriptor.size,
            registry_update: self.registry_update_plan(),
        };
        self.append_pane_start_event(&update)?;
        Ok(true)
    }

    /// Applies one process lifecycle event through the transport-neutral transition contract.
    pub(crate) fn apply_process_transition(
        &mut self,
        event: ProcessEvent,
    ) -> Result<RuntimeTransition> {
        let (applied, render_reason) = match event {
            ProcessEvent::Exited {
                pane_id,
                primary_pid,
                exit_code,
                signal,
            } => {
                let primary_pid = primary_pid
                    .or_else(|| self.process.pane_processes.primary_pid(&pane_id))
                    .unwrap_or(0);
                let signal_number = signal
                    .as_deref()
                    .and_then(|signal| signal.parse::<i32>().ok());
                let status = PaneExitStatus {
                    code: exit_code,
                    signal: signal_number,
                    success: exit_code == Some(0) && signal.is_none(),
                };
                (
                    self.apply_pane_process_exit_event(pane_id, primary_pid, status)?
                        .is_some(),
                    Some(RenderInvalidationReason::Layout),
                )
            }
            ProcessEvent::Failed { pane_id, error } => (
                self.apply_pane_process_failure_event(pane_id, error)?,
                Some(RenderInvalidationReason::FullRedraw),
            ),
            ProcessEvent::Spawned { pane_id, pid } => (
                self.apply_pane_process_spawn_event(pane_id, pid)?,
                Some(RenderInvalidationReason::FullRedraw),
            ),
        };
        let mut transition = self.runtime_transition_with_render(applied, render_reason);
        if applied {
            transition
                .side_effects
                .extend(self.registry_persistence_transition().side_effects);
        }
        Ok(transition)
    }

    /// Applies one non-output pane event through the transport-neutral transition contract.
    ///
    /// Pane output remains actor-owned temporarily because it also updates ingress metrics and
    /// pane-pipe health timers. Completion events can already return their ordered render effects
    /// without depending on Tokio or transport state.
    pub(crate) fn apply_pane_completion_transition(
        &mut self,
        event: PaneEvent,
    ) -> Result<RuntimeTransition> {
        let (applied, render_reason) = match event {
            PaneEvent::WriteFailed { pane_id, error } => (
                self.apply_pane_write_failure_event(pane_id, error)?,
                Some(RenderInvalidationReason::FullRedraw),
            ),
            PaneEvent::Resized { pane_id, size } => (
                self.apply_pane_resize_completion_event(pane_id, size)?,
                Some(RenderInvalidationReason::Layout),
            ),
            PaneEvent::ForegroundProcess {
                pane_id,
                process_name,
                process_group_id,
                current_working_directory,
            } => (
                self.apply_pane_foreground_process_event(
                    pane_id,
                    process_name,
                    process_group_id,
                    current_working_directory,
                )?,
                Some(RenderInvalidationReason::PaneOutput),
            ),
            PaneEvent::InputWritten { pane_id, bytes } => {
                (self.apply_pane_input_written_event(pane_id, bytes)?, None)
            }
            PaneEvent::Output { .. } => {
                return Err(MezError::invalid_state(
                    "pane output must use the output transition path",
                ));
            }
        };
        Ok(self.runtime_transition_with_render(applied, render_reason))
    }

    /// Builds a transition with one render invalidation for every attached client.
    pub(crate) fn runtime_transition_with_render(
        &self,
        applied: bool,
        render_reason: Option<RenderInvalidationReason>,
    ) -> RuntimeTransition {
        let side_effects = if applied {
            render_reason
                .into_iter()
                .flat_map(|reason| {
                    self.session
                        .clients()
                        .iter()
                        .filter(|client| client.state == mez_mux::session::ClientState::Attached)
                        .map(move |client| RuntimeSideEffect::RenderClient {
                            client_id: client.id.clone(),
                            reason,
                        })
                })
                .collect()
        } else {
            Vec::new()
        };
        RuntimeTransition {
            applied,
            side_effects,
        }
    }

    /// Applies one pane input write failure delivered by an async pane driver.
    pub fn apply_pane_write_failure_event(
        &mut self,
        pane_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        let error = error.into();
        let pane_id = descriptor.pane_id.to_string();
        let window_id = descriptor.window_id.to_string();
        let primary_pid = self
            .primary_pid_for_live_pane_process(pane_id.as_str())
            .unwrap_or(0);
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"pane_io":"write_failed","error":"{}"}}"#,
                json_escape(&pane_id),
                json_escape(&window_id),
                primary_pid,
                json_escape(&error)
            ),
        )?;
        self.fail_shell_transactions_for_pane_write_failure(&pane_id, &error)?;
        Ok(true)
    }

    /// Applies one pane input write completion delivered by an async pane driver.
    pub fn apply_pane_input_written_event(
        &mut self,
        pane_id: impl Into<String>,
        bytes: usize,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        if self.find_pane_descriptor(&pane_id).is_none() {
            return Ok(false);
        }
        let active_transactions = self
            .process
            .running_shell_transactions
            .iter()
            .filter(|(_, transaction)| transaction.pane_id == pane_id)
            .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
            .collect::<Vec<_>>();
        for (marker, transaction) in &active_transactions {
            let action_fragment = match &transaction.kind {
                RunningShellTransactionKind::AgentAction { action_id } => {
                    format!(" action={action_id}")
                }
                RunningShellTransactionKind::FocusedShellHook
                | RunningShellTransactionKind::ReadinessProbe
                | RunningShellTransactionKind::Bootstrap
                | RunningShellTransactionKind::ShellIdentityProbe { .. }
                | RunningShellTransactionKind::PathResolution { .. }
                | RunningShellTransactionKind::EnvironmentEvidence { .. }
                | RunningShellTransactionKind::BubblewrapCapabilityProbe { .. } => String::new(),
            };
            self.append_agent_trace_turn_event(
                &pane_id,
                &transaction.turn_id,
                &format!(
                    "pane_input written bytes={} marker={} kind={}{}",
                    bytes,
                    marker,
                    runtime_running_shell_transaction_kind_name(&transaction.kind),
                    action_fragment
                ),
            )?;
        }
        Ok(!active_transactions.is_empty())
    }

    /// Applies one PTY resize completion delivered by an async pane driver.
    pub fn apply_pane_resize_completion_event(
        &mut self,
        pane_id: impl Into<String>,
        size: Size,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        if !self
            .persistence
            .accept_pane_resize_completion(&pane_id, size)
        {
            return Ok(false);
        }
        if !self.rebuild_agent_presentation_after_resize(&pane_id, size)?
            && let Some(screen) = self
                .process
                .process_pane_screens
                .get_mut(descriptor.pane_id.as_str())
        {
            screen.resize(size);
        }
        if let Some(screen) = self
            .process
            .pane_transaction_osc_screens
            .get_mut(descriptor.pane_id.as_str())
        {
            screen.resize(size);
        }
        let primary_pid = self
            .process
            .pane_processes
            .primary_pid(descriptor.pane_id.as_str())
            .unwrap_or(0);
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"pty_resize":"applied","columns":{},"rows":{}}}"#,
                json_escape(descriptor.pane_id.as_str()),
                json_escape(descriptor.window_id.as_str()),
                primary_pid,
                size.columns,
                size.rows
            ),
        )?;
        Ok(true)
    }

    /// Runs the apply exited pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_exited_pane_process(
        &mut self,
        process: ExitedPaneProcess,
        remove_recorded_process: bool,
    ) -> Result<PaneExitUpdate> {
        let descriptor = self.find_pane_descriptor(&process.pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "exited pane process has no matching pane",
            )
        })?;
        let previous_window_count = self.session.windows().len();

        let _ = self.stop_active_pane_pipe(process.pane_id.as_str());
        self.process
            .pane_current_working_directories
            .remove(process.pane_id.as_str());
        self.fail_agent_turns_for_pane_shutdown(
            std::slice::from_ref(&process.pane_id),
            "pane primary process exited",
        )?;
        self.process.pane_exit_records.insert(
            process.pane_id.clone(),
            PaneExitRecord {
                exit_status: process.status,
            },
        );
        let transition = self
            .session
            .close_exited_pane_with_effects(descriptor.pane_id.as_str())?;
        self.sync_pane_resize_effects(&transition.effects)?;
        if remove_recorded_process {
            self.process
                .pane_processes
                .remove_exited(&process.pane_id)?;
        }
        self.session
            .set_lifecycle_state(RuntimeLifecycleState::from_session_state(
                self.session.state,
            ));

        let closed_window = self.session.windows().len() < previous_window_count;
        let update = PaneExitUpdate {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid: process.primary_pid,
            exit_status: process.status,
            closed_window,
            session_empty: self.session.windows().is_empty(),
            registry_update: self.registry_update_plan(),
        };
        self.append_pane_exit_event(&update)?;
        if closed_window {
            self.append_lifecycle_event(
                EventKind::WindowChanged,
                format!(
                    r#"{{"window_id":"{}","state":"closed","session_empty":{}}}"#,
                    json_escape(&update.window_id),
                    update.session_empty
                ),
            )?;
        }
        self.persist_or_defer_registry_update_plan(update.registry_update.clone())?;
        Ok(update)
    }

    /// Applies pane output through the transport-neutral transition contract.
    pub(crate) fn apply_pane_output_transition(
        &mut self,
        pane_id: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<RuntimeTransition> {
        let update = self.apply_pane_output_bytes(pane_id, bytes)?;
        let applied = update.is_some();
        let render_reason = update.map(|update| {
            if update.invalidate_output_frame {
                RenderInvalidationReason::FullRedraw
            } else {
                RenderInvalidationReason::PaneOutput
            }
        });
        Ok(self.runtime_transition_with_render(applied, render_reason))
    }

    /// Runs the poll pane outputs operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn poll_pane_outputs(
        &mut self,
        max_bytes_per_pane: usize,
    ) -> Result<Vec<PaneOutputUpdate>> {
        self.require_live()?;
        let outputs = self
            .process
            .pane_processes
            .read_available_output(max_bytes_per_pane)?;
        let mut updates = Vec::new();
        let mut terminal_title_panes = BTreeSet::new();

        for output in outputs {
            if self.find_pane_descriptor(&output.pane_id).is_none() {
                continue;
            }
            updates.push(self.apply_pane_process_output(output, &mut terminal_title_panes)?);
        }

        if self.should_sync_pane_titles_from_foreground_processes(!updates.is_empty()) {
            self.sync_pane_titles_from_foreground_processes(&terminal_title_panes)?;
        }

        Ok(updates)
    }

    /// Applies one pane-output event delivered by an async pane driver.
    ///
    /// This is the event-driven equivalent of one item returned by
    /// `PaneProcessManager::read_available_output`. It preserves the same
    /// filtering, OSC observation, screen feeding, shell transaction tracking,
    /// pane-pipe forwarding, title syncing, and event-log behavior used by the
    /// synchronous polling path.
    pub fn apply_pane_output_bytes(
        &mut self,
        pane_id: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<Option<PaneOutputUpdate>> {
        self.require_live()?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let pane_id = pane_id.into();
        if self.find_pane_descriptor(&pane_id).is_none() {
            return Ok(None);
        }
        let primary_pid = self
            .primary_pid_for_live_pane_process(&pane_id)
            .unwrap_or(0);
        let mut terminal_title_panes = BTreeSet::new();
        let update = self.apply_pane_process_output(
            PaneProcessOutput {
                pane_id,
                primary_pid,
                bytes,
            },
            &mut terminal_title_panes,
        )?;
        if self.should_sync_pane_titles_from_foreground_processes(true) {
            self.sync_pane_titles_from_foreground_processes(&terminal_title_panes)?;
        }
        Ok(Some(update))
    }

    /// Runs the start pane process with start directory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_pane_process_with_start_directory(
        &mut self,
        descriptor: PaneDescriptor,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        let environment = pane_environment_with_term(
            self.session.socket_path(),
            &self.session.id,
            &descriptor.window_id,
            &descriptor.pane_id,
            &self.process.settings.terminal_term,
        )?;
        let classification = ShellClassification::classify_with_probe(
            self.session.shell.path(),
            self.session.shell.version_probe(),
        );
        let fish_compatibility =
            if explicit_command.is_none() && classification == ShellClassification::Fish {
                let owner = runtime_random_marker_token(&format!(
                    "fish-integration\0{}\0{}",
                    self.session.id, descriptor.pane_id
                ))?;
                Some(fish_compat::ManagedFishCompatibility::new(owner))
            } else {
                None
            };
        let mut bash_compatibility_diagnostic = None;
        let bash_compatibility =
            if explicit_command.is_none() && classification == ShellClassification::Bash {
                let token = runtime_random_marker_token(&format!(
                    "bash-receiver\0{}\0{}",
                    self.session.id, descriptor.pane_id
                ))?;
                match bash_compat::ManagedBashCompatibility::create(
                    self.session.socket_path(),
                    descriptor.pane_id.as_str(),
                    token,
                ) {
                    Ok(compatibility) => Some(compatibility),
                    Err(error) => {
                        bash_compatibility_diagnostic = Some(error.to_string());
                        None
                    }
                }
            } else {
                None
            };
        let mut zsh_compatibility_diagnostic = None;
        let zsh_compatibility =
            if explicit_command.is_none() && classification == ShellClassification::Zsh {
                let token = runtime_random_marker_token(&format!(
                    "zsh-history\0{}\0{}",
                    self.session.id, descriptor.pane_id
                ))?;
                match zsh_compat::ManagedZshCompatibility::create(
                    self.session.socket_path(),
                    descriptor.pane_id.as_str(),
                    token,
                    std::env::var_os("ZDOTDIR"),
                ) {
                    Ok(compatibility) => Some(compatibility),
                    Err(error) => {
                        zsh_compatibility_diagnostic = Some(error.to_string());
                        None
                    }
                }
            } else {
                None
            };
        let mut launch =
            mez_mux::process::PaneProcessLaunch::new(self.session.shell.path().to_path_buf());
        if let Some(compatibility) = fish_compatibility.as_ref() {
            launch = compatibility.configure_launch(launch);
        }
        if let Some(compatibility) = bash_compatibility.as_ref() {
            launch = compatibility.configure_launch(launch);
        }
        if let Some(compatibility) = zsh_compatibility.as_ref() {
            launch = compatibility.configure_launch(launch);
        }
        let primary_pid = self
            .process
            .pane_processes
            .spawn_for_pane_with_start_directory(
                descriptor.pane_id.as_str(),
                &launch,
                explicit_command,
                &environment,
                descriptor.size,
                start_directory,
            )?;
        if let Some(compatibility) = fish_compatibility {
            self.process
                .pane_fish_compatibility
                .insert(descriptor.pane_id.to_string(), compatibility);
        }
        if let Some(compatibility) = bash_compatibility {
            self.process
                .pane_bash_compatibility
                .insert(descriptor.pane_id.to_string(), compatibility);
        }
        if let Some(compatibility) = zsh_compatibility {
            self.process
                .pane_zsh_compatibility
                .insert(descriptor.pane_id.to_string(), compatibility);
        }
        if let Some(error) = bash_compatibility_diagnostic {
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","diagnostic":"Bash private receiver unavailable; starting without managed compatibility","error":"{}"}}"#,
                    json_escape(descriptor.pane_id.as_str()),
                    json_escape(&error)
                ),
            )?;
        }
        if let Some(error) = zsh_compatibility_diagnostic {
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","diagnostic":"zsh history isolation unavailable; starting without managed compatibility","error":"{}"}}"#,
                    json_escape(descriptor.pane_id.as_str()),
                    json_escape(&error)
                ),
            )?;
        }
        self.process
            .pane_exit_records
            .remove(descriptor.pane_id.as_str());
        self.process.process_pane_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?,
        );
        self.process.pane_transaction_osc_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?,
        );
        self.process
            .pane_readiness_states
            .insert(descriptor.pane_id.to_string(), PaneReadinessState::Unknown);
        self.process
            .pane_bootstrap_pending
            .insert(descriptor.pane_id.to_string());
        if let Some(start_directory) = start_directory {
            self.process.pane_current_working_directories.insert(
                descriptor.pane_id.to_string(),
                start_directory.to_path_buf(),
            );
        }

        if self.session.shell.used_fallback() {
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","diagnostic":"resolved shell fell back to /bin/sh"}}"#,
                    json_escape(descriptor.pane_id.as_str())
                ),
            )?;
        }

        let update = PaneProcessStart {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid,
            size: descriptor.size,
            registry_update: self.registry_update_plan(),
        };
        self.append_pane_start_event(&update)?;
        Ok(update)
    }

    /// Removes a live pane process from synchronous manager ownership for an
    /// external pane process adapter.
    ///
    /// The session, screen, readiness, and lifecycle metadata stay in the
    /// runtime service; only PTY/process I/O ownership moves. Callers must start
    /// a replacement external adapter before routing user input away from the
    /// compatibility manager path.
    pub fn take_running_pane_process_for_adapter(&mut self, pane_id: &str) -> Result<PaneProcess> {
        self.require_live()?;
        let primary_pid = self
            .process
            .pane_processes
            .primary_pid(pane_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane process not found",
                )
            })?;
        if let Some(current_working_directory) = self
            .process
            .pane_processes
            .current_working_directory(pane_id)
        {
            self.process
                .pane_current_working_directories
                .insert(pane_id.to_string(), current_working_directory);
        }
        let process = self
            .process
            .pane_processes
            .take_running_pane_process(pane_id)?;
        self.process.next_detached_pane_generation = self
            .process
            .next_detached_pane_generation
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("pane process generation exhausted"))?;
        self.process.detached_pane_processes.insert(
            pane_id.to_string(),
            DetachedPaneProcess {
                primary_pid,
                generation: self.process.next_detached_pane_generation,
            },
        );
        Ok(process)
    }

    /// Removes up to `limit` running pane processes for pane I/O adapters.
    ///
    /// This is the dynamic production handoff entry point used by the async
    /// pane-process supervisor. Pane state remains in the runtime service while
    /// process, PTY output, input, resize, and termination ownership moves to
    /// one external adapter per returned process.
    pub fn take_running_pane_process_instances_for_adapter(
        &mut self,
        limit: usize,
    ) -> Result<Vec<(PaneProcessInstance, PaneProcess)>> {
        self.require_live()?;
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async pane process handoff limit must be greater than zero",
            ));
        }
        let pane_ids = self
            .process
            .pane_processes
            .tracked_running_pane_ids()
            .into_iter()
            .take(limit)
            .collect::<Vec<_>>();
        let mut processes = Vec::with_capacity(pane_ids.len());
        for pane_id in pane_ids {
            let process = self.take_running_pane_process_for_adapter(&pane_id)?;
            let generation = self
                .process
                .detached_pane_processes
                .get(&pane_id)
                .map(|detached| detached.generation)
                .ok_or_else(|| {
                    MezError::invalid_state("adapter-owned pane process identity was not recorded")
                })?;
            processes.push((
                PaneProcessInstance {
                    pane_id,
                    generation,
                },
                process,
            ));
        }
        Ok(processes)
    }

    /// Removes running pane processes while retaining the legacy pane-id-only
    /// handoff shape used by synchronous compatibility callers and fixtures.
    #[cfg(test)]
    pub fn take_running_pane_processes_for_adapter(
        &mut self,
        limit: usize,
    ) -> Result<Vec<(String, PaneProcess)>> {
        self.take_running_pane_process_instances_for_adapter(limit)
            .map(|processes| {
                processes
                    .into_iter()
                    .map(|(instance, process)| (instance.pane_id, process))
                    .collect()
            })
    }

    /// Restores a pane process to synchronous manager ownership after a
    /// cancelled external adapter handoff.
    #[cfg(test)]
    pub fn restore_running_pane_process_from_adapter(
        &mut self,
        pane_id: impl Into<String>,
        process: PaneProcess,
    ) -> Result<u32> {
        self.require_live()?;
        let pane_id = pane_id.into();
        self.process.detached_pane_processes.remove(&pane_id);
        Ok(self
            .process
            .pane_processes
            .insert_running_pane_process(pane_id, process)?)
    }

    /// Drains pane-worker I/O through the transport-neutral transition contract.
    pub(crate) fn drain_pane_io_transition(&mut self) -> RuntimeTransition {
        let side_effects = self.persistence.take_pane_io_effects();
        RuntimeTransition {
            applied: false,
            side_effects,
        }
    }

    /// Returns true when a pane's PTY/process handle is owned by an external adapter.
    pub fn pane_process_is_adapter_owned(&self, pane_id: &str) -> bool {
        self.process.detached_pane_processes.contains_key(pane_id)
    }

    /// Returns whether an adapter event belongs to the currently owned process instance.
    pub(crate) fn pane_process_instance_is_current(&self, instance: &PaneProcessInstance) -> bool {
        self.find_pane_descriptor(&instance.pane_id).is_some()
            && self
                .process
                .detached_pane_processes
                .get(&instance.pane_id)
                .is_some_and(|process| process.generation == instance.generation)
    }

    /// Returns the current adapter-owned process identity for one pane.
    pub(crate) fn adapter_owned_pane_process_instance(
        &self,
        pane_id: &str,
    ) -> Option<PaneProcessInstance> {
        self.process
            .detached_pane_processes
            .get(pane_id)
            .map(|process| PaneProcessInstance {
                pane_id: pane_id.to_string(),
                generation: process.generation,
            })
    }

    /// Runs the primary pid for live pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn primary_pid_for_live_pane_process(&self, pane_id: &str) -> Option<u32> {
        self.process
            .pane_processes
            .primary_pid(pane_id)
            .or_else(|| {
                self.process
                    .detached_pane_processes
                    .get(pane_id)
                    .map(|process| process.primary_pid)
            })
    }

    /// Writes pane input immediately when the synchronous manager still owns
    /// the pane, or records it for the pane I/O adapter when ownership has
    /// moved across the actor boundary.
    pub(super) fn write_runtime_pane_input(&mut self, pane_id: &str, input: &[u8]) -> Result<()> {
        self.write_runtime_pane_input_with_priority(pane_id, input, false)
    }

    /// Writes generated interactive shell source using platform-native pacing.
    ///
    /// Darwin's terminal stack can accept multiple complete records before an
    /// interactive shell has consumed the preceding one. Adapter-owned macOS
    /// panes therefore wait for fresh shell output between bounded records;
    /// Linux and synchronously owned panes retain the ordinary write path.
    pub(super) fn write_runtime_pane_shell_input(
        &mut self,
        pane_id: &str,
        input: &[u8],
    ) -> Result<()> {
        let delivery = self
            .process
            .running_shell_transactions
            .iter()
            .find_map(|(marker, transaction)| {
                (transaction.pane_id == pane_id).then(|| marker.clone())
            })
            .map_or_else(
                || mez_mux::process::ShellInputDelivery::generated_source(input.to_vec()),
                |marker| {
                    mez_mux::process::ShellInputDelivery::generated_source_for_transaction(
                        input.to_vec(),
                        marker,
                    )
                },
            );
        self.write_runtime_pane_shell_delivery(pane_id, delivery)
    }

    /// Writes one typed shell delivery without dropping pacing or identity.
    pub(super) fn write_runtime_pane_shell_delivery(
        &mut self,
        pane_id: &str,
        delivery: mez_mux::process::ShellInputDelivery,
    ) -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        if self.process.pane_processes.contains_pane(pane_id) {
            return Ok(self
                .process
                .pane_processes
                .write_pane_shell_delivery(pane_id, &delivery)?);
        }

        #[cfg(target_os = "macos")]
        if self.process.pane_processes.contains_pane(pane_id) {
            return Ok(self
                .process
                .pane_processes
                .write_pane_shell_delivery(pane_id, &delivery)?);
        }

        if delivery.bytes.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        if let Some(instance) = self.adapter_owned_pane_process_instance(pane_id) {
            self.persistence
                .queue_pane_input(RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: PaneProcessIoEffect::WriteShellInput { delivery },
                });
            return Ok(());
        }
        Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "pane process not found",
        ))
    }

    /// Cancels the unsent tail of one transaction-scoped shell delivery.
    ///
    /// Cancellation is bound to the current adapter-owned process generation,
    /// so a stale transaction cannot discard input queued for a replacement
    /// process that reused the same pane id. Synchronously owned panes have no
    /// deferred delivery tail and therefore require no cancellation effect.
    pub(super) fn cancel_runtime_pane_shell_delivery(&mut self, pane_id: &str, delivery_id: &str) {
        let Some(instance) = self.adapter_owned_pane_process_instance(pane_id) else {
            return;
        };
        self.persistence
            .queue_shell_input_cancellation(instance, delivery_id.to_string());
    }

    /// Writes pane input with optional async queue priority.
    fn write_runtime_pane_input_with_priority(
        &mut self,
        pane_id: &str,
        input: &[u8],
        priority: bool,
    ) -> Result<()> {
        if input.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        #[cfg(test)]
        if std::mem::take(&mut self.process.require_registered_transaction_on_next_write)
            && !self
                .process
                .running_shell_transactions
                .values()
                .any(|transaction| transaction.pane_id == pane_id)
        {
            return Err(MezError::invalid_state(
                "pane transaction must be registered before delivery",
            ));
        }
        #[cfg(test)]
        if input == b"\x03" && std::mem::take(&mut self.process.fail_next_pane_interrupt_write) {
            return Err(MezError::new(
                crate::error::MezErrorKind::Io,
                "injected pane interrupt write failure",
            ));
        }
        if self.process.pane_processes.contains_pane(pane_id) {
            return Ok(self
                .process
                .pane_processes
                .write_pane_input(pane_id, input)?);
        }
        if let Some(instance) = self.adapter_owned_pane_process_instance(pane_id) {
            self.persistence
                .queue_pane_input(RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: if priority {
                        PaneProcessIoEffect::WriteInputPriority {
                            bytes: input.to_vec(),
                        }
                    } else {
                        PaneProcessIoEffect::WriteInput {
                            bytes: input.to_vec(),
                        }
                    },
                });
            return Ok(());
        }
        Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "pane process not found",
        ))
    }

    /// Writes pane input ahead of later queued input for the same async pane.
    pub(super) fn write_runtime_pane_input_priority(
        &mut self,
        pane_id: &str,
        input: &[u8],
    ) -> Result<()> {
        self.write_runtime_pane_input_with_priority(pane_id, input, true)
    }

    /// Terminates a pane process immediately when manager-owned, or queues a
    /// termination request for an external adapter when ownership has moved.
    pub(super) fn terminate_runtime_pane_process(
        &mut self,
        pane_id: &str,
        force: bool,
    ) -> Result<bool> {
        self.clear_agent_subshell_state(pane_id);
        self.clear_agent_subshell_shell_identity(pane_id);
        if self.process.pane_processes.contains_pane(pane_id) {
            return Ok(self
                .process
                .pane_processes
                .terminate_pane(pane_id)
                .map(|process| process.is_some())?);
        }
        if let Some(process) = self.process.detached_pane_processes.get(pane_id).copied() {
            self.persistence.queue_pane_termination(
                pane_id.to_string(),
                RuntimeSideEffect::PaneProcessIo {
                    instance: PaneProcessInstance {
                        pane_id: pane_id.to_string(),
                        generation: process.generation,
                    },
                    effect: PaneProcessIoEffect::Terminate { force },
                },
            );
            return Ok(true);
        }
        Ok(false)
    }

    /// Terminates each listed pane process through the current owner boundary.
    pub(super) fn terminate_runtime_pane_processes<'a>(
        &mut self,
        pane_ids: impl IntoIterator<Item = &'a str>,
        force: bool,
    ) -> Result<usize> {
        let mut terminated = 0usize;
        for pane_id in pane_ids {
            if self.terminate_runtime_pane_process(pane_id, force)? {
                terminated = terminated.saturating_add(1);
            }
        }
        Ok(terminated)
    }

    /// Terminates all manager-owned and adapter-owned pane processes.
    pub(super) fn terminate_all_runtime_pane_processes(&mut self, force: bool) -> Result<usize> {
        let mut pane_ids = self.process.pane_processes.tracked_pane_ids();
        pane_ids.extend(self.process.detached_pane_processes.keys().cloned());
        self.terminate_runtime_pane_processes(pane_ids.iter().map(String::as_str), force)
    }

    /// Drops runtime-only state for a pane that has been removed from the
    /// session layout.
    ///
    /// Pane closure and process-exit paths remove the pane from the session
    /// model first, then call this helper to clear prompt, readiness, screen,
    /// deferred I/O, and subagent bookkeeping that would otherwise make a
    /// closed pane appear partially alive to later agent/session surfaces.
    pub(super) fn cleanup_removed_pane_runtime_state(&mut self, pane_id: &str) -> Result<()> {
        let pane_present = self.find_pane_descriptor(pane_id).is_some();
        if !pane_present && let Some(process) = self.process.detached_pane_processes.remove(pane_id)
        {
            self.persistence.ensure_pane_termination(
                pane_id.to_string(),
                RuntimeSideEffect::PaneProcessIo {
                    instance: PaneProcessInstance {
                        pane_id: pane_id.to_string(),
                        generation: process.generation,
                    },
                    effect: PaneProcessIoEffect::Terminate { force: true },
                },
            );
        }
        let has_live_agent_turn = self.agent_turn_ledger().turns().iter().any(|turn| {
            turn.pane_id == pane_id
                && matches!(
                    turn.state,
                    AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                )
        });
        if has_live_agent_turn {
            self.fail_agent_turns_for_pane_shutdown(
                &[pane_id.to_string()],
                "pane removed from session layout",
            )?;
        }
        self.presentation.remove_completion_attention(pane_id);
        self.presentation.remove_agent_presentation_state(pane_id);
        self.discard_agent_loop_parent_projections_for_pane(pane_id);
        let removed_transaction_markers = self
            .process
            .running_shell_transactions
            .iter()
            .filter(|(_, transaction)| transaction.pane_id == pane_id)
            .map(|(marker, _)| marker.clone())
            .collect::<Vec<_>>();
        for marker in removed_transaction_markers {
            self.remove_running_shell_transaction(&marker);
            self.clear_shell_transaction_protocol_state(&marker);
        }
        self.agent_shell_store_mut().remove_session(pane_id);
        self.integration.remove_pane_permission_override(pane_id);
        self.clear_agent_subshell_state(pane_id);
        self.clear_agent_subshell_shell_identity(pane_id);
        self.process
            .pane_shell_interaction_generations
            .remove(pane_id);
        self.remove_agent_prompt_input(pane_id);
        self.clear_agent_pane_presentation_preferences(pane_id);
        self.integration
            .agent_personality_selections_mut()
            .remove(pane_id);
        self.clear_agent_routing_override(pane_id);
        self.clear_agent_pane_artifacts(pane_id);
        self.clear_copy_state_for_pane(pane_id);
        self.process
            .pane_current_working_directories
            .remove(pane_id);
        self.process.pane_fish_compatibility.remove(pane_id);
        self.process.pane_bash_compatibility.remove(pane_id);
        self.process.pane_zsh_compatibility.remove(pane_id);
        self.process.pane_foreground_process_groups.remove(pane_id);
        self.process.program_owned_pane_titles.remove(pane_id);
        self.persistence.cleanup_pane_io(pane_id);
        self.process.process_pane_screens.remove(pane_id);
        self.process.agent_pane_screens.remove(pane_id);
        self.process.pane_transaction_osc_screens.remove(pane_id);
        self.process
            .pane_shell_output_render_pending
            .remove(pane_id);
        self.process.pane_mez_wrapper_filter_pending.remove(pane_id);
        self.process
            .pane_mez_wrapper_filter_recent_commands
            .remove(pane_id);
        self.process
            .pane_mez_wrapper_filter_recent_polls
            .remove(pane_id);
        self.process
            .pane_hidden_shell_render_recent_polls
            .remove(pane_id);
        self.process.pane_exit_records.remove(pane_id);
        self.process.active_pane_pipes.remove(pane_id);
        self.persistence.remove_pane_transcript_refs(pane_id);
        self.process.pane_readiness_states.remove(pane_id);
        self.process
            .pane_readiness_overrides
            .clear_pending_probe(pane_id);
        self.process
            .pane_readiness_overrides
            .revoke(pane_id, ReadinessOverrideRevocation::PaneClosed);
        self.process.pane_environment_signatures.remove(pane_id);
        self.process
            .pane_environment_authority_failures
            .remove(pane_id);
        self.process
            .pane_path_scopes
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_path_scope_failures
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_environment_evidence
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .sandbox_mapping_warnings_emitted
            .retain(|identity| !identity.starts_with(&format!("{pane_id}\0")));
        self.process.pane_bootstrap_pending.remove(pane_id);
        self.clear_pane_agent_instruction_files(pane_id);
        self.process.pane_closing.remove(pane_id);
        self.clear_terminal_subagent_pane_close(pane_id);
        self.integration
            .model_profile_overrides_mut()
            .pane_profiles
            .remove(pane_id);
        self.set_agent_auto_sizing_override(pane_id, None);
        self.set_agent_root_routing_policy_override(pane_id, None);
        let pane_turn_ids = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| turn.pane_id == pane_id)
            .map(|turn| turn.turn_id.clone())
            .collect::<Vec<_>>();
        for turn_id in &pane_turn_ids {
            self.clear_agent_failure_feedback_attempts_for_turn(turn_id);
            self.remove_subagent_task_parent(turn_id);
            self.clear_joined_subagent_dependencies_for_turn(turn_id);
        }

        let agent_id = format!("agent-{pane_id}");
        self.remove_subagent_task_routes_for_parent(&agent_id);
        self.remove_joined_subagent_dependencies_for_agent(&agent_id);
        self.integration
            .model_profile_overrides_mut()
            .agent_profiles
            .remove(&agent_id);
        self.remove_subagent_authority_state(&agent_id);
        self.deregister_macro_managed_subagent(&agent_id);
        if let Some(agent_id) = AgentId::opaque(agent_id)
            && self
                .control
                .message_service()
                .registered_identity(&agent_id)
                .is_some()
        {
            let _ = self.control.message_service_mut().update_presence(
                &agent_id,
                mez_agent::messaging::AgentPresenceStatus::Offline,
                current_unix_seconds().saturating_mul(1000),
            );
        }

        let live_windows = self
            .session
            .windows()
            .iter()
            .map(|window| window.id.to_string())
            .collect::<BTreeSet<_>>();
        self.retain_live_subagent_windows(&live_windows);
        let pane_is_live = self
            .session
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .any(|pane| pane.id.as_str() == pane_id);
        if !pane_is_live {
            self.checkpoint_agent_session_metadata()?;
        }
        Ok(())
    }

    /// Runs the initial pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn initial_pane_descriptor(&self) -> Result<PaneDescriptor> {
        let window = self
            .session
            .windows()
            .first()
            .ok_or_else(|| MezError::invalid_state("session has no windows"))?;
        let pane = window
            .panes()
            .first()
            .ok_or_else(|| MezError::invalid_state("initial window has no panes"))?;
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        Ok(PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        })
    }

    /// Runs the active window pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn active_window_pane_descriptor(
        &self,
        target: Option<&str>,
    ) -> Result<PaneDescriptor> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let pane = match target {
            Some(target) => window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == target || pane.index.to_string() == target)
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
                })?,
            None => window.active_pane(),
        };
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        Ok(PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        })
    }

    /// Returns the pane descriptors that should receive primary input.
    pub(super) fn active_window_input_descriptors(&self) -> Result<Vec<PaneDescriptor>> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let panes = if self.session.active_window_panes_synchronized() {
            window.panes().iter().collect::<Vec<_>>()
        } else {
            vec![window.active_pane()]
        };
        Ok(panes
            .into_iter()
            .map(|pane| PaneDescriptor {
                window_id: window.id.clone(),
                pane_id: pane.id.clone(),
                size: self
                    .pane_process_size_for(window, pane.id.as_str())
                    .unwrap_or(pane.size),
            })
            .collect())
    }

    /// Runs the find pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn find_pane_descriptor(&self, pane_id: &str) -> Option<PaneDescriptor> {
        self.session.windows().iter().find_map(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == pane_id)
                .map(|pane| PaneDescriptor {
                    window_id: window.id.clone(),
                    pane_id: pane.id.clone(),
                    size: self
                        .pane_process_size_for(window, pane.id.as_str())
                        .unwrap_or(pane.size),
                })
        })
    }

    /// Runs the find pane title operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn find_pane_title(&self, pane_id: &str) -> Option<String> {
        self.session.windows().iter().find_map(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == pane_id)
                .map(|pane| pane.title.clone())
        })
    }

    /// Runs the tracked pane descriptors operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn tracked_pane_descriptors(&self) -> Vec<PaneDescriptor> {
        self.session
            .windows()
            .iter()
            .flat_map(|window| {
                window.panes().iter().filter_map(|pane| {
                    if self.process.pane_processes.contains_pane(pane.id.as_str())
                        || self.pane_process_is_adapter_owned(pane.id.as_str())
                    {
                        let size = self
                            .pane_process_size_for(window, pane.id.as_str())
                            .unwrap_or(pane.size);
                        Some(PaneDescriptor {
                            window_id: window.id.clone(),
                            pane_id: pane.id.clone(),
                            size,
                        })
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Runs the pane process size for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn pane_process_size_for(
        &self,
        window: &mez_mux::layout::Window,
        pane_id: &str,
    ) -> Option<Size> {
        let size = self.pane_presentation_size_for(window, pane_id)?;
        Some(self.pane_process_size_after_agent_prompt_reservation(pane_id, size))
    }

    /// Returns the process terminal's unreserved presentation geometry.
    pub(crate) fn pane_presentation_size_for(
        &self,
        window: &mez_mux::layout::Window,
        pane_id: &str,
    ) -> Option<Size> {
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let window_frame_visible = self.window_frames_enabled();
        let group_rows = u16::from(self.session.window_groups().len() > 1);
        let display_size = Size::new(
            window.size.columns,
            window.size.rows.saturating_sub(group_rows).max(1),
        )
        .ok()?;
        if window.zoomed_pane_id() == Some(&pane.id) {
            let body_size = rendered_window_body_size(display_size, window_frame_visible);
            let geometry = mez_mux::layout::PaneGeometry {
                index: pane.index,
                column: 0,
                row: 0,
                columns: body_size.columns,
                rows: body_size.rows,
            };
            let content_size = pane_content_size_for_geometry(
                &geometry,
                std::slice::from_ref(&geometry),
                self.pane_frames_enabled(),
                self.pane_frame_position(),
            );
            return Some(content_size);
        }

        let body_size = rendered_window_body_size(display_size, window_frame_visible);
        let geometries = window.pane_geometries_for_size(body_size);
        let geometry = geometries
            .iter()
            .find(|geometry| geometry.index == pane.index)?;
        let content_size = pane_content_size_for_geometry(
            geometry,
            &geometries,
            self.pane_frames_enabled(),
            self.pane_frame_position(),
        );
        Some(content_size)
    }

    /// Removes rows reserved for the pane-local agent prompt from the PTY size
    /// advertised to the shell. Keeping the process size aligned with the
    /// visible terminal buffer prevents prompts and cursor reports from
    /// landing underneath the agent input row.
    fn pane_process_size_after_agent_prompt_reservation(&self, pane_id: &str, size: Size) -> Size {
        let reserved_rows = self.agent_prompt_reserved_rows_for_pane(
            pane_id,
            usize::from(size.columns),
            usize::from(size.rows),
        );
        let reserved_rows = u16::try_from(reserved_rows)
            .unwrap_or(u16::MAX)
            .min(size.rows.saturating_sub(1));
        Size {
            columns: size.columns,
            rows: size.rows.saturating_sub(reserved_rows).max(1),
        }
    }

    /// Runs the append pane start event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_start_event(&mut self, update: &PaneProcessStart) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","columns":{},"rows":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.size.columns,
                update.size.rows
            ),
        )
    }

    /// Runs the append pane resize event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_resize_event(&mut self, update: &PaneResizeUpdate) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","layout":"resized","columns":{},"rows":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.size.columns,
                update.size.rows
            ),
        )
    }

    /// Runs the append pane output event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_output_event(&mut self, update: &PaneOutputUpdate) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","output_bytes":{},"activity_events":{},"bell_events":{},"background":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.bytes_read,
                update.activity_events,
                update.bell_events,
                update.background
            ),
        )
    }

    /// Runs the append pane title event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_title_event(&mut self, update: &PaneOutputUpdate) -> Result<()> {
        let title = self
            .find_pane_title(update.pane_id.as_str())
            .unwrap_or_else(|| "shell".to_string());
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","title":"{}"}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                json_escape(&title)
            ),
        )
    }

    /// Runs the append pane exit event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_exit_event(&mut self, update: &PaneExitUpdate) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"exited","exit_status":{},"exit_code":{},"signal":{},"closed_window":{},"session_empty":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.exit_status.to_json(),
                optional_i32_json(update.exit_status.code),
                optional_i32_json(update.exit_status.signal),
                update.closed_window,
                update.session_empty
            ),
        )
    }

    /// Runs the append pane close event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_close_event(
        &mut self,
        pane_id: &str,
        window_id: &str,
        terminated_panes: usize,
        session_empty: bool,
    ) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","state":"closed","closed":true,"terminated_panes":{},"session_empty":{}}}"#,
                json_escape(pane_id),
                json_escape(window_id),
                terminated_panes,
                session_empty
            ),
        )
    }

    /// Runs the append window close event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_window_close_event(
        &mut self,
        window_id: &str,
        terminated_panes: usize,
        session_empty: bool,
    ) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::WindowChanged,
            format!(
                r#"{{"window_id":"{}","state":"closed","closed":true,"terminated_panes":{},"session_empty":{}}}"#,
                json_escape(window_id),
                terminated_panes,
                session_empty
            ),
        )
    }
}
