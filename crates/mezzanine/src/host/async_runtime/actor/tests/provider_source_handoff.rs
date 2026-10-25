//! Source-owned provider dispatch recovery at real capacity-drain boundaries.
//!
//! These fixtures submit a genuine prompt but never execute a provider or run
//! timer/persistence workers. Native cases own a real pane root for admission
//! evidence, but never execute a native workload. A descriptor refused by the
//! finite lane must remain recoverable from its service task, and a descriptor already
//! offered to a worker must not be regenerated before preparation/claim settles.

use super::super::*;

/// Creates a real pending provider task with a saturated finite transient lane.
pub(super) fn pending_provider_at_capacity() -> (AsyncRuntimeSessionActor, String, ClientId) {
    pending_provider_in_mode_at_capacity(false)
}

/// Selects transport before admitting the prompt; no later configuration reload
/// can retire the turn whose accepted output supplies the dispatch source.
fn pending_provider_in_mode_at_capacity(
    native: bool,
) -> (AsyncRuntimeSessionActor, String, ClientId) {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service.replace_config_layers(vec![ConfigLayer {
        name: "recovered-provider-claim".into(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"default\"\nshell_mode = \"{}\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.default]\nprovider = \"runtime-batch\"\nmodel = \"test\"\n[permissions]\napproval_policy = \"ask\"\nsandbox = \"policy-only\"\n", if native { "native" } else { "pane" }),
    }]).unwrap();
    let primary = service
        .attach_primary("handoff-primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    if native {
        service.start_initial_pane_process(None).unwrap();
    }
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "source handoff")
        .unwrap();
    let task = service.pending_agent_provider_tasks().remove(0);
    let (_, mut actor) = AsyncRuntimeSessionActor::new(
        service,
        AsyncRuntimeActorConfig {
            side_effect_buffer: 1,
            ..Default::default()
        },
    )
    .unwrap();
    actor.drain_runtime_side_effects(64).unwrap();
    actor.drain_timer_side_effects(64).unwrap();
    actor.drain_persistence_side_effects(64).unwrap();
    let capacity = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..capacity)
                .map(|index| RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::PanePipeHealth,
                        format!("occupied-{index}"),
                        1,
                    ),
                    delay_ms: 25,
                })
                .collect(),
        )
        .unwrap();
    (actor, task.turn_id, primary)
}

/// Rejection cannot strand a real pending provider task once another lane frees
/// capacity. Recovery uses its existing owner without another runtime event.
/// Draining that recovered descriptor is only an offer, not a claim: repeated
/// drains and ordinary pending-task projection must not duplicate it while a
/// worker still owns preparation. No request or action is actually executed.
#[tokio::test(flavor = "current_thread")]
async fn refused_provider_dispatch_recovers_once_on_capacity_drain() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert_eq!(effects.len(), 1);
    assert!(actor.admit_committed_side_effects(effects).is_err());
    assert!(actor.service.agent_provider_task_is_pending(&turn_id));
    assert_eq!(actor.drain_timer_side_effects(1).unwrap().len(), 1);
    let offered = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentProvider { turn_id: owner, .. }] if owner == &turn_id),
        "{offered:?}"
    );
    assert!(actor.service.agent_provider_task_is_pending(&turn_id));
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .is_empty()
    );
    assert!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .is_empty()
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// Cancellation retires the original source before recovery. Releasing lane
/// capacity afterward must not recreate a dispatch for the stopped turn, even
/// though the earlier capacity refusal consumed the event-side descriptor.
#[tokio::test(flavor = "current_thread")]
async fn cancelled_provider_source_is_not_recovered_after_refusal() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(!actor.service.agent_provider_task_is_owned(&turn_id));
    actor.drain_timer_side_effects(1).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .is_empty()
    );
}

/// Normal pending tasks have not suffered committed refusal. A timer drain
/// cannot accelerate their poll scheduling, and strict producer refusal cannot
/// silently convert them into committed recovery work.
#[tokio::test(flavor = "current_thread")]
async fn ordinary_pending_provider_is_not_accelerated_by_lane_drain() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.queue_runtime_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .len(),
        1
    );
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(actor.offered_dispatch_sources.is_empty());
    assert!(actor.service.agent_provider_task_is_pending(&turn_id));
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// The exact recovered offer may return no work during preparation. Its source
/// remains recoverable without duplicate offers while held, then becomes ready
/// again after settlement. Cancellation prunes both small identity sets on real
/// progress, rather than accumulating obsolete turn IDs over the actor lifetime.
#[tokio::test(flavor = "current_thread")]
async fn recovered_offer_returns_to_pending_source_and_retires_on_stop() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert_eq!(actor.drain_runtime_side_effects(1).unwrap().len(), 1);
    assert!(
        actor
            .offered_dispatch_sources
            .contains(&SourceDispatch::Provider(turn_id.clone()))
    );
    actor.settle_provider_offer(&turn_id);
    assert!(
        !actor
            .offered_dispatch_sources
            .contains(&SourceDispatch::Provider(turn_id.clone()))
    );
    assert!(
        actor
            .refused_dispatch_sources
            .contains(&SourceDispatch::Provider(turn_id.clone()))
    );
    actor.drain_timer_side_effects(1).unwrap();
    assert_eq!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .len(),
        1
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    actor.drain_timer_side_effects(1).unwrap();
    assert!(actor.offered_dispatch_sources.is_empty());
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .is_empty()
    );
}

/// Continuously ready service input cannot consume every newly freed slot ahead
/// of refused provider work. The next two single-slot drains give each source
/// family first access once, while both stay within the original finite reserve.
#[tokio::test(flavor = "current_thread")]
async fn provider_recovery_alternates_capacity_with_deferred_service_input() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let capacity = 1 + actor.committed_side_effect_overshoot();
    for _ in 0..capacity + 1 {
        actor.service.queue_deferred_pane_input_for_tests(
            RuntimeSideEffect::WritePaneInputPriority {
                pane_id: "%1".into(),
                bytes: b"owned-input".to_vec(),
            },
        );
    }
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    for _ in 0..2 {
        assert_eq!(actor.drain_timer_side_effects(1).unwrap().len(), 1);
        assert_eq!(actor.available_committed_side_effect_capacity(), 0);
    }
    let offered = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentProvider { turn_id: owner, .. }] if owner == &turn_id)
    );
    let service = actor.drain_runtime_side_effects(1).unwrap();
    assert!(
        matches!(service.as_slice(), [RuntimeSideEffect::WritePaneInputPriority { bytes, .. }] if bytes == b"owned-input")
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// A recovered descriptor can be cancelled while held by a worker. Its actual
/// preparation request must return no work and release the offer at the request
/// boundary, without depending on a later lane drain to repair stale metadata.
/// The cancelled turn cannot regain provider authority or create another offer.
#[tokio::test(flavor = "current_thread")]
async fn cancelled_recovered_offer_retires_at_real_preparation_boundary() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    let offered = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
    let RuntimeSideEffect::DispatchAgentProvider { agent_id, .. } =
        offered.into_iter().next().unwrap()
    else {
        panic!("expected the recovered provider descriptor");
    };
    actor.drain_timer_side_effects(64).unwrap();
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    let (reply, response) = tokio::sync::oneshot::channel();
    assert!(
        !actor
            .handle_request(AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask {
                agent_id,
                turn_id: turn_id.clone(),
                reply,
            })
            .await
    );
    assert!(response.await.unwrap().unwrap().is_none());
    assert!(
        !actor
            .offered_dispatch_sources
            .contains(&SourceDispatch::Provider(turn_id.clone()))
    );
    assert!(
        !actor
            .refused_dispatch_sources
            .contains(&SourceDispatch::Provider(turn_id.clone()))
    );
    assert!(!actor.service.agent_provider_task_is_owned(&turn_id));
}

/// Creates accepted external work through actual provider-result adoption,
/// without executing inference, HTTP, MCP or a native worker.
async fn pending_external_at_capacity() -> (
    AsyncRuntimeSessionActor,
    String,
    ClientId,
    mez_agent::AgentAction,
) {
    let action = mez_agent::AgentAction {
        id: "refused-fetch".into(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/never-executed".into(),
            format: None,
            max_bytes: None,
        },
    };
    pending_action_at_capacity(action, false).await
}

/// Adopts a genuine accepted action into its service dispatch owner without
/// inference or worker execution. Transport is selected before the prompt, so
/// native admission does not reload or retire the admitted provider turn.
async fn pending_action_at_capacity(
    action: mez_agent::AgentAction,
    native: bool,
) -> (
    AsyncRuntimeSessionActor,
    String,
    ClientId,
    mez_agent::AgentAction,
) {
    let (mut actor, turn_id, primary) = pending_provider_in_mode_at_capacity(native);
    if native {
        actor
            .service
            .permission_policy_mut()
            .set_approval_bypass(true);
    }
    let turn = actor
        .service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .clone();
    actor.service.remove_pending_agent_provider_task(&turn_id);
    let request = mez_agent::ModelRequest {
        provider: "runtime-batch".into(),
        model: "test".into(),
        model_capabilities: Default::default(),
        max_input_tokens: None,
        reasoning_effort: None,
        thinking_enabled: None,
        latency_preference: None,
        prompt_cache_retention: None,
        max_output_tokens: None,
        temperature: None,
        stop: None,
        prompt_cache_session_id: None,
        prompt_cache_lineage_id: None,
        turn_id: turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        available_mcp_tools: Vec::new(),
        memory_actions_enabled: false,
        issue_actions_enabled: true,
        interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
        allowed_actions: mez_agent::AllowedActionSet::capability_decision(),
        messages: vec![mez_agent::ModelMessage {
            role: mez_agent::ModelMessageRole::User,
            source: mez_agent::ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            content: "source handoff".into(),
        }]
        .into(),
    };
    let execution = mez_agent::AgentTurnExecution {
        request,
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "inert accepted external source".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "inert source custody".into(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult::running(
            &turn,
            &action,
            vec![],
            None,
        )],
        final_turn: false,
        terminal_state: mez_agent::AgentTurnState::Running,
    };
    actor
        .service
        .apply_agent_provider_completed_event(
            &AgentId::opaque(turn.agent_id).unwrap(),
            &turn_id,
            execution,
        )
        .await
        .unwrap();
    assert_eq!(
        if native {
            actor.service.pending_native_shell_actions()
        } else {
            actor.service.pending_approved_external_actions()
        },
        vec![(turn_id.clone(), action.id.clone())],
        "fixture source admission: {:?}",
        actor
            .service
            .agent_turn_executions()
            .get(&turn_id)
            .map(|execution| &execution.action_results)
    );
    (actor, turn_id, primary, action)
}

/// Creates a real authorized native source using an owned pane root. No native
/// workload starts and no action command is written to the pane PTY.
async fn pending_native_at_capacity() -> (
    AsyncRuntimeSessionActor,
    String,
    ClientId,
    mez_agent::AgentAction,
) {
    pending_action_at_capacity(
        mez_agent::AgentAction {
            id: "refused-native".into(),
            payload: mez_agent::AgentActionPayload::ShellCommand {
                summary: "Inert native dispatch custody fixture".into(),
                command: "printf never-executed".into(),
                interactive: false,
                stateful: false,
                timeout_ms: Some(5_000),
            },
        },
        true,
    )
    .await
}

/// A committed refusal must recover native work on real lane progress, not on
/// another runtime event. Generic and typed drains must both fence the offered
/// descriptor until actual claim; the immutable phase marker remains unchanged.
/// Claiming transfers authority only: this test never invokes the executor.
#[tokio::test(flavor = "current_thread")]
async fn refused_native_dispatch_recovers_and_claims_once() {
    for compatibility in [false, true] {
        let (mut actor, turn_id, primary, action) = pending_native_at_capacity().await;
        let marker = actor
            .service
            .pending_native_shell_action_marker(&turn_id, &action.id)
            .unwrap()
            .to_string();
        let effects = actor.pending_provider_dispatch_side_effects().unwrap();
        assert!(
            matches!(effects.as_slice(), [RuntimeSideEffect::DispatchNativeShellAction { turn_id: owner, action_id }] if owner == &turn_id && action_id == &action.id)
        );
        assert!(actor.admit_committed_side_effects(effects).is_err());
        actor.drain_timer_side_effects(1).unwrap();
        let offered = if compatibility {
            actor.drain_runtime_side_effects(1).unwrap()
        } else {
            actor.drain_agent_provider_dispatch_side_effects(1).unwrap()
        };
        if offered.is_empty() {
            actor
                .service
                .execute_agent_shell_command(&primary, "/stop")
                .unwrap();
            actor.service.terminate_all_pane_processes().unwrap();
        }
        assert!(
            matches!(offered.as_slice(), [RuntimeSideEffect::DispatchNativeShellAction { turn_id: owner, action_id }] if owner == &turn_id && action_id == &action.id),
            "{offered:?}"
        );
        assert!(
            !actor
                .drain_runtime_side_effects(1)
                .unwrap()
                .iter()
                .any(|effect| matches!(
                    effect,
                    RuntimeSideEffect::DispatchNativeShellAction { .. }
                ))
        );
        assert!(
            actor
                .pending_provider_dispatch_side_effects()
                .unwrap()
                .is_empty()
        );
        actor.drain_timer_side_effects(64).unwrap();
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_request(AsyncRuntimeRequest::ClaimNativeShellAction {
                turn_id: turn_id.clone(),
                action_id: action.id.clone(),
                reply,
            })
            .await;
        let dispatch = response
            .await
            .unwrap()
            .unwrap()
            .expect("recovered native claim");
        assert_eq!(dispatch.turn_id, turn_id);
        assert_eq!(dispatch.action_id, action.id);
        assert_eq!(dispatch.marker, marker);
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        assert!(actor.service.pending_native_shell_actions().is_empty());
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_request(AsyncRuntimeRequest::ClaimNativeShellAction {
                turn_id: turn_id.clone(),
                action_id: action.id.clone(),
                reply,
            })
            .await;
        assert!(response.await.unwrap().unwrap().is_none());
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        assert!(
            dispatch
                .cancellation
                .flag()
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        actor.service.terminate_all_pane_processes().unwrap();
    }
}

/// Stop before transfer and stop after an offer both retire native source
/// custody. A late actual claim returns no work; freeing more capacity cannot
/// recreate the phase. Only an owned pane root exists, and it is retired here.
#[tokio::test(flavor = "current_thread")]
async fn cancelled_native_source_is_not_recovered_or_claimed() {
    for after_offer in [false, true] {
        let (mut actor, turn_id, primary, action) = pending_native_at_capacity().await;
        let effects = actor.pending_provider_dispatch_side_effects().unwrap();
        assert!(actor.admit_committed_side_effects(effects).is_err());
        if after_offer {
            actor.drain_timer_side_effects(1).unwrap();
            assert!(matches!(
                actor
                    .drain_agent_provider_dispatch_side_effects(1)
                    .unwrap()
                    .as_slice(),
                [RuntimeSideEffect::DispatchNativeShellAction { .. }]
            ));
        }
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        assert!(
            actor
                .service
                .pending_native_shell_action_marker(&turn_id, &action.id)
                .is_none()
        );
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_request(AsyncRuntimeRequest::ClaimNativeShellAction {
                turn_id,
                action_id: action.id,
                reply,
            })
            .await;
        assert!(response.await.unwrap().unwrap().is_none());
        actor.drain_timer_side_effects(64).unwrap();
        assert!(
            !actor
                .drain_agent_provider_dispatch_side_effects(64)
                .unwrap()
                .iter()
                .any(|effect| matches!(
                    effect,
                    RuntimeSideEffect::DispatchNativeShellAction { .. }
                ))
        );
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        actor.service.terminate_all_pane_processes().unwrap();
    }
}

/// A human dispatch gate makes an actual native claim return no work without
/// surrendering the unclaimed phase. Recovery must retain the exact marker,
/// release the old offer, and fence a later reoffer. Stop retires it without
/// executing the workload or interpreting pause as cancellation/settlement.
#[tokio::test(flavor = "current_thread")]
async fn paused_native_claim_retains_exact_recovery_source() {
    let (mut actor, turn_id, primary, action) = pending_native_at_capacity().await;
    let marker = actor
        .service
        .pending_native_shell_action_marker(&turn_id, &action.id)
        .unwrap()
        .to_string();
    let source = SourceDispatch::Native(turn_id.clone(), action.id.clone(), marker.clone());
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert!(matches!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .as_slice(),
        [RuntimeSideEffect::DispatchNativeShellAction { .. }]
    ));
    let target = actor
        .service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    actor
        .service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimNativeShellAction {
            turn_id: turn_id.clone(),
            action_id: action.id.clone(),
            reply,
        })
        .await;
    assert!(response.await.unwrap().unwrap().is_none());
    assert_eq!(
        actor
            .service
            .pending_native_shell_action_marker(&turn_id, &action.id),
        Some(marker.as_str())
    );
    assert!(actor.refused_dispatch_sources.contains(&source));
    assert!(!actor.offered_dispatch_sources.contains(&source));
    actor.drain_timer_side_effects(64).unwrap();
    assert!(matches!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .as_slice(),
        [RuntimeSideEffect::DispatchNativeShellAction { .. }]
    ));
    assert!(actor.offered_dispatch_sources.contains(&source));
    assert!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .is_empty()
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    actor.service.terminate_all_pane_processes().unwrap();
}

/// Ordinary strict-producer refusal must not enroll committed recovery. A
/// deliberately stale recovery key likewise cannot accelerate the genuinely
/// pending phase: turn/action equality without the exact marker is insufficient.
/// This is a negative stale-key injection, not simulated successful execution or
/// proof of an actual patch read/write phase transition.
#[tokio::test(flavor = "current_thread")]
async fn ordinary_or_stale_native_source_is_not_accelerated() {
    for stale in [false, true] {
        let (mut actor, turn_id, primary, action) = pending_native_at_capacity().await;
        let marker = actor
            .service
            .pending_native_shell_action_marker(&turn_id, &action.id)
            .unwrap();
        if stale {
            actor
                .refused_dispatch_sources
                .insert(SourceDispatch::Native(
                    turn_id.clone(),
                    action.id.clone(),
                    format!("{marker}-retired"),
                ));
        } else {
            let effects = actor.pending_provider_dispatch_side_effects().unwrap();
            assert!(actor.queue_runtime_side_effects(effects).is_err());
        }
        actor.drain_timer_side_effects(1).unwrap();
        assert!(
            !actor
                .drain_agent_provider_dispatch_side_effects(1)
                .unwrap()
                .iter()
                .any(|effect| matches!(
                    effect,
                    RuntimeSideEffect::DispatchNativeShellAction { .. }
                ))
        );
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        assert!(matches!(
            actor
                .pending_provider_dispatch_side_effects()
                .unwrap()
                .as_slice(),
            [RuntimeSideEffect::DispatchNativeShellAction { .. }]
        ));
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        actor.service.terminate_all_pane_processes().unwrap();
    }
}

/// A real committed descriptor refusal must recover after capacity drains,
/// stay fenced across both drain APIs, and transfer once through actual claim.
#[tokio::test(flavor = "current_thread")]
async fn refused_external_dispatch_recovers_and_claims_once() {
    recovered_external_claim(false).await;
    recovered_external_claim(true).await;
}

/// Both compatibility and typed adapters must fence the offer before refill.
async fn recovered_external_claim(compatibility: bool) {
    let (mut actor, turn_id, primary, action) = pending_external_at_capacity().await;
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    let offered = if compatibility {
        actor.drain_runtime_side_effects(1).unwrap()
    } else {
        actor.drain_agent_provider_dispatch_side_effects(1).unwrap()
    };
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchApprovedExternalAction { turn_id: owner, action_id }] if owner == &turn_id && action_id == &action.id),
        "{offered:?}"
    );
    assert!(
        !actor
            .drain_runtime_side_effects(1)
            .unwrap()
            .iter()
            .any(|effect| matches!(
                effect,
                RuntimeSideEffect::DispatchApprovedExternalAction { .. }
            ))
    );
    assert!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .is_empty()
    );
    actor.drain_timer_side_effects(64).unwrap();
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimApprovedExternalAction {
            turn_id: turn_id.clone(),
            action_id: action.id.clone(),
            reply,
        })
        .await;
    let dispatch = response
        .await
        .unwrap()
        .unwrap()
        .expect("actual recovered external claim");
    assert_eq!(dispatch.turn.turn_id, turn_id);
    assert_eq!(dispatch.action.id, action.id);
    assert!(actor.service.pending_approved_external_actions().is_empty());
    assert!(
        !actor
            .refused_dispatch_sources
            .contains(&SourceDispatch::External(
                turn_id.clone(),
                action.id.clone()
            ))
    );
    assert!(
        !actor
            .offered_dispatch_sources
            .contains(&SourceDispatch::External(
                turn_id.clone(),
                action.id.clone()
            ))
    );
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimApprovedExternalAction {
            turn_id,
            action_id: action.id,
            reply,
        })
        .await;
    assert!(response.await.unwrap().unwrap().is_none());
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// Genuine recovered provider work must pass real off-actor preparation and
/// claim with one mandatory timer, then retire both recovery identities. The
/// configured fake provider has no credentials; preparation cannot do refresh
/// HTTP, MCP discovery or accounting work, and inference is never executed.
#[tokio::test(flavor = "current_thread")]
async fn recovered_provider_offer_prepares_and_claims_once() {
    let (mut actor, turn_id, primary) = pending_provider_at_capacity();
    let root = std::env::temp_dir().join(format!(
        "mez-recovered-provider-auth-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    assert!(!root.exists());
    actor
        .service
        .set_auth_store(crate::security::auth::AuthStore::new(
            crate::security::auth::AuthPaths::under_config_root(&root),
        ));
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    let offered = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
    let RuntimeSideEffect::DispatchAgentProvider { agent_id, .. } =
        offered.into_iter().next().unwrap()
    else {
        panic!("recovered provider offer");
    };
    actor.drain_timer_side_effects(64).unwrap();
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
            reply,
        })
        .await;
    let work = response
        .await
        .unwrap()
        .unwrap()
        .expect("actual recovered preparation");
    assert!(work.mcp_plans.is_empty());
    assert!(work.accounting_projects.is_none());
    assert_eq!(
        work.provider_auth_refresh_profiles,
        vec!["runtime-batch".to_string()]
    );
    assert!(
        work.auth_store
            .as_ref()
            .unwrap()
            .read_metadata_for_provider("runtime-batch")
            .unwrap()
            .is_none()
    );
    let preparation = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    assert!(preparation.provider_refresh_error.is_none());
    assert!(!root.exists());
    assert!(
        actor
            .offered_dispatch_sources
            .contains(&SourceDispatch::Provider(turn_id.clone()))
    );
    assert!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .is_empty()
    );
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimConfiguredAgentProviderTask {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
            preparation,
            reply,
        })
        .await;
    let dispatch = response
        .await
        .unwrap()
        .unwrap()
        .expect("actual recovered claim");
    assert!(actor.service.agent_provider_claim_matches(
        &agent_id,
        &turn_id,
        dispatch.claim_generation
    ));
    assert!(
        !actor
            .offered_dispatch_sources
            .contains(&SourceDispatch::Provider(turn_id.clone()))
    );
    assert!(
        !actor
            .refused_dispatch_sources
            .contains(&SourceDispatch::Provider(turn_id.clone()))
    );
    let timers = actor.drain_timer_side_effects(64).unwrap();
    assert_eq!(timers.iter().filter(|effect| matches!(effect,
        RuntimeSideEffect::ScheduleTimer { key, .. } if key.kind == RuntimeTimerKind::ProviderClaim && key.owner_id == turn_id && key.generation == dispatch.claim_generation
    )).count(), 1);
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask {
            agent_id,
            turn_id: turn_id.clone(),
            reply,
        })
        .await;
    assert!(response.await.unwrap().unwrap().is_none());
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert!(!root.exists());
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// Cancellation before recovery and after an offer both retire the original
/// external source. An actual no-work claim cannot retain an obsolete offer or
/// recreate dispatch after later capacity drains. No remote attempt executes.
#[tokio::test(flavor = "current_thread")]
async fn cancelled_external_recovery_retires_before_and_after_offer() {
    for after_offer in [false, true] {
        let (mut actor, turn_id, primary, action) = pending_external_at_capacity().await;
        let effects = actor.pending_provider_dispatch_side_effects().unwrap();
        assert!(actor.admit_committed_side_effects(effects).is_err());
        if after_offer {
            actor.drain_timer_side_effects(1).unwrap();
            assert_eq!(
                actor
                    .drain_agent_provider_dispatch_side_effects(1)
                    .unwrap()
                    .len(),
                1
            );
        }
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        actor.drain_timer_side_effects(64).unwrap();
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_request(AsyncRuntimeRequest::ClaimApprovedExternalAction {
                turn_id: turn_id.clone(),
                action_id: action.id.clone(),
                reply,
            })
            .await;
        assert!(response.await.unwrap().unwrap().is_none());
        let source = SourceDispatch::External(turn_id, action.id);
        assert!(!actor.offered_dispatch_sources.contains(&source));
        assert!(!actor.refused_dispatch_sources.contains(&source));
        assert!(
            actor
                .drain_agent_provider_dispatch_side_effects(64)
                .unwrap()
                .is_empty()
        );
    }
}

/// A mixed event batch contains actual claimed native completion plus optional
/// output, rather than stale classification-only progress. Full transient
/// pressure may refuse the resulting provider continuation but cannot undo the
/// accepted action result or take unrelated immutable service input. Real
/// lane drains recover ordinary input in FIFO order and offer the continuation once;
/// duplicate completion cannot run the action or publish its result again.
/// Priority input is intentionally not the FIFO oracle: it overtakes existing
/// same-pane input at each enqueue boundary under its established policy.
///
/// The fixture owns a pane root for ordinary native admission, but no native
/// workload, provider request, timer or persistence worker is executed. The
/// supplied output is a deterministic worker-result fixture, not evidence that
/// a command actually ran. Explicit pane retirement is attempted; this test
/// does not infer affirmative reader/reap settlement from that request alone.
#[tokio::test(flavor = "current_thread")]
async fn mixed_claimed_native_completion_preserves_service_tail_and_continuation() {
    let (mut actor, turn_id, primary, action) = pending_native_at_capacity().await;
    actor.drain_timer_side_effects(64).unwrap();
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimNativeShellAction {
            turn_id: turn_id.clone(),
            action_id: action.id.clone(),
            reply,
        })
        .await;
    let dispatch = response
        .await
        .unwrap()
        .unwrap()
        .expect("actual native claim");
    actor.drain_runtime_side_effects(64).unwrap();
    actor.drain_timer_side_effects(64).unwrap();
    let capacity = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..capacity)
                .map(|index| RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::PanePipeHealth,
                        format!("mixed-pressure-{index}"),
                        1,
                    ),
                    delay_ms: 25,
                })
                .collect(),
        )
        .unwrap();
    let expected = (0..capacity + 3)
        .map(|index| format!("original-native-tail-{index}").into_bytes())
        .collect::<Vec<_>>();
    for bytes in &expected {
        actor
            .service
            .queue_deferred_pane_input_for_tests(RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".into(),
                bytes: bytes.clone(),
            });
    }
    let outcome = crate::runtime::RuntimeNativeShellOutcome {
        turn_id: dispatch.turn_id.clone(),
        action_id: dispatch.action_id.clone(),
        marker: dispatch.marker.clone(),
        command: dispatch.request.transaction.command.clone(),
        started_at_unix_ms: dispatch.started_at_unix_ms,
        sandbox_capability: None,
        capability_probe_only: dispatch.capability_probe_only,
        launch_counts: None,
        result: Ok(mez_agent::ShellExecutionOutput::new(
            Some(0),
            "MIXED_NATIVE_ACCEPTED_BODY\n".into(),
            String::new(),
            false,
            false,
        )),
    };
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::NativeShellProgress(
        crate::runtime::RuntimeNativeShellProgress {
            presentation: mez_agent::ActionPresentationProgress::new(
                &turn_id,
                &action.id,
                mez_agent::ActionPresentationExecutionIdentity::Attempt(dispatch.marker),
                1,
                mez_agent::ActionPresentationComponentIdentity::ShellOutput,
                "MIXED_NATIVE_ACCEPTED_BODY\n",
            ),
        },
    ));
    batch.push(RuntimeEvent::NativeShell(outcome.clone()));
    let error = actor.apply_runtime_event_batch(batch).await.unwrap_err();
    assert!(error.message().contains("consumed=1"), "{error:?}");
    let execution = &actor.service.agent_turn_executions()[&turn_id];
    assert_eq!(execution.action_results.len(), 1);
    assert_eq!(
        execution.action_results[0].status,
        mez_agent::ActionStatus::Succeeded
    );
    assert!(actor.service.pending_native_shell_actions().is_empty());
    assert!(actor.service.agent_provider_task_is_pending(&turn_id));
    let mut duplicate = RuntimeEventBatch::new();
    duplicate.push(RuntimeEvent::NativeShell(outcome));
    assert_eq!(
        actor
            .apply_runtime_event_batch(duplicate)
            .await
            .unwrap()
            .applied,
        0
    );
    let mut observed = Vec::new();
    let mut continuations = 0;
    let mut redraws = 0;
    for _ in 0..capacity * 4 {
        actor.drain_timer_side_effects(1).unwrap();
        for effect in actor.drain_runtime_side_effects(1).unwrap() {
            match effect {
                RuntimeSideEffect::WritePaneInput { bytes, .. } => observed.push(bytes),
                RuntimeSideEffect::DispatchAgentProvider { turn_id: owner, .. } => {
                    assert_eq!(owner, turn_id);
                    continuations += 1;
                }
                RuntimeSideEffect::RenderClient { client_id, reason } => {
                    assert_eq!(client_id, primary);
                    assert_eq!(reason, RenderInvalidationReason::FullRedraw);
                    redraws += 1;
                }
                other => panic!("unexpected mixed-completion recovery: {other:?}"),
            }
        }
        assert!(
            actor.side_effects.len() + actor.side_effect_routes.non_persistence_len() <= capacity
        );
    }
    assert_eq!(observed, expected);
    assert_eq!(continuations, 1);
    assert_eq!(redraws, 1);
    assert!(
        actor
            .service
            .drain_pane_io_transition()
            .side_effects
            .is_empty()
    );
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        actor.service.agent_turn_executions()[&turn_id]
            .action_results
            .len(),
        1
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    actor.service.terminate_all_pane_processes().unwrap();
}

/// Strict producer refusal cannot enroll external recovery or accelerate its
/// ordinary pending scheduling when an unrelated lane frees capacity.
#[tokio::test(flavor = "current_thread")]
async fn strict_external_refusal_keeps_producer_scheduling() {
    let (mut actor, _turn_id, primary, _action) = pending_external_at_capacity().await;
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.queue_runtime_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .len(),
        1
    );
    assert!(actor.refused_dispatch_sources.is_empty());
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}
