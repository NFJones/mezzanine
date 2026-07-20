//! Typed sandbox-policy compilation for product-owned confinement backends.
//!
//! This module converts trusted pane-shell path authority and the structured
//! permission evaluation computed for the original policy command into a
//! deterministic Bubblewrap launch plan. Authorization remains owned by the
//! permission subsystem. This compiler only narrows already-authorized
//! resource authority and never interprets command prefixes as mount grants.
//!
//! The boundary is deliberately fail-closed: unresolved paths, host-root
//! mounts, credential or process-control requirements, network requirements
//! without mediated egress, and unsupported stateful or interactive execution
//! all fail before a workload can start. Generated plans contain typed argv,
//! never user-provided Bubblewrap arguments or wrapper shell fragments.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use mez_agent::permissions::{
    EffectCompleteness, PathResolutionStatus, PathScopes, PermissionEvaluation,
    ResolvedPathEvidence, ResolvedPathKind, RuleDecision,
};
use sha2::{Digest, Sha256};

use crate::runtime::{
    BubblewrapConfig, BubblewrapNetworkMode, NetworkPolicy, SandboxEnvironmentPolicy,
    SandboxUnavailablePolicy,
};

/// Version of the fixed runtime projection emitted by this compiler.
pub(crate) const BUBBLEWRAP_RUNTIME_PROFILE_VERSION: &str = "bubblewrap-v1";

const SANDBOX_COMMAND_PATH: &str = "/run/mez/command";
/// Sentinel replaced by the pane transaction's materialized command-file
/// argument immediately before rendering the typed child launch.
pub(crate) const BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER: &str =
    "/run/mez/host-command-placeholder";
const SANDBOX_HOME: &str = "/home/mez";
const MINIMAL_PATH: &str = "/usr/bin:/bin";

/// Inputs required to compile one authorized command into a launch plan.
#[derive(Debug, Clone)]
pub(crate) struct BubblewrapCompileRequest<'a> {
    /// Typed Bubblewrap backend configuration.
    pub(crate) config: &'a BubblewrapConfig,
    /// Successful capability probe for the active pane environment.
    pub(crate) capability: BubblewrapCapability,
    /// Bootstrap-derived identity of the active pane environment.
    pub(crate) pane_environment_signature: &'a str,
    /// Effective authorization policy for network-requiring commands.
    pub(crate) network_policy: NetworkPolicy,
    /// Trusted maximum filesystem authority resolved by the pane shell.
    pub(crate) maximum_authority: &'a PathScopes,
    /// Structured evaluation computed from the original policy command.
    pub(crate) permission_evaluation: &'a PermissionEvaluation,
    /// Absolute child shell path in the pane environment.
    pub(crate) child_shell_path: &'a str,
    /// Absolute harness-owned command-file path in the pane environment.
    pub(crate) command_file_host_path: &'a str,
    /// Whether the command must mutate persistent shell state.
    pub(crate) stateful: bool,
    /// Whether the command requires direct terminal interaction.
    pub(crate) interactive: bool,
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

/// Access granted by one compiled bind mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SandboxMountAccess {
    /// The workload can read but cannot modify the mounted path.
    ReadOnly,
    /// The workload can read and modify the mounted path.
    ReadWrite,
}

/// One deterministic host-to-sandbox path projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxMount {
    /// Canonical source path in the pane environment.
    pub(crate) source: String,
    /// Destination path inside the sandbox.
    pub(crate) destination: String,
    /// Access granted to the workload.
    pub(crate) access: SandboxMountAccess,
}

/// Effective confinement policy after maximum-authority normalization and
/// optional complete-effect narrowing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectiveSandboxPolicy {
    /// Canonical working directory used inside the sandbox.
    pub(crate) working_directory: String,
    /// Deterministically ordered filesystem mounts.
    pub(crate) mounts: Vec<SandboxMount>,
    /// Whether mounts use maximum or narrowed authority.
    pub(crate) authority_source: SandboxAuthoritySource,
    /// Effective isolated network mode.
    pub(crate) network: BubblewrapNetworkMode,
    /// Effective minimal environment policy.
    pub(crate) environment: SandboxEnvironmentPolicy,
}

/// Redacted facts suitable for status and audit records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxAuditSummary {
    /// Fixed runtime-profile version.
    pub(crate) runtime_profile_version: &'static str,
    /// Whether complete effects narrowed maximum authority.
    pub(crate) authority_source: SandboxAuthoritySource,
    /// Number of read-only command-authority mounts.
    pub(crate) read_only_mount_count: usize,
    /// Number of writable command-authority mounts.
    pub(crate) read_write_mount_count: usize,
    /// Stable normalized launch-plan digest.
    pub(crate) plan_sha256: String,
}

/// Fully typed Bubblewrap process plan consumed by pane transaction rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BubblewrapLaunchPlan {
    /// Absolute Bubblewrap executable path in the pane environment.
    pub(crate) executable: String,
    /// Deterministic Bubblewrap argv excluding argv[0].
    pub(crate) arguments: Vec<String>,
    /// Fixed command-file path visible to the child shell.
    pub(crate) sandbox_command_path: String,
    /// Canonical working directory visible to the child shell.
    pub(crate) sandbox_working_directory: String,
    /// Redacted plan facts for audit and diagnostics.
    pub(crate) audit_summary: SandboxAuditSummary,
}

/// Deterministic pane-shell probe for the fixed Bubblewrap runtime profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BubblewrapCapabilityProbePlan {
    /// Absolute Bubblewrap executable path in the pane environment.
    pub(crate) executable: String,
    /// Deterministic Bubblewrap argv excluding argv[0].
    pub(crate) arguments: Vec<String>,
    /// Exact stdout emitted only after every probe assertion succeeds.
    pub(crate) expected_stdout: &'static str,
    /// Stable digest of the executable and arguments.
    pub(crate) probe_sha256: String,
}

/// Cache identity for one successful pane-environment capability probe.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BubblewrapCapabilityCacheKey {
    /// Bootstrap-derived identity of the pane environment.
    pub(crate) pane_environment_signature: String,
    /// Absolute executable path tested by the probe.
    pub(crate) executable: String,
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
    /// Network was denied by policy.
    NetworkDenied,
    /// Authorized network access requires a not-yet-available broker.
    MediatedNetworkUnavailable,
    /// The command requires an unsupported sandbox capability.
    UnsupportedRequirement,
    /// A typed path or executable violates launch-plan invariants.
    InvalidInput,
    /// Bubblewrap did not satisfy the fixed runtime-profile probe.
    CapabilityProbeFailed,
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

/// Compiles one authorized command into a deterministic Bubblewrap launch
/// plan without performing filesystem or process I/O.
pub(crate) fn compile_bubblewrap_launch_plan(
    request: BubblewrapCompileRequest<'_>,
) -> Result<BubblewrapLaunchPlan, SandboxCompileError> {
    validate_request(&request)?;
    let policy = effective_sandbox_policy(&request)?;
    let arguments = bubblewrap_arguments(&request, &policy);
    let plan_sha256 = launch_plan_sha256(&request.config.executable, &arguments);
    let read_only_mount_count = policy
        .mounts
        .iter()
        .filter(|mount| mount.access == SandboxMountAccess::ReadOnly)
        .count();
    let read_write_mount_count = policy
        .mounts
        .iter()
        .filter(|mount| mount.access == SandboxMountAccess::ReadWrite)
        .count();
    Ok(BubblewrapLaunchPlan {
        executable: request.config.executable.clone(),
        arguments,
        sandbox_command_path: SANDBOX_COMMAND_PATH.to_string(),
        sandbox_working_directory: policy.working_directory.clone(),
        audit_summary: SandboxAuditSummary {
            runtime_profile_version: BUBBLEWRAP_RUNTIME_PROFILE_VERSION,
            authority_source: policy.authority_source,
            read_only_mount_count,
            read_write_mount_count,
            plan_sha256,
        },
    })
}

/// Builds a deterministic pane-environment probe for every facility used by
/// the fixed Bubblewrap runtime profile.
pub(crate) fn bubblewrap_capability_probe_plan(
    config: &BubblewrapConfig,
    child_shell_path: &str,
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
    let expected_stdout = "mez-bubblewrap-capability-v1\n";
    let probe_script = format!(
        "test ! -e /etc/passwd && test -r /proc/self/status && test -c /dev/null && test -w /tmp && test -w \"$HOME\" && test -z \"${{SSH_AUTH_SOCK+x}}\" && printf '%s\\n' '{}'",
        expected_stdout.trim_end()
    );
    let arguments = vec![
        "--unshare-user",
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
        SANDBOX_HOME,
        "--setenv",
        "HOME",
        SANDBOX_HOME,
        "--setenv",
        "PATH",
        MINIMAL_PATH,
        "--setenv",
        "TMPDIR",
        "/tmp",
        "--",
        child_shell_path,
        "-c",
        probe_script.as_str(),
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let probe_sha256 = argument_plan_sha256(
        b"mez-bubblewrap-capability-probe-v1\0",
        &config.executable,
        &arguments,
    );
    Ok(BubblewrapCapabilityProbePlan {
        executable: config.executable.clone(),
        arguments,
        expected_stdout,
        probe_sha256,
    })
}

/// Validates one completed pane-shell capability probe and returns its exact
/// cache identity. Failed or ambiguous output never enables the backend.
pub(crate) fn parse_bubblewrap_capability_probe(
    pane_environment_signature: &str,
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
        cache_key: bubblewrap_capability_cache_key(pane_environment_signature, plan)?,
    })
}

/// Builds the exact cache identity for a deterministic capability probe.
pub(crate) fn bubblewrap_capability_cache_key(
    pane_environment_signature: &str,
    plan: &BubblewrapCapabilityProbePlan,
) -> Result<BubblewrapCapabilityCacheKey, SandboxCompileError> {
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
    Ok(BubblewrapCapabilityCacheKey {
        pane_environment_signature: pane_environment_signature.to_string(),
        executable: plan.executable.clone(),
        runtime_profile_version: BUBBLEWRAP_RUNTIME_PROFILE_VERSION,
        probe_sha256: plan.probe_sha256.clone(),
    })
}

fn validate_request(request: &BubblewrapCompileRequest<'_>) -> Result<(), SandboxCompileError> {
    let expected_probe =
        bubblewrap_capability_probe_plan(request.config, request.child_shell_path)?;
    if request.capability.cache_key.pane_environment_signature != request.pane_environment_signature
        || request.capability.cache_key.executable != request.config.executable
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
            "sandbox compilation requires an allowed permission evaluation",
        ));
    }
    if request.maximum_authority.resolution_status != PathResolutionStatus::ShellResolved {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnresolvedAuthority,
            "sandbox compilation requires pane-shell-resolved path authority",
        ));
    }
    validate_canonical_path(
        &request.maximum_authority.current_directory,
        "sandbox working directory",
    )?;
    validate_printable_absolute_path(&request.config.executable, "Bubblewrap executable")?;
    validate_canonical_path(request.command_file_host_path, "sandbox command file")?;
    validate_canonical_path(request.child_shell_path, "sandbox child shell")?;
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
    if effects.network {
        return match request.network_policy {
            NetworkPolicy::Deny => Err(SandboxCompileError::new(
                SandboxCompileErrorKind::NetworkDenied,
                "network access is denied by the effective permission policy",
            )),
            NetworkPolicy::Prompt | NetworkPolicy::Allow => Err(SandboxCompileError::new(
                SandboxCompileErrorKind::MediatedNetworkUnavailable,
                "network-requiring commands need mediated egress, which is unavailable",
            )),
        };
    }
    match request.config.unavailable {
        SandboxUnavailablePolicy::Fail => {}
    }
    match request.config.network {
        BubblewrapNetworkMode::Isolated => {}
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
    let (mounts, authority_source) =
        if evaluation.completeness == EffectCompleteness::Complete && !evaluation.effects.unknown {
            (
                narrowed_mounts(request.maximum_authority, evaluation)?,
                SandboxAuthoritySource::Narrowed,
            )
        } else {
            (
                maximum_mounts(request.maximum_authority),
                SandboxAuthoritySource::Maximum,
            )
        };
    Ok(EffectiveSandboxPolicy {
        working_directory: request.maximum_authority.current_directory.clone(),
        mounts,
        authority_source,
        network: request.config.network,
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
        if path_overlaps(path, "/run/user") || path_overlaps(path, "/var/run") {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "Bubblewrap authority must not expose host user-runtime or IPC paths",
            ));
        }
        if path_is_credential_directory(path) {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "Bubblewrap authority must not project credential directories",
            ));
        }
    }
    for path in &authority.write_scopes {
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

fn maximum_mounts(authority: &PathScopes) -> Vec<SandboxMount> {
    let mut mounts = authority
        .read_scopes
        .iter()
        .map(|path| SandboxMount {
            source: path.clone(),
            destination: path.clone(),
            access: SandboxMountAccess::ReadOnly,
        })
        .collect::<Vec<_>>();
    mounts.extend(authority.write_scopes.iter().map(|path| SandboxMount {
        source: path.clone(),
        destination: path.clone(),
        access: SandboxMountAccess::ReadWrite,
    }));
    normalize_mounts(mounts)
}

fn narrowed_mounts(
    authority: &PathScopes,
    evaluation: &PermissionEvaluation,
) -> Result<Vec<SandboxMount>, SandboxCompileError> {
    let effects = &evaluation.effects;
    let mut mounts = Vec::new();
    for path in &effects.reads {
        let resolved = resolve_effect_path(path, authority, false)?;
        mounts.push(SandboxMount {
            source: resolved.mount_source,
            destination: resolved.mount_destination,
            access: SandboxMountAccess::ReadOnly,
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
        mounts.push(SandboxMount {
            source: resolved.mount_source,
            destination: resolved.mount_destination,
            access: SandboxMountAccess::ReadWrite,
        });
    }
    Ok(normalize_mounts(mounts))
}

struct ResolvedEffectPath {
    mount_source: String,
    mount_destination: String,
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
    let (canonical_target, mount_source) = match evidence {
        Some(evidence) => resolved_effect_mount(evidence, write)?,
        None if exact_authority_path => (normalized.clone(), normalized),
        None => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::UnresolvedEffectPath,
                "complete sandbox effects require pane-shell canonical path evidence",
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
            .any(|scope| Path::new(&mount_source).starts_with(scope))
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::EffectOutsideAuthority,
            "complete sandbox effects request a path outside maximum authority",
        ));
    }
    Ok(ResolvedEffectPath {
        mount_source: mount_source.clone(),
        mount_destination: mount_source,
    })
}

fn resolved_effect_mount(
    evidence: &ResolvedPathEvidence,
    write: bool,
) -> Result<(String, String), SandboxCompileError> {
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
    let mount_source = if write && evidence.kind == ResolvedPathKind::CreateTarget {
        evidence.nearest_existing_parent.clone()
    } else {
        evidence.canonical_path.clone()
    };
    Ok((evidence.canonical_path.clone(), mount_source))
}

fn normalize_mounts(mounts: Vec<SandboxMount>) -> Vec<SandboxMount> {
    let mut by_destination = BTreeMap::<String, SandboxMount>::new();
    for mount in mounts {
        by_destination
            .entry(mount.destination.clone())
            .and_modify(|existing| {
                if mount.access > existing.access {
                    *existing = mount.clone();
                }
            })
            .or_insert(mount);
    }
    let mut ordered = by_destination.into_values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        path_depth(&left.destination)
            .cmp(&path_depth(&right.destination))
            .then_with(|| left.destination.cmp(&right.destination))
            .then_with(|| left.access.cmp(&right.access))
    });
    let mut normalized: Vec<SandboxMount> = Vec::new();
    for mount in ordered {
        let covered = normalized.iter().any(|parent| {
            Path::new(&mount.destination).starts_with(&parent.destination)
                && parent.access == mount.access
        });
        if !covered {
            normalized.push(mount);
        }
    }
    normalized
}

fn bubblewrap_arguments(
    request: &BubblewrapCompileRequest<'_>,
    policy: &EffectiveSandboxPolicy,
) -> Vec<String> {
    let mut arguments = vec![
        "--unshare-user",
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
        "--dir",
        "/etc",
        "--ro-bind-try",
        "/etc/ld.so.cache",
        "/etc/ld.so.cache",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/home",
        "--tmpfs",
        SANDBOX_HOME,
        "--dir",
        "/run",
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
    for mount in &policy.mounts {
        arguments.push(
            match mount.access {
                SandboxMountAccess::ReadOnly => "--ro-bind",
                SandboxMountAccess::ReadWrite => "--bind",
            }
            .to_string(),
        );
        arguments.push(mount.source.clone());
        arguments.push(mount.destination.clone());
    }
    arguments.extend(
        [
            "--chdir",
            policy.working_directory.as_str(),
            "--setenv",
            "HOME",
            SANDBOX_HOME,
            "--setenv",
            "PATH",
            MINIMAL_PATH,
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
            "mez",
            "--setenv",
            "LOGNAME",
            "mez",
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

fn path_is_credential_directory(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        let Component::Normal(component) = component else {
            return false;
        };
        matches!(
            component.to_str(),
            Some(".ssh" | ".gnupg" | ".aws" | ".azure" | ".kube" | ".docker")
        )
    })
}

#[cfg(test)]
mod tests;
