//! Shared native-action/bridge source accounting and actual root FIFO selection.
//!
//! Count/byte stress builds genuinely policy-planned slots in one canonical
//! execution; it does not assert that a provider may return an arbitrarily large
//! action batch. The FIFO control uses an actual configured request and offline
//! provider runner, then the real actor/root commit lane in both source orders.

use super::native_bridge::{bridge, cleanup, shutdown};
use super::*;
use crate::host::async_runtime::AsyncRuntimeSessionActor;
use mez_agent::{ActionStatus, AgentTurnExecution, AgentTurnRecord};

async fn planned(
    actor: &mut AsyncRuntimeSessionActor,
    turn: &AgentTurnRecord,
    target: &SenderIdentity,
) -> (
    crate::runtime::RuntimeAgentProviderDispatch,
    AgentTurnExecution,
) {
    let id = mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap();
    let dispatch = actor
        .service
        .claim_configured_agent_provider_task(&id, &turn.turn_id)
        .unwrap()
        .expect("actual configured native request");
    let (_, dispatch, execution) = super::native_actions::plan(dispatch, target, false).await;
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    (dispatch, execution)
}

/// Both orders cross actual canonical source admission and physical acceptance.
/// The first source must take the first occurrence; a later source cannot gain
/// priority from being a bridge or from being a native action.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_bridge_and_action_share_fifo_in_both_orders() {
    for bridge_first in [false, true] {
        let fixture = Fixture::new(true);
        let (handle, mut actor, root, turn, target, _) =
            super::native_actions::actor_fixture_with_transcript(
                &fixture,
                mez_agent::ApprovalPolicy::AutoAllow,
                true,
            )
            .await;
        // Establish the complete native sender before capturing either source.
        // Enriching an incomplete registration later must legitimately fence a
        // source; that authority change is not a FIFO failure.
        actor
            .service
            .ensure_runtime_message_identity(
                &turn.agent_id,
                None,
                "agent",
                &["agent-harness"],
                crate::runtime::current_unix_millis(),
            )
            .unwrap();
        actor
            .enable_message_payload_spill(Default::default())
            .unwrap();
        actor.poll_message_custody_for_tests();
        assert_eq!(
            actor.observe_message_custody_for_tests().await.unwrap(),
            CustodyWorkerStatus::NativeContinuityAwaitingAdoption
        );
        drop(root);
        let (dispatch, execution) = planned(&mut actor, &turn, &target).await;
        let sender = mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap();
        let registration = actor
            .service
            .message_service()
            .capture_sender_registration(&sender)
            .unwrap();
        let mut envelope = bridge(&actor, &turn, "fifo-bridge");
        envelope.recipient = Recipient::Agent(target.agent_id.clone());
        if bridge_first {
            assert!(
                !actor
                    .service
                    .accept_runtime_bridge_message(
                        &turn,
                        envelope.clone(),
                        crate::runtime::current_unix_millis()
                    )
                    .unwrap()
            );
        }
        actor
            .service
            .apply_agent_provider_execution_async_for_tests(
                &turn,
                &dispatch.model_profile,
                execution,
            )
            .await
            .unwrap();
        assert!(
            actor
                .service
                .message_service()
                .sender_registration_is_current(&registration)
        );
        if !bridge_first {
            assert!(
                !actor
                    .service
                    .accept_runtime_bridge_message(
                        &turn,
                        envelope,
                        crate::runtime::current_unix_millis()
                    )
                    .unwrap()
            );
        }
        assert_eq!(actor.service.pending_stored_message_actions().len(), 1);
        assert_eq!(
            actor.service.stored_bridge_message_has_priority(),
            bridge_first
        );
        let mut types = Vec::new();
        for _ in 0..16 {
            actor.poll_message_custody_for_tests();
            actor.observe_message_custody_for_tests().await.unwrap();
            types = actor
                .service
                .message_service()
                .receive_subscribed(&target.agent_id, crate::runtime::current_unix_millis(), 10)
                .unwrap()
                .messages
                .iter()
                .map(|message| message.envelope.message_type.clone())
                .collect();
            if types.len() == 2 {
                break;
            }
        }
        shutdown(&mut actor).await;
        cleanup(&fixture, handle, actor).await;
        let expected = if bridge_first {
            vec!["task_status", "send"]
        } else {
            vec!["send", "task_status"]
        };
        assert_eq!(types, expected);
    }
}

/// Stress only the semantic source boundary, not provider batch limits. Every
/// result slot is authorized by the ordinary planner and belongs to the exact
/// retained canonical action. No lower reservation or storage write may occur.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_bridge_and_actions_share_count_and_byte_budget() {
    for bytes in [false, true] {
        for bridge_first in [false, true] {
            let fixture = Fixture::new(true);
            let (handle, mut actor, root, turn, target, _) =
                super::native_actions::actor_fixture_with_transcript(
                    &fixture,
                    mez_agent::ApprovalPolicy::AutoAllow,
                    true,
                )
                .await;
            drop(root);
            let (_, mut execution) = planned(&mut actor, &turn, &target).await;
            let mut envelope = bridge(&actor, &turn, "budget-bridge");
            envelope.recipient = Recipient::Agent(target.agent_id.clone());
            if bytes {
                envelope.payload = format!(
                    r#"{{"task_id":"{}","state":"running","summary":"{}"}}"#,
                    turn.turn_id,
                    "b".repeat(40_000)
                );
            }
            if bridge_first {
                assert!(
                    !actor
                        .service
                        .accept_runtime_bridge_message(
                            &turn,
                            envelope.clone(),
                            crate::runtime::current_unix_millis()
                        )
                        .unwrap()
                );
            }
            // Nine larger bodies reach the same aggregate byte boundary while
            // avoiding 32 repeated full canonical transitions in this control.
            let count = if bytes { 9 } else { 64 };
            let template = execution.response.action_batch.as_ref().unwrap().actions[0].clone();
            let actions = (0..count)
                .map(|index| {
                    let mut action = template.clone();
                    action.id = format!("bounded-source-{index}");
                    if bytes
                        && let mez_agent::AgentActionPayload::SendMessage { payload, .. } =
                            &mut action.payload
                    {
                        *payload = "n".repeat(925_000);
                    }
                    action
                })
                .collect::<Vec<_>>();
            execution.action_results = actions
                .iter()
                .map(|action| {
                    mez_agent::plan_action_result(
                        &turn,
                        action,
                        mez_agent::ActionPlanningInput {
                            approval_policy: mez_agent::ApprovalPolicy::AutoAllow,
                            message_rule_decision: Some(
                                mez_agent::permissions::RuleDecision::Allow,
                            ),
                            ..Default::default()
                        },
                    )
                    .unwrap()
                })
                .collect();
            assert!(
                execution
                    .action_results
                    .iter()
                    .all(|result| result.status == ActionStatus::Running)
            );
            execution.response.action_batch.as_mut().unwrap().actions = actions;
            actor
                .service
                .append_agent_execution_chronology(&turn, &execution)
                .unwrap();
            let sequence = actor.service.message_service().next_message_sequence();
            let writes = fixture.probe.attempts.load(Ordering::SeqCst);
            actor
                .service
                .execute_running_message_actions_for_turn(&turn, &mut execution)
                .unwrap();
            let queued = actor.service.pending_stored_message_actions().len();
            if !bridge_first {
                assert!(
                    actor
                        .service
                        .accept_runtime_bridge_message(
                            &turn,
                            envelope,
                            crate::runtime::current_unix_millis()
                        )
                        .is_err()
                );
            }
            let expected = count - usize::from(bridge_first);
            assert_eq!(
                queued, expected,
                "bytes={bytes} bridge_first={bridge_first}"
            );
            assert_eq!(
                actor.service.has_pending_stored_bridge_message(),
                bridge_first
            );
            assert_eq!(
                execution
                    .action_results
                    .iter()
                    .filter(|result| result.status == ActionStatus::Failed)
                    .count(),
                usize::from(bridge_first)
            );
            assert_eq!(
                actor.service.message_service().next_message_sequence(),
                sequence
            );
            assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), writes);
            assert!(!actor.service.message_service().has_unpublished_acceptance());
            // These sources have never issued. Stop must cancel exact native
            // slots and the bridge without creating accepted storage evidence.
            shutdown(&mut actor).await;
            assert!(!actor.service.has_pending_stored_message_actions());
            cleanup(&fixture, handle, actor).await;
        }
    }
}
