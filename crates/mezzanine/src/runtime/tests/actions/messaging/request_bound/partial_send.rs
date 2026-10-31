//! Accepted native sends across receiver failures after irreversible acceptance.
//!
//! Normal provider application supplies the actual action/frame identity. A
//! recoverable idle-receiver fault must preserve sender acceptance evidence and
//! original request custody, rather than misreport loss or repeat the send.

use super::*;

/// Produces one send through real provider application with explicit intent.
/// Receiver failure cannot replace a planned/accepted result or frame identity.
fn attempt_send(
    service: &mut RuntimeSessionService,
    parent: &AgentTurnRecord,
    child: &AgentTurnRecord,
    tracked: bool,
) -> crate::Result<mez_agent::AgentTurnExecution> {
    service.execute_agent_turn_with_provider(
        &parent.turn_id,
        &RuntimeBatchProvider {
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".into(),
                model: "test".into(),
                raw_text: "send once across a real post-accept receiver boundary".into(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "retain actual accepted effect even if receiver commit fails".into(),
                    actions: vec![AgentAction {
                        id: "action-1".into(),
                        payload: AgentActionPayload::SendMessage {
                            expect_reply: tracked,
                            recipient: format!("agent:{}", child.agent_id),
                            scope: Some("session".into()),
                            content_type: "text/plain; charset=utf-8".into(),
                            payload: "PARTIALLY_ACCEPTED_TRACKED_REQUEST".into(),
                            correlation_id: None,
                        },
                    }],
                }),
                provider_transcript_events: Vec::new(),
            },
        },
        runtime_model_profile("runtime-batch", "test"),
    )
}

/// Real acceptance precedes idle-recipient context storage and cursor commit.
/// A failure at either receiver boundary must leave one successful sender
/// action result containing the actual message ID. Receiver recovery remains
/// diagnostic rather than invalidating that already accepted sender action. Normal delivery recovery may finish that
/// original recipient turn but must not resend or lose request tracking.
#[test]
fn request_bound_accepted_send_retains_success_across_receiver_commit_failure() {
    for after_cursor in [false, true] {
        let (mut service, parent, _) = fixture();
        service.configure_agent_scheduler_limit(2).unwrap();
        let initial = spawn_child(&mut service, &parent);
        assert!(
            service
                .validate_native_agent_surface_startup(&initial.pane_id)
                .unwrap()
        );
        service.start_ready_agent_turns().unwrap();
        service
            .complete_running_agent_turn_and_start_ready(
                &initial,
                AgentTurnState::Completed,
                "idle persistent recipient before tracked native send",
            )
            .unwrap();
        if after_cursor {
            service.fail_next_peer_message_receive_after_cursor_advance_for_tests();
        } else {
            service.fail_next_peer_message_receive_after_context_storage_for_tests();
        }
        let sequence = service.message_service().next_message_sequence();
        let execution = attempt_send(&mut service, &parent, &initial, true).unwrap();
        assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
        assert!(
            execution.action_results[0]
                .content_text()
                .contains("accepted for")
        );
        let recipient = AgentId::opaque(initial.agent_id.clone()).unwrap();
        let cursor = service
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence;
        if after_cursor {
            assert_eq!(cursor, sequence);
        } else {
            assert!(cursor < sequence);
        }
        let diagnostic = service.agent_pane_trace_log_text(&parent.pane_id).unwrap();
        assert!(diagnostic.contains("message delivery deferred after acceptance"));
        assert!(diagnostic.contains(if after_cursor {
            "after cursor advancement"
        } else {
            "after context storage"
        }));
        let state = service.message_service().snapshot_state();
        let accepted: Vec<_> = state
            .accepted_messages
            .iter()
            .filter(|row| row.envelope.payload == "PARTIALLY_ACCEPTED_TRACKED_REQUEST")
            .collect();
        assert_eq!(
            accepted.len(),
            1,
            "the genuine send must already have been accepted"
        );
        assert_eq!(state.next_sequence, sequence + 1);
        let message = accepted[0].envelope.id.clone();
        assert_eq!(service.tracked_message_request_usage().0.0, 1);
        let results: Vec<_> = service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .filter(|block| {
                block.source == mez_agent::ContextSourceKind::ActionResult
                    && block.content.contains(&message)
                    && block.content.contains("send_message")
            })
            .collect();
        assert_eq!(
            results.len(),
            1,
            "actual accepted send evidence was lost when receiver commit returned an error"
        );
        assert!(results[0].content.contains("succeeded"));
        for tick in 1..=3 {
            service
                .apply_peer_message_delivery_timer(current_unix_millis(), tick)
                .unwrap();
        }
        let later = service
            .agent_turn_ledger()
            .latest_turn_for_pane(&initial.pane_id)
            .unwrap()
            .clone();
        assert_ne!(later.turn_id, initial.turn_id);
        assert_eq!(service.agent_turn_ledger().turns().len(), 3);
        assert_eq!(
            service
                .message_service()
                .snapshot_state()
                .accepted_messages
                .iter()
                .filter(|row| row.envelope.payload == "PARTIALLY_ACCEPTED_TRACKED_REQUEST")
                .count(),
            1
        );
        assert_eq!(
            service.agent_turn_contexts()[&later.turn_id]
                .blocks()
                .iter()
                .filter(
                    |block| block.source == mez_agent::ContextSourceKind::PeerMessage
                        && block.label.contains(&message)
                )
                .count(),
            1
        );
        wait(&mut service, &parent, &message, None);
        let sender = service.runtime_message_sender_identity(&later).unwrap();
        reply(
            &mut service,
            &parent,
            &sender,
            "actual-response-after-partial-send",
            Some(&message),
        );
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        execute(
            &mut service,
            &parent,
            AgentAction {
                id: "close-partial-send-child".into(),
                payload: AgentActionPayload::CloseAgent {
                    agent_id: initial.agent_id.clone(),
                },
            },
        );
        finish(&mut service, &parent);
    }
}

/// A genuine primary-approved send is irrevocable before a receiver recovery
/// fault. Its stored action must be Succeeded and the original approval consumed
/// while receiver recovery is deferred. Reusing the decision, including a
/// changed candidate tracking flag, cannot resend or rewrite accepted intent.
#[test]
fn request_bound_approved_partial_send_consumes_decision_and_freezes_intent() {
    for tracked in [false, true] {
        let (mut service, parent, _, primary) = fixture_with_primary_provider_claims();
        service.configure_agent_scheduler_limit(2).unwrap();
        let initial = spawn_child(&mut service, &parent);
        assert!(
            service
                .validate_native_agent_surface_startup(&initial.pane_id)
                .unwrap()
        );
        service.start_ready_agent_turns().unwrap();
        service
            .complete_running_agent_turn_and_start_ready(
                &initial,
                AgentTurnState::Completed,
                "idle recipient before actual approval",
            )
            .unwrap();
        service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::Ask));
        let blocked = attempt_send(&mut service, &parent, &initial, tracked).unwrap();
        assert_eq!(blocked.action_results[0].status, ActionStatus::Blocked);
        let approval = service
            .blocked_approvals()
            .pending()
            .into_iter()
            .find(|approval| {
                approval.pane_id == parent.pane_id && approval.action_kind == "send_message"
            })
            .unwrap()
            .clone();
        service
            .integration
            .blocked_approvals_mut()
            .decide_with_client_at(
                &approval.id,
                mez_agent::permissions::ApprovalDecision::Approve,
                None,
                Some(primary.as_str().to_string()),
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let decided = service
            .blocked_approvals()
            .get(&approval.id)
            .unwrap()
            .clone();
        service.fail_next_peer_message_receive_after_context_storage_for_tests();
        let sequence = service.message_service().next_message_sequence();
        assert_eq!(
            service
                .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
                .unwrap(),
            Some(1)
        );
        let diagnostic = service.agent_pane_trace_log_text(&parent.pane_id).unwrap();
        assert!(diagnostic.contains("deferred after acceptance"));
        assert!(diagnostic.contains("after context storage"));
        assert_eq!(
            service.message_service().next_message_sequence(),
            sequence + 1
        );
        let accepted = service.agent_turn_executions()[&parent.turn_id].clone();
        assert_eq!(
            accepted.action_results[0].status,
            ActionStatus::Succeeded,
            "primary-approved send remained retryable after actual irreversible acceptance"
        );
        let outcome: serde_json::Value = serde_json::from_str(
            accepted.action_results[0]
                .structured_content_json
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(outcome["delivery_status"], "accepted");
        assert_eq!(outcome["expect_reply"], tracked);
        let message = outcome["message_id"].as_str().unwrap().to_string();
        assert_eq!(
            service.tracked_message_request_usage().0.0,
            usize::from(tracked)
        );
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        let transport = service.message_service().snapshot_state();
        assert_eq!(
            service
                .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
                .unwrap(),
            None
        );
        let mut changed = accepted.clone();
        let AgentActionPayload::SendMessage { expect_reply, .. } =
            &mut changed.response.action_batch.as_mut().unwrap().actions[0].payload
        else {
            unreachable!()
        };
        *expect_reply = !tracked;
        service
            .agent_turn_executions_mut()
            .insert(parent.turn_id.clone(), changed);
        assert_eq!(
            service
                .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
                .unwrap(),
            None
        );
        assert_eq!(service.message_service().snapshot_state(), transport);
        assert_eq!(
            service.tracked_message_request_usage().0.0,
            usize::from(tracked)
        );
        service
            .agent_turn_executions_mut()
            .insert(parent.turn_id.clone(), accepted);
        for tick in 1..=3 {
            service
                .apply_peer_message_delivery_timer(current_unix_millis(), tick)
                .unwrap();
        }
        claim_native_provider_continuation(&mut service, &parent);
        wait(&mut service, &parent, &message, None);
        if !tracked {
            assert!(
                service.agent_turn_executions()[&parent.turn_id].action_results[0]
                    .content_text()
                    .contains("request wait unavailable")
            );
            assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        }
        let later = service
            .agent_turn_ledger()
            .latest_turn_for_pane(&initial.pane_id)
            .unwrap()
            .clone();
        let sender = service.runtime_message_sender_identity(&later).unwrap();
        reply(
            &mut service,
            &parent,
            &sender,
            "approved-partial-reply",
            Some(&message),
        );
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        service
            .dispatch_runtime_pane_close(
                &primary,
                &serde_json::json!({"pane_id":initial.pane_id,"force":true}).to_string(),
            )
            .unwrap();
        finish(&mut service, &parent);
        service.terminate_all_pane_processes().unwrap();
    }
}
