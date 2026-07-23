//! Unit tests for auth metadata, credential stores, and auth-store orchestration.

use super::{
    AuthCredentialKind, AuthCredentialState, AuthMetadata, AuthMethod, AuthPaths, AuthStore,
    CommandBackedCredentialStore, CredentialCommandOutput, CredentialCommandRunner,
    CredentialStore, CredentialStoreAvailability, CredentialStoreKind, CredentialStorePlan,
    FileCredentialFallbackReason, McpAuthMetadata, McpCredentialKind, McpOAuthCredential,
    OpenAiProviderCredential, PrivateFileCredentialStore, SECRET_TOOL_PROGRAM,
};
use crate::error::Result;
use secrecy::{ExposeSecret, SecretString};
use sha2::Digest;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::rc::Rc;

/// Carries Fake Credential Runner state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Clone, Default)]
struct FakeCredentialRunner {
    /// Stores the state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    state: Rc<RefCell<FakeCredentialRunnerState>>,
}

/// Carries Fake Credential Runner State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Default)]
struct FakeCredentialRunnerState {
    /// Stores the command available value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    command_available: bool,
    /// Stores the service available value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    service_available: bool,
    /// Stores the secrets value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    secrets: BTreeMap<String, String>,
    /// Stores the commands value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    commands: Vec<String>,
}

impl FakeCredentialRunner {
    /// Runs the available operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn available() -> Self {
        let runner = Self::default();
        {
            let mut state = runner.state.borrow_mut();
            state.command_available = true;
            state.service_available = true;
        }
        runner
    }

    /// Runs the command missing operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn command_missing() -> Self {
        let runner = Self::available();
        runner.state.borrow_mut().command_available = false;
        runner
    }

    /// Runs the service unavailable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn service_unavailable() -> Self {
        let runner = Self::available();
        runner.state.borrow_mut().service_available = false;
        runner
    }

    /// Runs the command count operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn command_count(&self, command: &str) -> usize {
        self.state
            .borrow()
            .commands
            .iter()
            .filter(|recorded| recorded.as_str() == command)
            .count()
    }
}

impl CredentialCommandRunner for FakeCredentialRunner {
    /// Runs the command available operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn command_available(&self, executable: &str) -> bool {
        executable == SECRET_TOOL_PROGRAM && self.state.borrow().command_available
    }

    /// Runs the run command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn run_command(
        &self,
        executable: &str,
        args: &[String],
        stdin: Option<&str>,
    ) -> Result<CredentialCommandOutput> {
        if executable != SECRET_TOOL_PROGRAM {
            return Ok(CredentialCommandOutput::failure());
        }
        let command = args.first().map(String::as_str).unwrap_or_default();
        self.state.borrow_mut().commands.push(command.to_string());

        if command == "search" {
            return if self.state.borrow().service_available {
                Ok(CredentialCommandOutput::success(Vec::new()))
            } else {
                Ok(CredentialCommandOutput::failure())
            };
        }
        if !self.state.borrow().service_available {
            return Ok(CredentialCommandOutput::failure());
        }

        let Some(provider) = attribute_value(args, "provider") else {
            return Ok(CredentialCommandOutput::failure());
        };
        match command {
            "store" => {
                self.state
                    .borrow_mut()
                    .secrets
                    .insert(provider.to_string(), stdin.unwrap_or_default().to_string());
                Ok(CredentialCommandOutput::success(Vec::new()))
            }
            "lookup" => {
                if let Some(secret) = self.state.borrow().secrets.get(provider) {
                    Ok(CredentialCommandOutput::success(format!("{secret}\n")))
                } else {
                    Ok(CredentialCommandOutput::success(Vec::new()))
                }
            }
            "clear" => {
                self.state.borrow_mut().secrets.remove(provider);
                Ok(CredentialCommandOutput::success(Vec::new()))
            }
            _ => Ok(CredentialCommandOutput::failure()),
        }
    }
}

/// Runs the attribute value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn attribute_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window[0] == name)
        .map(|window| window[1].as_str())
}

/// Runs the test secret operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_secret(value: &str) -> SecretString {
    SecretString::from(value)
}

/// Runs the exposed secret operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn exposed_secret(secret: SecretString) -> String {
    secret.expose_secret().to_string()
}

/// Runs the exposed optional secret operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn exposed_optional_secret(secret: Option<SecretString>) -> Option<String> {
    secret.map(exposed_secret)
}

/// Verifies auth file is dedicated under config root.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_file_is_dedicated_under_config_root() {
    let paths = AuthPaths::under_config_root(Path::new("/home/user/.config/mezzanine"));

    assert_eq!(
        paths.auth_file(),
        Path::new("/home/user/.config/mezzanine/auth.toml")
    );
    assert_eq!(
        paths.secret_directory(),
        Path::new("/home/user/.config/mezzanine/auth-secrets")
    );
}

/// Verifies metadata rejects obvious secret values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn metadata_rejects_obvious_secret_values() {
    let mut metadata = AuthMetadata::new("openai", "default");
    metadata.credential_store_ref = Some("Bearer secret".to_string());

    let error = metadata.validate_non_secret().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Config);
}

/// Verifies debug formatting for secret-bearing auth credentials redacts raw
/// token material before credentials are persisted to secret storage.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn credential_debug_formatting_redacts_secret_tokens() {
    let openai = OpenAiProviderCredential {
        api_key: "access-secret".to_string(),
        refresh_token: Some("refresh-secret".to_string()),
        account_id: Some("acct_123".to_string()),
        organization_id: Some("org_123".to_string()),
        token_expires_at: Some("12345".to_string()),
    };
    let mcp = McpOAuthCredential {
        access_token: "mcp-access-secret".to_string(),
        refresh_token: Some("mcp-refresh-secret".to_string()),
        token_expires_at: Some("12345".to_string()),
        scopes: vec!["read".to_string()],
        client_id: Some("public-client".to_string()),
        resource: Some("https://example.test".to_string()),
        authorization_endpoint: Some("https://example.test/auth".to_string()),
        token_endpoint: Some("https://example.test/token".to_string()),
    };

    let openai_debug = format!("{openai:?}");
    let mcp_debug = format!("{mcp:?}");

    assert!(!openai_debug.contains("access-secret"));
    assert!(!openai_debug.contains("refresh-secret"));
    assert!(openai_debug.contains("acct_123"));
    assert!(!mcp_debug.contains("mcp-access-secret"));
    assert!(!mcp_debug.contains("mcp-refresh-secret"));
    assert!(mcp_debug.contains("public-client"));
}

/// Verifies that new auth files record the non-secret credential kind so
/// runtime provider setup can distinguish direct API keys from ChatGPT OAuth
/// credentials after daemon restart.
#[test]
fn metadata_persists_and_decodes_credential_kind() {
    let mut metadata = AuthMetadata::new("openai", "default");
    metadata.credential_kind = AuthCredentialKind::ChatGpt;
    metadata.account_id = Some("acct_123".to_string());

    let encoded = super::metadata::encode_metadata(&metadata);
    let decoded = super::metadata::decode_metadata(&encoded).unwrap();

    assert_eq!(decoded.credential_kind, AuthCredentialKind::ChatGpt);
    assert_eq!(decoded.account_id.as_deref(), Some("acct_123"));
    assert!(encoded.contains("credential_kind = \"chatgpt\""));
}

/// Verifies that auth files written before the credential-kind field existed
/// are inferred as ChatGPT credentials when they carry browser/device metadata.
#[test]
fn metadata_decodes_legacy_provider_auth_as_chatgpt() {
    let legacy = "provider = \"openai\"\naccount_id = \"acct_123\"\norganization_id = \"\"\nselected_model_profile = \"default\"\ncredential_store_ref = \"file:/tmp/access\"\nrefresh_credential_store_ref = \"file:/tmp/refresh\"\ntoken_expires_at = \"12345\"\n";

    let decoded = super::metadata::decode_metadata(legacy).unwrap();

    assert_eq!(decoded.credential_kind, AuthCredentialKind::ChatGpt);
}

/// Verifies private file credential store stores loads and deletes secret.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn private_file_credential_store_stores_loads_and_deletes_secret() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-credential-store-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = PrivateFileCredentialStore::new(root.join("secrets"));
    let credential_store: &dyn CredentialStore = &store;

    let reference = credential_store
        .store_secret("openai", &test_secret("sk-test-secret"))
        .unwrap();
    let secret_path = reference.strip_prefix("file:").unwrap();

    assert_eq!(
        credential_store.kind(),
        CredentialStoreKind::PrivateFileFallback
    );
    assert_eq!(
        exposed_optional_secret(credential_store.load_secret(&reference).unwrap()),
        Some("sk-test-secret".to_string())
    );
    assert!(credential_store.contains_secret(&reference).unwrap());
    assert_eq!(
        fs::metadata(secret_path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    assert!(credential_store.delete_secret(&reference).unwrap());
    assert!(credential_store.load_secret(&reference).unwrap().is_none());
    assert!(!credential_store.delete_secret(&reference).unwrap());

    let _ = fs::remove_dir_all(root);
}

/// Verifies private file credential store rejects symlinked roots and files.
///
/// Private fallback credentials must stay inside the configured auth-secret
/// directory. Symlinked directories or pre-existing symlink secret files could
/// otherwise redirect writes or reads outside that private directory.
#[cfg(unix)]
#[test]
fn private_file_credential_store_rejects_symlink_secret_paths() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-credential-symlink-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let secrets = root.join("secrets");
    let outside = root.join("outside");
    fs::create_dir_all(&secrets).unwrap();
    fs::create_dir_all(&outside).unwrap();

    let symlinked_root_store = PrivateFileCredentialStore::new(root.join("linked-secrets"));
    std::os::unix::fs::symlink(&outside, symlinked_root_store.directory()).unwrap();
    let root_error = (&symlinked_root_store as &dyn CredentialStore)
        .store_secret("openai", &test_secret("sk-test-secret"))
        .unwrap_err();
    assert_eq!(root_error.kind(), crate::error::MezErrorKind::Forbidden);

    let leak = outside.join("leak.secret");
    fs::write(&leak, "outside").unwrap();
    std::os::unix::fs::symlink(&leak, secrets.join("openai.secret")).unwrap();
    let store = PrivateFileCredentialStore::new(secrets);
    let credential_store: &dyn CredentialStore = &store;

    let file_error = credential_store
        .store_secret("openai", &test_secret("sk-new-secret"))
        .unwrap_err();

    assert_eq!(file_error.kind(), crate::error::MezErrorKind::Forbidden);
    assert_eq!(fs::read_to_string(leak).unwrap(), "outside");

    let _ = fs::remove_dir_all(root);
}

/// Verifies command backed credential store uses runner without real keychain.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_backed_credential_store_uses_runner_without_real_keychain() {
    let runner = FakeCredentialRunner::available();
    let store = CommandBackedCredentialStore::secret_tool_with_runner(runner.clone());
    let credential_store: &dyn CredentialStore = &store;

    let reference = credential_store
        .store_secret("openai", &test_secret("sk-test-secret"))
        .unwrap();

    assert_eq!(
        reference,
        "os-keyring:secret-tool/mezzanine/openai".to_string()
    );
    assert_eq!(
        credential_store.kind(),
        CredentialStoreKind::OperatingSystem
    );
    assert_eq!(
        exposed_optional_secret(credential_store.load_secret(&reference).unwrap()),
        Some("sk-test-secret".to_string())
    );
    assert!(credential_store.contains_secret(&reference).unwrap());
    assert!(credential_store.delete_secret(&reference).unwrap());
    assert!(credential_store.load_secret(&reference).unwrap().is_none());
    assert!(!credential_store.delete_secret(&reference).unwrap());
    assert_eq!(runner.command_count("store"), 1);
    assert_eq!(runner.command_count("clear"), 1);
}

/// Verifies command backed credential store fails closed when unavailable.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_backed_credential_store_fails_closed_when_unavailable() {
    let missing_command = CommandBackedCredentialStore::secret_tool_with_runner(
        FakeCredentialRunner::command_missing(),
    );
    let unreachable_service = CommandBackedCredentialStore::secret_tool_with_runner(
        FakeCredentialRunner::service_unavailable(),
    );

    assert!(matches!(
        missing_command.availability().unwrap(),
        CredentialStoreAvailability::Unavailable { .. }
    ));
    assert!(matches!(
        unreachable_service.availability().unwrap(),
        CredentialStoreAvailability::Unavailable { .. }
    ));

    let reference = "os-keyring:secret-tool/mezzanine/openai";
    let error = missing_command
        .store_secret("openai", &test_secret("sk-test-secret"))
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    let error = unreachable_service.load_secret(reference).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
}

/// Verifies auth store plans os store only when command backend is available.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_store_plans_os_store_only_when_command_backend_is_available() {
    let root = Path::new("/home/user/.config/mezzanine");
    let store = AuthStore::new(AuthPaths::under_config_root(root));
    let available_os =
        CommandBackedCredentialStore::secret_tool_with_runner(FakeCredentialRunner::available());
    let missing_os = CommandBackedCredentialStore::secret_tool_with_runner(
        FakeCredentialRunner::command_missing(),
    );

    assert_eq!(
        store.credential_store_plan_with_os_store("openai", &available_os),
        CredentialStorePlan::OperatingSystem {
            service: "secret-tool/mezzanine".to_string(),
            account: "openai".to_string(),
        }
    );
    assert_eq!(
        store.credential_store_plan_with_os_store("openai", &missing_os),
        CredentialStorePlan::PrivateFileFallback {
            directory: root.join("auth-secrets/openai"),
            reason: FileCredentialFallbackReason::OperatingSystemStoreUnavailable,
        }
    );
}

/// Verifies auth status checks os store before authenticating.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_status_checks_os_store_before_authenticating() {
    let root = std::env::temp_dir().join(format!("mez-auth-os-status-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(AuthPaths::under_config_root(&root));
    let os_runner = FakeCredentialRunner::available();
    let os_store = CommandBackedCredentialStore::secret_tool_with_runner(os_runner);
    let reference = os_store
        .store_secret("openai", &test_secret("sk-test-secret"))
        .unwrap();
    let mut metadata = AuthMetadata::new("openai", "default");
    metadata.credential_store_ref = Some(reference.clone());

    assert_eq!(
        auth_store
            .credential_state_with_os_store(Some(&metadata), &os_store)
            .unwrap(),
        AuthCredentialState::Available {
            store: CredentialStoreKind::OperatingSystem,
            reference: reference.clone(),
        }
    );

    let empty_os =
        CommandBackedCredentialStore::secret_tool_with_runner(FakeCredentialRunner::available());
    assert_eq!(
        auth_store
            .credential_state_with_os_store(Some(&metadata), &empty_os)
            .unwrap(),
        AuthCredentialState::MissingSecret {
            reference: Some(reference.clone()),
        }
    );

    let unavailable_os = CommandBackedCredentialStore::secret_tool_with_runner(
        FakeCredentialRunner::service_unavailable(),
    );
    assert_eq!(
        auth_store
            .credential_state_with_os_store(Some(&metadata), &unavailable_os)
            .unwrap(),
        AuthCredentialState::MissingSecret {
            reference: Some(reference),
        }
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies logout removes os secret through command backend.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn logout_removes_os_secret_through_command_backend() {
    let root = std::env::temp_dir().join(format!("mez-auth-os-logout-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(AuthPaths::under_config_root(&root));
    let runner = FakeCredentialRunner::available();
    let os_store = CommandBackedCredentialStore::secret_tool_with_runner(runner.clone());
    let reference = os_store
        .store_secret("openai", &test_secret("sk-test-secret"))
        .unwrap();

    assert!(
        auth_store
            .remove_secret_with_os_store(&reference, &os_store)
            .unwrap()
    );
    assert!(os_store.load_secret(&reference).unwrap().is_none());
    assert_eq!(runner.command_count("clear"), 1);

    let _ = fs::remove_dir_all(root);
}

/// Verifies auth metadata round trips to private file.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_metadata_round_trips_to_private_file() {
    let root = std::env::temp_dir().join(format!("mez-auth-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let mut metadata = AuthMetadata::new("openai", "default");
    metadata.account_id = Some("acct_1".to_string());
    metadata.credential_store_ref = Some("os-keyring:secret-tool/mezzanine/openai".to_string());
    metadata.refresh_credential_store_ref =
        Some("os-keyring:secret-tool/mezzanine/openai-refresh".to_string());

    let path = store.write_metadata(&metadata).unwrap();
    let loaded = store.read_metadata().unwrap().unwrap();

    assert_eq!(loaded, metadata);
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies file secret backend is private and not written to metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn file_secret_backend_is_private_and_not_written_to_metadata() {
    let root = std::env::temp_dir().join(format!("mez-auth-secret-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));

    let reference = store.write_file_secret("openai", "sk-test-secret").unwrap();
    let secret_path = reference.strip_prefix("file:").unwrap();
    let mut metadata = AuthMetadata::new("openai", "default");
    metadata.credential_store_ref = Some(reference.clone());
    store.write_metadata(&metadata).unwrap();

    let auth_data = fs::read_to_string(store.paths().auth_file()).unwrap();
    assert!(!auth_data.contains("sk-test-secret"));
    assert_eq!(
        exposed_optional_secret(store.read_file_secret(&reference).unwrap()),
        Some("sk-test-secret".to_string())
    );
    assert!(store.status().unwrap().authenticated);
    assert_eq!(
        store.status().unwrap().credential_state,
        AuthCredentialState::Available {
            store: CredentialStoreKind::PrivateFileFallback,
            reference: reference.clone(),
        }
    );
    assert_eq!(
        fs::metadata(store.paths().secret_directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(secret_path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    assert!(store.logout().unwrap());
    assert!(!Path::new(secret_path).exists());
    assert!(!store.paths().auth_file().exists());

    let _ = fs::remove_dir_all(root);
}

/// Verifies auth status classifies logout and missing secret.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_status_classifies_logout_and_missing_secret() {
    let root = std::env::temp_dir().join(format!("mez-auth-status-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));

    let logged_out = store.status().unwrap();
    assert!(!logged_out.authenticated);
    assert_eq!(logged_out.credential_state, AuthCredentialState::LoggedOut);

    let metadata = AuthMetadata::new("openai", "default");
    store.write_metadata(&metadata).unwrap();
    let missing_reference = store.status().unwrap();
    assert!(!missing_reference.authenticated);
    assert_eq!(
        missing_reference.credential_state,
        AuthCredentialState::MissingSecret { reference: None }
    );

    let missing_path = store
        .paths()
        .secret_directory()
        .join("openai")
        .join("missing.secret");
    let mut metadata = AuthMetadata::new("openai", "default");
    metadata.credential_store_ref = Some(format!("file:{}", missing_path.display()));
    store.write_metadata(&metadata).unwrap();
    let missing_secret = store.status().unwrap();
    assert!(!missing_secret.authenticated);
    assert_eq!(
        missing_secret.credential_state,
        AuthCredentialState::MissingSecret {
            reference: metadata.credential_store_ref.clone(),
        }
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies file secret references must stay under auth secret directory.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn file_secret_references_must_stay_under_auth_secret_directory() {
    let root =
        std::env::temp_dir().join(format!("mez-auth-secret-path-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));

    let error = store
        .read_file_secret("file:/tmp/not-mezzanine")
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    let traversal = format!(
        "file:{}",
        store
            .paths()
            .secret_directory()
            .join("openai")
            .join("..")
            .join("other.secret")
            .display()
    );
    let error = store.read_file_secret(&traversal).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    let _ = fs::remove_dir_all(root);
}

/// Verifies missing file secret marks status unauthenticated.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn missing_file_secret_marks_status_unauthenticated() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-secret-missing-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let reference = store.write_file_secret("openai", "secret-value").unwrap();
    let secret_path = reference.strip_prefix("file:").unwrap().to_string();
    let mut metadata = AuthMetadata::new("openai", "default");
    metadata.credential_store_ref = Some(reference);
    store.write_metadata(&metadata).unwrap();
    fs::remove_file(secret_path).unwrap();

    assert!(!store.status().unwrap().authenticated);
    assert_eq!(
        store.status().unwrap().credential_state,
        AuthCredentialState::MissingSecret {
            reference: metadata.credential_store_ref.clone(),
        }
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies api key login stores secret and persists only metadata reference.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn api_key_login_stores_secret_and_persists_only_metadata_reference() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-api-key-login-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let credential_store = store.file_credential_store("openai").unwrap();

    let metadata = store
        .login_openai_api_key("default", "sk-test-secret", &credential_store)
        .unwrap();
    let status = store.status().unwrap();
    let auth_file = fs::read_to_string(store.paths().auth_file()).unwrap();

    assert_eq!(metadata.provider, "openai");
    assert_eq!(metadata.credential_kind, AuthCredentialKind::ApiKey);
    assert_eq!(metadata.selected_model_profile, "default");
    assert!(
        metadata
            .credential_store_ref
            .as_deref()
            .unwrap()
            .starts_with("file:")
    );
    assert!(status.authenticated);
    assert!(!auth_file.contains("sk-test-secret"));
    assert_eq!(
        exposed_optional_secret(
            store
                .read_file_secret(metadata.credential_store_ref.as_deref().unwrap())
                .unwrap()
        ),
        Some("sk-test-secret".to_string())
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies static MCP bearer login stores the token as a secret while MCP
/// auth metadata remains secret-safe and refresh-ineligible.
///
/// Static bearer credentials are user-supplied long-lived bearer tokens, not
/// OAuth access tokens. This regression ensures the auth store persists the raw
/// token outside metadata, reports a usable stored credential, and does not
/// expose an OAuth refresh token path for runtime retry handling.
#[test]
fn mcp_static_bearer_login_stores_secret_and_skips_refresh() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-mcp-static-bearer-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let credential_store = store.file_credential_store("demo").unwrap();
    let metadata = McpAuthMetadata::new("demo", "https://example.invalid", "sha256:static-bearer");

    let metadata = store
        .login_mcp_static_bearer_credential(
            metadata,
            "static-token-secret".to_string(),
            &credential_store,
        )
        .unwrap();
    let status = store
        .mcp_status(
            "demo",
            Some("https://example.invalid"),
            Some("sha256:static-bearer"),
        )
        .unwrap();
    let auth_file = fs::read_to_string(store.paths().mcp_auth_file()).unwrap();

    assert_eq!(metadata.credential_kind, McpCredentialKind::StaticBearer);
    assert!(metadata.refresh_credential_store_ref.is_none());
    assert!(metadata.scopes.is_empty());
    assert!(status.authenticated);
    assert_eq!(
        status.metadata.as_ref().unwrap().credential_kind,
        McpCredentialKind::StaticBearer
    );
    assert!(store.mcp_refresh_token("demo").unwrap().is_none());
    assert_eq!(
        store.mcp_access_token("demo").unwrap().expose_secret(),
        "static-token-secret"
    );
    assert!(!auth_file.contains("static-token-secret"));

    let _ = fs::remove_dir_all(root);
}

/// Verifies optional MCP token loading distinguishes absent auth from failure.
///
/// Streamable HTTP servers may intentionally operate without authentication
/// even when the runtime has a global auth store. Missing per-server metadata
/// must therefore return `None`, while configured metadata whose secret cannot
/// be loaded must fail closed instead of allowing an unauthenticated request.
#[test]
fn mcp_optional_access_token_propagates_configured_secret_load_failures() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-mcp-optional-access-token-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));

    assert!(
        store
            .mcp_access_token_if_configured("unauthenticated")
            .unwrap()
            .is_none()
    );

    let secret_path = store
        .paths()
        .secret_directory()
        .join("broken")
        .join("access.secret");
    fs::create_dir_all(&secret_path).unwrap();
    let mut metadata = McpAuthMetadata::new("broken", "https://example.invalid", "sha256:broken");
    metadata.credential_store_ref = Some(format!("file:{}", secret_path.display()));
    store.write_mcp_metadata(&metadata).unwrap();

    let error = store.mcp_access_token_if_configured("broken").unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.to_string().contains("regular file"), "{error}");

    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime MCP token loading enforces the configured URL binding.
///
/// Reusing a server id for a different origin or path must not disclose the
/// stored bearer credential to the replacement endpoint. An unchanged URL
/// remains usable, while either binding mismatch requires re-authentication.
#[test]
fn mcp_access_token_for_url_rejects_stale_origin_and_fingerprint() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-mcp-url-binding-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let credential_store = store.file_credential_store("demo").unwrap();
    let configured_url = "https://example.invalid/v1/mcp";
    let digest = sha2::Sha256::digest(configured_url.as_bytes());
    let fingerprint = format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let metadata = McpAuthMetadata::new("demo", "https://example.invalid", fingerprint);
    store
        .login_mcp_static_bearer_credential(
            metadata,
            "bound-token-secret".to_string(),
            &credential_store,
        )
        .unwrap();

    assert_eq!(
        store
            .mcp_access_token_for_url_if_configured("demo", configured_url)
            .unwrap()
            .unwrap()
            .expose_secret(),
        "bound-token-secret"
    );
    for stale_url in [
        "https://replacement.invalid/v1/mcp",
        "https://example.invalid/v2/mcp",
    ] {
        let error = store
            .mcp_access_token_for_url_if_configured("demo", stale_url)
            .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
        assert!(error.to_string().contains("re-authentication"), "{error}");
    }

    let _ = fs::remove_dir_all(root);
}

/// Verifies that provider-account auth stores access and refresh material as
/// separate credential-store entries while keeping the auth metadata file
/// non-secret. Browser and device-code login rely on this path to survive
/// process restarts without exposing bearer or refresh tokens.
#[test]
fn provider_login_persists_access_and_refresh_secrets_as_references() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-provider-login-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let credential_store = store.file_credential_store("openai").unwrap();

    let metadata = store
        .login_openai_provider_credential(
            "default",
            OpenAiProviderCredential {
                api_key: "access-secret".to_string(),
                refresh_token: Some("refresh-secret".to_string()),
                account_id: Some("acct_123".to_string()),
                organization_id: Some("org_123".to_string()),
                token_expires_at: Some("12345".to_string()),
            },
            &credential_store,
        )
        .unwrap();
    let auth_file = fs::read_to_string(store.paths().auth_file()).unwrap();

    assert_eq!(metadata.account_id.as_deref(), Some("acct_123"));
    assert_eq!(metadata.credential_kind, AuthCredentialKind::ChatGpt);
    assert_eq!(metadata.organization_id.as_deref(), Some("org_123"));
    assert_eq!(metadata.token_expires_at.as_deref(), Some("12345"));
    assert_ne!(
        metadata.credential_store_ref,
        metadata.refresh_credential_store_ref
    );
    assert!(!auth_file.contains("access-secret"));
    assert!(!auth_file.contains("refresh-secret"));
    assert_eq!(
        store.provider_secret("openai").unwrap().expose_secret(),
        "access-secret"
    );
    assert_eq!(
        exposed_optional_secret(
            store
                .read_file_secret(metadata.refresh_credential_store_ref.as_deref().unwrap())
                .unwrap()
        ),
        Some("refresh-secret".to_string())
    );
    assert!(store.logout().unwrap());
    assert!(!store.paths().auth_file().exists());

    let _ = fs::remove_dir_all(root);
}

/// Verifies the refresh scheduling predicate used at daemon startup. Refresh
/// attempts should only be started for OpenAI metadata that has a refresh-token
/// reference and whose access-token expiry is already inside the configured
/// leeway window.
#[test]
fn openai_refresh_needed_only_when_expiry_is_inside_leeway_and_refresh_exists() {
    let mut metadata = AuthMetadata::new("openai", "default");
    metadata.refresh_credential_store_ref = Some("file:/tmp/refresh".to_string());
    metadata.token_expires_at = Some("110".to_string());

    assert!(super::store::openai_refresh_needed_at(&metadata, 100, 10));
    assert!(!super::store::openai_refresh_needed_at(&metadata, 100, 9));

    metadata.refresh_credential_store_ref = None;
    assert!(!super::store::openai_refresh_needed_at(&metadata, 100, 10));

    metadata.refresh_credential_store_ref = Some("file:/tmp/refresh".to_string());
    metadata.provider = "other".to_string();
    assert!(!super::store::openai_refresh_needed_at(&metadata, 100, 10));
}

/// Verifies provider secret loads referenced secret without metadata leakage.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn provider_secret_loads_referenced_secret_without_metadata_leakage() {
    let root =
        std::env::temp_dir().join(format!("mez-auth-provider-secret-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let credential_store = store.file_credential_store("openai").unwrap();
    store
        .login_openai_api_key("default", "sk-test-secret", &credential_store)
        .unwrap();

    assert_eq!(
        store.provider_secret("openai").unwrap().expose_secret(),
        "sk-test-secret"
    );
    let metadata = fs::read_to_string(store.paths().auth_file()).unwrap();
    assert!(!metadata.contains("sk-test-secret"));

    let error = store.provider_secret("other").unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);

    let _ = fs::remove_dir_all(root);
}

/// Verifies provider secret lookup selects metadata by provider name rather
/// than by metadata-file iteration order.
///
/// Multi-provider auth files are sorted by provider id when persisted, so a
/// file containing both DeepSeek and OpenAI metadata lists `deepseek` first.
/// This regression protects concurrent provider credentials by proving OpenAI
/// lookup still uses the OpenAI metadata entry and DeepSeek lookup still uses
/// the DeepSeek entry.
#[test]
fn provider_secret_uses_requested_provider_metadata() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-provider-secret-multi-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let openai_store = store.file_credential_store("openai").unwrap();
    let deepseek_store = store.file_credential_store("deepseek").unwrap();
    store
        .login_openai_api_key("default", "sk-openai-secret", &openai_store)
        .unwrap();
    store
        .login_provider_api_key(
            "deepseek",
            "deepseek-default",
            "sk-deepseek-secret",
            &deepseek_store,
        )
        .unwrap();

    assert_eq!(
        store.provider_secret("openai").unwrap().expose_secret(),
        "sk-openai-secret"
    );
    assert_eq!(
        store.provider_secret("deepseek").unwrap().expose_secret(),
        "sk-deepseek-secret"
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies OpenAI refresh scheduling reads OpenAI metadata even when another
/// provider sorts first in the multi-provider auth file.
///
/// The refresh probe runs during daemon startup and should not be disabled just
/// because a DeepSeek credential is present in the same metadata file.
#[test]
fn openai_refresh_needed_soon_uses_openai_metadata_entry() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-openai-refresh-multi-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let deepseek_store = store.file_credential_store("deepseek").unwrap();
    store
        .login_provider_api_key(
            "deepseek",
            "deepseek-default",
            "sk-deepseek-secret",
            &deepseek_store,
        )
        .unwrap();
    let mut openai = AuthMetadata::new("openai", "default");
    openai.credential_kind = AuthCredentialKind::ChatGpt;
    openai.credential_store_ref = Some("file:/tmp/openai-access".to_string());
    openai.refresh_credential_store_ref = Some("file:/tmp/openai-refresh".to_string());
    openai.token_expires_at = Some("1".to_string());
    store.write_metadata(&openai).unwrap();

    assert!(store.openai_refresh_needed_soon().unwrap());

    let _ = fs::remove_dir_all(root);
}

/// Verifies auth status reports an available provider credential even when an
/// earlier provider entry has a missing secret.
///
/// Provider metadata is sorted by provider id, which means DeepSeek is checked
/// before OpenAI. This regression keeps a broken DeepSeek secret from hiding a
/// valid OpenAI credential in multi-provider auth status.
#[test]
fn auth_status_uses_first_available_provider_secret() {
    let root = std::env::temp_dir().join(format!("mez-auth-status-multi-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let openai_store = store.file_credential_store("openai").unwrap();
    store
        .login_openai_api_key("default", "sk-openai-secret", &openai_store)
        .unwrap();
    let missing_deepseek_secret = store
        .paths()
        .secret_directory()
        .join("deepseek")
        .join("missing.secret");
    let mut deepseek = AuthMetadata::new("deepseek", "deepseek-default");
    deepseek.credential_store_ref = Some(format!("file:{}", missing_deepseek_secret.display()));
    store.write_metadata(&deepseek).unwrap();

    let status = store.status().unwrap();

    assert!(status.authenticated);
    assert_eq!(
        status
            .metadata
            .as_ref()
            .map(|metadata| metadata.provider.as_str()),
        Some("openai")
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies global logout removes secrets for every provider in the metadata
/// file before deleting the metadata document.
///
/// A multi-provider auth file may contain separate private-file secrets. This
/// regression prevents logout from deleting only the first provider's secret
/// and orphaning the remaining provider credential.
#[test]
fn logout_removes_all_provider_secrets_from_multi_provider_metadata() {
    let root = std::env::temp_dir().join(format!("mez-auth-logout-multi-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let openai_store = store.file_credential_store("openai").unwrap();
    let deepseek_store = store.file_credential_store("deepseek").unwrap();
    let openai = store
        .login_openai_api_key("default", "sk-openai-secret", &openai_store)
        .unwrap();
    let deepseek = store
        .login_provider_api_key(
            "deepseek",
            "deepseek-default",
            "sk-deepseek-secret",
            &deepseek_store,
        )
        .unwrap();
    let openai_secret = openai
        .credential_store_ref
        .as_deref()
        .unwrap()
        .strip_prefix("file:")
        .unwrap()
        .to_string();
    let deepseek_secret = deepseek
        .credential_store_ref
        .as_deref()
        .unwrap()
        .strip_prefix("file:")
        .unwrap()
        .to_string();

    assert!(store.logout().unwrap());
    assert!(!Path::new(&openai_secret).exists());
    assert!(!Path::new(&deepseek_secret).exists());
    assert!(!store.paths().auth_file().exists());

    let _ = fs::remove_dir_all(root);
}

/// Verifies api key login rejects empty secret before writing metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn api_key_login_rejects_empty_secret_before_writing_metadata() {
    let root = std::env::temp_dir().join(format!(
        "mez-auth-api-key-empty-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root));
    let credential_store = store.file_credential_store("openai").unwrap();

    let error = store
        .login_openai_api_key("default", "   ", &credential_store)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(!store.paths().auth_file().exists());

    let _ = fs::remove_dir_all(root);
}

/// Verifies openai auth flow plan identifies method without claiming entitlement.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn openai_auth_flow_plan_identifies_method_without_claiming_entitlement() {
    let root = Path::new("/home/user/.config/mezzanine");
    let store = AuthStore::new(AuthPaths::under_config_root(root));
    let missing_os = CommandBackedCredentialStore::secret_tool_with_runner(
        FakeCredentialRunner::command_missing(),
    );

    let plan = store.plan_openai_flow_with_os_store(AuthMethod::ApiKey, &missing_os);

    assert_eq!(plan.provider, "openai");
    assert_eq!(plan.method, AuthMethod::ApiKey);
    assert_eq!(
        plan.credential_store,
        CredentialStorePlan::PrivateFileFallback {
            directory: root.join("auth-secrets/openai"),
            reason: FileCredentialFallbackReason::OperatingSystemStoreUnavailable,
        }
    );
    assert_eq!(
        plan.credential_store.selected_store(),
        CredentialStoreKind::PrivateFileFallback
    );
    assert!(plan.user_instruction.contains("configuration shell"));
}
