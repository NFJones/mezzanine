//! Shell discovery and resolution.
//!
//! The specification treats `SHELL` as authoritative only when it is absolute
//! and executable, then falls back to `/bin/sh`. This module implements that
//! precedence without consulting hidden host-side alternatives.
//!
//! Session-shell classification is derived only from the recorded absolute
//! path basename, never by executing the resolved shell. A renamed or wrapper
//! executable therefore settles as `UnknownUnix` and degrades to native mode
//! until an authenticated receiver attests its dialect: the capability cost of
//! losing first-bootstrap syntax for renamed shells is accepted so `--version`
//! output can never promote a shell the runtime has not verified.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};

/// Carries Shell Source state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellSource {
    /// Represents the Shell Env case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ShellEnv,
    /// Represents the Fallback Bin Sh case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FallbackBinSh,
}

/// Carries Resolved Shell state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedShell {
    /// Stores the path value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    path: PathBuf,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    source: ShellSource,
    /// Classification derived from the recorded absolute path basename only.
    classification: mez_agent::ShellClassification,
    /// Recorded version text retained from a session record, when any. It is
    /// never collected by executing the shell and never promotes the
    /// classification.
    version_probe: Option<String>,
}

impl ResolvedShell {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(path: PathBuf, source: ShellSource) -> Self {
        let classification = mez_agent::ShellClassification::classify(&path);
        Self {
            path,
            source,
            classification,
            version_probe: None,
        }
    }

    /// Runs the path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs the source operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn source(&self) -> &ShellSource {
        &self.source
    }

    /// Runs the used fallback operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn used_fallback(&self) -> bool {
        self.source == ShellSource::FallbackBinSh
    }

    /// Returns the classification selected from path and bounded probe evidence.
    pub fn classification(&self) -> mez_agent::ShellClassification {
        self.classification
    }

    /// Returns recorded session version text, when a session record carried it.
    ///
    /// Resolution never executes the shell to collect this evidence, and it
    /// never selects or promotes the classification.
    pub fn version_probe(&self) -> Option<&str> {
        self.version_probe.as_deref()
    }
}

impl From<ResolvedShell> for mez_mux::session::SessionShell {
    fn from(shell: ResolvedShell) -> Self {
        let source = match shell.source() {
            ShellSource::ShellEnv => "shell-env",
            ShellSource::FallbackBinSh => "fallback-bin-sh",
        };
        let classification = shell.classification().as_str().to_string();
        let version_probe = shell.version_probe.clone();
        mez_mux::session::SessionShell::new(
            shell.path().to_path_buf(),
            source,
            shell.used_fallback(),
        )
        .with_execution_identity(classification, version_probe)
    }
}

impl From<mez_mux::session::SessionShell> for ResolvedShell {
    fn from(shell: mez_mux::session::SessionShell) -> Self {
        let source = if shell.used_fallback() {
            ShellSource::FallbackBinSh
        } else {
            ShellSource::ShellEnv
        };
        let path = shell.path().to_path_buf();
        // Classification comes only from the recorded absolute path basename:
        // neither the recorded classification string nor recorded version text
        // may promote a renamed or wrapper executable to a dialect.
        let classification = mez_agent::ShellClassification::classify(&path);
        let version_probe = shell.version_probe().map(ToOwned::to_owned);
        Self {
            path,
            source,
            classification,
            version_probe,
        }
    }
}

/// Runs the resolve shell from process operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "test-only adapter retained for focused boundary coverage"
)]
pub fn resolve_shell_from_process() -> Result<ResolvedShell> {
    resolve_shell(std::env::var_os("SHELL"))
}

/// Runs the resolve shell operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn resolve_shell(shell_env: Option<OsString>) -> Result<ResolvedShell> {
    resolve_shell_with_fallback(shell_env.as_deref(), Path::new("/bin/sh"))
}

/// Runs the resolve shell with fallback operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn resolve_shell_with_fallback(
    shell_env: Option<&OsStr>,
    fallback: &Path,
) -> Result<ResolvedShell> {
    if let Some(candidate) = shell_env {
        let candidate_path = PathBuf::from(candidate);
        if !candidate.is_empty() && candidate_path.is_absolute() && is_executable(&candidate_path) {
            return Ok(ResolvedShell::new(candidate_path, ShellSource::ShellEnv));
        }
    }

    if fallback.is_absolute() && is_executable(fallback) {
        return Ok(ResolvedShell::new(
            fallback.to_path_buf(),
            ShellSource::FallbackBinSh,
        ));
    }

    Err(MezError::invalid_state(
        "no usable shell found: SHELL is unset or unusable and /bin/sh is unavailable",
    ))
}

/// Runs the is executable operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests {
    use super::{
        OsStr, Path, PathBuf, ResolvedShell, ShellSource, fs, resolve_shell_with_fallback,
    };
    use std::fs::File;

    /// Runs the make executable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    /// Runs the temp file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn temp_file(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("mez-shell-test-{name}-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        File::create(&path).unwrap();
        #[cfg(unix)]
        make_executable(&path);
        path
    }

    /// Verifies uses absolute executable shell env.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn uses_absolute_executable_shell_env() {
        let shell = temp_file("shell");
        let fallback = temp_file("fallback");

        let resolved = resolve_shell_with_fallback(Some(shell.as_os_str()), &fallback).unwrap();

        assert_eq!(resolved.path(), shell.as_path());
        assert_eq!(resolved.source(), &ShellSource::ShellEnv);

        let _ = fs::remove_file(shell);
        let _ = fs::remove_file(fallback);
    }

    /// Verifies falls back when shell env is relative.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn falls_back_when_shell_env_is_relative() {
        let fallback = temp_file("fallback-relative");

        let resolved = resolve_shell_with_fallback(Some(OsStr::new("bash")), &fallback).unwrap();

        assert_eq!(resolved.path(), fallback.as_path());
        assert_eq!(resolved.source(), &ShellSource::FallbackBinSh);

        let _ = fs::remove_file(fallback);
    }

    /// Verifies the normal absolute Bash, Fish, Zsh, and POSIX paths still
    /// select their adapters from the recorded basename alone, without any
    /// version evidence collected by executing the shell.
    #[test]
    fn standard_absolute_shell_paths_keep_their_classification() {
        for (path, expected) in [
            ("/bin/bash", mez_agent::ShellClassification::Bash),
            ("/usr/bin/bash", mez_agent::ShellClassification::Bash),
            ("/usr/bin/fish", mez_agent::ShellClassification::Fish),
            ("/bin/zsh", mez_agent::ShellClassification::Zsh),
            ("/bin/dash", mez_agent::ShellClassification::PosixSh),
            ("/bin/sh", mez_agent::ShellClassification::PosixSh),
        ] {
            let resolved = ResolvedShell::new(PathBuf::from(path), ShellSource::ShellEnv);
            assert_eq!(
                resolved.classification(),
                expected,
                "{path} must keep its session classification"
            );
            assert!(resolved.version_probe().is_none());
        }
    }

    /// Verifies a renamed executable is classified by its absolute path
    /// basename only and is never launched with `--version`: the sentinel the
    /// executable would write on any launch must stay absent, and the shell
    /// settles unknown instead of being promoted to a dialect.
    #[cfg(unix)]
    #[test]
    fn renamed_executable_is_classified_by_path_only_and_never_executed() {
        use std::io::Write as _;

        let sentinel = std::env::temp_dir().join(format!(
            "mez-shell-probe-sentinel-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_file(&sentinel);
        let renamed = std::env::temp_dir().join(format!(
            "mez-renamed-shell-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_file(&renamed);
        {
            let mut file = File::create(&renamed).unwrap();
            file.write_all(
                format!("#!/bin/sh\nprintf launched > '{}'\n", sentinel.display()).as_bytes(),
            )
            .unwrap();
        }
        make_executable(&renamed);

        let resolved =
            resolve_shell_with_fallback(Some(renamed.as_os_str()), Path::new("/bin/sh")).unwrap();

        assert_eq!(
            resolved.classification(),
            mez_agent::ShellClassification::UnknownUnix,
            "a renamed executable must settle unknown instead of being promoted"
        );
        assert!(resolved.version_probe().is_none());
        assert!(
            !sentinel.exists(),
            "resolving a session shell must never execute it to read version text"
        );

        let _ = fs::remove_file(renamed);
        let _ = fs::remove_file(sentinel);
    }

    /// Verifies a restored session record still classifies normal absolute
    /// Bash/Fish/Zsh paths by basename, and that neither a recorded
    /// classification string nor recorded version text promotes a renamed
    /// path to a dialect it was never verified against.
    #[test]
    fn restored_session_shell_classifies_by_basename_only() {
        let fish =
            mez_mux::session::SessionShell::new(PathBuf::from("/usr/bin/fish"), "shell-env", false)
                .with_execution_identity("fish", Some("fish, version 3.7.1".to_string()));
        let resolved = ResolvedShell::from(fish);
        assert_eq!(
            resolved.classification(),
            mez_agent::ShellClassification::Fish
        );
        assert_eq!(resolved.path(), Path::new("/usr/bin/fish"));
        assert_eq!(
            resolved.version_probe(),
            Some("fish, version 3.7.1"),
            "recorded version text must stay inert metadata"
        );

        let renamed = mez_mux::session::SessionShell::new(
            PathBuf::from("/opt/custom-shell"),
            "shell-env",
            false,
        )
        .with_execution_identity("fish", Some("fish, version 3.7.1".to_string()));
        let resolved = ResolvedShell::from(renamed);
        assert_eq!(
            resolved.classification(),
            mez_agent::ShellClassification::UnknownUnix,
            "recorded classification and version text must not promote a renamed path"
        );
    }
}
