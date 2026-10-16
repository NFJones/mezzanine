//! Exact-owned idle failure receipts wait for checked parent bookkeeping.
//!
//! Ownership comes from real persistent spawn and terminal child settlement,
//! never from a fabricated envelope. Replacement controls ensure a late result
//! cannot turn retained evidence into authority over a new parent conversation.

use super::*;

/// A locally proven child failure must retain its original receipt while its
/// completed parent's chronology is queued or claimed. Checked settlement
/// admits exactly one ordinary response; conversation removal/replacement
/// instead retires the old receipt and cannot wake replacement work.
#[test]
fn runtime_owned_child_idle_failure_defers_parent_bookkeeping() {
    for mode in ["queued", "claimed", "replaced", "removed"] {
        let (mut service, parent, child) = owned_child_fixture(false);
        service.use_transcript_effect_adapter();
        let provider = RuntimeBatchProvider {
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".into(),
                model: "test".into(),
                raw_text: "parent finished".into(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "parent idle while reviewer works".into(),
                    actions: vec![mez_agent::AgentAction {
                        id: "parent-final".into(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: "parent finished".into(),
                            content_type: "text/plain; charset=utf-8".into(),
                        },
                    }],
                }),
                provider_transcript_events: Vec::new(),
            },
        };
        service
            .execute_agent_turn_with_provider(
                &parent.turn_id,
                &provider,
                runtime_model_profile("runtime-batch", "test"),
            )
            .unwrap();
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Completed
        );
        assert!(
            service
                .persistence
                .bookkeeping_pending(&parent.conversation_id)
        );
        let mut work = (mode != "queued").then(|| {
            service
                .claim_bookkeeping_candidates()
                .into_iter()
                .find(|work| work.candidate.turn.turn_id == parent.turn_id)
                .unwrap()
        });
        fail_owned_child(&mut service, &child);
        let before = service.agent_turn_ledger().turns().len();
        for generation in 1..=3 {
            let transition = service
                .apply_peer_message_delivery_timer(current_unix_millis(), generation)
                .unwrap();
            assert!(!transition.applied);
            assert!(transition.side_effects.is_empty());
            assert_eq!(service.agent_turn_ledger().turns().len(), before);
            assert_eq!(service.agent_peer_message_turn_count(&parent.agent_id), 0);
        }
        if mode == "replaced" {
            service.agent_shell_store_mut().remove_session("%1");
            service
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap();
        } else if mode == "removed" {
            service.agent_shell_store_mut().remove_session("%1");
        }
        let work = work.take().unwrap_or_else(|| {
            service
                .claim_bookkeeping_candidates()
                .into_iter()
                .find(|work| work.candidate.turn.turn_id == parent.turn_id)
                .unwrap()
        });
        let history = work.execute();
        assert!(
            service
                .complete_bookkeeping_candidate(work, history)
                .unwrap()
        );
        service
            .deliver_pending_runtime_agent_messages(current_unix_millis())
            .unwrap();
        if matches!(mode, "replaced" | "removed") {
            assert_eq!(service.agent_turn_ledger().turns().len(), before);
        } else {
            let response = service
                .agent_turn_ledger()
                .turns()
                .iter()
                .rev()
                .find(|turn| turn.agent_id == parent.agent_id)
                .unwrap()
                .clone();
            assert_ne!(response.turn_id, parent.turn_id);
            assert_eq!(response.conversation_id, parent.conversation_id);
            assert_eq!(owned_failure_count(&service, &response), 1);
            assert_eq!(service.agent_turn_ledger().turns().len(), before + 1);
            service
                .deliver_pending_runtime_agent_messages(current_unix_millis())
                .unwrap();
            assert_eq!(service.agent_turn_ledger().turns().len(), before + 1);
            assert_eq!(service.agent_peer_message_turn_count(&parent.agent_id), 1);
        }
        service.terminate_all_pane_processes().unwrap();
    }
}
