//! Deterministic macOS Seatbelt profile compilation.
//!
//! This module translates one already authorized, backend-neutral effective
//! sandbox policy into code-owned SBPL and a typed `sandbox-exec` launch. It
//! performs no filesystem discovery, accepts no user profile fragments, and
//! keeps generated profile bytes out of audit metadata. Seatbelt confines
//! operations in the host namespace; it does not claim Bubblewrap namespace
//! equivalence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use mez_agent::{ShellChildArgument, ShellChildLaunch, ShellLaunchArtifact, ShellLaunchArtifactId};
use sha2::{Digest, Sha256};

use super::seatbelt_child::INTERNAL_LAUNCH_ARGUMENT;
use super::{
    EffectiveSandboxPolicy, SandboxAuditSummary, SandboxCompileError, SandboxCompileErrorKind,
    SandboxPathAccess, SandboxPathGrant, SandboxPathKind,
};
use crate::runtime::{
    SandboxBackend, SandboxEnvironmentPolicy, SandboxNetworkMode, SandboxUnavailablePolicy,
    SeatbeltConfig,
};

/// Version of the code-owned Seatbelt profile emitted by this compiler.
pub(crate) const SEATBELT_RUNTIME_PROFILE_VERSION: &str = "seatbelt-v1";

const PROFILE_ARTIFACT_ID: &str = "seatbelt-profile";
const MINIMAL_PATH: &str = "/usr/bin:/bin";

const FIXED_READ_SUBPATHS: &[&str] = &[
    "/System",
    "/usr",
    "/bin",
    "/sbin",
    "/private/etc",
    "/private/var/select",
    "/private/var/db/timezone",
];
const FIXED_READ_LITERALS: &[&str] = &["/dev/null", "/dev/random", "/dev/urandom"];
const PROTECTED_ENVIRONMENT_NAMES: &[&str] = &[
    "HOME",
    "TMPDIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_COUNT",
    "USER",
    "LOGNAME",
    "SHELL",
];

/// Inputs required to compile one effective policy into a Seatbelt launch.
#[derive(Debug, Clone)]
pub(crate) struct SeatbeltCompileRequest<'a> {
    /// Typed fail-closed Seatbelt configuration.
    pub(crate) config: &'a SeatbeltConfig,
    /// Backend-neutral policy produced from trusted authority evidence.
    pub(crate) policy: &'a EffectiveSandboxPolicy,
    /// Selected non-interactive child shell.
    pub(crate) child_shell_path: &'a str,
    /// Canonical Mezzanine executable owning the hidden child-launch mode.
    pub(crate) child_launcher_path: &'a str,
    /// Canonical command file already materialized by the transport.
    pub(crate) command_file_path: &'a str,
    /// Canonical owner-only environment document read by the child launcher.
    pub(crate) environment_file_path: &'a str,
    /// Canonical private home supplied to the payload.
    pub(crate) home_directory: &'a str,
    /// Canonical private temporary directory supplied to the payload.
    pub(crate) temporary_directory: &'a str,
    /// Native user name projected without host account configuration.
    pub(crate) user_name: &'a str,
    /// Protected environment evidence selected by configuration.
    pub(crate) environment_evidence: &'a mez_agent::shell::PaneEnvironmentEvidence,
    /// Whether the command must mutate persistent shell state.
    pub(crate) stateful: bool,
    /// Whether the command requires direct terminal interaction.
    pub(crate) interactive: bool,
}

/// Fully typed Seatbelt launch plus redacted compiler evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeatbeltLaunchPlan {
    /// Typed `sandbox-exec` launch carrying the owner-only profile artifact.
    pub(crate) child_launch: ShellChildLaunch,
    /// Canonical working directory retained for transport diagnostics.
    pub(crate) working_directory: String,
    /// Digest of the generated profile bytes for capability identity.
    pub(crate) profile_sha256: String,
    /// Bounded canonical environment document written before launch.
    pub(crate) environment_document: Vec<u8>,
    /// Redacted plan facts safe for status and audit records.
    pub(crate) audit_summary: SandboxAuditSummary,
}

/// Compiles one effective policy into deterministic deny-default Seatbelt SBPL.
pub(crate) fn compile_seatbelt_launch_plan(
    request: SeatbeltCompileRequest<'_>,
) -> Result<SeatbeltLaunchPlan, SandboxCompileError> {
    validate_request(&request)?;
    let profile = seatbelt_profile(&request)?;
    let profile_sha256 = sha256_hex(profile.as_bytes());
    let artifact_id = ShellLaunchArtifactId::new(PROFILE_ARTIFACT_ID)
        .map_err(|error| invalid_input(error.message()))?;
    let artifact = ShellLaunchArtifact::new(artifact_id.clone(), profile.into_bytes(), 0o400)
        .map_err(|error| invalid_input(error.message()))?;
    let environment = payload_environment(&request)?;
    let environment_document = serde_json::to_vec(&environment)
        .map_err(|error| invalid_input(format!("Seatbelt environment encoding failed: {error}")))?;
    let arguments = vec![
        ShellChildArgument::Literal(INTERNAL_LAUNCH_ARGUMENT.to_string()),
        ShellChildArgument::Literal(request.config.executable.clone()),
        ShellChildArgument::MaterializedArtifact(artifact_id),
        ShellChildArgument::Literal(request.policy.working_directory.clone()),
        ShellChildArgument::Literal(request.home_directory.to_string()),
        ShellChildArgument::Literal(request.temporary_directory.to_string()),
        ShellChildArgument::Literal(request.child_shell_path.to_string()),
        ShellChildArgument::Literal(request.command_file_path.to_string()),
        ShellChildArgument::Literal(request.environment_file_path.to_string()),
    ];
    let child_launch = ShellChildLaunch::new_with_artifacts(
        request.child_launcher_path.to_string(),
        arguments,
        vec![artifact],
    )
    .map_err(|error| invalid_input(error.message()))?;
    let plan_sha256 = seatbelt_plan_sha256(&request, &child_launch, &profile_sha256);

    Ok(SeatbeltLaunchPlan {
        child_launch,
        working_directory: canonicalize_macos_alias(&request.policy.working_directory),
        profile_sha256,
        environment_document,
        audit_summary: SandboxAuditSummary {
            backend: SandboxBackend::Seatbelt,
            runtime_profile_version: SEATBELT_RUNTIME_PROFILE_VERSION,
            authority_source: request.policy.authority_source,
            read_only_grant_count: request
                .policy
                .grants
                .iter()
                .filter(|grant| grant.access == SandboxPathAccess::ReadOnly)
                .count(),
            read_write_grant_count: request
                .policy
                .grants
                .iter()
                .filter(|grant| grant.access == SandboxPathAccess::ReadWrite)
                .count(),
            network: request.policy.network,
            plan_sha256,
        },
    })
}

fn validate_request(request: &SeatbeltCompileRequest<'_>) -> Result<(), SandboxCompileError> {
    if request.config.executable != "/usr/bin/sandbox-exec" {
        return Err(invalid_input(
            "Seatbelt executable must be /usr/bin/sandbox-exec",
        ));
    }
    validate_printable_absolute_path(request.child_shell_path, "Seatbelt child shell")?;
    validate_printable_absolute_path(request.child_launcher_path, "Seatbelt child launcher")?;
    validate_printable_absolute_path(request.command_file_path, "Seatbelt command file")?;
    validate_printable_absolute_path(
        request.environment_file_path,
        "Seatbelt environment document",
    )?;
    validate_printable_absolute_path(request.home_directory, "Seatbelt private home")?;
    validate_printable_absolute_path(request.temporary_directory, "Seatbelt temporary directory")?;
    validate_printable_absolute_path(
        &request.policy.working_directory,
        "Seatbelt working directory",
    )?;
    if request.user_name.trim().is_empty() || request.user_name.chars().any(char::is_control) {
        return Err(invalid_input("Seatbelt user name must be printable"));
    }
    if request.stateful {
        return Err(unsupported(
            "stateful shell actions are unsupported by per-command Seatbelt confinement",
        ));
    }
    if request.interactive {
        return Err(unsupported(
            "interactive shell actions are unsupported by the initial Seatbelt profile",
        ));
    }
    match request.config.unavailable {
        SandboxUnavailablePolicy::Fail => {}
    }
    match request.config.environment {
        SandboxEnvironmentPolicy::Minimal => {}
    }
    match request.policy.environment {
        SandboxEnvironmentPolicy::Minimal => {}
    }
    match request.policy.network {
        SandboxNetworkMode::Isolated | SandboxNetworkMode::Connected => {}
    }
    for grant in &request.policy.grants {
        validate_grant(grant)?;
    }
    for protected in PROTECTED_ENVIRONMENT_NAMES {
        if request.environment_evidence.values.contains_key(*protected) {
            return Err(invalid_input(format!(
                "Seatbelt environment evidence must not override protected variable {protected}"
            )));
        }
    }
    Ok(())
}

fn validate_grant(grant: &SandboxPathGrant) -> Result<(), SandboxCompileError> {
    validate_printable_absolute_path(&grant.canonical_path, "Seatbelt canonical grant")?;
    validate_printable_absolute_path(&grant.enforcement_path, "Seatbelt enforcement grant")?;
    let enforcement = canonicalize_macos_alias(&grant.enforcement_path);
    if matches!(enforcement.as_str(), "/" | "/Users") {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "Seatbelt authority must not expose host root or the multi-user home root",
        ));
    }
    if grant.access == SandboxPathAccess::ReadWrite
        && FIXED_READ_SUBPATHS
            .iter()
            .chain(FIXED_READ_LITERALS)
            .any(|protected| paths_overlap(&enforcement, protected))
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "Seatbelt write authority overlaps the fixed runtime projection",
        ));
    }
    match grant.kind {
        SandboxPathKind::File | SandboxPathKind::Directory => Ok(()),
        SandboxPathKind::UnixSocket => Err(unsupported(
            "exact Unix-domain socket authority is unsupported by the initial Seatbelt profile",
        )),
        SandboxPathKind::Other | SandboxPathKind::Unknown => Err(unsupported(
            "Seatbelt grants require trusted regular-file or directory evidence",
        )),
    }
}

fn seatbelt_profile(request: &SeatbeltCompileRequest<'_>) -> Result<String, SandboxCompileError> {
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(allow process-exec)\n(allow process-fork)\n(allow signal (target same-sandbox))\n(allow sysctl-read)\n(allow file-read-data (literal \"/\"))\n(allow file-write* (literal \"/dev/null\"))\n",
    );
    append_filter_rule(&mut profile, "file-read*", "subpath", FIXED_READ_SUBPATHS)?;
    append_filter_rule(&mut profile, "file-read*", "literal", FIXED_READ_LITERALS)?;

    let working_directory_ancestors = path_metadata_ancestors(&request.policy.working_directory);
    let working_directory_ancestor_refs = working_directory_ancestors
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    append_filter_rule(
        &mut profile,
        "file-read*",
        "literal",
        &working_directory_ancestor_refs,
    )?;

    let fixed_literals = [
        request.child_launcher_path,
        request.child_shell_path,
        request.command_file_path,
        request.environment_file_path,
    ];
    append_filter_rule(&mut profile, "file-read*", "literal", &fixed_literals)?;
    let private_directories = [request.home_directory, request.temporary_directory];
    append_filter_rule(
        &mut profile,
        "file-read* file-write*",
        "subpath",
        &private_directories,
    )?;

    for grant in &request.policy.grants {
        let path = canonicalize_macos_alias(&grant.enforcement_path);
        let filter = match grant.kind {
            SandboxPathKind::File => "literal",
            SandboxPathKind::Directory => "subpath",
            SandboxPathKind::UnixSocket | SandboxPathKind::Other | SandboxPathKind::Unknown => {
                return Err(unsupported(
                    "Seatbelt profile compilation received an unsupported path kind",
                ));
            }
        };
        let operation = match grant.access {
            SandboxPathAccess::ReadOnly => "file-read*",
            SandboxPathAccess::ReadWrite => "file-read* file-write*",
        };
        append_filter_rule(&mut profile, operation, filter, &[path.as_str()])?;
    }
    if request.policy.network == SandboxNetworkMode::Connected {
        profile.push_str("(allow network*)\n");
    }
    Ok(profile)
}

fn append_filter_rule(
    profile: &mut String,
    operations: &str,
    filter: &str,
    paths: &[&str],
) -> Result<(), SandboxCompileError> {
    if paths.is_empty() {
        return Ok(());
    }
    profile.push_str("(allow ");
    profile.push_str(operations);
    for path in paths {
        profile.push_str(" (");
        profile.push_str(filter);
        profile.push(' ');
        profile.push_str(&sbpl_string(&canonicalize_macos_alias(path))?);
        profile.push(')');
    }
    profile.push_str(")\n");
    Ok(())
}

fn path_metadata_ancestors(path: &str) -> Vec<String> {
    let canonical = canonicalize_macos_alias(path);
    Path::new(&canonical)
        .ancestors()
        .map(|ancestor| ancestor.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn payload_environment(
    request: &SeatbeltCompileRequest<'_>,
) -> Result<BTreeMap<String, String>, SandboxCompileError> {
    let home = canonicalize_macos_alias(request.home_directory);
    let temporary = canonicalize_macos_alias(request.temporary_directory);
    let mut environment = BTreeMap::from([
        ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ("HOME".to_string(), home.clone()),
        ("LANG".to_string(), "C".to_string()),
        ("LC_ALL".to_string(), "C".to_string()),
        ("LOGNAME".to_string(), request.user_name.to_string()),
        ("PATH".to_string(), MINIMAL_PATH.to_string()),
        ("SHELL".to_string(), request.child_shell_path.to_string()),
        ("TMPDIR".to_string(), temporary),
        ("USER".to_string(), request.user_name.to_string()),
        ("XDG_CACHE_HOME".to_string(), format!("{home}/.cache")),
        ("XDG_CONFIG_HOME".to_string(), format!("{home}/.config")),
        ("XDG_DATA_HOME".to_string(), format!("{home}/.local/share")),
        ("XDG_STATE_HOME".to_string(), format!("{home}/.local/state")),
    ]);
    for (name, value) in &request.environment_evidence.values {
        if PROTECTED_ENVIRONMENT_NAMES.contains(&name.as_str()) {
            return Err(invalid_input(format!(
                "Seatbelt environment evidence must not override protected variable {name}"
            )));
        }
        environment.insert(name.clone(), value.clone());
    }
    if let (Some(name), Some(email)) = (
        request.config.git_user_name.as_deref(),
        request.config.git_user_email.as_deref(),
    ) {
        environment.insert("GIT_CONFIG_COUNT".to_string(), "2".to_string());
        environment.insert("GIT_CONFIG_KEY_0".to_string(), "user.name".to_string());
        environment.insert("GIT_CONFIG_VALUE_0".to_string(), name.to_string());
        environment.insert("GIT_CONFIG_KEY_1".to_string(), "user.email".to_string());
        environment.insert("GIT_CONFIG_VALUE_1".to_string(), email.to_string());
    }
    Ok(environment)
}

fn seatbelt_plan_sha256(
    request: &SeatbeltCompileRequest<'_>,
    launch: &ShellChildLaunch,
    profile_sha256: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mez-seatbelt-launch-plan-v1\0");
    digest.update(SEATBELT_RUNTIME_PROFILE_VERSION.as_bytes());
    digest.update(b"\0");
    digest.update(request.config.executable.as_bytes());
    digest.update(b"\0");
    digest.update(profile_sha256.as_bytes());
    digest.update(b"\0");
    digest.update(canonicalize_macos_alias(&request.policy.working_directory).as_bytes());
    for argument in &launch.arguments {
        digest.update(b"\0");
        match argument {
            ShellChildArgument::Literal(value) => digest.update(value.as_bytes()),
            ShellChildArgument::MaterializedCommandFile => digest.update(b"materialized-command"),
            ShellChildArgument::MaterializedArtifact(artifact) => {
                digest.update(b"materialized-artifact:");
                digest.update(artifact.as_str().as_bytes());
            }
            ShellChildArgument::MaterializedPathBinding { name, artifact } => {
                digest.update(b"materialized-binding:");
                digest.update(name.as_bytes());
                digest.update(b"=");
                digest.update(artifact.as_str().as_bytes());
            }
        }
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sbpl_string(value: &str) -> Result<String, SandboxCompileError> {
    if value.contains('\0') || value.chars().any(char::is_control) {
        return Err(invalid_input(
            "Seatbelt SBPL paths must be printable and contain no NUL bytes",
        ));
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("\"{escaped}\""))
}

fn canonicalize_macos_alias(path: &str) -> String {
    for (alias, canonical) in [
        ("/tmp", "/private/tmp"),
        ("/var", "/private/var"),
        ("/etc", "/private/etc"),
    ] {
        if path == alias {
            return canonical.to_string();
        }
        if let Some(suffix) = path
            .strip_prefix(alias)
            .filter(|suffix| suffix.starts_with('/'))
        {
            return format!("{canonical}{suffix}");
        }
    }
    path.to_string()
}

fn validate_printable_absolute_path(path: &str, label: &str) -> Result<(), SandboxCompileError> {
    let parsed = Path::new(path);
    if path.is_empty()
        || path.contains('\0')
        || path.chars().any(char::is_control)
        || !parsed.is_absolute()
        || parsed.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid_input(format!(
            "{label} must be a printable canonical absolute path"
        )));
    }
    Ok(())
}

fn paths_overlap(left: &str, right: &str) -> bool {
    Path::new(left).starts_with(right) || Path::new(right).starts_with(left)
}

fn invalid_input(message: impl Into<String>) -> SandboxCompileError {
    SandboxCompileError::new(SandboxCompileErrorKind::InvalidInput, message)
}

fn unsupported(message: impl Into<String>) -> SandboxCompileError {
    SandboxCompileError::new(SandboxCompileErrorKind::UnsupportedRequirement, message)
}

#[cfg(test)]
mod tests {
    use super::super::{
        SandboxAuthoritySource, SandboxPathAccess, SandboxPathGrant, SandboxPathKind,
    };
    use super::*;
    use crate::runtime::SandboxConfig;

    fn config() -> SeatbeltConfig {
        let SandboxConfig::Seatbelt(config) =
            crate::runtime::runtime_configured_permissions_from_config(
                &serde_json::json!({"permissions": {"sandbox": "seatbelt"}}),
            )
            .unwrap()
            .sandbox
        else {
            panic!("expected Seatbelt config");
        };
        config
    }

    fn policy(network: SandboxNetworkMode) -> EffectiveSandboxPolicy {
        EffectiveSandboxPolicy {
            working_directory: "/private/tmp/workspace".to_string(),
            grants: vec![
                SandboxPathGrant {
                    canonical_path: "/private/tmp/workspace/input.txt".to_string(),
                    enforcement_path: "/private/tmp/workspace/input.txt".to_string(),
                    kind: SandboxPathKind::File,
                    access: SandboxPathAccess::ReadOnly,
                },
                SandboxPathGrant {
                    canonical_path: "/private/tmp/workspace/target/new.txt".to_string(),
                    enforcement_path: "/private/tmp/workspace/target".to_string(),
                    kind: SandboxPathKind::Directory,
                    access: SandboxPathAccess::ReadWrite,
                },
            ],
            authority_source: SandboxAuthoritySource::Narrowed,
            network,
            environment: SandboxEnvironmentPolicy::Minimal,
        }
    }

    fn evidence() -> mez_agent::shell::PaneEnvironmentEvidence {
        let request =
            mez_agent::shell::PaneEnvironmentRequest::new(vec!["PATH".to_string()]).unwrap();
        let mut evidence =
            mez_agent::shell::PaneEnvironmentEvidence::restrictive(&request, "test_default");
        evidence.values.insert(
            "PATH".to_string(),
            "/opt/tools/bin:/usr/bin:/bin".to_string(),
        );
        evidence
    }

    fn request<'a>(
        config: &'a SeatbeltConfig,
        policy: &'a EffectiveSandboxPolicy,
        evidence: &'a mez_agent::shell::PaneEnvironmentEvidence,
    ) -> SeatbeltCompileRequest<'a> {
        SeatbeltCompileRequest {
            config,
            policy,
            child_shell_path: "/bin/sh",
            child_launcher_path: "/usr/bin/true",
            command_file_path: "/private/tmp/mez-action/command",
            environment_file_path: "/private/tmp/mez-action/environment.json",
            home_directory: "/private/tmp/mez-action/home",
            temporary_directory: "/private/tmp/mez-action/tmp",
            user_name: "mez",
            environment_evidence: evidence,
            stateful: false,
            interactive: false,
        }
    }

    fn profile(plan: &SeatbeltLaunchPlan) -> String {
        String::from_utf8(plan.child_launch.artifacts[0].content.clone()).unwrap()
    }

    #[test]
    fn compiler_emits_deterministic_deny_default_profile_and_typed_artifact() {
        let config = config();
        let policy = policy(SandboxNetworkMode::Isolated);
        let evidence = evidence();

        let first = compile_seatbelt_launch_plan(request(&config, &policy, &evidence)).unwrap();
        let second = compile_seatbelt_launch_plan(request(&config, &policy, &evidence)).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.child_launch.executable, "/usr/bin/true");
        assert_eq!(first.child_launch.artifacts[0].mode, 0o400);
        assert_eq!(first.audit_summary.backend, SandboxBackend::Seatbelt);
        assert_eq!(
            first.audit_summary.runtime_profile_version,
            SEATBELT_RUNTIME_PROFILE_VERSION
        );
        assert_eq!(first.audit_summary.read_only_grant_count, 1);
        assert_eq!(first.audit_summary.read_write_grant_count, 1);
        assert_eq!(first.audit_summary.plan_sha256.len(), 64);
        let profile = profile(&first);
        assert!(profile.starts_with("(version 1)\n(deny default)\n"));
        assert!(profile.contains("(literal \"/private/tmp/workspace/input.txt\")"));
        assert!(profile.contains("(subpath \"/private/tmp/workspace/target\")"));
        assert!(!profile.contains("(allow network*)"));
    }

    #[test]
    fn connected_profile_is_explicit_and_environment_is_minimal() {
        let mut config = config();
        config.git_user_name = Some("Sandbox Author".to_string());
        config.git_user_email = Some("sandbox@example.invalid".to_string());
        let policy = policy(SandboxNetworkMode::Connected);
        let evidence = evidence();

        let plan = compile_seatbelt_launch_plan(request(&config, &policy, &evidence)).unwrap();
        let environment =
            serde_json::from_slice::<BTreeMap<String, String>>(&plan.environment_document).unwrap();

        assert!(profile(&plan).contains("(allow network*)"));
        assert_eq!(environment["HOME"], "/private/tmp/mez-action/home");
        assert_eq!(environment["TMPDIR"], "/private/tmp/mez-action/tmp");
        assert_eq!(environment["PATH"], "/opt/tools/bin:/usr/bin:/bin");
        assert_eq!(environment["GIT_CONFIG_COUNT"], "2");
        assert!(!environment.contains_key("SSH_AUTH_SOCK"));
    }

    #[test]
    fn compiler_escapes_paths_and_normalizes_top_level_macos_aliases() {
        let config = config();
        let mut policy = policy(SandboxNetworkMode::Isolated);
        policy.grants = vec![SandboxPathGrant {
            canonical_path: "/tmp/quoted\"back\\slash".to_string(),
            enforcement_path: "/tmp/quoted\"back\\slash".to_string(),
            kind: SandboxPathKind::File,
            access: SandboxPathAccess::ReadOnly,
        }];
        let evidence = evidence();

        let plan = compile_seatbelt_launch_plan(request(&config, &policy, &evidence)).unwrap();

        assert!(profile(&plan).contains("(literal \"/private/tmp/quoted\\\"back\\\\slash\")"));
    }

    /// Verifies the compiler rejects a noncanonical launcher before it can
    /// materialize a workload profile or child-launch plan.
    #[test]
    fn compiler_rejects_noncanonical_seatbelt_executable() {
        let mut config = config();
        config.executable = "/tmp/sandbox-exec".to_string();
        let policy = policy(SandboxNetworkMode::Isolated);
        let error =
            compile_seatbelt_launch_plan(request(&config, &policy, &evidence())).unwrap_err();

        assert_eq!(error.kind(), SandboxCompileErrorKind::InvalidInput);
        assert_eq!(
            error.message(),
            "Seatbelt executable must be /usr/bin/sandbox-exec"
        );
    }

    #[test]
    fn compiler_fails_closed_for_unsupported_path_kinds_and_protected_environment() {
        let config = config();
        let environment_evidence = evidence();
        for kind in [
            SandboxPathKind::UnixSocket,
            SandboxPathKind::Other,
            SandboxPathKind::Unknown,
        ] {
            let mut policy = policy(SandboxNetworkMode::Isolated);
            policy.grants[0].kind = kind;
            assert_eq!(
                compile_seatbelt_launch_plan(request(&config, &policy, &environment_evidence))
                    .unwrap_err()
                    .kind(),
                SandboxCompileErrorKind::UnsupportedRequirement
            );
        }

        let policy = policy(SandboxNetworkMode::Isolated);
        let mut protected = evidence();
        protected
            .values
            .insert("HOME".to_string(), "/Users/ambient".to_string());
        assert_eq!(
            compile_seatbelt_launch_plan(request(&config, &policy, &protected))
                .unwrap_err()
                .kind(),
            SandboxCompileErrorKind::InvalidInput
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn real_profile_allows_scoped_descendant_writes_and_denies_siblings() {
        use std::fs;
        use std::process::Command;

        if !Path::new("/usr/bin/sandbox-exec").is_file() {
            return;
        }
        let root = std::env::temp_dir().join(format!("mez-seatbelt-real-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("allowed")).unwrap();
        fs::create_dir_all(root.join("denied")).unwrap();
        fs::create_dir_all(root.join("read-only")).unwrap();
        fs::write(root.join("read-only/input"), b"input").unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("tmp")).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let command_file = root.join("command");
        fs::write(
            &command_file,
            "touch \"$1/direct\"\n/bin/sh -c 'touch \"$1/child\"' sh \"$1\"\nif touch \"$2/denied\" 2>/dev/null; then exit 90; fi\ncat \"$3/input\" >/dev/null\nif touch \"$3/denied\" 2>/dev/null; then exit 91; fi\nif [ -n \"${MEZ_REAL_SEATBELT_SECRET:-}\" ]; then exit 92; fi\n",
        )
        .unwrap();
        let config = config();
        let policy = EffectiveSandboxPolicy {
            working_directory: root.to_string_lossy().into_owned(),
            grants: vec![
                SandboxPathGrant {
                    canonical_path: root.join("allowed").to_string_lossy().into_owned(),
                    enforcement_path: root.join("allowed").to_string_lossy().into_owned(),
                    kind: SandboxPathKind::Directory,
                    access: SandboxPathAccess::ReadWrite,
                },
                SandboxPathGrant {
                    canonical_path: root.join("read-only").to_string_lossy().into_owned(),
                    enforcement_path: root.join("read-only").to_string_lossy().into_owned(),
                    kind: SandboxPathKind::Directory,
                    access: SandboxPathAccess::ReadOnly,
                },
            ],
            authority_source: SandboxAuthoritySource::Narrowed,
            network: SandboxNetworkMode::Isolated,
            environment: SandboxEnvironmentPolicy::Minimal,
        };
        let evidence = evidence();
        let command_file_path = command_file.to_string_lossy().into_owned();
        let child_launcher_path = "/bin/sh".to_string();
        let environment_file_path = root.join("environment.json").to_string_lossy().into_owned();
        let home_directory = root.join("home").to_string_lossy().into_owned();
        let temporary_directory = root.join("tmp").to_string_lossy().into_owned();
        let compile_request = SeatbeltCompileRequest {
            config: &config,
            policy: &policy,
            child_shell_path: "/bin/sh",
            child_launcher_path: &child_launcher_path,
            command_file_path: &command_file_path,
            environment_file_path: &environment_file_path,
            home_directory: &home_directory,
            temporary_directory: &temporary_directory,
            user_name: "mez",
            environment_evidence: &evidence,
            stateful: false,
            interactive: false,
        };
        let plan = compile_seatbelt_launch_plan(compile_request).unwrap();
        let profile_path = root.join("profile.sb");
        fs::write(&profile_path, &plan.child_launch.artifacts[0].content).unwrap();
        let mut command = Command::new(&config.executable);
        command
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .arg(&command_file)
            .current_dir(&root)
            .env_remove("MEZ_REAL_SEATBELT_SECRET")
            .arg(root.join("allowed"))
            .arg(root.join("denied"))
            .arg(root.join("read-only"));
        let output = command.output().unwrap();

        assert!(output.status.success(), "{output:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("/dev/null"),
            "sibling denial must not be short-circuited by a null-device denial: {output:?}"
        );
        assert!(root.join("allowed/direct").is_file());
        assert!(root.join("allowed/child").is_file());
        assert!(!root.join("denied/denied").exists());
        assert!(!root.join("read-only/denied").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
