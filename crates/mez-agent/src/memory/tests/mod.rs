//! Storage-independent memory behavior tests.

mod codec;
mod records;
mod session_store;

use crate::memory::{MemoryRecord, MemoryScope, MemorySource};

/// Builds a canonical memory record shared across intrinsic behavior tests.
fn record(id: &str, scope: MemoryScope, content: &str) -> MemoryRecord {
    MemoryRecord::new_with_defaults(id, scope, 10, 10, MemorySource::Agent, 10, content)
}
