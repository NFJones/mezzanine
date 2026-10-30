//! Request-bound lifecycle recovery through genuine actor-owned provider leases.
//!
//! Each response uses real preparation, policy planning and completion ingress.
//! The original parent consumes canonical evidence before its next admitted
//! inference, with one scheduler slot and no model-authored reply. Typed provider
//! persistence work executes against fixture-owned stores and settles through
//! its real generation; this fixture does not claim full archive-worker coverage.

use super::*;
use crate::host::async_runtime::actor::coalesce::async_runtime_current_unix_millis;

/// Runs existing append-receipt admission and dispatches only actual worker
/// requests received through bounded actor ingress. A notifier and watchdog
/// replace sleeps; no invented success acknowledgement releases the lane.
async fn admit_receipts(actor: &mut DialogueActor) {
    let _ = actor.start_transcript_receipt_admission(
        0,
        crate::host::async_runtime::actor::TranscriptReceiptReply::Startup,
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while actor.side_effect_routes.has_held_transcript_receipts() {
            let notify = actor.request_ingress_notify.clone();
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let request = {
                let mut ingress = actor.request_ingress.lock().unwrap();
                ingress
                    .normal_requests
                    .pop_front()
                    .or_else(|| ingress.maintenance_requests.pop_front())
                    .or_else(|| ingress.urgent_requests.pop_front())
                    .or_else(|| ingress.interactive_requests.pop_front())
            };
            if let Some(request) = request {
                if let AsyncRuntimeRequest::CompleteTranscriptReceipts { results, .. } =
                    &request.envelope.request
                {
                    assert!(!results.is_empty());
                    assert!(
                        results.iter().all(|(_, result)| result.is_ok()),
                        "{results:?}"
                    );
                }
                assert!(!Box::pin(actor.handle_request(request.envelope.request)).await);
            } else {
                notified.await;
            }
        }
    })
    .await
    .expect("actual transcript receipt admission must finish");
}

/// Executes actual provider-persistence work and its typed settlement, including
/// fork preparation. Other persistence families are outside this worker seam;
/// they cannot manufacture a provider settlement or grant a request dependency.
pub(super) async fn settle_provider_work(actor: &mut DialogueActor) -> usize {
    let mut settled = 0;
    for _ in 0..8 {
        admit_receipts(actor).await;
        let effects = actor.drain_persistence_side_effects(128).unwrap();
        let mut found = false;
        for effect in effects {
            if let RuntimeSideEffect::SettleAgentProviderPersistence { work } = effect {
                found = true;
                let outcome = tokio::task::spawn_blocking(move || {
                    crate::runtime::execute_agent_provider_persistence_work(*work)
                })
                .await
                .unwrap()
                .unwrap();
                let mut batch = RuntimeEventBatch::new();
                batch.push(RuntimeEvent::AgentProvider(
                    AgentProviderEvent::PersistenceSettled {
                        outcome: Box::new(outcome),
                    },
                ));
                assert_eq!(
                    actor
                        .apply_runtime_event_batch(batch)
                        .await
                        .unwrap()
                        .applied,
                    1
                );
                settled += 1;
            }
        }
        if !found {
            return settled;
        }
    }
    panic!("provider persistence failed to reach a bounded quiescent boundary");
}

/// Admits exactly one real lease, applies its response and settles any resulting
/// typed provider persistence. Duplicate ingress must not repeat accepted effects.
async fn step(
    actor: &mut DialogueActor,
    turn: &mez_agent::AgentTurnRecord,
    mut actions: Vec<AgentAction>,
    generations: &mut std::collections::BTreeSet<u64>,
) -> (crate::runtime::RuntimeAgentProviderDispatch, usize) {
    for (index, action) in actions.iter_mut().enumerate() {
        action.id = format!("action-{}", index + 1);
    }
    let dispatch = claim(actor, turn).await;
    assert!(generations.insert(dispatch.claim_generation));
    let execution = execute(&dispatch, &response(actions)).await;
    assert_eq!(complete(actor, &dispatch, execution.clone()).await, 1);
    assert_eq!(complete(actor, &dispatch, execution).await, 0);
    let settled = settle_provider_work(actor).await;
    (dispatch, settled)
}

/// Tracks a genuine accepted send and reads its runtime-returned message ID.
async fn request(
    actor: &mut DialogueActor,
    parent: &mez_agent::AgentTurnRecord,
    peer: &str,
    generations: &mut std::collections::BTreeSet<u64>,
) -> String {
    let mut action = send("request-send", peer, "ACTOR_EXACT_REQUEST");
    if let AgentActionPayload::SendMessage { expect_reply, .. } = &mut action.payload {
        *expect_reply = true;
    }
    step(actor, parent, vec![action], generations).await;
    let execution = &actor.service.agent_turn_executions()[&parent.turn_id];
    let result = execution
        .action_results
        .iter()
        .find(|r| r.action_type == "send_message")
        .unwrap();
    assert_eq!(result.status, mez_agent::ActionStatus::Succeeded);
    let value: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    value["message_id"].as_str().unwrap().into()
}

/// Parks on the accepted ID in a separate provider batch, freeing the one slot.
async fn park(
    actor: &mut DialogueActor,
    parent: &mez_agent::AgentTurnRecord,
    message: &str,
    generations: &mut std::collections::BTreeSet<u64>,
) {
    step(
        actor,
        parent,
        vec![AgentAction {
            id: "request-wait".into(),
            payload: AgentActionPayload::Wait {
                for_message: Some(message.into()),
                from_agent: None,
            },
        }],
        generations,
    )
    .await;
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
}

/// Verifies exact typed wait settlement before a later response replaces the
/// stored execution. Ordinary bridge mail alone cannot produce this witness.
fn assert_outcome(
    actor: &DialogueActor,
    parent: &mez_agent::AgentTurnRecord,
    reason: &str,
    progress: bool,
) {
    let execution = &actor.service.agent_turn_executions()[&parent.turn_id];
    let waits: Vec<_> = execution
        .action_results
        .iter()
        .filter(|r| r.action_type == "wait")
        .collect();
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].status, mez_agent::ActionStatus::Succeeded);
    let value: serde_json::Value =
        serde_json::from_str(waits[0].structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(value["state"], reason);
    assert_eq!(value["dependency_satisfied"], true);
    assert_eq!(value["progress_witness"], progress);
}

/// Requires successful useful work from the actual settled worker result,
/// rather than interpreting the existence of a persistence event as success.
fn assert_memory_saved(actor: &DialogueActor, parent: &mez_agent::AgentTurnRecord) {
    let execution = &actor.service.agent_turn_executions()[&parent.turn_id];
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(execution.action_results[0].action_type, "memory_store");
    assert_eq!(
        execution.action_results[0].status,
        mez_agent::ActionStatus::Succeeded,
        "{:?}",
        execution.action_results[0]
    );
}

/// Gives the continued parent useful durable work through the actual typed
/// persistence owner, rather than treating mere scheduler wake as completion.
fn record_evidence() -> AgentAction {
    AgentAction {
        id: "request-evidence".into(),
        payload: AgentActionPayload::MemoryStore {
            kind: "fact".into(),
            priority: Some(50),
            scope: Some("global".into()),
            keywords: vec!["request-actor".into()],
            content: "used exact lifecycle evidence to finish original task".into(),
            expires_in_days: Some(1),
        },
    }
}

/// One real scheduler slot must run the original parent, its exact persistent
/// child and the same continued parent. The child emits only a final result,
/// never a send_message response. Actual terminal evidence reaches the parent's
/// claimed context once, and a normal memory action crosses typed persistence
/// before the same task completes; duplicate ingress grants no second handoff.
#[tokio::test(flavor = "current_thread")]
async fn request_bound_actor_owned_success_without_reply_finishes_same_parent() {
    let (mut actor, parent, child, mut generations, _) =
        actor_fixture_with_lifetime(1, Vec::new(), mez_agent::SubagentLifetime::Persistent).await;
    let message = request(&mut actor, &parent, &child.agent_id, &mut generations).await;
    park(&mut actor, &parent, &message, &mut generations).await;
    let (dispatch, _) = step(
        &mut actor,
        &child,
        vec![done("request-child-result", "ACTOR_OWNED_TERMINAL_OUTPUT")],
        &mut generations,
    )
    .await;
    assert!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .any(|b| b.content.contains("ACTOR_EXACT_REQUEST"))
    );
    actor
        .service
        .deliver_pending_runtime_agent_messages(async_runtime_current_unix_millis())
        .unwrap();
    assert_outcome(&actor, &parent, "peer_terminal", true);
    let (resumed, persisted) = step(
        &mut actor,
        &parent,
        vec![record_evidence()],
        &mut generations,
    )
    .await;
    assert!(
        persisted > 0,
        "useful continuation must actually settle provider persistence; enabled={} execution={:?}",
        resumed.memory_actions_enabled,
        actor
            .service
            .agent_turn_executions()
            .get(&parent.turn_id)
            .map(|e| &e.action_results)
    );
    assert_eq!(resumed.turn.turn_id, parent.turn_id);
    assert_eq!(
        resumed
            .context
            .durable()
            .blocks()
            .iter()
            .filter(|b| b.source == mez_agent::ContextSourceKind::PeerMessage
                && b.label.starts_with("request-handoff:")
                && b.content.contains("ACTOR_OWNED_TERMINAL_OUTPUT")
                && b.content.contains(&child.turn_id))
            .count(),
        1
    );
    assert_memory_saved(&actor, &parent);
    // The distinct existing task-result bridge remains ordinary lifecycle mail;
    // duplicate provider ingress must not create a second typed handoff.
    assert_eq!(
        resumed
            .context
            .durable()
            .blocks()
            .iter()
            .filter(|b| b.source == mez_agent::ContextSourceKind::PeerMessage
                && !b.label.starts_with("request-handoff:")
                && b.content.contains("ACTOR_OWNED_TERMINAL_OUTPUT"))
            .count(),
        1
    );
    assert_eq!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    step(
        &mut actor,
        &parent,
        vec![done("request-parent-final", "used owned output")],
        &mut generations,
    )
    .await;
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), 2);
    assert_eq!(generations, (1..=6).collect());
}

/// An unrelated owned task completion cannot satisfy a selected nonowned
/// registration. Genuine retirement then supplies only opaque unavailability,
/// permits useful alternative work through typed persistence and completes the
/// same parent. No private peer task, output or process information is exposed.
#[tokio::test(flavor = "current_thread")]
async fn request_bound_actor_nonowned_retirement_permits_same_parent_alternative() {
    let (mut actor, parent, child, mut generations, _) =
        actor_fixture_with_lifetime(1, Vec::new(), mez_agent::SubagentLifetime::Persistent).await;
    let peer = actor
        .service
        .ensure_runtime_message_identity(
            "agent-actor-independent-peer",
            None,
            "worker",
            &[],
            async_runtime_current_unix_millis(),
        )
        .unwrap();
    let message = request(
        &mut actor,
        &parent,
        peer.agent_id.as_str(),
        &mut generations,
    )
    .await;
    park(&mut actor, &parent, &message, &mut generations).await;
    step(
        &mut actor,
        &child,
        vec![done(
            "request-unrelated-child",
            "PRIVATE_UNRELATED_CHILD_OUTPUT",
        )],
        &mut generations,
    )
    .await;
    actor
        .service
        .deliver_pending_runtime_agent_messages(async_runtime_current_unix_millis())
        .unwrap();
    // Ordinary unrelated mail may resume inference but never consumes the edge.
    assert_ne!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    if actor
        .service
        .agent_turn_ledger()
        .turn(&parent.turn_id)
        .unwrap()
        .state
        == AgentTurnState::Running
    {
        park(&mut actor, &parent, &message, &mut generations).await;
    }
    assert!(
        actor
            .service
            .retire_runtime_message_identity_after_native_commits(&peer.agent_id)
            .unwrap()
    );
    actor
        .service
        .deliver_pending_runtime_agent_messages(async_runtime_current_unix_millis())
        .unwrap();
    assert_outcome(&actor, &parent, "peer_unavailable", false);
    let (resumed, persisted) = step(
        &mut actor,
        &parent,
        vec![record_evidence()],
        &mut generations,
    )
    .await;
    assert!(
        persisted > 0,
        "enabled={} execution={:?}",
        resumed.memory_actions_enabled,
        actor
            .service
            .agent_turn_executions()
            .get(&parent.turn_id)
            .map(|e| &e.action_results)
    );
    assert_memory_saved(&actor, &parent);
    assert_eq!(resumed.turn.turn_id, parent.turn_id);
    let outcomes: Vec<_> = resumed
        .context
        .durable()
        .blocks()
        .iter()
        .filter(|b| {
            b.source == mez_agent::ContextSourceKind::PeerMessage
                && b.label.starts_with("request-handoff:")
                && b.content
                    .contains("The selected original dependency is unavailable")
        })
        .collect();
    assert_eq!(outcomes.len(), 1);
    assert!(
        !outcomes[0]
            .content
            .contains("PRIVATE_UNRELATED_CHILD_OUTPUT")
    );
    assert!(!outcomes[0].content.contains(&child.turn_id));
    assert_eq!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    step(
        &mut actor,
        &parent,
        vec![done(
            "request-alternative-final",
            "completed useful alternative after unavailable peer",
        )],
        &mut generations,
    )
    .await;
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), 2);
    assert_eq!(generations, (1..=6).collect());
}
