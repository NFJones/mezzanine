//! Structured agent memory stores.
//!
//! Session memory is process-local and removed with its session. Persistent
//! memory is stored as escaped records in a user-private file so callers can
//! inspect, edit, export, and delete records without provider credentials or
//! terminal transcripts leaking into an opaque database.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
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

pub use types::{
    MemoryRecord, MemoryScope, MemorySource, PersistentMemoryStore, SessionMemoryStore,
};

use encoding::{
    decode_scope, encode_scope, escape_field, parse_bool, parse_source, parse_u64, source_name,
    split_fields,
};
use validation::{
    looks_sensitive, scope_belongs_to_session, set_private_dir_permissions,
    set_private_file_permissions, validate_non_empty, validate_scope,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
