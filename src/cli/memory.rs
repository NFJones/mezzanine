//! Cli Memory implementation.
//!
//! This module owns the cli memory boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    Args, CliEnv, CliOutputFormat, MemoryRecord, MemoryScope, MemorySource, MezError,
    PersistentMemoryStore, Result, Serialize, Subcommand, Write, current_unix_seconds,
    serialize_json, write_json_or_plain,
};

// Memory subcommands and output formatting.

/// Runs the run memory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn run_memory<W: Write>(
    parsed: MemoryCliArgs,
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let paths = env.config_paths()?;
    let store = PersistentMemoryStore::under_config_root(paths.root());

    match parsed.command.unwrap_or(MemoryCliCommand::List) {
        MemoryCliCommand::List => {
            let output = memory_records_json(&store.list()?)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Inspect { id } => {
            let output = memory_record_json(&store.inspect(&id)?)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Add {
            id,
            scope,
            content,
            priority,
        } => {
            let scope = parse_memory_scope(&scope)?;
            let now = current_unix_seconds()?;
            let record = MemoryRecord {
                id: id.clone(),
                scope,
                created_at_unix_seconds: now,
                updated_at_unix_seconds: now,
                source: MemorySource::User,
                priority,
                content,
            };
            store.upsert(record)?;
            let output = memory_record_json(&store.inspect(&id)?)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Edit { id, content } => {
            let record = store.edit_content(&id, &content, current_unix_seconds()?)?;
            let output = memory_record_json(&record)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Delete { id } => {
            let deleted = store.delete(&id)?;
            let output = serialize_json(&MemoryDeleteJson { deleted })?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Export => {
            write!(stdout, "{}", store.export_tsv()?)?;
        }
    }
    Ok(())
}

/// Typed process CLI arguments for `mez memory`.
#[derive(Debug, Clone, Args)]
pub(super) struct MemoryCliArgs {
    /// Optional memory subcommand, defaulting to `list`.
    #[command(subcommand)]
    command: Option<MemoryCliCommand>,
}

/// Typed process CLI subcommands for persistent agent memory.
#[derive(Debug, Clone, Subcommand)]
enum MemoryCliCommand {
    /// Lists configured memory records.
    List,
    /// Inspects one memory record by id.
    Inspect {
        /// Memory record id.
        id: String,
    },
    /// Adds or replaces one memory record.
    Add {
        /// Memory record id.
        id: String,
        /// Memory scope, such as `global` or `project:<root>`.
        #[arg(long)]
        scope: String,
        /// Memory content text.
        #[arg(long, allow_hyphen_values = true)]
        content: String,
        /// Memory priority from 0 to 255.
        #[arg(long, default_value_t = 10)]
        priority: u8,
    },
    /// Edits the content for one memory record.
    Edit {
        /// Memory record id.
        id: String,
        /// Replacement memory content text.
        #[arg(long, allow_hyphen_values = true)]
        content: String,
    },
    /// Deletes one memory record.
    Delete {
        /// Memory record id.
        id: String,
    },
    /// Exports memory records as TSV.
    Export,
}
/// Runs the memory records json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn memory_records_json(records: &[MemoryRecord]) -> Result<String> {
    let records = records
        .iter()
        .map(MemoryRecordJson::from)
        .collect::<Vec<_>>();
    serialize_json(&records)
}

/// Runs the memory record json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn memory_record_json(record: &MemoryRecord) -> Result<String> {
    serialize_json(&MemoryRecordJson::from(record))
}

/// Structured JSON payload emitted for one persistent memory record.
#[derive(Serialize)]
struct MemoryRecordJson<'a> {
    /// Stable memory record identifier.
    id: &'a str,
    /// User-facing memory scope label.
    scope: String,
    /// Creation time as Unix seconds.
    created_at_unix_seconds: u64,
    /// Last update time as Unix seconds.
    updated_at_unix_seconds: u64,
    /// Source label for the memory record.
    source: &'static str,
    /// Memory priority from 0 to 255.
    priority: u8,
    /// Stored memory content.
    content: &'a str,
}

impl<'a> From<&'a MemoryRecord> for MemoryRecordJson<'a> {
    fn from(record: &'a MemoryRecord) -> Self {
        Self {
            id: &record.id,
            scope: memory_scope_name(&record.scope),
            created_at_unix_seconds: record.created_at_unix_seconds,
            updated_at_unix_seconds: record.updated_at_unix_seconds,
            source: memory_source_name(record.source),
            priority: record.priority,
            content: &record.content,
        }
    }
}

/// Structured JSON payload emitted when a memory delete command completes.
#[derive(Serialize)]
struct MemoryDeleteJson {
    /// Whether a record was removed.
    deleted: bool,
}

/// Runs the memory scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn memory_scope_name(scope: &MemoryScope) -> String {
    match scope {
        MemoryScope::Global => "global".to_string(),
        MemoryScope::Project { root } => format!("project:{root}"),
        MemoryScope::Session { session_id } => format!("session:{session_id}"),
        MemoryScope::Window {
            session_id,
            window_id,
        } => format!("window:{session_id}:{window_id}"),
        MemoryScope::Pane {
            session_id,
            pane_id,
        } => format!("pane:{session_id}:{pane_id}"),
        MemoryScope::Agent {
            session_id,
            agent_id,
        } => format!("agent:{session_id}:{agent_id}"),
    }
}

/// Runs the memory source name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn memory_source_name(source: MemorySource) -> &'static str {
    match source {
        MemorySource::User => "user",
        MemorySource::Agent => "agent",
        MemorySource::Imported => "imported",
        MemorySource::Configuration => "configuration",
    }
}

/// Runs the parse memory scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_memory_scope(value: &str) -> Result<MemoryScope> {
    if value == "global" {
        return Ok(MemoryScope::Global);
    }
    if let Some(root) = value.strip_prefix("project:") {
        return Ok(MemoryScope::Project {
            root: root.to_string(),
        });
    }
    if let Some(session_id) = value.strip_prefix("session:") {
        return Ok(MemoryScope::Session {
            session_id: session_id.to_string(),
        });
    }
    if let Some(rest) = value.strip_prefix("window:") {
        let (session_id, window_id) = split_memory_scope_pair(rest, "window")?;
        return Ok(MemoryScope::Window {
            session_id,
            window_id,
        });
    }
    if let Some(rest) = value.strip_prefix("pane:") {
        let (session_id, pane_id) = split_memory_scope_pair(rest, "pane")?;
        return Ok(MemoryScope::Pane {
            session_id,
            pane_id,
        });
    }
    if let Some(rest) = value.strip_prefix("agent:") {
        let (session_id, agent_id) = split_memory_scope_pair(rest, "agent")?;
        return Ok(MemoryScope::Agent {
            session_id,
            agent_id,
        });
    }
    Err(MezError::invalid_args(
        "memory scope must be global, project:<root>, session:<id>, window:<session>:<window>, pane:<session>:<pane>, or agent:<session>:<agent>",
    ))
}

/// Runs the split memory scope pair operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_memory_scope_pair(value: &str, label: &str) -> Result<(String, String)> {
    let mut parts = value.splitn(2, ':');
    let first = parts.next().unwrap_or_default();
    let second = parts.next().unwrap_or_default();
    if first.is_empty() || second.is_empty() {
        return Err(MezError::invalid_args(format!(
            "memory {label} scope requires two identifiers"
        )));
    }
    Ok((first.to_string(), second.to_string()))
}
