//! Local remote-transport administration through the Unix control socket.

use std::io::Write;

use clap::{Args, Subcommand, ValueEnum};

use super::{
    CliOutputFormat, Result, SocketSelection, cli_idempotency_key, json_escape, run_control_request,
};

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
        /// Invitation lifetime in seconds.
        #[arg(long = "expires", default_value_t = 600)]
        expires_seconds: u64,
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

/// Runs one local remote-administration command over authenticated Unix control.
pub(super) fn run_remote<W: Write>(
    args: RemoteCliArgs,
    socket_selection: &SocketSelection,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let (method, params) = match args.command {
        RemoteCliCommand::Status => ("remote/status", "{}".to_string()),
        RemoteCliCommand::Invite {
            role,
            expires_seconds,
        } => (
            "remote/invite",
            format!(
                r#"{{"role":"{}","expires_seconds":{},"idempotency_key":"{}"}}"#,
                role.as_str(),
                expires_seconds,
                cli_idempotency_key("remote-invite")
            ),
        ),
        RemoteCliCommand::Clients => ("remote/client/list", "{}".to_string()),
        RemoteCliCommand::Rename { client_id, label } => (
            "remote/client/rename",
            format!(
                r#"{{"client_id":"{}","label":"{}","idempotency_key":"{}"}}"#,
                json_escape(&client_id),
                json_escape(&label),
                cli_idempotency_key("remote-client-rename")
            ),
        ),
        RemoteCliCommand::Revoke { client_id, reason } => {
            let reason = reason
                .as_deref()
                .map(|reason| format!(r#", "reason":"{}""#, json_escape(reason)))
                .unwrap_or_default();
            (
                "remote/client/revoke",
                format!(
                    r#"{{"client_id":"{}"{},"idempotency_key":"{}"}}"#,
                    json_escape(&client_id),
                    reason,
                    cli_idempotency_key("remote-client-revoke")
                ),
            )
        }
    };
    run_control_request(socket_selection, method, &params, output_format, stdout)
}
