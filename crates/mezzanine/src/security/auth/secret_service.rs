//! Native Secret Service credential store implementation.
//!
//! This backend uses the platform keyring API when available and handles existing
//! Secret Service credential references without leaking provider secrets.

use secrecy::{ExposeSecret, SecretString};

use crate::error::{MezError, Result};

use super::fs::validate_safe_name;
use super::types::{
    CredentialStore, CredentialStoreAvailability, CredentialStoreKind, OS_CREDENTIAL_SERVICE,
    SECRET_SERVICE_BACKEND,
};

/// Concrete OS credential store implemented through the platform Secret Service.
///
/// This store uses the `keyring-core` API and a native Secret Service backend
/// on platforms where that backend is available. The command-backed
/// `secret-tool` implementation remains as a fallback and for compatibility
/// with previously persisted references.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeSecretServiceCredentialStore;

impl NativeSecretServiceCredentialStore {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new() -> Self {
        Self
    }

    /// Runs the availability operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn availability(&self) -> CredentialStoreAvailability {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            run_secret_service_operation(|| {
                Ok(match zbus_secret_service_keyring_store::Store::new() {
                    Ok(_) => CredentialStoreAvailability::Available,
                    Err(error) => CredentialStoreAvailability::Unavailable {
                        reason: format!("native Secret Service unavailable: {error}"),
                    },
                })
            })
            .unwrap_or_else(|error| CredentialStoreAvailability::Unavailable {
                reason: format!("native Secret Service unavailable: {error}"),
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        {
            CredentialStoreAvailability::Unavailable {
                reason: "native Secret Service credential store is unavailable on this platform"
                    .to_string(),
            }
        }
    }

    /// Runs the plan service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn plan_service(&self) -> String {
        format!("{SECRET_SERVICE_BACKEND}/{OS_CREDENTIAL_SERVICE}")
    }

    /// Runs the reference for provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn reference_for_provider(&self, provider: &str) -> Result<String> {
        validate_safe_name(provider, "provider name is not credential-store safe")?;
        Ok(format!(
            "{}{}/{}/{}",
            CredentialStoreKind::OperatingSystem.reference_prefix(),
            SECRET_SERVICE_BACKEND,
            OS_CREDENTIAL_SERVICE,
            provider
        ))
    }

    /// Runs the provider from reference operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn provider_from_reference(&self, reference: &str) -> Result<Option<String>> {
        let Some(body) =
            reference.strip_prefix(CredentialStoreKind::OperatingSystem.reference_prefix())
        else {
            return Ok(None);
        };

        let mut parts = body.split('/');
        let backend = parts.next();
        let service = parts.next();
        let provider = parts.next();
        if backend != Some(SECRET_SERVICE_BACKEND) {
            return Ok(None);
        }
        if parts.next().is_some() || service != Some(OS_CREDENTIAL_SERVICE) {
            return Err(MezError::config(
                "malformed Secret Service credential reference",
            ));
        }

        let Some(provider) = provider else {
            return Err(MezError::config(
                "malformed Secret Service credential reference",
            ));
        };
        validate_safe_name(provider, "provider name is not credential-store safe")?;
        Ok(Some(provider.to_string()))
    }

    /// Runs the entry for provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn entry_for_provider(&self, provider: &str) -> Result<keyring_core::Entry> {
        use keyring_core::api::CredentialStoreApi;

        validate_safe_name(provider, "provider name is not credential-store safe")?;
        let store =
            zbus_secret_service_keyring_store::Store::new().map_err(secret_service_error)?;
        store
            .build(OS_CREDENTIAL_SERVICE, provider, None)
            .map_err(secret_service_error)
    }
}

impl CredentialStore for NativeSecretServiceCredentialStore {
    /// Runs the kind operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn kind(&self) -> CredentialStoreKind {
        CredentialStoreKind::OperatingSystem
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

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            let store = *self;
            let provider = provider.to_string();
            let secret = secret.expose_secret().to_string();
            run_secret_service_operation(move || {
                let entry = store.entry_for_provider(&provider)?;
                entry.set_password(&secret).map_err(secret_service_error)?;
                store.reference_for_provider(&provider)
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        {
            let _ = provider;
            let _ = secret;
            Err(MezError::invalid_state(
                "native Secret Service credential store is unavailable on this platform",
            ))
        }
    }

    /// Runs the load secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn load_secret(&self, reference: &str) -> Result<Option<SecretString>> {
        let Some(provider) = self.provider_from_reference(reference)? else {
            return Ok(None);
        };

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            let store = *self;
            run_secret_service_operation(move || {
                let entry = store.entry_for_provider(&provider)?;
                match entry.get_password() {
                    Ok(secret) if secret.is_empty() => Ok(None),
                    Ok(secret) => Ok(Some(SecretString::from(secret))),
                    Err(keyring_core::Error::NoEntry) => Ok(None),
                    Err(error) => Err(secret_service_error(error)),
                }
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        {
            let _ = provider;
            Err(MezError::invalid_state(
                "native Secret Service credential store is unavailable on this platform",
            ))
        }
    }

    /// Runs the delete secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn delete_secret(&self, reference: &str) -> Result<bool> {
        let Some(provider) = self.provider_from_reference(reference)? else {
            return Ok(false);
        };

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            let store = *self;
            run_secret_service_operation(move || {
                let entry = store.entry_for_provider(&provider)?;
                match entry.delete_credential() {
                    Ok(()) => Ok(true),
                    Err(keyring_core::Error::NoEntry) => Ok(false),
                    Err(error) => Err(secret_service_error(error)),
                }
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        {
            let _ = provider;
            Err(MezError::invalid_state(
                "native Secret Service credential store is unavailable on this platform",
            ))
        }
    }
}

/// Runs native Secret Service work away from any active Tokio runtime.
///
/// The upstream synchronous keyring adapter may internally create and drive a
/// Tokio runtime while talking to Secret Service over DBus. If Mezzanine calls
/// it from an async runtime worker, Tokio rejects the nested runtime; moving
/// the operation to a plain thread preserves the synchronous credential-store
/// trait without leaking async runtime details to callers.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn run_secret_service_operation<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_err() {
        return operation();
    }

    std::thread::spawn(operation).join().map_err(|_panic| {
        MezError::invalid_state("native Secret Service credential-store operation panicked")
    })?
}

/// Runs the secret service error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn secret_service_error(error: keyring_core::Error) -> MezError {
    match error {
        keyring_core::Error::NoEntry => MezError::new(
            crate::error::MezErrorKind::NotFound,
            "Secret Service credential was not found",
        ),
        other => MezError::invalid_state(format!("Secret Service credential store error: {other}")),
    }
}
