//! CLI commands for local project issue tracking.
//!
//! This module owns the process-level `mez issue` surface. It keeps argument
//! parsing and JSON/plain output formatting close to the shared issue store so
//! CLI behavior matches the runtime and agent action surfaces.

use super::{
    Args, CliEnv, CliOutputFormat, Result, Serialize, Subcommand, Write, load_runtime_config_layers,
};
use crate::issues::{
    IssueKind, IssueQuery, IssueRecord, IssueStore, IssueUpdate, issue_database_location,
    project_key_for_working_directory,
};

/// Runs one `mez issue` command against the configured local issue store.
pub(super) fn run_issue<W: Write>(
    parsed: IssueCliArgs,
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let paths = env.config_paths()?;
    let root =
        crate::runtime::runtime_effective_config_value(&load_runtime_config_layers(&paths)?)?;
    let issues = root.get("issues").and_then(serde_json::Value::as_object);
    let issues_enabled = issues
        .and_then(|config| config.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    if !issues_enabled {
        return Err(crate::error::MezError::invalid_state(
            "issue commands require issues.enabled to be true",
        ));
    }
    let configured_database_path = issues
        .and_then(|config| config.get("database_path"))
        .and_then(serde_json::Value::as_str);
    let store = IssueStore::from_database_path(issue_database_location(
        paths.root(),
        configured_database_path,
    ));
    let project = parsed.project.unwrap_or_else(|| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| paths.root().to_path_buf());
        project_key_for_working_directory(cwd)
    });
    let output = match parsed.command {
        IssueCliCommand::Add {
            kind,
            title,
            body,
            notes,
        } => {
            let record = store.add_issue(
                project,
                IssueKind::parse(&kind)?,
                title,
                body,
                notes,
                super::current_unix_seconds()?,
            )?;
            issue_record_json(&record)?
        }
        IssueCliCommand::Query { kind, text, limit } => {
            let kind = kind.as_deref().map(IssueKind::parse).transpose()?;
            let query = IssueQuery::new(project, kind, text, limit)?;
            issue_records_json(&store.query_issues(&query)?)?
        }
        IssueCliCommand::Delete { id } => {
            let result = store.delete_issue(project, id)?;
            super::serialize_json(&IssueDeleteJson {
                project: &result.project,
                id: &result.id,
                deleted: result.deleted,
            })?
        }
        IssueCliCommand::Show { id } => match store.get_issue(project, id)? {
            Some(record) => issue_record_json(&record)?,
            None => super::serialize_json(&Option::<IssueRecordJson<'_>>::None)?,
        },
        IssueCliCommand::Update {
            id,
            kind,
            title,
            body,
            clear_body,
            notes,
            clear_notes,
        } => {
            let result = store.update_issue(
                project,
                id,
                IssueUpdate {
                    kind: kind.as_deref().map(IssueKind::parse).transpose()?,
                    title,
                    body,
                    clear_body,
                    notes,
                    clear_notes,
                },
                super::current_unix_seconds()?,
            )?;
            super::serialize_json(&IssueUpdateJson::from(&result))?
        }
    };
    super::write_json_or_plain(stdout, output_format, &output)
}

/// Typed process CLI arguments for `mez issue`.
#[derive(Debug, Clone, Args)]
pub(super) struct IssueCliArgs {
    /// Override the project key; defaults to the current git root or cwd.
    #[arg(long)]
    project: Option<String>,
    /// Issue operation to perform.
    #[command(subcommand)]
    command: IssueCliCommand,
}

/// Typed issue subcommands.
#[derive(Debug, Clone, Subcommand)]
enum IssueCliCommand {
    /// Adds one issue to the current or specified project.
    Add {
        /// Issue kind: defect or task.
        #[arg(long, default_value = "defect")]
        kind: String,
        /// Single-line issue title.
        #[arg(long, allow_hyphen_values = true)]
        title: String,
        /// Optional issue details.
        #[arg(long, allow_hyphen_values = true)]
        body: Option<String>,
        /// Optional mutable progress or handoff notes.
        #[arg(long, allow_hyphen_values = true)]
        notes: Option<String>,
    },
    /// Shows one issue by id within the current or specified project.
    Show {
        /// Issue id.
        id: String,
    },
    /// Updates one issue by id within the current or specified project.
    Update {
        /// Issue id.
        id: String,
        /// Optional replacement issue kind: defect or task.
        #[arg(long)]
        kind: Option<String>,
        /// Optional replacement single-line issue title.
        #[arg(long, allow_hyphen_values = true)]
        title: Option<String>,
        /// Optional replacement issue details.
        #[arg(long, allow_hyphen_values = true, conflicts_with = "clear_body")]
        body: Option<String>,
        /// Clear existing issue details.
        #[arg(long)]
        clear_body: bool,
        /// Optional replacement mutable progress or handoff notes.
        #[arg(long, allow_hyphen_values = true, conflicts_with = "clear_notes")]
        notes: Option<String>,
        /// Clear existing mutable progress or handoff notes.
        #[arg(long)]
        clear_notes: bool,
    },
    /// Queries issues for the current or specified project.
    Query {
        /// Optional issue kind filter: defect or task.
        #[arg(long)]
        kind: Option<String>,
        /// Optional title/body substring query.
        #[arg(long, allow_hyphen_values = true)]
        text: Option<String>,
        /// Maximum records to return.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Deletes one issue by id within the current or specified project.
    Delete {
        /// Issue id.
        id: String,
    },
}

/// JSON payload emitted for one issue record.
#[derive(Serialize)]
struct IssueRecordJson<'a> {
    /// Stable issue id.
    id: &'a str,
    /// Canonical project key.
    project: &'a str,
    /// Defect or task classification.
    kind: &'static str,
    /// Required issue title.
    title: &'a str,
    /// Optional issue body.
    body: Option<&'a str>,
    /// Optional mutable progress or handoff notes.
    notes: Option<&'a str>,
    /// Creation time as Unix seconds.
    created_at_unix_seconds: u64,
    /// Last update time as Unix seconds.
    updated_at_unix_seconds: u64,
}

impl<'a> From<&'a IssueRecord> for IssueRecordJson<'a> {
    fn from(record: &'a IssueRecord) -> Self {
        Self {
            id: &record.id,
            project: &record.project,
            kind: record.kind.as_str(),
            title: &record.title,
            body: record.body.as_deref(),
            notes: record.notes.as_deref(),
            created_at_unix_seconds: record.created_at_unix_seconds,
            updated_at_unix_seconds: record.updated_at_unix_seconds,
        }
    }
}

/// JSON payload emitted after deleting an issue.
#[derive(Serialize)]
struct IssueDeleteJson<'a> {
    /// Project key used for deletion.
    project: &'a str,
    /// Issue id targeted by deletion.
    id: &'a str,
    /// Whether a row was removed.
    deleted: bool,
}

/// JSON payload emitted after updating an issue.
#[derive(Serialize)]
struct IssueUpdateJson<'a> {
    /// Project key used for update.
    project: &'a str,
    /// Issue id targeted by update.
    id: &'a str,
    /// Whether a row was updated.
    updated: bool,
    /// Updated record when a matching issue existed.
    record: Option<IssueRecordJson<'a>>,
}

impl<'a> From<&'a crate::issues::UpdateIssueResult> for IssueUpdateJson<'a> {
    fn from(result: &'a crate::issues::UpdateIssueResult) -> Self {
        Self {
            project: &result.project,
            id: &result.id,
            updated: result.updated,
            record: result.record.as_ref().map(IssueRecordJson::from),
        }
    }
}

fn issue_record_json(record: &IssueRecord) -> Result<String> {
    super::serialize_json(&IssueRecordJson::from(record))
}

fn issue_records_json(records: &[IssueRecord]) -> Result<String> {
    let records = records
        .iter()
        .map(IssueRecordJson::from)
        .collect::<Vec<_>>();
    super::serialize_json(&records)
}
