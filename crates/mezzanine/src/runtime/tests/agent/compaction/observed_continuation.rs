//! Observed pressure retains its trigger across failure and deferred execution.
//! Real prior completion supplies eligible source. Current exact instructions
//! and action receipts stay outside that prefix; only an accepted publication
//! releases the ordinary continuation. No replacement first-turn history or
//! fabricated running/succeeded action result supplies eligibility.

use super::*;

#[test]
fn runtime_observed_input_limit_compaction_failure_keeps_its_trigger() {
    let (mut service, store, turn) = observed_history::queued();
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    assert_eq!(task.source, "observed-input-limit");
    assert!(task.replay_source.is_some());
    let conversation = task.conversation_id.clone();
    let usage = service.agent_context_usage_snapshot(&conversation).unwrap();
    assert_eq!(usage.input_tokens, 20_000);
    let original = store.inspect(&conversation).unwrap();
    service.claim_agent_compaction_task_state("%1", task);
    assert!(
        service
            .apply_agent_compaction_failed_event(
                "%1",
                "forbidden",
                "provider authentication rejected the compaction request",
                Some(r#"{"status_code":401,"error":{"code":"invalid_api_key"}}"#),
            )
            .unwrap()
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let failure = normalized_pane_log_text(&pane_text);
    assert!(
        failure.contains("automatic observed-input-limit compaction failed before provider retry"),
        "{pane_text}"
    );
    assert!(
        !failure.contains("automatic output-limit compaction"),
        "{pane_text}"
    );
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
    assert_eq!(
        service.agent_context_usage_snapshot(&conversation),
        Some(usage)
    );
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    assert!(
        store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    assert!(store.inspect(&conversation).unwrap().starts_with(&original));
}

/// A post-plan exact arrival can exceed the next request allowance. It remains
/// raw through the one valid publication; that estimate cannot regenerate or
/// reject the frozen summary, nor authorize summarizing the new instruction.
#[test]
fn runtime_observed_input_limit_preserves_over_cap_arrival_after_commit() {
    let (mut service, store, turn) = observed_history::queued();
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    assert_eq!(task.source, "observed-input-limit");
    assert!(matches!(
        task.replay_source.as_ref().unwrap().trigger(),
        Some(
            crate::runtime::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                observed_input_tokens: 20_000,
                max_input_tokens: 20_000,
            }
        )
    ));
    let exact = "OVERSIZED_EXACT_STEERING ".repeat(5_000);
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn)
        .unwrap()
        .append_user_event("over-cap exact steering", exact.clone())
        .unwrap();
    let before = service.agent_turn_contexts()[&turn].clone();
    let retained = before.chronology().last().unwrap().clone();
    assert_eq!(retained.retention(), mez_agent::ContextRetention::Exact);
    assert!(
        service
            .observed_compaction_candidate_input_tokens_for_tests("%1", "x")
            .unwrap()
            > 20_000
    );
    let original = store.inspect(&task.conversation_id).unwrap();
    let requests = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("observed input summary")),
    );
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0]
            .messages
            .iter()
            .any(|message| message.content.contains(&exact))
    );
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    assert!(!service.agent_provider_task_is_pending(&turn));
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, "observed input summary");
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Running
    );
    assert!(
        service.agent_turn_contexts()[&turn]
            .chronology()
            .contains(&retained)
    );
    assert!(service.agent_provider_task_is_pending(&turn));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert_eq!(store.inspect(&task.conversation_id).unwrap(), original);
}

/// Owns only this fixture's physical pane processes, including panic cleanup.
struct NativeCleanup(RuntimeSessionService);
impl Drop for NativeCleanup {
    fn drop(&mut self) {
        let _ = self.0.terminate_all_pane_processes();
    }
}

#[test]
fn runtime_observed_input_limit_compacts_before_deferred_action_continuation_claim() {
    let (service, store, turn) = observed_history::prepared_with_cap(40_000, true);
    let mut cleanup = NativeCleanup(service);
    let service = &mut cleanup.0;
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&temp_root(
            "observed-native-continuation-auth",
        )),
    ));
    let root = temp_root("observed-native-once");
    std::fs::create_dir_all(&root).unwrap();
    let counter = root.join("executions");
    let quoted = counter.to_str().unwrap().replace('\'', "'\\''");
    let mut response = runtime_say_response(&turn, "collect deferred evidence", false);
    response.usage.input_tokens = 40_000;
    response.usage.output_tokens = 1;
    response.action_batch.as_mut().unwrap().actions = vec![mez_agent::AgentAction {
        id: "deferred-shell".into(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Collect deferred evidence once".into(),
            command: format!("printf x >> '{quoted}'; printf deferred"),
            interactive: false,
            stateful: false,
            timeout_ms: Some(5_000),
        },
    }];
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    // This offline provider executes in place of the scheduled initial offer.
    // Consume that offer before dispatch so the real native completion must
    // establish its own continuation transfer rather than meet a stale wake.
    assert!(service.remove_pending_agent_provider_task(&turn));
    let execution = service
        .execute_agent_turn_with_provider(&turn, &RuntimeBatchProvider { response }, profile)
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        execution
            .action_results
            .iter()
            .any(|result| result.status == mez_agent::ActionStatus::Running)
    );
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_none()
    );
    let dispatch = service
        .claim_native_shell_action(&turn, "deferred-shell")
        .unwrap()
        .expect("original actual native action must own a worker");
    let completion = crate::runtime::execute_native_shell_dispatch(dispatch);
    let duplicate = completion.clone();
    assert!(service.complete_native_shell_action(completion).unwrap());
    for _ in 0..4 {
        service.retry_completion_publications();
    }
    let settled = service.agent_turn_contexts()[&turn].clone();
    assert!(!service.complete_native_shell_action(duplicate).unwrap());
    assert_eq!(service.agent_turn_contexts()[&turn], settled);
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "x");
    assert!(
        service
            .claim_native_shell_action(&turn, "deferred-shell")
            .unwrap()
            .is_none()
    );
    let agent = AgentId::opaque(
        service
            .agent_turn_ledger()
            .turn(&turn)
            .unwrap()
            .agent_id
            .clone(),
    )
    .unwrap();
    assert!(
        service
            .claim_configured_agent_provider_task(&agent, &turn)
            .unwrap()
            .is_none()
    );
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap_or_else(|| {
            panic!(
                "native continuation did not queue compaction: state={:?}, queue={:?}",
                service.agent_turn_ledger().turn(&turn).unwrap().state,
                service.completion_provider_queue_flags_for_tests(&turn),
            )
        });
    assert!(task.replay_source.is_some());
    assert_eq!(task.source, "observed-input-limit");
    assert!(
        !task
            .request
            .messages
            .last()
            .unwrap()
            .content
            .contains("printf deferred")
    );
    let before = service.agent_turn_contexts()[&turn].clone();
    let result = before
        .chronology()
        .iter()
        .filter(|event| {
            event.block().source == ContextSourceKind::ActionResult
                && event.block().content.contains("deferred")
        })
        .cloned()
        .collect::<Vec<_>>();
    assert!(!result.is_empty());
    let evidence = before
        .chronology()
        .iter()
        .filter(|event| event.block().label == "completion settlement evidence v1")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(evidence.len(), 1);
    assert!(evidence[0].block().content.contains("deferred-shell"));
    assert!(
        evidence[0]
            .block()
            .content
            .contains("automatic_replay_allowed")
    );
    complete_runtime_test_compaction(service, "%1", "completed earlier source summary");
    replay_observed::adopt(service, &turn);
    replay_observed::assert_head(service, &store, &turn, "completed earlier source summary");
    for event in result.iter().chain(&evidence) {
        assert!(
            service.agent_turn_contexts()[&turn]
                .chronology()
                .contains(event)
        );
    }
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "x");
    assert!(
        service
            .claim_native_shell_action(&turn, "deferred-shell")
            .unwrap()
            .is_none()
    );
    service.terminate_all_pane_processes().unwrap();
}
