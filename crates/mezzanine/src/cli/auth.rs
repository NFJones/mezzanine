//! Cli Auth implementation.
//!
//! This module owns the cli auth boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    Args, AuthMethod, AuthPaths, AuthStore, CliEnv, CliOutputFormat, ConfigPaths, MezError,
    OpenAiProviderCredential, PathBuf, Result, Serialize, Subcommand, Write, fs, json_escape,
    load_runtime_config_layers, run_openai_browser_login_with_theme_async,
    run_openai_device_code_login_async, runtime_effective_config_value,
    runtime_ui_theme_from_config, serialize_json, write_json_or_plain,
};
use crate::security::auth::selected_auth_method_from_flags;
use mez_mux::theme::UiTheme;

/// Local-only namespaced search credential commands.
mod search;

// Authentication subcommands and output formatting.

/// Identifies only a grammar-selected search command for secret-safe error
/// reporting. Partial matches are never returned for execution/authorization.
/// Parsing can stop before selecting the command on malformed global options;
/// this deliberately makes no claim to sanitize those unclassified invocations.
pub(super) fn is_search_parse_target(mut command: clap::Command, args: &[String]) -> bool {
    /// Applies tolerant parsing solely to inspect a selected subcommand path.
    fn tolerate(command: &mut clap::Command) {
        *command = command.clone().ignore_errors(true);
        for child in command.get_subcommands_mut() {
            tolerate(child);
        }
    }
    tolerate(&mut command);
    command
        .try_get_matches_from(args)
        .ok()
        .and_then(|matches| {
            matches
                .subcommand_matches("auth")
                .and_then(|auth| auth.subcommand_matches("search"))
                .map(|_| ())
        })
        .is_some()
}

/// Structured JSON payload emitted when `mez auth logout` completes.
#[derive(Serialize)]
struct AuthLogoutJson {
    /// Whether a stored authentication session was removed.
    logged_out: bool,
}

/// Runs the run auth operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_auth<W: Write>(
    parsed: AuthCliArgs,
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let paths = env.config_paths()?;
    let store = AuthStore::new(AuthPaths::under_config_root(paths.root()));

    match parsed.command.unwrap_or(AuthCliCommand::Status) {
        AuthCliCommand::Search(args) => {
            search::run(
                args,
                store.search_auth(),
                interactive,
                output_format,
                stdout,
            )
            .await?;
        }
        AuthCliCommand::Status => {
            let status = run_auth_store_operation({
                let store = store.clone();
                move || store.status()
            })
            .await?;
            let output = auth_status_json(status.authenticated, &status.metadata)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        AuthCliCommand::Login(login) => {
            let method = login.method()?;
            let provider = login.provider.clone();
            let selected_profile = login.profile.clone();
            let credential_store = login.credential_store.clone();
            let auth_profile = login
                .auth_profile
                .clone()
                .unwrap_or_else(|| "openai".to_string());
            if login.auth_profile.is_some()
                && (provider != "openai" || method == AuthMethod::ApiKey)
            {
                return Err(MezError::invalid_args(
                    "--auth-profile is supported only for OpenAI ChatGPT browser/device-code login",
                ));
            }
            match method {
                AuthMethod::ApiKey => {
                    if let Some(path) = login.api_key_file.as_ref() {
                        let secret = fs::read_to_string(path)?;
                        let secret = secret.trim().to_string();
                        let metadata = run_auth_store_operation({
                            let store = store.clone();
                            let selected_profile = selected_profile.to_string();
                            let credential_store = credential_store.clone();
                            let provider = provider.clone();
                            move || {
                                login_provider_api_key_for_cli(
                                    &store,
                                    &provider,
                                    &selected_profile,
                                    &secret,
                                    credential_store.as_deref(),
                                )
                            }
                        })
                        .await?;
                        persist_authenticated_provider_defaults(&paths, &metadata)?;
                        write_auth_login(stdout, output_format, &metadata)?;
                        return Ok(());
                    }
                    if interactive {
                        let prompt = format!("{provider} API key: ");
                        let secret = rpassword::prompt_password(prompt).map_err(|error| {
                            MezError::new(
                                crate::error::MezErrorKind::Io,
                                format!("failed to read API key: {error}"),
                            )
                        })?;
                        let secret = secret.trim().to_string();
                        let metadata = run_auth_store_operation({
                            let store = store.clone();
                            let selected_profile = selected_profile.to_string();
                            let credential_store = credential_store.clone();
                            let provider = provider.clone();
                            move || {
                                login_provider_api_key_for_cli(
                                    &store,
                                    &provider,
                                    &selected_profile,
                                    &secret,
                                    credential_store.as_deref(),
                                )
                            }
                        })
                        .await?;
                        persist_authenticated_provider_defaults(&paths, &metadata)?;
                        write_auth_login(stdout, output_format, &metadata)?;
                        return Ok(());
                    }
                    return Err(if provider == "openai" {
                        noninteractive_api_key_login_error()
                    } else {
                        MezError::invalid_args(format!(
                            "auth login for `{provider}` requires noninteractive API-key input; \
                             pass --api-key-file PATH or run `mez auth login --provider {provider} --api-key` \
                             from an interactive terminal"
                        ))
                    });
                }
                AuthMethod::Browser => {
                    if provider != "openai" {
                        return Err(MezError::invalid_args(
                            "browser-based login is only supported for OpenAI in Mez auth; use `mez auth login --provider anthropic --api-key` for Anthropic Console API-key auth",
                        ));
                    }
                    if !interactive {
                        return Err(noninteractive_browser_login_error());
                    }
                    let ui_theme = auth_login_ui_theme(&paths);
                    let credential = run_openai_browser_login_with_theme_async(&ui_theme).await?;
                    let metadata = run_auth_store_operation({
                        let store = store.clone();
                        let selected_profile = selected_profile.to_string();
                        let credential_store = credential_store.clone();
                        let auth_profile = auth_profile.clone();
                        move || {
                            login_openai_provider_credential_for_cli(
                                &store,
                                &auth_profile,
                                &selected_profile,
                                credential_store.as_deref(),
                                credential,
                            )
                        }
                    })
                    .await?;
                    if auth_profile == "openai" {
                        persist_authenticated_provider_defaults(&paths, &metadata)?;
                    }
                    write_auth_login(stdout, output_format, &metadata)?;
                }
                AuthMethod::DeviceCode => {
                    if provider != "openai" {
                        return Err(MezError::invalid_args(
                            "device-code login is only supported for OpenAI in Mez auth; use `mez auth login --provider anthropic --api-key` for Anthropic Console API-key auth",
                        ));
                    }
                    let credential = run_openai_device_code_login_async().await?;
                    let metadata = run_auth_store_operation({
                        let store = store.clone();
                        let selected_profile = selected_profile.to_string();
                        let credential_store = credential_store.clone();
                        let auth_profile = auth_profile.clone();
                        move || {
                            login_openai_provider_credential_for_cli(
                                &store,
                                &auth_profile,
                                &selected_profile,
                                credential_store.as_deref(),
                                credential,
                            )
                        }
                    })
                    .await?;
                    if auth_profile == "openai" {
                        persist_authenticated_provider_defaults(&paths, &metadata)?;
                    }
                    write_auth_login(stdout, output_format, &metadata)?;
                }
            }
        }
        AuthCliCommand::Logout => {
            let changed = run_auth_store_operation({
                let store = store.clone();
                move || store.logout()
            })
            .await?;
            let output = serialize_json(&AuthLogoutJson {
                logged_out: changed,
            })?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
    }
    Ok(())
}

/// Typed process CLI arguments for `mez auth`.
#[derive(Debug, Clone, Args)]
pub(super) struct AuthCliArgs {
    /// Optional auth subcommand, defaulting to `status`.
    #[command(subcommand)]
    command: Option<AuthCliCommand>,
}

/// Typed process CLI subcommands for authentication.
#[derive(Debug, Clone, Subcommand)]
enum AuthCliCommand {
    /// Manages local search credentials without selecting or contacting providers.
    Search(search::SearchCliArgs),
    /// Shows provider authentication metadata.
    Status,
    /// Starts an authentication flow.
    #[command(alias = "start")]
    Login(AuthLoginCliArgs),
    /// Removes local authentication metadata.
    Logout,
}

/// Typed process CLI arguments for `mez auth login`.
#[derive(Debug, Clone, clap::Args)]
pub(super) struct AuthLoginCliArgs {
    /// Provider kind to authenticate against.
    #[arg(long, default_value = "openai")]
    provider: String,
    /// Explicit credential identity for a named/shared ChatGPT auth profile.
    #[arg(long)]
    auth_profile: Option<String>,
    /// Selects API-key authentication.
    #[arg(long)]
    api_key: bool,
    /// Selects browser-based ChatGPT sign-in.
    #[arg(long)]
    browser: bool,
    /// Selects out-of-band device-code ChatGPT sign-in.
    #[arg(long, alias = "device-auth")]
    device_code: bool,
    /// Reads a provider API key from a file.
    #[arg(long)]
    api_key_file: Option<PathBuf>,
    /// Selected model profile metadata to store.
    #[arg(long, default_value = "default")]
    profile: String,
    /// Credential store implementation to use.
    #[arg(long)]
    credential_store: Option<String>,
}

impl AuthLoginCliArgs {
    /// Returns the selected authentication method.
    pub(super) fn method(&self) -> Result<AuthMethod> {
        selected_auth_method_from_flags(
            self.api_key,
            self.browser,
            self.device_code,
            "auth login accepts only one authentication method flag",
        )
    }
}

/// Runs synchronous auth store work outside Tokio runtime worker threads.
///
/// Native credential-store backends can use their own internal async runtimes
/// behind synchronous APIs. Running those calls on a blocking thread prevents
/// nested-runtime panics while keeping the CLI entry point async.
async fn run_auth_store_operation<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            MezError::invalid_state(format!("auth credential-store task failed: {error}"))
        })?
}

/// Resolves the active UI theme for browser-login callback presentation.
///
/// Login should not fail because a decorative callback page cannot load theme
/// settings. Configuration errors are therefore treated as a signal to fall
/// back to the default theme while preserving the credential workflow.
pub(super) fn auth_login_ui_theme(paths: &ConfigPaths) -> UiTheme {
    let Ok(layers) = load_runtime_config_layers(paths) else {
        return UiTheme::default();
    };
    let Ok(structured) = runtime_effective_config_value(&layers) else {
        return UiTheme::default();
    };
    runtime_ui_theme_from_config(&structured).unwrap_or_default()
}

/// Runs the auth login method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn auth_login_method(args: &[String]) -> Result<AuthMethod> {
    match super::parse_cli_arg_group::<AuthCliArgs>("mez auth", args)?.command {
        Some(AuthCliCommand::Login(login)) => login.method(),
        _ => Ok(AuthMethod::Browser),
    }
}

/// Runs the noninteractive api key login error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn noninteractive_api_key_login_error() -> MezError {
    MezError::invalid_args(
        "auth login requires noninteractive API-key input; pass --api-key-file \
         PATH or run `mez auth login --api-key` from an interactive terminal",
    )
}

/// Runs the noninteractive browser login error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn noninteractive_browser_login_error() -> MezError {
    MezError::invalid_args(
        "auth login defaults to browser-based ChatGPT sign-in and requires an \
         interactive terminal; use `mez auth login --device-code` for out-of-band \
         ChatGPT sign-in or `mez auth login --provider anthropic --api-key \
         --api-key-file PATH` for Anthropic Console API-key auth",
    )
}

/// Runs the login provider api key for cli operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn login_provider_api_key_for_cli(
    store: &AuthStore,
    provider: &str,
    selected_profile: &str,
    secret: &str,
    credential_store: Option<&str>,
) -> Result<crate::security::auth::AuthMetadata> {
    store.login_provider_api_key_with_selected_store(
        provider,
        selected_profile,
        secret,
        credential_store,
    )
}

/// Runs the login openai provider credential for cli operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn login_openai_provider_credential_for_cli(
    store: &AuthStore,
    auth_profile: &str,
    selected_profile: &str,
    credential_store: Option<&str>,
    credential: OpenAiProviderCredential,
) -> Result<crate::security::auth::AuthMetadata> {
    store.login_chatgpt_profile_credential_with_selected_store(
        auth_profile,
        selected_profile,
        credential,
        credential_store,
    )
}

/// Materializes built-in provider defaults after authentication succeeds.
///
/// Credential persistence completes before this configuration write, so a
/// failed authentication attempt cannot create a provider entry.
fn persist_authenticated_provider_defaults(
    paths: &ConfigPaths,
    metadata: &crate::security::auth::AuthMetadata,
) -> Result<()> {
    paths.materialize_authenticated_provider_defaults(&metadata.provider)
}

/// Runs the write auth login operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn write_auth_login<W: Write>(
    stdout: &mut W,
    output_format: CliOutputFormat,
    metadata: &crate::security::auth::AuthMetadata,
) -> Result<()> {
    let output = format!(
        r#"{{"provider":"{}","authenticated":true,"credential_kind":"{}","selected_model_profile":"{}","credential_store":"{}"}}"#,
        json_escape(&metadata.provider),
        metadata.credential_kind.as_str(),
        json_escape(&metadata.selected_model_profile),
        json_escape(
            metadata
                .credential_store_ref
                .as_deref()
                .unwrap_or_default()
                .split(':')
                .next()
                .unwrap_or("unknown")
        )
    );
    write_json_or_plain(stdout, output_format, &output)?;
    Ok(())
}
/// Carries Auth Status With Metadata Json state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Serialize)]
pub(super) struct AuthStatusWithMetadataJson<'a> {
    /// Stores the authenticated value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) authenticated: bool,
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) provider: &'a str,
    /// Stores the credential kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) credential_kind: &'a str,
    /// Stores the organization id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    /// Stores the selected model profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) selected_model_profile: &'a str,
    /// Stores the credential store ref value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    /// Stores the token expires at value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) token_expires_at: Option<&'a str>,
}

/// Carries Auth Status Without Metadata Json state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Serialize)]
pub(super) struct AuthStatusWithoutMetadataJson {
    /// Stores the authenticated value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) authenticated: bool,
    /// Stores the metadata value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) metadata: Option<()>,
}

/// Runs the auth status json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn auth_status_json(
    authenticated: bool,
    metadata: &Option<crate::security::auth::AuthMetadata>,
) -> Result<String> {
    match metadata {
        Some(metadata) => serialize_json(&AuthStatusWithMetadataJson {
            authenticated,
            provider: &metadata.provider,
            credential_kind: metadata.credential_kind.as_str(),
            selected_model_profile: &metadata.selected_model_profile,
            token_expires_at: metadata.token_expires_at.as_deref(),
        }),
        None => serialize_json(&AuthStatusWithoutMetadataJson {
            authenticated,
            metadata: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::run_auth_store_operation;

    /// Verifies the exact recovery command emitted for a shared ChatGPT profile
    /// is accepted by the CLI and persists only that selected identity through
    /// the browser/device credential completion helper, without network login.
    #[test]
    fn auth_named_profile_recovery_command_and_persistence() {
        super::super::parse_cli_arg_group::<super::AuthCliArgs>(
            "mez auth",
            &[
                "login",
                "--provider",
                "openai",
                "--auth-profile",
                "shared-chatgpt",
                "--device-code",
            ]
            .map(str::to_string),
        )
        .unwrap();
        let root = std::env::temp_dir().join(format!(
            "mez-cli-named-auth-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let store = super::AuthStore::new(super::AuthPaths::under_config_root(&root));
        let credential = super::OpenAiProviderCredential {
            api_key: "synthetic-access".into(),
            refresh_token: Some("synthetic-refresh".into()),
            account_id: None,
            organization_id: None,
            token_expires_at: Some("4102444800".into()),
        };
        let metadata = super::login_openai_provider_credential_for_cli(
            &store,
            "shared-chatgpt",
            "default",
            Some("file"),
            credential,
        )
        .unwrap();
        assert_eq!(metadata.provider, "shared-chatgpt");
        assert!(
            store
                .status_for_provider("shared-chatgpt")
                .unwrap()
                .authenticated
        );
        assert!(
            store
                .read_metadata_for_provider("openai")
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Verifies auth store operations run on a blocking thread rather than a
    /// Tokio runtime worker. Native secret-store adapters may start and drive
    /// their own runtime internally, so this protects browser/device login,
    /// API-key login, status, and logout from nested-runtime panics.
    #[test]
    fn auth_store_operation_allows_runtime_backed_secret_store_work() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let value = runtime
            .block_on(run_auth_store_operation(|| {
                let nested_runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()?;
                Ok(nested_runtime.block_on(async { 42usize }))
            }))
            .unwrap();

        assert_eq!(value, 42);
    }
}
