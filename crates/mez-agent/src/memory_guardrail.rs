//! Per-turn persistent-memory action guardrails.
//!
//! This module owns deterministic memory-search budgeting and rejection of
//! model actions emitted only to satisfy imagined tool-wrapper requirements.
//! It reconstructs active-turn usage from canonical model context and returns
//! canonical skipped action results without depending on memory persistence or
//! runtime dispatch.

use crate::{
    ActionResult, AgentAction, AgentActionPayload, AgentContext, AgentTurnResultIdentity,
    ContextSourceKind,
};

/// Maximum memory searches accepted during one user turn.
///
/// Memory is durable prior context, not a route-discovery fallback. Keeping the
/// runtime cap small gives the model room for one focused search plus one
/// exceptional follow-up while preventing paraphrase loops.
const MEMORY_SEARCH_ACTION_LIMIT_PER_TURN: usize = 2;

/// Tracks persistent-memory action use during one active agent turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryActionBudget {
    /// Number of memory search actions already accepted or observed in the
    /// active turn context.
    search_count: usize,
}

impl MemoryActionBudget {
    /// Builds the memory action budget from current model context.
    ///
    /// Only action-result blocks are counted because they represent memory
    /// actions that have already been surfaced to the model during this active
    /// work loop.
    pub fn from_context(context: &AgentContext) -> Self {
        let mut budget = Self::default();
        for block in context.blocks() {
            if block.source != ContextSourceKind::ActionResult {
                continue;
            }
            if action_result_block_has_action_type(&block.content, "memory_search") {
                budget.search_count = budget.search_count.saturating_add(1);
            }
        }
        budget
    }

    /// Accepts a memory action against the turn budget or returns a skipped result.
    ///
    /// Non-memory actions do not consume this budget. Over-budget memory
    /// actions become successful skipped results so unrelated useful actions in
    /// the same batch are not blocked by the guardrail.
    pub fn accept_or_skip(
        &mut self,
        turn: &(impl AgentTurnResultIdentity + ?Sized),
        action: &AgentAction,
        batch_rationale: &str,
        batch_thought: Option<&str>,
    ) -> Option<ActionResult> {
        if memory_action_is_wrapper_placeholder(action, batch_rationale, batch_thought) {
            return Some(memory_budget_skip_result(
                turn,
                action,
                "memory_wrapper_placeholder",
                "memory action skipped: rationale identified this as action-wrapper compliance rather than a concrete durable-context need; continue with the direct task action instead",
                0,
            ));
        }
        match &action.payload {
            AgentActionPayload::MemorySearch { .. } => {
                if self.search_count >= MEMORY_SEARCH_ACTION_LIMIT_PER_TURN {
                    return Some(memory_budget_skip_result(
                        turn,
                        action,
                        "memory_search_turn_limit",
                        "memory_search skipped: per-turn memory search limit reached; continue the task with direct artifacts, current action results, MCP, shell, web, or a bounded report instead, and do not search memory again this turn",
                        MEMORY_SEARCH_ACTION_LIMIT_PER_TURN,
                    ));
                }
                self.search_count = self.search_count.saturating_add(1);
                None
            }
            AgentActionPayload::MemoryStore { .. } => None,
            _ => None,
        }
    }
}

/// Reports whether a model-context action-result block has the supplied action type.
fn action_result_block_has_action_type(content: &str, action_type: &str) -> bool {
    let Some(header) = content.lines().next() else {
        return false;
    };
    header.starts_with("[action_result ") && header.split_whitespace().nth(2) == Some(action_type)
}

/// Builds the structured skipped result returned for an over-budget memory action.
fn memory_budget_skip_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    code: &str,
    message: &str,
    limit: usize,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![message.to_string()],
        Some(
            serde_json::json!({
                "state": "skipped_runtime_memory_guardrail",
                "code": code,
                "limit": limit,
                "message": message,
            })
            .to_string(),
        ),
    )
}

/// Reports whether a memory action is only satisfying the function/action wrapper.
///
/// The runtime should keep memory available when it is the right tool, but
/// should not let a model convert wrapper-compliance confusion into a durable
/// memory lookup or store. The match requires both wrapper terminology and
/// compliance/setup language so ordinary memory searches about prompt behavior
/// are not rejected merely for mentioning a function call.
fn memory_action_is_wrapper_placeholder(
    action: &AgentAction,
    batch_rationale: &str,
    batch_thought: Option<&str>,
) -> bool {
    if !matches!(
        action.payload,
        AgentActionPayload::MemorySearch { .. } | AgentActionPayload::MemoryStore { .. }
    ) {
        return false;
    }
    let mut text = String::new();
    text.push_str(batch_rationale);
    text.push('\n');
    if let Some(batch_thought) = batch_thought {
        text.push_str(batch_thought);
        text.push('\n');
    }
    text.push_str(&action.rationale);
    memory_placeholder_text_mentions_wrapper_compliance(&text)
}

/// Reports whether text frames an action as wrapper compliance instead of work.
fn memory_placeholder_text_mentions_wrapper_compliance(text: &str) -> bool {
    let normalized = normalize_memory_placeholder_text(text);
    let mentions_wrapper = [
        "required function call",
        "required tool call",
        "required current actions call",
        "required current action call",
        "current actions call",
        "schema wrapper",
        "action wrapper",
        "function call requirement",
        "schema valid batch",
        "action batch envelope",
        "transport envelope",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase));
    if !mentions_wrapper {
        return false;
    }
    [
        "comply",
        "complying",
        "satisfy",
        "satisfying",
        "satisfies",
        "placeholder",
        "prerequisite",
        "before proceeding",
        "required immediate",
        "schema valid batch is needed",
        "initial batch is needed",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase))
}

/// Normalizes model-authored rationale text for guardrail phrase matching.
fn normalize_memory_placeholder_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut previous_space = true;
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            output.push(character);
            previous_space = false;
        } else if !previous_space {
            output.push(' ');
            previous_space = true;
        }
    }
    output.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentTurnResultIdentity, ContextBlock};

    /// Stable synthetic identity for guardrail-produced action results.
    struct TestTurn;

    impl AgentTurnResultIdentity for TestTurn {
        fn turn_id(&self) -> &str {
            "turn-1"
        }

        fn agent_id(&self) -> &str {
            "agent-1"
        }
    }

    /// Builds one memory search with a configurable rationale.
    fn memory_search(id: &str, rationale: &str) -> AgentAction {
        AgentAction {
            id: id.to_string(),
            rationale: rationale.to_string(),
            payload: AgentActionPayload::MemorySearch {
                query: "durable context".to_string(),
                limit: Some(1),
            },
        }
    }

    /// Builds a valid context with no prior action-result budget usage.
    fn context_without_prior_searches() -> AgentContext {
        AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "continue the task".to_string(),
        }])
        .unwrap()
    }

    #[test]
    /// Verifies prior memory-search action results reconstruct the active-turn
    /// budget and cause the next search to return the canonical successful skip
    /// result once the two-search ceiling is already exhausted.
    fn memory_action_budget_reconstructs_prior_searches_from_context() {
        let context = AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "first".to_string(),
                content: "[action_result memory-1 memory_search succeeded]".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "second".to_string(),
                content: "[action_result memory-2 memory_search succeeded]".to_string(),
            },
        ])
        .unwrap();
        let mut budget = MemoryActionBudget::from_context(&context);

        let result = budget
            .accept_or_skip(
                &TestTurn,
                &memory_search("memory-3", "search prior context"),
                "",
                None,
            )
            .expect("third memory search should be skipped");

        assert_eq!(result.status, crate::ActionStatus::Succeeded);
        assert!(
            result
                .structured_content_json
                .as_deref()
                .is_some_and(|content| content.contains("memory_search_turn_limit"))
        );
    }

    #[test]
    /// Verifies wrapper-compliance language skips a memory action without
    /// consuming either legitimate search slot, allowing two subsequent
    /// concrete searches before the normal ceiling applies.
    fn memory_action_budget_skips_wrapper_placeholders_without_consuming_searches() {
        let context = context_without_prior_searches();
        let mut budget = MemoryActionBudget::from_context(&context);
        let placeholder = memory_search(
            "placeholder",
            "comply with the required current actions call before proceeding",
        );

        let skipped = budget
            .accept_or_skip(&TestTurn, &placeholder, "schema wrapper prerequisite", None)
            .expect("wrapper placeholder should be skipped");
        assert!(
            skipped
                .structured_content_json
                .as_deref()
                .is_some_and(|content| content.contains("memory_wrapper_placeholder"))
        );
        assert!(
            budget
                .accept_or_skip(
                    &TestTurn,
                    &memory_search("one", "find prior decision"),
                    "",
                    None
                )
                .is_none()
        );
        assert!(
            budget
                .accept_or_skip(
                    &TestTurn,
                    &memory_search("two", "find prior invariant"),
                    "",
                    None
                )
                .is_none()
        );
        assert!(
            budget
                .accept_or_skip(
                    &TestTurn,
                    &memory_search("three", "find prior detail"),
                    "",
                    None
                )
                .is_some()
        );
    }

    #[test]
    /// Verifies non-memory actions never consume the search budget or produce a
    /// guardrail result, even when surrounding rationale mentions tool-wrapper
    /// compliance language that would skip a memory action.
    fn memory_action_budget_ignores_non_memory_actions() {
        let context = context_without_prior_searches();
        let mut budget = MemoryActionBudget::from_context(&context);
        let action = AgentAction {
            id: "say-1".to_string(),
            rationale: "comply with required tool call".to_string(),
            payload: AgentActionPayload::Say {
                status: crate::SayStatus::Progress,
                text: "working".to_string(),
                content_type: "text/plain".to_string(),
            },
        };

        assert!(
            budget
                .accept_or_skip(&TestTurn, &action, "action wrapper placeholder", None)
                .is_none()
        );
        assert!(
            budget
                .accept_or_skip(
                    &TestTurn,
                    &memory_search("one", "find prior decision"),
                    "",
                    None
                )
                .is_none()
        );
        assert!(
            budget
                .accept_or_skip(
                    &TestTurn,
                    &memory_search("two", "find prior invariant"),
                    "",
                    None
                )
                .is_none()
        );
    }
}
