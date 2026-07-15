//! Persistent product adapters for canonical agent memory records.
//!
//! Canonical records, validation, line codecs, and session state live in
//! `mez_agent::memory`. Root owns SQLite/FTS storage, legacy migration,
//! configured paths, private filesystem permissions, and retrieval I/O.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};
use mez_agent::memory::{
    MemoryKind, MemoryRecord, MemoryScope, MemorySource, MemoryState, decode_scope, encode_scope,
    kind_name, parse_kind, parse_source, parse_state, source_name, state_name,
};

mod permissions;
mod persistent_store;
mod retrieval;

pub use persistent_store::{MemoryRetentionPolicy, MemorySearchRequest, MemorySearchResult};
pub use retrieval::{MemoryRetrievalRequest, MemoryRetrievalResult, retrieve_persistent_memory};

use permissions::{set_private_dir_permissions, set_private_file_permissions};

/// SQLite-backed persistent memory repository configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentMemoryStore {
    path: PathBuf,
    fts_enabled: bool,
}

#[cfg(test)]
mod tests;
