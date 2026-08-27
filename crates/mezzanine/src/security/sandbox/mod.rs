//! Typed sandbox-policy compilation for product-owned confinement backends.
//!
//! This module converts trusted pane-shell path authority and the structured
//! permission evaluation computed for the original policy command into a
//! backend-neutral effective policy and deterministic launch plan. Authorization
//! remains owned by the permission subsystem. Backend compilers only narrow
//! already-authorized resource authority and never infer grants from command
//! prefixes or rediscover filesystem facts.
//!
//! The boundary is deliberately fail-closed: unresolved paths, host-root
//! grants, credential or process-control requirements, network requirements
//! without mediated egress, and unsupported stateful or interactive execution
//! all fail before a workload can start. Generated plans contain typed argv,
//! never user-provided backend arguments or wrapper shell fragments.

use std::collections::BTreeMap;
use std::fmt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use mez_agent::permissions::{
    EffectiveCommandEffects, PathResolutionStatus, PathScopes, PermissionEvaluation,
    ResolvedPathEvidence, ResolvedPathKind, ResolvedPathObjectKind, RuleDecision,
};
use sha2::{Digest, Sha256};

use crate::runtime::{
    BubblewrapConfig, NetworkPolicy, SandboxBackend, SandboxEnvironmentPolicy, SandboxNetworkMode,
    SandboxUnavailablePolicy,
};

mod identity;
mod managed_home;
pub(crate) mod seatbelt;
mod workflow;

pub(crate) use identity::{
    ResolvedSandboxIdentity, resolve_group_name, resolve_sandbox_identity, resolve_user_name,
};
#[cfg(test)]
pub(crate) use managed_home::{
    BubblewrapManagedHome, prepare_bubblewrap_managed_home,
    prepare_bubblewrap_managed_home_for_workload,
};
pub(crate) use managed_home::{
    BubblewrapManagedHomeActivityLock, BubblewrapManagedHomeMaintenance,
    clear_bubblewrap_managed_home, inspect_bubblewrap_managed_home,
    prepare_bubblewrap_managed_home_for_workload_with_identity, prune_bubblewrap_managed_homes,
    remove_bubblewrap_managed_home,
};
pub(crate) use workflow::{
    SandboxDiagnosticSeverity, SandboxWorkflowPlan, SandboxWorkflowRequest,
    effective_sandbox_boundary, plan_sandbox_workflow,
};

/// Version of the runtime projection emitted by this compiler.
pub(crate) const BUBBLEWRAP_RUNTIME_PROFILE_VERSION: &str = "bubblewrap-v14";
/// Runtime-owned descriptor used for Bubblewrap lifecycle status documents.
pub(crate) const BUBBLEWRAP_STATUS_FD: u8 = 3;

const SANDBOX_COMMAND_PATH: &str = "/run/mez/command";
/// Sentinel replaced by the pane transaction's materialized command-file
/// argument immediately before rendering the typed child launch.
pub(crate) const BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER: &str =
    "/run/mez/host-command-placeholder";
const SANDBOX_HOME: &str = "/home/mez";
const MINIMAL_PATH: &str = "/usr/bin:/bin";
/// Stable, non-sensitive restriction identifiers used by status and failure diagnostics.
pub(crate) const BUBBLEWRAP_RESTRICTION_IDS: [&str; 4] = [
    "authority-mounts-only",
    "synthetic-home",
    "minimal-path",
    "network-policy-enforced",
];

/// Adds the direct-user sandbox diagnostic command to one concise live error.
///
/// Runtime failures use this shared wording so pane, transcript, and model
/// consumers receive an actionable next step without exposing launch argv or
/// suggesting that sandbox authority be broadened.
pub(crate) fn bubblewrap_failure_remediation(message: &str) -> String {
    let message = message.trim();
    if message.contains("`mez sandbox status --verbose`") {
        return message.to_string();
    }
    format!(
        "{}. Run `mez sandbox status --verbose` to inspect the executable, authority, and configuration remedies.",
        message.trim_end_matches('.')
    )
}

/// Returns whether a configured Bubblewrap path names an executable file.
///
/// This side-effect-free check is shared by first-run configuration selection
/// and sandbox diagnostics. Full runtime capability remains verified per pane
/// before any sandboxed workload starts.
pub(crate) fn bubblewrap_executable_available(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Inputs required to compile one authorized command into a launch plan.
#[derive(Debug, Clone)]
pub(crate) struct BubblewrapCompileRequest<'a> {
    /// Typed Bubblewrap backend configuration.
    pub(crate) config: &'a BubblewrapConfig,
    /// NSS-resolved exact native identity shared with the successful probe.
    pub(crate) identity: ResolvedSandboxIdentity,
    /// Successful capability probe for the active pane environment.
    pub(crate) capability: BubblewrapCapability,
    /// Bootstrap-derived identity of the active pane environment.
    pub(crate) pane_environment_signature: &'a str,
    /// Protected pane-derived values selected for forwarding.
    pub(crate) environment_evidence: &'a mez_agent::shell::PaneEnvironmentEvidence,
    /// Effective authorization policy for network-requiring commands.
    pub(crate) network_policy: NetworkPolicy,
    /// Trusted maximum filesystem authority resolved by the pane shell.
    pub(crate) maximum_authority: &'a PathScopes,
    /// Structured evaluation computed from the original policy command.
    pub(crate) permission_evaluation: &'a PermissionEvaluation,
    /// Whether this action requires the complete maximum filesystem authority.
    pub(crate) preserve_maximum_authority: bool,
    /// Absolute child shell path in the pane environment.
    pub(crate) child_shell_path: &'a str,
    /// Absolute harness-owned command-file path in the pane environment.
    pub(crate) command_file_host_path: &'a str,
    /// Optional Mezzanine-owned persistent home and synthetic account records.
    pub(crate) managed_home: Option<&'a managed_home::BubblewrapManagedHome>,
    /// Canonical pane home whose authorized descendants map below `/home/mez`.
    pub(crate) pane_home_directory: Option<&'a Path>,
    /// Whether the command must mutate persistent shell state.
    pub(crate) stateful: bool,
    /// Whether the command requires direct terminal interaction.
    pub(crate) interactive: bool,
}

/// Backend-discriminated request for compiling one sandboxed workload.
#[derive(Debug, Clone)]
pub(crate) enum SandboxCompileRequest<'a> {
    /// Compile the request with the Linux Bubblewrap backend.
    Bubblewrap(BubblewrapCompileRequest<'a>),
}

/// Identifies whether command effects narrowed maximum filesystem authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxAuthoritySource {
    /// Unknown effects retain the complete configured maximum.
    Maximum,
    /// Complete effects narrowed the command to specific resolved paths.
    Narrowed,
}

impl SandboxAuthoritySource {
    /// Returns the stable audit spelling for this authority source.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Maximum => "maximum",
            Self::Narrowed => "narrowed",
        }
    }
}

/// Access granted to one normalized host path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SandboxPathAccess {
    /// The workload can read but cannot modify the path.
    ReadOnly,
    /// The workload can read and modify the path.
    ReadWrite,
}

/// Trusted kind of the existing object on which confinement is enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SandboxPathKind {
    /// A regular file.
    File,
    /// A directory and, where the backend supports it, its descendants.
    Directory,
    /// A Unix-domain socket node.
    UnixSocket,
    /// Another existing filesystem object kind.
    Other,
    /// Legacy canonical evidence that did not include object metadata.
    Unknown,
}

/// One deterministic host-path authority grant shared by sandbox backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxPathGrant {
    /// Canonical target requested by the authorized effect or maximum scope.
    pub(crate) canonical_path: String,
    /// Canonical existing object on which the backend enforces authority.
    pub(crate) enforcement_path: String,
    /// Trusted kind of the enforcement object.
    pub(crate) kind: SandboxPathKind,
    /// Access granted to the workload.
    pub(crate) access: SandboxPathAccess,
}

/// Effective confinement policy after maximum-authority normalization and
/// optional complete-effect narrowing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectiveSandboxPolicy {
    /// Canonical working directory used inside the sandbox.
    pub(crate) working_directory: String,
    /// Deterministically ordered host-path grants.
    pub(crate) grants: Vec<SandboxPathGrant>,
    /// Whether grants use maximum or narrowed authority.
    pub(crate) authority_source: SandboxAuthoritySource,
    /// Effective backend-neutral network mode.
    pub(crate) network: SandboxNetworkMode,
    /// Effective minimal environment policy.
    pub(crate) environment: SandboxEnvironmentPolicy,
}

/// Redacted facts suitable for status and audit records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxAuditSummary {
    /// Backend that compiled the redacted plan summary.
    pub(crate) backend: SandboxBackend,
    /// Fixed runtime-profile version.
    pub(crate) runtime_profile_version: &'static str,
    /// Whether complete effects narrowed maximum authority.
    pub(crate) authority_source: SandboxAuthoritySource,
    /// Number of read-only command-authority grants.
    pub(crate) read_only_grant_count: usize,
    /// Number of writable command-authority grants.
    pub(crate) read_write_grant_count: usize,
    /// Effective backend-neutral network mode.
    pub(crate) network: SandboxNetworkMode,
    /// Stable normalized launch-plan digest.
    pub(crate) plan_sha256: String,
}

/// Fully typed sandbox process plan consumed by pane and native transports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxLaunchPlan {
    /// Backend that compiled the launch plan.
    pub(crate) backend: SandboxBackend,
    /// Absolute backend executable path in the pane environment.
    pub(crate) executable: String,
    /// Deterministic backend argv excluding argv[0].
    pub(crate) arguments: Vec<String>,
    /// Fixed command-file path visible to the child shell.
    pub(crate) sandbox_command_path: String,
    /// Canonical working directory visible to the child shell.
    pub(crate) sandbox_working_directory: String,
    /// Redacted plan facts for audit and diagnostics.
    pub(crate) audit_summary: SandboxAuditSummary,
}

/// Compatibility name for Bubblewrap-specific owners during backend
/// generalization.
pub(crate) type BubblewrapLaunchPlan = SandboxLaunchPlan;

/// Deterministic pane-shell probe for the fixed Bubblewrap runtime profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BubblewrapCapabilityProbePlan {
    /// Absolute Bubblewrap executable path in the pane environment.
    pub(crate) executable: String,
    /// Deterministic Bubblewrap argv excluding argv[0].
    pub(crate) arguments: Vec<String>,
    /// Exact stdout emitted only after every probe assertion succeeds.
    pub(crate) expected_stdout: &'static str,
    /// Digest of the pane identity and configured group mappings under test.
    pub(crate) identity_sha256: String,
    /// Digest of the protected effective environment mapping.
    pub(crate) environment_sha256: String,
    /// Stable digest of the executable and arguments.
    pub(crate) probe_sha256: String,
}

/// Cache identity for one successful pane-environment capability probe.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BubblewrapCapabilityCacheKey {
    /// Pane whose registered shell transaction executed the probe.
    pub(crate) pane_id: String,
    /// Bootstrap-derived identity of the pane environment.
    pub(crate) pane_environment_signature: String,
    /// Configuration generation that selected the executable and profile.
    pub(crate) config_generation: u64,
    /// Absolute Bubblewrap path tested by the probe.
    pub(crate) executable: String,
    /// Absolute Bubblewrap executable selected by configuration.
    pub(crate) bubblewrap_executable: String,
    /// Digest of the exact resolved native identity exercised by the probe.
    pub(crate) identity_sha256: String,
    /// Digest of the protected effective environment mapping.
    pub(crate) environment_sha256: String,
    /// Fixed runtime-profile version exercised by the probe.
    pub(crate) runtime_profile_version: &'static str,
    /// Digest of the exact probe plan that succeeded.
    pub(crate) probe_sha256: String,
}

/// Verified Bubblewrap capability in one exact pane environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BubblewrapCapability {
    /// Exact cache identity that must match before capability reuse.
    pub(crate) cache_key: BubblewrapCapabilityCacheKey,
}

/// Validated Bubblewrap lifecycle evidence captured outside workload output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BubblewrapStatus {
    /// Bubblewrap child process identity, when process creation was reported.
    pub(crate) child_pid: Option<u32>,
    /// Payload exit status, which proves payload exec succeeded when present.
    pub(crate) exit_code: Option<i32>,
}

/// Backend-tagged trusted lifecycle evidence captured outside workload output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxLifecycleStatus {
    /// Ordered status documents emitted by Bubblewrap.
    Bubblewrap(BubblewrapStatus),
}

impl SandboxLifecycleStatus {
    /// Returns the trusted child process identity when the backend reported it.
    pub(crate) const fn child_pid(self) -> Option<u32> {
        match self {
            Self::Bubblewrap(status) => status.child_pid,
        }
    }

    /// Returns the trusted payload exit status when execution was established.
    pub(crate) const fn exit_code(self) -> Option<i32> {
        match self {
            Self::Bubblewrap(status) => status.exit_code,
        }
    }
}

/// Parses lifecycle evidence through the selected backend contract.
pub(crate) fn parse_sandbox_lifecycle_status(
    backend: SandboxBackend,
    status: &str,
) -> Result<SandboxLifecycleStatus, SandboxCompileError> {
    match backend {
        SandboxBackend::Bubblewrap => {
            parse_bubblewrap_status(status).map(SandboxLifecycleStatus::Bubblewrap)
        }
        SandboxBackend::Seatbelt => Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            "Seatbelt lifecycle evidence is unavailable before runtime integration",
        )),
    }
}

/// Parses ordered JSON status documents emitted by Bubblewrap.
pub(crate) fn parse_bubblewrap_status(
    status: &str,
) -> Result<BubblewrapStatus, SandboxCompileError> {
    let mut parsed = BubblewrapStatus {
        child_pid: None,
        exit_code: None,
    };
    for line in status.lines().filter(|line| !line.trim().is_empty()) {
        let value = serde_json::from_str::<serde_json::Value>(line).map_err(|_| {
            SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "Bubblewrap status contains malformed JSON",
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "Bubblewrap status document must be a JSON object",
            )
        })?;
        match (object.get("child-pid"), object.get("exit-code")) {
            (Some(child_pid), None) if parsed.child_pid.is_none() && parsed.exit_code.is_none() => {
                let child_pid = child_pid
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok());
                let child_pid = child_pid.filter(|value| *value > 0).ok_or_else(|| {
                    SandboxCompileError::new(
                        SandboxCompileErrorKind::InvalidInput,
                        "Bubblewrap child-pid status must be a positive u32",
                    )
                })?;
                parsed.child_pid = Some(child_pid);
            }
            (None, Some(exit_code)) if parsed.child_pid.is_some() && parsed.exit_code.is_none() => {
                let exit_code = exit_code
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok());
                parsed.exit_code = Some(exit_code.ok_or_else(|| {
                    SandboxCompileError::new(
                        SandboxCompileErrorKind::InvalidInput,
                        "Bubblewrap exit-code status must be an i32",
                    )
                })?);
            }
            _ => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "Bubblewrap status documents are duplicate, unknown, or out of order",
                ));
            }
        }
    }
    Ok(parsed)
}

/// Stable failure categories emitted before a workload is launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxCompileErrorKind {
    /// The original permission evaluation is not authorized.
    Unauthorized,
    /// Filesystem authority was not resolved by the pane shell.
    UnresolvedAuthority,
    /// A required path lacks trusted canonical evidence.
    UnresolvedEffectPath,
    /// A complete effect requested access outside maximum authority.
    EffectOutsideAuthority,
    /// Configuration would expose a forbidden host path.
    ForbiddenHostPath,
    /// The command requires an unsupported sandbox capability.
    UnsupportedRequirement,
    /// A typed path or executable violates launch-plan invariants.
    InvalidInput,
    /// Bubblewrap did not satisfy the fixed runtime-profile probe.
    CapabilityProbeFailed,
}

impl SandboxCompileErrorKind {
    /// Returns whether this preparation failure may offer an exact
    /// user-approved unsandboxed retry for an originally prompted action.
    pub(crate) const fn approval_fallback_eligible(self) -> bool {
        matches!(self, Self::UnsupportedRequirement)
    }

    /// Returns the stable diagnostic spelling used by fallback evidence.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::UnresolvedAuthority => "unresolved_authority",
            Self::UnresolvedEffectPath => "unresolved_effect_path",
            Self::EffectOutsideAuthority => "effect_outside_authority",
            Self::ForbiddenHostPath => "forbidden_host_path",
            Self::UnsupportedRequirement => "unsupported_requirement",
            Self::InvalidInput => "invalid_input",
            Self::CapabilityProbeFailed => "capability_probe_failed",
        }
    }
}

/// Fail-closed Bubblewrap policy-compilation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxCompileError {
    kind: SandboxCompileErrorKind,
    message: String,
}

impl SandboxCompileError {
    fn new(kind: SandboxCompileErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the stable machine-readable failure category.
    pub(crate) const fn kind(&self) -> SandboxCompileErrorKind {
        self.kind
    }

    /// Returns the redacted actionable diagnostic.
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SandboxCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SandboxCompileError {}

/// Dispatches one backend-tagged request to its deterministic compiler.
pub(crate) fn compile_sandbox_launch_plan(
    request: SandboxCompileRequest<'_>,
) -> Result<SandboxLaunchPlan, SandboxCompileError> {
    match request {
        SandboxCompileRequest::Bubblewrap(request) => compile_bubblewrap_launch_plan(request),
    }
}

/// Compiles one authorized command into a deterministic Bubblewrap launch
/// plan without performing filesystem or process I/O.
pub(crate) fn compile_bubblewrap_launch_plan(
    request: BubblewrapCompileRequest<'_>,
) -> Result<BubblewrapLaunchPlan, SandboxCompileError> {
    validate_request(&request)?;
    let sandbox_home = sandbox_home_path(&request.identity.user_name);
    let policy = project_policy_into_synthetic_home(
        effective_sandbox_policy(&request)?,
        request.pane_home_directory,
        &sandbox_home,
    );
    let arguments = bubblewrap_arguments(&request, &policy);
    let plan_sha256 = launch_plan_sha256(&request.config.executable, &arguments);
    let read_only_grant_count = policy
        .grants
        .iter()
        .filter(|grant| grant.access == SandboxPathAccess::ReadOnly)
        .count();
    let read_write_grant_count = policy
        .grants
        .iter()
        .filter(|grant| grant.access == SandboxPathAccess::ReadWrite)
        .count();
    Ok(BubblewrapLaunchPlan {
        backend: SandboxBackend::Bubblewrap,
        executable: request.config.executable.clone(),
        arguments,
        sandbox_command_path: SANDBOX_COMMAND_PATH.to_string(),
        sandbox_working_directory: policy.working_directory.clone(),
        audit_summary: SandboxAuditSummary {
            backend: SandboxBackend::Bubblewrap,
            runtime_profile_version: BUBBLEWRAP_RUNTIME_PROFILE_VERSION,
            authority_source: policy.authority_source,
            read_only_grant_count,
            read_write_grant_count,
            network: policy.network,
            plan_sha256,
        },
    })
}

/// Builds a deterministic pane-environment probe for every facility used by
/// the fixed Bubblewrap runtime profile.
///
/// The sentinel deliberately omits a line terminator because pane PTYs may
/// translate LF output to CRLF. Transaction framing supplies the output
/// boundary, so exact matching can still reject every additional byte.
#[cfg(test)]
pub(crate) fn bubblewrap_capability_probe_plan(
    config: &BubblewrapConfig,
    child_shell_path: &str,
) -> Result<BubblewrapCapabilityProbePlan, SandboxCompileError> {
    let environment = identity::current_process_environment_signature()?;
    let identity = resolve_sandbox_identity(&config.group_whitelist, &environment)?;
    let request =
        mez_agent::shell::PaneEnvironmentRequest::new(config.env_whitelist.requested_names.clone())
            .map_err(|error| {
                SandboxCompileError::new(SandboxCompileErrorKind::InvalidInput, error.message())
            })?;
    let evidence = mez_agent::shell::PaneEnvironmentEvidence::restrictive(&request, "test_default");
    bubblewrap_capability_probe_plan_for_identity(config, child_shell_path, &identity, &evidence)
}

/// Builds the deterministic capability probe for one already resolved exact
/// identity so runtime cache lookup and workload compilation share it.
pub(crate) fn bubblewrap_capability_probe_plan_for_identity(
    config: &BubblewrapConfig,
    child_shell_path: &str,
    identity: &ResolvedSandboxIdentity,
    environment_evidence: &mez_agent::shell::PaneEnvironmentEvidence,
) -> Result<BubblewrapCapabilityProbePlan, SandboxCompileError> {
    validate_printable_absolute_path(&config.executable, "Bubblewrap executable")?;
    validate_canonical_path(child_shell_path, "sandbox child shell")?;
    if !Path::new(child_shell_path).starts_with("/bin")
        && !Path::new(child_shell_path).starts_with("/usr")
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "initial Bubblewrap profile supports child shells under /bin or /usr only",
        ));
    }
    let user_id = identity.user_id;
    let group_id = identity.primary_group_id;
    let sandbox_home = sandbox_home_path(&identity.user_name);
    let executable_path = sandbox_command_path(environment_evidence);
    let expected_stdout = "mez-bubblewrap-capability-v6";
    let probe_shell_path = "/bin/sh";
    let probe_script = format!(
        "uid_ok=; gid_ok=; while read -r key real effective saved filesystem; do case \"$key\" in Uid:) test \"$effective\" = '{user_id}' && uid_ok=1 ;; Gid:) test \"$effective\" = '{group_id}' && gid_ok=1 ;; esac; done < /proc/self/status; test ! -e /etc/passwd && test \"$uid_ok\" = 1 && test \"$gid_ok\" = 1 && test -c /dev/null && test -w /tmp && test -w \"$HOME\" && test -z \"${{SSH_AUTH_SOCK+x}}\" && printf '%s' '{expected_stdout}'"
    );
    let bubblewrap_arguments = vec![
        "--unshare-user",
        "--uid",
        &user_id.to_string(),
        "--gid",
        &group_id.to_string(),
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup",
        "--unshare-net",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--disable-userns",
        "--clearenv",
        "--tmpfs",
        "/",
        "--ro-bind-try",
        "/usr",
        "/usr",
        "--ro-bind-try",
        "/bin",
        "/bin",
        "--ro-bind-try",
        "/lib",
        "/lib",
        "--ro-bind-try",
        "/lib64",
        "/lib64",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/home",
        "--tmpfs",
        sandbox_home.as_str(),
        "--setenv",
        "HOME",
        sandbox_home.as_str(),
        "--setenv",
        "PATH",
        executable_path,
        "--setenv",
        "TMPDIR",
        "/tmp",
        "--",
        probe_shell_path,
        "-c",
        probe_script.as_str(),
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let mut probe_identity_arguments = bubblewrap_arguments.clone();
    probe_identity_arguments.push(child_shell_path.to_string());
    let probe_sha256 = argument_plan_sha256(
        b"mez-bubblewrap-capability-probe-v4\0",
        &config.executable,
        &probe_identity_arguments,
    );
    Ok(BubblewrapCapabilityProbePlan {
        executable: config.executable.clone(),
        arguments: bubblewrap_arguments,
        expected_stdout,
        identity_sha256: identity.identity_sha256.clone(),
        environment_sha256: environment_evidence.value_sha256.clone(),
        probe_sha256,
    })
}

/// Validates one completed pane-shell capability probe and returns its exact
/// cache identity. Failed or ambiguous output never enables the backend.
pub(crate) fn parse_bubblewrap_capability_probe(
    pane_id: &str,
    pane_environment_signature: &str,
    config_generation: u64,
    plan: &BubblewrapCapabilityProbePlan,
    exit_code: i32,
    stdout: &str,
) -> Result<BubblewrapCapability, SandboxCompileError> {
    if pane_environment_signature.is_empty()
        || pane_environment_signature
            .bytes()
            .any(|byte| byte.is_ascii_control())
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            "Bubblewrap capability caching requires a printable pane environment signature",
        ));
    }
    if exit_code != 0 || stdout != plan.expected_stdout {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::CapabilityProbeFailed,
            "Bubblewrap did not satisfy the fixed runtime-profile capability probe",
        ));
    }
    Ok(BubblewrapCapability {
        cache_key: bubblewrap_capability_cache_key(
            pane_id,
            pane_environment_signature,
            config_generation,
            plan,
        )?,
    })
}

/// Builds the exact cache identity for a deterministic capability probe.
pub(crate) fn bubblewrap_capability_cache_key(
    pane_id: &str,
    pane_environment_signature: &str,
    config_generation: u64,
    plan: &BubblewrapCapabilityProbePlan,
) -> Result<BubblewrapCapabilityCacheKey, SandboxCompileError> {
    if pane_id.is_empty()
        || pane_id.bytes().any(|byte| byte.is_ascii_control())
        || pane_environment_signature.is_empty()
        || pane_environment_signature
            .bytes()
            .any(|byte| byte.is_ascii_control())
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            "Bubblewrap capability caching requires a printable pane environment signature",
        ));
    }
    Ok(BubblewrapCapabilityCacheKey {
        pane_id: pane_id.to_string(),
        pane_environment_signature: pane_environment_signature.to_string(),
        config_generation,
        executable: plan.executable.clone(),
        bubblewrap_executable: plan.executable.clone(),
        identity_sha256: plan.identity_sha256.clone(),
        environment_sha256: plan.environment_sha256.clone(),
        runtime_profile_version: BUBBLEWRAP_RUNTIME_PROFILE_VERSION,
        probe_sha256: plan.probe_sha256.clone(),
    })
}

fn validate_request(request: &BubblewrapCompileRequest<'_>) -> Result<(), SandboxCompileError> {
    let expected_probe = bubblewrap_capability_probe_plan_for_identity(
        request.config,
        request.child_shell_path,
        &request.identity,
        request.environment_evidence,
    )?;
    if request.capability.cache_key.pane_environment_signature != request.pane_environment_signature
        || request.capability.cache_key.executable != request.config.executable
        || request.capability.cache_key.bubblewrap_executable != request.config.executable
        || request.capability.cache_key.identity_sha256 != request.identity.identity_sha256
        || request.capability.cache_key.environment_sha256
            != request.environment_evidence.value_sha256
        || request.capability.cache_key.runtime_profile_version
            != BUBBLEWRAP_RUNTIME_PROFILE_VERSION
        || request.capability.cache_key.probe_sha256 != expected_probe.probe_sha256
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::CapabilityProbeFailed,
            "Bubblewrap capability does not match the active pane environment, executable, or runtime profile",
        ));
    }
    if request.permission_evaluation.decision != RuleDecision::Allow {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::Unauthorized,
            "sandbox compilation requires an explicitly allowed permission evaluation",
        ));
    }
    if request.maximum_authority.resolution_status == PathResolutionStatus::Unresolved {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnresolvedAuthority,
            "sandbox compilation requires trusted resolved path authority",
        ));
    }
    validate_canonical_path(
        &request.maximum_authority.current_directory,
        "sandbox working directory",
    )?;
    validate_printable_absolute_path(&request.config.executable, "Bubblewrap executable")?;
    validate_canonical_path(request.command_file_host_path, "sandbox command file")?;
    validate_canonical_path(request.child_shell_path, "sandbox child shell")?;
    if let Some(managed_home) = request.managed_home {
        validate_canonical_path(
            &managed_home.host_path.to_string_lossy(),
            "managed Bubblewrap home",
        )?;
        validate_canonical_path(
            &managed_home.passwd_path.to_string_lossy(),
            "managed Bubblewrap passwd record",
        )?;
        validate_canonical_path(
            &managed_home.group_path.to_string_lossy(),
            "managed Bubblewrap group record",
        )?;
    }
    if let Some(home) = request.pane_home_directory {
        validate_canonical_path(&home.to_string_lossy(), "pane home directory")?;
    }
    if !Path::new(request.child_shell_path).starts_with("/bin")
        && !Path::new(request.child_shell_path).starts_with("/usr")
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "initial Bubblewrap profile supports child shells under /bin or /usr only",
        ));
    }
    if request.stateful {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "stateful shell actions are unsupported by per-command Bubblewrap isolation",
        ));
    }
    if request.interactive {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "interactive shell actions are unsupported by the initial Bubblewrap profile",
        ));
    }
    let effects = &request.permission_evaluation.effects;
    if effects.credentials {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "credential access requires a dedicated sandbox credential broker",
        ));
    }
    if effects.process_control || effects.privilege_change {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "host process control and privilege changes are unsupported in Bubblewrap mode",
        ));
    }
    match request.config.unavailable {
        SandboxUnavailablePolicy::Fail => {}
    }
    match request.config.network {
        SandboxNetworkMode::Isolated | SandboxNetworkMode::Connected => {}
    }
    match request.config.environment {
        SandboxEnvironmentPolicy::Minimal => {}
    }
    Ok(())
}

fn effective_sandbox_policy(
    request: &BubblewrapCompileRequest<'_>,
) -> Result<EffectiveSandboxPolicy, SandboxCompileError> {
    validate_maximum_authority(request.maximum_authority)?;
    let evaluation = request.permission_evaluation;
    let (grants, authority_source) = if request.preserve_maximum_authority {
        (
            maximum_grants(request.maximum_authority),
            SandboxAuthoritySource::Maximum,
        )
    } else if let Some(effects) = evaluation.confinement_effects.as_ref() {
        (
            narrowed_grants(request.maximum_authority, effects)?,
            SandboxAuthoritySource::Narrowed,
        )
    } else {
        (
            maximum_grants(request.maximum_authority),
            SandboxAuthoritySource::Maximum,
        )
    };
    Ok(EffectiveSandboxPolicy {
        working_directory: request.maximum_authority.current_directory.clone(),
        grants,
        authority_source,
        network: if matches!(request.network_policy, NetworkPolicy::Allow)
            || (evaluation.effects.network
                && matches!(request.network_policy, NetworkPolicy::Prompt))
        {
            SandboxNetworkMode::Connected
        } else {
            request.config.network
        },
        environment: request.config.environment,
    })
}

fn validate_maximum_authority(authority: &PathScopes) -> Result<(), SandboxCompileError> {
    for path in authority.read_scopes.iter().chain(&authority.write_scopes) {
        validate_canonical_path(path, "maximum sandbox authority")?;
        if path == "/" {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "Bubblewrap authority must not expose host root",
            ));
        }
        if path == "/home" {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "Bubblewrap authority must not expose the multi-user home root",
            ));
        }
    }
    for path in &authority.read_scopes {
        if path_overlaps(path, "/run/user") || path_overlaps(path, "/var/run") {
            validate_ipc_read_scope(authority, path)?;
        }
    }
    for path in &authority.write_scopes {
        if path_overlaps(path, "/run/user") || path_overlaps(path, "/var/run") {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "Bubblewrap write authority must not expose host user-runtime or IPC paths",
            ));
        }
        if [
            "/usr", "/bin", "/lib", "/lib64", "/etc", "/proc", "/dev", "/run", "/tmp",
        ]
        .iter()
        .any(|protected| path_overlaps(path, protected))
        {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "Bubblewrap write authority overlaps the fixed runtime projection",
            ));
        }
    }
    Ok(())
}

/// Allows a protected IPC read scope only when trusted resolver evidence
/// identifies its exact canonical object as a Unix-domain socket.
fn validate_ipc_read_scope(authority: &PathScopes, path: &str) -> Result<(), SandboxCompileError> {
    if authority_path_kind(authority, path) != SandboxPathKind::UnixSocket {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "sandbox IPC read authority requires trusted Unix-socket evidence",
        ));
    }
    Ok(())
}

fn maximum_grants(authority: &PathScopes) -> Vec<SandboxPathGrant> {
    let mut grants = authority
        .read_scopes
        .iter()
        .map(|path| authority_path_grant(authority, path, SandboxPathAccess::ReadOnly))
        .collect::<Vec<_>>();
    grants.extend(
        authority
            .write_scopes
            .iter()
            .map(|path| authority_path_grant(authority, path, SandboxPathAccess::ReadWrite)),
    );
    normalize_grants(grants)
}

fn authority_path_grant(
    authority: &PathScopes,
    path: &str,
    access: SandboxPathAccess,
) -> SandboxPathGrant {
    let evidence = authority
        .path_evidence
        .values()
        .find(|evidence| evidence.canonical_path == path);
    let enforcement_path = evidence
        .filter(|evidence| evidence.kind == ResolvedPathKind::CreateTarget)
        .map(|evidence| evidence.nearest_existing_parent.clone())
        .unwrap_or_else(|| path.to_string());
    SandboxPathGrant {
        canonical_path: path.to_string(),
        enforcement_path,
        kind: evidence
            .map(|evidence| sandbox_path_kind(evidence.object_kind))
            .unwrap_or(SandboxPathKind::Unknown),
        access,
    }
}

fn narrowed_grants(
    authority: &PathScopes,
    effects: &EffectiveCommandEffects,
) -> Result<Vec<SandboxPathGrant>, SandboxCompileError> {
    let mut grants = Vec::new();
    for path in &effects.reads {
        let resolved = resolve_effect_path(path, authority, false)?;
        grants.push(SandboxPathGrant {
            canonical_path: resolved.canonical_path,
            enforcement_path: resolved.enforcement_path,
            kind: resolved.kind,
            access: SandboxPathAccess::ReadOnly,
        });
    }
    for path in effects
        .writes
        .iter()
        .chain(&effects.creates)
        .chain(&effects.deletes)
        .chain(&effects.touches)
    {
        let resolved = resolve_effect_path(path, authority, true)?;
        grants.push(SandboxPathGrant {
            canonical_path: resolved.canonical_path,
            enforcement_path: resolved.enforcement_path,
            kind: resolved.kind,
            access: SandboxPathAccess::ReadWrite,
        });
    }
    Ok(normalize_grants(grants))
}

struct ResolvedEffectPath {
    canonical_path: String,
    enforcement_path: String,
    kind: SandboxPathKind,
}

fn resolve_effect_path(
    requested: &str,
    authority: &PathScopes,
    write: bool,
) -> Result<ResolvedEffectPath, SandboxCompileError> {
    if requested.is_empty() || requested.contains('\0') || requested.starts_with('~') {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnresolvedEffectPath,
            "sandbox effect path is empty, unexpanded, or contains a NUL byte",
        ));
    }
    let normalized = lexical_absolute_path(&authority.current_directory, requested)?;
    let evidence = authority
        .path_evidence
        .get(requested)
        .or_else(|| authority.path_evidence.get(&normalized))
        .or_else(|| {
            authority
                .path_evidence
                .values()
                .find(|evidence| evidence.canonical_path == normalized)
        });
    let exact_authority_path = authority
        .read_scopes
        .iter()
        .chain(&authority.write_scopes)
        .any(|scope| scope == &normalized)
        || normalized == authority.current_directory;
    let (canonical_target, enforcement_path, kind) = match evidence {
        Some(evidence) => resolved_effect_mount(evidence, write)?,
        None if exact_authority_path => (normalized.clone(), normalized, SandboxPathKind::Unknown),
        None => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::UnresolvedEffectPath,
                "complete sandbox effects require trusted canonical path evidence",
            ));
        }
    };
    let allowed_scopes = if write {
        &authority.write_scopes
    } else {
        &authority.read_scopes
    };
    if !allowed_scopes
        .iter()
        .any(|scope| Path::new(&canonical_target).starts_with(scope))
        || !allowed_scopes
            .iter()
            .any(|scope| Path::new(&enforcement_path).starts_with(scope))
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::EffectOutsideAuthority,
            "complete sandbox effects request a path outside maximum authority",
        ));
    }
    Ok(ResolvedEffectPath {
        canonical_path: canonical_target,
        enforcement_path,
        kind,
    })
}

fn resolved_effect_mount(
    evidence: &ResolvedPathEvidence,
    write: bool,
) -> Result<(String, String, SandboxPathKind), SandboxCompileError> {
    validate_canonical_path(&evidence.canonical_path, "resolved effect target")?;
    validate_canonical_path(
        &evidence.nearest_existing_parent,
        "resolved effect existing parent",
    )?;
    if !write && evidence.kind == ResolvedPathKind::CreateTarget {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnresolvedEffectPath,
            "read effects cannot target a path that did not exist during resolution",
        ));
    }
    let enforcement_path = if write && evidence.kind == ResolvedPathKind::CreateTarget {
        evidence.nearest_existing_parent.clone()
    } else {
        evidence.canonical_path.clone()
    };
    Ok((
        evidence.canonical_path.clone(),
        enforcement_path,
        sandbox_path_kind(evidence.object_kind),
    ))
}

fn authority_path_kind(authority: &PathScopes, path: &str) -> SandboxPathKind {
    authority
        .path_evidence
        .values()
        .find(|evidence| evidence.canonical_path == path)
        .map(|evidence| sandbox_path_kind(evidence.object_kind))
        .unwrap_or(SandboxPathKind::Unknown)
}

const fn sandbox_path_kind(kind: ResolvedPathObjectKind) -> SandboxPathKind {
    match kind {
        ResolvedPathObjectKind::Unknown => SandboxPathKind::Unknown,
        ResolvedPathObjectKind::File => SandboxPathKind::File,
        ResolvedPathObjectKind::Directory => SandboxPathKind::Directory,
        ResolvedPathObjectKind::UnixSocket => SandboxPathKind::UnixSocket,
        ResolvedPathObjectKind::Other => SandboxPathKind::Other,
    }
}

fn normalize_grants(grants: Vec<SandboxPathGrant>) -> Vec<SandboxPathGrant> {
    let mut by_enforcement_path = BTreeMap::<String, SandboxPathGrant>::new();
    for grant in grants {
        by_enforcement_path
            .entry(grant.enforcement_path.clone())
            .and_modify(|existing| {
                if grant.access > existing.access {
                    *existing = grant.clone();
                }
            })
            .or_insert(grant);
    }
    let mut ordered = by_enforcement_path.into_values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        path_depth(&left.enforcement_path)
            .cmp(&path_depth(&right.enforcement_path))
            .then_with(|| left.enforcement_path.cmp(&right.enforcement_path))
            .then_with(|| left.access.cmp(&right.access))
    });
    let mut normalized: Vec<SandboxPathGrant> = Vec::new();
    for grant in ordered {
        let covered = normalized.iter().any(|parent| {
            Path::new(&grant.enforcement_path).starts_with(&parent.enforcement_path)
                && parent.access == grant.access
        });
        if !covered {
            normalized.push(grant);
        }
    }
    normalized
}

/// Rehomes only explicitly authorized paths beneath the pane home below the
/// synthetic home while preserving every other authorized destination.
fn project_policy_into_synthetic_home(
    mut policy: EffectiveSandboxPolicy,
    pane_home_directory: Option<&Path>,
    sandbox_home: &str,
) -> EffectiveSandboxPolicy {
    let Some(pane_home_directory) = pane_home_directory else {
        return policy;
    };
    policy.working_directory =
        synthetic_home_path(&policy.working_directory, pane_home_directory, sandbox_home);
    policy
}

/// Returns the synthetic destination for an authorized path within the pane home.
fn synthetic_home_path(path: &str, pane_home_directory: &Path, sandbox_home: &str) -> String {
    Path::new(path)
        .strip_prefix(pane_home_directory)
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                sandbox_home.to_string()
            } else {
                Path::new(sandbox_home)
                    .join(relative)
                    .to_string_lossy()
                    .into_owned()
            }
        })
        .unwrap_or_else(|_| path.to_string())
}

/// Returns the synthetic home path associated with one validated pane user.
fn sandbox_home_path(user_name: &str) -> String {
    format!("/home/{user_name}")
}

/// Rehomes fixed managed-state templates below the active synthetic home.
fn rehome_managed_path(path: &str, sandbox_home: &str) -> String {
    Path::new(path)
        .strip_prefix(SANDBOX_HOME)
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                sandbox_home.to_string()
            } else {
                Path::new(sandbox_home)
                    .join(relative)
                    .to_string_lossy()
                    .into_owned()
            }
        })
        .unwrap_or_else(|_| path.to_string())
}

fn bubblewrap_arguments(
    request: &BubblewrapCompileRequest<'_>,
    policy: &EffectiveSandboxPolicy,
) -> Vec<String> {
    let user_id = request.identity.user_id;
    let group_id = request.identity.primary_group_id;
    let sandbox_home = sandbox_home_path(&request.identity.user_name);
    let xdg_cache_home = format!("{sandbox_home}/.cache");
    let xdg_config_home = format!("{sandbox_home}/.config");
    let xdg_data_home = format!("{sandbox_home}/.local/share");
    let xdg_state_home = format!("{sandbox_home}/.local/state");
    let mut arguments = vec![
        "--json-status-fd",
        "3",
        "--unshare-user",
        "--uid",
        &user_id.to_string(),
        "--gid",
        &group_id.to_string(),
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--disable-userns",
        "--clearenv",
        "--tmpfs",
        "/",
        "--ro-bind-try",
        "/usr",
        "/usr",
        "--ro-bind-try",
        "/bin",
        "/bin",
        "--ro-bind-try",
        "/lib",
        "/lib",
        "--ro-bind-try",
        "/lib64",
        "/lib64",
        "--dir",
        "/etc",
        "--dir",
        "/etc/ssl",
        "--ro-bind-try",
        "/etc/alternatives",
        "/etc/alternatives",
        "--ro-bind-try",
        "/etc/ld.so.cache",
        "/etc/ld.so.cache",
        "--ro-bind-try",
        "/etc/ssl/certs",
        "/etc/ssl/certs",
        "--ro-bind-try",
        "/etc/resolv.conf",
        "/etc/resolv.conf",
        "--ro-bind-try",
        "/etc/nsswitch.conf",
        "/etc/nsswitch.conf",
        "--ro-bind-try",
        "/etc/hosts",
        "/etc/hosts",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/home",
        "--dir",
        "/run",
        "--dir",
        "/var",
        "--symlink",
        "/run",
        "/var/run",
        "--dir",
        "/run/mez",
        "--dir",
        policy.working_directory.as_str(),
        "--ro-bind",
        request.command_file_host_path,
        SANDBOX_COMMAND_PATH,
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    if policy.network == SandboxNetworkMode::Isolated {
        arguments.insert(7, "--unshare-net".to_string());
    }
    if let Some(managed_home) = request.managed_home {
        arguments.push("--bind".to_string());
        arguments.push(managed_home.host_path.to_string_lossy().into_owned());
        arguments.push(sandbox_home.clone());
        arguments.push("--ro-bind".to_string());
        arguments.push(managed_home.passwd_path.to_string_lossy().into_owned());
        arguments.push("/etc/passwd".to_string());
        arguments.push("--ro-bind".to_string());
        arguments.push(managed_home.group_path.to_string_lossy().into_owned());
        arguments.push("/etc/group".to_string());
    } else {
        arguments.push("--tmpfs".to_string());
        arguments.push(sandbox_home.clone());
    }
    for grant in &policy.grants {
        arguments.push(
            match grant.access {
                SandboxPathAccess::ReadOnly => "--ro-bind",
                SandboxPathAccess::ReadWrite => "--bind",
            }
            .to_string(),
        );
        arguments.push(grant.enforcement_path.clone());
        arguments.push(
            request
                .pane_home_directory
                .map(|pane_home| {
                    synthetic_home_path(&grant.enforcement_path, pane_home, &sandbox_home)
                })
                .unwrap_or_else(|| grant.enforcement_path.clone()),
        );
    }
    arguments.extend(
        [
            "--setenv",
            "GIT_CONFIG_NOSYSTEM",
            "1",
            "--setenv",
            "GIT_CONFIG_GLOBAL",
            "/dev/null",
        ]
        .into_iter()
        .map(str::to_string),
    );
    for (name, value) in &request.environment_evidence.values {
        arguments.extend(
            ["--setenv", name.as_str(), value.as_str()]
                .into_iter()
                .map(str::to_string),
        );
    }
    if let (Some(name), Some(email)) = (
        request.config.git_user_name.as_deref(),
        request.config.git_user_email.as_deref(),
    ) {
        for (key, value) in [("user.name", name), ("user.email", email)] {
            let index = if key == "user.name" { "0" } else { "1" };
            arguments.extend(
                [
                    "--setenv",
                    &format!("GIT_CONFIG_KEY_{index}"),
                    key,
                    "--setenv",
                    &format!("GIT_CONFIG_VALUE_{index}"),
                    value,
                ]
                .into_iter()
                .map(str::to_string),
            );
        }
        arguments.extend(
            ["--setenv", "GIT_CONFIG_COUNT", "2"]
                .into_iter()
                .map(str::to_string),
        );
    }
    let executable_path = sandbox_command_path(request.environment_evidence);
    arguments.extend(
        [
            "--chdir",
            policy.working_directory.as_str(),
            "--setenv",
            "HOME",
            sandbox_home.as_str(),
            "--setenv",
            "XDG_CACHE_HOME",
            xdg_cache_home.as_str(),
            "--setenv",
            "XDG_CONFIG_HOME",
            xdg_config_home.as_str(),
            "--setenv",
            "XDG_DATA_HOME",
            xdg_data_home.as_str(),
            "--setenv",
            "XDG_STATE_HOME",
            xdg_state_home.as_str(),
            "--setenv",
            "PATH",
            executable_path,
            "--setenv",
            "TMPDIR",
            "/tmp",
            "--setenv",
            "LANG",
            "C.UTF-8",
            "--setenv",
            "LC_ALL",
            "C.UTF-8",
            "--setenv",
            "USER",
            request.identity.user_name.as_str(),
            "--setenv",
            "LOGNAME",
            request.identity.user_name.as_str(),
            "--setenv",
            "SHELL",
            request.child_shell_path,
            "--",
            request.child_shell_path,
            SANDBOX_COMMAND_PATH,
        ]
        .into_iter()
        .map(str::to_string),
    );
    arguments
}

/// Selects the verified pane command-search path or the minimal fallback when
/// PATH was not configured or could not be safely resolved.
fn sandbox_command_path(environment_evidence: &mez_agent::shell::PaneEnvironmentEvidence) -> &str {
    environment_evidence
        .values
        .get("PATH")
        .map(String::as_str)
        .unwrap_or(MINIMAL_PATH)
}

fn launch_plan_sha256(executable: &str, arguments: &[String]) -> String {
    argument_plan_sha256(b"mez-bubblewrap-launch-plan-v1\0", executable, arguments)
}

fn argument_plan_sha256(domain: &[u8], executable: &str, arguments: &[String]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(executable.as_bytes());
    for argument in arguments {
        digest.update(b"\0");
        digest.update(argument.as_bytes());
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn lexical_absolute_path(base: &str, requested: &str) -> Result<String, SandboxCompileError> {
    let requested = Path::new(requested);
    let combined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        PathBuf::from(base).join(requested)
    };
    let mut normalized = PathBuf::new();
    for component in combined.components() {
        match component {
            Component::RootDir => normalized.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(_) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "sandbox paths must use canonical Unix path syntax",
                ));
            }
        }
    }
    let normalized = normalized.to_string_lossy().into_owned();
    validate_canonical_path(&normalized, "sandbox effect path")?;
    Ok(normalized)
}

fn validate_printable_absolute_path(path: &str, label: &str) -> Result<(), SandboxCompileError> {
    validate_canonical_path(path, label)?;
    if path.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("{label} must be printable"),
        ));
    }
    Ok(())
}

fn validate_canonical_path(path: &str, label: &str) -> Result<(), SandboxCompileError> {
    let parsed = Path::new(path);
    if path.is_empty() || path.contains('\0') || !parsed.is_absolute() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("{label} must be a non-empty absolute path without NUL bytes"),
        ));
    }
    if parsed.components().any(|component| {
        matches!(
            component,
            Component::CurDir | Component::ParentDir | Component::Prefix(_)
        )
    }) {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("{label} must not contain lexical traversal components"),
        ));
    }
    Ok(())
}

fn path_depth(path: &str) -> usize {
    Path::new(path).components().count()
}

fn path_overlaps(left: &str, right: &str) -> bool {
    Path::new(left).starts_with(right) || Path::new(right).starts_with(left)
}

/// Reports whether one canonical mount root identifies a single user beneath
/// a Linux-style home directory rather than an unbounded multi-user ancestor.
#[cfg(test)]
mod tests;
