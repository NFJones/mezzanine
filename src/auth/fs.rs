//! Filesystem safety and credential-helper utility functions.
//!
//! Helpers centralize private permissions, safe provider-name validation, path
//! containment checks, executable detection, and command-output secret parsing.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use secrecy::SecretString;
use zeroize::Zeroizing;

use crate::error::{MezError, Result};

/// Runs the ensure private dir operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Runs the set private file operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_private_file(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Runs the is executable file operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Runs the path is under directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn path_is_under_directory(path: &Path, directory: &Path) -> bool {
    !path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::Prefix(_)
        )
    }) && path.starts_with(directory)
}

/// Runs the validate safe name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_safe_name(value: &str, message: &str) -> Result<()> {
    if value.is_empty() || !value.bytes().all(is_safe_name_byte) {
        return Err(MezError::invalid_args(message));
    }
    Ok(())
}

/// Runs the is safe name byte operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_safe_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

/// Runs the secret from command stdout operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn secret_from_command_stdout(stdout: Vec<u8>) -> Result<Option<SecretString>> {
    if stdout.is_empty() {
        return Ok(None);
    }

    let mut secret = Zeroizing::new(String::from_utf8(stdout).map_err(|_| {
        MezError::invalid_state("operating system credential command returned invalid UTF-8")
    })?);
    if secret.ends_with('\n') {
        secret.pop();
        if secret.ends_with('\r') {
            secret.pop();
        }
    }
    if secret.is_empty() {
        Ok(None)
    } else {
        Ok(Some(SecretString::from(secret.as_str().to_string())))
    }
}
