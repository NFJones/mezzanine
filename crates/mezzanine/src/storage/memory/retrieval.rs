//! Memory retrieval policy types.
//!
//! This module keeps retrieval orchestration separate from the persistent store.
//! SQLite/FTS provides candidate generation, while this layer defines bounded
//! request/result contracts that callers can use for deterministic local
//! memory retrieval.

#[cfg(test)]
use super::{PersistentMemoryStore, Result};
#[cfg(test)]
use mez_agent::memory::{MemoryRetrievalRequest, MemoryRetrievalResult, complete_memory_retrieval};

/// Retrieves persistent memory using deterministic SQLite/FTS policy.
#[cfg(test)]
pub fn retrieve_persistent_memory(
    store: &PersistentMemoryStore,
    request: &MemoryRetrievalRequest,
) -> Result<MemoryRetrievalResult> {
    let candidates = store.search(&request.search_request())?;
    Ok(complete_memory_retrieval(candidates, request))
}
