//! Actor/persistence-worker acceptance for terminal compaction continuity.
//!
//! Model responses are synthetic, but context selection, compactor completion,
//! terminal capture, checked candidate admission, append receipts, retries and
//! replay publication use their production owners without provider credentials.

use super::*;

/// Two accepted live summaries cross an interrupted terminal boundary and a
/// genuinely asynchronous failed append/retry. The actor must keep next-history
/// admission fenced until a source-proven epoch and exact receipt settlement
/// exist; reopened replay retains summaries once and never dispatches compaction
/// just because the old turn compacted.
#[tokio::test(flavor = "current_thread")]
async fn async_terminal_compaction_receipt_retry_preserves_reopened_continuity() {
    let store = AgentTranscriptStore::new(std::env::temp_dir().join(format!(
        "mez-async-terminal-handoff-{}",
        crate::storage::token_usage::new_token_usage_event_id(),
    )));
    let mut service = test_service_with_event_log();
    service.set_agent_transcript_store(store.clone());
    service.replace_config_layers(vec![ConfigLayer {
        name: "terminal-worker".into(), path: None, format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"fixture\"\ndefault_model_profile=\"fixture\"\n[providers.fixture]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.fixture]\nprovider=\"fixture\"\nmodel=\"test\"\ncontext_window_tokens=40000\nmax_input_tokens=20000\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "ASYNC_INITIAL_PROMPT")
        .unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    for index in 0..2 {
        if index > 0 {
            context
                .append_user_event("steering", "ASYNC_EXACT_STEERING")
                .unwrap();
        }
        let group = mez_agent::ContextExecutionGroupId::new(format!("async-live-{index}")).unwrap();
        context
            .append_assistant_event("decision", "ASYNC_RAW_SOURCE ".repeat(200), group.clone())
            .unwrap();
        context
            .append_evidence_event(
                mez_agent::ContextSourceKind::ActionResult,
                "result",
                "settled",
                group,
                None,
                true,
            )
            .unwrap();
    }
    for index in 0..2 {
        let context = service.agent_turn_contexts().get("turn-1").unwrap();
        let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
            context,
            20_000,
            0,
            context.event_sequence_high_water_mark(),
            mez_agent::ProviderBudgetProjection::new(
                mez_agent::ProviderApiCompatibility::OpenAiResponses,
                "fixture",
            ),
        )
        .unwrap();
        let profile = service.agent_turn_model_profile("turn-1").unwrap().clone();
        assert!(
            service
                .queue_agent_context_limit_recovery_compaction(
                    "turn-1",
                    "fixture".into(),
                    profile,
                    1,
                    plan
                )
                .unwrap()
        );
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        service.claim_agent_compaction_task_state("%1", task);
        service
            .apply_agent_compaction_completed_event(
                "%1",
                mez_agent::ModelResponse {
                    provider: "fixture".into(),
                    model: "test".into(),
                    raw_text: format!("ASYNC_SUMMARY_{index}"),
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Default::default(),
                    action_batch: None,
                    provider_transcript_events: Vec::new(),
                },
            )
            .unwrap();
    }
    service.use_transcript_effect_adapter();
    service
        .finish_agent_turn("%1", "turn-1", mez_agent::AgentTurnState::Interrupted)
        .unwrap();
    assert!(!service.agent_turn_contexts().contains_key("turn-1"));
    assert!(service.runtime_agent_history_epoch_context("%1").is_err());
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    store.fail_transcript_append_attempts(2);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 100,
                drain_limit: 64,
                idle_interval: Duration::from_millis(2),
            },
            |polls, _| polls >= 100,
        )
        .await
        .unwrap();
        assert!(report.failed >= 1, "{report:?}");
        assert!(report.completed >= 1, "{report:?}");
        assert!(store.pending_append_receipts().unwrap().is_empty());
        assert_eq!(
            store
                .compaction_epoch(&conversation)
                .unwrap()
                .unwrap()
                .ranges
                .len(),
            2
        );
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_compaction_task_ids().is_empty());
    exit.service
        .set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    assert!(
        exit.service
            .runtime_agent_history_epoch_context("%1")
            .is_ok()
    );
    let epoch = store.compaction_epoch(&conversation).unwrap().unwrap();
    assert_eq!(
        epoch
            .ranges
            .iter()
            .map(|range| range.summary.as_str())
            .collect::<Vec<_>>(),
        vec!["ASYNC_SUMMARY_0", "ASYNC_SUMMARY_1"]
    );
    exit.service.terminate_all_pane_processes().unwrap();
}
