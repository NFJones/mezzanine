//! Pure effective sandbox-boundary resolution for status, audit, and results.
//!
//! This module owns the single typed answer to "what execution boundary is
//! actually in force?" for one configured sandbox, approval policy, execution
//! host, and set of execution evidence. It performs no probing, no policy
//! mutation, and no path discovery: callers pass evidence they already hold and
//! receive one closed projection whose stable spellings never include backend
//! arguments, environment values, or probe output.
//!
//! Reporting rules are deliberately fail closed:
//!
//! - an unattested foreign pane reports `remote-unattested` and never inherits
//!   the configured backend;
//! - a primary-user host-access policy reports `host-bypass`, no enforcement,
//!   and an unenforced network mode;
//! - an approved one-shot unsandboxed retry reports `policy-only`;
//! - a configured backend whose fixed executable is missing reports
//!   `unavailable`;
//! - a configured backend without an exact capability proof and a compiled
//!   launch plan reports no enforcement and an unknown network mode;
//! - only a compiled plan plus an exact capability proof for the same backend
//!   may report `isolated` or `connected`; the network mode always comes from
//!   the compiled plan that `effective_sandbox_policy_for_authority` produced
//!   and is never re-derived from `NetworkPolicy` alone.

use std::path::Path;

use mez_agent::ApprovalPolicy;

use crate::runtime::{SandboxBackend, SandboxConfig, SandboxNetworkMode};

use super::{SandboxAuditSummary, SandboxCapabilityCacheKey, sandbox_executable_available};

/// Closed execution boundary reported for one sandbox decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxEffectiveBoundary {
    /// Bubblewrap namespace confinement is the selected boundary.
    Bubblewrap,
    /// Seatbelt operation-level confinement is the selected boundary.
    Seatbelt,
    /// Policy-only execution; no operating-system confinement is claimed.
    PolicyOnly,
    /// Primary-user host access deliberately bypasses the configured sandbox.
    HostBypass,
    /// A foreign or unattested pane shell the configured backend cannot cover.
    RemoteUnattested,
    /// The selected backend executable is unavailable, so no workload starts.
    Unavailable,
}

impl SandboxEffectiveBoundary {
    /// Returns the fixed status spelling for this boundary.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Bubblewrap => "bubblewrap",
            Self::Seatbelt => "seatbelt",
            Self::PolicyOnly => "policy-only",
            Self::HostBypass => "host-bypass",
            Self::RemoteUnattested => "remote-unattested",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Closed enforcement mechanism backing one reported boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxEnforcement {
    /// Bubblewrap enforces an isolated per-action network namespace.
    BubblewrapNetworkNamespace,
    /// Bubblewrap runs the authorized connected-host-network profile.
    BubblewrapConnectedProfile,
    /// Seatbelt enforces an operation-level socket policy.
    SeatbeltSocketPolicy,
    /// Seatbelt authorizes host networking through operation grants.
    SeatbeltAuthorizedHostNetwork,
    /// No operating-system enforcement mechanism is in force.
    None,
}

impl SandboxEnforcement {
    /// Returns the fixed status spelling for this enforcement mechanism.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::BubblewrapNetworkNamespace => "bubblewrap-network-namespace",
            Self::BubblewrapConnectedProfile => "bubblewrap-connected-profile",
            Self::SeatbeltSocketPolicy => "seatbelt-socket-policy",
            Self::SeatbeltAuthorizedHostNetwork => "seatbelt-authorized-host-network",
            Self::None => "none",
        }
    }
}

/// Closed effective backend-neutral network mode for one boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxEffectiveNetworkMode {
    /// An authorized workload may reach the host network through the backend.
    Connected,
    /// The backend denies host network access for the workload.
    Isolated,
    /// Policy-only, host-access, or approved-bypass execution is not enforced.
    Unenforced,
    /// No compiled plan or probe proves what the workload receives.
    Unknown,
}

impl SandboxEffectiveNetworkMode {
    /// Returns the fixed status spelling for this network mode.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Isolated => "isolated",
            Self::Unenforced => "unenforced",
            Self::Unknown => "unknown",
        }
    }
}

/// Closed reason explaining why one boundary projection was reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxEffectiveReason {
    /// A compiled plan and matching capability proof verified this boundary.
    Verified,
    /// The backend applies, but no probe or compiled-plan evidence exists yet.
    NotProbed,
    /// The configured backend executable is unavailable.
    BackendUnavailable,
    /// Policy-only execution is configured or was selected.
    PolicyOnly,
    /// Primary-user host access bypasses the configured sandbox.
    HostAccessBypass,
    /// An approved one-shot unsandboxed retry bypassed the backend.
    SandboxBypassApproved,
    /// A foreign or unattested pane shell cannot carry the configured backend.
    RemoteShellUnattested,
    /// The retained structured effects were incomplete, so coverage is unproven.
    EffectsIncomplete,
}

impl SandboxEffectiveReason {
    /// Returns the fixed status spelling for this reason.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::NotProbed => "not-probed",
            Self::BackendUnavailable => "backend-unavailable",
            Self::PolicyOnly => "policy-only",
            Self::HostAccessBypass => "host-access-bypass",
            Self::SandboxBypassApproved => "sandbox-bypass-approved",
            Self::RemoteShellUnattested => "remote-shell-unattested",
            Self::EffectsIncomplete => "effects-incomplete",
        }
    }
}

/// Execution host whose shell boundary is being resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxExecutionHost {
    /// A live pane shell attested by the runtime.
    Pane,
    /// A native worker transport attested by the runtime.
    Native,
    /// A foreign or unattested pane shell; never inherits the backend.
    Unattested,
}

/// Exact probe and compiled-plan evidence supplied by one caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SandboxEffectiveEvidence<'a> {
    /// Exact successful capability-cache identity for this environment.
    pub(crate) capability: Option<&'a SandboxCapabilityCacheKey>,
    /// Redacted facts from a compiled launch plan.
    pub(crate) plan: Option<&'a SandboxAuditSummary>,
    /// Whether the retained structured permission effects were complete.
    pub(crate) effects_complete: bool,
    /// Whether an approved one-shot unsandboxed retry is active for this action.
    pub(crate) sandbox_bypass_approved: bool,
}

impl<'a> SandboxEffectiveEvidence<'a> {
    /// Returns evidence containing no probe or compiled-plan proof.
    pub(crate) const fn none() -> Self {
        Self {
            capability: None,
            plan: None,
            effects_complete: false,
            sandbox_bypass_approved: false,
        }
    }
}

/// Complete closed projection of one effective sandbox decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectiveSandboxState {
    /// Configured backend intent before approval or host selection.
    pub(crate) configured_intent: &'static str,
    /// Configured operating-system backend, when one was selected.
    pub(crate) selected_backend: Option<SandboxBackend>,
    /// Effective execution boundary actually in force.
    pub(crate) effective_boundary: SandboxEffectiveBoundary,
    /// Enforcement mechanism actually backing that boundary.
    pub(crate) enforcement: SandboxEnforcement,
    /// Effective backend-neutral network mode for that boundary.
    pub(crate) network_mode: SandboxEffectiveNetworkMode,
    /// Closed reason explaining the projection.
    pub(crate) reason: SandboxEffectiveReason,
}

impl EffectiveSandboxState {
    /// Builds one state from already-resolved parts.
    fn resolved(
        sandbox: &SandboxConfig,
        effective_boundary: SandboxEffectiveBoundary,
        enforcement: SandboxEnforcement,
        network_mode: SandboxEffectiveNetworkMode,
        reason: SandboxEffectiveReason,
    ) -> Self {
        Self {
            configured_intent: sandbox.as_str(),
            selected_backend: sandbox.backend(),
            effective_boundary,
            enforcement,
            network_mode,
            reason,
        }
    }

    /// Returns the fixed boundary spelling for this state.
    pub(crate) const fn boundary_str(&self) -> &'static str {
        self.effective_boundary.as_str()
    }

    /// Returns the fixed enforcement spelling for this state.
    pub(crate) const fn enforcement_str(&self) -> &'static str {
        self.enforcement.as_str()
    }

    /// Returns the fixed network-mode spelling for this state.
    pub(crate) const fn network_mode_str(&self) -> &'static str {
        self.network_mode.as_str()
    }

    /// Returns the fixed reason spelling for this state.
    pub(crate) const fn reason_str(&self) -> &'static str {
        self.reason.as_str()
    }

    /// Returns the backend-name audit value permitted for this state.
    ///
    /// A configured backend name is written only when the projected boundary
    /// and the compiled plan agree. Policy-only and host access report their
    /// own fixed spellings, and an unproven configured backend reports no
    /// backend name at all instead of falling back to configuration.
    pub(crate) fn audit_backend_name(
        &self,
        plan: Option<&SandboxAuditSummary>,
    ) -> Option<&'static str> {
        match (&self.effective_boundary, plan) {
            (SandboxEffectiveBoundary::Bubblewrap, Some(plan))
                if plan.backend == SandboxBackend::Bubblewrap =>
            {
                Some(SandboxBackend::Bubblewrap.as_str())
            }
            (SandboxEffectiveBoundary::Seatbelt, Some(plan))
                if plan.backend == SandboxBackend::Seatbelt =>
            {
                Some(SandboxBackend::Seatbelt.as_str())
            }
            (SandboxEffectiveBoundary::PolicyOnly, _) => Some("policy-only"),
            (SandboxEffectiveBoundary::HostBypass, _) => Some("host-bypass"),
            _ => None,
        }
    }

    /// Returns the bounded fixed-key JSON projection for shell action results.
    ///
    /// The document contains only the closed boundary, enforcement, network
    /// mode, and reason spellings; it never includes argv, paths, environment
    /// values, or probe output.
    pub(crate) fn structured_json(&self) -> serde_json::Value {
        serde_json::json!({
            "execution_boundary": self.boundary_str(),
            "enforcement": self.enforcement_str(),
            "network_mode": self.network_mode_str(),
            "reason": self.reason_str(),
        })
    }
}

/// Resolves the one effective sandbox boundary from existing owners only.
///
/// The function is pure: it inspects only the configured executable path, the
/// supplied approval policy, the execution host, and the caller's exact
/// evidence. Precedence is fixed so every surface reports the same answer:
/// an unattested pane, then host-access bypass, then an approved one-shot
/// unsandboxed retry, then policy-only, then backend availability, then probe
/// and plan verification.
pub(crate) fn resolve_effective_sandbox(
    sandbox: &SandboxConfig,
    approval_policy: ApprovalPolicy,
    host: SandboxExecutionHost,
    evidence: SandboxEffectiveEvidence<'_>,
) -> EffectiveSandboxState {
    if matches!(host, SandboxExecutionHost::Unattested) {
        return EffectiveSandboxState::resolved(
            sandbox,
            SandboxEffectiveBoundary::RemoteUnattested,
            SandboxEnforcement::None,
            SandboxEffectiveNetworkMode::Unknown,
            SandboxEffectiveReason::RemoteShellUnattested,
        );
    }
    if approval_policy.bypasses_sandbox() {
        return EffectiveSandboxState::resolved(
            sandbox,
            SandboxEffectiveBoundary::HostBypass,
            SandboxEnforcement::None,
            SandboxEffectiveNetworkMode::Unenforced,
            SandboxEffectiveReason::HostAccessBypass,
        );
    }
    if evidence.sandbox_bypass_approved {
        return EffectiveSandboxState::resolved(
            sandbox,
            SandboxEffectiveBoundary::PolicyOnly,
            SandboxEnforcement::None,
            SandboxEffectiveNetworkMode::Unenforced,
            SandboxEffectiveReason::SandboxBypassApproved,
        );
    }
    let Some(backend) = sandbox.backend() else {
        return EffectiveSandboxState::resolved(
            sandbox,
            SandboxEffectiveBoundary::PolicyOnly,
            SandboxEnforcement::None,
            SandboxEffectiveNetworkMode::Unenforced,
            SandboxEffectiveReason::PolicyOnly,
        );
    };
    let Some(executable) = configured_executable(sandbox) else {
        return EffectiveSandboxState::resolved(
            sandbox,
            SandboxEffectiveBoundary::Unavailable,
            SandboxEnforcement::None,
            SandboxEffectiveNetworkMode::Unknown,
            SandboxEffectiveReason::BackendUnavailable,
        );
    };
    let boundary = boundary_for_backend(backend);
    if !sandbox_executable_available(Path::new(executable)) {
        return EffectiveSandboxState::resolved(
            sandbox,
            SandboxEffectiveBoundary::Unavailable,
            SandboxEnforcement::None,
            SandboxEffectiveNetworkMode::Unknown,
            SandboxEffectiveReason::BackendUnavailable,
        );
    }
    let Some(plan) = evidence.plan.filter(|plan| plan.backend == backend) else {
        return EffectiveSandboxState::resolved(
            sandbox,
            boundary,
            SandboxEnforcement::None,
            SandboxEffectiveNetworkMode::Unknown,
            SandboxEffectiveReason::NotProbed,
        );
    };
    let capability_proven = evidence
        .capability
        .is_some_and(|capability| capability_matches(capability, backend, executable, plan));
    if !capability_proven {
        return EffectiveSandboxState::resolved(
            sandbox,
            boundary,
            SandboxEnforcement::None,
            SandboxEffectiveNetworkMode::Unknown,
            SandboxEffectiveReason::NotProbed,
        );
    }
    if !evidence.effects_complete {
        return EffectiveSandboxState::resolved(
            sandbox,
            boundary,
            SandboxEnforcement::None,
            SandboxEffectiveNetworkMode::Unknown,
            SandboxEffectiveReason::EffectsIncomplete,
        );
    }
    let (enforcement, network_mode) = match (backend, plan.network) {
        (SandboxBackend::Bubblewrap, SandboxNetworkMode::Isolated) => (
            SandboxEnforcement::BubblewrapNetworkNamespace,
            SandboxEffectiveNetworkMode::Isolated,
        ),
        (SandboxBackend::Bubblewrap, SandboxNetworkMode::Connected) => (
            SandboxEnforcement::BubblewrapConnectedProfile,
            SandboxEffectiveNetworkMode::Connected,
        ),
        (SandboxBackend::Seatbelt, SandboxNetworkMode::Isolated) => (
            SandboxEnforcement::SeatbeltSocketPolicy,
            SandboxEffectiveNetworkMode::Isolated,
        ),
        (SandboxBackend::Seatbelt, SandboxNetworkMode::Connected) => (
            SandboxEnforcement::SeatbeltAuthorizedHostNetwork,
            SandboxEffectiveNetworkMode::Connected,
        ),
    };
    EffectiveSandboxState::resolved(
        sandbox,
        boundary,
        enforcement,
        network_mode,
        SandboxEffectiveReason::Verified,
    )
}

/// Returns the closed boundary spelling for one selected backend.
fn boundary_for_backend(backend: SandboxBackend) -> SandboxEffectiveBoundary {
    match backend {
        SandboxBackend::Bubblewrap => SandboxEffectiveBoundary::Bubblewrap,
        SandboxBackend::Seatbelt => SandboxEffectiveBoundary::Seatbelt,
    }
}

/// Returns the configured fixed executable path for one selected backend.
fn configured_executable(sandbox: &SandboxConfig) -> Option<&str> {
    match sandbox {
        SandboxConfig::PolicyOnly => None,
        SandboxConfig::Bubblewrap(config) => Some(config.executable.as_str()),
        SandboxConfig::Seatbelt(config) => Some(config.executable.as_str()),
    }
}

/// Reports whether one exact cache identity proves the configured backend.
fn capability_matches(
    capability: &SandboxCapabilityCacheKey,
    backend: SandboxBackend,
    executable: &str,
    plan: &SandboxAuditSummary,
) -> bool {
    let (proven_backend, proven_executable, proven_profile) = match capability {
        SandboxCapabilityCacheKey::Bubblewrap(key) => (
            SandboxBackend::Bubblewrap,
            key.bubblewrap_executable.as_str(),
            key.runtime_profile_version,
        ),
        SandboxCapabilityCacheKey::Seatbelt(key) => (
            SandboxBackend::Seatbelt,
            key.sandbox_executable.as_str(),
            key.runtime_profile_version,
        ),
    };
    proven_backend == backend
        && proven_executable == executable
        && proven_profile == plan.runtime_profile_version
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        BubblewrapConfig, ConfiguredSandboxEnvironment, ConfiguredSandboxGroups,
        SandboxEnvironmentPolicy, SandboxUnavailablePolicy,
    };
    use crate::security::sandbox::{
        BUBBLEWRAP_RUNTIME_PROFILE_VERSION, BubblewrapCapabilityCacheKey,
        SEATBELT_RUNTIME_PROFILE_VERSION, SandboxAuthoritySource, SeatbeltCapabilityCacheKey,
    };

    /// Returns a real regular executable path available on Linux and macOS.
    fn available_executable() -> String {
        let candidate = std::env::current_exe().expect("test executable path is available");
        assert!(
            sandbox_executable_available(&candidate),
            "test executable must be a regular executable file"
        );
        candidate.to_string_lossy().into_owned()
    }

    /// Builds one Bubblewrap configuration for the requested network mode.
    fn bubblewrap_config(executable: &str, network: SandboxNetworkMode) -> SandboxConfig {
        SandboxConfig::Bubblewrap(BubblewrapConfig {
            executable: executable.to_string(),
            unavailable: SandboxUnavailablePolicy::Fail,
            network,
            environment: SandboxEnvironmentPolicy::Minimal,
            group_whitelist: ConfiguredSandboxGroups::default(),
            env_whitelist: ConfiguredSandboxEnvironment::default(),
            git_user_name: None,
            git_user_email: None,
        })
    }

    /// Builds one Seatbelt configuration for the requested network mode.
    fn seatbelt_config(executable: &str, network: SandboxNetworkMode) -> SandboxConfig {
        SandboxConfig::Seatbelt(crate::runtime::SeatbeltConfig {
            executable: executable.to_string(),
            unavailable: SandboxUnavailablePolicy::Fail,
            network,
            environment: SandboxEnvironmentPolicy::Minimal,
            env_whitelist: ConfiguredSandboxEnvironment::default(),
            git_user_name: None,
            git_user_email: None,
        })
    }

    /// Builds one redacted Bubblewrap plan summary for the requested mode.
    fn bubblewrap_plan(network: SandboxNetworkMode) -> SandboxAuditSummary {
        SandboxAuditSummary {
            backend: SandboxBackend::Bubblewrap,
            runtime_profile_version: BUBBLEWRAP_RUNTIME_PROFILE_VERSION,
            authority_source: SandboxAuthoritySource::Narrowed,
            read_only_grant_count: 1,
            read_write_grant_count: 0,
            network,
            plan_sha256: "a".repeat(64),
        }
    }

    /// Builds one redacted Seatbelt plan summary for the requested mode.
    fn seatbelt_plan(network: SandboxNetworkMode) -> SandboxAuditSummary {
        SandboxAuditSummary {
            backend: SandboxBackend::Seatbelt,
            runtime_profile_version: SEATBELT_RUNTIME_PROFILE_VERSION,
            authority_source: SandboxAuthoritySource::Narrowed,
            read_only_grant_count: 1,
            read_write_grant_count: 0,
            network,
            plan_sha256: "b".repeat(64),
        }
    }

    /// Builds one exact Bubblewrap capability proof for the executable.
    fn bubblewrap_capability(executable: &str) -> SandboxCapabilityCacheKey {
        SandboxCapabilityCacheKey::Bubblewrap(BubblewrapCapabilityCacheKey {
            pane_id: "%1".to_string(),
            pane_environment_signature: "environment-v1".to_string(),
            config_generation: 7,
            executable: executable.to_string(),
            bubblewrap_executable: executable.to_string(),
            identity_sha256: "c".repeat(64),
            environment_sha256: "d".repeat(64),
            runtime_profile_version: BUBBLEWRAP_RUNTIME_PROFILE_VERSION,
            probe_sha256: "e".repeat(64),
        })
    }

    /// Builds one exact Seatbelt capability proof for the executable.
    fn seatbelt_capability(executable: &str) -> SandboxCapabilityCacheKey {
        SandboxCapabilityCacheKey::Seatbelt(SeatbeltCapabilityCacheKey {
            backend: SandboxBackend::Seatbelt,
            pane_id: "%1".to_string(),
            pane_environment_signature: "environment-v1".to_string(),
            config_generation: 7,
            executable: executable.to_string(),
            sandbox_executable: executable.to_string(),
            executable_identity_sha256: "c".repeat(64),
            sandbox_executable_identity_sha256: "d".repeat(64),
            child_shell_path: "/bin/sh".to_string(),
            child_shell_identity_sha256: "f".repeat(64),
            environment_sha256: "g".repeat(64),
            host_identity_sha256: "h".repeat(64),
            runtime_profile_version: SEATBELT_RUNTIME_PROFILE_VERSION,
            profile_sha256: "i".repeat(64),
            probe_sha256: "j".repeat(64),
        })
    }

    /// Verifies host access never inherits a backend namespace or network claim.
    #[test]
    fn host_access_reports_unenforced_host_bypass() {
        let executable = available_executable();
        let sandbox = bubblewrap_config(&executable, SandboxNetworkMode::Isolated);
        let capability = bubblewrap_capability(&executable);
        let plan = bubblewrap_plan(SandboxNetworkMode::Isolated);
        let state = resolve_effective_sandbox(
            &sandbox,
            ApprovalPolicy::HostAccess,
            SandboxExecutionHost::Pane,
            SandboxEffectiveEvidence {
                capability: Some(&capability),
                plan: Some(&plan),
                effects_complete: true,
                sandbox_bypass_approved: false,
            },
        );

        assert_eq!(state.configured_intent, "bubblewrap");
        assert_eq!(state.selected_backend, Some(SandboxBackend::Bubblewrap));
        assert_eq!(state.boundary_str(), "host-bypass");
        assert_eq!(state.enforcement_str(), "none");
        assert_eq!(state.network_mode_str(), "unenforced");
        assert_eq!(state.reason_str(), "host-access-bypass");
    }

    /// Verifies policy-only configuration never claims confinement.
    #[test]
    fn policy_only_reports_unenforced_policy_boundary() {
        let state = resolve_effective_sandbox(
            &SandboxConfig::PolicyOnly,
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Pane,
            SandboxEffectiveEvidence::none(),
        );

        assert_eq!(state.configured_intent, "policy-only");
        assert_eq!(state.selected_backend, None);
        assert_eq!(state.boundary_str(), "policy-only");
        assert_eq!(state.enforcement_str(), "none");
        assert_eq!(state.network_mode_str(), "unenforced");
        assert_eq!(state.reason_str(), "policy-only");
    }

    /// Verifies a missing backend executable stops reporting the backend name.
    #[test]
    fn missing_executable_reports_unavailable_without_network_claim() {
        let sandbox = bubblewrap_config(
            "/nonexistent/mez-effective-test-bwrap",
            SandboxNetworkMode::Isolated,
        );
        let state = resolve_effective_sandbox(
            &sandbox,
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Pane,
            SandboxEffectiveEvidence::none(),
        );

        assert_eq!(state.boundary_str(), "unavailable");
        assert_eq!(state.enforcement_str(), "none");
        assert_eq!(state.network_mode_str(), "unknown");
        assert_eq!(state.reason_str(), "backend-unavailable");
        assert_eq!(state.audit_backend_name(None), None);
    }

    /// Verifies an available backend without probe or plan evidence reports no
    /// enforcement and an unknown network mode for every network setting.
    #[test]
    fn backend_without_evidence_reports_not_probed() {
        let executable = available_executable();
        for network in [SandboxNetworkMode::Isolated, SandboxNetworkMode::Connected] {
            let sandbox = bubblewrap_config(&executable, network);
            let state = resolve_effective_sandbox(
                &sandbox,
                ApprovalPolicy::Ask,
                SandboxExecutionHost::Pane,
                SandboxEffectiveEvidence::none(),
            );
            assert_eq!(state.boundary_str(), "bubblewrap");
            assert_eq!(state.enforcement_str(), "none");
            assert_eq!(state.network_mode_str(), "unknown");
            assert_eq!(state.reason_str(), "not-probed");
        }

        let capability = bubblewrap_capability(&executable);
        let capability_only = resolve_effective_sandbox(
            &bubblewrap_config(&executable, SandboxNetworkMode::Isolated),
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Pane,
            SandboxEffectiveEvidence {
                capability: Some(&capability),
                plan: None,
                effects_complete: true,
                sandbox_bypass_approved: false,
            },
        );
        assert_eq!(capability_only.network_mode_str(), "unknown");
        assert_eq!(capability_only.reason_str(), "not-probed");

        let plan = bubblewrap_plan(SandboxNetworkMode::Isolated);
        let plan_only = resolve_effective_sandbox(
            &bubblewrap_config(&executable, SandboxNetworkMode::Isolated),
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Native,
            SandboxEffectiveEvidence {
                capability: None,
                plan: Some(&plan),
                effects_complete: true,
                sandbox_bypass_approved: false,
            },
        );
        assert_eq!(plan_only.enforcement_str(), "none");
        assert_eq!(plan_only.network_mode_str(), "unknown");
        assert_eq!(plan_only.reason_str(), "not-probed");
    }

    /// Verifies only a compiled plan plus a matching capability proof yields a
    /// verified network claim for either backend.
    #[test]
    fn compiled_plan_with_capability_reports_verified_network_claims() {
        let executable = available_executable();
        let bubblewrap = bubblewrap_config(&executable, SandboxNetworkMode::Isolated);
        let bubblewrap_capability = bubblewrap_capability(&executable);
        let isolated = bubblewrap_plan(SandboxNetworkMode::Isolated);
        let state = resolve_effective_sandbox(
            &bubblewrap,
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Pane,
            SandboxEffectiveEvidence {
                capability: Some(&bubblewrap_capability),
                plan: Some(&isolated),
                effects_complete: true,
                sandbox_bypass_approved: false,
            },
        );
        assert_eq!(state.boundary_str(), "bubblewrap");
        assert_eq!(state.enforcement_str(), "bubblewrap-network-namespace");
        assert_eq!(state.network_mode_str(), "isolated");
        assert_eq!(state.reason_str(), "verified");
        assert_eq!(
            state.audit_backend_name(Some(&isolated)),
            Some("bubblewrap")
        );

        let connected = bubblewrap_plan(SandboxNetworkMode::Connected);
        let connected_state = resolve_effective_sandbox(
            &bubblewrap,
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Pane,
            SandboxEffectiveEvidence {
                capability: Some(&bubblewrap_capability),
                plan: Some(&connected),
                effects_complete: true,
                sandbox_bypass_approved: false,
            },
        );
        assert_eq!(
            connected_state.enforcement_str(),
            "bubblewrap-connected-profile"
        );
        assert_eq!(connected_state.network_mode_str(), "connected");

        let seatbelt = seatbelt_config(&executable, SandboxNetworkMode::Isolated);
        let seatbelt_capability = seatbelt_capability(&executable);
        let seatbelt_isolated = seatbelt_plan(SandboxNetworkMode::Isolated);
        let seatbelt_state = resolve_effective_sandbox(
            &seatbelt,
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Native,
            SandboxEffectiveEvidence {
                capability: Some(&seatbelt_capability),
                plan: Some(&seatbelt_isolated),
                effects_complete: true,
                sandbox_bypass_approved: false,
            },
        );
        assert_eq!(seatbelt_state.boundary_str(), "seatbelt");
        assert_eq!(seatbelt_state.enforcement_str(), "seatbelt-socket-policy");
        assert_eq!(seatbelt_state.network_mode_str(), "isolated");

        let seatbelt_connected = seatbelt_plan(SandboxNetworkMode::Connected);
        let seatbelt_connected_state = resolve_effective_sandbox(
            &seatbelt,
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Pane,
            SandboxEffectiveEvidence {
                capability: Some(&seatbelt_capability),
                plan: Some(&seatbelt_connected),
                effects_complete: true,
                sandbox_bypass_approved: false,
            },
        );
        assert_eq!(
            seatbelt_connected_state.enforcement_str(),
            "seatbelt-authorized-host-network"
        );
        assert_eq!(seatbelt_connected_state.network_mode_str(), "connected");
    }

    /// Verifies incomplete retained effects downgrade a compiled claim.
    #[test]
    fn incomplete_effects_report_effects_incomplete() {
        let executable = available_executable();
        let sandbox = bubblewrap_config(&executable, SandboxNetworkMode::Isolated);
        let capability = bubblewrap_capability(&executable);
        let plan = bubblewrap_plan(SandboxNetworkMode::Isolated);
        let state = resolve_effective_sandbox(
            &sandbox,
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Pane,
            SandboxEffectiveEvidence {
                capability: Some(&capability),
                plan: Some(&plan),
                effects_complete: false,
                sandbox_bypass_approved: false,
            },
        );

        assert_eq!(state.boundary_str(), "bubblewrap");
        assert_eq!(state.enforcement_str(), "none");
        assert_eq!(state.network_mode_str(), "unknown");
        assert_eq!(state.reason_str(), "effects-incomplete");
    }

    /// Verifies a foreign pane never inherits the configured backend.
    #[test]
    fn foreign_pane_reports_remote_unattested() {
        let executable = available_executable();
        let sandbox = bubblewrap_config(&executable, SandboxNetworkMode::Connected);
        let capability = bubblewrap_capability(&executable);
        let plan = bubblewrap_plan(SandboxNetworkMode::Connected);
        let state = resolve_effective_sandbox(
            &sandbox,
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Unattested,
            SandboxEffectiveEvidence {
                capability: Some(&capability),
                plan: Some(&plan),
                effects_complete: true,
                sandbox_bypass_approved: false,
            },
        );

        assert_eq!(state.boundary_str(), "remote-unattested");
        assert_eq!(state.enforcement_str(), "none");
        assert_eq!(state.network_mode_str(), "unknown");
        assert_eq!(state.reason_str(), "remote-shell-unattested");
        assert_eq!(state.audit_backend_name(Some(&plan)), None);
    }

    /// Verifies an approved one-shot bypass reports the fallback distinctly.
    #[test]
    fn approved_fallback_bypass_reports_policy_only() {
        let executable = available_executable();
        let sandbox = bubblewrap_config(&executable, SandboxNetworkMode::Isolated);
        let state = resolve_effective_sandbox(
            &sandbox,
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Pane,
            SandboxEffectiveEvidence {
                capability: None,
                plan: None,
                effects_complete: false,
                sandbox_bypass_approved: true,
            },
        );

        assert_eq!(state.boundary_str(), "policy-only");
        assert_eq!(state.enforcement_str(), "none");
        assert_eq!(state.network_mode_str(), "unenforced");
        assert_eq!(state.reason_str(), "sandbox-bypass-approved");
        assert_eq!(state.audit_backend_name(None), Some("policy-only"));
    }

    /// Verifies evidence for another backend never verifies the configured one.
    #[test]
    fn mismatched_evidence_never_verifies_the_configured_backend() {
        let executable = available_executable();
        let sandbox = bubblewrap_config(&executable, SandboxNetworkMode::Isolated);
        let capability = seatbelt_capability(&executable);
        let plan = seatbelt_plan(SandboxNetworkMode::Isolated);
        let state = resolve_effective_sandbox(
            &sandbox,
            ApprovalPolicy::Ask,
            SandboxExecutionHost::Pane,
            SandboxEffectiveEvidence {
                capability: Some(&capability),
                plan: Some(&plan),
                effects_complete: true,
                sandbox_bypass_approved: false,
            },
        );

        assert_eq!(state.boundary_str(), "bubblewrap");
        assert_eq!(state.enforcement_str(), "none");
        assert_eq!(state.network_mode_str(), "unknown");
        assert_eq!(state.reason_str(), "not-probed");
        assert_eq!(state.audit_backend_name(Some(&plan)), None);
    }

    /// Verifies every reason and boundary spelling belongs to the closed sets.
    #[test]
    fn reported_spellings_are_closed_enum_values() {
        let boundaries = [
            SandboxEffectiveBoundary::Bubblewrap,
            SandboxEffectiveBoundary::Seatbelt,
            SandboxEffectiveBoundary::PolicyOnly,
            SandboxEffectiveBoundary::HostBypass,
            SandboxEffectiveBoundary::RemoteUnattested,
            SandboxEffectiveBoundary::Unavailable,
        ];
        assert!(
            boundaries
                .iter()
                .all(|boundary| !boundary.as_str().is_empty())
        );
        let reasons = [
            SandboxEffectiveReason::Verified,
            SandboxEffectiveReason::NotProbed,
            SandboxEffectiveReason::BackendUnavailable,
            SandboxEffectiveReason::PolicyOnly,
            SandboxEffectiveReason::HostAccessBypass,
            SandboxEffectiveReason::SandboxBypassApproved,
            SandboxEffectiveReason::RemoteShellUnattested,
            SandboxEffectiveReason::EffectsIncomplete,
        ];
        assert_eq!(reasons.len(), 8);
        assert!(reasons.iter().all(|reason| !reason.as_str().is_empty()));
    }
}
