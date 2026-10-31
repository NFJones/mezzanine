//! Request-bound lifecycle recovery through genuine actor-owned provider leases.
//!
//! Each response uses real preparation, policy planning and completion ingress.
//! Original parents consume canonical evidence before their next admitted
//! inference. Workflows cover one-slot scheduling, ordinary correlated replies,
//! exact lifecycle outcomes and overlapping native-result custody. Typed provider
//! persistence executes against fixture-owned stores and settles through its
//! real generation. The native overlap case also checks original bookkeeping;
//! these seams do not claim full archive-worker coverage.

use super::*;
use crate::host::async_runtime::actor::coalesce::async_runtime_current_unix_millis;

/// An exact owned terminal outcome may arrive while the original parent owns
/// an actual native command result. It must neither settle that Running action
/// nor admit a concurrent provider. The real native callback completes first;
/// only then may a genuine parent claim consume both exact result sources and
/// perform useful typed persistence before completing the same original task.
#[tokio::test(flavor = "current_thread")]
async fn request_bound_actor_owned_outcome_preserves_running_native_result_gate() {
    let (mut actor, parent, child, mut generations, _) =
        actor_fixture_with_lifetime(2, Vec::new(), mez_agent::SubagentLifetime::Persistent).await;
    let message = request(&mut actor, &parent, &child.agent_id, &mut generations).await;
    park(&mut actor, &parent, &message, &mut generations).await;
    unrelated_mail(&mut actor, &parent);
    step(
        &mut actor,
        &parent,
        vec![AgentAction {
            id: "native-pending".into(),
            payload: AgentActionPayload::ShellCommand {
                summary: "Read independent native evidence".into(),
                command: "printf NATIVE_SIBLING_RESULT".into(),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        }],
        &mut generations,
    )
    .await;
    let result = actor.service.agent_turn_executions()[&parent.turn_id].action_results[0].clone();
    assert_eq!(result.status, mez_agent::ActionStatus::Running);
    let native = actor
        .service
        .claim_native_shell_action(&parent.turn_id, &result.action_id)
        .unwrap()
        .unwrap();
    assert!(!native.capability_probe_only);
    let outcome = crate::runtime::execute_native_shell_dispatch(native);
    step(
        &mut actor,
        &child,
        vec![done(
            "owned-native-child-final",
            "NATIVE_SIBLING_CHILD_RESULT",
        )],
        &mut generations,
    )
    .await;
    actor
        .service
        .deliver_pending_runtime_agent_messages(async_runtime_current_unix_millis())
        .unwrap();
    assert_eq!(
        actor.service.agent_turn_executions()[&parent.turn_id].action_results[0],
        result
    );
    assert!(
        !actor
            .service
            .agent_provider_task_is_pending(&parent.turn_id)
    );
    assert_eq!(actor.service.tracked_message_request_usage().0.0, 1);
    assert!(actor.service.complete_native_shell_action(outcome).unwrap());
    let mut candidates = actor.service.claim_bookkeeping_candidates();
    assert_eq!(candidates.len(), 1);
    let work = candidates.remove(0);
    let read = work.clone();
    let history = tokio::task::spawn_blocking(move || read.execute())
        .await
        .unwrap();
    assert!(history.is_ok(), "{history:?}");
    assert!(
        !Box::pin(
            actor.handle_request(AsyncRuntimeRequest::CompleteBookkeepingCandidate {
                work,
                history,
            })
        )
        .await
    );
    admit_receipts(&mut actor).await;
    actor
        .service
        .deliver_pending_runtime_agent_messages(async_runtime_current_unix_millis())
        .unwrap();
    let (dispatch, persisted) = step(
        &mut actor,
        &parent,
        vec![record_evidence()],
        &mut generations,
    )
    .await;
    assert!(persisted > 0);
    assert_memory_saved(&actor, &parent);
    assert_eq!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .filter(
                |block| block.source == mez_agent::ContextSourceKind::ActionResult
                    && block.content.contains("NATIVE_SIBLING_RESULT")
            )
            .count(),
        1
    );
    let handoffs = dispatch
        .context
        .durable()
        .blocks()
        .iter()
        .filter(|block| block.label.starts_with("request-handoff:"))
        .collect::<Vec<_>>();
    assert_eq!(handoffs.len(), 1);
    assert!(handoffs[0].content.contains("NATIVE_SIBLING_CHILD_RESULT"));
    assert_eq!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    step(
        &mut actor,
        &parent,
        vec![done(
            "native-parent-done",
            "used both exact native and owned results",
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
    assert_eq!(generations, (1..=7).collect());
    assert_eq!(
        actor
            .service
            .message_service()
            .snapshot_state()
            .accepted_messages
            .iter()
            .filter(|row| row.envelope.id == message)
            .count(),
        1
    );
}

/// A real provider response blocks on Ask, releases its sole scheduler slot,
/// and resumes only after the primary's actual approval. Failure after accepted
/// send publication must retain the Succeeded result in its original transcript
/// handoff even when scheduler startup retires the live turn. Reusing that exact
/// decision cannot resend or change the accepted request's frozen intent.
#[tokio::test(flavor = "current_thread")]
async fn request_bound_actor_approved_send_retains_result_after_late_publication_failure() {
    let (mut actor, parent, child, mut generations, primary) =
        actor_fixture_with_lifetime(1, Vec::new(), mez_agent::SubagentLifetime::Persistent).await;
    let peer = actor
        .service
        .ensure_runtime_message_identity(
            "agent-late-approved-send-peer",
            None,
            "worker",
            &[],
            async_runtime_current_unix_millis(),
        )
        .unwrap();
    actor
        .service
        .set_pane_approval_policy_override(&parent.pane_id, Some(mez_agent::ApprovalPolicy::Ask));
    let mut action = send(
        "late-approved-send",
        peer.agent_id.as_str(),
        "LATE_APPROVED_SEND",
    );
    if let AgentActionPayload::SendMessage { expect_reply, .. } = &mut action.payload {
        *expect_reply = true;
    }
    step(&mut actor, &parent, vec![action], &mut generations).await;
    assert_eq!(
        actor.service.agent_turn_executions()[&parent.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Blocked
    );
    let approval = actor
        .service
        .blocked_approvals()
        .pending()
        .into_iter()
        .find(|approval| {
            approval.pane_id == parent.pane_id && approval.action_kind == "send_message"
        })
        .unwrap()
        .clone();
    step(
        &mut actor,
        &child,
        vec![done("idle-before-approval", "initial child finished")],
        &mut generations,
    )
    .await;
    actor
        .service
        .fail_next_approved_message_late_publication_for_tests();
    let mut connection =
        crate::control::ControlConnectionState::trusted_existing_client(primary.clone());
    let body = serde_json::json!({"jsonrpc":"2.0", "id":"late-approved-send",
        "method":"approval/decide", "params":{"approval_id":approval.id,
        "decision":"approve", "idempotency_key":"late-approved-send"}})
    .to_string();
    let input = crate::control::encode_control_body(&body);
    let (output, _) = actor
        .service
        .handle_control_input_for_connection(&input, 4096, &mut connection)
        .unwrap();
    let (error, _) = crate::control::decode_control_frame(&output, 4096).unwrap();
    assert!(
        error.contains("approved message late publication failure"),
        "{error}"
    );
    let decided = actor
        .service
        .blocked_approvals()
        .get(&approval.id)
        .unwrap()
        .clone();
    assert_eq!(
        decided.decision,
        Some(mez_agent::permissions::ApprovalDecision::Approve)
    );
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Failed
    );
    assert!(
        !actor
            .service
            .agent_turn_executions()
            .contains_key(&parent.turn_id)
    );
    assert!(
        !actor
            .service
            .agent_provider_task_is_pending(&parent.turn_id)
    );
    let state = actor.service.message_service().snapshot_state();
    let accepted = state
        .accepted_messages
        .iter()
        .filter(|row| row.envelope.payload == "LATE_APPROVED_SEND")
        .collect::<Vec<_>>();
    assert_eq!(accepted.len(), 1);
    let message = accepted[0].envelope.id.clone();
    let snapshot = actor
        .service
        .retained_completion_snapshot_for_tests(&parent.turn_id)
        .unwrap()
        .expect("the original transcript must retain its accepted result through cleanup");
    let snapshot: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
    assert_eq!(snapshot["turn"], parent.turn_id);
    assert_eq!(snapshot["conversation"], parent.conversation_id);
    let results = snapshot["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["role"] == "tool")
        .filter_map(|entry| entry["content"].as_str())
        .filter(|content| {
            content.contains(&message)
                && content.contains("send_message")
                && content.contains("succeeded")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        results.len(),
        1,
        "late publication lost the canonical Succeeded result after actual acceptance"
    );
    let (_, data) = results[0].split_once("\ndata: ").unwrap();
    let result: serde_json::Value = serde_json::from_str(data).unwrap();
    assert_eq!(result["message_id"], message);
    assert_eq!(result["expect_reply"], true);
    assert_eq!(
        actor
            .service
            .resume_approved_blocked_agent_action(&approval.id, &decided, &primary,)
            .unwrap(),
        None
    );
    assert_eq!(actor.service.message_service().snapshot_state(), state);
    assert_eq!(generations, (1..=3).collect());
}

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

/// Commits genuine unrelated peer mail through authenticated lower acceptance.
/// The fresh registration has no request correlation or lifecycle authority.
fn unrelated_mail(actor: &mut DialogueActor, parent: &mez_agent::AgentTurnRecord) {
    use mez_agent::messaging::{Envelope, MessageScope, Recipient};
    let now = async_runtime_current_unix_millis();
    let sender = actor
        .service
        .ensure_runtime_message_identity("agent-actor-unrelated-observer", None, "worker", &[], now)
        .unwrap();
    actor
        .service
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "actor-unrelated-mail".into(),
                message_type: "send".into(),
                time: format!("fixture:{now}"),
                sender: sender.clone(),
                recipient: Recipient::Agent(AgentId::opaque(parent.agent_id.clone()).unwrap()),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".into(),
                payload: "UNRELATED_ACTIVE_COORDINATION".into(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now,
        )
        .unwrap();
    actor
        .service
        .deliver_pending_runtime_agent_messages(now)
        .unwrap();
}

/// Independent canonical-source failures at the real actor boundary.
#[derive(Clone, Copy)]
enum RetirementFault {
    /// Ordinary committed reference with no injected source failure.
    None,
    /// Another genuine context event cannot stand in for the selected source.
    ReferenceMismatch,
    /// The original context owner is temporarily unavailable, then restored.
    ContextUnavailable,
}

/// Unrelated model mail may resume the original targeted Wait but cannot erase
/// its selected dependency. If that exact peer retires during an admitted model
/// request, canonical evidence must fence the old response at completion. The
/// next real claim consumes the outcome, performs useful persisted work and
/// completes the SAME task without replaying a send or concurrent inference.
async fn active_retirement(fault: RetirementFault) {
    let (mut actor, parent, child, mut generations, _) =
        actor_fixture_with_lifetime(1, Vec::new(), mez_agent::SubagentLifetime::Persistent).await;
    let peer = actor
        .service
        .ensure_runtime_message_identity(
            "agent-actor-active-dependency",
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
        vec![done("active-unrelated-child", "unrelated child done")],
        &mut generations,
    )
    .await;
    unrelated_mail(&mut actor, &parent);
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    assert_ne!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    let obsolete = claim(&mut actor, &parent).await;
    assert!(generations.insert(obsolete.claim_generation));
    assert!(
        !obsolete
            .context
            .durable()
            .blocks()
            .iter()
            .any(|b| b.label.starts_with("request-handoff:"))
    );
    let execution = execute(
        &obsolete,
        &response(vec![done("obsolete-final", "STALE_WITHOUT_RETIREMENT")]),
    )
    .await;
    let original_context = if matches!(fault, RetirementFault::ContextUnavailable) {
        Some(
            actor
                .service
                .agent_turn_contexts_mut()
                .remove(&parent.turn_id)
                .unwrap(),
        )
    } else {
        None
    };
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
    let mut handoff = None;
    if original_context.is_none() {
        assert_eq!(
            actor.service.agent_turn_contexts()[&parent.turn_id]
                .blocks()
                .iter()
                .filter(|b| b.label.starts_with("request-handoff:"))
                .count(),
            1,
            "selected retirement was not staged at the active owner's safe context boundary"
        );
        let source = actor.service.agent_turn_contexts()[&parent.turn_id]
            .chronology()
            .iter()
            .find(|event| event.block().label.starts_with("request-handoff:"))
            .unwrap()
            .sequence();
        assert!(
            actor.service.agent_turn_contexts()[&parent.turn_id].peer_message_is_pending(source)
        );
        if matches!(fault, RetirementFault::ReferenceMismatch) {
            let unrelated = actor.service.agent_turn_contexts()[&parent.turn_id]
                .chronology()
                .iter()
                .find(|event| {
                    event.block().source == mez_agent::ContextSourceKind::PeerMessage
                        && event
                            .block()
                            .content
                            .contains("UNRELATED_ACTIVE_COORDINATION")
                })
                .unwrap()
                .sequence();
            actor
                .service
                .replace_request_handoff_reference_for_test(&parent.turn_id, unrelated)
                .unwrap();
        }
        handoff = Some(source);
    } else {
        assert!(
            !actor
                .service
                .agent_turn_contexts()
                .contains_key(&parent.turn_id)
        );
    }
    assert_eq!(complete(&mut actor, &obsolete, execution.clone()).await, 1);
    assert_eq!(complete(&mut actor, &obsolete, execution).await, 0);
    settle_provider_work(&mut actor).await;
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running,
        "an old provider response completed the task without seeing selected retirement"
    );
    if original_context.is_none() {
        assert!(
            !actor.service.agent_turn_contexts()[&parent.turn_id]
                .blocks()
                .iter()
                .any(|block| block.content.contains("STALE_WITHOUT_RETIREMENT"))
        );
    } else {
        assert!(
            !actor
                .service
                .agent_turn_contexts()
                .contains_key(&parent.turn_id)
        );
    }
    assert_ne!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    if !matches!(fault, RetirementFault::None) {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        assert!(
            !actor
                .handle_request(AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask {
                    agent_id: AgentId::opaque(parent.agent_id.clone()).unwrap(),
                    turn_id: parent.turn_id.clone(),
                    reply,
                })
                .await
        );
        assert!(
            receiver.await.unwrap().unwrap().is_none(),
            "an unavailable or mismatched canonical source must defer provider preparation"
        );
        assert_ne!(
            actor.service.tracked_message_request_usage(),
            ((0, 0, 0), 0)
        );
        if let Some(original) = original_context {
            actor
                .service
                .agent_turn_contexts_mut()
                .insert(parent.turn_id.clone(), original);
            actor
                .service
                .deliver_pending_runtime_agent_messages(async_runtime_current_unix_millis())
                .unwrap();
            let context = &actor.service.agent_turn_contexts()[&parent.turn_id];
            assert_eq!(
                context
                    .blocks()
                    .iter()
                    .filter(|block| block.label.starts_with("request-handoff:"))
                    .count(),
                1
            );
            handoff = Some(
                context
                    .chronology()
                    .iter()
                    .find(|event| event.block().label.starts_with("request-handoff:"))
                    .unwrap()
                    .sequence(),
            );
        } else {
            actor
                .service
                .replace_request_handoff_reference_for_test(&parent.turn_id, handoff.unwrap())
                .unwrap();
        }
    }
    let handoff = handoff.unwrap();
    let resumed = claim(&mut actor, &parent).await;
    assert!(generations.insert(resumed.claim_generation));
    assert_ne!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0),
        "issuing a new request is not accepted outcome consumption"
    );
    assert!(actor.service.agent_turn_contexts()[&parent.turn_id].peer_message_is_pending(handoff));
    let mut evidence = record_evidence();
    evidence.id = "action-1".into();
    let execution = execute(&resumed, &response(vec![evidence])).await;
    assert_eq!(complete(&mut actor, &resumed, execution.clone()).await, 1);
    assert_eq!(complete(&mut actor, &resumed, execution).await, 0);
    let persisted = settle_provider_work(&mut actor).await;
    assert!(persisted > 0);
    assert!(!actor.service.agent_turn_contexts()[&parent.turn_id].peer_message_is_pending(handoff));
    assert_memory_saved(&actor, &parent);
    assert_eq!(resumed.turn.turn_id, parent.turn_id);
    assert_eq!(
        resumed
            .context
            .durable()
            .blocks()
            .iter()
            .filter(|b| b.source == mez_agent::ContextSourceKind::PeerMessage
                && b.label.starts_with("request-handoff:")
                && b.content.contains("unavailable"))
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
        vec![done(
            "active-final",
            "used selected retirement for useful alternative",
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
    assert_eq!(generations, (1..=7).collect());
}

/// A selected retirement that arrives during actual inference must fence that
/// older response, remain pinned through the replacement claim, and be consumed
/// only by its accepted response before useful same-parent completion.
#[tokio::test(flavor = "current_thread")]
async fn request_bound_actor_retirement_during_unrelated_continuation_fences_old_response() {
    active_retirement(RetirementFault::None).await;
}

/// A stale local handoff pointer to a different genuine peer event must neither
/// consume the selected dependency nor admit a model request. Actual source
/// repair restores normal admission without a new send, task or forged ACK.
#[tokio::test(flavor = "current_thread")]
async fn request_bound_actor_active_source_fault_retains_custody_until_exact_repair() {
    active_retirement(RetirementFault::ReferenceMismatch).await;
}

/// A missing canonical context during actual inference cannot allow an old
/// completion to settle the selected request or admit a replacement provider.
/// Restoring the SAME original context lets normal delivery stage its one
/// outcome, then genuine claim/typed persistence consumes it on the same task.
#[tokio::test(flavor = "current_thread")]
async fn request_bound_actor_unavailable_context_fences_completion_until_original_restored() {
    active_retirement(RetirementFault::ContextUnavailable).await;
}

/// A genuine scheduler/lifecycle completion can end the captured child without
/// ever issuing a child provider request. The actor must still claim the SAME
/// original parent with exactly one terminal handoff, successful typed durable
/// work and final completion. Five actual leases prove no synthetic child
/// inference, new task, automatic request resend or duplicate wake is required.
#[tokio::test(flavor = "current_thread")]
async fn request_bound_actor_state_only_success_finishes_same_parent_without_child_inference() {
    let (mut actor, parent, child, mut generations, _) =
        actor_fixture_with_lifetime(1, Vec::new(), mez_agent::SubagentLifetime::Persistent).await;
    let message = request(&mut actor, &parent, &child.agent_id, &mut generations).await;
    park(&mut actor, &parent, &message, &mut generations).await;
    assert!(
        !actor
            .service
            .agent_turn_executions()
            .contains_key(&child.turn_id)
    );
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&child.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    actor
        .service
        .complete_running_agent_turn_and_start_ready(
            &child,
            AgentTurnState::Completed,
            "native state-only completion",
        )
        .unwrap();
    actor
        .service
        .deliver_pending_runtime_agent_messages(async_runtime_current_unix_millis())
        .unwrap();
    assert_outcome(&actor, &parent, "peer_terminal", true);
    let (dispatch, persisted) = step(
        &mut actor,
        &parent,
        vec![record_evidence()],
        &mut generations,
    )
    .await;
    assert!(persisted > 0);
    assert_memory_saved(&actor, &parent);
    assert_eq!(dispatch.turn.turn_id, parent.turn_id);
    let references: Vec<_> = dispatch
        .context
        .durable()
        .blocks()
        .iter()
        .filter(|block| {
            block.source == mez_agent::ContextSourceKind::PeerMessage
                && block.label.starts_with("request-handoff:")
        })
        .collect();
    assert_eq!(references.len(), 1);
    assert!(
        references[0]
            .content
            .contains("completed without provider output")
    );
    assert!(references[0].content.contains(&child.turn_id));
    assert_eq!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    step(
        &mut actor,
        &parent,
        vec![done(
            "state-only-parent-final",
            "used the real terminal occurrence",
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
    assert!(
        !actor
            .service
            .agent_turn_executions()
            .contains_key(&child.turn_id)
    );
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), 2);
    assert_eq!(generations, (1..=5).collect());
}

/// A selected response accepted before sender retirement may still await the
/// delivery sweep while an unrelated-mail continuation is in flight. Actual
/// pending custody must fence the old response even with no new context event;
/// it cannot count as progress, ACK or satisfaction until canonical commit.
/// After real delivery, the same parent consumes the reply and completes via
/// typed persistence without replaying its request or creating another task.
#[tokio::test(flavor = "current_thread")]
async fn request_bound_actor_pending_original_reply_fences_completion_before_canonical_commit() {
    use mez_agent::messaging::{Envelope, MessageScope, Recipient};
    let (mut actor, parent, child, mut generations, _) =
        actor_fixture_with_lifetime(1, Vec::new(), mez_agent::SubagentLifetime::Persistent).await;
    let peer = actor
        .service
        .ensure_runtime_message_identity(
            "agent-actor-retiring-answerer",
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
        vec![done("pending-unrelated-child", "unrelated child done")],
        &mut generations,
    )
    .await;
    unrelated_mail(&mut actor, &parent);
    let obsolete = claim(&mut actor, &parent).await;
    assert!(generations.insert(obsolete.claim_generation));
    let before = obsolete.context.durable().event_sequence_high_water_mark();
    let execution = execute(
        &obsolete,
        &response(vec![done(
            "pending-obsolete-final",
            "OBSOLETE_BEFORE_AUTHENTIC_REPLY",
        )]),
    )
    .await;
    let now = async_runtime_current_unix_millis();
    actor
        .service
        .message_service_mut()
        .accept_at_with_scope(
            &peer.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "actual-original-interval-answer".into(),
                message_type: "send".into(),
                time: format!("fixture:{now}"),
                sender: peer.clone(),
                recipient: Recipient::Agent(AgentId::opaque(parent.agent_id.clone()).unwrap()),
                correlation_id: Some(message),
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".into(),
                payload: "ACTUAL_AUTHENTIC_ANSWER".into(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now,
        )
        .unwrap();
    assert!(
        actor
            .service
            .retire_runtime_message_identity_after_native_commits(&peer.agent_id)
            .unwrap()
    );
    assert_eq!(
        actor.service.agent_turn_contexts()[&parent.turn_id].event_sequence_high_water_mark(),
        before
    );
    assert_eq!(complete(&mut actor, &obsolete, execution.clone()).await, 1);
    assert_eq!(complete(&mut actor, &obsolete, execution).await, 0);
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    assert!(
        !actor.service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|block| block.content.contains("OBSOLETE_BEFORE_AUTHENTIC_REPLY"))
    );
    assert!(
        !actor.service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|block| block.label.starts_with("request-handoff:"))
    );
    assert_ne!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    let (reply, receiver) = tokio::sync::oneshot::channel();
    assert!(
        !actor
            .handle_request(AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask {
                agent_id: AgentId::opaque(parent.agent_id.clone()).unwrap(),
                turn_id: parent.turn_id.clone(),
                reply
            })
            .await
    );
    assert!(
        receiver.await.unwrap().unwrap().is_none(),
        "pending acceptance must not fabricate canonical input or an inference request"
    );
    actor
        .service
        .deliver_pending_runtime_agent_messages(async_runtime_current_unix_millis())
        .unwrap();
    let (dispatch, persisted) = step(
        &mut actor,
        &parent,
        vec![record_evidence()],
        &mut generations,
    )
    .await;
    assert!(persisted > 0);
    assert_memory_saved(&actor, &parent);
    assert_eq!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .filter(|block| block.label.starts_with("request-handoff:")
                && block.content.contains("state=reply"))
            .count(),
        1
    );
    assert!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .any(|block| block.content.contains("ACTUAL_AUTHENTIC_ANSWER"))
    );
    assert_eq!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    step(
        &mut actor,
        &parent,
        vec![done(
            "pending-parent-final",
            "used authentic reply after retirement",
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
    assert_eq!(generations, (1..=7).collect());
}

/// An actual one-slot provider claim may accept its tracked send before an idle
/// receiver fails canonical admission. That fault must remain receiver work:
/// the SAME parent receives a successful accepted-ID result, parks on it, then
/// consumes the original registration's real reply and performs typed durable
/// work. No sender failure/retry, duplicate receive task or synthetic lease can
/// replace the original accepted effect, even across duplicate event ingress.
#[tokio::test(flavor = "current_thread")]
async fn request_bound_actor_accepted_send_survives_receiver_commit_fault() {
    let (mut actor, parent, initial, mut generations, _) =
        actor_fixture_with_lifetime(1, Vec::new(), mez_agent::SubagentLifetime::Persistent).await;
    step(
        &mut actor,
        &parent,
        vec![AgentAction {
            id: "prepare-idle-recipient".into(),
            payload: AgentActionPayload::Wait {
                for_message: None,
                from_agent: None,
            },
        }],
        &mut generations,
    )
    .await;
    step(
        &mut actor,
        &initial,
        vec![done(
            "initial-recipient-done",
            "idle before actual tracked acceptance",
        )],
        &mut generations,
    )
    .await;
    unrelated_mail(&mut actor, &parent);
    actor
        .service
        .fail_next_peer_message_receive_after_context_storage_for_tests();
    let message = request(&mut actor, &parent, &initial.agent_id, &mut generations).await;
    assert_eq!(
        actor
            .service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    assert_eq!(
        actor.service.agent_turn_executions()[&parent.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Succeeded
    );
    let diagnostic = actor
        .service
        .agent_pane_trace_log_text(&parent.pane_id)
        .unwrap();
    assert!(diagnostic.contains("deferred after acceptance"));
    assert!(diagnostic.contains("after context storage"));
    actor
        .service
        .apply_peer_message_delivery_timer(async_runtime_current_unix_millis(), 77)
        .unwrap();
    let recipient = actor
        .service
        .agent_turn_ledger()
        .latest_turn_for_pane(&initial.pane_id)
        .unwrap()
        .clone();
    assert_ne!(recipient.turn_id, initial.turn_id);
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), 3);
    assert_eq!(
        actor.service.agent_turn_contexts()[&recipient.turn_id]
            .blocks()
            .iter()
            .filter(
                |block| block.source == mez_agent::ContextSourceKind::PeerMessage
                    && block.label.contains(&message)
            )
            .count(),
        1
    );
    park(&mut actor, &parent, &message, &mut generations).await;
    step(
        &mut actor,
        &recipient,
        vec![
            AgentAction {
                id: "actual-reply-after-receiver-recovery".into(),
                payload: AgentActionPayload::SendMessage {
                    expect_reply: false,
                    recipient: format!("agent:{}", parent.agent_id),
                    scope: Some("session".into()),
                    content_type: "text/plain; charset=utf-8".into(),
                    payload: "ACTUAL_REPLY_AFTER_PARTIAL_SEND".into(),
                    correlation_id: Some(message.clone()),
                },
            },
            done(
                "recovered-recipient-done",
                "replied once after receiver recovery",
            ),
        ],
        &mut generations,
    )
    .await;
    assert_outcome(&actor, &parent, "reply", true);
    let (dispatch, persisted) = step(
        &mut actor,
        &parent,
        vec![record_evidence()],
        &mut generations,
    )
    .await;
    assert!(persisted > 0);
    assert_memory_saved(&actor, &parent);
    assert!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .any(|block| block.content.contains("ACTUAL_REPLY_AFTER_PARTIAL_SEND"))
    );
    assert_eq!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .filter(|block| block.label.starts_with("request-handoff:"))
            .count(),
        1
    );
    assert_eq!(
        actor.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    assert_eq!(
        actor
            .service
            .message_service()
            .snapshot_state()
            .accepted_messages
            .iter()
            .filter(|row| row.envelope.id == message)
            .count(),
        1
    );
    step(
        &mut actor,
        &parent,
        vec![done(
            "partial-send-parent-final",
            "used real reply after receiver recovery",
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
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), 3);
    assert_eq!(generations, (1..=8).collect());
}
