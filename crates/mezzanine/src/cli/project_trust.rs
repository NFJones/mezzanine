//! Direct-user project trust inspection and mutation commands.
//!
//! This module owns the trust-record CLI hierarchy shared by sandbox policy
//! workflows. Configuration loading may consume trust records, but command
//! registration and persistence remain outside the configuration CLI surface.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde::Serialize;

use super::{CliOutputFormat, MezError, Result, serialize_json, write_json_or_plain};
use crate::config::ConfigPaths;
use crate::security::project::{
    ProjectTrustRecord, ProjectTrustStore, TrustDecision, default_trust_database_path,
};

/// Typed process CLI arguments for `mez sandbox trust`.
#[derive(Debug, Clone, Args)]
pub(super) struct ProjectTrustCliArgs {
    /// Optional trust subcommand, defaulting to `list`.
    #[command(subcommand)]
    command: Option<ProjectTrustCliCommand>,
}

/// Runs the sandbox project-trust command hierarchy.
pub(super) fn run_project_trust<W: Write>(
    args: ProjectTrustCliArgs,
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let trust_path = default_trust_database_path(paths.root());
    let store = ProjectTrustStore::load_from_file(&trust_path)?;
    match args.command.unwrap_or(ProjectTrustCliCommand::List) {
        ProjectTrustCliCommand::List => {
            let output = project_records_json(store.records())?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        ProjectTrustCliCommand::Inspect { root } => {
            let Some(record) = store.get(&root) else {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "project trust record not found",
                ));
            };
            let output = project_record_json(record)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        ProjectTrustCliCommand::Add { root } => {
            persist_project_trust_decision(
                &trust_path,
                root,
                TrustDecision::Trusted,
                output_format,
                stdout,
            )?;
        }
        ProjectTrustCliCommand::Reject { root } => {
            persist_project_trust_decision(
                &trust_path,
                root,
                TrustDecision::Rejected,
                output_format,
                stdout,
            )?;
        }
        ProjectTrustCliCommand::Revoke { root } => {
            persist_project_trust_decision(
                &trust_path,
                root,
                TrustDecision::Revoked,
                output_format,
                stdout,
            )?;
        }
    }
    Ok(())
}

/// Persists one project trust decision and writes the resulting record.
fn persist_project_trust_decision<W: Write>(
    trust_path: &Path,
    root: PathBuf,
    decision: TrustDecision,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let git_marker = root.join(".git");
    let git_marker = git_marker.exists().then_some(git_marker);
    let snapshot = ProjectTrustStore::update_file(trust_path, |store| {
        store.decide(root.clone(), decision, git_marker)
    })?;
    let record = snapshot.store.get(&root).ok_or_else(|| {
        MezError::new(
            crate::error::MezErrorKind::NotFound,
            "project trust record not found after decision",
        )
    })?;
    let output = project_record_json(record)?;
    write_json_or_plain(stdout, output_format, &output)?;
    Ok(())
}

/// Typed process CLI subcommands for project trust records.
#[derive(Debug, Clone, Subcommand)]
enum ProjectTrustCliCommand {
    /// Lists project trust records.
    List,
    /// Inspects one project trust record.
    Inspect {
        /// Project root path.
        root: PathBuf,
    },
    /// Adds one project root as trusted.
    Add {
        /// Project root path.
        root: PathBuf,
    },
    /// Marks one project root as rejected.
    Reject {
        /// Project root path.
        root: PathBuf,
    },
    /// Revokes one project trust record.
    Revoke {
        /// Project root path.
        root: PathBuf,
    },
}

/// Serializes all project trust records for CLI output.
fn project_records_json<'a>(
    records: impl Iterator<Item = &'a ProjectTrustRecord>,
) -> Result<String> {
    let records = records
        .map(ProjectTrustRecordJson::from)
        .collect::<Vec<_>>();
    serialize_json(&records)
}

/// Serializes one project trust record for CLI output.
fn project_record_json(record: &ProjectTrustRecord) -> Result<String> {
    serialize_json(&ProjectTrustRecordJson::from(record))
}

/// Structured JSON payload emitted for one project trust record.
#[derive(Serialize)]
struct ProjectTrustRecordJson {
    /// Canonical project root path associated with the trust record.
    project_root: String,
    /// Current trust decision label.
    state: &'static str,
    /// Canonical Git marker path used to bind trust when available.
    git_marker_path: Option<String>,
    /// Unix timestamp recording when the decision was made.
    trusted_at_unix_seconds: u64,
    /// Trust policy version recorded with the decision.
    trust_policy_version: u32,
    /// Configuration schema version recorded with the decision.
    configuration_schema_version: u32,
    /// VCS remote recorded with the decision when available.
    vcs_remote: Option<String>,
}

impl From<&ProjectTrustRecord> for ProjectTrustRecordJson {
    fn from(record: &ProjectTrustRecord) -> Self {
        Self {
            project_root: record.project_root.to_string_lossy().into_owned(),
            state: trust_decision_name(record.state),
            git_marker_path: record
                .git_marker_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            trusted_at_unix_seconds: record.trusted_at_unix_seconds,
            trust_policy_version: record.trust_policy_version,
            configuration_schema_version: record.configuration_schema_version,
            vcs_remote: record.vcs_remote.clone(),
        }
    }
}

/// Returns the stable CLI label for one project trust decision.
fn trust_decision_name(decision: TrustDecision) -> &'static str {
    match decision {
        TrustDecision::Pending => "pending",
        TrustDecision::Trusted => "trusted",
        TrustDecision::Rejected => "rejected",
        TrustDecision::Revoked => "revoked",
    }
}
