//! Shell discovery and resolution.
//!
//! The specification treats `SHELL` as authoritative only when it is absolute
//! and executable, then falls back to `/bin/sh`. This module implements that
//! precedence without consulting hidden host-side alternatives.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::{MezError, Result};
use crate::host::process::wait_for_child_with_timeout;

/// Maximum time spent collecting syntax-neutral shell version evidence.
const SHELL_VERSION_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// Maximum retained bytes from one shell version probe.
const SHELL_VERSION_PROBE_MAX_BYTES: u64 = 4 * 1024;

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
    /// Classification derived from path plus bounded runtime version evidence.
    classification: mez_agent::ShellClassification,
    /// First bounded line emitted by `<shell> --version`, when available.
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

    /// Creates resolved shell metadata after collecting bounded runtime
    /// version evidence from the exact executable path.
    fn with_runtime_probe(path: PathBuf, source: ShellSource) -> Self {
        let version_probe = probe_shell_version(&path);
        let classification =
            mez_agent::ShellClassification::classify_with_probe(&path, version_probe.as_deref());
        Self {
            path,
            source,
            classification,
            version_probe,
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

    /// Returns bounded version evidence captured before bootstrap rendering.
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
        let version_probe = shell.version_probe().map(ToOwned::to_owned);
        let classification = if shell.classification().is_empty() {
            mez_agent::ShellClassification::classify_with_probe(&path, version_probe.as_deref())
        } else {
            mez_agent::ShellClassification::classify_with_probe(
                Path::new(shell.classification()),
                version_probe.as_deref(),
            )
        };
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
            return Ok(ResolvedShell::with_runtime_probe(
                candidate_path,
                ShellSource::ShellEnv,
            ));
        }
    }

    if fallback.is_absolute() && is_executable(fallback) {
        return Ok(ResolvedShell::with_runtime_probe(
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

/// Collects one bounded version line from an exact resolved shell executable.
///
/// The probe passes only `--version`, supplies no stdin, discards stderr, and
/// kills and reaps the child at the deadline. A reader thread drains at most
/// the retained byte limit so an unexpectedly verbose executable cannot block
/// on its stdout pipe while the parent waits.
fn probe_shell_version(path: &Path) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout
            .take(SHELL_VERSION_PROBE_MAX_BYTES)
            .read_to_end(&mut bytes);
        bytes
    });
    let completed = wait_for_child_with_timeout(&mut child, SHELL_VERSION_PROBE_TIMEOUT)
        .ok()
        .flatten();
    if completed.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let bytes = reader.join().ok()?;
    completed?.success().then_some(())?;
    String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests {
    use super::{OsStr, Path, PathBuf, ShellSource, fs, resolve_shell_with_fallback};
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

    /// Verifies bounded runtime version evidence identifies Fish even when the
    /// executable basename no longer says `fish`. Wrapper dialect selection
    /// must happen after this probe so renamed Fish never receives POSIX
    /// bootstrap source.
    #[cfg(unix)]
    #[test]
    fn version_probe_classifies_renamed_fish_shell() {
        use std::os::unix::fs::symlink;

        let fish = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("fish"))
                .find(|candidate| candidate.is_file())
        });
        let Some(fish) = fish else {
            return;
        };
        let renamed = std::env::temp_dir().join(format!(
            "mez-renamed-shell-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_file(&renamed);
        symlink(&fish, &renamed).unwrap();

        let resolved =
            resolve_shell_with_fallback(Some(renamed.as_os_str()), Path::new("/bin/sh")).unwrap();

        assert_eq!(
            resolved.classification(),
            mez_agent::ShellClassification::Fish
        );
        assert!(
            resolved
                .version_probe()
                .is_some_and(|version| version.to_ascii_lowercase().contains("fish"))
        );

        let _ = fs::remove_file(renamed);
    }
}
