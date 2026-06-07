//! Memory retrieval policy types.
//!
//! This module keeps retrieval orchestration separate from the persistent store.
//! SQLite/FTS provides candidate generation, while this layer defines bounded
//! request/result contracts that callers can use for deterministic local
//! memory retrieval.

use super::{
    MemoryKind, MemoryScope, MemorySearchRequest, MemorySearchResult, MemorySource, MemoryState,
    PersistentMemoryStore, Result,
};

/// Request used by context assembly to retrieve relevant memory candidates.
///
/// The request is storage-independent: callers provide optional query and
/// metadata filters plus candidate/injection limits, and the retrieval layer
/// delegates concrete search to the persistent store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryRetrievalRequest {
    /// Optional query text used for FTS candidate generation.
    pub query: Option<String>,
    /// Optional exact scope filter.
    pub scope: Option<MemoryScope>,
    /// Optional memory kind filter.
    pub kind: Option<MemoryKind>,
    /// Optional lifecycle state filter.
    pub state: Option<MemoryState>,
    /// Optional source filter.
    pub source: Option<MemorySource>,
    /// Maximum records to fetch from the underlying store.
    pub candidate_limit: usize,
    /// Maximum records eligible for injection after ranking.
    pub injection_limit: usize,
}

/// Result returned by deterministic memory retrieval.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRetrievalResult {
    /// Search candidates selected for model-facing use.
    pub candidates: Vec<MemorySearchResult>,
    /// Reason for the retrieval path used.
    pub reason: String,
}

/// Retrieves persistent memory using deterministic SQLite/FTS policy.
pub fn retrieve_persistent_memory(
    store: &PersistentMemoryStore,
    request: &MemoryRetrievalRequest,
) -> Result<MemoryRetrievalResult> {
    let search_request = MemorySearchRequest {
        query: request.query.clone(),
        scope: request.scope.clone(),
        kind: request.kind,
        state: request.state.or(Some(MemoryState::Active)),
        source: request.source,
        limit: request.candidate_limit,
    };
    let mut candidates = store.search(&search_request)?;
    candidates.truncate(request.injection_limit);
    let reason = if request
        .query
        .as_deref()
        .is_some_and(|query| !query.trim().is_empty())
    {
        "sqlite fts retrieval with deterministic metadata ranking".to_string()
    } else {
        "deterministic metadata fallback retrieval".to_string()
    };
    Ok(MemoryRetrievalResult { candidates, reason })
}
