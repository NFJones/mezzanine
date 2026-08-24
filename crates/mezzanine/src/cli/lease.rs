//! Local durable lease administration through the protected host socket.

use std::io::Write;

use clap::{Args, Subcommand, ValueEnum};

use super::{
    CliEnv, CliOutputFormat, ControlTargetSelection, MezError, Result, request_host,
    serialize_json, write_json_or_plain,
};

/// Typed process CLI arguments for `mez lease`.
#[derive(Debug, Clone, Args)]
pub(super) struct LeaseCliArgs {
    /// Durable lease administration operation.
    #[command(subcommand)]
    command: LeaseCliCommand,
}

/// Local-only durable lease operations.
#[derive(Debug, Clone, Subcommand)]
enum LeaseCliCommand {
    /// Lists durable leases with optional lifecycle and owner filters.
    List {
        /// Retains only leases in this lifecycle state.
        #[arg(long, value_enum)]
        state: Option<LeaseStateArg>,
        /// Retains only leases owned by this host trust record.
        #[arg(long, value_name = "CLIENT_ID")]
        owner: Option<String>,
        /// Includes released, revoked, and failed tombstones.
        #[arg(long)]
        all: bool,
    },
    /// Shows one lease by lease id, session id, or exact name.
    Show { target: String },
    /// Captures a generation-fenced checkpoint for one active lease.
    Checkpoint { target: String },
    /// Explicitly restores one recoverable lease from its checkpoint.
    Recover { target: String },
    /// Releases a durable reservation without revoking device trust.
    Release {
        target: String,
        /// Explicitly terminates a currently live runtime first.
        #[arg(long)]
        terminate: bool,
    },
    /// Revokes future attachment and recovery without revoking device trust.
    Revoke {
        target: String,
        /// Optional audit-safe revocation reason.
        #[arg(long)]
        reason: Option<String>,
        /// Explicitly terminates a currently live runtime first.
        #[arg(long)]
        terminate: bool,
    },
    /// Previews or applies garbage collection of terminal lease tombstones.
    Gc {
        /// Minimum terminal age (plain seconds or an s/m/h/d suffix); defaults to thirty days.
        #[arg(long, value_name = "DURATION")]
        older_than: Option<String>,
        /// Applies the previewed deletion set.
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        /// Explicitly requests preview-only behavior.
        #[arg(long)]
        dry_run: bool,
    },
}

/// CLI lifecycle filter accepted by `mez lease list`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum LeaseStateArg {
    Pending,
    Active,
    Recoverable,
    Released,
    Revoked,
    Failed,
}

impl LeaseStateArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Recoverable => "recoverable",
            Self::Released => "released",
            Self::Revoked => "revoked",
            Self::Failed => "failed",
        }
    }
}

/// Runs one local durable lease administration command.
pub(super) async fn run_lease<W: Write>(
    args: LeaseCliArgs,
    control_target: &ControlTargetSelection,
    env: &CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    if !control_target.is_unix() {
        return Err(MezError::forbidden(
            "lease administration is available only through the local host socket",
        ));
    }
    let (method, params) = match args.command {
        LeaseCliCommand::List { state, owner, all } => {
            let mut params = serde_json::json!({"all": all});
            if let Some(state) = state {
                params["state"] = serde_json::Value::String(state.as_str().to_string());
            }
            if let Some(owner) = owner {
                params["owner"] = serde_json::Value::String(owner);
            }
            ("lease/list", params)
        }
        LeaseCliCommand::Show { target } => ("lease/get", serde_json::json!({"target": target})),
        LeaseCliCommand::Checkpoint { target } => {
            ("lease/checkpoint", serde_json::json!({"target": target}))
        }
        LeaseCliCommand::Recover { target } => {
            ("lease/recover", serde_json::json!({"target": target}))
        }
        LeaseCliCommand::Release { target, terminate } => (
            "lease/release",
            serde_json::json!({"target": target, "terminate": terminate}),
        ),
        LeaseCliCommand::Revoke {
            target,
            reason,
            terminate,
        } => {
            let mut params = serde_json::json!({"target": target, "terminate": terminate});
            if let Some(reason) = reason {
                params["reason"] = serde_json::Value::String(reason);
            }
            ("lease/revoke", params)
        }
        LeaseCliCommand::Gc {
            older_than,
            apply,
            dry_run: _,
        } => {
            let mut params = serde_json::json!({"apply": apply});
            if let Some(older_than) = older_than {
                params["older_than_seconds"] =
                    serde_json::Value::from(duration_seconds(&older_than)?);
            }
            ("lease/gc", params)
        }
    };
    let result = request_host(env, method, params).await?;
    write_json_or_plain(stdout, output_format, &serialize_json(&result)?)
}

fn duration_seconds(value: &str) -> Result<u64> {
    let value = value.trim();
    if value.is_empty() {
        return Err(MezError::invalid_args(
            "lease gc duration must not be empty",
        ));
    }
    let (number, multiplier) = match value.as_bytes().last().copied() {
        Some(b's') => (&value[..value.len() - 1], 1),
        Some(b'm') => (&value[..value.len() - 1], 60),
        Some(b'h') => (&value[..value.len() - 1], 60 * 60),
        Some(b'd') => (&value[..value.len() - 1], 24 * 60 * 60),
        Some(byte) if byte.is_ascii_digit() => (value, 1),
        _ => {
            return Err(MezError::invalid_args(
                "lease gc duration must use seconds or an s, m, h, or d suffix",
            ));
        }
    };
    let number = number
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| MezError::invalid_args("lease gc duration must be positive"))?;
    number
        .checked_mul(multiplier)
        .ok_or_else(|| MezError::invalid_args("lease gc duration is too large"))
}

#[cfg(test)]
mod tests {
    use super::duration_seconds;

    #[test]
    fn gc_duration_accepts_bounded_documented_units() {
        assert_eq!(duration_seconds("90").unwrap(), 90);
        assert_eq!(duration_seconds("2m").unwrap(), 120);
        assert_eq!(duration_seconds("3h").unwrap(), 10_800);
        assert_eq!(duration_seconds("4d").unwrap(), 345_600);
        assert!(duration_seconds("0").is_err());
        assert!(duration_seconds("1w").is_err());
    }
}
