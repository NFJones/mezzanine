//! Immediate result source custody after an action header is already installed.
//!
//! These process-free presenter tests do not execute a worker or prove canonical
//! completion acceptance. They isolate immutable publication source/occurrence
//! ownership from the separately fallible action-completion settlement path.

use super::*;

/// A visible header must not bypass source retention. Two failed immediate
/// installations retain the original result bytes, entry identity and occurrence,
/// even when a later caller supplies different text (which must be rejected).
/// Both an ordinary ordered drain and a successful exact-source retry acknowledge that original
/// component once without any action/provider execution.
#[test]
fn runtime_immediate_result_retains_original_source_across_install_failures() {
    for immediate_retry in [false, true] {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let started = service
            .start_agent_prompt_turn("%1", "inspect result custody")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .clone();
        service.remove_pending_agent_provider_task(&turn.turn_id);
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(120, 24).unwrap(), 120).unwrap(),
        );
        let action = mez_agent::AgentAction {
            id: "retained-result".into(),
            payload: mez_agent::AgentActionPayload::ListAgents {
                agent_type: None,
                scope: None,
            },
        };
        let result = mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["ORIGINAL_RESULT_CUSTODY".into()],
            None,
        );
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".into(),
                model: "test".into(),
                raw_text: String::new(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: String::new(),
                    actions: vec![action.clone()],
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: Default::default(),
            action_results: vec![result.clone()],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };
        service
            .present_agent_response_actions_to_terminal_buffer("%1", &execution)
            .unwrap();
        service
            .queue_ordered_provider_header_with_text(
                "%1",
                &execution,
                &action,
                "INSTALLED_HEADER_CUSTODY".into(),
            )
            .unwrap();
        service
            .flush_ordered_provider_headers("%1", &execution)
            .unwrap();
        let key = service
            .presentation
            .agent_published_provider_headers
            .iter()
            .next()
            .unwrap()
            .clone();
        assert_eq!(key.0, "%1");
        assert_eq!(key.1, turn.turn_id);
        assert_eq!(key.2, turn.conversation_id);
        assert_eq!(key.4, 0);
        assert!(
            service
                .presentation
                .agent_queued_provider_results
                .is_empty()
        );

        service.fail_agent_presentation_install_for_tests(0);
        assert!(
            service
                .queue_ordered_provider_result("%1", &execution, &action, &result)
                .is_err()
        );
        let retained = service
            .presentation
            .agent_queued_provider_results
            .get(&key)
            .expect("failed immediate installation must retain its original source")
            .clone();
        assert_eq!(retained.1, result);
        assert_eq!(retained.2, result.content_text());
        let replacement = mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["REPLACEMENT_MUST_NOT_SUBSTITUTE".into()],
            None,
        );
        service.fail_agent_presentation_install_for_tests(0);
        assert!(
            service
                .queue_ordered_provider_result("%1", &execution, &action, &result)
                .is_err()
        );
        let conflict = service
            .queue_ordered_provider_result("%1", &execution, &action, &replacement)
            .expect_err("a different callback cannot replace retained source");
        assert!(conflict.message().contains("retained source owner"));
        let pending = service
            .presentation
            .agent_queued_provider_results
            .get(&key)
            .unwrap();
        assert_eq!(pending.1, retained.1);
        assert_eq!(pending.2, retained.2);
        assert_eq!(pending.3.id, retained.3.id);
        assert_eq!(pending.3.occurrence, retained.3.occurrence);

        if immediate_retry {
            service
                .queue_ordered_provider_result("%1", &execution, &action, &result)
                .unwrap();
            assert!(
                service
                    .presentation
                    .agent_queued_provider_results
                    .is_empty()
            );
            assert!(
                service
                    .presentation
                    .agent_published_provider_results
                    .contains(&(key.clone(), result.content_text()))
            );
            assert!(
                !service
                    .presentation
                    .agent_published_provider_results
                    .contains(&(key.clone(), replacement.content_text()))
            );
        }
        service
            .flush_ordered_provider_headers("%1", &execution)
            .unwrap();
        service
            .flush_ordered_provider_headers("%1", &execution)
            .unwrap();
        assert!(
            service
                .presentation
                .agent_queued_provider_results
                .is_empty()
        );
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(
            rows.matches("INSTALLED_HEADER_CUSTODY").count(),
            1,
            "{rows}"
        );
        assert_eq!(rows.matches("ORIGINAL_RESULT_CUSTODY").count(), 1, "{rows}");
        assert!(!rows.contains("REPLACEMENT_MUST_NOT_SUBSTITUTE"), "{rows}");
        assert!(
            service
                .presentation
                .agent_published_provider_results
                .contains(&(key, result.content_text()))
        );
    }
}
