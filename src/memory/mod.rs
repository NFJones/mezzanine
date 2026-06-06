//! Structured agent memory stores.
//!
//! Session memory is process-local and removed with its session. Persistent
//! memory is stored in a user-private SQLite database with TSV import/export
//! compatibility so callers can inspect, edit, export, and delete records
//! without provider credentials or terminal transcripts leaking into an opaque
//! provider service.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};

/// Exposes the encoding module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod encoding;
/// Exposes the persistent store module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod persistent_store;
/// Exposes the retrieval module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod retrieval;
/// Exposes the session store module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod session_store;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;
/// Exposes the validation module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod validation;

pub use persistent_store::{MemorySearchRequest, MemorySearchResult};
pub use retrieval::{
    MemoryCandidateCard, MemoryRetrievalRequest, MemoryRetrievalResult,
    MemorySidecarFallbackReason, MemorySidecarPlan, MemorySidecarRerankSelection,
    MemorySidecarRerankSelectionItem, MemorySidecarState, candidate_cards,
    retrieve_persistent_memory,
};
pub use types::{
    MemoryKind, MemoryRecord, MemoryScope, MemorySource, MemoryState, PersistentMemoryStore,
    SessionMemoryStore,
};

use encoding::{
    decode_scope, encode_scope, escape_field, kind_name, parse_kind, parse_optional_u64,
    parse_source, parse_state, parse_u64, source_name, split_fields, state_name,
};
use validation::{
    scope_belongs_to_session, set_private_dir_permissions, set_private_file_permissions,
    validate_non_empty, validate_scope,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
