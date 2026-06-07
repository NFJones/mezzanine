//! Memory record and store data types.
//!
//! These types model scopes, sources, records, and store containers while keeping
//! persistence and session-specific operations in sibling modules.

use super::{
    BTreeMap, MezError, PathBuf, Result, decode_scope, encode_scope, escape_field, kind_name,
    parse_kind, parse_optional_u64, parse_source, parse_state, parse_u64, source_name,
    split_fields, state_name, validate_non_empty, validate_scope,
};

/// Carries Memory Scope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryScope {
    /// Represents the Global case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Global,
    /// Represents the Project case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Project {
        /// Stores the root value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        root: String,
    },
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session {
        /// Stores the session id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        session_id: String,
    },
    /// Represents the Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Window {
        /// Stores the session id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        session_id: String,
        /// Stores the window id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        window_id: String,
    },
    /// Represents the Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pane {
        /// Stores the session id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        session_id: String,
        /// Stores the pane id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        pane_id: String,
    },
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent {
        /// Stores the session id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        session_id: String,
        /// Stores the agent id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        agent_id: String,
    },
}

/// Carries Memory Source state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySource {
    /// Represents the User case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    User,
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent,
    /// Represents the Imported case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Imported,
    /// Represents the Configuration case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Configuration,
}

/// Classifies the durable role a memory record plays during retrieval.
///
/// Retrieval, retention, and future sidecar planning use this type to avoid
/// applying one policy to preferences, facts, procedures, episodes, warnings,
/// and scratch notes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MemoryKind {
    /// Stable user or project preference.
    Preference,
    /// Durable factual memory.
    #[default]
    Fact,
    /// Reusable workflow or command procedure.
    Procedure,
    /// Summarized interaction or task outcome.
    Episode,
    /// Known caveat, risk, or failure mode.
    Warning,
    /// Short-lived working memory.
    Scratch,
}

/// Describes whether a memory record is eligible for retrieval and injection.
///
/// Active records are selected normally. Other states remain inspectable and
/// exportable while retrieval can demote or exclude them according to policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MemoryState {
    /// Eligible for retrieval and context injection.
    #[default]
    Active,
    /// Eligible only with a staleness penalty or explicit query.
    Stale,
    /// Retained for audit but normally replaced by a newer record.
    Superseded,
    /// Retained for operator inspection but excluded from injection.
    Archived,
    /// Ready for pruning.
    Expired,
}

/// Carries Memory Record state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecord {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the scope value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub scope: MemoryScope,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub created_at_unix_seconds: u64,
    /// Stores the updated at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub updated_at_unix_seconds: u64,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: MemorySource,
    /// Stores the priority value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub priority: u8,
    /// Stores the retrieval kind for this memory record.
    ///
    /// The field controls filtering and ranking while defaulting legacy records
    /// to fact memory during migration and TSV decoding.
    pub kind: MemoryKind,
    /// Stores the lifecycle state for this memory record.
    ///
    /// The field lets retrieval exclude archived, expired, or superseded
    /// records without deleting operator-visible history.
    pub state: MemoryState,
    /// Stores the last time this memory was selected for use, if known.
    pub last_used_at_unix_seconds: Option<u64>,
    /// Stores the number of times this memory has been selected for use.
    pub use_count: u64,
    /// Stores the number of explicit confirmations for this memory.
    pub confirmed_count: u64,
    /// Stores the last explicit confirmation time, if known.
    pub last_confirmed_at_unix_seconds: Option<u64>,
    /// Stores the memory id this record supersedes, if any.
    pub supersedes_id: Option<String>,
    /// Stores the expiry time for retention policy, if any.
    pub expires_at_unix_seconds: Option<u64>,
    /// Stores the retention duration used to refresh expiry after use.
    pub expiration_duration_seconds: Option<u64>,
    /// Stores the content value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub content: String,
}

/// Carries Session Memory Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionMemoryStore {
    /// Stores the records value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) records: BTreeMap<String, MemoryRecord>,
}

/// Carries Persistent Memory Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentMemoryStore {
    /// Stores the path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) path: PathBuf,
}

impl MemoryRecord {
    /// Runs the validate for session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate_for_session(&self) -> Result<()> {
        self.validate(false)
    }

    /// Runs the validate for persistence operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate_for_persistence(&self) -> Result<()> {
        self.validate(true)
    }

    /// Runs the with content operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_content(&self, content: impl Into<String>, updated_at_unix_seconds: u64) -> Self {
        let mut record = self.clone();
        record.content = content.into();
        record.updated_at_unix_seconds = updated_at_unix_seconds;
        record
    }

    /// Builds a record with the legacy default retrieval metadata.
    ///
    /// Existing session and CLI call sites use this constructor when the caller
    /// has not supplied kind, lifecycle, reinforcement, or retention metadata.
    pub fn new_with_defaults(
        id: impl Into<String>,
        scope: MemoryScope,
        created_at_unix_seconds: u64,
        updated_at_unix_seconds: u64,
        source: MemorySource,
        priority: u8,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            scope,
            created_at_unix_seconds,
            updated_at_unix_seconds,
            source,
            priority,
            kind: MemoryKind::Fact,
            state: MemoryState::Active,
            last_used_at_unix_seconds: None,
            use_count: 0,
            confirmed_count: 0,
            last_confirmed_at_unix_seconds: None,
            supersedes_id: None,
            expires_at_unix_seconds: None,
            expiration_duration_seconds: None,
            content: content.into(),
        }
    }

    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self, persistent: bool) -> Result<()> {
        validate_non_empty("memory id", &self.id)?;
        if self.created_at_unix_seconds == 0 || self.updated_at_unix_seconds == 0 {
            return Err(MezError::invalid_args(
                "memory timestamps must be non-zero unix seconds",
            ));
        }
        if self.updated_at_unix_seconds < self.created_at_unix_seconds {
            return Err(MezError::invalid_args(
                "memory update time must not precede creation time",
            ));
        }
        validate_scope(&self.scope)?;
        if let Some(last_used_at) = self.last_used_at_unix_seconds
            && last_used_at == 0
        {
            return Err(MezError::invalid_args(
                "memory last used time must be non-zero unix seconds",
            ));
        }
        if let Some(last_confirmed_at) = self.last_confirmed_at_unix_seconds
            && last_confirmed_at == 0
        {
            return Err(MezError::invalid_args(
                "memory last confirmed time must be non-zero unix seconds",
            ));
        }
        if let Some(expires_at) = self.expires_at_unix_seconds
            && expires_at == 0
        {
            return Err(MezError::invalid_args(
                "memory expiry time must be non-zero unix seconds",
            ));
        }
        if let Some(duration) = self.expiration_duration_seconds
            && duration == 0
        {
            return Err(MezError::invalid_args(
                "memory expiration duration must be non-zero seconds",
            ));
        }
        if let Some(supersedes_id) = &self.supersedes_id {
            validate_non_empty("superseded memory id", supersedes_id)?;
        }
        validate_non_empty("memory content", &self.content)?;
        let _ = persistent;
        Ok(())
    }

    /// Runs the encode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn encode(&self) -> Result<String> {
        self.validate_for_persistence()?;
        Ok([
            self.id.clone(),
            encode_scope(&self.scope),
            self.created_at_unix_seconds.to_string(),
            self.updated_at_unix_seconds.to_string(),
            source_name(self.source).to_string(),
            self.priority.to_string(),
            kind_name(self.kind).to_string(),
            state_name(self.state).to_string(),
            optional_u64_field(self.last_used_at_unix_seconds),
            self.use_count.to_string(),
            self.confirmed_count.to_string(),
            optional_u64_field(self.last_confirmed_at_unix_seconds),
            self.supersedes_id.clone().unwrap_or_default(),
            optional_u64_field(self.expires_at_unix_seconds),
            optional_u64_field(self.expiration_duration_seconds),
            self.content.clone(),
        ]
        .into_iter()
        .map(|field| escape_field(&field))
        .collect::<Vec<_>>()
        .join("\t"))
    }

    /// Runs the decode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn decode(line: &str) -> Result<Self> {
        let fields = split_fields(line)?;
        if fields.len() != 7 && fields.len() != 15 && fields.len() != 16 {
            return Err(MezError::invalid_args(
                "memory record has wrong field count",
            ));
        }
        let mut record = Self::new_with_defaults(
            fields[0].clone(),
            decode_scope(&fields[1])?,
            parse_u64(&fields[2], "created_at_unix_seconds")?,
            parse_u64(&fields[3], "updated_at_unix_seconds")?,
            parse_source(&fields[4])?,
            fields[5]
                .parse::<u8>()
                .map_err(|_| MezError::invalid_args("invalid memory priority"))?,
            fields.last().cloned().unwrap_or_default(),
        );
        if fields.len() == 15 {
            record.kind = parse_kind(&fields[6])?;
            record.state = parse_state(&fields[7])?;
            record.last_used_at_unix_seconds =
                parse_optional_u64(&fields[8], "last_used_at_unix_seconds")?;
            record.use_count = parse_u64(&fields[9], "use_count")?;
            record.confirmed_count = parse_u64(&fields[10], "confirmed_count")?;
            record.last_confirmed_at_unix_seconds =
                parse_optional_u64(&fields[11], "last_confirmed_at_unix_seconds")?;
            record.supersedes_id = optional_string_field(&fields[12]);
            record.expires_at_unix_seconds =
                parse_optional_u64(&fields[13], "expires_at_unix_seconds")?;
            if fields.len() == 16 {
                record.expiration_duration_seconds =
                    parse_optional_u64(&fields[14], "expiration_duration_seconds")?;
            }
        }
        record.validate_for_persistence()?;
        Ok(record)
    }
}

/// Formats an optional integer field for the TSV export format.
fn optional_u64_field(value: Option<u64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

/// Parses an optional string field from the TSV export format.
fn optional_string_field(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}
