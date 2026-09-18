//! Cli Storage implementation.
//!
//! This module owns the operator escape hatch for converted session-state
//! stores: once a store's visible flat file disappears, `mez storage export
//! <store>` prints its rows in the legacy flat-file shape so the data stays
//! inspectable and diffable. Each store conversion registers its exporter here.

use super::{
    Args, CliEnv, CliOutputFormat, MezError, PersistentMemoryStore, Result, Subcommand, Write,
};

/// Typed process CLI arguments for `mez storage`.
#[derive(Debug, Clone, Args)]
pub(super) struct StorageCliArgs {
    /// Optional storage subcommand.
    #[command(subcommand)]
    command: Option<StorageCliCommand>,
}

/// Typed process CLI subcommands for storage maintenance.
#[derive(Debug, Clone, Subcommand)]
enum StorageCliCommand {
    /// Prints one store's rows in its legacy flat-file shape (always TSV).
    Export(StorageExportCliArgs),
}

/// Store names that have a registered legacy-shape exporter.
///
/// Each store conversion appends its store here; the error message and the
/// exporter match below both derive from this list so they cannot diverge.
pub(super) const STORAGE_EXPORTERS: &[&str] = &[
    "memory",
    "sessions",
    "leases",
    "assignments",
    "history",
    "project-trust",
    "snapshots",
];

/// Typed process CLI arguments for `mez storage export`.
#[derive(Debug, Clone, Args)]
struct StorageExportCliArgs {
    /// Store to export; unsupported names list the supported stores.
    store: String,
}

/// Runs the `mez storage` command group.
pub(super) fn run_storage<W: Write>(
    parsed: StorageCliArgs,
    env: CliEnv,
    _output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    match parsed.command {
        Some(StorageCliCommand::Export(args)) => {
            let body = storage_export_body(&args.store, &env)?;
            stdout.write_all(body.as_bytes()).map_err(|error| {
                MezError::invalid_state(format!("write storage export: {error}"))
            })?;
            Ok(())
        }
        None => Err(MezError::invalid_args(
            "storage requires a subcommand; try `mez storage export memory`",
        )),
    }
}

/// Returns one store's rows in its legacy flat-file shape.
///
/// Only stores whose SQLite conversion has landed appear here, because the
/// escape hatch exists for data whose visible flat file no longer exists. The
/// remaining stores keep their flat files until their conversion registers an
/// exporter in this match.
fn storage_export_body(store: &str, env: &CliEnv) -> Result<String> {
    match store {
        "memory" => {
            let paths = env.config_paths()?;
            let body =
                PersistentMemoryStore::under_config_root(paths.root()).export_tsv_read_only()?;
            body.ok_or_else(|| {
                MezError::invalid_state(
                    "no persistent memory store found; create one with `mez memory add` first",
                )
            })
        }
        "sessions" => {
            let selection = super::env::default_socket_selection(&env.runtime)?;
            let root = super::env::registry_root(&selection)?;
            let registry = crate::storage::registry::SessionRegistry::new(root, env.runtime.uid);
            let body = registry.export_tsv_read_only()?;
            body.ok_or_else(|| {
                MezError::invalid_state(
                    "no session registry found; start a session with `mez new` first",
                )
            })
        }
        "leases" => {
            let paths = env.config_paths()?;
            let repository = crate::storage::lease::RemoteSessionLeaseRepository::new(
                crate::storage::lease::default_remote_session_lease_directory(paths.root()),
            );
            repository.export_tsv_read_only()?.ok_or_else(|| {
                MezError::invalid_state(
                    "no remote session lease store found; a remote session creates it on its first reservation",
                )
            })
        }
        "assignments" => {
            let paths = env.config_paths()?;
            let repository =
                crate::storage::local_assignment::LocalSessionAssignmentRepository::new(
                    crate::storage::local_assignment::default_local_assignment_directory(
                        paths.root(),
                    ),
                );
            repository.export_tsv_read_only()?.ok_or_else(|| {
                MezError::invalid_state(
                    "no local session assignment store found; create one with `mez new` first",
                )
            })
        }
        "history" => {
            let paths = env.config_paths()?;
            let store =
                crate::storage::transcript::AgentTranscriptStore::under_config_root(paths.root());
            store.export_prompt_history_tsv_read_only()?.ok_or_else(|| {
                MezError::invalid_state(
                    "no prompt history store found; submit an agent prompt or run a command first",
                )
            })
        }
        "project-trust" => {
            let paths = env.config_paths()?;
            crate::security::project::ProjectTrustStore::export_database_tsv_read_only(
                &crate::security::project::default_trust_database_path(paths.root()),
            )?
            .ok_or_else(|| {
                MezError::invalid_state(
                    "no project trust database found; decide project trust first",
                )
            })
        }
        "snapshots" => {
            let paths = env.config_paths()?;
            let repository =
                crate::storage::snapshot::SnapshotRepository::new(paths.root().join("snapshots"));
            repository.export_tsv_read_only()?.ok_or_else(|| {
                MezError::invalid_state(
                    "no snapshot index found; create a snapshot with `mez snapshot create` first",
                )
            })
        }
        other => Err(MezError::invalid_args(format!(
            "unknown storage store `{other}`; supported stores: {}",
            STORAGE_EXPORTERS.join(", ")
        ))),
    }
}
