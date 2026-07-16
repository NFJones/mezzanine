//! Canonical agent memory records, validation, codecs, and session state.
//!
//! This module owns storage-independent memory scope, lifecycle, record, and
//! process-local session behavior. Product adapters retain SQLite/FTS storage,
//! migrations, filesystem permissions, configured paths, and async execution.

use std::collections::BTreeMap;

mod context;
mod encoding;
mod error;
mod session_store;
mod types;
mod validation;

pub use context::{MemoryContextRecord, MemoryContextScope};
pub use error::{MemoryRecordError, MemoryRecordResult};
pub use types::{
    MemoryKind, MemoryRecord, MemoryScope, MemorySource, MemoryState, SessionMemoryStore,
};

use error::{MemoryRecordError as MezError, MemoryRecordResult as Result};
use validation::{scope_belongs_to_session, validate_non_empty, validate_scope};

pub use encoding::{
    kind_name, parse_kind, parse_model_writable_kind, parse_state, source_name, state_name,
};

#[doc(hidden)]
pub use encoding::{
    decode_scope, encode_scope, escape_component, escape_field, parse_optional_u64, parse_source,
    parse_u64, split_components, split_escaped, split_fields,
};

#[cfg(test)]
mod tests;
