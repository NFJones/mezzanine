//! CLI commands for local project issue tracking.
//!
//! This module owns the process-level `mez issue` surface. It keeps argument
//! parsing and JSON/plain output formatting close to the shared issue store so
//! CLI behavior matches the runtime and agent action surfaces.

use super::{
    Args, CliEnv, CliOutputFormat, Result, Serialize, Subcommand, Write, load_runtime_config_layers,
};
use crate::storage::issues::{
    IssueStore, issue_database_location, project_key_for_working_directory,
};
use mez_agent::issues::{
    IssueKind, IssueQuery, IssueRecord, IssueState, IssueUpdate, NewIssueRecord, issue_record_json,
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
            depends_on,
        } => {
            let record = store.add_issue_with_dependencies(
                NewIssueRecord {
                    project,
                    kind: IssueKind::parse(&kind)?,
                    title,
                    body,
                    notes,
                    depends_on,
                },
                super::current_unix_seconds()?,
            )?;
            issue_record_json(&record).to_string()
        }
        IssueCliCommand::Query {
            kind,
            state,
            text,
            limit,
        } => {
            let kind = kind.as_deref().map(IssueKind::parse).transpose()?;
            let state = state.as_deref().map(IssueState::parse).transpose()?;
            let query = IssueQuery::new_with_state(
                project,
                kind,
                state.or(Some(IssueState::Open)),
                text,
                limit,
            )?;
            issue_records_json(&store.query_issues(&query)?)
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
            Some(record) => issue_record_json(&record).to_string(),
            None => serde_json::Value::Null.to_string(),
        },
        IssueCliCommand::Update {
            id,
            kind,
            state,
            title,
            body,
            clear_body,
            notes,
            clear_notes,
            depends_on,
            clear_depends_on,
        } => {
            let result = store.update_issue(
                project,
                id,
                IssueUpdate {
                    kind: kind.as_deref().map(IssueKind::parse).transpose()?,
                    state: state.as_deref().map(IssueState::parse).transpose()?,
                    title,
                    body,
                    clear_body,
                    notes,
                    clear_notes,
                    depends_on: (!depends_on.is_empty()).then_some(depends_on),
                    clear_depends_on,
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
        /// Issue ids that must be completed before this issue.
        #[arg(long = "depends-on", allow_hyphen_values = true)]
        depends_on: Vec<String>,
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
        /// Optional replacement workflow state: open or resolved.
        #[arg(long)]
        state: Option<String>,
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
        /// Replacement dependency issue ids.
        #[arg(
            long = "depends-on",
            allow_hyphen_values = true,
            conflicts_with = "clear_depends_on"
        )]
        depends_on: Vec<String>,
        /// Clear existing dependency issue ids.
        #[arg(long = "clear-depends-on")]
        clear_depends_on: bool,
    },
    /// Queries issues for the current or specified project.
    Query {
        /// Optional issue kind filter: defect or task.
        #[arg(long)]
        kind: Option<String>,
        /// Optional issue state filter: open or resolved.
        #[arg(long)]
        state: Option<String>,
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
    record: Option<serde_json::Value>,
}

impl<'a> From<&'a mez_agent::issues::UpdateIssueResult> for IssueUpdateJson<'a> {
    fn from(result: &'a mez_agent::issues::UpdateIssueResult) -> Self {
        Self {
            project: &result.project,
            id: &result.id,
            updated: result.updated,
            record: result.record.as_ref().map(issue_record_json),
        }
    }
}

fn issue_records_json(records: &[IssueRecord]) -> String {
    serde_json::Value::Array(records.iter().map(issue_record_json).collect()).to_string()
}
