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

/// Fully inferred execution context for one native spawned shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeShellContext {
    /// Absolute shell executable selected by the inference chain.
    shell_path: PathBuf,
    /// Shell grammar selected for the executable.
    classification: ShellClassification,
    /// Root-process environment overlaid on the parent `mez` environment.
    environment: Vec<RawEnvironmentEntry>,
    /// Root-process working directory for the spawned shell.
    working_directory: PathBuf,
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

    /// Returns the raw pane-root environment overlaid on the spawned shell.
    pub(crate) fn environment(&self) -> &[RawEnvironmentEntry] {
        &self.environment
    }

    /// Returns the working directory for the spawned shell.
    pub(crate) fn working_directory(&self) -> &Path {
        &self.working_directory
    }
}

#[cfg(test)]
impl NativeShellContext {
    /// Builds one context fixture without running host inference.
    pub(crate) fn for_test(
        shell_path: PathBuf,
        environment: Vec<RawEnvironmentEntry>,
        working_directory: PathBuf,
    ) -> Self {
        let classification = ShellClassification::classify(&shell_path);
        Self {
            shell_path,
            classification,
            environment,
            working_directory,
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
    let environment = environment.unwrap_or_default();
    let (shell_path, classification) =
        select_native_shell_path(executable_path.as_deref(), &environment, session_shell_path)?;
    Ok(NativeShellContext {
        shell_path,
        classification,
        environment,
        working_directory,
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

    /// Verifies unavailable pane-root environment metadata leaves an empty
    /// overlay so the executor can retain the parent `mez` environment.
    #[test]
    fn inference_allows_unavailable_root_process_environment_overlay() {
        let context = infer_native_shell_context(
            Some(42),
            Some(PathBuf::from("/bin/bash")),
            None,
            Some(PathBuf::from("/tmp/work")),
            Path::new("/bin/sh"),
        )
        .expect("parent environment remains available without an overlay");
        assert!(context.environment().is_empty());
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
