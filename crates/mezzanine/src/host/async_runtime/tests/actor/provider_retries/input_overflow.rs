//! Missing-code input-overflow admission through the production actor boundary.
//!
//! Synthetic HTTP failure envelopes have no model output or partial-output
//! state. Exact claims, classification, bounded recovery, and dispatch selection
//! remain production-owned. An offline completed task supplies eligible source;
//! the rejected request runs no provider or action worker.

use super::*;

/// Output-parameter advice in a real failure envelope must admit exactly one
/// compaction, not terminal output recovery or an ordinary provider replay.
/// Stale and duplicate deliveries cannot spend another attempt or alter source;
/// the durable rejected context stays exact until validated compactor output.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_missing_code_input_overflow_queues_one_compaction() {
    for parameter in ["max_tokens", "max_output_tokens"] {
        let mut service = test_service();
        service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
            std::env::temp_dir().join(format!(
                "mez-overflow-source-{}",
                crate::storage::token_usage::new_token_usage_event_id()
            )),
        ));
        service.replace_config_layers(vec![ConfigLayer {
            name: "input-overflow".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nrouting=false\ndefault_provider=\"fixture\"\ndefault_model_profile=\"fixture\"\n[providers.fixture]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.fixture]\nprovider=\"fixture\"\nmodel=\"test\"\ncontext_window_tokens=40000\n".into(),
        }]).unwrap();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        crate::test_support::compaction_source::complete_task_and_admit_successor(
            &mut service,
            &primary,
            &format!("ORIGINAL_OVERFLOW_SOURCE {}", "observed ".repeat(3_000)),
            "original observation task completed",
            "preserve the exact task",
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
        );
        let task = service.pending_agent_provider_tasks().remove(0);
        let context = service.agent_turn_contexts().get(&task.turn_id).unwrap();
        let before = context.blocks().to_vec();
        let conversation = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        let prior_usage = service.agent_token_usage_for_conversation(&conversation);
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
            .build()
            .unwrap();
        let client = async {
            handle
                .record_claimed_agent_provider_task_for_tests(task.turn_id.clone(), 1)
                .await
                .unwrap();
            let message = format!(
                "Maximum context length exceeded: input is too large; reduce the messages or {parameter}."
            );
            let payload = serde_json::json!({
                "status_code":400,
                "error":{"type":"invalid_request_error","message":message,"param":parameter}
            })
            .to_string();
            let failure = |generation| {
                RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
                    agent_id: AgentId::opaque(task.agent_id.clone()).unwrap(),
                    turn_id: task.turn_id.clone(),
                    claim_generation: generation,
                    kind: "invalid_state".into(),
                    message: message.clone(),
                    provider_failure_json: Some(payload.clone()),
                    provider_raw_text: None,
                    provider_output_limit_state: None,
                })
            };
            let mut stale = RuntimeEventBatch::new();
            stale.push(failure(0));
            assert_eq!(
                handle.submit_runtime_events(stale).await.unwrap().applied,
                0
            );
            assert!(
                handle
                    .drain_agent_provider_dispatch_side_effects(8)
                    .await
                    .unwrap()
                    .is_empty()
            );

            let mut accepted = RuntimeEventBatch::new();
            accepted.push(failure(1));
            assert_eq!(
                handle
                    .submit_runtime_events(accepted)
                    .await
                    .unwrap()
                    .applied,
                1
            );
            let dispatches = handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap();
            assert!(
                matches!(dispatches.as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { pane_id, .. }] if pane_id == "%1"),
                "{dispatches:?}"
            );
            assert!(
                handle
                    .pending_agent_provider_tasks()
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(!handle.drain_timer_side_effects(8).await.unwrap().iter().any(|effect| matches!(effect,
                RuntimeSideEffect::ScheduleTimer { key, .. } if key.kind == RuntimeTimerKind::ProviderRetry
            )));

            let mut duplicate = RuntimeEventBatch::new();
            duplicate.push(failure(1));
            assert_eq!(
                handle
                    .submit_runtime_events(duplicate)
                    .await
                    .unwrap()
                    .applied,
                0
            );
            assert!(
                handle
                    .drain_agent_provider_dispatch_side_effects(8)
                    .await
                    .unwrap()
                    .is_empty()
            );
            handle.shutdown().await.unwrap();
        };
        let ((), mut exit) = tokio::join!(client, actor.run());
        assert_eq!(
            exit.service
                .provider_retry_scheduler_mut()
                .attempt(&task.turn_id),
            1
        );
        assert_eq!(
            exit.service
                .agent_turn_contexts()
                .get(&task.turn_id)
                .unwrap()
                .blocks(),
            before
        );
        assert_eq!(
            exit.service.pending_agent_compaction_task_ids(),
            vec!["%1".to_string()]
        );
        assert!(
            exit.service
                .pending_agent_compaction_task_for_tests("%1")
                .unwrap()
                .replay_source
                .is_some()
        );
        assert_eq!(
            exit.service
                .agent_turn_ledger()
                .turn(&task.turn_id)
                .unwrap()
                .state,
            mez_agent::AgentTurnState::Running
        );
        assert_eq!(
            exit.service
                .agent_token_usage_for_conversation(&conversation),
            prior_usage,
            "rejected and duplicate requests cannot add usage to prior completed work",
        );
        exit.service.terminate_all_pane_processes().unwrap();
    }
}
