//! Saved-session catalog status and recovery commands.
//!
//! These commands operate directly on the user-private catalog beneath the
//! configuration root. Status is bounded and read-only; rebuild is the only
//! operator command that intentionally scans retained session payloads.

use std::io::Write;

use clap::{Args, Subcommand};

use super::{
    CliEnv, CliOutputFormat, Result, current_unix_seconds, serialize_json, write_json_or_plain,
};
use crate::storage::transcript::AgentTranscriptStore;

/// Typed arguments for `mez session-catalog`.
#[derive(Debug, Clone, Args)]
pub(super) struct SessionCatalogCliArgs {
    /// Catalog administration operation.
    #[command(subcommand)]
    command: SessionCatalogCliCommand,
}

/// Supported saved-session catalog administration operations.
#[derive(Debug, Clone, Subcommand)]
enum SessionCatalogCliCommand {
    /// Reports bounded schema, integrity, lock, and recovery status.
    Status,
    /// Rebuilds discovery metadata from retained session files.
    Rebuild,
}

/// Runs one saved-session catalog administration command.
pub(super) fn run_session_catalog<W: Write>(
    args: SessionCatalogCliArgs,
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let paths = env.config_paths()?;
    let store = AgentTranscriptStore::under_config_root(paths.root());
    match args.command {
        SessionCatalogCliCommand::Status => {
            let output = serialize_json(&store.catalog_status())?;
            write_json_or_plain(stdout, output_format, &output)
        }
        SessionCatalogCliCommand::Rebuild => {
            store.rebuild_catalog(current_unix_seconds()?)?;
            let output = serialize_json(&store.catalog_status())?;
            write_json_or_plain(stdout, output_format, &output)
        }
    }
}
