//! Memory retrieval policy and sidecar state-flow types.
//!
//! This module keeps retrieval orchestration separate from the persistent store.
//! SQLite/FTS provides candidate generation, while this layer defines the
//! deterministic fallback request/result contracts and the future sidecar
//! planner/reranker state flow. The sidecar contracts intentionally carry only
//! bounded candidate cards and selected ids; they never expose write access or
//! direct full-store access to a model.

use super::{
    MemoryKind, MemoryRecord, MemoryScope, MemorySearchRequest, MemorySearchResult, MemorySource,
    MemoryState, PersistentMemoryStore, Result,
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
    /// Search candidates selected for injection or sidecar reranking.
    pub candidates: Vec<MemorySearchResult>,
    /// Reason for the retrieval path used.
    pub reason: String,
}

/// Bounded memory representation sent to a sidecar reranker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCandidateCard {
    /// Stable memory identifier.
    pub id: String,
    /// Display scope for sidecar relevance judgment.
    pub scope: String,
    /// Memory kind label.
    pub kind: MemoryKind,
    /// Lifecycle state label.
    pub state: MemoryState,
    /// User/agent/source label.
    pub source: MemorySource,
    /// Priority used by deterministic fallback.
    pub priority: u8,
    /// Last update time as Unix seconds.
    pub updated_at_unix_seconds: u64,
    /// Number of observed uses.
    pub use_count: u64,
    /// Short content snippet; not the full store.
    pub snippet: String,
}

/// Sidecar memory retrieval state machine step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemorySidecarState {
    /// Sidecar selection is disabled and deterministic retrieval is used.
    Disabled,
    /// Sidecar is producing contextual search queries and filters.
    Planning,
    /// Runtime is validating the plan and fetching SQLite/FTS candidates.
    Fetching,
    /// Sidecar is reranking bounded candidate cards.
    Reranking,
    /// Runtime is validating selected ids and applying final policy/caps.
    Selecting,
    /// Runtime is injecting selected authoritative records into context.
    Injecting,
    /// Runtime fell back to deterministic retrieval for a specific reason.
    Fallback { reason: MemorySidecarFallbackReason },
}

/// Explicit reason a sidecar retrieval flow used deterministic fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemorySidecarFallbackReason {
    /// Config disabled sidecar memory retrieval.
    DisabledByConfig,
    /// The current turn did not contain enough query context.
    NoUsefulQueryContext,
    /// Query planning exceeded its time budget.
    PlanningTimeout,
    /// Query planning model call failed.
    PlanningModelError,
    /// Query plan failed runtime validation.
    InvalidQueryPlan,
    /// Local SQLite/FTS retrieval failed.
    RetrievalError,
    /// Local retrieval produced no candidates.
    NoCandidates,
    /// Reranking exceeded its time budget.
    RerankTimeout,
    /// Reranking model call failed.
    RerankModelError,
    /// Reranking selected ids outside the candidate set or invalid fields.
    InvalidSelection,
    /// Reranking intentionally selected no memories.
    EmptySelection,
}

/// Validated sidecar query plan for local memory retrieval.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemorySidecarPlan {
    /// FTS query strings requested by the sidecar.
    pub queries: Vec<String>,
    /// Optional exact scope filter.
    pub scope: Option<MemoryScope>,
    /// Optional memory kind filter.
    pub kind: Option<MemoryKind>,
    /// Optional lifecycle state filter.
    pub state: Option<MemoryState>,
    /// Optional source filter.
    pub source: Option<MemorySource>,
    /// Maximum candidates the runtime may fetch.
    pub candidate_limit: usize,
    /// Human-readable plan reason for debug output.
    pub reason: String,
}

/// One sidecar-selected candidate id.
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySidecarRerankSelectionItem {
    /// Selected candidate id.
    pub id: String,
    /// Sidecar confidence in this relevance judgment.
    pub confidence: f64,
    /// Short reason to expose in debug/provenance output.
    pub reason: String,
}

/// Sidecar reranking output after runtime validation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemorySidecarRerankSelection {
    /// Selected candidate ids, ordered by sidecar preference.
    pub selected: Vec<MemorySidecarRerankSelectionItem>,
    /// Optional summary for rejected candidates.
    pub rejected_summary: String,
}

/// Retrieves persistent memory using deterministic SQLite/FTS policy.
///
/// This function is the sidecar fallback path and the first-stage candidate
/// generator for future sidecar reranking.
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
    if request.injection_limit > 0 {
        candidates.truncate(request.injection_limit);
    }
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

/// Converts full search results into bounded sidecar candidate cards.
pub fn candidate_cards(
    results: &[MemorySearchResult],
    snippet_bytes: usize,
) -> Vec<MemoryCandidateCard> {
    results
        .iter()
        .map(|result| candidate_card(&result.record, snippet_bytes))
        .collect()
}

/// Converts one authoritative memory record into a bounded candidate card.
fn candidate_card(record: &MemoryRecord, snippet_bytes: usize) -> MemoryCandidateCard {
    MemoryCandidateCard {
        id: record.id.clone(),
        scope: scope_label(&record.scope),
        kind: record.kind,
        state: record.state,
        source: record.source,
        priority: record.priority,
        updated_at_unix_seconds: record.updated_at_unix_seconds,
        use_count: record.use_count,
        snippet: snippet(&record.content, snippet_bytes),
    }
}

/// Formats a scope for sidecar/debug display.
fn scope_label(scope: &MemoryScope) -> String {
    match scope {
        MemoryScope::Global => "global".to_string(),
        MemoryScope::Project { root } => format!("project:{root}"),
        MemoryScope::Session { session_id } => format!("session:{session_id}"),
        MemoryScope::Window {
            session_id,
            window_id,
        } => format!("window:{session_id}:{window_id}"),
        MemoryScope::Pane {
            session_id,
            pane_id,
        } => format!("pane:{session_id}:{pane_id}"),
        MemoryScope::Agent {
            session_id,
            agent_id,
        } => format!("agent:{session_id}:{agent_id}"),
    }
}

/// Returns a byte-bounded UTF-8-safe snippet.
fn snippet(content: &str, snippet_bytes: usize) -> String {
    if snippet_bytes == 0 || content.len() <= snippet_bytes {
        return content.to_string();
    }
    let mut end = snippet_bytes.min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &content[..end])
}
