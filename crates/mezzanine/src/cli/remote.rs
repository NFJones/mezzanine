//! Local remote-transport administration through the Unix control socket.

use std::io::Write;
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

use super::{
    CliEnv, CliOutputFormat, MezError, Result, SocketSelection, check_iroh_profile,
    cli_idempotency_key, inspect_iroh_invitation_file, pair_iroh_invitation, request_control_body,
    request_host, serialize_json, write_control_response, write_json_or_plain,
};
use crate::security::remote::{RemoteClientProfileStore, write_remote_invitation_file_new};

/// Typed process CLI arguments for `mez remote`.
#[derive(Debug, Clone, Args)]
pub(super) struct RemoteCliArgs {
    /// Remote transport administration command.
    #[command(subcommand)]
    command: RemoteCliCommand,
}

/// Local-only remote transport administration commands.
#[derive(Debug, Clone, Subcommand)]
enum RemoteCliCommand {
    /// Shows remote transport and endpoint status.
    Status,
    /// Creates a short-lived, single-use pairing invitation.
    Invite {
        /// Maximum role granted by the invitation.
        #[arg(long, value_enum, default_value_t = RemoteInviteRole::Observer)]
        role: RemoteInviteRole,
        /// Permits this paired device to create lease-backed host sessions.
        #[arg(long)]
        allow_create: bool,
        /// Permits this paired primary device to force-kill sessions it creates.
        #[arg(long, requires = "allow_create")]
        allow_kill: bool,
        /// Maximum active leases granted to this paired device.
        #[arg(long, value_name = "N", requires = "allow_create")]
        max_leases: Option<usize>,
        /// Maximum concurrently live sessions granted to this paired device.
        #[arg(long, value_name = "N", requires = "allow_create")]
        max_live_sessions: Option<usize>,
        /// Optional invitation lifetime in seconds; the server policy supplies the default.
        #[arg(long = "expires")]
        expires_seconds: Option<u64>,
        /// Writes the invitation atomically to a new owner-only file.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
    /// Redeems an invitation and saves a profile without attaching a terminal.
    Pair {
        /// Owner-only invitation file to redeem.
        #[arg(long = "invite-file", value_name = "PATH")]
        invite_file: PathBuf,
        /// Client-local profile alias.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },
    /// Inspects an invitation without displaying its bearer token.
    Invitation {
        /// Invitation operation.
        #[command(subcommand)]
        command: RemoteInvitationCommand,
    },
    /// Manages protected client-local reconnect profiles.
    Profile {
        /// Profile operation.
        #[command(subcommand)]
        command: RemoteProfileCommand,
    },
    /// Lists paired remote clients without credentials or verifiers.
    Clients,
    /// Renames one paired remote client.
    Rename {
        /// Stable remote client record id.
        client_id: String,
        /// New display label.
        label: String,
    },
    /// Revokes one paired remote client.
    Revoke {
        /// Stable remote client record id.
        client_id: String,
        /// Optional audit-safe revocation reason.
        #[arg(long)]
        reason: Option<String>,
    },
}

/// Secret-safe invitation inspection commands.
#[derive(Debug, Clone, Subcommand)]
enum RemoteInvitationCommand {
    /// Displays expiration, role, fingerprint, and route counts without secrets.
    Inspect {
        /// Owner-only invitation file to inspect.
        path: PathBuf,
    },
}

/// Client-local reconnect profile commands.
#[derive(Debug, Clone, Subcommand)]
enum RemoteProfileCommand {
    /// Lists protected profiles without credentials.
    List,
    /// Shows one protected profile without credentials.
    Show { name: String },
    /// Renames a client-local profile alias.
    Rename {
        current_name: String,
        new_name: String,
    },
    /// Removes a local profile without revoking server-side trust.
    Remove { name: String },
    /// Authenticates one profile and reports redacted connection metadata.
    Check { name: String },
}

/// Maximum role carried by a pairing invitation.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum RemoteInviteRole {
    Observer,
    Primary,
}

impl RemoteInviteRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Primary => "primary",
        }
    }
}

/// Runs local remote administration and client-local pairing/profile commands.
pub(super) async fn run_remote<W: Write>(
    args: RemoteCliArgs,
    socket_selection: &SocketSelection,
    env: &CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    match args.command {
        RemoteCliCommand::Status => {
            let body = request_remote_administration(
                env,
                socket_selection,
                "remote/status",
                serde_json::json!({}),
            )
            .await?;
            write_control_response(stdout, output_format, &body)
        }
        RemoteCliCommand::Invite {
            role,
            allow_create,
            allow_kill,
            max_leases,
            max_live_sessions,
            expires_seconds,
            output,
        } => {
            let mut params = serde_json::json!({
                "role": role.as_str(),
                "idempotency_key": cli_idempotency_key("remote-invite"),
            });
            if allow_create {
                params["allow_create"] = serde_json::Value::Bool(true);
            }
            if allow_kill {
                if !matches!(role, RemoteInviteRole::Primary) {
                    return Err(MezError::invalid_args(
                        "--allow-kill requires --role primary",
                    ));
                }
                params["allow_kill"] = serde_json::Value::Bool(true);
            }
            if let Some(max_leases) = max_leases {
                params["max_leases"] = serde_json::Value::from(max_leases);
            }
            if let Some(max_live_sessions) = max_live_sessions {
                params["max_live_sessions"] = serde_json::Value::from(max_live_sessions);
            }
            if let Some(expires_seconds) = expires_seconds {
                params["expires_seconds"] = serde_json::Value::from(expires_seconds);
            }
            let body =
                request_remote_administration(env, socket_selection, "remote/invite", params)
                    .await?;
            if let Some(path) = output {
                write_remote_invitation_file_new(&path, body.as_bytes())?;
                let result = serialize_json(&serde_json::json!({
                    "created": true,
                    "path": path.to_string_lossy(),
                }))?;
                write_json_or_plain(stdout, output_format, &result)
            } else {
                write_control_response(stdout, output_format, &body)
            }
        }
        RemoteCliCommand::Pair { invite_file, name } => {
            let profile = pair_iroh_invitation(env, &invite_file, name.as_deref()).await?;
            let reconnect_command = format!(
                "mez --iroh-profile {} attach",
                mez_agent::shell_quote(&profile.name)
            );
            let result = serialize_json(&serde_json::json!({
                "paired": true,
                "profile": profile,
                "reconnect_command": reconnect_command,
            }))?;
            write_json_or_plain(stdout, output_format, &result)
        }
        RemoteCliCommand::Invitation {
            command: RemoteInvitationCommand::Inspect { path },
        } => {
            let result = serialize_json(&inspect_iroh_invitation_file(&path)?)?;
            write_json_or_plain(stdout, output_format, &result)
        }
        RemoteCliCommand::Profile { command } => {
            let paths = env.config_paths()?;
            let store = RemoteClientProfileStore::under_config_root(paths.root());
            let result = match command {
                RemoteProfileCommand::List => {
                    serialize_json(&serde_json::json!({ "profiles": store.list()? }))?
                }
                RemoteProfileCommand::Show { name } => {
                    let profile = require_profile_summary(&store, &name)?;
                    serialize_json(&profile)?
                }
                RemoteProfileCommand::Rename {
                    current_name,
                    new_name,
                } => serialize_json(&store.rename(&current_name, &new_name)?)?,
                RemoteProfileCommand::Remove { name } => {
                    let profile = store.remove(&name)?;
                    serialize_json(&serde_json::json!({
                        "removed": true,
                        "server_trust_revoked": false,
                        "profile": profile,
                    }))?
                }
                RemoteProfileCommand::Check { name } => {
                    let profile = check_iroh_profile(env, &name).await?;
                    serialize_json(&serde_json::json!({
                        "reachable": true,
                        "authenticated": true,
                        "profile": profile,
                    }))?
                }
            };
            write_json_or_plain(stdout, output_format, &result)
        }
        RemoteCliCommand::Clients => {
            let body = request_remote_administration(
                env,
                socket_selection,
                "remote/client/list",
                serde_json::json!({}),
            )
            .await?;
            write_control_response(stdout, output_format, &body)
        }
        RemoteCliCommand::Rename { client_id, label } => {
            let params = serde_json::json!({
                "client_id": client_id,
                "label": label,
                "idempotency_key": cli_idempotency_key("remote-client-rename"),
            });
            let body = request_remote_administration(
                env,
                socket_selection,
                "remote/client/rename",
                params,
            )
            .await?;
            write_control_response(stdout, output_format, &body)
        }
        RemoteCliCommand::Revoke { client_id, reason } => {
            let mut params = serde_json::json!({
                "client_id": client_id,
                "idempotency_key": cli_idempotency_key("remote-client-revoke"),
            });
            if let Some(reason) = reason {
                params["reason"] = serde_json::Value::String(reason);
            }
            let body = request_remote_administration(
                env,
                socket_selection,
                "remote/client/revoke",
                params,
            )
            .await?;
            write_control_response(stdout, output_format, &body)
        }
    }
}

/// Prefers the persistent host administration socket while retaining the
/// direct-session compatibility path when no host process is present.
async fn request_remote_administration(
    env: &CliEnv,
    socket_selection: &SocketSelection,
    method: &str,
    params: serde_json::Value,
) -> Result<String> {
    if !matches!(socket_selection, SocketSelection::Default(_)) {
        return request_control_body(socket_selection, method, &params.to_string());
    }
    match request_host(env, method, params.clone()).await {
        Ok(result) => Ok(serde_json::json!({
            "jsonrpc": "2.0",
            "id": "cli",
            "result": result,
        })
        .to_string()),
        Err(error)
            if matches!(
                error.io_kind(),
                Some(
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                )
            ) =>
        {
            request_control_body(socket_selection, method, &params.to_string())
        }
        Err(error) => Err(error),
    }
}

/// Loads one redacted local profile summary or returns an actionable error.
fn require_profile_summary(
    store: &RemoteClientProfileStore,
    name: &str,
) -> Result<crate::security::remote::RemoteClientProfileSummary> {
    store.summary(name)?.ok_or_else(|| {
        MezError::new(
            crate::error::MezErrorKind::NotFound,
            format!("remote client profile `{name}` was not found"),
        )
    })
}

#[cfg(test)]
mod tests {
    /// Verifies reconnect commands preserve a human-readable alias as exactly
    /// one shell argument, including whitespace and shell metacharacters.
    ///
    /// Aliases are client-local display metadata, but success output is meant
    /// to be reusable. It must not turn a valid alias into shell syntax.
    #[test]
    fn profile_reconnect_command_quotes_human_readable_alias() {
        let alias = "home mez's $(server)";
        let command = format!(
            "mez --iroh-profile {} attach",
            mez_agent::shell_quote(alias)
        );

        assert_eq!(
            shlex::split(&command).unwrap(),
            vec!["mez", "--iroh-profile", alias, "attach"]
        );
    }
}
