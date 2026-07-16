//! Cli Mcp implementation.
//!
//! This module owns the cli mcp boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    Args, AuthPaths, AuthStore, BTreeSet, CliEnv, CliOutputFormat, ConfigFormat, ConfigLayer,
    ConfigMutationPlan, ConfigPaths, ConfigScope, DEFAULT_CONFIG_TOML, EffectiveConfig,
    McpRegistry, MezError, ProjectTrustStore, Result, Serialize, Subcommand, TrustDecision, Write,
    compose_effective_config, default_trust_database_path, discover_existing_overlays,
    discover_project_root, fs, migrate_config_file, serialize_json, write_json_or_plain,
};
use crate::auth::{
    AuthCredentialState, McpAuthMetadata, McpAuthStatus, McpCredentialKind, McpOAuthCredential,
    run_mcp_oauth_login_async,
};
use crate::mcp::{
    McpConfigCommand, McpConfigTransport, mcp_config_command_report, mcp_config_setting_from_user,
    persist_mcp_config_command,
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
            let output = mcp_list_json(&effective, &registry)?;
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
            let output = mcp_inspect_json(server)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Login {
            id,
            token,
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
            if token.is_some() && (!scopes.is_empty() || client_id.is_some() || resource.is_some())
            {
                return Err(MezError::invalid_args(
                    "mcp login --token cannot be combined with OAuth login options",
                ));
            }
            if token.is_none() && !interactive {
                return Err(MezError::invalid_args(
                    "mcp login requires an interactive terminal for browser OAuth callback handling",
                ));
            }
            let server_url = binding.url.as_deref().ok_or_else(|| {
                MezError::invalid_state("mcp login HTTP binding has no configured URL")
            })?;
            let login_credential = if let Some(token) = token {
                McpLoginCredential::StaticBearer(token)
            } else {
                McpLoginCredential::OAuth(
                    run_mcp_oauth_login_async(
                        server_url,
                        &scopes,
                        client_id.as_deref(),
                        resource.as_deref(),
                    )
                    .await?,
                )
            };
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
                move || match login_credential {
                    McpLoginCredential::StaticBearer(token) => {
                        login_mcp_static_bearer_credential_for_cli(
                            &store,
                            metadata,
                            credential_store.as_deref(),
                            token,
                        )
                    }
                    McpLoginCredential::OAuth(credential) => login_mcp_oauth_credential_for_cli(
                        &store,
                        metadata,
                        credential_store.as_deref(),
                        credential,
                    ),
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
            disabled,
        } => {
            if command.is_some() == url.is_some() {
                return Err(MezError::invalid_args(
                    "mcp add requires exactly one of --command or --url",
                ));
            }
            let transport = if let Some(command) = command {
                McpConfigTransport::Stdio {
                    command,
                    args: arg_values,
                }
            } else {
                McpConfigTransport::StreamableHttp {
                    url: url.expect("url is Some when command is None"),
                }
            };
            let plans = persist_mcp_config_command(
                &paths,
                &McpConfigCommand::Add {
                    id: id.clone(),
                    transport,
                    enabled: !disabled,
                },
            )?;
            let output = mcp_mutation_json(
                &id,
                mutation_plans_changed(&plans),
                mutation_plans_reload_required(&plans),
            )?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Remove { id } => {
            let plans =
                persist_mcp_config_command(&paths, &McpConfigCommand::Remove { id: id.clone() })?;
            let output = mcp_remove_json(
                &id,
                mutation_plans_changed(&plans),
                mutation_plans_reload_required(&plans),
            )?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Enable { id } => {
            let enabled = true;
            let plans = persist_mcp_config_command(
                &paths,
                &McpConfigCommand::Enable {
                    id: id.clone(),
                    enabled,
                },
            )?;
            let report = mcp_config_command_report(&plans);
            let output = mcp_enabled_json(&id, enabled, report.changed, report.reload_required)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Disable { id } => {
            let enabled = false;
            let plans = persist_mcp_config_command(
                &paths,
                &McpConfigCommand::Enable {
                    id: id.clone(),
                    enabled,
                },
            )?;
            let report = mcp_config_command_report(&plans);
            let output = mcp_enabled_json(&id, enabled, report.changed, report.reload_required)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Set { id, setting, value } => {
            let setting = mcp_config_setting_from_user(&setting)?;
            let plans = persist_mcp_config_command(
                &paths,
                &McpConfigCommand::Set {
                    id: id.clone(),
                    setting,
                    value,
                },
            )?;
            let report = mcp_config_command_report(&plans);
            let output = mcp_setting_json(&id, report.changed, report.reload_required)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Unset { id, setting } => {
            let setting = mcp_config_setting_from_user(&setting)?;
            let plans = persist_mcp_config_command(
                &paths,
                &McpConfigCommand::Unset {
                    id: id.clone(),
                    setting,
                },
            )?;
            let report = mcp_config_command_report(&plans);
            let output = mcp_setting_json(&id, report.changed, report.reload_required)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Tools { command } => {
            let command = match command {
                McpCliToolsCommand::Enable { id, tools } => {
                    McpConfigCommand::ToolsEnable { id, tools }
                }
                McpCliToolsCommand::Disable { id, tools } => {
                    McpConfigCommand::ToolsDisable { id, tools }
                }
                McpCliToolsCommand::Reset { id } => McpConfigCommand::ToolsReset { id },
            };
            let id = mcp_cli_command_id(&command).to_string();
            let plans = persist_mcp_config_command(&paths, &command)?;
            let report = mcp_config_command_report(&plans);
            let output = mcp_setting_json(&id, report.changed, report.reload_required)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Approval { command } => {
            let command = match command {
                McpCliApprovalCommand::Set { id, approval } => {
                    McpConfigCommand::ApprovalSet { id, approval }
                }
                McpCliApprovalCommand::Unset { id } => McpConfigCommand::ApprovalUnset { id },
            };
            let id = mcp_cli_command_id(&command).to_string();
            let plans = persist_mcp_config_command(&paths, &command)?;
            let report = mcp_config_command_report(&plans);
            let output = mcp_setting_json(&id, report.changed, report.reload_required)?;
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
        /// Static bearer token stored without OAuth login.
        #[arg(long)]
        token: Option<String>,
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
        /// Register the server disabled instead of enabling it immediately.
        #[arg(long)]
        disabled: bool,
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
    /// Sets one safe scalar MCP server setting.
    Set {
        /// MCP server id.
        id: String,
        /// Setting name.
        setting: String,
        /// Setting value.
        value: String,
    },
    /// Unsets one safe scalar MCP server setting.
    Unset {
        /// MCP server id.
        id: String,
        /// Setting name.
        setting: String,
    },
    /// Controls persisted MCP tool allow and deny lists.
    Tools {
        /// Tool-list subcommand.
        #[command(subcommand)]
        command: McpCliToolsCommand,
    },
    /// Controls server-level MCP tool approval behavior.
    Approval {
        /// Approval subcommand.
        #[command(subcommand)]
        command: McpCliApprovalCommand,
    },
}

/// Typed process CLI subcommands for MCP tool filters.
#[derive(Debug, Clone, Subcommand)]
enum McpCliToolsCommand {
    /// Replaces the enabled-tools allow-list.
    Enable {
        /// MCP server id.
        id: String,
        /// Tool names to allow.
        tools: Vec<String>,
    },
    /// Replaces the disabled-tools deny-list.
    Disable {
        /// MCP server id.
        id: String,
        /// Tool names to deny.
        tools: Vec<String>,
    },
    /// Clears enabled and disabled tool lists.
    Reset {
        /// MCP server id.
        id: String,
    },
}

/// Typed process CLI subcommands for server-level MCP approval settings.
#[derive(Debug, Clone, Subcommand)]
enum McpCliApprovalCommand {
    /// Sets server-level approval to inherit, prompt, allow, or deny.
    Set {
        /// MCP server id.
        id: String,
        /// Approval value.
        approval: String,
    },
    /// Removes the server-level approval override.
    Unset {
        /// MCP server id.
        id: String,
    },
}

fn mcp_cli_command_id(command: &McpConfigCommand) -> &str {
    match command {
        McpConfigCommand::Add { id, .. }
        | McpConfigCommand::Remove { id }
        | McpConfigCommand::Enable { id, .. }
        | McpConfigCommand::Set { id, .. }
        | McpConfigCommand::Unset { id, .. }
        | McpConfigCommand::ToolsEnable { id, .. }
        | McpConfigCommand::ToolsDisable { id, .. }
        | McpConfigCommand::ToolsReset { id }
        | McpConfigCommand::ApprovalSet { id, .. }
        | McpConfigCommand::ApprovalUnset { id } => id,
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

/// Runs the configured mcp servers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated subsystem rules.
fn configured_mcp_servers(effective: &EffectiveConfig) -> Vec<ConfiguredMcpServerJson> {
    configured_mcp_server_ids(effective)
        .into_iter()
        .filter_map(|id| configured_mcp_server_json(effective, &id))
        .collect()
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
fn configured_mcp_server_json(
    effective: &EffectiveConfig,
    id: &str,
) -> Option<ConfiguredMcpServerJson> {
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
        .map(config_value_array_values)
        .unwrap_or_default();
    Some(ConfiguredMcpServerJson {
        id: id.to_string(),
        name,
        enabled,
        transport: transport.to_string(),
        command: command.map(ToOwned::to_owned),
        url: url.map(ToOwned::to_owned),
        args,
        source: "primary".to_string(),
    })
}

/// Typed JSON output for `mcp list`.
#[derive(Serialize)]
struct McpListJson {
    /// Configured MCP servers visible from the primary config.
    servers: Vec<ConfiguredMcpServerJson>,
    /// Registered MCP tools visible to the CLI registry.
    tools: Vec<McpToolJson>,
}

/// Typed JSON output for `mcp inspect`.
#[derive(Serialize)]
struct McpInspectJson {
    /// One configured MCP server record.
    server: ConfiguredMcpServerJson,
}

/// Typed JSON output for one configured MCP server.
#[derive(Serialize)]
struct ConfiguredMcpServerJson {
    /// Stable configured server identifier.
    id: String,
    /// Display name for the configured server.
    name: String,
    /// Whether the configured server is enabled.
    enabled: bool,
    /// Configured transport label.
    transport: String,
    /// Optional stdio command.
    command: Option<String>,
    /// Optional streamable HTTP URL.
    url: Option<String>,
    /// Configured stdio arguments.
    args: Vec<serde_json::Value>,
    /// Origin of this configuration record.
    source: String,
}

/// Typed JSON output for one MCP tool entry.
#[derive(Serialize)]
struct McpToolJson {
    /// Stable server identifier that owns the tool.
    server_id: String,
    /// Tool name exposed by the server.
    name: String,
    /// Whether the tool is currently available.
    available: bool,
    /// Whether the tool is blacklisted.
    blacklisted: bool,
    /// Whether the tool requires approval.
    permission_required: bool,
    /// Input schema advertised by the tool.
    input_schema: serde_json::Value,
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
    /// Secret-safe auth mode name.
    auth_mode: String,
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

/// Credential material selected by `mez mcp login` before persistence.
enum McpLoginCredential {
    /// OAuth credential minted by browser authorization-code login.
    OAuth(McpOAuthCredential),
    /// Static bearer token supplied directly by the user.
    StaticBearer(String),
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

/// Stores an MCP static bearer credential with the CLI-selected credential backend.
fn login_mcp_static_bearer_credential_for_cli(
    store: &AuthStore,
    metadata: McpAuthMetadata,
    credential_store: Option<&str>,
    token: String,
) -> Result<McpAuthMetadata> {
    store.login_mcp_static_bearer_credential_with_selected_store(metadata, token, credential_store)
}

/// Renders successful MCP login output through typed JSON serialization.
fn mcp_login_json(
    id: &str,
    credential_store_name: &str,
    metadata: &McpAuthMetadata,
) -> Result<String> {
    serialize_json(&McpLoginJson {
        server_id: id.to_string(),
        auth_mode: mcp_stored_auth_mode(metadata).to_string(),
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
    } else if let Some(metadata) = status.metadata.as_ref() {
        mcp_stored_auth_mode(metadata)
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

/// Returns the secret-safe auth mode label for stored MCP credentials.
fn mcp_stored_auth_mode(metadata: &McpAuthMetadata) -> &'static str {
    match metadata.credential_kind {
        McpCredentialKind::OAuthBearer => "oauth",
        McpCredentialKind::StaticBearer => "stored-bearer",
    }
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

/// Renders MCP setting mutation output through typed JSON serialization.
fn mcp_setting_json(id: &str, changed: bool, reload_required: bool) -> Result<String> {
    mcp_mutation_json(id, changed, reload_required)
}

/// Returns the stable display name for a secret-safe credential state.
fn mcp_credential_state_name(state: &AuthCredentialState) -> &'static str {
    match state {
        AuthCredentialState::LoggedOut => "logged-out",
        AuthCredentialState::MissingSecret { .. } => "missing-secret",
        AuthCredentialState::Available { .. } => "available",
    }
}

/// Runs the config value array values operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn config_value_array_values(value: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
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
/// Runs the mcp tools operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mcp_tools(registry: &McpRegistry) -> Vec<McpToolJson> {
    registry
        .list_servers()
        .iter()
        .flat_map(|server| {
            server.tools.iter().map(|tool| McpToolJson {
                server_id: tool.server_id.clone(),
                name: tool.name.clone(),
                available: tool.available,
                blacklisted: tool.blacklisted,
                permission_required: tool.permission_required,
                input_schema: serde_json::from_str(&tool.input_schema_json)
                    .unwrap_or(serde_json::Value::Null),
            })
        })
        .collect()
}

/// Renders `mcp list` output through typed JSON serialization.
fn mcp_list_json(effective: &EffectiveConfig, registry: &McpRegistry) -> Result<String> {
    serialize_json(&McpListJson {
        servers: configured_mcp_servers(effective),
        tools: mcp_tools(registry),
    })
}

/// Renders `mcp inspect` output through typed JSON serialization.
fn mcp_inspect_json(server: ConfiguredMcpServerJson) -> Result<String> {
    serialize_json(&McpInspectJson { server })
}

#[cfg(test)]
mod tests {
    use super::{
        AuthCredentialState, ConfigFormat, ConfigLayer, ConfigScope, McpAuthBinding,
        McpAuthMetadata, McpAuthStatus, McpRegistry, compose_effective_config,
        configured_mcp_server_json, mcp_inspect_json, mcp_list_json, mcp_login_json,
        mcp_status_json,
    };
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
            r#"{"server_id":"demo","auth_mode":"oauth","authenticated":true,"metadata_present":true,"credential_store":"file","url_origin":"https://example.invalid","url_fingerprint":"sha256:def","token_expires_at":null,"scopes":["scope:read"]}"#
        );
    }

    /// Verifies MCP login/status JSON distinguishes stored static bearer tokens
    /// from OAuth credentials without exposing raw bearer material.
    #[test]
    fn mcp_static_bearer_auth_json_reports_stored_bearer_mode() {
        let binding = McpAuthBinding {
            id: "demo".to_string(),
            transport: "streamable_http",
            url: Some("https://example.invalid/mcp".to_string()),
            url_origin: Some("https://example.invalid".to_string()),
            url_fingerprint: Some("sha256:static".to_string()),
            bearer_token_env: None,
        };
        let metadata = McpAuthMetadata {
            server_id: "demo".to_string(),
            credential_kind: McpCredentialKind::StaticBearer,
            url_origin: "https://example.invalid".to_string(),
            url_fingerprint: "sha256:static".to_string(),
            scopes: Vec::new(),
            client_id: None,
            resource: None,
            authorization_endpoint: None,
            token_endpoint: None,
            credential_store_ref: Some("file:demo".to_string()),
            refresh_credential_store_ref: None,
            token_expires_at: None,
        };
        let status = McpAuthStatus {
            server_id: "demo".to_string(),
            authenticated: true,
            metadata_present: true,
            credential_state: AuthCredentialState::Available {
                store: CredentialStoreKind::PrivateFileFallback,
                reference: "file:demo".to_string(),
            },
            metadata: Some(metadata.clone()),
            stale_url: false,
        };

        let login = mcp_login_json("demo", "file", &metadata).unwrap();
        let status = mcp_status_json(&binding, &status).unwrap();

        assert_eq!(
            login,
            r#"{"server_id":"demo","auth_mode":"stored-bearer","authenticated":true,"metadata_present":true,"credential_store":"file","url_origin":"https://example.invalid","url_fingerprint":"sha256:static","token_expires_at":null,"scopes":[]}"#
        );
        assert_eq!(
            status,
            r#"{"server_id":"demo","transport":"streamable_http","auth_mode":"stored-bearer","authenticated":true,"metadata_present":true,"stale_url":false,"credential_state":"available","bearer_token_env":null,"url_origin":"https://example.invalid","url_fingerprint":"sha256:static","token_expires_at":null,"scopes":[]}"#
        );
        assert!(!login.contains("static-token"));
        assert!(!status.contains("static-token"));
    }

    /// Verifies configured bearer-token environment auth remains the reported
    /// high-precedence mode even when stored static bearer metadata exists.
    #[test]
    fn mcp_status_json_reports_env_bearer_before_stored_bearer() {
        let binding = McpAuthBinding {
            id: "demo".to_string(),
            transport: "streamable_http",
            url: Some("https://example.invalid/mcp".to_string()),
            url_origin: Some("https://example.invalid".to_string()),
            url_fingerprint: Some("sha256:static".to_string()),
            bearer_token_env: Some("MCP_TOKEN".to_string()),
        };
        let status = McpAuthStatus {
            server_id: "demo".to_string(),
            authenticated: true,
            metadata_present: true,
            credential_state: AuthCredentialState::Available {
                store: CredentialStoreKind::PrivateFileFallback,
                reference: "file:demo".to_string(),
            },
            metadata: Some(McpAuthMetadata {
                server_id: "demo".to_string(),
                credential_kind: McpCredentialKind::StaticBearer,
                url_origin: "https://example.invalid".to_string(),
                url_fingerprint: "sha256:static".to_string(),
                scopes: Vec::new(),
                client_id: None,
                resource: None,
                authorization_endpoint: None,
                token_endpoint: None,
                credential_store_ref: Some("file:demo".to_string()),
                refresh_credential_store_ref: None,
                token_expires_at: None,
            }),
            stale_url: false,
        };

        let output = mcp_status_json(&binding, &status).unwrap();

        assert!(output.contains(r#""auth_mode":"env-bearer""#));
        assert!(output.contains(r#""bearer_token_env":"MCP_TOKEN""#));
    }

    /// Verifies typed MCP list and inspect JSON preserve existing field order
    /// and compact nested server serialization for CLI consumers.
    #[test]
    fn mcp_server_json_preserves_list_and_inspect_output_shape() {
        let effective = compose_effective_config(&[ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[mcp_servers.demo]\nname = \"Demo\"\ncommand = \"demo-server\"\nargs = [\"--serve\", \"stdio\"]\nenabled = true\n".to_string(),
        }])
        .unwrap();

        let server = configured_mcp_server_json(&effective, "demo").unwrap();

        assert_eq!(
            mcp_inspect_json(server).unwrap(),
            r#"{"server":{"id":"demo","name":"Demo","enabled":true,"transport":"stdio","command":"demo-server","url":null,"args":["--serve","stdio"],"source":"primary"}}"#
        );

        assert_eq!(
            mcp_list_json(&effective, &McpRegistry::default()).unwrap(),
            r#"{"servers":[{"id":"demo","name":"Demo","enabled":true,"transport":"stdio","command":"demo-server","url":null,"args":["--serve","stdio"],"source":"primary"}],"tools":[]}"#
        );
    }
}
