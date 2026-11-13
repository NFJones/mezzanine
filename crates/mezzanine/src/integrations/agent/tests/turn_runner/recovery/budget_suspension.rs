//! Local budget suspension must return the original error and observed expense
//! without a second inference, even when attached diagnostics invite recovery.
//! These are actual product turn runners with resource-free counted providers.

use super::*;

async fn qualify(asynchronous: bool) {
    for message in [
        "provider MAAP output is malformed: fixture",
        "context length exceeded; reduce max_output_tokens and retry",
        "provider response was incomplete: max_messages",
        "temporary server error; please retry",
    ] {
        let usage = mez_agent::ModelTokenUsage {
            input_tokens: 11,
            output_tokens: 3,
            ..Default::default()
        };
        let error = crate::MezError::new(crate::error::MezErrorKind::BudgetSuspended, message)
            .with_provider_failure_json(
                r#"{"type":"malformed_model_output","error":{"code":"context_length_exceeded"},"incomplete_details":{"reason":"max_messages"}}"#,
            )
            .with_provider_incurred_usage(usage);
        let provider = SequencedProvider::new(vec![Err(error)]);
        let policy = PermissionPolicy::default();
        let approvals = SessionApprovalStore::default();
        let permissions =
            crate::security::permissions::ProductPermissionPlanning::new(&policy, &approvals, None);
        let mut ledger = AgentTurnLedger::new(false);
        let turn = turn();
        let id = turn.turn_id.clone();
        let runner = AgentTurnRunner {
            provider: &provider,
            model_profile: ModelProfile {
                provider: "batch".into(),
                model: "test".into(),
                ..Default::default()
            },
            permissions: &permissions,
            subagent_scope: None,
            subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
            available_mcp_servers: Vec::new(),
            available_mcp_tools: &[],
            memory_actions_enabled: false,
            issue_actions_enabled: true,
        };
        let context = AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "user".into(),
            content: "fixture".into(),
        }])
        .unwrap();
        let result = if asynchronous {
            runner.run_turn_async(&mut ledger, turn, context).await
        } else {
            runner.run_turn(&mut ledger, turn, context)
        };
        assert_eq!(provider.requests().len(), 1, "{message}");
        let error = result.unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::BudgetSuspended);
        assert_eq!(error.provider_incurred_usage(), usage);
        // The harness returns this hold to runtime, not a terminal failure or
        // a fabricated successful/failed response from a generated summary.
        assert_eq!(ledger.turn(&id).unwrap().state, AgentTurnState::Running);
    }
}

#[tokio::test]
async fn budget_suspension_sync_runner_never_repairs_or_summarizes() {
    qualify(false).await;
}

#[tokio::test]
async fn budget_suspension_async_runner_never_repairs_or_summarizes() {
    qualify(true).await;
}
