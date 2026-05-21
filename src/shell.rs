//! Shell discovery and resolution.
//!
//! The specification treats `SHELL` as authoritative only when it is absolute
//! and executable, then falls back to `/bin/sh`. This module implements that
//! precedence without consulting hidden host-side alternatives.

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
}

impl ResolvedShell {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(path: PathBuf, source: ShellSource) -> Self {
        Self { path, source }
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
}

/// Runs the resolve shell from process operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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
}
