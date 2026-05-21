//! Private file fallback credential store.
//!
//! File fallback storage is constrained to the configured auth-secret directory
//! and uses private permissions for both directories and secret files.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use crate::error::{MezError, Result};

use super::fs::{
    ensure_private_dir, path_is_under_directory, set_private_file, validate_safe_name,
};
use super::types::{CredentialStore, CredentialStoreKind};

/// Carries Private File Credential Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateFileCredentialStore {
    /// Stores the directory value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    directory: PathBuf,
}

impl PrivateFileCredentialStore {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// Runs the directory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Runs the secret path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn secret_path(&self, provider: &str) -> Result<PathBuf> {
        validate_safe_name(provider, "provider name is not file-safe")?;
        Ok(self.directory.join(format!("{provider}.secret")))
    }

    /// Runs the path from reference operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn path_from_reference(&self, reference: &str) -> Result<Option<PathBuf>> {
        let Some(path) =
            reference.strip_prefix(CredentialStoreKind::PrivateFileFallback.reference_prefix())
        else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        if !path_is_under_directory(&path, &self.directory) {
            return Err(MezError::forbidden(
                "auth secret reference points outside the auth secret directory",
            ));
        }
        Ok(Some(path))
    }
}

impl CredentialStore for PrivateFileCredentialStore {
    /// Runs the kind operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn kind(&self) -> CredentialStoreKind {
        CredentialStoreKind::PrivateFileFallback
    }

    /// Runs the store secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn store_secret(&self, provider: &str, secret: &SecretString) -> Result<String> {
        if secret.expose_secret().is_empty() {
            return Err(MezError::invalid_args("auth secret must not be empty"));
        }

        ensure_private_dir(&self.directory)?;
        let path = self.secret_path(provider)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        file.write_all(secret.expose_secret().as_bytes())?;
        file.sync_all()?;
        set_private_file(&path)?;
        Ok(format!(
            "{}{}",
            CredentialStoreKind::PrivateFileFallback.reference_prefix(),
            path.display()
        ))
    }

    /// Runs the load secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn load_secret(&self, reference: &str) -> Result<Option<SecretString>> {
        let Some(path) = self.path_from_reference(reference)? else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        let mut secret = Zeroizing::new(String::new());
        fs::File::open(path)?.read_to_string(&mut secret)?;
        Ok(Some(SecretString::from(secret.as_str().to_string())))
    }

    /// Runs the delete secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn delete_secret(&self, reference: &str) -> Result<bool> {
        let Some(path) = self.path_from_reference(reference)? else {
            return Ok(false);
        };
        if path.exists() {
            fs::remove_file(path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
