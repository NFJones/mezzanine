//! Provider-independent issue action-result projection.
//!
//! This module converts canonical issue repository results into canonical MAAP
//! action results and structured JSON. Project discovery, identifier
//! generation, SQLite execution, clocks, and product error projection remain
//! in the application adapter.

use super::{DeleteIssueResult, IssueRecord, UpdateIssueResult};
use crate::{ActionResult, AgentAction, AgentTurnResultIdentity};

/// Builds the structured JSON representation of one canonical issue record.
pub fn issue_record_json(record: &IssueRecord) -> serde_json::Value {
    serde_json::json!({
        "id": record.id,
        "project": record.project,
        "kind": record.kind.as_str(),
        "state": record.state.as_str(),
        "title": record.title,
        "body": record.body,
        "notes": record.notes,
        "depends_on": record.depends_on,
        "created_at_unix_seconds": record.created_at_unix_seconds,
        "updated_at_unix_seconds": record.updated_at_unix_seconds,
    })
}

/// Builds the successful result for an issue add or record-returning action.
pub fn issue_record_action_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    operation: &str,
    record: &IssueRecord,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![format!("issue_{operation} {}", record.id)],
        Some(issue_record_json(record).to_string()),
    )
}

/// Builds the successful result for an issue query.
pub fn issue_query_action_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    records: &[IssueRecord],
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![format!("issue_query returned {} records", records.len())],
        Some(
            serde_json::json!({
                "count": records.len(),
                "issues": records.iter().map(issue_record_json).collect::<Vec<_>>(),
            })
            .to_string(),
        ),
    )
}

/// Builds the successful result for an issue update repository outcome.
pub fn issue_update_action_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    result: &UpdateIssueResult,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![format!(
            "issue_update {} updated={}",
            result.id, result.updated
        )],
        Some(
            serde_json::json!({
                "id": result.id,
                "project": result.project,
                "updated": result.updated,
                "record": result.record.as_ref().map(issue_record_json),
            })
            .to_string(),
        ),
    )
}

/// Builds the successful result for an issue deletion repository outcome.
pub fn issue_delete_action_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    result: &DeleteIssueResult,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![format!(
            "issue_delete {} deleted={}",
            result.id, result.deleted
        )],
        Some(
            serde_json::json!({
                "id": result.id,
                "project": result.project,
                "deleted": result.deleted,
            })
            .to_string(),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::{IssueKind, IssueState};
    use crate::{AgentActionPayload, AgentTurnRecord, AgentTurnState, AgentTurnTrigger};

    /// Builds one active turn fixture for issue result projection.
    fn turn() -> AgentTurnRecord {
        AgentTurnRecord {
            turn_id: "turn-1".to_string(),
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

    /// Builds one issue-query action fixture for projection tests.
    fn action() -> AgentAction {
        AgentAction {
            id: "query-1".to_string(),
            rationale: "Inspect open issues".to_string(),
            payload: AgentActionPayload::IssueQuery {
                kind: None,
                state: None,
                text: None,
                limit: None,
            },
        }
    }

    /// Builds one canonical issue record fixture.
    fn record() -> IssueRecord {
        IssueRecord {
            id: "issue-1".to_string(),
            project: "/repo".to_string(),
            kind: IssueKind::Task,
            state: IssueState::Open,
            title: "Finish decomposition".to_string(),
            body: Some("Move neutral policy lower.".to_string()),
            notes: None,
            depends_on: Vec::new(),
            created_at_unix_seconds: 1,
            updated_at_unix_seconds: 2,
        }
    }

    /// Verifies query projection preserves stable issue names, records, and
    /// counts without relying on product repository internals.
    #[test]
    fn issue_query_projection_preserves_canonical_records() {
        let result = issue_query_action_result(&turn(), &action(), &[record()]);
        let structured: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();

        assert_eq!(result.content_text(), "issue_query returned 1 records");
        assert_eq!(structured["count"], 1);
        assert_eq!(structured["issues"][0]["kind"], "task");
        assert_eq!(structured["issues"][0]["state"], "open");
    }

    /// Verifies update and delete repository outcomes retain their boolean
    /// state in both user-visible and structured result content.
    #[test]
    fn issue_mutation_projection_preserves_repository_outcomes() {
        let update = issue_update_action_result(
            &turn(),
            &action(),
            &UpdateIssueResult {
                project: "/repo".to_string(),
                id: "issue-1".to_string(),
                updated: true,
                record: Some(record()),
            },
        );
        let delete = issue_delete_action_result(
            &turn(),
            &action(),
            &DeleteIssueResult {
                project: "/repo".to_string(),
                id: "issue-1".to_string(),
                deleted: false,
            },
        );

        assert!(update.content_text().contains("updated=true"));
        assert!(delete.content_text().contains("deleted=false"));
    }
}
