//! Cli Memory implementation.
//!
//! This module owns the cli memory boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    Args, CliEnv, CliOutputFormat, MemoryKind, MemoryRecord, MemoryRetentionPolicy, MemoryScope,
    MemorySearchRequest, MemorySource, MemoryState, MezError, PersistentMemoryStore, Result,
    Serialize, Subcommand, Write, current_unix_seconds, load_primary_config_layers,
    runtime_effective_config_value, serialize_json, write_json_or_plain,
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

    match parsed.command.unwrap_or(MemoryCliCommand::List {
        query: None,
        scope: None,
        kind: None,
        state: None,
        source: None,
        limit: 0,
    }) {
        MemoryCliCommand::List {
            query,
            scope,
            kind,
            state,
            source,
            limit,
        } => {
            let records =
                memory_records_for_filters(&store, query, scope, kind, state, source, limit)?;
            let output = memory_records_json(&records)?;
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
            kind,
        } => {
            let scope = parse_memory_scope(&scope)?;
            let now = current_unix_seconds()?;
            let mut record = MemoryRecord::new_with_defaults(
                id.clone(),
                scope,
                now,
                now,
                MemorySource::User,
                priority,
                content,
            );
            record.kind = parse_memory_kind(&kind)?;
            store.upsert(record)?;
            let output = memory_record_json(&store.inspect(&id)?)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Edit {
            id,
            content,
            kind,
            state,
            priority,
            expires_at,
        } => {
            let mut record = store.inspect(&id)?;
            record.content = content;
            if let Some(kind) = kind.as_deref().map(parse_memory_kind).transpose()? {
                record.kind = kind;
            }
            if let Some(state) = state.as_deref().map(parse_memory_state).transpose()? {
                record.state = state;
            }
            if let Some(priority) = priority {
                record.priority = priority;
            }
            if let Some(expires_at) = expires_at {
                record.expires_at_unix_seconds = (expires_at != 0).then_some(expires_at);
            }
            record.updated_at_unix_seconds = current_unix_seconds()?;
            store.upsert(record.clone())?;
            let output = memory_record_json(&record)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Delete { id } => {
            let deleted = store.delete(&id)?;
            let output = serialize_json(&MemoryDeleteJson { deleted })?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Archive { id } => {
            let record = store.set_state(&id, MemoryState::Archived, current_unix_seconds()?)?;
            let output = memory_record_json(&record)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Stale { id } => {
            let record = store.set_state(&id, MemoryState::Stale, current_unix_seconds()?)?;
            let output = memory_record_json(&record)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Restore { id } => {
            let record = store.set_state(&id, MemoryState::Active, current_unix_seconds()?)?;
            let output = memory_record_json(&record)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Use { id } => {
            let record = store.record_use(&id, current_unix_seconds()?)?;
            let output = memory_record_json(&record)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Confirm { id } => {
            let record = store.confirm(&id, current_unix_seconds()?)?;
            let output = memory_record_json(&record)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Supersede { old_id, new_id } => {
            let record = store.supersede(&old_id, &new_id, current_unix_seconds()?)?;
            let output = memory_record_json(&record)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Prune { dry_run } => {
            let policy = memory_retention_policy(&env, current_unix_seconds()?)?;
            let records = store.enforce_retention(policy, dry_run)?;
            let output = memory_records_json(&records)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        MemoryCliCommand::Export => {
            write!(stdout, "{}", store.export_tsv()?)?;
        }
        MemoryCliCommand::Search {
            query,
            scope,
            kind,
            state,
            source,
            limit,
        } => {
            let records =
                memory_records_for_filters(&store, Some(query), scope, kind, state, source, limit)?;
            let output = memory_records_json(&records)?;
            write_json_or_plain(stdout, output_format, &output)?;
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
    List {
        /// Optional full-text search query.
        #[arg(long, allow_hyphen_values = true)]
        query: Option<String>,
        /// Optional exact memory scope filter.
        #[arg(long)]
        scope: Option<String>,
        /// Optional memory kind filter.
        #[arg(long)]
        kind: Option<String>,
        /// Optional memory lifecycle state filter.
        #[arg(long)]
        state: Option<String>,
        /// Optional memory source filter.
        #[arg(long)]
        source: Option<String>,
        /// Maximum records to return; zero uses the store default.
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
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
        /// Memory kind: preference, fact, procedure, episode, warning, or scratch.
        #[arg(long, default_value = "fact")]
        kind: String,
    },
    /// Edits the content for one memory record.
    Edit {
        /// Memory record id.
        id: String,
        /// Replacement memory content text.
        #[arg(long, allow_hyphen_values = true)]
        content: String,
        /// Replacement memory kind.
        #[arg(long)]
        kind: Option<String>,
        /// Replacement memory lifecycle state.
        #[arg(long)]
        state: Option<String>,
        /// Replacement memory priority from 0 to 255.
        #[arg(long)]
        priority: Option<u8>,
        /// Replacement expiry time as Unix seconds; zero clears expiry.
        #[arg(long)]
        expires_at: Option<u64>,
    },
    /// Deletes one memory record.
    Delete {
        /// Memory record id.
        id: String,
    },
    /// Archives one memory record so retrieval excludes it by default.
    Archive {
        /// Memory record id.
        id: String,
    },
    /// Marks one memory record stale.
    Stale {
        /// Memory record id.
        id: String,
    },
    /// Restores one memory record to active retrieval state.
    Restore {
        /// Memory record id.
        id: String,
    },
    /// Records that one memory was selected for use.
    Use {
        /// Memory record id.
        id: String,
    },
    /// Records explicit operator confirmation for one memory.
    Confirm {
        /// Memory record id.
        id: String,
    },
    /// Marks an older memory as superseded by a newer memory.
    Supersede {
        /// Older memory record id to mark superseded.
        old_id: String,
        /// Newer memory record id that replaces the older record.
        new_id: String,
    },
    /// Applies memory retention policy, or lists affected records with `--dry-run`.
    Prune {
        /// Show records that would be pruned without deleting them.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// Exports memory records as TSV.
    Export,
    /// Searches memory records with SQLite FTS and metadata filters.
    Search {
        /// Full-text query.
        #[arg(allow_hyphen_values = true)]
        query: String,
        /// Optional exact memory scope filter.
        #[arg(long)]
        scope: Option<String>,
        /// Optional memory kind filter.
        #[arg(long)]
        kind: Option<String>,
        /// Optional memory lifecycle state filter.
        #[arg(long)]
        state: Option<String>,
        /// Optional memory source filter.
        #[arg(long)]
        source: Option<String>,
        /// Maximum records to return; zero uses the store default.
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
}

/// Builds the configured persistent-memory retention policy for CLI pruning.
fn memory_retention_policy(env: &CliEnv, now_unix_seconds: u64) -> Result<MemoryRetentionPolicy> {
    let paths = env.config_paths()?;
    let layers = load_primary_config_layers(&paths)?;
    let root = runtime_effective_config_value(&layers)?;
    let memory = root.get("memory").and_then(serde_json::Value::as_object);
    Ok(MemoryRetentionPolicy {
        now_unix_seconds,
        max_records: memory_config_usize(memory, "max_records"),
        max_bytes: memory_config_usize(memory, "max_bytes"),
        archive_before_prune: memory
            .and_then(|config| config.get("archive_before_prune"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
    })
}

/// Returns one memory config integer as a usize when present and non-zero.
fn memory_config_usize(
    memory: Option<&serde_json::Map<String, serde_json::Value>>,
    key: &str,
) -> Option<usize> {
    memory
        .and_then(|config| config.get(key))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
}

/// Returns records matching CLI search and metadata filters.
fn memory_records_for_filters(
    store: &PersistentMemoryStore,
    query: Option<String>,
    scope: Option<String>,
    kind: Option<String>,
    state: Option<String>,
    source: Option<String>,
    limit: usize,
) -> Result<Vec<MemoryRecord>> {
    let has_filters = query
        .as_deref()
        .is_some_and(|query| !query.trim().is_empty())
        || scope.is_some()
        || kind.is_some()
        || state.is_some()
        || source.is_some()
        || limit > 0;
    if !has_filters {
        return store.list();
    }
    Ok(store
        .search(&MemorySearchRequest {
            query,
            scope: scope.as_deref().map(parse_memory_scope).transpose()?,
            kind: kind.as_deref().map(parse_memory_kind).transpose()?,
            state: state.as_deref().map(parse_memory_state).transpose()?,
            source: source.as_deref().map(parse_memory_source).transpose()?,
            limit,
        })?
        .into_iter()
        .map(|result| result.record)
        .collect())
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
    /// Retrieval kind label for this memory record.
    kind: &'static str,
    /// Lifecycle state label for this memory record.
    state: &'static str,
    /// Last time this memory was selected for use.
    last_used_at_unix_seconds: Option<u64>,
    /// Number of times this memory was selected for use.
    use_count: u64,
    /// Number of explicit confirmations for this memory.
    confirmed_count: u64,
    /// Last explicit confirmation timestamp.
    last_confirmed_at_unix_seconds: Option<u64>,
    /// Memory id superseded by this record, when applicable.
    supersedes_id: Option<&'a str>,
    /// Expiry time used by retention policy.
    expires_at_unix_seconds: Option<u64>,
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
            kind: memory_kind_name(record.kind),
            state: memory_state_name(record.state),
            last_used_at_unix_seconds: record.last_used_at_unix_seconds,
            use_count: record.use_count,
            confirmed_count: record.confirmed_count,
            last_confirmed_at_unix_seconds: record.last_confirmed_at_unix_seconds,
            supersedes_id: record.supersedes_id.as_deref(),
            expires_at_unix_seconds: record.expires_at_unix_seconds,
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

/// Returns the user-facing label for a memory kind.
pub(super) fn memory_kind_name(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Preference => "preference",
        MemoryKind::Fact => "fact",
        MemoryKind::Procedure => "procedure",
        MemoryKind::Episode => "episode",
        MemoryKind::Warning => "warning",
        MemoryKind::Scratch => "scratch",
    }
}

/// Parses a memory kind CLI value.
pub(super) fn parse_memory_kind(value: &str) -> Result<MemoryKind> {
    match value {
        "preference" => Ok(MemoryKind::Preference),
        "fact" => Ok(MemoryKind::Fact),
        "procedure" => Ok(MemoryKind::Procedure),
        "episode" => Ok(MemoryKind::Episode),
        "warning" => Ok(MemoryKind::Warning),
        "scratch" => Ok(MemoryKind::Scratch),
        _ => Err(MezError::invalid_args(
            "memory kind must be preference, fact, procedure, episode, warning, or scratch",
        )),
    }
}

/// Returns the user-facing label for a memory lifecycle state.
pub(super) fn memory_state_name(state: MemoryState) -> &'static str {
    match state {
        MemoryState::Active => "active",
        MemoryState::Stale => "stale",
        MemoryState::Superseded => "superseded",
        MemoryState::Archived => "archived",
        MemoryState::Expired => "expired",
    }
}

/// Parses a memory lifecycle state CLI value.
pub(super) fn parse_memory_state(value: &str) -> Result<MemoryState> {
    match value {
        "active" => Ok(MemoryState::Active),
        "stale" => Ok(MemoryState::Stale),
        "superseded" => Ok(MemoryState::Superseded),
        "archived" => Ok(MemoryState::Archived),
        "expired" => Ok(MemoryState::Expired),
        _ => Err(MezError::invalid_args(
            "memory state must be active, stale, superseded, archived, or expired",
        )),
    }
}

/// Parses a memory source CLI value.
pub(super) fn parse_memory_source(value: &str) -> Result<MemorySource> {
    match value {
        "user" => Ok(MemorySource::User),
        "agent" => Ok(MemorySource::Agent),
        "imported" => Ok(MemorySource::Imported),
        "configuration" => Ok(MemorySource::Configuration),
        _ => Err(MezError::invalid_args(
            "memory source must be user, agent, imported, or configuration",
        )),
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
