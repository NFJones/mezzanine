//! Provider-independent memory action planning and result projection.
//!
//! This module owns bounded action limits, durable content shaping, idempotent
//! record identifiers, result previews, and canonical MAAP result envelopes.
//! Persistent search execution, ranking, clocks, and concrete storage remain
//! in the product adapter.

use super::{
    MemoryRecord, MemoryRecordResult, MemoryScope, MemorySource, parse_model_writable_kind,
};
use crate::{ActionResult, AgentAction, AgentTurnResultIdentity};

/// Default number of records returned by a model-authored memory search.
pub const DEFAULT_MEMORY_ACTION_LIMIT: usize = 5;
/// Maximum number of records returned by a model-authored memory search.
pub const MAX_MEMORY_ACTION_LIMIT: usize = 20;

/// Product search result facts needed for canonical memory result projection.
#[derive(Debug, Clone)]
pub struct MemorySearchActionRecord<'a> {
    /// Canonical memory record returned by persistent retrieval.
    pub record: &'a MemoryRecord,
    /// Product retrieval score used for display and structured context.
    pub score: f64,
    /// Product retrieval reason shown to the model.
    pub reason: &'a str,
}

/// Deterministic facts needed to build one model-authored memory record.
///
/// The product supplies the resolved durable scope, current time, and
/// configured default retention. This crate owns normalization and record
/// construction so concrete stores do not duplicate agent policy.
#[derive(Debug, Clone)]
pub struct MemoryStoreRecordRequest<'a> {
    /// Model-authored canonical memory kind name.
    pub kind: &'a str,
    /// Optional model-authored priority before canonical bounding.
    pub priority: Option<u64>,
    /// Product-resolved durable scope.
    pub scope: MemoryScope,
    /// Optional keyword anchors appended to durable content.
    pub keywords: &'a [String],
    /// Model-authored durable content.
    pub content: &'a str,
    /// Optional action-specific retention period in days.
    pub expires_in_days: Option<u64>,
    /// Product clock value used for creation, update, and expiry.
    pub now_unix_seconds: u64,
    /// Configured fallback retention period in days.
    pub default_ttl_days: u64,
}

/// Returns the bounded search result limit for a memory action.
pub fn memory_action_limit(limit: Option<u64>) -> usize {
    limit
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_MEMORY_ACTION_LIMIT)
        .clamp(1, MAX_MEMORY_ACTION_LIMIT)
}

/// Builds durable memory content with optional normalized keyword anchors.
pub fn memory_action_content(content: &str, keywords: &[String]) -> String {
    let keywords = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| !keyword.is_empty())
        .collect::<Vec<_>>();
    if keywords.is_empty() {
        content.to_string()
    } else {
        format!("{}\n\nKeywords: {}", content.trim(), keywords.join(", "))
    }
}

/// Builds a stable idempotent record identifier for one memory-store action.
pub fn memory_action_record_id(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
) -> String {
    format!("agent:{}:{}", turn.turn_id(), action.id)
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Builds one canonical persistent record from a model-authored store action.
///
/// Returns a validation error when the kind is not model-writable or an
/// action-specific retention duration cannot be represented in seconds.
pub fn memory_store_record(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    request: MemoryStoreRecordRequest<'_>,
) -> MemoryRecordResult<MemoryRecord> {
    let priority = request.priority.unwrap_or(50).min(100) as u8;
    let mut record = MemoryRecord::new_with_defaults(
        memory_action_record_id(turn, action),
        request.scope,
        request.now_unix_seconds,
        request.now_unix_seconds,
        MemorySource::Agent,
        priority,
        memory_action_content(request.content, request.keywords),
    );
    record.kind = parse_model_writable_kind(request.kind).map_err(|_| {
        super::MemoryRecordError::invalid_args(
            "memory_store kind must be preference, fact, procedure, documentation, research, or warning",
        )
    })?;
    let expiration_seconds = if let Some(days) = request.expires_in_days {
        Some(days.checked_mul(86_400).ok_or_else(|| {
            super::MemoryRecordError::invalid_args("memory expires_in_days is too large to store")
        })?)
    } else {
        request
            .default_ttl_days
            .checked_mul(86_400)
            .filter(|seconds| *seconds > 0)
    };
    if let Some(seconds) = expiration_seconds {
        record.expiration_duration_seconds = Some(seconds);
        record.expires_at_unix_seconds = Some(
            request
                .now_unix_seconds
                .checked_add(seconds)
                .ok_or_else(|| {
                    super::MemoryRecordError::invalid_args(if request.expires_in_days.is_some() {
                        "memory expiration timestamp is too large to store"
                    } else {
                        "memory default_ttl_days is too large to store"
                    })
                })?,
        );
    }
    Ok(record)
}

/// Returns a bounded single-line preview for memory action output.
pub fn memory_action_preview(content: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 160;
    let text = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= MAX_PREVIEW_CHARS {
        return text;
    }
    let mut preview = text.chars().take(MAX_PREVIEW_CHARS).collect::<String>();
    preview.push('…');
    preview
}

/// Builds the canonical successful result for a persistent memory search.
pub fn memory_search_action_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    query: &str,
    results: &[MemorySearchActionRecord<'_>],
) -> ActionResult {
    let content = if results.is_empty() {
        vec!["memory_search returned 0 records".to_string()]
    } else {
        results
            .iter()
            .map(|result| {
                format!(
                    "{} score={:.3}: {}",
                    result.record.id,
                    result.score,
                    memory_action_preview(&result.record.content)
                )
            })
            .collect()
    };
    let records = results
        .iter()
        .map(|result| {
            serde_json::json!({
                "id": result.record.id,
                "scope": format!("{:?}", result.record.scope),
                "kind": format!("{:?}", result.record.kind),
                "priority": result.record.priority,
                "score": result.score,
                "reason": result.reason,
                "content": result.record.content,
            })
        })
        .collect::<Vec<_>>();
    ActionResult::succeeded(
        turn,
        action,
        content,
        Some(
            serde_json::json!({
                "query": query,
                "count": results.len(),
                "results": records,
            })
            .to_string(),
        ),
    )
}

/// Builds the canonical successful result for a persistent memory store.
pub fn memory_store_action_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    record: &MemoryRecord,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![format!("stored memory {}", record.id)],
        Some(
            serde_json::json!({
                "id": record.id,
                "scope": format!("{:?}", record.scope),
                "kind": format!("{:?}", record.kind),
                "priority": record.priority,
                "expires_at_unix_seconds": record.expires_at_unix_seconds,
            })
            .to_string(),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryKind, MemoryScope, MemorySource, MemoryState};
    use crate::{AgentActionPayload, AgentTurnRecord, AgentTurnState, AgentTurnTrigger};

    /// Builds one active turn fixture for memory result projection.
    fn turn() -> AgentTurnRecord {
        AgentTurnRecord {
            turn_id: "turn/1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "%1".to_string(),
            trigger: AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: AgentTurnState::Running,
            cooperation_mode: None,
            initial_capability: None,
        }
    }

    /// Builds one memory-search action fixture.
    fn action() -> AgentAction {
        AgentAction {
            id: "search/1".to_string(),
            rationale: "Recall project facts".to_string(),
            payload: AgentActionPayload::MemorySearch {
                query: "decomposition".to_string(),
                limit: None,
            },
        }
    }

    /// Builds one canonical memory record fixture.
    fn record() -> MemoryRecord {
        MemoryRecord {
            id: "memory-1".to_string(),
            scope: MemoryScope::Project {
                root: "/repo".to_string(),
            },
            kind: MemoryKind::Fact,
            source: MemorySource::Agent,
            state: MemoryState::Active,
            priority: 100,
            created_at_unix_seconds: 1,
            updated_at_unix_seconds: 2,
            last_used_at_unix_seconds: None,
            use_count: 0,
            confirmed_count: 0,
            last_confirmed_at_unix_seconds: None,
            expires_at_unix_seconds: None,
            supersedes_id: None,
            expiration_duration_seconds: None,
            content: "The decomposition plan remains active.".to_string(),
        }
    }

    /// Verifies limit and content planning bound model inputs while retaining
    /// normalized keyword anchors in durable content.
    #[test]
    fn memory_action_planning_bounds_limits_and_keywords() {
        assert_eq!(memory_action_limit(None), DEFAULT_MEMORY_ACTION_LIMIT);
        assert_eq!(memory_action_limit(Some(0)), 1);
        assert_eq!(memory_action_limit(Some(500)), MAX_MEMORY_ACTION_LIMIT);
        assert_eq!(
            memory_action_content(
                "  Durable fact  ",
                &[" decomposition ".to_string(), String::new()]
            ),
            "Durable fact\n\nKeywords: decomposition"
        );
        assert_eq!(
            memory_action_record_id(&turn(), &action()),
            "agent:turn-1:search-1"
        );
    }

    /// Verifies memory-store planning applies canonical kind, priority,
    /// content, scope, and configured-retention policy in the lower crate.
    #[test]
    fn memory_store_planning_builds_canonical_record() {
        let action = action();
        let record = memory_store_record(
            &turn(),
            &action,
            MemoryStoreRecordRequest {
                kind: "procedure",
                priority: Some(500),
                scope: MemoryScope::Project {
                    root: "/repo".to_string(),
                },
                keywords: &[" release ".to_string()],
                content: "Run the release checks",
                expires_in_days: None,
                now_unix_seconds: 10,
                default_ttl_days: 2,
            },
        )
        .unwrap();

        assert_eq!(record.id, "agent:turn-1:search-1");
        assert_eq!(record.kind, MemoryKind::Procedure);
        assert_eq!(record.priority, 100);
        assert_eq!(
            record.content,
            "Run the release checks\n\nKeywords: release"
        );
        assert_eq!(record.expiration_duration_seconds, Some(172_800));
        assert_eq!(record.expires_at_unix_seconds, Some(172_810));
    }

    /// Verifies malformed kinds and unrepresentable action retention are
    /// rejected before the concrete store receives a record.
    #[test]
    fn memory_store_planning_rejects_invalid_model_inputs() {
        let action = action();
        let request = MemoryStoreRecordRequest {
            kind: "episode",
            priority: None,
            scope: MemoryScope::Global,
            keywords: &[],
            content: "invalid",
            expires_in_days: None,
            now_unix_seconds: 10,
            default_ttl_days: 0,
        };
        assert_eq!(
            memory_store_record(&turn(), &action, request.clone())
                .unwrap_err()
                .message(),
            "memory_store kind must be preference, fact, procedure, documentation, research, or warning"
        );

        assert_eq!(
            memory_store_record(
                &turn(),
                &action,
                MemoryStoreRecordRequest {
                    kind: "fact",
                    expires_in_days: Some(u64::MAX),
                    ..request
                },
            )
            .unwrap_err()
            .message(),
            "memory expires_in_days is too large to store"
        );
    }

    /// Verifies search result projection preserves retrieval facts and bounded
    /// model-visible summaries without storage-adapter dependencies.
    #[test]
    fn memory_search_projection_preserves_retrieval_facts() {
        let record = record();
        let result = memory_search_action_result(
            &turn(),
            &action(),
            "decomposition",
            &[MemorySearchActionRecord {
                record: &record,
                score: 0.75,
                reason: "semantic match",
            }],
        );
        let structured: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();

        assert!(result.content_text().contains("memory-1 score=0.750"));
        assert_eq!(structured["count"], 1);
        assert_eq!(structured["results"][0]["reason"], "semantic match");
    }
}
