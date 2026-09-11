//! Native shell context inference for spawned-shell execution.
//!
//! Native shell mode executes agent actions in a freshly spawned shell
//! process without writing to or reading from the pane PTY. This module
//! derives that process's shell path, grammar, environment, and working
//! directory from the pane's root process through host process inspection
//! alone. No command is ever executed through the pane shell to build this
//! context, so the mode keeps working while an alternative screen application
//! occupies the pane.

use std::path::{Path, PathBuf};

use mez_agent::ShellClassification;
use mez_mux::process::RawEnvironmentEntry;

use crate::error::{MezError, Result};

use super::native_workload_environment::{
    NativeLaunchEnvironmentRole, NativeWorkloadEnvironment, compose_native_workload_environment,
    native_ambient_environment,
};

/// Fully inferred execution context for one native spawned shell.
///
/// The context never expresses ambient inheritance. Every native launch starts
/// from a cleared environment and receives exactly the composed
/// [`NativeWorkloadEnvironment`] bucket that matches its launch role, so an
/// ambient-only credential that exists only in the daemon environment cannot
/// reach a workload shell, a workload interpreter, or a code-owned launcher. A
/// value the pane root itself carries, including one the pane inherited when it
/// was created, is pane evidence and is forwarded by design: pane creation owns
/// that inheritance boundary rather than this context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeShellContext {
    /// Absolute shell executable selected by the inference chain.
    shell_path: PathBuf,
    /// Shell grammar selected for the executable.
    classification: ShellClassification,
    /// Composed native workload environment for this launch.
    environment: NativeWorkloadEnvironment,
    /// Root-process working directory for the spawned shell.
    working_directory: PathBuf,
    /// Environment bucket consumed by the launched process.
    role: NativeLaunchEnvironmentRole,
}

impl NativeShellContext {
    /// Returns the shell executable path paired with this context.
    pub(crate) fn shell_path(&self) -> &Path {
        &self.shell_path
    }

    /// Returns the shell grammar paired with this context.
    pub(crate) fn classification(&self) -> ShellClassification {
        self.classification
    }

    /// Returns the validated pane-root evidence carried by this context.
    ///
    /// Environment signatures and configured forwarding evidence read this
    /// evidence because it is the authoritative pane-provided source. Entries
    /// the host reported as malformed, oversized, or unportable are absent.
    pub(crate) fn environment(&self) -> &[RawEnvironmentEntry] {
        self.environment.pane_root_evidence()
    }

    /// Returns the composed native workload environment for this launch.
    pub(crate) fn workload_environment(&self) -> &NativeWorkloadEnvironment {
        &self.environment
    }

    /// Returns the environment entries the launched process receives.
    ///
    /// Workload launches receive the workload bucket. Code-owned sandbox
    /// launchers receive only the launcher/control bucket so internal transport
    /// requirements never become workload-visible and workload credentials are
    /// never copied into a launcher.
    pub(crate) fn launch_environment(&self) -> &[RawEnvironmentEntry] {
        self.environment.for_role(self.role)
    }

    /// Returns the working directory for the spawned shell.
    pub(crate) fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    /// Returns a copy whose launched process receives one launch-role bucket.
    ///
    /// The native dispatch selects the role, so a compiled Bubblewrap or
    /// Seatbelt dispatch hands the code-owned launcher only the launcher
    /// control bucket while every other native launch receives the workload
    /// bucket.
    pub(crate) fn for_launch_role(mut self, role: NativeLaunchEnvironmentRole) -> Self {
        self.role = role;
        self
    }

    /// Builds a credential-free context for one admitted pane-status provider.
    ///
    /// The compiled sandbox launch supplies its own minimal payload environment,
    /// so pane evidence and workload entries are dropped. The outer code-owned
    /// launcher keeps only its declared launcher control entries, such as the
    /// command-search `PATH` a bare launcher executable needs, and never sees a
    /// pane credential.
    pub(crate) fn restricted_for_pane_status_provider(&self) -> Self {
        Self {
            shell_path: self.shell_path.clone(),
            classification: self.classification,
            environment: self.environment.restricted_to_launcher(),
            working_directory: self.working_directory.clone(),
            role: NativeLaunchEnvironmentRole::SandboxLauncher,
        }
    }
}

#[cfg(test)]
impl NativeShellContext {
    /// Builds one context fixture without running host inference.
    ///
    /// The fixture composes the supplied pane-root evidence through the shared
    /// builder and captures the ambient test-process environment, so fixtures
    /// follow the same composition rules as production inference without
    /// mutating the process environment.
    pub(crate) fn for_test(
        shell_path: PathBuf,
        environment: Vec<RawEnvironmentEntry>,
        working_directory: PathBuf,
    ) -> Self {
        let composed = compose_native_workload_environment(
            &environment,
            &native_ambient_environment(),
            &shell_path,
        )
        .expect("fixture composition never requires pane identity");
        Self::for_test_composed(shell_path, composed, working_directory)
    }

    /// Builds one context fixture around an explicitly composed environment.
    pub(crate) fn for_test_composed(
        shell_path: PathBuf,
        environment: NativeWorkloadEnvironment,
        working_directory: PathBuf,
    ) -> Self {
        let classification = ShellClassification::classify(&shell_path);
        Self {
            shell_path,
            classification,
            environment,
            working_directory,
            role: NativeLaunchEnvironmentRole::Workload,
        }
    }
}

/// Infers native shell context from pane root-process metadata.
///
/// # Parameters
/// - `primary_pid`: Live pane root process id fenced by the host readers.
/// - `executable_path`: Host-reported root process executable path.
/// - `environment`: Host-reported root process exec-time environment.
/// - `current_working_directory`: Host-reported root process working
///   directory.
/// - `session_shell_path`: Spawn-time session shell recorded for the pane.
///
/// The returned context always carries a composed native workload environment
/// that starts from a cleared base. Malformed host entries are dropped, and the
/// ambient `mez` environment is consulted only for requirements that declare
/// forwarding (`PATH`, `HOME`, and the launcher search path). Inference never
/// claims to represent a remote shell environment: the context describes the
/// local pane root process only, and stricter status-provider contexts derive
/// from it through [`NativeShellContext::restricted_for_pane_status_provider`].
///
/// # Errors
/// Returns an error when the pane has no live primary process, the host
/// exposes no readable working directory, or no usable shell can be selected
/// from the fallback chain.
pub(crate) fn infer_native_shell_context(
    primary_pid: Option<u32>,
    executable_path: Option<PathBuf>,
    environment: Option<Vec<RawEnvironmentEntry>>,
    current_working_directory: Option<PathBuf>,
    session_shell_path: &Path,
) -> Result<NativeShellContext> {
    let primary_pid = primary_pid.ok_or_else(|| {
        MezError::invalid_state("native shell mode requires a live pane root process")
    })?;
    let working_directory = current_working_directory.ok_or_else(|| {
        MezError::invalid_state(format!(
            "native shell mode requires a readable root-process working directory for pid {primary_pid}"
        ))
    })?;
    let raw_environment = environment.unwrap_or_default();
    let (shell_path, classification) = select_native_shell_path(
        executable_path.as_deref(),
        &raw_environment,
        session_shell_path,
    )?;
    let environment = compose_native_workload_environment(
        &raw_environment,
        &native_ambient_environment(),
        &shell_path,
    )?;
    Ok(NativeShellContext {
        shell_path,
        classification,
        environment,
        working_directory,
        role: NativeLaunchEnvironmentRole::Workload,
    })
}

/// Selects the spawned shell executable through the documented fallback chain.
///
/// The root-process executable wins when it is a known shell, then `SHELL`
/// from the root-process environment (preserved across `exec` replacement),
/// then the spawn-time session shell, then `/bin/sh`.
fn select_native_shell_path(
    executable_path: Option<&Path>,
    environment: &[RawEnvironmentEntry],
    session_shell_path: &Path,
) -> Result<(PathBuf, ShellClassification)> {
    if let Some(path) = executable_path {
        let classification = ShellClassification::classify(path);
        if classification != ShellClassification::UnknownUnix {
            return Ok((path.to_path_buf(), classification));
        }
    }
    if let Some((path, classification)) = shell_from_environment(environment) {
        return Ok((path, classification));
    }
    for candidate in [session_shell_path, Path::new("/bin/sh")] {
        let classification = ShellClassification::classify(candidate);
        if classification != ShellClassification::UnknownUnix {
            return Ok((candidate.to_path_buf(), classification));
        }
    }
    Err(MezError::invalid_state(
        "native shell mode could not select a usable shell from pane root-process metadata",
    ))
}

/// Recovers a known shell from the `SHELL` entry of a raw environment.
///
/// Relative or unclassifiable paths are ignored so the caller can fall
/// through to the spawn-time session shell.
fn shell_from_environment(
    environment: &[RawEnvironmentEntry],
) -> Option<(PathBuf, ShellClassification)> {
    let shell = environment.iter().find(|entry| entry.key == b"SHELL")?;
    #[cfg(unix)]
    let path: Option<PathBuf> = {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(&shell.value)))
    };
    #[cfg(not(unix))]
    let path: Option<PathBuf> = None;
    let path = path?;
    if !path.is_absolute() {
        return None;
    }
    let classification = ShellClassification::classify(&path);
    (classification != ShellClassification::UnknownUnix).then_some((path, classification))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one raw environment entry for inference tests.
    fn entry(key: &str, value: &str) -> RawEnvironmentEntry {
        RawEnvironmentEntry {
            key: key.as_bytes().to_vec(),
            value: value.as_bytes().to_vec(),
        }
    }

    /// Returns one entry value as text for environment assertions.
    fn value_of<'a>(entries: &'a [RawEnvironmentEntry], key: &str) -> Option<&'a str> {
        entries
            .iter()
            .find(|entry| entry.key.as_slice() == key.as_bytes())
            .and_then(|entry| std::str::from_utf8(&entry.value).ok())
    }

    /// Builds one successful inference result from the supplied metadata.
    fn context(
        executable: Option<&str>,
        environment: Vec<RawEnvironmentEntry>,
        session_shell: &str,
    ) -> NativeShellContext {
        infer_native_shell_context(
            Some(42),
            executable.map(PathBuf::from),
            Some(environment),
            Some(PathBuf::from("/tmp/work")),
            Path::new(session_shell),
        )
        .expect("inference succeeds")
    }

    /// Verifies the root shell executable wins over environment and session
    /// shell evidence, matching the documented inference precedence.
    #[test]
    fn inference_prefers_root_shell_executable() {
        let context = context(
            Some("/bin/bash"),
            vec![entry("SHELL", "/bin/zsh"), entry("PATH", "/usr/bin")],
            "/bin/sh",
        );

        assert_eq!(context.shell_path(), Path::new("/bin/bash"));
        assert_eq!(context.classification(), ShellClassification::Bash);
        assert_eq!(context.working_directory(), Path::new("/tmp/work"));
        assert_eq!(context.environment().len(), 2);
    }

    /// Verifies `SHELL` from the root-process environment recovers the pane
    /// shell after the root executable was replaced by a screen application.
    #[test]
    fn inference_recovers_shell_from_environment_after_exec_replacement() {
        let context = context(
            Some("/usr/bin/tmux"),
            vec![entry("SHELL", "/bin/zsh")],
            "/bin/sh",
        );

        assert_eq!(context.shell_path(), Path::new("/bin/zsh"));
        assert_eq!(context.classification(), ShellClassification::Zsh);
    }

    /// Verifies the spawn-time session shell is used when neither the root
    /// executable nor the environment exposes a shell.
    #[test]
    fn inference_falls_back_to_session_shell_without_shell_environment() {
        let context = context(
            Some("/usr/bin/tmux"),
            vec![entry("PATH", "/usr/bin")],
            "/bin/bash",
        );

        assert_eq!(context.shell_path(), Path::new("/bin/bash"));
        assert_eq!(context.classification(), ShellClassification::Bash);
    }

    /// Verifies `/bin/sh` closes the chain when every earlier source is
    /// absent or unclassifiable.
    #[test]
    fn inference_falls_back_to_bin_sh_without_any_shell_evidence() {
        let context = context(Some("/usr/bin/tmux"), Vec::new(), "/opt/unknown-shell");

        assert_eq!(context.shell_path(), Path::new("/bin/sh"));
        assert_eq!(context.classification(), ShellClassification::PosixSh);
    }

    /// Verifies relative or unclassifiable `SHELL` values fall through to
    /// the session shell instead of selecting an unusable executable.
    #[test]
    fn inference_ignores_relative_or_unclassifiable_shell_environment() {
        let relative = context(
            Some("/usr/bin/tmux"),
            vec![entry("SHELL", "bin/zsh")],
            "/bin/bash",
        );
        assert_eq!(relative.shell_path(), Path::new("/bin/bash"));

        let unclassifiable = context(
            Some("/usr/bin/tmux"),
            vec![entry("SHELL", "/usr/bin/tcsh")],
            "/bin/bash",
        );
        assert_eq!(unclassifiable.shell_path(), Path::new("/bin/bash"));
    }

    /// Verifies inference requires a live pane root process.
    #[test]
    fn inference_requires_live_root_process() {
        let error = infer_native_shell_context(
            None,
            Some(PathBuf::from("/bin/bash")),
            Some(Vec::new()),
            Some(PathBuf::from("/tmp/work")),
            Path::new("/bin/sh"),
        )
        .expect_err("missing pid must fail");
        assert!(error.to_string().contains("root process"));
    }

    /// Verifies unavailable pane-root environment metadata leaves empty
    /// evidence while the composed workload still receives the declared
    /// fallback requirements, so no launch restores ambient inheritance.
    #[test]
    fn inference_allows_unavailable_root_process_environment_evidence() {
        let context = infer_native_shell_context(
            Some(42),
            Some(PathBuf::from("/bin/bash")),
            None,
            Some(PathBuf::from("/tmp/work")),
            Path::new("/bin/sh"),
        )
        .expect("declared requirements replace unavailable pane-root evidence");
        assert!(context.environment().is_empty());
        assert!(
            value_of(context.workload_environment().workload(), "PATH")
                .is_some_and(|path| !path.is_empty())
        );
    }

    /// Verifies one inferred context composes the workload environment from
    /// validated pane-root evidence plus the declared requirements instead of
    /// inheriting the ambient daemon environment.
    #[test]
    fn inference_composes_workload_environment_from_pane_evidence() {
        let context = context(
            Some("/bin/bash"),
            vec![
                entry("MEZ_PANE_PROVIDED", "pane-value"),
                entry("PATH", "/pane/bin"),
            ],
            "/bin/sh",
        );
        let workload = context.workload_environment().workload();

        assert_eq!(context.launch_environment(), workload);
        assert_eq!(value_of(workload, "PATH"), Some("/pane/bin"));
        assert_eq!(value_of(workload, "SHELL"), Some("/bin/bash"));
        assert_eq!(value_of(workload, "MEZ_PANE_PROVIDED"), Some("pane-value"));
    }

    /// Verifies one sandbox-launcher context exposes only the launcher control
    /// bucket, so pane credentials never reach the outer launcher process.
    #[test]
    fn inference_sandbox_launcher_role_keeps_only_launcher_control_entries() {
        let context = context(
            Some("/bin/bash"),
            vec![entry("MEZ_PANE_CREDENTIAL", "pane-secret")],
            "/bin/sh",
        )
        .for_launch_role(NativeLaunchEnvironmentRole::SandboxLauncher);
        let launcher = context.launch_environment();

        assert_eq!(value_of(launcher, "MEZ_PANE_CREDENTIAL"), None);
        assert!(value_of(launcher, "PATH").is_some());
        assert_eq!(context.environment().len(), 1);
    }

    /// Verifies the stricter pane-status-provider context drops pane evidence
    /// while keeping the launcher search path for the outer launcher process.
    #[test]
    fn inference_restricts_status_provider_context_to_launcher_entries() {
        let context = context(
            Some("/bin/bash"),
            vec![entry("MEZ_PANE_CREDENTIAL", "pane-secret")],
            "/bin/sh",
        )
        .restricted_for_pane_status_provider();

        assert!(context.environment().is_empty());
        assert!(value_of(context.launch_environment(), "PATH").is_some());
        assert_eq!(
            value_of(context.launch_environment(), "MEZ_PANE_CREDENTIAL"),
            None
        );
    }

    /// Verifies inference requires a readable root-process working directory.
    #[test]
    fn inference_requires_readable_working_directory() {
        let error = infer_native_shell_context(
            Some(42),
            Some(PathBuf::from("/bin/bash")),
            Some(Vec::new()),
            None,
            Path::new("/bin/sh"),
        )
        .expect_err("missing cwd must fail");
        assert!(error.to_string().contains("working directory"));
    }
}
