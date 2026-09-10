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
            let output = session_catalog_status_json(&store)?;
            write_json_or_plain(stdout, output_format, &output)
        }
        SessionCatalogCliCommand::Rebuild => {
            store.rebuild_catalog(current_unix_seconds()?)?;
            let output = session_catalog_status_json(&store)?;
            write_json_or_plain(stdout, output_format, &output)
        }
    }
}

/// Serializes catalog status together with bounded objective mirror diagnostics.
///
/// The objective title mirror index is saved-session discovery metadata next to
/// the catalog, so one bounded status command reports both. A recovered mirror
/// index stays visible here: the report carries the bounded reason, the recovery
/// count, and whether the quarantined file is still retained on disk.
fn session_catalog_status_json(store: &AgentTranscriptStore) -> Result<String> {
    let mut status = catalog_status_object(&store.catalog_status())?;
    status.insert(
        "objective_mirrors".to_string(),
        serde_json::Value::Object(catalog_status_object(
            &store.session_objective_mirror_status(),
        )?),
    );
    serialize_json(&serde_json::Value::Object(status))
}

/// Serializes one bounded status report into a JSON object.
fn catalog_status_object<T: serde::Serialize>(
    value: &T,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    match serde_json::to_value(value).map_err(|error| {
        crate::error::MezError::invalid_state(format!("failed to serialize JSON: {error}"))
    })? {
        serde_json::Value::Object(object) => Ok(object),
        other => Err(crate::error::MezError::invalid_state(format!(
            "status report must serialize to a JSON object, got {other}"
        ))),
    }
}
