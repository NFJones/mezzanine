//! CLI commands for local project issue tracking.
//!
//! This module owns the process-level `mez issue` surface. It keeps argument
//! parsing and JSON/plain output formatting close to the shared issue store so
//! CLI behavior matches the runtime and agent action surfaces.

use super::{Args, CliEnv, CliOutputFormat, Result, Serialize, Subcommand, Write};
use crate::issues::{
    IssueKind, IssueQuery, IssueRecord, IssueStore, project_key_for_working_directory,
};

/// Runs one `mez issue` command against the configured local issue store.
pub(super) fn run_issue<W: Write>(
    parsed: IssueCliArgs,
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let paths = env.config_paths()?;
    let store = IssueStore::under_config_root(paths.root());
    let project = parsed.project.unwrap_or_else(|| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| paths.root().to_path_buf());
        project_key_for_working_directory(cwd)
    });
    let output = match parsed.command {
        IssueCliCommand::Add { kind, title, body } => {
            let record = store.add_issue(
                project,
                IssueKind::parse(&kind)?,
                title,
                body,
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
