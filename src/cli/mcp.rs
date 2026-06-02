//! Cli Mcp implementation.
//!
//! This module owns the cli mcp boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    Args, AuthPaths, AuthStore, BTreeSet, CliEnv, CliOutputFormat, ConfigFormat, ConfigLayer,
    ConfigMutation, ConfigMutationOperation, ConfigMutationPlan, ConfigMutationValue, ConfigPaths,
    ConfigScope, DEFAULT_CONFIG_TOML, EffectiveConfig, McpRegistry, MezError, ProjectTrustStore,
    Result, Serialize, Subcommand, TrustDecision, Write, compose_effective_config,
    default_trust_database_path, discover_existing_overlays, discover_project_root, fs,
    json_escape, json_optional, migrate_config_file, persist_config_mutation, serialize_json,
    write_json_or_plain,
};
use crate::auth::{
    AuthCredentialState, McpAuthMetadata, McpAuthStatus, McpOAuthCredential,
    run_mcp_oauth_login_async,
};
use sha2::Digest;

// MCP subcommands and config mutation helpers.

/// Runs the run mcp operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_mcp<W: Write>(
    parsed: McpCliArgs,
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let paths = env.config_paths()?;
    match parsed.command.unwrap_or(McpCliCommand::List) {
        McpCliCommand::List => {
            let effective = load_primary_effective_config(&paths)?;
            let registry = McpRegistry::default();
            let output = format!(
                r#"{{"servers":{},"tools":{}}}"#,
                configured_mcp_servers_json(&effective),
                mcp_tools_json(&registry)
            );
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Inspect { id } => {
            validate_config_identifier(&id, "MCP server id")?;
            let effective = load_primary_effective_config(&paths)?;
            let Some(server) = configured_mcp_server_json(&effective, &id) else {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "MCP server not found",
                ));
            };
            let output = format!(r#"{{"server":{server}}}"#);
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Login {
            id,
            scopes,
            client_id,
            resource,
            credential_store,
            replace_env_token,
        } => {
            validate_config_identifier(&id, "MCP server id")?;
            let effective = load_primary_effective_config(&paths)?;
            let binding = configured_mcp_http_binding(&effective, &id)?;
            if binding.bearer_token_env.is_some() && !replace_env_token {
                return Err(MezError::invalid_args(
                    "mcp login refuses bearer_token_env servers unless --replace-env-token is set",
                ));
            }
            if credential_store
                .as_deref()
                .is_some_and(|store| store != "file" && store != "os")
            {
                return Err(MezError::invalid_args(
                    "mcp login --credential-store must be `file` or `os`",
                ));
            }
            if !interactive {
                return Err(MezError::invalid_args(
                    "mcp login requires an interactive terminal for browser OAuth callback handling",
                ));
            }
            let server_url = binding.url.as_deref().ok_or_else(|| {
                MezError::invalid_state("mcp login HTTP binding has no configured URL")
            })?;
            let credential = run_mcp_oauth_login_async(
                server_url,
                &scopes,
                client_id.as_deref(),
                resource.as_deref(),
            )
            .await?;
            let metadata = McpAuthMetadata::new(
                id.clone(),
                binding
                    .url_origin
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                binding
                    .url_fingerprint
                    .clone()
                    .unwrap_or_else(|| url_fingerprint(server_url)),
            );
            let store = AuthStore::new(AuthPaths::under_config_root(paths.root()));
            let metadata = run_mcp_auth_store_operation({
                let store = store.clone();
                let credential_store = credential_store.clone();
                move || {
                    login_mcp_oauth_credential_for_cli(
                        &store,
                        metadata,
                        credential_store.as_deref(),
                        credential,
                    )
                }
            })
            .await?;
            let credential_store_name = metadata
                .credential_store_ref
                .as_deref()
                .and_then(|reference| reference.split_once(':').map(|(prefix, _)| prefix))
                .unwrap_or("unknown");
            let output = mcp_login_json(&id, credential_store_name, &metadata)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Logout { id } => {
            validate_config_identifier(&id, "MCP server id")?;
            let store = AuthStore::new(AuthPaths::under_config_root(paths.root()));
            let changed = store.logout_mcp_server(&id)?;
            let output = mcp_logout_json(&id, changed)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Status { id } => {
            validate_config_identifier(&id, "MCP server id")?;
            let effective = load_primary_effective_config(&paths)?;
            let binding = configured_mcp_auth_binding(&effective, &id)?;
            let store = AuthStore::new(AuthPaths::under_config_root(paths.root()));
            let status = store.mcp_status(
                &id,
                binding.url_origin.as_deref(),
                binding.url_fingerprint.as_deref(),
            )?;
            let output = mcp_status_json(&binding, &status)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Add {
            id,
            command,
            url,
            args: arg_values,
        } => {
            validate_config_identifier(&id, "MCP server id")?;
            if command.is_some() == url.is_some() {
                return Err(MezError::invalid_args(
                    "mcp add requires exactly one of --command or --url",
                ));
            }
            let mut plans = Vec::new();
            plans.push(persist_primary_config_mutation(
                &paths,
                ConfigMutation::set_boolean(format!("mcp_servers.{}.enabled", id), true),
            )?);
            if let Some(command) = command {
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation::set_string(format!("mcp_servers.{}.command", id), command),
                )?);
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation::set_string_array(
                        format!("mcp_servers.{}.args", id),
                        &arg_values,
                    ),
                )?);
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation {
                        path: format!("mcp_servers.{}.url", id),
                        operation: ConfigMutationOperation::Unset,
                    },
                )?);
            }
            if let Some(url) = url {
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation::set_string(format!("mcp_servers.{}.url", id), url),
                )?);
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation {
                        path: format!("mcp_servers.{}.command", id),
                        operation: ConfigMutationOperation::Unset,
                    },
                )?);
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation::set_string_array(format!("mcp_servers.{}.args", id), &[]),
                )?);
            }
            let output = mcp_mutation_json(
                &id,
                mutation_plans_changed(&plans),
                mutation_plans_reload_required(&plans),
            )?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Remove { id } => {
            validate_config_identifier(&id, "MCP server id")?;
            let plans = persist_mcp_server_removal(&paths, &id)?;
            let output = mcp_remove_json(
                &id,
                mutation_plans_changed(&plans),
                mutation_plans_reload_required(&plans),
            )?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Enable { id } => {
            validate_config_identifier(&id, "MCP server id")?;
            let enabled = true;
            let plan = persist_primary_config_mutation(
                &paths,
                ConfigMutation::set_boolean(format!("mcp_servers.{id}.enabled"), enabled),
            )?;
            let output = mcp_enabled_json(&id, enabled, plan.changed, plan.reload_required)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Disable { id } => {
            validate_config_identifier(&id, "MCP server id")?;
            let enabled = false;
            let plan = persist_primary_config_mutation(
                &paths,
                ConfigMutation::set_boolean(format!("mcp_servers.{id}.enabled"), enabled),
            )?;
            let output = mcp_enabled_json(&id, enabled, plan.changed, plan.reload_required)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
    }
    Ok(())
}

/// Typed process CLI arguments for `mez mcp`.
#[derive(Debug, Clone, Args)]
pub(super) struct McpCliArgs {
    /// Optional MCP subcommand, defaulting to `list`.
    #[command(subcommand)]
    command: Option<McpCliCommand>,
}

/// Typed process CLI subcommands for MCP server configuration.
#[derive(Debug, Clone, Subcommand)]
enum McpCliCommand {
    /// Lists configured MCP servers and known tools.
    List,
    /// Inspects one configured MCP server.
    Inspect {
        /// MCP server id.
        id: String,
    },
    /// Starts MCP HTTP OAuth login for one configured server.
    Login {
        /// MCP server id.
        id: String,
        /// Comma-separated or repeated OAuth scopes.
        #[arg(long, value_delimiter = ',')]
        scopes: Vec<String>,
        /// OAuth client id for servers that require a pre-registered client.
        #[arg(long)]
        client_id: Option<String>,
        /// OAuth resource parameter to request.
        #[arg(long)]
        resource: Option<String>,
        /// Credential store preference for persisted MCP OAuth secrets.
        #[arg(long)]
        credential_store: Option<String>,
        /// Permit login even when bearer_token_env is configured.
        #[arg(long)]
        replace_env_token: bool,
    },
    /// Deletes stored MCP OAuth credentials for one configured server.
    Logout {
        /// MCP server id.
        id: String,
    },
    /// Reports secret-safe MCP auth status for one configured server.
    Status {
        /// MCP server id.
        id: String,
    },
    /// Adds or replaces one configured MCP server.
    Add {
        /// MCP server id.
        id: String,
        /// Stdio command path/name.
        #[arg(long)]
        command: Option<String>,
        /// Streamable HTTP endpoint URL.
        #[arg(long)]
        url: Option<String>,
        /// Stdio command argument. May be repeated.
        #[arg(long = "arg", allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Removes one configured MCP server.
    Remove {
        /// MCP server id.
        id: String,
    },
    /// Enables one configured MCP server.
    Enable {
        /// MCP server id.
        id: String,
    },
    /// Disables one configured MCP server.
    Disable {
        /// MCP server id.
        id: String,
    },
}

impl ConfigMutation {
    /// Runs the set string operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_string(path: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.into())),
        }
    }

    /// Runs the set boolean operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_boolean(path: impl Into<String>, value: bool) -> Self {
        Self {
            path: path.into(),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(value)),
        }
    }

    /// Runs the set string array operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_string_array(path: impl Into<String>, values: &[String]) -> Self {
        Self {
            path: path.into(),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
                values.to_vec(),
            )),
        }
    }
}

/// Runs the load primary effective config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn load_primary_effective_config(paths: &ConfigPaths) -> Result<EffectiveConfig> {
    compose_effective_config(&load_primary_config_layers(paths)?)
}

/// Runs the load runtime config layers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn load_runtime_config_layers(paths: &ConfigPaths) -> Result<Vec<ConfigLayer>> {
    let trust_store =
        ProjectTrustStore::load_from_file(&default_trust_database_path(paths.root()))?;
    let current_dir = std::env::current_dir()?;
    load_runtime_config_layers_for_directory(paths, &trust_store, &current_dir)
}

/// Runs the load runtime config layers for directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn load_runtime_config_layers_for_directory(
    paths: &ConfigPaths,
    trust_store: &ProjectTrustStore,
    current_dir: &std::path::Path,
) -> Result<Vec<ConfigLayer>> {
    let mut layers = load_primary_config_layers(paths)?;
    let project_root = discover_project_root(current_dir);
    let overlay_files = discover_existing_overlays(&project_root, current_dir)?;
    let trusted = trust_store
        .get(&project_root)
        .is_some_and(|record| record.state == TrustDecision::Trusted);
    let overlay_count = overlay_files.len();

    for (index, overlay_path) in overlay_files.into_iter().enumerate() {
        let name = if overlay_count == 1 {
            "project".to_string()
        } else {
            format!("project:{}", index + 1)
        };
        layers.push(ConfigLayer {
            name,
            format: ConfigFormat::from_path(&overlay_path)?,
            text: fs::read_to_string(&overlay_path)?,
            path: Some(overlay_path),
            scope: ConfigScope::ProjectOverlay,
            trusted,
        });
    }

    Ok(layers)
}

/// Runs the load primary config layers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn load_primary_config_layers(paths: &ConfigPaths) -> Result<Vec<ConfigLayer>> {
    let (layer_path, format, text) = if let Some(selected) = paths.select_primary_file()? {
        migrate_config_file(&selected)?;
        (
            Some(selected.clone()),
            ConfigFormat::from_path(&selected)?,
            fs::read_to_string(selected)?,
        )
    } else {
        (None, ConfigFormat::Toml, DEFAULT_CONFIG_TOML.to_string())
    };
    Ok(vec![ConfigLayer {
        name: "primary".to_string(),
        path: layer_path,
        format,
        scope: ConfigScope::Primary,
        trusted: true,
        text,
    }])
}

/// Runs the persist primary config mutation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn persist_primary_config_mutation(
    paths: &ConfigPaths,
    mutation: ConfigMutation,
) -> Result<ConfigMutationPlan> {
    let path = paths.ensure_default_config()?;
    persist_config_mutation(&path, ConfigScope::Primary, mutation)
}

/// Runs the persist mcp server removal operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn persist_mcp_server_removal(
    paths: &ConfigPaths,
    id: &str,
) -> Result<Vec<ConfigMutationPlan>> {
    persist_primary_config_mutation(
        paths,
        ConfigMutation {
            path: format!("mcp_servers.{id}"),
            operation: ConfigMutationOperation::Unset,
        },
    )
    .map(|plan| vec![plan])
}

/// Runs the configured mcp servers json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn configured_mcp_servers_json(effective: &EffectiveConfig) -> String {
    let servers = configured_mcp_server_ids(effective)
        .into_iter()
        .filter_map(|id| configured_mcp_server_json(effective, &id))
        .collect::<Vec<_>>();
    format!("[{}]", servers.join(","))
}

/// Runs the configured mcp server ids operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn configured_mcp_server_ids(effective: &EffectiveConfig) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for path in effective.values().keys() {
        let mut segments = path.split('.');
        if segments.next() == Some("mcp_servers")
            && let Some(id) = segments.next()
            && segments.next().is_some()
        {
            ids.insert(id.to_string());
        }
    }
    ids.into_iter().collect()
}

/// Runs the configured mcp server json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn configured_mcp_server_json(effective: &EffectiveConfig, id: &str) -> Option<String> {
    let prefix = format!("mcp_servers.{id}.");
    if !effective
        .values()
        .keys()
        .any(|path| path.starts_with(&prefix))
    {
        return None;
    }
    let name = effective
        .get(&format!("{prefix}name"))
        .unwrap_or(id)
        .to_string();
    let command = effective.get(&format!("{prefix}command"));
    let url = effective.get(&format!("{prefix}url"));
    let transport = if url.is_some() {
        "streamable_http"
    } else {
        "stdio"
    };
    let enabled = effective
        .get(&format!("{prefix}enabled"))
        .map(|value| value == "true")
        .unwrap_or(true);
    let args = effective
        .get(&format!("{prefix}args"))
        .map(config_value_array_json)
        .unwrap_or_else(|| "[]".to_string());
    Some(format!(
        r#"{{"id":"{}","name":"{}","enabled":{},"transport":"{}","command":{},"url":{},"args":{},"source":"primary"}}"#,
        json_escape(id),
        json_escape(&name),
        enabled,
        transport,
        json_optional(command),
        json_optional(url),
        args
    ))
}

/// Secret-safe auth binding for one configured MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
struct McpAuthBinding {
    /// Configured server id.
    id: String,
    /// Configured transport label.
    transport: &'static str,
    /// Configured server URL when the transport is HTTP.
    url: Option<String>,
    /// URL origin used to bind stored OAuth credentials.
    url_origin: Option<String>,
    /// Stable fingerprint of the full configured URL.
    url_fingerprint: Option<String>,
    /// Environment variable used for bearer auth, when configured.
    bearer_token_env: Option<String>,
}

/// Typed JSON output for successful MCP login flows.
#[derive(Serialize)]
struct McpLoginJson {
    /// Stable configured server identifier.
    server_id: String,
    /// Whether a usable credential is now available.
    authenticated: bool,
    /// Whether metadata exists for the configured server.
    metadata_present: bool,
    /// Credential backend selected for the stored secret.
    credential_store: String,
    /// Origin component of the configured MCP server URL.
    url_origin: String,
    /// Stable fingerprint of the full configured MCP URL.
    url_fingerprint: String,
    /// Optional Unix-seconds access-token expiration timestamp.
    token_expires_at: Option<String>,
    /// Optional non-secret OAuth scopes attached to the credential.
    scopes: Vec<String>,
}

/// Typed JSON output for secret-safe MCP auth status.
#[derive(Serialize)]
struct McpStatusJson {
    /// Stable configured server identifier.
    server_id: String,
    /// Configured transport label.
    transport: String,
    /// Secret-safe auth mode name.
    auth_mode: String,
    /// Whether a usable credential is currently available.
    authenticated: bool,
    /// Whether metadata exists for this server.
    metadata_present: bool,
    /// Whether the stored credential URL binding mismatches config.
    stale_url: bool,
    /// Secret-safe credential availability state name.
    credential_state: String,
    /// Environment variable used for bearer auth, when configured.
    bearer_token_env: Option<String>,
    /// URL origin used to bind stored OAuth credentials.
    url_origin: Option<String>,
    /// Stable fingerprint of the full configured URL.
    url_fingerprint: Option<String>,
    /// Optional Unix-seconds access-token expiration timestamp.
    token_expires_at: Option<String>,
    /// Optional non-secret OAuth scopes attached to the credential.
    scopes: Vec<String>,
}

/// Typed JSON output for MCP logout flows.
#[derive(Serialize)]
struct McpLogoutJson {
    /// Stable configured server identifier.
    server_id: String,
    /// Whether the credential is now logged out.
    logged_out: bool,
    /// Whether any stored state changed.
    changed: bool,
}

/// Typed JSON output for MCP config mutations.
#[derive(Serialize)]
struct McpMutationJson {
    /// Stable configured server identifier.
    server_id: String,
    /// Whether any persisted state changed.
    changed: bool,
    /// Whether the runtime must reload configuration.
    reload_required: bool,
}

/// Typed JSON output for MCP server removals.
#[derive(Serialize)]
struct McpRemoveJson {
    /// Stable configured server identifier.
    server_id: String,
    /// Whether the server was removed.
    removed: bool,
    /// Whether any persisted state changed.
    changed: bool,
    /// Whether the runtime must reload configuration.
    reload_required: bool,
}

/// Typed JSON output for MCP enable or disable operations.
#[derive(Serialize)]
struct McpEnabledJson {
    /// Stable configured server identifier.
    server_id: String,
    /// Whether the server is enabled after the mutation.
    enabled: bool,
    /// Whether any persisted state changed.
    changed: bool,
    /// Whether the runtime must reload configuration.
    reload_required: bool,
}

/// Resolves the configured auth binding for status output.
fn configured_mcp_auth_binding(effective: &EffectiveConfig, id: &str) -> Result<McpAuthBinding> {
    let prefix = format!("mcp_servers.{id}.");
    if !effective
        .values()
        .keys()
        .any(|path| path.starts_with(&prefix))
    {
        return Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "MCP server not found",
        ));
    }
    let url = effective
        .get(&format!("{prefix}url"))
        .map(ToOwned::to_owned);
    let transport = if url.is_some() {
        "streamable_http"
    } else {
        "stdio"
    };
    let (url_origin, url_fingerprint) = if let Some(url) = url.as_deref() {
        (Some(http_url_origin(url)?), Some(url_fingerprint(url)))
    } else {
        (None, None)
    };
    Ok(McpAuthBinding {
        id: id.to_string(),
        transport,
        url,
        url_origin,
        url_fingerprint,
        bearer_token_env: effective
            .get(&format!("{prefix}bearer_token_env"))
            .map(ToOwned::to_owned),
    })
}

/// Resolves and validates the HTTP auth binding required by `mcp login`.
fn configured_mcp_http_binding(effective: &EffectiveConfig, id: &str) -> Result<McpAuthBinding> {
    let binding = configured_mcp_auth_binding(effective, id)?;
    let Some(url) = binding.url.as_deref() else {
        return Err(MezError::invalid_args(
            "mcp login requires a streamable HTTP server",
        ));
    };
    if !url.starts_with("https://") {
        return Err(MezError::invalid_args(
            "mcp login requires an HTTPS server URL",
        ));
    }
    Ok(binding)
}

/// Computes an origin string from a configured HTTP URL.
fn http_url_origin(url: &str) -> Result<String> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(MezError::invalid_args(
            "MCP HTTP server URL must include a scheme",
        ));
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())
        .ok_or_else(|| MezError::invalid_args("MCP HTTP server URL must include a host"))?;
    Ok(format!("{}://{}", scheme.to_ascii_lowercase(), authority))
}

/// Computes the stable non-secret configured-URL fingerprint for MCP auth binding.
fn url_fingerprint(url: &str) -> String {
    let digest = sha2::Sha256::digest(url.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

/// Runs a blocking MCP auth-store operation off the async CLI task.
async fn run_mcp_auth_store_operation<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            MezError::invalid_state(format!("MCP auth credential-store task failed: {error}"))
        })?
}

/// Stores an MCP OAuth credential with the CLI-selected credential backend.
fn login_mcp_oauth_credential_for_cli(
    store: &AuthStore,
    metadata: McpAuthMetadata,
    credential_store: Option<&str>,
    credential: McpOAuthCredential,
) -> Result<McpAuthMetadata> {
    store.login_mcp_oauth_credential_with_selected_store(metadata, credential, credential_store)
}

/// Renders successful MCP login output through typed JSON serialization.
fn mcp_login_json(
    id: &str,
    credential_store_name: &str,
    metadata: &McpAuthMetadata,
) -> Result<String> {
    serialize_json(&McpLoginJson {
        server_id: id.to_string(),
        authenticated: true,
        metadata_present: true,
        credential_store: credential_store_name.to_string(),
        url_origin: metadata.url_origin.clone(),
        url_fingerprint: metadata.url_fingerprint.clone(),
        token_expires_at: metadata.token_expires_at.clone(),
        scopes: metadata.scopes.clone(),
    })
}

/// Renders secret-safe MCP auth status as JSON.
fn mcp_status_json(binding: &McpAuthBinding, status: &McpAuthStatus) -> Result<String> {
    let auth_mode = if binding.bearer_token_env.is_some() {
        "env-bearer"
    } else if status.metadata_present {
        "oauth"
    } else {
        "none"
    };
    let scopes = status
        .metadata
        .as_ref()
        .map(|metadata| metadata.scopes.clone())
        .unwrap_or_default();
    let token_expires_at = status
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.token_expires_at.clone());
    serialize_json(&McpStatusJson {
        server_id: binding.id.clone(),
        transport: binding.transport.to_string(),
        auth_mode: auth_mode.to_string(),
        authenticated: status.authenticated,
        metadata_present: status.metadata_present,
        stale_url: status.stale_url,
        credential_state: mcp_credential_state_name(&status.credential_state).to_string(),
        bearer_token_env: binding.bearer_token_env.clone(),
        url_origin: binding.url_origin.clone(),
        url_fingerprint: binding.url_fingerprint.clone(),
        token_expires_at,
        scopes,
    })
}

/// Renders MCP logout output through typed JSON serialization.
fn mcp_logout_json(id: &str, changed: bool) -> Result<String> {
    serialize_json(&McpLogoutJson {
        server_id: id.to_string(),
        logged_out: changed,
        changed,
    })
}

/// Renders MCP config mutation output through typed JSON serialization.
fn mcp_mutation_json(id: &str, changed: bool, reload_required: bool) -> Result<String> {
    serialize_json(&McpMutationJson {
        server_id: id.to_string(),
        changed,
        reload_required,
    })
}

/// Renders MCP server removal output through typed JSON serialization.
fn mcp_remove_json(id: &str, changed: bool, reload_required: bool) -> Result<String> {
    serialize_json(&McpRemoveJson {
        server_id: id.to_string(),
        removed: true,
        changed,
        reload_required,
    })
}

/// Renders MCP enable or disable output through typed JSON serialization.
fn mcp_enabled_json(
    id: &str,
    enabled: bool,
    changed: bool,
    reload_required: bool,
) -> Result<String> {
    serialize_json(&McpEnabledJson {
        server_id: id.to_string(),
        enabled,
        changed,
        reload_required,
    })
}

/// Returns the stable display name for a secret-safe credential state.
fn mcp_credential_state_name(state: &AuthCredentialState) -> &'static str {
    match state {
        AuthCredentialState::LoggedOut => "logged-out",
        AuthCredentialState::MissingSecret { .. } => "missing-secret",
        AuthCredentialState::Available { .. } => "available",
    }
}

/// Runs the config value array json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_value_array_json(value: &str) -> String {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()
        .filter(serde_json::Value::is_array)
        .and_then(|value| serde_json::to_string(&value).ok())
        .unwrap_or_else(|| "[]".to_string())
}

/// Runs the mutation plans changed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mutation_plans_changed(plans: &[ConfigMutationPlan]) -> bool {
    plans.iter().any(|plan| plan.changed)
}

/// Runs the mutation plans reload required operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mutation_plans_reload_required(plans: &[ConfigMutationPlan]) -> bool {
    plans.iter().any(|plan| plan.reload_required)
}

/// Runs the validate config identifier operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_config_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(MezError::invalid_args(format!(
            "{label} must contain only ASCII letters, digits, '_' or '-'"
        )));
    }
    Ok(())
}
/// Runs the mcp tools json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_tools_json(registry: &McpRegistry) -> String {
    let tools = registry
        .list_servers()
        .iter()
        .flat_map(|server| {
            server.tools.iter().map(|tool| {
                format!(
                    r#"{{"server_id":"{}","name":"{}","available":{},"blacklisted":{},"permission_required":{},"input_schema":{}}}"#,
                    json_escape(&tool.server_id),
                    json_escape(&tool.name),
                    tool.available,
                    tool.blacklisted,
                    tool.permission_required,
                    tool.input_schema_json
                )
            })
        })
        .collect::<Vec<_>>();
    format!("[{}]", tools.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{CredentialStoreKind, McpCredentialKind};

    /// Verifies typed MCP status JSON preserves the existing secret-safe field
    /// order and null-handling expected by CLI scripts.
    #[test]
    fn mcp_status_json_preserves_secret_safe_output_shape() {
        let binding = McpAuthBinding {
            id: "demo".to_string(),
            transport: "streamable_http",
            url: Some("https://example.invalid/mcp".to_string()),
            url_origin: Some("https://example.invalid".to_string()),
            url_fingerprint: Some("sha256:abc".to_string()),
            bearer_token_env: None,
        };
        let status = McpAuthStatus {
            server_id: "demo".to_string(),
            authenticated: true,
            metadata_present: true,
            credential_state: AuthCredentialState::Available {
                store: CredentialStoreKind::OperatingSystem,
                reference: "os-keyring:demo".to_string(),
            },
            metadata: Some(McpAuthMetadata {
                server_id: "demo".to_string(),
                credential_kind: McpCredentialKind::OAuthBearer,
                url_origin: "https://example.invalid".to_string(),
                url_fingerprint: "sha256:abc".to_string(),
                scopes: vec!["scope:read".to_string(), "scope:write".to_string()],
                client_id: None,
                resource: None,
                authorization_endpoint: None,
                token_endpoint: None,
                credential_store_ref: Some("os-keyring:demo".to_string()),
                refresh_credential_store_ref: None,
                token_expires_at: Some("1700000000".to_string()),
            }),
            stale_url: false,
        };

        let output = mcp_status_json(&binding, &status).unwrap();

        assert_eq!(
            output,
            r#"{"server_id":"demo","transport":"streamable_http","auth_mode":"oauth","authenticated":true,"metadata_present":true,"stale_url":false,"credential_state":"available","bearer_token_env":null,"url_origin":"https://example.invalid","url_fingerprint":"sha256:abc","token_expires_at":"1700000000","scopes":["scope:read","scope:write"]}"#
        );
    }

    /// Verifies typed MCP login JSON preserves the CLI contract for
    /// credential-store, expiry, and scope fields.
    #[test]
    fn mcp_login_json_preserves_secret_safe_output_shape() {
        let metadata = McpAuthMetadata {
            server_id: "demo".to_string(),
            credential_kind: McpCredentialKind::OAuthBearer,
            url_origin: "https://example.invalid".to_string(),
            url_fingerprint: "sha256:def".to_string(),
            scopes: vec!["scope:read".to_string()],
            client_id: None,
            resource: None,
            authorization_endpoint: None,
            token_endpoint: None,
            credential_store_ref: Some("file:demo".to_string()),
            refresh_credential_store_ref: None,
            token_expires_at: None,
        };

        let output = mcp_login_json("demo", "file", &metadata).unwrap();

        assert_eq!(
            output,
            r#"{"server_id":"demo","authenticated":true,"metadata_present":true,"credential_store":"file","url_origin":"https://example.invalid","url_fingerprint":"sha256:def","token_expires_at":null,"scopes":["scope:read"]}"#
        );
    }
}
