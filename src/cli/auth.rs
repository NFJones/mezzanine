//! Cli Auth implementation.
//!
//! This module owns the cli auth boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AuthMethod, AuthPaths, AuthStore, CliEnv, CliOutputFormat, ConfigPaths, CredentialStorePlan,
    MezError, OpenAiProviderCredential, Parser, PathBuf, Result, Serialize, Subcommand, UiTheme,
    Write, fs, is_cli_help_request, json_escape, load_runtime_config_layers, parse_cli_args,
    run_openai_browser_login_with_theme_async, run_openai_device_code_login_async,
    runtime_effective_config_value, runtime_ui_theme_from_config, serialize_json,
    write_json_or_plain,
};

// Authentication subcommands and output formatting.

/// Runs the run auth operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_auth<W: Write>(
    args: &[String],
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    if is_cli_help_request(args) {
        writeln!(
            stdout,
            "usage: mez auth <status|login|logout>\n\
             \n\
             login defaults to browser-based ChatGPT sign-in.\n\
             Use --device-code for out-of-band ChatGPT sign-in, or --api-key \
             [--api-key-file PATH] for OpenAI API-key setup."
        )?;
        return Ok(());
    }
    let parsed = parse_cli_args::<AuthCliArgs>("mez auth", args)?;
    let paths = env.config_paths()?;
    let store = AuthStore::new(AuthPaths::under_config_root(paths.root()));

    match parsed.command.unwrap_or_default() {
        AuthCliCommand::Help => {
            writeln!(
                stdout,
                "usage: mez auth <status|login|logout>\n\
                 \n\
                 login defaults to browser-based ChatGPT sign-in.\n\
                 Use --device-code for out-of-band ChatGPT sign-in, or --api-key \
                 [--api-key-file PATH] for OpenAI API-key setup."
            )?;
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
            let selected_profile = login.profile.clone();
            let credential_store = login.credential_store.clone();
            match method {
                AuthMethod::ApiKey => {
                    if let Some(path) = login.api_key_file.as_ref() {
                        let secret = fs::read_to_string(path)?;
                        let secret = secret.trim().to_string();
                        let metadata = run_auth_store_operation({
                            let store = store.clone();
                            let selected_profile = selected_profile.to_string();
                            let credential_store = credential_store.clone();
                            move || {
                                login_openai_api_key_for_cli(
                                    &store,
                                    &selected_profile,
                                    &secret,
                                    credential_store.as_deref(),
                                )
                            }
                        })
                        .await?;
                        write_auth_login(stdout, output_format, &metadata)?;
                        return Ok(());
                    }
                    if interactive {
                        let secret =
                            rpassword::prompt_password("OpenAI API key: ").map_err(|error| {
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
                            move || {
                                login_openai_api_key_for_cli(
                                    &store,
                                    &selected_profile,
                                    &secret,
                                    credential_store.as_deref(),
                                )
                            }
                        })
                        .await?;
                        write_auth_login(stdout, output_format, &metadata)?;
                        return Ok(());
                    }
                    return Err(noninteractive_api_key_login_error());
                }
                AuthMethod::Browser => {
                    if !interactive {
                        return Err(noninteractive_browser_login_error());
                    }
                    let ui_theme = auth_login_ui_theme(&paths);
                    let credential = run_openai_browser_login_with_theme_async(&ui_theme).await?;
                    let metadata = run_auth_store_operation({
                        let store = store.clone();
                        let selected_profile = selected_profile.to_string();
                        let credential_store = credential_store.clone();
                        move || {
                            login_openai_provider_credential_for_cli(
                                &store,
                                &selected_profile,
                                credential_store.as_deref(),
                                credential,
                            )
                        }
                    })
                    .await?;
                    write_auth_login(stdout, output_format, &metadata)?;
                }
                AuthMethod::DeviceCode => {
                    let credential = run_openai_device_code_login_async().await?;
                    let metadata = run_auth_store_operation({
                        let store = store.clone();
                        let selected_profile = selected_profile.to_string();
                        let credential_store = credential_store.clone();
                        move || {
                            login_openai_provider_credential_for_cli(
                                &store,
                                &selected_profile,
                                credential_store.as_deref(),
                                credential,
                            )
                        }
                    })
                    .await?;
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
            let output = format!(r#"{{"logged_out":{changed}}}"#);
            write_json_or_plain(stdout, output_format, &output)?;
        }
    }
    Ok(())
}

/// Typed process CLI arguments for `mez auth`.
#[derive(Debug, Parser)]
#[command(
    name = "mez auth",
    disable_help_flag = true,
    disable_help_subcommand = true
)]
struct AuthCliArgs {
    /// Optional auth subcommand, defaulting to `status`.
    #[command(subcommand)]
    command: Option<AuthCliCommand>,
}

/// Typed process CLI subcommands for authentication.
#[derive(Debug, Clone, Subcommand, Default)]
enum AuthCliCommand {
    /// Shows authentication CLI usage.
    #[command(name = "help")]
    Help,
    /// Shows provider authentication metadata.
    #[default]
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
    /// Selects API-key authentication.
    #[arg(long)]
    api_key: bool,
    /// Selects browser-based ChatGPT sign-in.
    #[arg(long)]
    browser: bool,
    /// Selects out-of-band device-code ChatGPT sign-in.
    #[arg(long, alias = "device-auth")]
    device_code: bool,
    /// Reads the OpenAI API key from a file.
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
        let selected_methods = [self.api_key, self.browser, self.device_code]
            .into_iter()
            .filter(|selected| *selected)
            .count();
        if selected_methods > 1 {
            return Err(MezError::invalid_args(
                "auth login accepts only one authentication method flag",
            ));
        }
        if self.api_key {
            Ok(AuthMethod::ApiKey)
        } else if self.device_code {
            Ok(AuthMethod::DeviceCode)
        } else {
            Ok(AuthMethod::Browser)
        }
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
fn auth_login_ui_theme(paths: &ConfigPaths) -> UiTheme {
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
    match parse_cli_args::<AuthCliArgs>("mez auth", args)?.command {
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
        "auth login requires an OpenAI API key in noninteractive mode; pass \
         --api-key-file PATH or run `mez auth login --api-key` from an interactive terminal",
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
         ChatGPT sign-in or `mez auth login --api-key --api-key-file PATH` for \
         noninteractive API-key setup",
    )
}

/// Runs the login openai api key for cli operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn login_openai_api_key_for_cli(
    store: &AuthStore,
    selected_profile: &str,
    secret: &str,
    credential_store: Option<&str>,
) -> Result<crate::auth::AuthMetadata> {
    match credential_store {
        Some("file") => {
            let credential_store = store.file_credential_store("openai")?;
            store.login_openai_api_key(selected_profile, secret, &credential_store)
        }
        Some("os") => store.login_openai_api_key_with_default_os_store(selected_profile, secret),
        Some(other) => Err(MezError::invalid_args(format!(
            "unknown credential store `{other}`"
        ))),
        None => match store.credential_store_plan("openai") {
            CredentialStorePlan::OperatingSystem { .. } => {
                store.login_openai_api_key_with_default_os_store(selected_profile, secret)
            }
            CredentialStorePlan::PrivateFileFallback { .. } => {
                let credential_store = store.file_credential_store("openai")?;
                store.login_openai_api_key(selected_profile, secret, &credential_store)
            }
        },
    }
}

/// Runs the login openai provider credential for cli operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn login_openai_provider_credential_for_cli(
    store: &AuthStore,
    selected_profile: &str,
    credential_store: Option<&str>,
    credential: OpenAiProviderCredential,
) -> Result<crate::auth::AuthMetadata> {
    match credential_store {
        Some("file") => {
            let credential_store = store.file_credential_store("openai")?;
            store.login_openai_provider_credential(selected_profile, credential, &credential_store)
        }
        Some("os") => store
            .login_openai_provider_credential_with_default_os_store(selected_profile, credential),
        Some(other) => Err(MezError::invalid_args(format!(
            "unknown credential store `{other}`"
        ))),
        None => store
            .login_openai_provider_credential_with_preferred_store(selected_profile, credential),
    }
}

/// Runs the write auth login operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn write_auth_login<W: Write>(
    stdout: &mut W,
    output_format: CliOutputFormat,
    metadata: &crate::auth::AuthMetadata,
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
    /// Stores the account id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) account_id: Option<&'a str>,
    /// Stores the organization id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) organization_id: Option<&'a str>,
    /// Stores the selected model profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) selected_model_profile: &'a str,
    /// Stores the credential store ref value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) credential_store_ref: Option<&'a str>,
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
    metadata: &Option<crate::auth::AuthMetadata>,
) -> Result<String> {
    match metadata {
        Some(metadata) => serialize_json(&AuthStatusWithMetadataJson {
            authenticated,
            provider: &metadata.provider,
            credential_kind: metadata.credential_kind.as_str(),
            account_id: metadata.account_id.as_deref(),
            organization_id: metadata.organization_id.as_deref(),
            selected_model_profile: &metadata.selected_model_profile,
            credential_store_ref: metadata.credential_store_ref.as_deref(),
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
    use super::*;

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
