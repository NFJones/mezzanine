//! Provider-independent memory action planning and result projection.
//!
//! This module owns bounded action limits, durable content shaping, idempotent
//! record identifiers, result previews, and canonical MAAP result envelopes.
//! Persistent search execution, ranking, clocks, and concrete storage remain
//! in the product adapter.

use super::MemoryRecord;
use crate::{ActionResult, AgentAction, AgentTurnResultIdentity};

/// Default number of records returned by a model-authored memory search.
pub const DEFAULT_MEMORY_ACTION_LIMIT: usize = 5;
/// Maximum number of records returned by a model-authored memory search.
pub const MAX_MEMORY_ACTION_LIMIT: usize = 20;

/// Product search result facts needed for canonical memory result projection.
#[derive(Debug, Clone, Copy)]
pub struct MemorySearchActionRecord<'a> {
    /// Canonical memory record returned by persistent retrieval.
    pub record: &'a MemoryRecord,
    /// Product retrieval score used for display and structured context.
    pub score: f64,
    /// Product retrieval reason shown to the model.
    pub reason: &'a str,
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
