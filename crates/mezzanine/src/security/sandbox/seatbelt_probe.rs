//! Code-owned macOS Seatbelt capability probe.
//!
//! The probe runs before any configured Seatbelt workload and proves the
//! fixed deny-default profile in a real descendant process. A hidden
//! Mezzanine orchestrator creates canonical owner-only state, materializes a
//! generated SBPL profile, starts this same executable through
//! `sandbox-exec`, and accepts one exact sentinel only after a Rust payload has
//! proven authorized file access plus sibling, descendant, TCP, UDP, Unix
//! socket, and ambient-environment denial. The private state is removed on
//! every orchestrator return path. No raw profile, launcher argument, or
//! arbitrary executable is accepted from user configuration.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{TcpListener, UdpSocket};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use mez_agent::EnvironmentSignature;
use sha2::{Digest, Sha256};

use super::seatbelt::SEATBELT_RUNTIME_PROFILE_VERSION;
use super::{SandboxCompileError, SandboxCompileErrorKind};
use crate::runtime::{SandboxBackend, SeatbeltConfig};

/// Exact stdout accepted from a complete Seatbelt capability probe.
pub(crate) const SEATBELT_CAPABILITY_SENTINEL: &str = "mez-seatbelt-capability-v1";

const INTERNAL_ORCHESTRATOR_ARGUMENT: &str = "--mez-internal-seatbelt-capability-probe";
const INTERNAL_PAYLOAD_ARGUMENT: &str = "--mez-internal-seatbelt-capability-payload";
const INTERNAL_DESCENDANT_ARGUMENT: &str = "--mez-internal-seatbelt-capability-descendant";
const PAYLOAD_SENTINEL: &str = "mez-seatbelt-capability-payload-v1";
const DESCENDANT_SENTINEL: &str = "mez-seatbelt-capability-descendant-v1";
const AMBIENT_SENTINEL_NAME: &str = "MEZ_SEATBELT_PROBE_AMBIENT_SENTINEL";
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

const FIXED_READ_SUBPATHS: &[&str] = &[
    "/System",
    "/usr",
    "/bin",
    "/sbin",
    "/private/etc",
    "/private/var/db/timezone",
];
const FIXED_READ_LITERALS: &[&str] = &["/dev/null", "/dev/random", "/dev/urandom"];

/// Dispatches one exact hidden Seatbelt probe mode from process argv.
///
/// The ordinary CLI receives `None`. Internal modes print only their fixed
/// sentinel on success and return a generic nonzero failure without exposing
/// private paths, generated profile source, or backend output.
pub(crate) fn run_internal_process(arguments: &[OsString]) -> Option<u8> {
    let mode = arguments.get(1)?.to_str()?;
    let result = match mode {
        INTERNAL_ORCHESTRATOR_ARGUMENT if arguments.len() == 3 => {
            run_orchestrator(Path::new(&arguments[2])).map(|()| SEATBELT_CAPABILITY_SENTINEL)
        }
        INTERNAL_PAYLOAD_ARGUMENT if arguments.len() == 4 => {
            run_payload(Path::new(&arguments[2]), Path::new(&arguments[3]))
                .map(|()| PAYLOAD_SENTINEL)
        }
        INTERNAL_DESCENDANT_ARGUMENT if arguments.len() == 3 => {
            run_descendant(Path::new(&arguments[2])).map(|()| DESCENDANT_SENTINEL)
        }
        INTERNAL_ORCHESTRATOR_ARGUMENT
        | INTERNAL_PAYLOAD_ARGUMENT
        | INTERNAL_DESCENDANT_ARGUMENT => Err(ProbeError::InvalidInput),
        _ => return None,
    };
    match result {
        Ok(sentinel) => {
            print!("{sentinel}");
            Some(0)
        }
        Err(error) => {
            eprintln!(
                "mez: internal Seatbelt capability probe failed (stage={})",
                error.code()
            );
            Some(1)
        }
    }
}

/// Returns the fixed typed argv used by pane and native probe transports.
pub(crate) fn orchestrator_arguments(sandbox_executable: &Path) -> Vec<String> {
    vec![
        INTERNAL_ORCHESTRATOR_ARGUMENT.to_string(),
        sandbox_executable.to_string_lossy().into_owned(),
    ]
}

/// Returns the stable digest input describing the generated probe profile.
pub(crate) fn profile_identity_bytes() -> &'static [u8] {
    b"mez-seatbelt-capability-profile-v2\0deny-default\0sysctl-read\0fixed-runtime-reads\0private-probe-write\0isolated-network\0"
}

/// Deterministic product-binary launch used to prove Seatbelt capability in
/// one exact pane or native root-process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeatbeltCapabilityProbePlan {
    /// Canonical Mezzanine product executable that owns the hidden probe mode.
    pub(crate) executable: String,
    /// Fixed internal argv excluding argv[0].
    pub(crate) arguments: Vec<String>,
    /// Exact stdout emitted only after every probe assertion succeeds.
    pub(crate) expected_stdout: &'static str,
    /// Configured fixed Seatbelt executable tested by the orchestrator.
    pub(crate) sandbox_executable: String,
    /// Metadata identity of the exact product executable under test.
    pub(crate) executable_identity_sha256: String,
    /// Metadata identity of the exact Seatbelt executable under test.
    pub(crate) sandbox_executable_identity_sha256: String,
    /// Canonical child shell selected for the later workload.
    pub(crate) child_shell_path: String,
    /// Metadata identity of the selected child shell.
    pub(crate) child_shell_identity_sha256: String,
    /// Digest of protected environment evidence selected for the action.
    pub(crate) environment_sha256: String,
    /// Digest of macOS product/build, kernel, and architecture evidence.
    pub(crate) host_identity_sha256: String,
    /// Digest of the fixed generated probe-profile template.
    pub(crate) profile_sha256: String,
    /// Stable digest of every exact probe-plan input.
    pub(crate) probe_sha256: String,
}

/// Exact reuse identity for one successful Seatbelt capability probe.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SeatbeltCapabilityCacheKey {
    /// Backend whose operation-level confinement was proven.
    pub(crate) backend: SandboxBackend,
    /// Pane or native logical pane whose environment executed the probe.
    pub(crate) pane_id: String,
    /// Bootstrap or root-process environment identity under test.
    pub(crate) pane_environment_signature: String,
    /// Configuration generation that selected the backend and environment.
    pub(crate) config_generation: u64,
    /// Canonical Mezzanine probe executable.
    pub(crate) executable: String,
    /// Configured fixed Seatbelt executable.
    pub(crate) sandbox_executable: String,
    /// Metadata identity of the Mezzanine probe executable.
    pub(crate) executable_identity_sha256: String,
    /// Metadata identity of the configured Seatbelt executable.
    pub(crate) sandbox_executable_identity_sha256: String,
    /// Selected workload child shell.
    pub(crate) child_shell_path: String,
    /// Metadata identity of the selected workload child shell.
    pub(crate) child_shell_identity_sha256: String,
    /// Digest of the protected effective environment mapping.
    pub(crate) environment_sha256: String,
    /// Digest of macOS product/build, kernel, and architecture evidence.
    pub(crate) host_identity_sha256: String,
    /// Fixed runtime-profile version exercised by the probe.
    pub(crate) runtime_profile_version: &'static str,
    /// Digest of the generated probe-profile template.
    pub(crate) profile_sha256: String,
    /// Digest of the exact probe plan that succeeded.
    pub(crate) probe_sha256: String,
}

/// Verified Seatbelt capability in one exact pane or native environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeatbeltCapability {
    /// Exact cache identity that must match before capability reuse.
    pub(crate) cache_key: SeatbeltCapabilityCacheKey,
}

/// Builds the deterministic Seatbelt capability probe for the current product
/// executable and one exact workload environment.
pub(crate) fn seatbelt_capability_probe_plan(
    config: &SeatbeltConfig,
    child_shell_path: &str,
    environment_signature: &EnvironmentSignature,
    environment_evidence: &mez_agent::shell::PaneEnvironmentEvidence,
) -> Result<SeatbeltCapabilityProbePlan, SandboxCompileError> {
    if config.executable != "/usr/bin/sandbox-exec" {
        return Err(invalid_input(
            "Seatbelt executable must be /usr/bin/sandbox-exec",
        ));
    }
    let current_executable = std::env::current_exe().map_err(|error| {
        capability_error(format!(
            "Seatbelt probe executable discovery failed: {error}"
        ))
    })?;
    seatbelt_capability_probe_plan_for_executable(
        config,
        child_shell_path,
        environment_signature,
        environment_evidence,
        &current_executable,
    )
}

#[cfg(test)]
mod canonical_executable_tests {
    use super::*;

    /// Verifies the public capability-probe entry rejects an arbitrary
    /// configured launcher before inspecting or executing host binaries.
    #[test]
    fn public_probe_rejects_noncanonical_seatbelt_executable() {
        let mut config = SeatbeltConfig {
            executable: "/usr/bin/sandbox-exec".to_string(),
            unavailable: crate::runtime::SandboxUnavailablePolicy::Fail,
            network: crate::runtime::SandboxNetworkMode::Isolated,
            environment: crate::runtime::SandboxEnvironmentPolicy::Minimal,
            env_whitelist: crate::runtime::ConfiguredSandboxEnvironment::default(),
            git_user_name: None,
            git_user_email: None,
        };
        config.executable = "/tmp/sandbox-exec".to_string();
        let request = mez_agent::shell::PaneEnvironmentRequest::new(Vec::new()).unwrap();
        let evidence = mez_agent::shell::PaneEnvironmentEvidence::restrictive(&request, "test");
        let signature = EnvironmentSignature::new(
            "macos",
            "aarch64",
            None,
            "host",
            "user",
            None,
            "/bin/sh",
            mez_agent::ShellClassification::PosixSh,
            None,
            None,
            "/tmp",
            None,
            false,
            None,
            Vec::new(),
        )
        .unwrap();

        let error =
            seatbelt_capability_probe_plan(&config, "/bin/sh", &signature, &evidence).unwrap_err();

        assert_eq!(error.kind(), SandboxCompileErrorKind::InvalidInput);
        assert_eq!(
            error.message(),
            "Seatbelt executable must be /usr/bin/sandbox-exec"
        );
    }
}

fn seatbelt_capability_probe_plan_for_executable(
    config: &SeatbeltConfig,
    child_shell_path: &str,
    environment_signature: &EnvironmentSignature,
    environment_evidence: &mez_agent::shell::PaneEnvironmentEvidence,
    current_executable: &Path,
) -> Result<SeatbeltCapabilityProbePlan, SandboxCompileError> {
    let executable = executable_identity(current_executable, "Mezzanine probe executable")?;
    let sandbox_executable = executable_identity(
        Path::new(&config.executable),
        "configured Seatbelt executable",
    )?;
    let child_shell = executable_identity(Path::new(child_shell_path), "Seatbelt child shell")?;
    if environment_signature.shell_path.is_empty()
        || environment_signature.os.is_empty()
        || environment_signature.arch.is_empty()
    {
        return Err(invalid_input(
            "Seatbelt capability probing requires a concrete environment signature",
        ));
    }
    let host_identity_sha256 = seatbelt_host_identity_sha256(environment_signature)?;
    let profile_sha256 = sha256_hex(profile_identity_bytes());
    let arguments = orchestrator_arguments(Path::new(&sandbox_executable.path));
    let mut digest = Sha256::new();
    digest.update(b"mez-seatbelt-capability-probe-plan-v1\0");
    for value in [
        SandboxBackend::Seatbelt.as_str(),
        SEATBELT_RUNTIME_PROFILE_VERSION,
        executable.path.as_str(),
        executable.identity_sha256.as_str(),
        sandbox_executable.path.as_str(),
        sandbox_executable.identity_sha256.as_str(),
        child_shell.path.as_str(),
        child_shell.identity_sha256.as_str(),
        environment_evidence.value_sha256.as_str(),
        host_identity_sha256.as_str(),
        profile_sha256.as_str(),
    ] {
        digest.update(value.as_bytes());
        digest.update(b"\0");
    }
    for argument in &arguments {
        digest.update(argument.as_bytes());
        digest.update(b"\0");
    }
    Ok(SeatbeltCapabilityProbePlan {
        executable: executable.path,
        arguments,
        expected_stdout: SEATBELT_CAPABILITY_SENTINEL,
        sandbox_executable: sandbox_executable.path,
        executable_identity_sha256: executable.identity_sha256,
        sandbox_executable_identity_sha256: sandbox_executable.identity_sha256,
        child_shell_path: child_shell.path,
        child_shell_identity_sha256: child_shell.identity_sha256,
        environment_sha256: environment_evidence.value_sha256.clone(),
        host_identity_sha256,
        profile_sha256,
        probe_sha256: hex_digest(digest.finalize()),
    })
}

/// Builds the exact cache identity for a deterministic Seatbelt probe.
pub(crate) fn seatbelt_capability_cache_key(
    pane_id: &str,
    pane_environment_signature: &str,
    config_generation: u64,
    plan: &SeatbeltCapabilityProbePlan,
) -> Result<SeatbeltCapabilityCacheKey, SandboxCompileError> {
    validate_cache_identity(pane_id, pane_environment_signature)?;
    Ok(SeatbeltCapabilityCacheKey {
        backend: SandboxBackend::Seatbelt,
        pane_id: pane_id.to_string(),
        pane_environment_signature: pane_environment_signature.to_string(),
        config_generation,
        executable: plan.executable.clone(),
        sandbox_executable: plan.sandbox_executable.clone(),
        executable_identity_sha256: plan.executable_identity_sha256.clone(),
        sandbox_executable_identity_sha256: plan.sandbox_executable_identity_sha256.clone(),
        child_shell_path: plan.child_shell_path.clone(),
        child_shell_identity_sha256: plan.child_shell_identity_sha256.clone(),
        environment_sha256: plan.environment_sha256.clone(),
        host_identity_sha256: plan.host_identity_sha256.clone(),
        runtime_profile_version: SEATBELT_RUNTIME_PROFILE_VERSION,
        profile_sha256: plan.profile_sha256.clone(),
        probe_sha256: plan.probe_sha256.clone(),
    })
}

/// Accepts only an exact successful Seatbelt sentinel and returns capability
/// evidence bound to the complete active cache identity.
pub(crate) fn parse_seatbelt_capability_probe(
    pane_id: &str,
    pane_environment_signature: &str,
    config_generation: u64,
    plan: &SeatbeltCapabilityProbePlan,
    exit_code: i32,
    stdout: &str,
) -> Result<SeatbeltCapability, SandboxCompileError> {
    if exit_code != 0 || stdout != plan.expected_stdout {
        return Err(capability_error(
            "Seatbelt did not satisfy the fixed runtime-profile capability probe",
        ));
    }
    Ok(SeatbeltCapability {
        cache_key: seatbelt_capability_cache_key(
            pane_id,
            pane_environment_signature,
            config_generation,
            plan,
        )?,
    })
}

#[derive(Debug)]
struct ExecutableIdentity {
    path: String,
    identity_sha256: String,
}

fn executable_identity(
    path: &Path,
    label: &str,
) -> Result<ExecutableIdentity, SandboxCompileError> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid_input(format!(
            "{label} must be a canonical absolute path"
        )));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| capability_error(format!("{label} metadata is unavailable: {error}")))?;
    if !super::sandbox_executable_available(path) {
        return Err(capability_error(format!(
            "{label} must be an executable regular file"
        )));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| capability_error(format!("{label} canonicalization failed: {error}")))?;
    let path = canonical
        .to_str()
        .filter(|value| !value.chars().any(char::is_control))
        .ok_or_else(|| invalid_input(format!("{label} must be printable UTF-8")))?
        .to_string();
    let mut digest = Sha256::new();
    digest.update(b"mez-sandbox-executable-identity-v1\0");
    digest.update(path.as_bytes());
    for value in [
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mode().into(),
        metadata.mtime().unsigned_abs(),
        metadata.mtime_nsec().unsigned_abs(),
    ] {
        digest.update(value.to_le_bytes());
    }
    Ok(ExecutableIdentity {
        path,
        identity_sha256: hex_digest(digest.finalize()),
    })
}

fn seatbelt_host_identity_sha256(
    signature: &EnvironmentSignature,
) -> Result<String, SandboxCompileError> {
    let mut digest = Sha256::new();
    digest.update(b"mez-seatbelt-host-identity-v1\0");
    digest.update(signature.os.as_bytes());
    digest.update(b"\0");
    digest.update(signature.arch.as_bytes());
    digest.update(b"\0");
    digest.update(signature.kernel_version.as_deref().unwrap_or("").as_bytes());
    #[cfg(target_os = "macos")]
    {
        const SYSTEM_VERSION_PATH: &str = "/System/Library/CoreServices/SystemVersion.plist";
        let product_build = fs::read(SYSTEM_VERSION_PATH).map_err(|error| {
            capability_error(format!(
                "macOS product/build identity is unavailable: {error}"
            ))
        })?;
        if product_build.len() > 64 * 1024 {
            return Err(capability_error(
                "macOS product/build identity exceeds its bounded size",
            ));
        }
        digest.update(b"\0");
        digest.update(product_build);
    }
    Ok(hex_digest(digest.finalize()))
}

fn validate_cache_identity(
    pane_id: &str,
    pane_environment_signature: &str,
) -> Result<(), SandboxCompileError> {
    if pane_id.is_empty()
        || pane_id.bytes().any(|byte| byte.is_ascii_control())
        || pane_environment_signature.is_empty()
        || pane_environment_signature
            .bytes()
            .any(|byte| byte.is_ascii_control())
    {
        return Err(invalid_input(
            "Seatbelt capability caching requires printable pane identity",
        ));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn invalid_input(message: impl Into<String>) -> SandboxCompileError {
    SandboxCompileError::new(SandboxCompileErrorKind::InvalidInput, message)
}

fn capability_error(message: impl Into<String>) -> SandboxCompileError {
    SandboxCompileError::new(SandboxCompileErrorKind::CapabilityProbeFailed, message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeError {
    InvalidInput,
    Io,
    AssertionFailed,
    Stage(&'static str),
}

impl ProbeError {
    /// Returns one bounded non-sensitive failure stage for diagnostics.
    const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid-input",
            Self::Io => "io",
            Self::AssertionFailed => "assertion",
            Self::Stage(stage) => stage,
        }
    }
}

/// Owner-only probe directory removed when orchestration settles.
struct ProbeDirectory {
    path: PathBuf,
}

impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run_orchestrator(sandbox_executable: &Path) -> Result<(), ProbeError> {
    validate_fixed_sandbox_executable(sandbox_executable)?;
    let current_executable = fs::canonicalize(std::env::current_exe().map_err(|_| ProbeError::Io)?)
        .map_err(|_| ProbeError::Io)?;
    let probe = create_probe_directory()?;
    let allowed = probe.path.join("allowed");
    let denied = probe.path.join("denied");
    create_private_directory(&allowed)?;
    create_private_directory(&denied)?;
    write_private_file(&denied.join("secret"), b"denied")?;
    let profile_path = probe.path.join("profile.sb");
    let profile = render_profile(&current_executable, &probe.path, &allowed)?;
    write_private_file(&profile_path, profile.as_bytes())?;

    let output = Command::new(sandbox_executable)
        .arg("-f")
        .arg(&profile_path)
        .arg(&current_executable)
        .arg(INTERNAL_PAYLOAD_ARGUMENT)
        .arg(&probe.path)
        .arg(&current_executable)
        .env_clear()
        .env("HOME", &allowed)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", &allowed)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|_| ProbeError::Io)?;
    if !output.status.success() {
        return Err(classify_payload_failure(
            &output.stderr,
            &allowed.join("stage"),
        ));
    }
    if output.stdout != PAYLOAD_SENTINEL.as_bytes() {
        return Err(ProbeError::Stage("payload-sentinel"));
    }
    if !output.stderr.is_empty() {
        return Err(ProbeError::Stage("payload-stderr"));
    }
    Ok(())
}

fn classify_payload_failure(stderr: &[u8], stage_path: &Path) -> ProbeError {
    const STAGES: &[&str] = &[
        "entry",
        "validate-root",
        "invalid-input",
        "io",
        "assertion",
        "ambient-environment",
        "allowed-write",
        "allowed-read",
        "sibling-read",
        "sibling-write",
        "descendant-spawn",
        "descendant-process",
        "tcp-isolation",
        "udp-isolation",
        "unix-socket-isolation",
    ];
    if let Ok(stage) = fs::read_to_string(stage_path)
        && let Some(stage) = STAGES.iter().copied().find(|candidate| *candidate == stage)
    {
        return ProbeError::Stage(stage);
    }
    let stderr = String::from_utf8_lossy(stderr);
    STAGES
        .iter()
        .copied()
        .find(|stage| stderr.contains(&format!("failed (stage={stage})")))
        .map_or(ProbeError::Stage("payload-process"), ProbeError::Stage)
}

fn run_payload(probe_root: &Path, current_executable: &Path) -> Result<(), ProbeError> {
    let allowed = probe_root.join("allowed");
    let stage_path = allowed.join("stage");
    write_probe_stage(&stage_path, "entry")?;
    write_probe_stage(&stage_path, "validate-root")?;
    validate_probe_root(probe_root)?;
    if !current_executable.is_absolute() {
        return Err(ProbeError::InvalidInput);
    }
    write_probe_stage(&stage_path, "ambient-environment")?;
    if std::env::var_os(AMBIENT_SENTINEL_NAME).is_some() {
        return Err(ProbeError::Stage("ambient-environment"));
    }
    let denied = probe_root.join("denied");
    let allowed_file = allowed.join("payload");
    write_probe_stage(&stage_path, "allowed-write")?;
    fs::write(&allowed_file, b"allowed").map_err(|_| ProbeError::Stage("allowed-write"))?;
    write_probe_stage(&stage_path, "allowed-read")?;
    if fs::read(&allowed_file).map_err(|_| ProbeError::Stage("allowed-read"))? != b"allowed" {
        return Err(ProbeError::Stage("allowed-read"));
    }
    write_probe_stage(&stage_path, "sibling-read")?;
    if fs::read(denied.join("secret")).is_ok() {
        return Err(ProbeError::Stage("sibling-read"));
    }
    write_probe_stage(&stage_path, "sibling-write")?;
    if OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(denied.join("payload"))
        .is_ok()
    {
        return Err(ProbeError::Stage("sibling-write"));
    }
    write_probe_stage(&stage_path, "descendant-spawn")?;
    let descendant = Command::new(current_executable)
        .arg(INTERNAL_DESCENDANT_ARGUMENT)
        .arg(probe_root)
        .env_clear()
        .env("HOME", &allowed)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", &allowed)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|_| ProbeError::Stage("descendant-spawn"))?;
    if !descendant.status.success()
        || descendant.stdout != DESCENDANT_SENTINEL.as_bytes()
        || !descendant.stderr.is_empty()
    {
        return Err(ProbeError::Stage("descendant-process"));
    }
    write_probe_stage(&stage_path, "tcp-isolation")?;
    if TcpListener::bind("127.0.0.1:0").is_ok() {
        return Err(ProbeError::Stage("tcp-isolation"));
    }
    write_probe_stage(&stage_path, "udp-isolation")?;
    if UdpSocket::bind("127.0.0.1:0").is_ok() {
        return Err(ProbeError::Stage("udp-isolation"));
    }
    write_probe_stage(&stage_path, "unix-socket-isolation")?;
    if UnixListener::bind(allowed.join("probe.sock")).is_ok() {
        return Err(ProbeError::Stage("unix-socket-isolation"));
    }
    Ok(())
}

fn write_probe_stage(path: &Path, stage: &'static str) -> Result<(), ProbeError> {
    fs::write(path, stage).map_err(|_| ProbeError::Stage("allowed-write"))
}

fn run_descendant(probe_root: &Path) -> Result<(), ProbeError> {
    validate_probe_root(probe_root)?;
    if fs::read(probe_root.join("denied/secret")).is_ok() {
        return Err(ProbeError::AssertionFailed);
    }
    Ok(())
}

fn create_probe_directory() -> Result<ProbeDirectory, ProbeError> {
    let temporary_root = std::env::temp_dir();
    for attempt in 0..8_u8 {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let candidate = temporary_root.join(format!(
            "mez-seatbelt-probe-{}-{unique}-{attempt}",
            std::process::id()
        ));
        let mut builder = fs::DirBuilder::new();
        builder.mode(PRIVATE_DIRECTORY_MODE);
        match builder.create(&candidate) {
            Ok(()) => {
                fs::set_permissions(
                    &candidate,
                    fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
                )
                .map_err(|_| ProbeError::Io)?;
                let path = fs::canonicalize(&candidate).map_err(|_| ProbeError::Io)?;
                return Ok(ProbeDirectory { path });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(ProbeError::Io),
        }
    }
    Err(ProbeError::Io)
}

fn create_private_directory(path: &Path) -> Result<(), ProbeError> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(PRIVATE_DIRECTORY_MODE);
    builder.create(path).map_err(|_| ProbeError::Io)?;
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
        .map_err(|_| ProbeError::Io)
}

fn write_private_file(path: &Path, content: &[u8]) -> Result<(), ProbeError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|_| ProbeError::Io)?;
    file.write_all(content).map_err(|_| ProbeError::Io)?;
    file.sync_all().map_err(|_| ProbeError::Io)?;
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .map_err(|_| ProbeError::Io)
}

fn validate_fixed_sandbox_executable(path: &Path) -> Result<(), ProbeError> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        return Err(ProbeError::InvalidInput);
    }
    if !super::sandbox_executable_available(path) {
        return Err(ProbeError::InvalidInput);
    }
    Ok(())
}

fn validate_probe_root(path: &Path) -> Result<(), ProbeError> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        return Err(ProbeError::InvalidInput);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| ProbeError::InvalidInput)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(ProbeError::InvalidInput);
    }
    Ok(())
}

fn render_profile(
    current_executable: &Path,
    probe_root: &Path,
    allowed: &Path,
) -> Result<String, ProbeError> {
    let executable = sbpl_string(current_executable)?;
    let probe_root = sbpl_string(probe_root)?;
    let allowed = sbpl_string(allowed)?;
    let mut profile = format!(
        "(version 1)\n(deny default)\n(allow process-exec)\n(allow process-fork)\n(allow signal (target same-sandbox))\n(allow sysctl-read)\n(allow file-read-data (literal \"/\"))\n(allow file-write* (literal \"/dev/null\"))\n(allow file-read* (literal {executable}))\n(allow file-read-metadata (literal {probe_root}))\n(allow file-read* file-write* (subpath {allowed}))\n"
    );
    for path in FIXED_READ_SUBPATHS {
        profile.push_str(&format!(
            "(allow file-read* (subpath {}))\n",
            sbpl_string(Path::new(path))?
        ));
    }
    for path in FIXED_READ_LITERALS {
        profile.push_str(&format!(
            "(allow file-read* (literal {}))\n",
            sbpl_string(Path::new(path))?
        ));
    }
    profile.push_str(&format!("; profile={}\n", SEATBELT_RUNTIME_PROFILE_VERSION));
    Ok(profile)
}

fn sbpl_string(path: &Path) -> Result<String, ProbeError> {
    let value = path.to_str().ok_or(ProbeError::InvalidInput)?;
    if value.contains('\0') || value.chars().any(char::is_control) {
        return Err(ProbeError::InvalidInput);
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies ordinary CLI arguments do not enter the hidden probe boundary,
    /// while malformed internal modes fail closed without running a payload.
    #[test]
    fn internal_dispatch_is_exact_and_fail_closed() {
        assert_eq!(
            run_internal_process(&[OsString::from("mez"), OsString::from("version")]),
            None
        );
        assert_eq!(
            run_internal_process(&[
                OsString::from("mez"),
                OsString::from(INTERNAL_PAYLOAD_ARGUMENT)
            ]),
            Some(1)
        );
    }

    /// Verifies the generated probe profile is deny-default, grants only its
    /// private writable subtree, and never enables ambient networking.
    #[test]
    fn profile_is_deny_default_and_path_escaped() {
        let profile = render_profile(
            Path::new("/private/tmp/mez probe/quoted\"binary"),
            Path::new("/private/tmp/mez probe"),
            Path::new("/private/tmp/mez probe/allowed"),
        )
        .unwrap();
        assert!(profile.starts_with("(version 1)\n(deny default)\n"));
        assert!(profile.contains("quoted\\\"binary"));
        assert!(profile.contains("(subpath \"/private/tmp/mez probe/allowed\")"));
        assert!(!profile.contains("(allow network"));
    }

    /// Verifies the Rust payload cannot falsely pass without Seatbelt because
    /// unconfined sibling and network operations remain available.
    #[test]
    fn payload_fails_when_run_without_seatbelt() {
        let probe = create_probe_directory().unwrap();
        create_private_directory(&probe.path.join("allowed")).unwrap();
        create_private_directory(&probe.path.join("denied")).unwrap();
        write_private_file(&probe.path.join("denied/secret"), b"denied").unwrap();
        let current_executable = std::env::current_exe().unwrap();
        assert_eq!(
            run_payload(&probe.path, &current_executable),
            Err(ProbeError::Stage("sibling-read"))
        );
    }

    /// Verifies capability plans and cache keys bind every execution-critical
    /// identity while strict parsing rejects contaminated output and nonzero
    /// exits. The fixture uses ordinary executable files so this identity
    /// contract remains testable on every supported host.
    #[test]
    fn capability_identity_and_sentinel_validation_are_exact() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "mez-seatbelt-capability-identity-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let sandbox = root.join("sandbox-exec");
        let product = root.join("mez");
        let shell = root.join("sh");
        for path in [&sandbox, &product, &shell] {
            fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = SeatbeltConfig {
            executable: sandbox.to_string_lossy().into_owned(),
            unavailable: crate::runtime::SandboxUnavailablePolicy::Fail,
            network: crate::runtime::SandboxNetworkMode::Isolated,
            environment: crate::runtime::SandboxEnvironmentPolicy::Minimal,
            env_whitelist: crate::runtime::ConfiguredSandboxEnvironment {
                requested_names: vec!["CI".to_string()],
            },
            git_user_name: None,
            git_user_email: None,
        };
        let signature = EnvironmentSignature::new(
            "macos",
            "aarch64",
            Some("Darwin-test".to_string()),
            "test-host",
            "test-user",
            None,
            shell.to_string_lossy(),
            mez_agent::ShellClassification::PosixSh,
            None,
            Some("/usr/bin:/bin".to_string()),
            root.to_string_lossy(),
            None,
            false,
            None,
            Vec::new(),
        )
        .unwrap();
        let request =
            mez_agent::shell::PaneEnvironmentRequest::new(vec!["CI".to_string()]).unwrap();
        let evidence = mez_agent::shell::PaneEnvironmentEvidence::from_parts(
            &request,
            std::collections::BTreeMap::from([("CI".to_string(), "1".to_string())]),
            std::collections::BTreeMap::new(),
        )
        .unwrap();
        let plan = seatbelt_capability_probe_plan_for_executable(
            &config,
            shell.to_str().unwrap(),
            &signature,
            &evidence,
            &product,
        )
        .unwrap();
        let key = seatbelt_capability_cache_key("%1", "environment-v1", 7, &plan).unwrap();

        assert_eq!(key.backend, SandboxBackend::Seatbelt);
        assert_eq!(key.config_generation, 7);
        assert_eq!(key.environment_sha256, evidence.value_sha256);
        assert_eq!(
            key.runtime_profile_version,
            SEATBELT_RUNTIME_PROFILE_VERSION
        );
        assert_eq!(key.profile_sha256.len(), 64);
        assert_eq!(key.probe_sha256.len(), 64);
        assert_eq!(
            plan.arguments,
            orchestrator_arguments(Path::new(&plan.sandbox_executable))
        );
        let capability = parse_seatbelt_capability_probe(
            "%1",
            "environment-v1",
            7,
            &plan,
            0,
            plan.expected_stdout,
        )
        .unwrap();
        assert_eq!(capability.cache_key, key);
        for (exit_code, output) in [
            (1, plan.expected_stdout),
            (0, ""),
            (0, "mez-seatbelt-capability-v1\n"),
            (0, "leading-mez-seatbelt-capability-v1"),
        ] {
            assert!(
                parse_seatbelt_capability_probe(
                    "%1",
                    "environment-v1",
                    7,
                    &plan,
                    exit_code,
                    output,
                )
                .is_err()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies environment, shell metadata, host architecture, and config
    /// generation changes produce distinct Seatbelt capability identities so
    /// stale successful probes cannot authorize a later workload.
    #[test]
    fn capability_identity_changes_with_bound_evidence() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "mez-seatbelt-capability-invalidation-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let sandbox = root.join("sandbox-exec");
        let product = root.join("mez");
        let shell = root.join("sh");
        for path in [&sandbox, &product, &shell] {
            fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = SeatbeltConfig {
            executable: sandbox.to_string_lossy().into_owned(),
            unavailable: crate::runtime::SandboxUnavailablePolicy::Fail,
            network: crate::runtime::SandboxNetworkMode::Isolated,
            environment: crate::runtime::SandboxEnvironmentPolicy::Minimal,
            env_whitelist: crate::runtime::ConfiguredSandboxEnvironment::default(),
            git_user_name: None,
            git_user_email: None,
        };
        let signature = |arch: &str| {
            EnvironmentSignature::new(
                "macos",
                arch,
                Some("Darwin-test".to_string()),
                "test-host",
                "test-user",
                None,
                shell.to_string_lossy(),
                mez_agent::ShellClassification::PosixSh,
                None,
                None,
                root.to_string_lossy(),
                None,
                false,
                None,
                Vec::new(),
            )
            .unwrap()
        };
        let request = mez_agent::shell::PaneEnvironmentRequest::new(Vec::new()).unwrap();
        let evidence =
            mez_agent::shell::PaneEnvironmentEvidence::restrictive(&request, "not_configured");
        let first = seatbelt_capability_probe_plan_for_executable(
            &config,
            shell.to_str().unwrap(),
            &signature("aarch64"),
            &evidence,
            &product,
        )
        .unwrap();
        let other_host = seatbelt_capability_probe_plan_for_executable(
            &config,
            shell.to_str().unwrap(),
            &signature("x86_64"),
            &evidence,
            &product,
        )
        .unwrap();
        fs::write(&shell, b"#!/bin/sh\n# changed identity\nexit 0\n").unwrap();
        let other_shell = seatbelt_capability_probe_plan_for_executable(
            &config,
            shell.to_str().unwrap(),
            &signature("aarch64"),
            &evidence,
            &product,
        )
        .unwrap();

        assert_ne!(first.host_identity_sha256, other_host.host_identity_sha256);
        assert_ne!(first.probe_sha256, other_host.probe_sha256);
        assert_ne!(
            first.child_shell_identity_sha256,
            other_shell.child_shell_identity_sha256
        );
        assert_ne!(first.probe_sha256, other_shell.probe_sha256);
        assert_ne!(
            seatbelt_capability_cache_key("%1", "env", 1, &first).unwrap(),
            seatbelt_capability_cache_key("%1", "env", 2, &first).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
