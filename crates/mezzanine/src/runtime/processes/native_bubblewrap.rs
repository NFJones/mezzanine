//! Native transport Bubblewrap assembly for native shell mode.
//!
//! Native shell mode never writes to the pane PTY, so sandboxed actions
//! cannot reuse pane transactions for environment evidence or capability
//! probing. This module assembles the identical Bubblewrap launch plan from
//! root-process metadata instead: credentials and groups read from the host,
//! optional environment forwarding resolved from the root-process
//! environment, a host-side capability probe executed directly, and path
//! authority from the transport-neutral permission and trust-store state.
//!
//! The compiled plan is returned as a `ShellChildLaunch` so the spawned
//! shell executor renders the identical argv as pane transport without any
//! pane interaction, and a managed-home activity lock is retained until the
//! spawned action settles.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mez_agent::permissions::{PathScopes, PermissionEvaluation};
use mez_agent::shell::PaneEnvironmentEvidence;
use mez_agent::{
    AgentAction, AgentActionPayload, AgentTurnRecord, EnvironmentGroup, EnvironmentSignature,
    LocalProgramDialect, ShellChildArgument, ShellChildLaunch,
};
#[cfg(test)]
use mez_mux::process::RawEnvironmentEntry;

use crate::error::{MezError, Result};
use crate::security::filesystem::host_resolved_path_scopes;
#[cfg(test)]
use crate::security::filesystem::resolve_host_path;
#[cfg(test)]
use mez_agent::permissions::ResolvedPathKind;

use super::native_shell_inference::NativeShellContext;

/// Maximum host-side Bubblewrap capability probe duration.
const NATIVE_BUBBLEWRAP_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Poll interval while waiting for the capability probe.
const NATIVE_BUBBLEWRAP_PROBE_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Maximum retained output from either native capability-probe pipe.
const NATIVE_BUBBLEWRAP_PROBE_OUTPUT_LIMIT_BYTES: usize = 8 * 1024;
/// Maximum escaped probe output included in one user-facing diagnostic.
const NATIVE_BUBBLEWRAP_PROBE_DIAGNOSTIC_PREVIEW_BYTES: usize = 512;

/// Uncached native capability probe transferred to the external worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeBubblewrapCapabilityProbe {
    pane_id: String,
    pane_environment_signature: String,
    config_generation: u64,
    plan: crate::security::sandbox::BubblewrapCapabilityProbePlan,
}

impl NativeBubblewrapCapabilityProbe {
    /// Builds one exact native probe for an admitted non-agent workload.
    pub(crate) fn new(
        pane_id: String,
        pane_environment_signature: String,
        config_generation: u64,
        plan: crate::security::sandbox::BubblewrapCapabilityProbePlan,
    ) -> Self {
        Self {
            pane_id,
            pane_environment_signature,
            config_generation,
            plan,
        }
    }

    /// Runs the exact native probe while observing a caller-owned lifecycle
    /// cancellation fence.
    pub(crate) fn run_with_cancellation(
        self,
        cancellation: &AtomicBool,
    ) -> Result<crate::security::sandbox::BubblewrapCapability> {
        run_native_bubblewrap_capability_probe(
            &self.pane_id,
            &self.pane_environment_signature,
            self.config_generation,
            &self.plan,
            Some(cancellation),
            &super::launch_accounting::NativeLaunchLedger::new(false),
        )
    }

    /// Builds one deterministic worker-owned probe fixture.
    #[cfg(test)]
    pub(crate) fn for_test(
        executable: &str,
        arguments: Vec<String>,
        expected_stdout: &'static str,
    ) -> Self {
        Self {
            pane_id: "%native-test".to_string(),
            pane_environment_signature: "native-test-signature".to_string(),
            config_generation: 1,
            plan: crate::security::sandbox::BubblewrapCapabilityProbePlan {
                executable: executable.to_string(),
                arguments,
                expected_stdout,
                identity_sha256: "native-test-identity".to_string(),
                environment_sha256: "native-test-environment".to_string(),
                probe_sha256: "native-test-probe".to_string(),
            },
        }
    }
}

/// Uncached native Seatbelt capability probe transferred to an external
/// worker by the dependent Seatbelt workload-integration boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeSeatbeltCapabilityProbe {
    pane_id: String,
    pane_environment_signature: String,
    config_generation: u64,
    plan: crate::security::sandbox::SeatbeltCapabilityProbePlan,
}

impl NativeSeatbeltCapabilityProbe {
    /// Builds one exact native probe from already trusted root-process
    /// environment evidence.
    pub(crate) fn new(
        pane_id: String,
        pane_environment_signature: String,
        config_generation: u64,
        plan: crate::security::sandbox::SeatbeltCapabilityProbePlan,
    ) -> Self {
        Self {
            pane_id,
            pane_environment_signature,
            config_generation,
            plan,
        }
    }

    /// Runs the exact native probe while observing a caller-owned lifecycle
    /// cancellation fence.
    pub(crate) fn run_with_cancellation(
        self,
        cancellation: &AtomicBool,
    ) -> Result<crate::security::sandbox::SeatbeltCapability> {
        run_native_seatbelt_capability_probe(
            &self.pane_id,
            &self.pane_environment_signature,
            self.config_generation,
            &self.plan,
            Some(cancellation),
            &super::launch_accounting::NativeLaunchLedger::new(false),
        )
    }

    /// Builds one deterministic worker-owned Seatbelt probe fixture.
    #[cfg(test)]
    pub(crate) fn for_test(
        executable: &str,
        arguments: Vec<String>,
        expected_stdout: &'static str,
    ) -> Self {
        Self {
            pane_id: "%native-seatbelt-test".to_string(),
            pane_environment_signature: "native-seatbelt-test-signature".to_string(),
            config_generation: 1,
            plan: crate::security::sandbox::SeatbeltCapabilityProbePlan {
                executable: executable.to_string(),
                arguments,
                expected_stdout,
                sandbox_executable: "/usr/bin/sandbox-exec".to_string(),
                executable_identity_sha256: "product-identity".to_string(),
                sandbox_executable_identity_sha256: "seatbelt-identity".to_string(),
                child_shell_path: "/bin/sh".to_string(),
                child_shell_identity_sha256: "shell-identity".to_string(),
                environment_sha256: "environment-identity".to_string(),
                host_identity_sha256: "host-identity".to_string(),
                profile_sha256: "profile-identity".to_string(),
                probe_sha256: "probe-identity".to_string(),
            },
        }
    }
}

/// Backend-tagged native capability probe executed by the external shell
/// worker before any corresponding sandbox workload may start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NativeSandboxCapabilityProbe {
    /// Linux Bubblewrap namespace capability probe.
    Bubblewrap(NativeBubblewrapCapabilityProbe),
    /// macOS Seatbelt operation-level capability probe.
    Seatbelt(NativeSeatbeltCapabilityProbe),
}

impl NativeSandboxCapabilityProbe {
    /// Runs a probe with the caller's explicit workload launch ledger.
    #[cfg(test)]
    pub(crate) fn run_accounted(
        self,
        launches: &super::launch_accounting::NativeLaunchLedger,
    ) -> Result<crate::security::sandbox::SandboxCapability> {
        self.run_accounted_with_cancellation(launches, None)
    }

    /// Runs the exact accounted probe with a monotonic workload cancellation fence.
    pub(crate) fn run_accounted_with_cancellation(
        self,
        launches: &super::launch_accounting::NativeLaunchLedger,
        cancellation: Option<&AtomicBool>,
    ) -> Result<crate::security::sandbox::SandboxCapability> {
        match self {
            Self::Bubblewrap(probe) => run_native_bubblewrap_capability_probe(
                &probe.pane_id,
                &probe.pane_environment_signature,
                probe.config_generation,
                &probe.plan,
                cancellation,
                launches,
            )
            .map(crate::security::sandbox::SandboxCapability::Bubblewrap),
            Self::Seatbelt(probe) => run_native_seatbelt_capability_probe(
                &probe.pane_id,
                &probe.pane_environment_signature,
                probe.config_generation,
                &probe.plan,
                cancellation,
                launches,
            )
            .map(crate::security::sandbox::SandboxCapability::Seatbelt),
        }
    }

    /// Runs the backend probe while observing a caller-owned lifecycle
    /// cancellation fence.
    pub(crate) fn run_with_cancellation(
        self,
        cancellation: &AtomicBool,
    ) -> Result<crate::security::sandbox::SandboxCapability> {
        match self {
            Self::Bubblewrap(probe) => probe
                .run_with_cancellation(cancellation)
                .map(crate::security::sandbox::SandboxCapability::Bubblewrap),
            Self::Seatbelt(probe) => probe
                .run_with_cancellation(cancellation)
                .map(crate::security::sandbox::SandboxCapability::Seatbelt),
        }
    }
}

/// Cloneable managed-home lease retained by both actor state and the external
/// native worker. Turn cleanup may drop the actor's clone, but the worker's
/// clone keeps maintenance excluded until probe and workload execution end.
#[derive(Debug, Clone)]
pub(crate) struct NativeBubblewrapActivityLease {
    activity_lock: Arc<crate::security::sandbox::BubblewrapManagedHomeActivityLock>,
}

impl NativeBubblewrapActivityLease {
    fn new(activity_lock: crate::security::sandbox::BubblewrapManagedHomeActivityLock) -> Self {
        Self {
            activity_lock: Arc::new(activity_lock),
        }
    }
}

impl PartialEq for NativeBubblewrapActivityLease {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.activity_lock, &other.activity_lock)
    }
}

impl Eq for NativeBubblewrapActivityLease {}

/// Everything native dispatch needs to run one sandboxed action.
pub(crate) struct NativeBubblewrapDispatch {
    /// Compiled Bubblewrap argv rendered by the spawned shell executor.
    pub(crate) child_launch: ShellChildLaunch,
    /// Redacted plan facts for diagnostics.
    pub(crate) audit_summary: crate::security::sandbox::SandboxAuditSummary,
    /// Uncached capability proof that must complete before workload launch.
    pub(crate) capability_probe: Option<NativeBubblewrapCapabilityProbe>,
    /// Shared workload lock retained until the spawned action settles.
    pub(crate) activity_lease: Option<NativeBubblewrapActivityLease>,
}

/// Everything native dispatch needs to run one Seatbelt-confined action.
pub(crate) struct NativeSeatbeltDispatch {
    /// Typed outer-launcher argv carrying the generated Seatbelt profile.
    pub(crate) child_launch: ShellChildLaunch,
    /// Redacted plan facts shared with pane dispatch and audit.
    pub(crate) audit_summary: crate::security::sandbox::SandboxAuditSummary,
    /// Cloneable action/home/temp cleanup lease retained through settlement.
    pub(crate) workload_lease: crate::security::sandbox::SeatbeltWorkloadLease,
}

impl crate::runtime::RuntimeSessionService {
    /// Compiles one trusted and explicitly allowed pane-status provider into a
    /// fail-closed sandbox launch. No unsandboxed fallback is available.
    pub(crate) fn compile_pane_status_provider_launch(
        &mut self,
        pane_id: &str,
        command: &str,
        context: &NativeShellContext,
        maximum_authority: &PathScopes,
        evaluation: &PermissionEvaluation,
    ) -> Result<crate::runtime::status_pills::RuntimePaneStatusProviderLaunch> {
        if evaluation.decision != mez_agent::permissions::RuleDecision::Allow {
            return Err(MezError::forbidden(
                "pane status provider requires an explicitly allowed permission evaluation",
            ));
        }
        let sandbox_config = self.sandbox_config_for_pane(pane_id);
        let policy = self.permission_policy_for_pane(pane_id);
        if !crate::runtime::config::sandbox_applies_to_policy(&sandbox_config, &policy) {
            return Err(MezError::forbidden(
                "pane status provider requires an active OS sandbox",
            ));
        }
        let canonical_cwd = std::fs::canonicalize(context.working_directory()).map_err(|error| {
            MezError::invalid_state(format!(
                "pane status provider could not canonicalize its pane working directory: {error}"
            ))
        })?;
        if maximum_authority.current_directory != canonical_cwd.to_string_lossy() {
            return Err(MezError::conflict(
                "pane status provider context changed before sandbox admission",
            ));
        }
        let signature = native_environment_signature_for_context(
            context,
            self.primary_pid_for_live_pane_process(pane_id),
        )?;
        let environment_request =
            mez_agent::shell::PaneEnvironmentRequest::new(vec!["MEZ_PANE_ID".to_string()])
                .map_err(|error| MezError::invalid_args(error.message()))?;
        let environment_evidence = PaneEnvironmentEvidence::from_parts(
            &environment_request,
            BTreeMap::from([(
                "MEZ_PANE_ID".to_string(),
                provider_pane_identity(context, pane_id)?,
            )]),
            BTreeMap::new(),
        )
        .map_err(|error| MezError::invalid_args(error.message()))?;
        let effective_policy = crate::security::sandbox::effective_sandbox_policy_for_authority(
            maximum_authority,
            evaluation,
            false,
            self.configured_permissions().resources.network_policy,
            match &sandbox_config {
                crate::runtime::SandboxConfig::Bubblewrap(config) => config.network,
                crate::runtime::SandboxConfig::Seatbelt(config) => config.network,
                crate::runtime::SandboxConfig::PolicyOnly => {
                    return Err(MezError::forbidden(
                        "pane status provider requires an active OS sandbox",
                    ));
                }
            },
            match &sandbox_config {
                crate::runtime::SandboxConfig::Bubblewrap(config) => config.environment,
                crate::runtime::SandboxConfig::Seatbelt(config) => config.environment,
                crate::runtime::SandboxConfig::PolicyOnly => unreachable!(
                    "policy-only pane providers are rejected before policy compilation"
                ),
            },
        )
        .map_err(|error| MezError::forbidden(error.message()))?;
        let context_generation = self.session.config_generation;
        let restricted_context = context.restricted_for_pane_status_provider();

        match sandbox_config {
            crate::runtime::SandboxConfig::Bubblewrap(config) => {
                let identity = crate::security::sandbox::resolve_sandbox_identity(
                    &config.group_whitelist,
                    &signature,
                )
                .map_err(|error| MezError::invalid_state(error.message()))?;
                let probe_plan =
                    crate::security::sandbox::bubblewrap_capability_probe_plan_for_identity(
                        &config,
                        context.shell_path().to_string_lossy().as_ref(),
                        &identity,
                        &environment_evidence,
                    )
                    .map_err(|error| MezError::invalid_state(error.message()))?;
                let signature_hash = signature.stable_hash();
                let cache_key = crate::security::sandbox::bubblewrap_capability_cache_key(
                    pane_id,
                    &signature_hash,
                    context_generation,
                    &probe_plan,
                )
                .map_err(|error| MezError::invalid_state(error.message()))?;
                let (capability, capability_probe) = match self.bubblewrap_capability(&cache_key) {
                    Some(capability) => (capability, None),
                    None => (
                        crate::security::sandbox::BubblewrapCapability {
                            cache_key: cache_key.clone(),
                        },
                        Some(NativeSandboxCapabilityProbe::Bubblewrap(
                            NativeBubblewrapCapabilityProbe::new(
                                pane_id.to_string(),
                                signature_hash.clone(),
                                context_generation,
                                probe_plan,
                            ),
                        )),
                    ),
                };
                let launch_plan = crate::security::sandbox::compile_sandbox_launch_plan(
                    crate::security::sandbox::SandboxCompileRequest::Bubblewrap(
                        crate::security::sandbox::BubblewrapCompileRequest {
                            config: &config,
                            identity,
                            capability,
                            pane_environment_signature: &signature_hash,
                            environment_evidence: &environment_evidence,
                            network_policy: self.configured_permissions().resources.network_policy,
                            maximum_authority,
                            permission_evaluation: evaluation,
                            preserve_maximum_authority: false,
                            child_shell_path: context.shell_path().to_string_lossy().as_ref(),
                            command_file_host_path:
                                crate::security::sandbox::BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER,
                            managed_home: None,
                            pane_home_directory: signature.home_directory.as_deref().map(Path::new),
                            stateful: false,
                            interactive: false,
                        },
                    ),
                )
                .map_err(|error| MezError::forbidden(error.message()))?;
                let arguments = launch_plan
                    .arguments
                    .into_iter()
                    .map(|argument| {
                        if argument
                            == crate::security::sandbox::BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER
                        {
                            ShellChildArgument::MaterializedCommandFile
                        } else {
                            ShellChildArgument::Literal(argument)
                        }
                    })
                    .collect();
                let child_launch = ShellChildLaunch::new(launch_plan.executable, arguments)
                    .map_err(|error| MezError::invalid_state(error.message()))?
                    .with_status_fd(crate::security::sandbox::BUBBLEWRAP_STATUS_FD)
                    .map_err(|error| MezError::invalid_state(error.message()))?;
                Ok(
                    crate::runtime::status_pills::RuntimePaneStatusProviderLaunch {
                        context: restricted_context,
                        capability_probe,
                        sandbox_backend: crate::runtime::SandboxBackend::Bubblewrap,
                        child_launch,
                        bubblewrap_activity_lease: None,
                        seatbelt_workload_lease: None,
                    },
                )
            }
            crate::runtime::SandboxConfig::Seatbelt(config) => {
                let signature_hash = signature.stable_hash();
                let probe_plan = crate::security::sandbox::seatbelt_capability_probe_plan(
                    &config,
                    context.shell_path().to_string_lossy().as_ref(),
                    &signature,
                    &environment_evidence,
                )
                .map_err(|error| MezError::invalid_state(error.message()))?;
                let cache_key = crate::security::sandbox::seatbelt_capability_cache_key(
                    pane_id,
                    &signature_hash,
                    context_generation,
                    &probe_plan,
                )
                .map_err(|error| MezError::invalid_state(error.message()))?;
                let capability_probe = self.seatbelt_capability(&cache_key).is_none().then(|| {
                    NativeSandboxCapabilityProbe::Seatbelt(NativeSeatbeltCapabilityProbe::new(
                        pane_id.to_string(),
                        signature_hash,
                        context_generation,
                        probe_plan,
                    ))
                });
                let home_directory = signature.home_directory.as_deref().ok_or_else(|| {
                    MezError::invalid_state(
                        "canonical pane home is unavailable for Seatbelt launch",
                    )
                })?;
                let artifacts = crate::security::sandbox::prepare_seatbelt_workload_artifacts(
                    Path::new(home_directory),
                    command,
                    None,
                )
                .map_err(|error| MezError::invalid_state(error.message()))?;
                let child_launcher = std::env::current_exe()
                    .and_then(std::fs::canonicalize)
                    .map_err(|error| {
                        MezError::invalid_state(format!(
                            "pane status provider launcher discovery failed: {error}"
                        ))
                    })?;
                let child_launcher = child_launcher.to_str().ok_or_else(|| {
                    MezError::invalid_state("pane status provider launcher path is not UTF-8")
                })?;
                let launch_plan = crate::security::sandbox::seatbelt::compile_seatbelt_launch_plan(
                    crate::security::sandbox::seatbelt::SeatbeltCompileRequest {
                        config: &config,
                        policy: &effective_policy,
                        child_shell_path: context.shell_path().to_string_lossy().as_ref(),
                        child_launcher_path: child_launcher,
                        command_file_path: &artifacts.command_file_path.to_string_lossy(),
                        environment_file_path: &artifacts.environment_file_path.to_string_lossy(),
                        home_directory: &artifacts.home_directory.to_string_lossy(),
                        temporary_directory: &artifacts.temporary_directory.to_string_lossy(),
                        user_name: &signature.user,
                        environment_evidence: &environment_evidence,
                        stateful: false,
                        interactive: false,
                    },
                )
                .map_err(|error| MezError::forbidden(error.message()))?;
                artifacts
                    .write_environment_document(&launch_plan.environment_document)
                    .map_err(|error| MezError::invalid_state(error.message()))?;
                let child_launch = launch_plan
                    .child_launch
                    .with_status_fd(crate::security::sandbox::SANDBOX_STATUS_FD)
                    .map_err(|error| MezError::invalid_state(error.message()))?;
                Ok(
                    crate::runtime::status_pills::RuntimePaneStatusProviderLaunch {
                        context: restricted_context,
                        capability_probe,
                        sandbox_backend: crate::runtime::SandboxBackend::Seatbelt,
                        child_launch,
                        bubblewrap_activity_lease: None,
                        seatbelt_workload_lease: Some(artifacts.lease),
                    },
                )
            }
            crate::runtime::SandboxConfig::PolicyOnly => Err(MezError::forbidden(
                "pane status provider requires an active OS sandbox",
            )),
        }
    }

    /// Builds an uncached native Seatbelt probe from exact root-process
    /// environment evidence, or returns `None` when that identity is cached.
    pub(crate) fn native_seatbelt_capability_probe_for_action(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        context: &NativeShellContext,
        config: &crate::runtime::SeatbeltConfig,
        program_dialect: LocalProgramDialect,
    ) -> Result<Option<NativeSeatbeltCapabilityProbe>> {
        let signature = native_environment_signature_for_context(
            context,
            self.primary_pid_for_live_pane_process(&turn.pane_id),
        )?;
        let signature_hash = signature.stable_hash();
        let request = mez_agent::shell::PaneEnvironmentRequest::new(
            config.env_whitelist.requested_names.clone(),
        )
        .map_err(|error| MezError::invalid_args(error.message()))?;
        let evidence = if matches!(action.payload, AgentActionPayload::ApplyPatch { .. }) {
            PaneEnvironmentEvidence::restrictive(&request, "semantic_patch_not_forwarded")
        } else {
            super::native_workload_environment::server_environment_evidence(
                &request,
                self.server_environment(),
            )
        };
        let child_shell_path = program_dialect
            .interpreter_path()
            .unwrap_or(&signature.shell_path);
        let plan = crate::security::sandbox::seatbelt_capability_probe_plan(
            config,
            child_shell_path,
            &signature,
            &evidence,
        )
        .map_err(|error| MezError::invalid_state(error.message()))?;
        let cache_key = crate::security::sandbox::seatbelt_capability_cache_key(
            &turn.pane_id,
            &signature_hash,
            self.session.config_generation,
            &plan,
        )
        .map_err(|error| MezError::invalid_state(error.message()))?;
        if self.seatbelt_capability(&cache_key).is_some() {
            return Ok(None);
        }
        Ok(Some(NativeSeatbeltCapabilityProbe::new(
            turn.pane_id.clone(),
            signature_hash,
            self.session.config_generation,
            plan,
        )))
    }

    /// Compiles one authorized native action into the same typed Seatbelt
    /// workload launch used by pane transport after exact capability proof.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the pane dispatch surface's per-action inputs"
    )]
    pub(crate) fn native_seatbelt_dispatch_for_action(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        context: &NativeShellContext,
        config: &crate::runtime::SeatbeltConfig,
        permission_evaluation: &PermissionEvaluation,
        program_dialect: LocalProgramDialect,
        command: &str,
        input_sidecar: Option<&str>,
    ) -> Result<NativeSeatbeltDispatch> {
        let signature = native_environment_signature_for_context(
            context,
            self.primary_pid_for_live_pane_process(&turn.pane_id),
        )?;
        let signature_hash = signature.stable_hash();
        let request = mez_agent::shell::PaneEnvironmentRequest::new(
            config.env_whitelist.requested_names.clone(),
        )
        .map_err(|error| MezError::invalid_args(error.message()))?;
        let evidence = if matches!(action.payload, AgentActionPayload::ApplyPatch { .. }) {
            PaneEnvironmentEvidence::restrictive(&request, "semantic_patch_not_forwarded")
        } else {
            super::native_workload_environment::server_environment_evidence(
                &request,
                self.server_environment(),
            )
        };
        let child_shell_path = program_dialect
            .interpreter_path()
            .unwrap_or(&signature.shell_path);
        let probe_plan = crate::security::sandbox::seatbelt_capability_probe_plan(
            config,
            child_shell_path,
            &signature,
            &evidence,
        )
        .map_err(|error| MezError::invalid_state(error.message()))?;
        let cache_key = crate::security::sandbox::seatbelt_capability_cache_key(
            &turn.pane_id,
            &signature_hash,
            self.session.config_generation,
            &probe_plan,
        )
        .map_err(|error| MezError::invalid_state(error.message()))?;
        self.seatbelt_capability(&cache_key).ok_or_else(|| {
            MezError::invalid_state(
                "Seatbelt capability is unavailable for the active native environment",
            )
        })?;
        let maximum_authority =
            self.native_bubblewrap_path_scopes_for_turn(turn, context, permission_evaluation)?;
        let policy = crate::security::sandbox::effective_sandbox_policy_for_authority(
            &maximum_authority,
            permission_evaluation,
            matches!(action.payload, AgentActionPayload::ApplyPatch { .. }),
            self.configured_permissions().resources.network_policy,
            config.network,
            config.environment,
        )
        .map_err(|error| MezError::invalid_state(error.message()))?;
        let home_directory = signature.home_directory.as_deref().ok_or_else(|| {
            MezError::invalid_state("canonical native home is unavailable for Seatbelt dispatch")
        })?;
        let artifacts = crate::security::sandbox::prepare_seatbelt_workload_artifacts(
            Path::new(home_directory),
            command,
            input_sidecar,
        )
        .map_err(|error| MezError::invalid_state(error.message()))?;
        let child_launcher = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|error| {
                MezError::invalid_state(format!(
                    "Seatbelt child launcher discovery failed: {error}"
                ))
            })?;
        let child_launcher = child_launcher
            .to_str()
            .ok_or_else(|| MezError::invalid_state("Seatbelt child launcher path is not UTF-8"))?;
        let plan = crate::security::sandbox::seatbelt::compile_seatbelt_launch_plan(
            crate::security::sandbox::seatbelt::SeatbeltCompileRequest {
                config,
                policy: &policy,
                child_shell_path,
                child_launcher_path: child_launcher,
                command_file_path: &artifacts.command_file_path.to_string_lossy(),
                environment_file_path: &artifacts.environment_file_path.to_string_lossy(),
                home_directory: &artifacts.home_directory.to_string_lossy(),
                temporary_directory: &artifacts.temporary_directory.to_string_lossy(),
                user_name: &signature.user,
                environment_evidence: &evidence,
                stateful: false,
                interactive: false,
            },
        )
        .map_err(|error| MezError::invalid_state(error.message()))?;
        artifacts
            .write_environment_document(&plan.environment_document)
            .map_err(|error| MezError::invalid_state(error.message()))?;
        Ok(NativeSeatbeltDispatch {
            child_launch: plan
                .child_launch
                .with_status_fd(crate::security::sandbox::SANDBOX_STATUS_FD)
                .map_err(|error| MezError::invalid_state(error.message()))?,
            audit_summary: plan.audit_summary,
            workload_lease: artifacts.lease,
        })
    }

    /// Assembles the native Bubblewrap child launch for one authorized action.
    ///
    /// Identity, forwarding evidence, capability, filesystem authority, and
    /// managed home are all derived without pane transactions: credentials
    /// come from host process metadata, evidence from the root-process
    /// environment, the capability probe runs directly, and path authority
    /// comes from transport-neutral permission and trust-store state.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the pane dispatch surface's per-action inputs"
    )]
    pub(crate) fn native_bubblewrap_dispatch_for_action(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        context: &NativeShellContext,
        sandbox_config: &crate::runtime::SandboxConfig,
        permission_evaluation: &PermissionEvaluation,
        program_dialect: LocalProgramDialect,
    ) -> Result<NativeBubblewrapDispatch> {
        let crate::runtime::SandboxConfig::Bubblewrap(config) = sandbox_config else {
            return Err(MezError::invalid_state(
                "native Bubblewrap dispatch requires the Bubblewrap backend",
            ));
        };
        let signature = native_environment_signature_for_context(
            context,
            self.primary_pid_for_live_pane_process(&turn.pane_id),
        )?;
        let signature_hash = signature.stable_hash();
        let identity =
            crate::security::sandbox::resolve_sandbox_identity(&config.group_whitelist, &signature)
                .map_err(|error| MezError::invalid_state(error.message()))?;
        for warning in &identity.mapping_warnings {
            self.append_sandbox_mapping_warning_once(
                &turn.pane_id,
                &format!(
                    "{}:{}:{}",
                    warning.mapping_kind, warning.configured_value, warning.reason
                ),
                &format!(
                    "{} `{}` ({})",
                    warning.mapping_kind, warning.configured_value, warning.reason
                ),
            )?;
        }
        let request = mez_agent::shell::PaneEnvironmentRequest::new(
            config.env_whitelist.requested_names.clone(),
        )
        .map_err(|error| MezError::invalid_args(error.message()))?;
        let evidence = if matches!(action.payload, AgentActionPayload::ApplyPatch { .. }) {
            PaneEnvironmentEvidence::restrictive(&request, "semantic_patch_not_forwarded")
        } else {
            super::native_workload_environment::server_environment_evidence(
                &request,
                self.server_environment(),
            )
        };
        let child_shell_path = program_dialect
            .interpreter_path()
            .unwrap_or(&signature.shell_path);
        let probe_plan = crate::security::sandbox::bubblewrap_capability_probe_plan_for_identity(
            config,
            child_shell_path,
            &identity,
            &evidence,
        )
        .map_err(|error| MezError::invalid_state(error.message()))?;
        let cache_key = crate::security::sandbox::bubblewrap_capability_cache_key(
            &turn.pane_id,
            &signature_hash,
            self.session.config_generation,
            &probe_plan,
        )
        .map_err(|error| MezError::invalid_state(error.message()))?;
        let (capability, capability_probe) = match self.bubblewrap_capability(&cache_key) {
            Some(capability) => (capability, None),
            None => (
                crate::security::sandbox::BubblewrapCapability {
                    cache_key: cache_key.clone(),
                },
                Some(NativeBubblewrapCapabilityProbe {
                    pane_id: turn.pane_id.clone(),
                    pane_environment_signature: signature_hash.clone(),
                    config_generation: self.session.config_generation,
                    plan: probe_plan,
                }),
            ),
        };
        let maximum_authority =
            self.native_bubblewrap_path_scopes_for_turn(turn, context, permission_evaluation)?;
        let trusted_project_root = self.native_trusted_project_root(context);
        let (managed_home, activity_lock) = match (
            self.integration.config_root(),
            trusted_project_root.as_ref(),
        ) {
            (Some(config_root), Some(project_root)) => {
                let (home, lock) =
                    crate::security::sandbox::prepare_bubblewrap_managed_home_for_workload_with_identity(
                        config_root,
                        project_root,
                        &identity,
                    )
                    .map_err(|error| MezError::invalid_state(error.message()))?;
                (Some(home), Some(lock))
            }
            _ => (None, None),
        };
        let launch_plan = crate::security::sandbox::compile_sandbox_launch_plan(
            crate::security::sandbox::SandboxCompileRequest::Bubblewrap(
                crate::security::sandbox::BubblewrapCompileRequest {
                    config,
                    identity,
                    capability,
                    pane_environment_signature: &signature_hash,
                    environment_evidence: &evidence,
                    network_policy: self.configured_permissions().resources.network_policy,
                    maximum_authority: &maximum_authority,
                    permission_evaluation,
                    preserve_maximum_authority: matches!(
                        action.payload,
                        AgentActionPayload::ApplyPatch { .. }
                    ),
                    child_shell_path,
                    command_file_host_path:
                        crate::security::sandbox::BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER,
                    managed_home: managed_home.as_ref(),
                    pane_home_directory: signature.home_directory.as_deref().map(Path::new),
                    stateful: false,
                    interactive: false,
                },
            ),
        )
        .map_err(|error| {
            MezError::invalid_state(format!(
                "native Bubblewrap dispatch could not compile the launch plan: {}",
                error.message()
            ))
        })?;
        let arguments = launch_plan
            .arguments
            .into_iter()
            .map(|argument| {
                if argument == crate::security::sandbox::BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER {
                    ShellChildArgument::MaterializedCommandFile
                } else {
                    ShellChildArgument::Literal(argument)
                }
            })
            .collect();
        let child_launch = ShellChildLaunch::new(launch_plan.executable, arguments)
            .map_err(|error| MezError::invalid_args(error.message()))?;
        let audit_summary = launch_plan.audit_summary;
        Ok(NativeBubblewrapDispatch {
            child_launch,
            audit_summary,
            capability_probe,
            activity_lease: activity_lock.map(NativeBubblewrapActivityLease::new),
        })
    }

    /// Resolves native filesystem authority directly from host metadata.
    ///
    /// Native provider preflight and Bubblewrap dispatch share this owner so
    /// neither path needs pane-shell environment or path-resolution
    /// transactions. `None` means the configured permissions and trusted
    /// project store grant no filesystem authority for the root-process
    /// working directory.
    pub(crate) fn native_path_scopes_for_pane_status_provider(
        &mut self,
        pane_id: &str,
        context: &NativeShellContext,
    ) -> Result<Option<PathScopes>> {
        self.refresh_project_trust_store_from_disk_if_changed()?;
        let resources = &self.configured_permissions().resources;
        let (read_scopes, write_scopes) =
            if !resources.read_scopes.is_empty() || !resources.write_scopes.is_empty() {
                (
                    resources.read_scopes.clone(),
                    resources.write_scopes.clone(),
                )
            } else if let Some(project_root) = self.native_trusted_project_root(context) {
                let project_root = project_root.to_string_lossy().into_owned();
                (vec![project_root.clone()], vec![project_root])
            } else {
                return Ok(None);
            };
        let primary = host_resolved_path_scopes(
            context.working_directory(),
            &read_scopes,
            &write_scopes,
            &[],
        )?;
        let agent_id = format!("agent-{pane_id}");
        let Some(scope) = self.subagent_scope_declaration(&agent_id) else {
            return Ok(Some(primary));
        };
        let child = host_resolved_path_scopes(
            Path::new(&scope.current_directory),
            &scope.read_scopes,
            &scope.write_scopes,
            &[],
        )?;
        let restricted = primary
            .intersection(&child)
            .map_err(|error| MezError::invalid_state(error.message()))?;
        let canonical_cwd = std::fs::canonicalize(context.working_directory()).map_err(|error| {
            MezError::invalid_state(format!(
                "pane status provider could not canonicalize its pane working directory: {error}"
            ))
        })?;
        if restricted.current_directory != canonical_cwd.to_string_lossy() {
            return Err(MezError::conflict(
                "pane status provider live working directory differs from delegated authority",
            ));
        }
        Ok(Some(restricted))
    }

    /// Resolves native filesystem authority directly from host metadata for
    /// one agent turn, preserving inherited subagent restrictions.
    ///
    /// `None` means the configured permissions and trusted project store grant
    /// no filesystem authority for the root-process working directory.
    pub(crate) fn native_path_scopes_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        context: &NativeShellContext,
    ) -> Result<Option<PathScopes>> {
        self.native_filesystem_scopes_for_turn(turn, context.working_directory())
    }

    /// Resolves maximum native Bubblewrap authority directly from host
    /// filesystem metadata and the pane root-process working directory.
    pub(crate) fn native_bubblewrap_maximum_path_scopes_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        context: &NativeShellContext,
    ) -> Result<PathScopes> {
        self.native_path_scopes_for_turn(turn, context)?
            .ok_or_else(|| {
                MezError::invalid_state(
                    "Sandbox filesystem authority is unavailable: configure permissions.read_scopes/write_scopes or trust the root-process working directory's project",
                )
            })
    }

    /// Resolves complete per-action effects from host filesystem metadata and
    /// combines them with the maximum native Bubblewrap authority.
    fn native_bubblewrap_path_scopes_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        context: &NativeShellContext,
        permission_evaluation: &PermissionEvaluation,
    ) -> Result<PathScopes> {
        let maximum = self.native_bubblewrap_maximum_path_scopes_for_turn(turn, context)?;
        let mut additional_paths = BTreeSet::new();
        if let Some(effects) = permission_evaluation.confinement_effects.as_ref() {
            additional_paths.extend(
                effects
                    .reads
                    .iter()
                    .chain(&effects.writes)
                    .chain(&effects.creates)
                    .chain(&effects.deletes)
                    .chain(&effects.touches)
                    .cloned(),
            );
        }
        host_resolved_path_scopes(
            Path::new(&maximum.current_directory),
            &maximum.read_scopes,
            &maximum.write_scopes,
            &additional_paths.into_iter().collect::<Vec<_>>(),
        )
    }

    /// Returns the deepest trusted project containing the root-process cwd.
    ///
    /// A deeper rejected or revoked stored decision withholds implicit native
    /// authority, so a broader trusted ancestor cannot restore it.
    fn native_trusted_project_root(&self, context: &NativeShellContext) -> Option<PathBuf> {
        let store = self.integration.project_trust_store()?;
        crate::security::project::resolve_project_trust_provenance(
            store,
            context.working_directory(),
        )
        .trusted_root()
        .map(Path::to_path_buf)
    }
}

/// Builds the pane-equivalent environment signature from root-process
/// credentials and the inferred native shell context.
fn native_environment_signature_for_context(
    context: &NativeShellContext,
    primary_pid: Option<u32>,
) -> Result<EnvironmentSignature> {
    let primary_pid = primary_pid.ok_or_else(|| {
        MezError::invalid_state("native Bubblewrap dispatch requires a live pane root process")
    })?;
    let credentials = mez_mux::process::process_credentials_for_pid(primary_pid).ok_or_else(|| {
        MezError::invalid_state(format!(
            "native Bubblewrap dispatch could not read root-process credentials for pid {primary_pid}"
        ))
    })?;
    let user_name = crate::security::sandbox::resolve_user_name(credentials.user_id)
        .unwrap_or_else(|_| credentials.user_id.to_string());
    let mut group_ids = credentials.supplementary_group_ids.clone();
    group_ids.push(credentials.primary_group_id);
    group_ids.sort_unstable();
    group_ids.dedup();
    let active_groups = group_ids
        .into_iter()
        .map(|id| EnvironmentGroup {
            id,
            name: crate::security::sandbox::resolve_group_name(id)
                .unwrap_or_else(|_| id.to_string()),
        })
        .collect::<Vec<_>>();
    let home_directory = context
        .environment()
        .iter()
        .find(|entry| entry.key.as_slice() == b"HOME")
        .and_then(|entry| std::str::from_utf8(&entry.value).ok())
        .map(ToString::to_string);
    let path = context
        .environment()
        .iter()
        .find(|entry| entry.key.as_slice() == b"PATH")
        .and_then(|entry| std::str::from_utf8(&entry.value).ok())
        .map(ToString::to_string);
    let signature = EnvironmentSignature::new(
        std::env::consts::OS,
        std::env::consts::ARCH,
        None,
        native_host_name(),
        user_name,
        home_directory,
        context.shell_path().to_string_lossy().into_owned(),
        context.classification(),
        None,
        path,
        context.working_directory().to_string_lossy().into_owned(),
        None,
        false,
        None,
        Vec::new(),
    )
    .map_err(|error| MezError::invalid_args(error.message()))?
    .with_process_identity(
        credentials.user_id,
        credentials.primary_group_id,
        active_groups,
    )
    .map_err(|error| MezError::invalid_args(error.message()))?;
    Ok(signature)
}

/// Resolves the required pane identity for one admitted pane-status provider.
///
/// The shared native workload builder owns the requirement: the identity is
/// validated as workload-visible evidence, and a missing or malformed identity
/// becomes a typed pre-dispatch error naming the `pane_identity` category and
/// the `MEZ_PANE_ID` key before any payload environment or sandbox plan is
/// compiled.
fn provider_pane_identity(context: &NativeShellContext, pane_id: &str) -> Result<String> {
    let environment = context
        .workload_environment()
        .with_required_workload_value(
            super::native_workload_environment::NativeLaunchEnvironmentRequirement::PANE_IDENTITY,
            pane_id,
        )?;
    environment
        .workload_value("MEZ_PANE_ID")
        .map(ToString::to_string)
        .ok_or_else(|| {
            MezError::invalid_state(
                "native pane status provider launch is missing its pane identity",
            )
        })
}

/// Runs the deterministic Bubblewrap capability probe as a host process.
fn run_native_bubblewrap_capability_probe(
    pane_id: &str,
    pane_environment_signature: &str,
    config_generation: u64,
    probe_plan: &crate::security::sandbox::BubblewrapCapabilityProbePlan,
    cancellation: Option<&AtomicBool>,
    launches: &super::launch_accounting::NativeLaunchLedger,
) -> Result<crate::security::sandbox::BubblewrapCapability> {
    let status_sink = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .map_err(|error| {
            MezError::invalid_state(crate::security::sandbox::bubblewrap_failure_remediation(
                &format!(
                    "native Bubblewrap capability probe could not open its status sink: {error}"
                ),
            ))
        })?;
    let status_fd = status_sink.as_raw_fd();
    let mut command = Command::new(&probe_plan.executable);
    command
        .args(&probe_plan.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .env_clear();
    // A capability-probe launcher runs from a cleared base and keeps only the
    // declared launcher command-search path, so an ambient credential or a
    // loader variable such as LD_PRELOAD or DYLD_INSERT_LIBRARIES cannot enter
    // the probe launcher. The probe payload environment stays owned by the
    // compiled proof plan.
    for entry in super::native_workload_environment::launcher_control_environment(
        &super::native_workload_environment::native_ambient_environment(),
    ) {
        command.env(
            OsStr::from_bytes(&entry.key),
            OsStr::from_bytes(&entry.value),
        );
    }
    // SAFETY: the hook only duplicates the still-live status sink and clears
    // close-on-exec on descriptor 3 before the probe executable starts.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(
                status_fd,
                i32::from(crate::security::sandbox::BUBBLEWRAP_STATUS_FD),
            ) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(
                i32::from(crate::security::sandbox::BUBBLEWRAP_STATUS_FD),
                libc::F_SETFD,
                0,
            ) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = launches.launch(
        super::launch_accounting::NativeLaunchReason::SandboxProbe,
        || {
            if cancellation.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Err(MezError::conflict(
                    "native Bubblewrap probe was cancelled before launch",
                ));
            }
            command.spawn().map_err(|error| {
                MezError::invalid_state(crate::security::sandbox::bubblewrap_failure_remediation(
                    &format!("native Bubblewrap capability probe could not start: {error}"),
                ))
            })
        },
    )?;
    let stdout_reader = child.stdout.take().map(spawn_bounded_probe_reader);
    let stderr_reader = child.stderr.take().map(spawn_bounded_probe_reader);
    let deadline = Instant::now() + NATIVE_BUBBLEWRAP_PROBE_TIMEOUT;
    let status = loop {
        if cancellation.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            terminate_probe_group(&child);
            let _ = child.kill();
            let _ = child.wait();
            return Err(MezError::conflict(
                "native Bubblewrap capability probe was cancelled",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                terminate_probe_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(MezError::invalid_state(
                    crate::security::sandbox::bubblewrap_failure_remediation(
                        "native Bubblewrap capability probe exceeded its time budget",
                    ),
                ));
            }
            Ok(None) => std::thread::sleep(NATIVE_BUBBLEWRAP_PROBE_POLL_INTERVAL),
            Err(error) => {
                terminate_probe_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(MezError::invalid_state(
                    crate::security::sandbox::bubblewrap_failure_remediation(&format!(
                        "native Bubblewrap capability probe wait failed: {error}"
                    )),
                ));
            }
        }
    };
    let stdout = join_bounded_probe_reader(stdout_reader);
    let stderr = join_bounded_probe_reader(stderr_reader);
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let exit_code = status.code().unwrap_or(-1);
    crate::security::sandbox::parse_bubblewrap_capability_probe(
        pane_id,
        pane_environment_signature,
        config_generation,
        probe_plan,
        exit_code,
        &stdout,
    )
    .map_err(|error| {
        let output_diagnostic = if !stderr.is_empty() {
            format!(
                "stderr: {}",
                native_bubblewrap_probe_output_preview(&stderr)
            )
        } else if !stdout.is_empty() {
            format!(
                "unexpected stdout: {}",
                native_bubblewrap_probe_output_preview(stdout.as_bytes())
            )
        } else {
            "no diagnostic output".to_string()
        };
        MezError::invalid_state(crate::security::sandbox::bubblewrap_failure_remediation(
            &format!(
                "native Bubblewrap capability probe failed: {} (exit code {}; {})",
                error.message(),
                exit_code,
                output_diagnostic
            ),
        ))
    })
}

/// Runs the deterministic Seatbelt capability probe as a native host process.
fn run_native_seatbelt_capability_probe(
    pane_id: &str,
    pane_environment_signature: &str,
    config_generation: u64,
    probe_plan: &crate::security::sandbox::SeatbeltCapabilityProbePlan,
    cancellation: Option<&AtomicBool>,
    launches: &super::launch_accounting::NativeLaunchLedger,
) -> Result<crate::security::sandbox::SeatbeltCapability> {
    let mut command = Command::new(&probe_plan.executable);
    command
        .args(&probe_plan.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .env_clear();
    // A capability-probe launcher runs from a cleared base and keeps only the
    // declared launcher command-search path, so an ambient credential or a
    // loader variable such as LD_PRELOAD or DYLD_INSERT_LIBRARIES cannot enter
    // the probe launcher. The probe payload environment stays owned by the
    // compiled proof plan.
    for entry in super::native_workload_environment::launcher_control_environment(
        &super::native_workload_environment::native_ambient_environment(),
    ) {
        command.env(
            OsStr::from_bytes(&entry.key),
            OsStr::from_bytes(&entry.value),
        );
    }
    let mut child = launches.launch(
        super::launch_accounting::NativeLaunchReason::SandboxProbe,
        || {
            if cancellation.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Err(MezError::conflict(
                    "native Seatbelt probe was cancelled before launch",
                ));
            }
            command.spawn().map_err(|error| {
                MezError::invalid_state(format!(
                    "native Seatbelt capability probe could not start: {error}"
                ))
            })
        },
    )?;
    let stdout_reader = child.stdout.take().map(spawn_bounded_probe_reader);
    let stderr_reader = child.stderr.take().map(spawn_bounded_probe_reader);
    let deadline = Instant::now() + NATIVE_BUBBLEWRAP_PROBE_TIMEOUT;
    let status = loop {
        if cancellation.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            terminate_probe_group(&child);
            let _ = child.kill();
            let _ = child.wait();
            return Err(MezError::conflict(
                "native Seatbelt capability probe was cancelled",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                terminate_probe_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(MezError::invalid_state(
                    "native Seatbelt capability probe exceeded its time budget",
                ));
            }
            Ok(None) => std::thread::sleep(NATIVE_BUBBLEWRAP_PROBE_POLL_INTERVAL),
            Err(error) => {
                terminate_probe_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(MezError::invalid_state(format!(
                    "native Seatbelt capability probe wait failed: {error}"
                )));
            }
        }
    };
    let stdout = join_bounded_probe_reader(stdout_reader);
    let stderr = join_bounded_probe_reader(stderr_reader);
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let exit_code = status.code().unwrap_or(-1);
    crate::security::sandbox::parse_seatbelt_capability_probe(
        pane_id,
        pane_environment_signature,
        config_generation,
        probe_plan,
        exit_code,
        &stdout,
    )
    .map_err(|error| {
        let output_diagnostic = if !stderr.is_empty() {
            format!(
                "stderr: {}",
                native_bubblewrap_probe_output_preview(&stderr)
            )
        } else if !stdout.is_empty() {
            format!(
                "unexpected stdout: {}",
                native_bubblewrap_probe_output_preview(stdout.as_bytes())
            )
        } else {
            "no diagnostic output".to_string()
        };
        MezError::invalid_state(format!(
            "native Seatbelt capability probe failed: {} (exit code {}; {})",
            error.message(),
            exit_code,
            output_diagnostic
        ))
    })
}

/// Escapes one bounded probe-output prefix for safe inline diagnostics.
fn native_bubblewrap_probe_output_preview(output: &[u8]) -> String {
    let output = String::from_utf8_lossy(output);
    let mut preview = String::new();
    for character in output.chars() {
        let escaped = character.escape_default().to_string();
        if preview.len().saturating_add(escaped.len())
            > NATIVE_BUBBLEWRAP_PROBE_DIAGNOSTIC_PREVIEW_BYTES
        {
            break;
        }
        preview.push_str(&escaped);
    }
    preview
}

/// Drains one probe pipe concurrently so a noisy child cannot fill its pipe
/// and stall until the timeout, while retaining only a bounded diagnostic
/// prefix. The reader continues draining after the bound so it never causes a
/// valid child to receive `SIGPIPE` merely for producing extra diagnostics.
fn spawn_bounded_probe_reader<R>(pipe: R) -> ProbeReader
where
    R: Read + std::os::fd::AsFd + Send + 'static,
{
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation = Arc::clone(&cancelled);
    let worker = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut pipe = pipe;
        let Ok(flags) = rustix::fs::fcntl_getfl(&pipe) else {
            return output;
        };
        if rustix::fs::fcntl_setfl(&pipe, flags | rustix::fs::OFlags::NONBLOCK).is_err() {
            return output;
        }
        let mut buffer = [0_u8; 4096];
        loop {
            if cancellation.load(Ordering::SeqCst) {
                break;
            }
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let remaining =
                        NATIVE_BUBBLEWRAP_PROBE_OUTPUT_LIMIT_BYTES.saturating_sub(output.len());
                    output.extend_from_slice(&buffer[..count.min(remaining)]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(NATIVE_BUBBLEWRAP_PROBE_POLL_INTERVAL);
                }
                Err(_) => break,
            }
        }
        output
    });
    ProbeReader {
        cancelled,
        worker: Some(worker),
    }
}

/// Owns one probe pipe reader; all exit paths cancel and join it.
struct ProbeReader {
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<Vec<u8>>>,
}

impl Drop for ProbeReader {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Requests termination of descendants in the probe's private process group.
fn terminate_probe_group(child: &std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: probes use process_group(0), so this group belongs to the child.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

/// Joins one bounded probe reader without letting reader failure obscure the
/// primary capability-probe result.
fn join_bounded_probe_reader(reader: Option<ProbeReader>) -> Vec<u8> {
    let Some(mut reader) = reader else {
        return Vec::new();
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    while reader
        .worker
        .as_ref()
        .is_some_and(|worker| !worker.is_finished())
        && Instant::now() < deadline
    {
        std::thread::sleep(NATIVE_BUBBLEWRAP_PROBE_POLL_INTERVAL);
    }
    reader.cancelled.store(true, Ordering::SeqCst);
    reader
        .worker
        .take()
        .and_then(|worker| worker.join().ok())
        .unwrap_or_default()
}

/// Returns the host name reported by the kernel, or `unknown` on failure.
fn native_host_name() -> String {
    let mut buffer = [0_u8; 256];
    // SAFETY: the buffer is writable for the call duration and the result is
    // bounded and NUL-terminated below.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return "unknown".to_string();
    }
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..length]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mez_agent::permissions::PathResolutionStatus;

    /// Verifies native Seatbelt evidence retains a safe forwarded PATH while
    /// filtering the shared default HOME and SHELL names before compilation.
    #[test]
    fn native_seatbelt_evidence_uses_server_values_without_name_filtering() {
        let request = mez_agent::shell::PaneEnvironmentRequest::new(vec![
            "PATH".to_string(),
            "HOME".to_string(),
            "SHELL".to_string(),
        ])
        .unwrap();
        let evidence = super::super::native_workload_environment::server_environment_evidence(
            &request,
            &[
                RawEnvironmentEntry {
                    key: b"PATH".to_vec(),
                    value: b"/server/bin".to_vec(),
                },
                RawEnvironmentEntry {
                    key: b"HOME".to_vec(),
                    value: b"/server/home".to_vec(),
                },
                RawEnvironmentEntry {
                    key: b"SHELL".to_vec(),
                    value: b"/bin/zsh".to_vec(),
                },
            ],
        );

        assert_eq!(
            evidence.values.get("PATH").map(String::as_str),
            Some("/server/bin")
        );
        assert_eq!(
            evidence.values.get("HOME").map(String::as_str),
            Some("/server/home")
        );
        assert_eq!(
            evidence.values.get("SHELL").map(String::as_str),
            Some("/bin/zsh")
        );
    }

    /// Verifies one admitted pane-status provider fails closed through the real
    /// required-evidence path before any sandbox plan or child process exists.
    ///
    /// `compile_pane_status_provider_launch` resolves the required `MEZ_PANE_ID`
    /// value through `provider_pane_identity` before it builds
    /// `PaneEnvironmentEvidence`, compiles a probe or launch plan, or spawns
    /// anything, so the `Err` asserted here proves no plan and no child was
    /// created. CI cannot run an admitted sandboxed provider launch to observe
    /// that absence directly, so this test instead asserts the typed
    /// pre-dispatch error, that the failure happens before dispatch, and that the
    /// rejected context still carries no pane identity evidence a plan could
    /// consume.
    #[test]
    fn provider_pane_identity_fails_closed_before_any_plan_or_child_exists() {
        let context = crate::runtime::processes::NativeShellContext::for_test(
            PathBuf::from("/bin/sh"),
            Vec::new(),
            std::env::temp_dir(),
        );
        assert!(
            !context
                .environment()
                .iter()
                .any(|entry| entry.key.as_slice() == b"MEZ_PANE_ID"),
            "the fixture must model a pane root that carries no MEZ_PANE_ID evidence"
        );

        let missing_evidence =
            super::super::native_workload_environment::NativeWorkloadEnvironmentBuilder::from_environment(
                context.workload_environment(),
            )
            .with_requirement(
                super::super::native_workload_environment::NativeLaunchEnvironmentRequirement::PANE_IDENTITY,
                None,
            )
            .expect_err("a missing MEZ_PANE_ID evidence value must fail closed");
        assert!(matches!(
            missing_evidence.kind(),
            crate::error::MezErrorKind::InvalidState
        ));
        assert!(missing_evidence.to_string().contains("pane_identity"));
        assert!(missing_evidence.to_string().contains("MEZ_PANE_ID"));
        assert!(missing_evidence.to_string().contains("before dispatch"));

        let missing_identity = provider_pane_identity(&context, "")
            .expect_err("a missing runtime pane identity must fail closed");
        assert!(matches!(
            missing_identity.kind(),
            crate::error::MezErrorKind::InvalidState
        ));
        assert!(missing_identity.to_string().contains("pane_identity"));
        assert!(missing_identity.to_string().contains("MEZ_PANE_ID"));
        assert!(missing_identity.to_string().contains("before dispatch"));

        let malformed_identity = provider_pane_identity(&context, "%1\n")
            .expect_err("a malformed runtime pane identity must fail closed");
        assert!(matches!(
            malformed_identity.kind(),
            crate::error::MezErrorKind::InvalidState
        ));
        assert!(malformed_identity.to_string().contains("MEZ_PANE_ID"));

        assert!(
            context
                .workload_environment()
                .workload_value("MEZ_PANE_ID")
                .is_none(),
            "a rejected launch must not write substituted pane identity evidence"
        );
        assert_eq!(
            provider_pane_identity(&context, "%1").expect("a live pane identity is accepted"),
            "%1"
        );
    }

    /// Verifies host resolution canonicalizes existing read authority and
    /// preserves nearest-parent evidence for a write target that does not yet
    /// exist, allowing native Bubblewrap to mount the parent without invoking
    /// a pane-shell resolver.
    #[test]
    fn native_host_path_resolution_preserves_existing_and_create_target_evidence() {
        let root = std::env::temp_dir().join(format!(
            "mez-native-host-paths-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let existing = root.join("existing");
        std::fs::create_dir_all(&existing).unwrap();

        let scopes = host_resolved_path_scopes(
            &root,
            &["existing".to_string()],
            &["generated/output.txt".to_string()],
            &[],
        )
        .unwrap();
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let create = &scopes.path_evidence["generated/output.txt"];

        assert_eq!(scopes.resolution_status, PathResolutionStatus::HostResolved);
        assert_eq!(create.kind, ResolvedPathKind::CreateTarget);
        assert_eq!(
            create.nearest_existing_parent,
            canonical_root.to_string_lossy()
        );
        assert_eq!(
            create.canonical_path,
            canonical_root
                .join("generated/output.txt")
                .to_string_lossy()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A parent component following a symlink must refer to the symlink
    /// target's parent, not the lexical parent of the link's spelling. Scope
    /// evidence must agree with kernel resolution for existing and new paths.
    #[test]
    fn native_host_resolution_expands_symlinks_before_parent_components() {
        let root = std::env::temp_dir().join(format!(
            "mez-native-link-parent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("physical/child")).unwrap();
        std::fs::write(root.join("physical/file"), b"physical").unwrap();
        std::fs::write(root.join("file"), b"lexical").unwrap();
        std::os::unix::fs::symlink("physical/child", root.join("link")).unwrap();
        let physical = std::fs::canonicalize(root.join("physical")).unwrap();
        let existing = resolve_host_path(&root, "link/../file").unwrap();
        let create = resolve_host_path(&root, "link/../new").unwrap();
        assert_eq!(
            existing.canonical_path,
            physical.join("file").to_string_lossy()
        );
        assert_eq!(
            create.canonical_path,
            physical.join("new").to_string_lossy()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Verifies a missing configured read scope fails closed because a
    /// non-existent read target cannot be represented as trusted sandbox
    /// authority.
    #[test]
    fn native_host_path_resolution_rejects_missing_read_scope() {
        let root = std::env::temp_dir().join(format!(
            "mez-native-host-read-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let error =
            host_resolved_path_scopes(&root, &["missing".to_string()], &[], &[]).unwrap_err();

        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
        assert_eq!(
            error.message(),
            "sandbox read scope does not exist: missing"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Verifies pane-provider lifecycle cancellation kills and reaps an
    /// in-flight Bubblewrap capability probe without waiting for its timeout.
    #[test]
    fn native_bubblewrap_probe_honors_provider_cancellation() {
        let probe = NativeBubblewrapCapabilityProbe::for_test(
            "/bin/sh",
            vec!["-c".to_string(), "sleep 30".to_string()],
            "mez-native-probe-ok",
        );
        let cancellation = AtomicBool::new(true);
        let started = Instant::now();

        let error = probe.run_with_cancellation(&cancellation).unwrap_err();

        assert!(error.to_string().contains("was cancelled"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Verifies pane-provider lifecycle cancellation kills and reaps an
    /// in-flight Seatbelt capability probe without waiting for its timeout.
    #[test]
    fn native_seatbelt_probe_honors_provider_cancellation() {
        let probe = NativeSeatbeltCapabilityProbe::for_test(
            "/bin/sh",
            vec!["-c".to_string(), "sleep 30".to_string()],
            "mez-native-seatbelt-ok",
        );
        let cancellation = AtomicBool::new(true);
        let started = Instant::now();

        let error = probe.run_with_cancellation(&cancellation).unwrap_err();

        assert!(error.to_string().contains("was cancelled"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Both probe adapters must cancel after startup, kill a delayed-effect
    /// descendant in their private group, and join readers whose pipes remain
    /// inherited. Pre-cancelled fixtures cannot establish these guarantees.
    #[test]
    fn native_probe_inflight_cancellation_stops_descendants_and_readers() {
        for seatbelt in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "mez-probe-inflight-{}-{}-{seatbelt}",
                std::process::id(),
                crate::runtime::current_unix_millis()
            ));
            std::fs::create_dir(&root).unwrap();
            let ready = root.join("ready");
            let effect = root.join("effect");
            let command = format!(
                "(sleep 1; printf forbidden > '{}') & printf ready > '{}'; wait",
                effect.display(),
                ready.display()
            );
            let args = vec!["-c".to_string(), command];
            let probe = if seatbelt {
                NativeSandboxCapabilityProbe::Seatbelt(NativeSeatbeltCapabilityProbe::for_test(
                    "/bin/sh", args, "probe-ok",
                ))
            } else {
                NativeSandboxCapabilityProbe::Bubblewrap(NativeBubblewrapCapabilityProbe::for_test(
                    "/bin/sh", args, "probe-ok",
                ))
            };
            let cancellation = Arc::new(AtomicBool::new(false));
            let worker_flag = Arc::clone(&cancellation);
            let worker = std::thread::spawn(move || probe.run_with_cancellation(&worker_flag));
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            cancellation.store(true, Ordering::SeqCst);
            let error = worker.join().unwrap().unwrap_err();
            let reached_ready = ready.exists();
            std::thread::sleep(Duration::from_millis(1100));
            let performed_effect = effect.exists();
            std::fs::remove_dir_all(root).unwrap();
            assert!(reached_ready);
            assert!(error.to_string().contains("was cancelled"));
            assert!(!performed_effect, "cancelled probe descendant survived");
        }
    }
}
