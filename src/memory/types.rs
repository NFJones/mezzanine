//! Memory record and store data types.
//!
//! These types model scopes, sources, records, and store containers while keeping
//! persistence and session-specific operations in sibling modules.

use super::{
    BTreeMap, MezError, PathBuf, Result, decode_scope, encode_scope, escape_field, parse_source,
    parse_u64, source_name, split_fields, validate_non_empty, validate_scope,
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
        if fields.len() != 7 {
            return Err(MezError::invalid_args(
                "memory record has wrong field count",
            ));
        }
        let record = Self {
            id: fields[0].clone(),
            scope: decode_scope(&fields[1])?,
            created_at_unix_seconds: parse_u64(&fields[2], "created_at_unix_seconds")?,
            updated_at_unix_seconds: parse_u64(&fields[3], "updated_at_unix_seconds")?,
            source: parse_source(&fields[4])?,
            priority: fields[5]
                .parse::<u8>()
                .map_err(|_| MezError::invalid_args("invalid memory priority"))?,
            content: fields[6].clone(),
        };
        record.validate_for_persistence()?;
        Ok(record)
    }
}
