//! Persistent product adapters for canonical agent memory records.
//!
//! Canonical records, validation, line codecs, and session state live in
//! `mez_agent::memory`. Root owns SQLite/FTS storage, legacy migration,
//! configured paths, private filesystem permissions, and retrieval I/O.

use std::fs;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use crate::error::{MezError, Result};
mod permissions;
mod persistent_store;
mod retrieval;

#[cfg(test)]
pub use retrieval::retrieve_persistent_memory;

use permissions::{set_private_dir_permissions, set_private_file_permissions};

/// SQLite-backed persistent memory repository configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentMemoryStore {
    path: PathBuf,
    fts_enabled: bool,
}

#[cfg(test)]
mod tests;
