//! Read-only sandbox workflow projection and diagnostics.
//!
//! This module turns effective configuration, project discovery, and trust
//! evidence into one deterministic user-facing status model. It deliberately
//! performs no capability probes and creates no managed-home directories;
//! commands that mutate sandbox policy build on this projection separately.

#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use mez_agent::ApprovalPolicy;

use crate::runtime::{ConfiguredPermissions, SandboxBackend, SandboxConfig};
use crate::security::project::{ProjectRootDiscovery, ProjectRootMarkerKind, TrustDecision};

use super::managed_home::inspect_seatbelt_managed_home;
use super::seatbelt::SEATBELT_RUNTIME_PROFILE_VERSION;
use super::{
    BUBBLEWRAP_RUNTIME_PROFILE_VERSION, bubblewrap_executable_available,
    inspect_bubblewrap_managed_home, sandbox_restriction_ids,
};

/// Inputs used to build one side-effect-free sandbox workflow projection.
pub(crate) struct SandboxWorkflowRequest<'a> {
    /// Materialized permission and sandbox configuration.
    pub(crate) permissions: &'a ConfiguredPermissions,
    /// Canonical project-root discovery evidence.
    pub(crate) discovery: &'a ProjectRootDiscovery,
    /// Current trust decision for the discovered project identity.
    pub(crate) trust_state: TrustDecision,
    /// Private configuration root inspected for managed-home readiness.
    pub(crate) config_root: &'a Path,
    /// Effective configuration source for the sandbox backend.
    pub(crate) sandbox_source: &'a str,
    /// Effective configuration source for the approval policy.
    pub(crate) approval_policy_source: &'a str,
    /// Effective configuration source for read scopes.
    pub(crate) read_scopes_source: &'a str,
    /// Effective configuration source for write scopes.
    pub(crate) write_scopes_source: &'a str,
}

/// Severity attached to one stable sandbox diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SandboxDiagnosticSeverity {
    /// Informational restriction or workflow fact.
    Info,
    /// A risky or incomplete state that does not prevent all sandbox use.
    Warning,
    /// A state that prevents the configured sandbox from operating safely.
    Error,
}

/// One stable, machine-readable sandbox workflow diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SandboxWorkflowDiagnostic {
    /// Stable diagnostic identity.
    pub(crate) id: &'static str,
    /// Diagnostic severity reported in sandbox status output.
    pub(crate) severity: SandboxDiagnosticSeverity,
    /// Short user-facing description.
    pub(crate) summary: String,
    /// Bounded explanation that excludes raw arguments and environment data.
    pub(crate) details: String,
    /// Safe direct-user remediation that never broadens authority automatically.
    pub(crate) remedy: String,
    /// Relevant project or executable path, when applicable.
    pub(crate) affected_path: Option<PathBuf>,
    /// Subsystem that produced the diagnostic.
    pub(crate) source: &'static str,
}

/// Configured sandbox policy before approval-policy boundary selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SandboxConfiguredState {
    /// Configured sandbox backend.
    pub(crate) sandbox: String,
    /// Source layer for the sandbox backend.
    pub(crate) sandbox_source: String,
    /// Configured approval policy.
    pub(crate) approval_policy: String,
    /// Source layer for the approval policy.
    pub(crate) approval_policy_source: String,
    /// Configured network authorization policy.
    pub(crate) network_policy: String,
    /// Raw configured read scopes pending pane-shell resolution.
    pub(crate) read_scopes: Vec<String>,
    /// Source layer for configured read scopes.
    pub(crate) read_scopes_source: String,
    /// Raw configured write scopes pending pane-shell resolution.
    pub(crate) write_scopes: Vec<String>,
    /// Source layer for configured write scopes.
    pub(crate) write_scopes_source: String,
    /// Primary-user-selected pane supplementary group mappings.
    pub(crate) group_whitelist: Vec<String>,
    /// Requested pane environment variable names; values are never serialized.
    pub(crate) env_whitelist: Vec<String>,
}

/// Effective sandbox boundary and local read-only readiness evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SandboxEffectiveState {
    /// Effective execution boundary: policy-only, bubblewrap, seatbelt, or host.
    pub(crate) sandbox: String,
    /// Provenance for effective filesystem authority.
    pub(crate) scope_provenance: String,
    /// Effective read scopes known outside a live pane.
    pub(crate) read_scopes: Vec<String>,
    /// Effective write scopes known outside a live pane.
    pub(crate) write_scopes: Vec<String>,
    /// Configured backend executable, when applicable.
    pub(crate) sandbox_executable: Option<PathBuf>,
    /// Read-only local executable inspection state.
    pub(crate) sandbox_executable_state: String,
    /// Code-owned runtime profile version for the configured backend.
    pub(crate) runtime_profile_version: Option<String>,
    /// Group resolution state for the active pane environment.
    pub(crate) supplementary_group_state: String,
    /// Number of configured supplementary groups after successful resolution.
    pub(crate) supplementary_group_count: usize,
    /// Pane- or native-worker-specific capability probe state.
    pub(crate) capability_state: String,
    /// Managed-home readiness without creating a home.
    pub(crate) managed_home_state: String,
    /// Regular-file bytes currently retained in the selected managed home.
    pub(crate) managed_home_bytes: u64,
    /// Whether the selected managed home is currently active in a workload.
    pub(crate) managed_home_active: bool,
    /// Stable description of how the managed home appears to the workload.
    pub(crate) managed_home_path_semantics: String,
    /// Backend-specific network confinement mechanism.
    pub(crate) network_boundary: String,
    /// Backend-specific host namespace visibility.
    pub(crate) namespace_boundary: String,
    /// Stable restriction identifiers for the configured backend.
    pub(crate) restrictions: Vec<String>,
    /// Freshness of this standalone projection relative to live sessions.
    pub(crate) reload_freshness: String,
}

/// Project identity and trust evidence used by the workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SandboxProjectState {
    /// Canonical path from which discovery began.
    pub(crate) canonical_start: PathBuf,
    /// Canonical discovered project root.
    pub(crate) canonical_root: PathBuf,
    /// Current-directory or explicit-path provenance.
    pub(crate) input_source: String,
    /// Git directory, Git file, or fallback marker kind.
    pub(crate) marker_kind: String,
    /// Repository nesting depth from the start directory.
    pub(crate) nesting_depth: usize,
    /// Current project trust decision.
    pub(crate) trust_state: String,
}

/// Confirmation data shared with future mutating workflows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SandboxWorkflowConfirmation {
    /// Read-only status plans never require confirmation.
    pub(crate) required: bool,
    /// Human-readable confirmation reason for future mutations.
    pub(crate) reason: Option<String>,
}

/// Complete deterministic sandbox workflow plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SandboxWorkflowPlan {
    /// Schema version for stable JSON consumers.
    pub(crate) version: u32,
    /// Canonical project identity and trust evidence.
    pub(crate) project: SandboxProjectState,
    /// Configured policy before runtime boundary selection.
    pub(crate) configured: SandboxConfiguredState,
    /// Effective boundary and readiness evidence.
    pub(crate) effective: SandboxEffectiveState,
    /// Planned mutations; always empty for this read-only projection.
    pub(crate) mutations: Vec<String>,
    /// Confirmation requirement; always false for read-only commands.
    pub(crate) confirmation: SandboxWorkflowConfirmation,
    /// Stable diagnostics and safe remedies.
    pub(crate) diagnostics: Vec<SandboxWorkflowDiagnostic>,
}

/// Returns the stable effective execution boundary after approval policy is
/// applied to the configured sandbox backend.
pub(crate) fn effective_sandbox_boundary(
    sandbox: &SandboxConfig,
    approval_policy: ApprovalPolicy,
) -> &'static str {
    if approval_policy.bypasses_sandbox() {
        "host"
    } else {
        sandbox.as_str()
    }
}

/// Builds one read-only status plan from already-loaded local state.
pub(crate) fn plan_sandbox_workflow(request: SandboxWorkflowRequest<'_>) -> SandboxWorkflowPlan {
    let configured_sandbox = request.permissions.sandbox.as_str().to_string();
    let approval_policy = request.permissions.authorization.approval_policy;
    let trusted = request.trust_state == TrustDecision::Trusted;
    let explicit_scopes = !request.permissions.resources.read_scopes.is_empty()
        || !request.permissions.resources.write_scopes.is_empty();
    let (scope_provenance, read_scopes, write_scopes) = if explicit_scopes {
        (
            "explicit",
            request.permissions.resources.read_scopes.clone(),
            request.permissions.resources.write_scopes.clone(),
        )
    } else if trusted {
        let root = request
            .discovery
            .canonical_root
            .to_string_lossy()
            .into_owned();
        ("trusted-project", vec![root.clone()], vec![root])
    } else {
        ("none", Vec::new(), Vec::new())
    };
    let effective_sandbox =
        effective_sandbox_boundary(&request.permissions.sandbox, approval_policy).to_string();

    let (configured_group_whitelist, supplementary_group_state, supplementary_group_count) =
        match &request.permissions.sandbox {
            SandboxConfig::PolicyOnly => (Vec::new(), "not-applicable".to_string(), 0),
            SandboxConfig::Bubblewrap(config) => {
                let configured = config.group_whitelist.requested_names.clone();
                let count = configured.len();
                (configured, "pane-bootstrap-required".to_string(), count)
            }
            SandboxConfig::Seatbelt(_) => (Vec::new(), "not-applicable".to_string(), 0),
        };

    let (
        sandbox_executable,
        executable_state,
        runtime_profile_version,
        managed_home_state,
        managed_home_bytes,
        managed_home_active,
        managed_home_path_semantics,
        network_boundary,
        namespace_boundary,
    ) = match &request.permissions.sandbox {
        SandboxConfig::PolicyOnly => (
            None,
            "not-configured",
            None,
            "not-applicable",
            0,
            false,
            "not-applicable",
            "policy-only",
            "visible-host-namespace",
        ),
        SandboxConfig::Bubblewrap(config) => {
            let executable = PathBuf::from(&config.executable);
            let executable_state = if bubblewrap_executable_available(&executable) {
                "available"
            } else {
                "unavailable"
            };
            let (managed_home_state, managed_home_bytes, managed_home_active) = if trusted {
                inspect_managed_home_state(
                    SandboxBackend::Bubblewrap,
                    request.config_root,
                    &request.discovery.canonical_root,
                )
            } else {
                ("not-applicable", 0, false)
            };
            (
                Some(executable),
                executable_state,
                Some(BUBBLEWRAP_RUNTIME_PROFILE_VERSION.to_string()),
                managed_home_state,
                managed_home_bytes,
                managed_home_active,
                "synthetic-mounted-home",
                "per-action-network-namespace-or-authorized-host-network",
                "private-mount-pid-user-uts-ipc-namespaces",
            )
        }
        SandboxConfig::Seatbelt(config) => {
            let executable = PathBuf::from(&config.executable);
            let executable_state = if bubblewrap_executable_available(&executable) {
                "available"
            } else {
                "unavailable"
            };
            let (managed_home_state, managed_home_bytes, managed_home_active) = if trusted {
                inspect_managed_home_state(
                    SandboxBackend::Seatbelt,
                    request.config_root,
                    &request.discovery.canonical_root,
                )
            } else {
                ("not-applicable", 0, false)
            };
            (
                Some(executable),
                executable_state,
                Some(SEATBELT_RUNTIME_PROFILE_VERSION.to_string()),
                managed_home_state,
                managed_home_bytes,
                managed_home_active,
                "private-canonical-host-path",
                "per-action-operation-denial-or-authorized-host-network",
                "visible-host-namespace",
            )
        }
    };

    let mut diagnostics = Vec::new();
    if request.discovery.marker_kind == ProjectRootMarkerKind::Fallback {
        diagnostics.push(SandboxWorkflowDiagnostic {
            id: "sandbox.project-root-fallback",
            severity: SandboxDiagnosticSeverity::Warning,
            summary: "No Git project marker was found".to_string(),
            details: "Sandbox authority is projected from the canonical start directory rather than a repository identity.".to_string(),
            remedy: "Pass an explicit project path or initialize a repository before changing sandbox authority.".to_string(),
            affected_path: Some(request.discovery.canonical_root.clone()),
            source: "project-discovery",
        });
    }
    if let Some(backend) = request.permissions.sandbox.backend() {
        let backend_name = backend.as_str();
        let (source, executable_id, display_name, executable_setting) = match backend {
            SandboxBackend::Bubblewrap => (
                "bubblewrap",
                "sandbox.bubblewrap-executable-unavailable",
                "Bubblewrap",
                "permissions.bubblewrap.executable",
            ),
            SandboxBackend::Seatbelt => (
                "seatbelt",
                "sandbox.seatbelt-executable-unavailable",
                "Seatbelt",
                "permissions.seatbelt.executable",
            ),
        };
        if executable_state != "available" {
            diagnostics.push(SandboxWorkflowDiagnostic {
                id: executable_id,
                severity: SandboxDiagnosticSeverity::Error,
                summary: format!("Configured {display_name} executable is unavailable"),
                details: "The configured absolute path is missing, not a regular file, or not executable.".to_string(),
                remedy: format!("Install {display_name} or set {executable_setting} to the code-owned executable path."),
                affected_path: sandbox_executable.clone(),
                source,
            });
        }
        if scope_provenance == "none" {
            diagnostics.push(SandboxWorkflowDiagnostic {
                id: "sandbox.filesystem-authority-unresolved",
                severity: SandboxDiagnosticSeverity::Error,
                summary: format!("{display_name} has no filesystem authority"),
                details: "No explicit scopes or trusted-project default are available for this project.".to_string(),
                remedy: "As the direct user, configure narrow read/write scopes or explicitly trust the intended project.".to_string(),
                affected_path: Some(request.discovery.canonical_root.clone()),
                source: "permissions",
            });
        }
        diagnostics.push(SandboxWorkflowDiagnostic {
            id: "sandbox.capability-probe-execution-specific",
            severity: SandboxDiagnosticSeverity::Info,
            summary: format!("{display_name} capability is verified per execution context"),
            details: "This read-only command does not run or populate pane or native-worker capability caches.".to_string(),
            remedy: "Start a sandboxed action in the intended execution mode to perform the fail-closed capability probe.".to_string(),
            affected_path: sandbox_executable.clone(),
            source,
        });
        let network_details = match backend {
            SandboxBackend::Bubblewrap => {
                "A deny policy uses an isolated network namespace; allow and approved prompt actions may use the host network."
            }
            SandboxBackend::Seatbelt => {
                "A deny policy rejects network operations in the visible host namespace; allow and approved prompt actions may use host networking."
            }
        };
        diagnostics.push(SandboxWorkflowDiagnostic {
            id: "sandbox.network-policy-enforced",
            severity: SandboxDiagnosticSeverity::Info,
            summary: format!("{display_name} enforces shell network policy"),
            details: network_details.to_string(),
            remedy: "Review permissions.network_policy and the active approval policy before running shell actions.".to_string(),
            affected_path: None,
            source,
        });
        diagnostics.push(SandboxWorkflowDiagnostic {
            id: "sandbox.minimal-path",
            severity: SandboxDiagnosticSeverity::Info,
            summary: format!("{display_name} uses a controlled executable path"),
            details: format!("A successfully resolved whitelisted PATH controls sandbox command lookup; otherwise {display_name} falls back to /usr/bin:/bin."),
            remedy: format!("Use narrow read scopes for external executable roots and include PATH in permissions.{backend_name}.env_whitelist when command lookup requires it."),
            affected_path: None,
            source,
        });
        match backend {
            SandboxBackend::Bubblewrap => diagnostics.push(SandboxWorkflowDiagnostic {
                id: "sandbox.synthetic-home",
                severity: SandboxDiagnosticSeverity::Info,
                summary: "Bubblewrap uses a synthetic mounted home".to_string(),
                details: "The real user home and host credentials remain hidden by mount projection.".to_string(),
                remedy: "Store non-secret build caches in the managed home; do not project host credentials.".to_string(),
                affected_path: None,
                source: "managed-home",
            }),
            SandboxBackend::Seatbelt => {
                diagnostics.push(SandboxWorkflowDiagnostic {
                    id: "sandbox.private-host-home",
                    severity: SandboxDiagnosticSeverity::Info,
                    summary: "Seatbelt uses a private canonical host-path home".to_string(),
                    details: "The managed home is visible at its canonical host path; Seatbelt denies operations outside authorized paths rather than mounting a synthetic namespace.".to_string(),
                    remedy: "Store non-secret build caches in the managed home; do not grant access to host credential directories.".to_string(),
                    affected_path: None,
                    source: "managed-home",
                });
                diagnostics.push(SandboxWorkflowDiagnostic {
                    id: "sandbox.visible-host-namespace",
                    severity: SandboxDiagnosticSeverity::Info,
                    summary: "Seatbelt retains the visible host namespace".to_string(),
                    details: "Seatbelt is operation-level mandatory access control and does not provide mount, PID, user, or network namespaces.".to_string(),
                    remedy: "Treat Seatbelt as operation confinement, not namespace isolation, when reviewing workload risk.".to_string(),
                    affected_path: None,
                    source: "seatbelt",
                });
            }
        }
    }
    if approval_policy.bypasses_sandbox() {
        diagnostics.push(SandboxWorkflowDiagnostic {
            id: "sandbox.host-policy-bypass",
            severity: SandboxDiagnosticSeverity::Warning,
            summary: "Host-access bypasses the configured sandbox".to_string(),
            details: format!("Local shell actions execute on the host even though the {configured_sandbox} configuration remains selected."),
            remedy: "As the direct user, select ask, auto-allow, or full-access to restore the configured sandbox boundary.".to_string(),
            affected_path: None,
            source: "approval-policy",
        });
    }

    SandboxWorkflowPlan {
        version: 2,
        project: SandboxProjectState {
            canonical_start: request.discovery.canonical_start.clone(),
            canonical_root: request.discovery.canonical_root.clone(),
            input_source: request.discovery.input_source.as_str().to_string(),
            marker_kind: request.discovery.marker_kind.as_str().to_string(),
            nesting_depth: request.discovery.nesting_depth,
            trust_state: trust_state_name(request.trust_state).to_string(),
        },
        configured: SandboxConfiguredState {
            sandbox: configured_sandbox,
            sandbox_source: request.sandbox_source.to_string(),
            approval_policy: approval_policy.as_str().to_string(),
            approval_policy_source: request.approval_policy_source.to_string(),
            network_policy: request
                .permissions
                .resources
                .network_policy
                .as_str()
                .to_string(),
            read_scopes: request.permissions.resources.read_scopes.clone(),
            read_scopes_source: request.read_scopes_source.to_string(),
            write_scopes: request.permissions.resources.write_scopes.clone(),
            write_scopes_source: request.write_scopes_source.to_string(),
            group_whitelist: configured_group_whitelist,
            env_whitelist: match &request.permissions.sandbox {
                SandboxConfig::PolicyOnly => Vec::new(),
                SandboxConfig::Bubblewrap(config) => config.env_whitelist.requested_names.clone(),
                SandboxConfig::Seatbelt(config) => config.env_whitelist.requested_names.clone(),
            },
        },
        effective: SandboxEffectiveState {
            sandbox: effective_sandbox,
            scope_provenance: scope_provenance.to_string(),
            read_scopes,
            write_scopes,
            sandbox_executable,
            sandbox_executable_state: executable_state.to_string(),
            runtime_profile_version,
            supplementary_group_state,
            supplementary_group_count,
            capability_state: if request.permissions.sandbox.backend().is_some() {
                "not-probed"
            } else {
                "not-applicable"
            }
            .to_string(),
            managed_home_state: managed_home_state.to_string(),
            managed_home_bytes,
            managed_home_active,
            managed_home_path_semantics: managed_home_path_semantics.to_string(),
            network_boundary: network_boundary.to_string(),
            namespace_boundary: namespace_boundary.to_string(),
            restrictions: request
                .permissions
                .sandbox
                .backend()
                .map(sandbox_restriction_ids)
                .unwrap_or_default()
                .iter()
                .map(|restriction| (*restriction).to_string())
                .collect(),
            reload_freshness: "standalone-current-config".to_string(),
        },
        mutations: Vec::new(),
        confirmation: SandboxWorkflowConfirmation {
            required: false,
            reason: None,
        },
        diagnostics,
    }
}

fn inspect_managed_home_state(
    backend: SandboxBackend,
    config_root: &Path,
    project_root: &Path,
) -> (&'static str, u64, bool) {
    let inspection = match backend {
        SandboxBackend::Bubblewrap => inspect_bubblewrap_managed_home(config_root, project_root),
        SandboxBackend::Seatbelt => inspect_seatbelt_managed_home(config_root, project_root),
    };
    match inspection {
        Ok(inspection) if inspection.exists && inspection.active => {
            ("active", inspection.bytes, true)
        }
        Ok(inspection) if inspection.exists => ("ready", inspection.bytes, false),
        Ok(_) => ("absent", 0, false),
        Err(_) => ("unsafe", 0, false),
    }
}

fn trust_state_name(state: TrustDecision) -> &'static str {
    match state {
        TrustDecision::Pending => "pending",
        TrustDecision::Trusted => "trusted",
        TrustDecision::Rejected => "rejected",
        TrustDecision::Revoked => "revoked",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        BubblewrapConfig, NetworkPolicy, SandboxEnvironmentPolicy, SandboxNetworkMode,
        SandboxUnavailablePolicy,
    };
    use crate::security::project::{ProjectRootInputSource, ProjectRootMarkerKind};

    /// Verifies planning a trusted Bubblewrap project reports effective
    /// authority and managed-home absence without creating any directory.
    #[test]
    fn read_only_plan_does_not_create_managed_home() {
        let root = std::env::temp_dir().join(format!(
            "mez-sandbox-workflow-read-only-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("project")).unwrap();
        let project = root.join("project").canonicalize().unwrap();
        let config_root = root.join("config");
        let mut permissions = ConfiguredPermissions::default();
        permissions.resources.read_scopes.clear();
        permissions.resources.write_scopes.clear();
        permissions.resources.network_policy = NetworkPolicy::Deny;
        permissions.sandbox = SandboxConfig::Bubblewrap(BubblewrapConfig {
            executable: "/bin/sh".to_string(),
            unavailable: SandboxUnavailablePolicy::Fail,
            network: SandboxNetworkMode::Isolated,
            environment: SandboxEnvironmentPolicy::Minimal,
            group_whitelist: crate::runtime::ConfiguredSandboxGroups::default(),
            env_whitelist: crate::runtime::ConfiguredSandboxEnvironment::default(),
            git_user_name: None,
            git_user_email: None,
        });
        let discovery = ProjectRootDiscovery {
            canonical_start: project.clone(),
            canonical_root: project.clone(),
            input_source: ProjectRootInputSource::ExplicitPath,
            marker_kind: ProjectRootMarkerKind::GitDirectory,
            nesting_depth: 0,
        };

        let plan = plan_sandbox_workflow(SandboxWorkflowRequest {
            permissions: &permissions,
            discovery: &discovery,
            trust_state: TrustDecision::Trusted,
            config_root: &config_root,
            sandbox_source: "primary",
            approval_policy_source: "primary",
            read_scopes_source: "default",
            write_scopes_source: "default",
        });

        assert_eq!(plan.effective.sandbox, "bubblewrap");
        assert_eq!(plan.effective.scope_provenance, "trusted-project");
        assert_eq!(plan.effective.managed_home_state, "absent");
        assert_eq!(plan.effective.managed_home_bytes, 0);
        assert!(!plan.effective.managed_home_active);
        assert!(!config_root.exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Verifies the shared status projection reports managed-home byte usage
    /// and activity without modifying the selected home.
    #[test]
    fn read_only_plan_reports_managed_home_usage_and_activity() {
        let root = std::env::temp_dir().join(format!(
            "mez-sandbox-workflow-managed-home-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("project")).unwrap();
        let project = root.join("project").canonicalize().unwrap();
        let config_root = root.join("config");
        let (managed, activity) =
            super::super::prepare_bubblewrap_managed_home_for_workload(&config_root, &project)
                .unwrap();
        fs::write(managed.host_path.join(".cache/status-payload"), b"payload").unwrap();
        let permissions = ConfiguredPermissions {
            sandbox: SandboxConfig::Bubblewrap(BubblewrapConfig {
                executable: "/bin/sh".to_string(),
                unavailable: SandboxUnavailablePolicy::Fail,
                network: SandboxNetworkMode::Isolated,
                environment: SandboxEnvironmentPolicy::Minimal,
                group_whitelist: crate::runtime::ConfiguredSandboxGroups::default(),
                env_whitelist: crate::runtime::ConfiguredSandboxEnvironment::default(),
                git_user_name: None,
                git_user_email: None,
            }),
            ..ConfiguredPermissions::default()
        };
        let discovery = ProjectRootDiscovery {
            canonical_start: project.clone(),
            canonical_root: project,
            input_source: ProjectRootInputSource::ExplicitPath,
            marker_kind: ProjectRootMarkerKind::GitDirectory,
            nesting_depth: 0,
        };

        let plan = plan_sandbox_workflow(SandboxWorkflowRequest {
            permissions: &permissions,
            discovery: &discovery,
            trust_state: TrustDecision::Trusted,
            config_root: &config_root,
            sandbox_source: "primary",
            approval_policy_source: "primary",
            read_scopes_source: "default",
            write_scopes_source: "default",
        });

        assert_eq!(plan.effective.managed_home_state, "active");
        assert!(plan.effective.managed_home_bytes >= 7);
        assert!(plan.effective.managed_home_active);
        assert!(managed.host_path.exists());
        drop(activity);
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies host access remains visibly distinct from configured
    /// Bubblewrap and reports the policy-bypass diagnostic.
    #[test]
    fn host_access_reports_effective_host_boundary() {
        let root = std::env::temp_dir();
        let mut permissions = ConfiguredPermissions::default();
        permissions.authorization.approval_policy = mez_agent::ApprovalPolicy::HostAccess;
        permissions.resources.read_scopes = vec!["/tmp".to_string()];
        permissions.sandbox = SandboxConfig::Bubblewrap(BubblewrapConfig {
            executable: "/bin/sh".to_string(),
            unavailable: SandboxUnavailablePolicy::Fail,
            network: SandboxNetworkMode::Isolated,
            environment: SandboxEnvironmentPolicy::Minimal,
            group_whitelist: crate::runtime::ConfiguredSandboxGroups::default(),
            env_whitelist: crate::runtime::ConfiguredSandboxEnvironment::default(),
            git_user_name: None,
            git_user_email: None,
        });
        let discovery = ProjectRootDiscovery {
            canonical_start: root.to_path_buf(),
            canonical_root: root.to_path_buf(),
            input_source: ProjectRootInputSource::CurrentDirectory,
            marker_kind: ProjectRootMarkerKind::GitDirectory,
            nesting_depth: 0,
        };

        let plan = plan_sandbox_workflow(SandboxWorkflowRequest {
            permissions: &permissions,
            discovery: &discovery,
            trust_state: TrustDecision::Pending,
            config_root: &root,
            sandbox_source: "primary",
            approval_policy_source: "primary",
            read_scopes_source: "primary",
            write_scopes_source: "default",
        });

        assert_eq!(plan.configured.sandbox, "bubblewrap");
        assert_eq!(plan.effective.sandbox, "host");
        assert!(
            plan.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.id == "sandbox.host-policy-bypass")
        );
    }
}
