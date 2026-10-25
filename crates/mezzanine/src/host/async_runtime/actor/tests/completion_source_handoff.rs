//! Accepted worker completions transfer only newly ready dispatch sources.
//!
//! Real prompt/history admission and immutable history execution produce the
//! provider owner. Tests drive the actual actor completion boundary under its
//! unchanged finite allowance, without inference, native children, timer or
//! provider execution. Bookkeeping also settles its actual append-receipt
//! admission worker before storage cleanup, not final transcript persistence.

use super::super::*;

/// Owns generated stores until after the directly driven actor is dropped.
struct FixtureRoot(std::path::PathBuf);

impl Drop for FixtureRoot {
    /// Cleanup never replaces the primary test outcome with a second panic.
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("completion handoff fixture cleanup failed: {error}");
        }
    }
}

/// Prepares a genuinely admitted history claim, then fills the existing finite
/// reserve. The immutable history work is executed by each test, never forged.
fn history_at_capacity() -> (
    FixtureRoot,
    AsyncRuntimeSessionActor,
    ClientId,
    crate::runtime::RuntimeAgentPromptHistoryDispatch,
) {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    let root = FixtureRoot(std::env::temp_dir().join(format!(
        "mez-completion-handoff-{:032x}",
        rand::random::<u128>()
    )));
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service.set_config_root(root.0.clone());
    service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
        root.0.join("transcripts"),
    ));
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root.0),
    ));
    service.replace_config_layers(vec![ConfigLayer {
        name: "completion-handoff".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"pane\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=128000\n[permissions]\nsandbox=\"policy-only\"\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("completion-primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
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
    actor
        .service
        .begin_agent_prompt_history_preparation(
            primary.clone(),
            "%1",
            "retain accepted completion dispatch",
        )
        .unwrap();
    let dispatch = actor.service.take_pending_agent_prompt_history().remove(0);
    assert!(
        actor
            .service
            .claim_agent_prompt_history_preparation(&dispatch)
    );
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
    (root, actor, primary, dispatch)
}

/// Adopt actual prepared history through the ordinary completion handler while
/// no transient slot is free. The resulting real provider task must retain its
/// exact source and become offered once after finite backlog drain, without a
/// second prompt, history read, provider poll or artificial task insertion.
#[tokio::test(flavor = "current_thread")]
async fn accepted_history_completion_recovers_new_provider_source() {
    let (_root, mut actor, primary, dispatch) = history_at_capacity();
    let history =
        crate::runtime::execute_runtime_agent_prompt_history_work(dispatch.history_work.clone());
    assert!(history.is_ok());
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteAgentPromptHistoryPreparation {
            dispatch,
            history,
        }),
    )
    .await;
    let task = actor.service.pending_agent_provider_tasks().remove(0);
    let conversation = actor
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let title = actor
        .service
        .pending_agent_session_title_dispatch_id(&conversation)
        .unwrap()
        .to_string();
    assert!(
        actor
            .refused_dispatch_sources
            .contains(&SourceDispatch::Provider(task.turn_id.clone()))
    );
    actor.drain_timer_side_effects(64).unwrap();
    let offered = actor
        .drain_agent_provider_dispatch_side_effects(64)
        .unwrap();
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentProvider { turn_id, .. }, RuntimeSideEffect::DispatchAgentSessionTitle { conversation_id, dispatch_id }] if turn_id == &task.turn_id && conversation_id == &conversation && dispatch_id == &title),
        "{offered:?}"
    );
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert!(actor.provider_dispatch_is_already_queued(&task.turn_id));
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    actor.service.cancel_agent_session_title_task(&conversation);
}

/// A title ready before history adoption is an ordinary producer, not part of
/// the completion's transfer. Strict refusal must not be upgraded to committed
/// recovery merely because a new provider becomes ready in the same pane.
#[tokio::test(flavor = "current_thread")]
async fn accepted_history_completion_excludes_previously_ready_sources() {
    let (_root, mut actor, primary, dispatch) = history_at_capacity();
    let conversation = dispatch.conversation_id.clone();
    actor
        .service
        .schedule_runtime_agent_session_title(
            &conversation,
            Some("Preserve ordinary title scheduling."),
        )
        .unwrap();
    let title = actor
        .service
        .pending_agent_session_title_dispatch_id(&conversation)
        .unwrap()
        .to_string();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert_eq!(effects.len(), 1);
    assert!(actor.queue_runtime_side_effects(effects).is_err());
    let history =
        crate::runtime::execute_runtime_agent_prompt_history_work(dispatch.history_work.clone());
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteAgentPromptHistoryPreparation {
            dispatch,
            history,
        }),
    )
    .await;
    let task = actor.service.pending_agent_provider_tasks().remove(0);
    assert_eq!(
        actor
            .refused_dispatch_sources
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![SourceDispatch::Provider(task.turn_id.clone())]
    );
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        matches!(actor.drain_agent_provider_dispatch_side_effects(64).unwrap().as_slice(), [RuntimeSideEffect::DispatchAgentProvider { turn_id, .. }] if turn_id == &task.turn_id)
    );
    assert!(
        matches!(actor.pending_provider_dispatch_side_effects().unwrap().as_slice(), [RuntimeSideEffect::DispatchAgentSessionTitle { dispatch_id, .. }] if dispatch_id == &title)
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    actor.service.cancel_agent_session_title_task(&conversation);
}

/// A history read failure creates no provider or title authority. Retire the
/// command through the real handler, then free pressure and require no phantom
/// dispatch. Error paint is not successful prompt adoption or inference.
#[tokio::test(flavor = "current_thread")]
async fn accepted_history_completion_failure_does_not_invent_sources() {
    let (_root, mut actor, _primary, dispatch) = history_at_capacity();
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteAgentPromptHistoryPreparation {
            dispatch,
            history: Err(crate::MezError::invalid_state(
                "generated immutable history failure",
            )),
        }),
    )
    .await;
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
    assert!(actor.refused_dispatch_sources.is_empty());
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
}

/// A conversation rebind invalidates an actual history claim. Its first stale
/// completion retires that claim; a repeated old callback cannot affect the
/// replacement. Only the replacement's completion may transfer a new turn.
#[tokio::test(flavor = "current_thread")]
async fn accepted_history_completion_stale_callback_preserves_replacement() {
    let (_root, mut actor, primary, dispatch) = history_at_capacity();
    let history =
        crate::runtime::execute_runtime_agent_prompt_history_work(dispatch.history_work.clone());
    actor
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", "replacement-history", 0)
        .unwrap();
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteAgentPromptHistoryPreparation {
            dispatch: dispatch.clone(),
            history: history.clone(),
        }),
    )
    .await;
    actor
        .service
        .begin_agent_prompt_history_preparation(primary.clone(), "%1", "new exact history owner")
        .unwrap();
    let replacement = actor.service.take_pending_agent_prompt_history().remove(0);
    assert_ne!(dispatch.claim_generation, replacement.claim_generation);
    assert_ne!(dispatch.conversation_id, replacement.conversation_id);
    assert!(
        actor
            .service
            .claim_agent_prompt_history_preparation(&replacement)
    );
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteAgentPromptHistoryPreparation {
            dispatch,
            history,
        }),
    )
    .await;
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
    assert!(actor.refused_dispatch_sources.is_empty());
    let history =
        crate::runtime::execute_runtime_agent_prompt_history_work(replacement.history_work.clone());
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteAgentPromptHistoryPreparation {
            dispatch: replacement,
            history,
        }),
    )
    .await;
    let task = actor.service.pending_agent_provider_tasks().remove(0);
    assert!(
        actor
            .refused_dispatch_sources
            .contains(&SourceDispatch::Provider(task.turn_id.clone()))
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// With actual spare reserve, accepted completion must queue its new dispatch
/// immediately. A duplicate callback cannot create another turn or descriptor;
/// ordinary worker preparation and mandatory execution leases remain separate.
#[tokio::test(flavor = "current_thread")]
async fn accepted_history_completion_available_capacity_and_duplicate_are_exact() {
    let (_root, mut actor, primary, dispatch) = history_at_capacity();
    actor.drain_timer_side_effects(64).unwrap();
    let duplicate = dispatch.clone();
    let history =
        crate::runtime::execute_runtime_agent_prompt_history_work(dispatch.history_work.clone());
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteAgentPromptHistoryPreparation {
            dispatch,
            history: history.clone(),
        }),
    )
    .await;
    let task = actor.service.pending_agent_provider_tasks().remove(0);
    assert!(actor.provider_dispatch_is_already_queued(&task.turn_id));
    assert!(actor.refused_dispatch_sources.is_empty());
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteAgentPromptHistoryPreparation {
            dispatch: duplicate,
            history,
        }),
    )
    .await;
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), 1);
    let effects = actor
        .drain_agent_provider_dispatch_side_effects(64)
        .unwrap();
    assert_eq!(effects.iter().filter(|effect| matches!(effect, RuntimeSideEffect::DispatchAgentProvider { turn_id, .. } if turn_id == &task.turn_id)).count(), 1);
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// Collects only an actual worker's bounded-ingress completion, with a
/// registered notifier and finite outer watchdog rather than polling or sleeps.
async fn next_completion(actor: &mut AsyncRuntimeSessionActor) -> AsyncRuntimeRequest {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
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
                break request.envelope.request;
            }
            notified.await;
        }
    })
    .await
    .unwrap()
}

/// A real nonterminal bookkeeping producer temporarily hides an already owned
/// provider task. Checked adoption must recover that SAME task when it becomes
/// ready, not allocate a replacement turn or enrol the previously ready title.
/// The inert execution is ordinary transcript projection input, not evidence
/// that inference ran. Receipt admission is observed before fixture teardown.
#[tokio::test(flavor = "current_thread")]
async fn accepted_bookkeeping_completion_recovers_released_provider() {
    let (_root, mut actor, primary, dispatch) = history_at_capacity();
    let history =
        crate::runtime::execute_runtime_agent_prompt_history_work(dispatch.history_work.clone());
    assert!(
        actor
            .service
            .complete_agent_prompt_history_preparation(&dispatch, history)
            .unwrap()
    );
    let task = actor.service.pending_agent_provider_tasks().remove(0);
    let turn = actor
        .service
        .agent_turn_ledger()
        .turn(&task.turn_id)
        .unwrap()
        .clone();
    let execution = mez_agent::AgentTurnExecution {
        request: mez_agent::ModelRequest {
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
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::capability_decision(),
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: mez_agent::ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                content: "retain accepted completion dispatch".into(),
            }]
            .into(),
        },
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "inert bookkeeping projection".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: mez_agent::AgentTurnState::Running,
    };
    assert_eq!(
        actor
            .service
            .persist_runtime_agent_turn_execution_transcript(&turn, &execution)
            .unwrap(),
        0
    );
    let mut candidates = actor.service.claim_bookkeeping_candidates();
    assert_eq!(candidates.len(), 1);
    let work = candidates.remove(0);
    assert!(actor.service.agent_provider_task_is_pending(&task.turn_id));
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
    let history = work.execute();
    assert!(history.is_ok());
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteBookkeepingCandidate { work, history }),
    )
    .await;
    assert!(actor.transcript_receipt_predecessor.is_some());
    let completion = next_completion(&mut actor).await;
    assert!(
        matches!(&completion, AsyncRuntimeRequest::CompleteTranscriptReceipts { results, .. } if !results.is_empty() && results.iter().all(|(_, result)| result.is_ok()))
    );
    Box::pin(actor.handle_request(completion)).await;
    assert_eq!(
        actor
            .refused_dispatch_sources
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![SourceDispatch::Provider(task.turn_id.clone())]
    );
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        matches!(actor.drain_agent_provider_dispatch_side_effects(64).unwrap().as_slice(), [RuntimeSideEffect::DispatchAgentProvider { turn_id, .. }] if turn_id == &task.turn_id)
    );
    assert_eq!(actor.service.agent_turn_ledger().turns().len(), 1);
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    actor
        .service
        .cancel_agent_session_title_task(&turn.conversation_id);
}

/// Checked bookkeeping is an accepted source boundary too. Fill the original
/// queue, produce a genuine stopped-turn candidate, then adopt its real archive
/// read. An unrelated service-owned input must remain retained until progress,
/// while append-receipt admission completes before generated storage is removed.
#[tokio::test(flavor = "current_thread")]
async fn accepted_bookkeeping_completion_preserves_service_sources() {
    let (_root, mut actor, primary, dispatch) = history_at_capacity();
    let history =
        crate::runtime::execute_runtime_agent_prompt_history_work(dispatch.history_work.clone());
    assert!(
        actor
            .service
            .complete_agent_prompt_history_preparation(&dispatch, history)
            .unwrap()
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    let mut candidates = actor.service.claim_bookkeeping_candidates();
    assert_eq!(candidates.len(), 1);
    let work = candidates.remove(0);
    let history = work.execute();
    assert!(history.is_ok());
    actor
        .service
        .queue_deferred_pane_input_for_tests(RuntimeSideEffect::WritePaneInputPriority {
            pane_id: "%1".into(),
            bytes: b"accepted-service-input".to_vec(),
        });
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteBookkeepingCandidate { work, history }),
    )
    .await;
    assert!(actor.transcript_receipt_predecessor.is_some());
    let completion = next_completion(&mut actor).await;
    assert!(
        matches!(&completion, AsyncRuntimeRequest::CompleteTranscriptReceipts { results, .. } if !results.is_empty() && results.iter().all(|(_, result)| result.is_ok()))
    );
    Box::pin(actor.handle_request(completion)).await;
    actor.drain_timer_side_effects(64).unwrap();
    let effects = actor.drain_runtime_side_effects(64).unwrap();
    assert_eq!(effects.iter().filter(|effect| matches!(effect, RuntimeSideEffect::WritePaneInputPriority { bytes, .. } if bytes == b"accepted-service-input")).count(), 1, "{effects:?}");
    assert!(actor.service.pending_agent_provider_tasks().is_empty());
}
