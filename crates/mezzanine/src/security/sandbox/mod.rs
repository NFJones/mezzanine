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
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};

use mez_agent::permissions::{
    EffectiveCommandEffects, PathResolutionStatus, PathScopes, PermissionEvaluation,
    ResolvedPathEvidence, ResolvedPathKind, RuleDecision,
};
use sha2::{Digest, Sha256};

use crate::runtime::SandboxToolchainKind;
use crate::runtime::{
    BubblewrapConfig, BubblewrapNetworkMode, NetworkPolicy, SandboxEnvironmentPolicy,
    SandboxUnavailablePolicy,
};

mod identity;
mod managed_home;
mod toolchains;
mod workflow;

pub(crate) use identity::{ResolvedSandboxIdentity, resolve_sandbox_identity};
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
pub(crate) use toolchains::{
    ResolvedToolchainProjection, RustToolchainHomeDiscovery, SANDBOX_BUN_PATH, SANDBOX_CMAKE_PATH,
    SANDBOX_DART_PATH, SANDBOX_DENO_PATH, SANDBOX_DOTNET_PATH, SANDBOX_ERLANG_ELIXIR_PATH,
    SANDBOX_ERLANG_PATH, SANDBOX_GCC_PATH, SANDBOX_GHC_CABAL_PATH, SANDBOX_GHC_PATH,
    SANDBOX_GHC_STACK_PATH, SANDBOX_GO_PATH, SANDBOX_JDK_GRADLE_PATH, SANDBOX_JDK_MAVEN_PATH,
    SANDBOX_JDK_PATH, SANDBOX_KOTLIN_JDK_PATH, SANDBOX_LLVM_PATH, SANDBOX_MESON_PATH,
    SANDBOX_NINJA_PATH, SANDBOX_NODE_PATH, SANDBOX_PHP_COMPOSER_PATH, SANDBOX_PHP_PATH,
    SANDBOX_PYTHON_PATH, SANDBOX_RUBY_PATH, SANDBOX_RUST_PATH, SANDBOX_SWIFT_PATH,
    SANDBOX_ZIG_PATH, SUPPORTED_SANDBOX_TOOLCHAIN_KINDS, ToolchainDescriptor, ToolchainPlatform,
    discover_bun_from_search_path, discover_cabal_from_search_path,
    discover_cmake_from_search_path, discover_composer_from_search_path,
    discover_dart_from_search_path, discover_deno_from_search_path,
    discover_dotnet_from_search_path, discover_elixir_from_search_path,
    discover_erlang_from_search_path, discover_gcc_from_search_path, discover_ghc_from_search_path,
    discover_go_from_search_path, discover_gradle_from_search_path, discover_jdk_from_search_path,
    discover_jvm_project_wrapper, discover_kotlin_from_search_path, discover_llvm_from_search_path,
    discover_maven_from_search_path, discover_meson_from_search_path,
    discover_ninja_from_search_path, discover_node_from_search_path,
    discover_ocaml_project_environment, discover_php_from_search_path,
    discover_python_from_search_path, discover_ruby_from_search_path,
    discover_rust_from_environment_managers, discover_rust_from_home,
    discover_stack_from_search_path, discover_swift_from_search_path,
    discover_zig_from_search_path, parse_sandbox_toolchain_kind,
    resolve_configured_toolchain_projection_for_project, resolve_toolchain_projection,
    resolve_toolchain_projection_for_project, toolchain_descriptor,
};
#[cfg(test)]
pub(crate) use toolchains::{
    SANDBOX_BUN_ROOT, SANDBOX_CABAL_ROOT, SANDBOX_COMPOSER_ROOT, SANDBOX_DART_ROOT,
    SANDBOX_DENO_ROOT, SANDBOX_DOTNET_ROOT, SANDBOX_ELIXIR_ROOT, SANDBOX_ERLANG_ROOT,
    SANDBOX_GHC_CABAL_STACK_PATH, SANDBOX_GHC_ROOT, SANDBOX_GO_ROOT, SANDBOX_GRADLE_ROOT,
    SANDBOX_JDK_ROOT, SANDBOX_KOTLIN_ROOT, SANDBOX_MAVEN_ROOT, SANDBOX_NODE_ROOT, SANDBOX_PHP_ROOT,
    SANDBOX_PYTHON_ROOT, SANDBOX_RUBY_ROOT, SANDBOX_RUST_CARGO_BIN, SANDBOX_RUSTUP_HOME,
    SANDBOX_STACK_ROOT, SANDBOX_SWIFT_ROOT, SANDBOX_ZIG_ROOT, ToolchainAuthorityClass,
};
pub(crate) use workflow::{
    SandboxDiagnosticSeverity, SandboxWorkflowPlan, SandboxWorkflowRequest,
    effective_sandbox_boundary, plan_sandbox_workflow,
};

/// Version of the fixed runtime projection emitted by this compiler.
pub(crate) const BUBBLEWRAP_RUNTIME_PROFILE_VERSION: &str = "bubblewrap-v12";
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
    format!(
        "{}. Run `mez sandbox status --verbose` to inspect the executable, authority, and configuration remedies.",
        message.trim().trim_end_matches('.')
    )
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
    /// Descriptor-composed toolchain projection resolved from pane bootstrap.
    pub(crate) toolchain_projection: Option<&'a ResolvedToolchainProjection>,
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
    /// Effective network namespace mode.
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
    /// Effective network namespace mode.
    pub(crate) network: BubblewrapNetworkMode,
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
    /// A selected toolchain root falls outside maximum read authority.
    ToolchainOutsideAuthority,
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
            Self::ToolchainOutsideAuthority => "toolchain_outside_authority",
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
    let expected_stdout = "mez-bubblewrap-capability-v6";
    let probe_script = format!(
        "test ! -e /etc/passwd && test \"$(id -u)\" = '{user_id}' && test \"$(id -g)\" = '{group_id}' && test -r /proc/self/status && test -c /dev/null && test -w /tmp && test -w \"$HOME\" && test -z \"${{SSH_AUTH_SOCK+x}}\" && printf '%s' '{expected_stdout}'"
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
        b"mez-bubblewrap-capability-probe-v3\0",
        &config.executable,
        &bubblewrap_arguments,
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
    if request.permission_evaluation.decision == RuleDecision::Forbid {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::Unauthorized,
            "sandbox compilation rejects forbidden permission evaluations",
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
    if let Some(toolchains) = request.toolchain_projection {
        toolchains.validate()?;
        let effective_read_authority =
            toolchains.extend_read_authority(request.maximum_authority)?;
        toolchains.validate_authority(&effective_read_authority)?;
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
        BubblewrapNetworkMode::Isolated | BubblewrapNetworkMode::Connected => {}
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
    let (mounts, authority_source) = if request.preserve_maximum_authority {
        (
            maximum_mounts(request.maximum_authority),
            SandboxAuthoritySource::Maximum,
        )
    } else if let Some(effects) = evaluation.confinement_effects.as_ref() {
        (
            narrowed_mounts(request.maximum_authority, effects)?,
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
        network: if matches!(request.network_policy, NetworkPolicy::Allow)
            || (evaluation.effects.network
                && matches!(request.network_policy, NetworkPolicy::Prompt))
        {
            BubblewrapNetworkMode::Connected
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
            validate_ipc_read_scope(path)?;
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

/// Allows a protected IPC read scope only when it identifies one existing Unix
/// socket node. Directories, regular files, missing paths, and symlinks could
/// expose broader IPC authority and therefore remain forbidden.
fn validate_ipc_read_scope(path: &str) -> Result<(), SandboxCompileError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("Bubblewrap IPC read authority must name an existing Unix socket: {error}"),
        )
    })?;
    #[cfg(unix)]
    let is_socket = !metadata.file_type().is_symlink() && metadata.file_type().is_socket();
    #[cfg(not(unix))]
    let is_socket = false;
    if !is_socket {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "Bubblewrap IPC read authority must name an existing Unix socket",
        ));
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
    effects: &EffectiveCommandEffects,
) -> Result<Vec<SandboxMount>, SandboxCompileError> {
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
    policy.mounts = normalize_mounts(
        policy
            .mounts
            .into_iter()
            .map(|mut mount| {
                mount.destination =
                    synthetic_home_path(&mount.destination, pane_home_directory, sandbox_home);
                mount
            })
            .collect(),
    );
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
    if policy.network == BubblewrapNetworkMode::Isolated {
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
    if let Some(toolchains) = request.toolchain_projection {
        for directory in &toolchains.sandbox_directories {
            arguments.push("--dir".to_string());
            arguments.push((*directory).to_string());
        }
        for root in &toolchains.roots {
            arguments.push("--ro-bind".to_string());
            arguments.push(root.host_path.to_string_lossy().into_owned());
            arguments.push(root.sandbox_destination.to_string());
        }
    }
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
    let executable_path = request
        .toolchain_projection
        .map(ResolvedToolchainProjection::executable_path)
        .unwrap_or_else(|| MINIMAL_PATH.to_string());
    if let Some(toolchains) = request.toolchain_projection {
        for (name, value) in &toolchains.environment {
            arguments.extend(
                [
                    "--setenv",
                    name.as_str(),
                    rehome_managed_path(value, &sandbox_home).as_str(),
                ]
                .into_iter()
                .map(str::to_string),
            );
        }
        for environment in &toolchains.project_environments {
            let variable = match environment.kind {
                SandboxToolchainKind::Python => "VIRTUAL_ENV",
                SandboxToolchainKind::Ocaml => "OPAM_SWITCH_PREFIX",
                _ => continue,
            };
            arguments.extend(
                ["--setenv", variable, environment.sandbox_path.as_str()]
                    .into_iter()
                    .map(str::to_string),
            );
        }
    }
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
            executable_path.as_str(),
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
