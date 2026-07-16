//! Long-lived runtime session-service aggregate and owned subsystem stores.

use super::*;
use crate::runtime::{
    RuntimeAgentComponent, RuntimePersistenceComponent, RuntimePresentationComponent,
    RuntimeProcessComponent,
};

/// Carries Runtime Session Service state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub struct RuntimeSessionService {
    /// Private state owner for terminal presentation and client interaction.
    pub(in crate::runtime) presentation: RuntimePresentationComponent,
    /// Private state owner for pane process metadata and lifecycle invariants.
    pub(in crate::runtime) process: RuntimeProcessComponent,
    /// Private state owner for application-side agent execution.
    pub(in crate::runtime) agent: RuntimeAgentComponent,
    /// Private state owner for repositories and deferred external effects.
    pub(in crate::runtime) persistence: RuntimePersistenceComponent,
    /// Stores the session value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) session: Session,
    /// Stores the window created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) window_created_at_unix_seconds: BTreeMap<String, u64>,
    /// Stores the config layers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) config_layers: Vec<ConfigLayer>,
    /// Stores the config root value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) config_root: Option<PathBuf>,
    /// Stores the control idempotency value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) control_idempotency: ControlIdempotencyCache,
    /// Stores the message service value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) message_service: MessageService,
    /// Stores the latest async runtime actor metrics snapshot when available.
    ///
    /// The actor-owned command path updates this snapshot before rendering
    /// `show-metrics` so runtime display helpers can present metrics without
    /// taking a direct dependency on actor internals.
    pub(in crate::runtime) async_runtime_metrics:
        Option<crate::async_runtime::AsyncRuntimeActorMetrics>,
    /// Stores runtime-owned agent, provider, and shell diagnostics.
    ///
    /// These counters and histograms are updated from the serialized runtime
    /// service path so `show-metrics` can expose prompt-cache shape, provider
    /// usage, turn lifecycle, and shell-transaction behavior without parsing
    /// trace logs.
    pub(in crate::runtime) runtime_metrics: RuntimeMetricsSnapshot,
    /// Stores the permission policy value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) permission_policy: PermissionPolicy,
    /// Stores an explicit live approval-bypass override selected by the user.
    ///
    /// Configuration is intentionally unable to enable approval bypass, so
    /// explicit runtime activation must survive unrelated configuration
    /// reloads without being encoded into normal config layers.
    pub(in crate::runtime) live_approval_bypass_override: Option<bool>,
    /// Stores an explicit live approval-policy override selected by the user.
    ///
    /// Runtime approval changes are session choices. They must survive unrelated
    /// configuration reloads without being erased by persistent config changes.
    pub(in crate::runtime) live_approval_policy_override: Option<mez_agent::ApprovalPolicy>,
    /// Stores the blocked approvals value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) blocked_approvals: BlockedApprovalQueue,
    /// Stores the session approvals value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) session_approvals: SessionApprovalStore,
    /// Stores the session memory value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) session_memory: SessionMemoryStore,
    /// Stores the mcp registry value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) mcp_registry: McpRegistry,
    /// Stores the mcp transports value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) mcp_transports: RuntimeMcpTransportSet,
    /// Stores the provider registry value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) provider_registry: RuntimeProviderRegistry,
    /// Stores the preset registry value for this data structure.
    pub(in crate::runtime) preset_registry: RuntimePresetRegistry,
    /// Stores the subagent profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) subagent_profiles: BTreeMap<String, SubagentProfile>,
    /// User-defined pane personality profiles.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_personality_profiles:
        BTreeMap<String, RuntimeAgentPersonalityProfile>,
    /// Configured default personality profile id, when one exists.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) default_agent_personality: Option<String>,
    /// User-configured system prompt text appended after the built-in prompt.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) custom_agent_system_prompt: Option<String>,
    /// Pane-local selected personality profile ids.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_personality_selections: BTreeMap<String, String>,
    /// Stores the model profile overrides value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) model_profile_overrides: RuntimeModelProfileOverrideStore,
    /// Stores the auth store value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) auth_store: Option<AuthStore>,
    /// Seconds before provider access-token expiry that triggers proactive refresh.
    ///
    /// The field is part of structured runtime state so startup and provider
    /// turn preflight checks use the same configured threshold.
    pub(in crate::runtime) provider_auth_refresh_leeway_seconds: u64,
    /// Stores the project trust store value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) project_trust_store: Option<ProjectTrustStore>,
    /// Stores the project trust database path value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) project_trust_database_path: Option<PathBuf>,
    /// Stores the announced project trust roots value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) announced_project_trust_roots: BTreeSet<PathBuf>,
    /// Stores the hook definitions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) hook_definitions: Vec<HookDefinition>,
    /// Stores the focused shell hooks value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) focused_shell_hooks: FocusedShellHookQueue,
    /// Stores the next focused shell hook marker value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) next_focused_shell_hook_marker: u64,
    /// Stores the focused shell hook transactions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) focused_shell_hook_transactions:
        BTreeMap<String, PendingFocusedShellHookTransaction>,
    /// Stores the focused shell hook results value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) focused_shell_hook_results: Vec<HookExecutionResult>,
    /// Stores the event log value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) event_log: Option<EventLog>,
    /// Stores the lifecycle state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) lifecycle_state: RuntimeLifecycleState,
    /// Stores the socket path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) socket_path: PathBuf,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) created_at_unix_seconds: u64,
    /// Stores the last attach at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) last_attach_at_unix_seconds: Option<u64>,
}
