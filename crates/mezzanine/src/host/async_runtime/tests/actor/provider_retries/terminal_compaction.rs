//! Actor/persistence-worker acceptance for terminal compaction continuity.
//!
//! Model responses are synthetic, but context selection, compactor completion,
//! terminal capture, checked candidate admission, append receipts, retries and
//! replay publication use their production owners without provider credentials.

use super::*;

/// Two accepted heads survive interruption and a genuinely asynchronous failed
/// terminal append/retry. Each rewrite publishes before actor adoption; later
/// audit settlement cannot roll back its accepted head. Reopen uses only the
/// latest head and never dispatches inference just because the turn compacted.
#[tokio::test(flavor = "current_thread")]
async fn async_terminal_compaction_receipt_retry_preserves_reopened_continuity() {
    let store = AgentTranscriptStore::new(std::env::temp_dir().join(format!(
        "mez-async-terminal-handoff-{}",
        crate::storage::token_usage::new_token_usage_event_id(),
    )));
    let mut service = test_service_with_event_log();
    service.set_agent_transcript_store(store.clone());
    let layers = vec![ConfigLayer {
        name: "terminal-worker".into(), path: None, format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"fixture\"\ndefault_model_profile=\"fixture\"\n[providers.fixture]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.fixture]\nprovider=\"fixture\"\nmodel=\"test\"\ncontext_window_tokens=40000\nmax_input_tokens=20000\n".into(),
    }];
    service.replace_config_layers(layers.clone()).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let mut turn = crate::test_support::compaction_source::complete_task_and_admit_successor(
        &mut service,
        &primary,
        &format!("ASYNC_RAW_SOURCE {}", "source ".repeat(5_000)),
        "original task completed",
        "ASYNC_INITIAL_PROMPT",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let audit = store.inspect(&conversation).unwrap();
    let error = crate::MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    for index in 0..2 {
        if index > 0 {
            // A successor admission supplies genuinely new closed history;
            // unchanged automatic pressure is not permission to recompact.
            crate::test_support::compaction_source::complete_task(
                &mut service,
                &turn,
                "first compacted task completed",
                mez_agent::ProviderApiCompatibility::OpenAiResponses,
            );
            let started = service
                .execute_agent_shell_command(&primary, "continue with a new exact task")
                .unwrap();
            assert!(started.contains(r#""state":"running""#), "{started}");
            turn = service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .clone()
                .unwrap();
        }
        assert!(
            service
                .schedule_agent_provider_retry_transition(
                    &AgentId::opaque("agent-%1").unwrap(),
                    &turn,
                    mez_agent::ProviderErrorRetryClass::ContextLimit,
                    &error,
                )
                .unwrap()
                .is_some()
        );
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        assert!(task.replay_source.is_some());
        service.claim_agent_compaction_task_state("%1", task.clone());
        let (result, _) = crate::test_support::compaction_source::execute_wave(
            &service,
            &task,
            Ok(mez_agent::ModelResponse {
                provider: "fixture".into(),
                model: "test".into(),
                raw_text: format!("ASYNC_SUMMARY_{index}"),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            }),
        );
        service
            .apply_agent_compaction_transition(
                crate::test_support::compaction_source::model_result_event(
                    "%1",
                    task.task_generation,
                    result,
                ),
            )
            .unwrap();
        assert!(!service.agent_provider_task_is_pending(&turn));
        let receipt =
            crate::test_support::compaction_publication::execute_pending_publication(&mut service);
        assert!(!service.agent_provider_task_is_pending(&turn));
        assert!(
            service
                .apply_replay_epoch_completion(receipt)
                .unwrap()
                .applied
        );
        assert!(service.agent_provider_task_is_pending(&turn));
    }
    let (_, accepted) = store
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    let accepted_head = accepted.head().cloned().unwrap();
    assert_eq!(accepted.revision().get(), 2);
    assert_eq!(
        accepted.context().chronology()[0].block().content,
        "ASYNC_SUMMARY_1"
    );
    service.use_transcript_effect_adapter();
    service
        .finish_agent_turn("%1", &turn, mez_agent::AgentTurnState::Interrupted)
        .unwrap();
    assert!(!service.agent_turn_contexts().contains_key(&turn));
    assert!(service.runtime_agent_history_epoch_context("%1").is_err());
    assert_eq!(
        store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap()
            .1
            .head(),
        Some(&accepted_head)
    );
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
        let (_, cold) = store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(cold.head(), Some(&accepted_head));
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_compaction_task_ids().is_empty());
    exit.service
        .set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    exit.service
        .runtime_agent_history_epoch_context("%1")
        .expect("settled terminal source must reopen");
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.head(), Some(&accepted_head));
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "ASYNC_SUMMARY_1"
    );
    assert!(
        !cold
            .context()
            .chronology()
            .iter()
            .skip(1)
            .any(|event| event.block().content == "ASYNC_SUMMARY_0")
    );
    assert_eq!(
        &store.inspect(&conversation).unwrap()[..audit.len()],
        audit.as_slice()
    );
    // A new terminal reference also needs original custody through saved copy;
    // accepting it for local replay alone would leave fork continuity broken.
    let rows = store.inspect(&conversation).unwrap();
    let original_hint = rows
        .iter()
        .filter(|row| row.role == mez_agent::TranscriptRole::System)
        .filter_map(|row| {
            mez_agent::transcript::TranscriptContextRecord::from_transcript_content(&row.content)
                .unwrap()
        })
        .find(|record| {
            matches!(
                record.event(),
                mez_agent::TranscriptContextEvent::InterruptedTurn { .. }
            )
        })
        .unwrap();
    store.fork(&conversation, "terminal-copy", 99).unwrap();
    let (_, copied) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch("terminal-copy", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(
        copied.context().chronology()[0].block().content,
        "ASYNC_SUMMARY_1"
    );
    exit.service.terminate_all_pane_processes().unwrap();
    let mut reopened = test_service_with_event_log();
    reopened.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    reopened.replace_config_layers(layers).unwrap();
    let copy_primary = reopened
        .attach_primary("copy-primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    reopened
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    reopened
        .agent_shell_store_mut()
        .bind_conversation("%1", "terminal-copy", rows.len() as u64)
        .unwrap();
    let started = reopened
        .execute_agent_shell_command(&copy_primary, "inspect copied interrupted work")
        .unwrap();
    assert!(started.contains(r#""state":"running""#), "{started}");
    let next = reopened
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    let hints = reopened.agent_turn_contexts()[&next]
        .chronology()
        .iter()
        .filter(|event| event.block().label == "interrupted turn context")
        .collect::<Vec<_>>();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].record(), original_hint.record());
    assert_eq!(
        hints[0].block().source,
        mez_agent::ContextSourceKind::RuntimeHint
    );
    assert!(
        hints[0]
            .block()
            .content
            .contains("continue with a new exact task")
    );
    assert!(reopened.pending_agent_compaction_task_ids().is_empty());
    assert_eq!(store.inspect(&conversation).unwrap(), rows);
    reopened.terminate_all_pane_processes().unwrap();
}
