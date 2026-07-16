//! Storage-independent persistent-memory retrieval contracts and ordering.
//!
//! This module owns query, result, retention, ranking, and bounded-injection
//! policy shared by SQLite adapters and agent action projection. Concrete
//! database search, migrations, and filesystem effects remain in the product.

use std::cmp::Ordering;

use super::{MemoryKind, MemoryRecord, MemoryScope, MemorySource, MemoryState};

/// Criteria used by a concrete store to search persistent memory records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemorySearchRequest {
    /// Optional FTS query. Omission selects deterministic fallback ordering.
    pub query: Option<String>,
    /// Optional exact scope filter.
    pub scope: Option<MemoryScope>,
    /// Optional memory kind filter.
    pub kind: Option<MemoryKind>,
    /// Optional lifecycle state filter.
    pub state: Option<MemoryState>,
    /// Optional source filter.
    pub source: Option<MemorySource>,
    /// Maximum number of results to return.
    pub limit: usize,
}

/// One persistent-memory search result with deterministic retrieval metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySearchResult {
    /// Matching persistent-memory record.
    pub record: MemoryRecord,
    /// Combined deterministic score used for ordering.
    pub score: f64,
    /// SQLite FTS rank when a query was provided.
    pub fts_rank: Option<f64>,
    /// Human-readable reason for retrieval and diagnostics.
    pub reason: String,
}

/// Retention policy applied by a concrete persistent-memory repository.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryRetentionPolicy {
    /// Current time used for expiry checks and archival updates.
    pub now_unix_seconds: u64,
    /// Maximum retained record count, when configured.
    pub max_records: Option<usize>,
    /// Maximum retained memory content bytes, when configured.
    pub max_bytes: Option<usize>,
    /// Archive non-expired over-limit records instead of deleting them.
    pub archive_before_prune: bool,
}

/// Request used to retrieve bounded candidates for model-facing injection.
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
    /// Maximum records fetched from the concrete store.
    pub candidate_limit: usize,
    /// Maximum records eligible for model-facing injection.
    pub injection_limit: usize,
}

impl MemoryRetrievalRequest {
    /// Builds the canonical store search request for this retrieval.
    pub fn search_request(&self) -> MemorySearchRequest {
        MemorySearchRequest {
            query: self.query.clone(),
            scope: self.scope.clone(),
            kind: self.kind,
            state: self.state.or(Some(MemoryState::Active)),
            source: self.source,
            limit: self.candidate_limit,
        }
    }
}

/// Result returned by bounded persistent-memory retrieval.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRetrievalResult {
    /// Search candidates selected for model-facing use.
    pub candidates: Vec<MemorySearchResult>,
    /// Stable reason for the retrieval path used.
    pub reason: String,
}

/// Orders search results by descending score and recency, then stable id.
pub fn compare_memory_search_results(
    left: &MemorySearchResult,
    right: &MemorySearchResult,
) -> Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            right
                .record
                .updated_at_unix_seconds
                .cmp(&left.record.updated_at_unix_seconds)
        })
        .then_with(|| left.record.id.cmp(&right.record.id))
}

/// Applies bounded injection and describes the selected retrieval path.
pub fn complete_memory_retrieval(
    mut candidates: Vec<MemorySearchResult>,
    request: &MemoryRetrievalRequest,
) -> MemoryRetrievalResult {
    candidates.sort_by(compare_memory_search_results);
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
    MemoryRetrievalResult { candidates, reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one stable record for retrieval-order tests.
    fn record(id: &str, updated_at_unix_seconds: u64) -> MemoryRecord {
        MemoryRecord::new_with_defaults(
            id,
            MemoryScope::Global,
            1,
            updated_at_unix_seconds,
            MemorySource::Agent,
            50,
            id,
        )
    }

    #[test]
    /// Verifies canonical retrieval orders score, recency, and id before
    /// applying the model-facing injection limit.
    fn memory_retrieval_orders_and_bounds_candidates() {
        let candidates = vec![
            MemorySearchResult {
                record: record("b", 2),
                score: 1.0,
                fts_rank: None,
                reason: "test".to_string(),
            },
            MemorySearchResult {
                record: record("a", 3),
                score: 1.0,
                fts_rank: None,
                reason: "test".to_string(),
            },
            MemorySearchResult {
                record: record("c", 1),
                score: 2.0,
                fts_rank: None,
                reason: "test".to_string(),
            },
        ];
        let result = complete_memory_retrieval(
            candidates,
            &MemoryRetrievalRequest {
                query: Some("release".to_string()),
                injection_limit: 2,
                ..MemoryRetrievalRequest::default()
            },
        );

        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.record.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "a"]
        );
        assert!(result.reason.contains("sqlite fts"));
    }
}
