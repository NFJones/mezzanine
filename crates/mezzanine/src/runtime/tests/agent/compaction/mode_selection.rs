//! Opposite-default summary selection and immutable policy during preparation.

use super::*;

/// All admissions must honor pane summary over global raw_tail and invoke the
/// real offline model-wave boundary before publishing one authoritative head.
#[test]
fn runtime_compaction_mode_summary_override_all_triggers() {
    for trigger in ["manual", "observed", "rejected"] {
        let (mut service, store, active) = if trigger == "manual" {
            let (mut service, store) =
                manual_model::queued("mode-summary-manual", "old eligible history ".repeat(2_000));
            service.cancel_current_agent_compaction_task("%1");
            (service, store, None)
        } else {
            let (service, store, turn) = observed_history::prepared_with_cap(20_000, false);
            (service, store, Some(turn))
        };
        raw_tail::enable_raw(&mut service);
        let primary = service
            .attach_primary("mode-summary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        let selected = service
            .execute_agent_shell_command(&primary, "/compaction-mode summary")
            .unwrap();
        assert!(selected.contains("changed=true"), "{selected}");
        assert_eq!(service.agent_compaction_mode().name(), "raw_tail");
        assert_eq!(
            service.effective_agent_compaction_mode("%sibling").name(),
            "raw_tail"
        );
        match trigger {
            "manual" => assert!(
                service
                    .execute_agent_shell_command(&primary, "/compact")
                    .unwrap()
                    .contains("state=queued")
            ),
            "observed" => {
                let turn = active.as_ref().unwrap();
                let profile = service.agent_turn_model_profile(turn).unwrap().clone();
                let mut response = runtime_say_response(turn, "continue", false);
                response.usage.input_tokens = 20_000;
                service
                    .execute_agent_turn_with_provider(
                        turn,
                        &RuntimeBatchProvider { response },
                        profile,
                    )
                    .unwrap();
            }
            _ => {
                let error = MezError::invalid_state("provider context length exceeded")
                    .with_provider_failure_json(
                        r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
                    );
                assert!(
                    service
                        .recover_agent_provider_context_limit_failure(
                            &AgentId::opaque("agent-%1").unwrap(),
                            active.as_ref().unwrap(),
                            &error,
                            1
                        )
                        .unwrap()
                );
            }
        }
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        assert_eq!(task.replay_source.as_ref().unwrap().mode.name(), "summary");
        assert_eq!(
            service
                .runtime_compaction_status("%1")
                .unwrap()
                .captured_mode
                .unwrap()
                .name(),
            "summary"
        );
        let busy = service
            .execute_agent_shell_command(&primary, "/compaction-mode raw_tail")
            .unwrap();
        assert!(busy.contains("cannot mutate pane state"), "{busy}");
        let requests = run_runtime_test_compaction_wave(
            &mut service,
            "%1",
            Ok(runtime_test_compaction_response(
                "one selected summary head",
            )),
        );
        assert!(
            !requests.is_empty(),
            "summary override did no actual inference: {trigger}"
        );
        let receipt = replay_publication::execute_pending_publication(&mut service);
        service
            .apply_replay_epoch_completion(receipt.clone())
            .unwrap();
        service.apply_replay_epoch_completion(receipt).unwrap();
        assert!(!service.agent_is_compacting("%1"));
        let (_, owner) = store
            .load_replay_epoch(&task.conversation_id, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(owner.head_policy(), mez_agent::ReplayHeadPolicy::Summary);
        assert!(owner.head().is_some());
        if let Some(turn) = active {
            assert!(service.agent_provider_task_is_pending(&turn));
        }
    }
}
