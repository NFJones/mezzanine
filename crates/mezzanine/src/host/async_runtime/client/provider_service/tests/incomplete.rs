//! Offline exact-inference replacement and issued-expense qualification.
//!
//! Synthetic unary/SSE errors cross the real product runner and observation
//! adapters, but no provider, credential, vendor, or user transcript is used.
//! Actor tests preserve claim fencing, cancellation expense and final dedup.

use super::*;
use crate::runtime::RuntimeSessionService;
use std::sync::Mutex;

mod crash_qualification;
mod native_retirement;

/// Records prepared requests and injects authoritative parser-produced errors.
struct IncompleteProvider {
    requests: Mutex<Vec<ModelRequest>>,
    failures: usize,
    stream: bool,
}

impl AsyncModelProvider for IncompleteProvider {
    fn provider_id(&self) -> &str {
        "local-chat"
    }
    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        mez_agent::ProviderApiCompatibility::OpenAiResponses
    }
    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            let call = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.clone());
                requests.len()
            };
            if call <= self.failures {
                let response = serde_json::json!({"status":"incomplete",
                    "incomplete_details":{"reason":"max_messages"},
                    "usage":{"input_tokens":17,"output_tokens":3,"input_tokens_details":{"cached_tokens":4}},
                    "output":[{"type":"function_call","name":"submit_maap_action_batch","call_id":"never_execute",
                        "arguments":"{\"rationale\":\"discard\",\"actions\":[{\"type\":\"shell_command\",\"summary\":\"must not run\",\"command\":\"touch NEVER_EXECUTE\"}]}"}]});
                let body = if self.stream {
                    format!(
                        "data: {}\n\n",
                        serde_json::json!({"type":"response.incomplete","response":response})
                    )
                } else {
                    response.to_string()
                };
                return Err(mez_agent::parse_openai_responses_provider_body(
                    &body,
                    "test-model",
                    self.stream,
                )
                .unwrap_err()
                .into());
            }
            Ok(ModelResponse {
                provider: "local-chat".into(),
                model: "test-model".into(),
                raw_text: "complete replacement".into(),
                usage: mez_agent::ModelTokenUsage {
                    input_tokens: 5,
                    output_tokens: 2,
                    cached_input_tokens: Some(0),
                    ..Default::default()
                },
                latest_request_usage: None,
                quota_usage: Vec::new(),
                provider_transcript_events: Vec::new(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "complete replacement".into(),
                    actions: vec![mez_agent::AgentAction {
                        id: "final".into(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            content_type: "text/plain; charset=utf-8".into(),
                            text: "complete replacement".into(),
                        },
                    }],
                }),
            })
        })
    }
}

/// Prepares a real auth-free native turn without claiming it. Production actor
/// admission may assign its own generation; older component controls can still
/// install their explicit claim through `issued_fixture` below.
async fn prepared_fixture(prompt: &str) -> (RuntimeSessionService, AgentId, String) {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let root = std::env::temp_dir().join(format!(
        "mez-offline-incomplete-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root.join("auth")),
    ));
    service.set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
        root.join("usage.sqlite"),
    ));
    service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
        root.join("transcripts"),
    ));
    service.replace_config_layers(vec![ConfigLayer {name:"offline-incomplete".into(),path:None,
        format:ConfigFormat::Toml,scope:ConfigScope::Primary,trusted:true,
        text:"[agents]\nrouting=false\nshell_mode=\"native\"\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\n[permissions]\nsandbox=\"policy-only\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:1/v1\"\nmodels=[\"test-model\"]\ndefault_model=\"test-model\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"test-model\"\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary(
            "primary",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, prompt)
        .unwrap();
    let task = service.pending_agent_provider_tasks().remove(0);
    let turn = service
        .agent_turn_ledger()
        .turn(&task.turn_id)
        .unwrap()
        .clone();
    let preparation = service.prepare_agent_provider_work(&turn.turn_id).unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
    (service, agent, turn.turn_id)
}

/// Creates the explicit generation-seven dispatch used by expense component
/// controls. Runtime-profile qualification instead uses actual actor admission.
async fn issued_fixture() -> (RuntimeSessionService, RuntimeAgentProviderDispatch) {
    let (mut service, agent, turn_id) = prepared_fixture("replace incomplete inference").await;
    let mut dispatch = service
        .claim_configured_agent_provider_task(&agent, &turn_id)
        .unwrap()
        .unwrap();
    service
        .record_claimed_agent_provider_task(&dispatch, 7, 30_000)
        .unwrap();
    dispatch.claim_generation = 7;
    (service, dispatch)
}

/// Runs the exact production observation/runner adapter with caller-controlled
/// backoff. The fixture changes clocks only, not provider result acceptance.
async fn run_incomplete_fixture<P: AsyncModelProvider>(
    provider: &P,
    observer: &ProviderWireRequestObserver,
    dispatch: &RuntimeAgentProviderDispatch,
    delay_ms: u64,
    progress: Option<tokio::sync::mpsc::Sender<mez_agent::StreamingSayEvent>>,
) -> Result<mez_agent::AgentTurnExecution> {
    let context = dispatch.context.clone().into_agent_context();
    let approvals = mez_agent::permissions::SessionApprovalStore::default();
    let policy = mez_agent::permissions::PermissionPolicy::default();
    let permissions =
        crate::security::permissions::ProductPermissionPlanning::new(&policy, &approvals, None);
    let inputs = ObservedTurnInputs {
        turn: &dispatch.turn,
        context: &context,
        allowed_actions: &dispatch.allowed_actions,
        interaction_kind: dispatch.interaction_kind,
        model_profile: &dispatch.model_profile,
        permissions: &permissions,
        subagent_scope: None,
        available_mcp_servers: &[],
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: false,
        progress: &progress,
        turn_limits: mez_agent::AgentTurnLimits {
            provider_retry_policy: mez_agent::ProviderRetryPolicy {
                initial_delay_ms: delay_ms,
                max_delay_ms: delay_ms,
                ..dispatch.turn_limits.provider_retry_policy
            },
            ..dispatch.turn_limits
        },
    };
    execute_observed_provider_turn(provider, observer, &inputs, None).await
}

/// Unary and SSE incompletes remain non-executable and retain every reported
/// counter. The runner retries identical input, accepts one complete final,
/// and checkpoint plus final/duplicate delivery never bills the expense twice.
#[tokio::test]
async fn max_messages_product_replacement_and_known_usage_once() {
    for stream in [false, true] {
        let (mut service, dispatch) = issued_fixture().await;
        let provider = IncompleteProvider {
            requests: Mutex::new(Vec::new()),
            failures: 2,
            stream,
        };
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        let observer =
            ProviderWireRequestObserver::new(&dispatch.turn.conversation_id, "%1", sender)
                .with_claim_generation(7);
        let context = dispatch.context.clone().into_agent_context();
        let approvals = mez_agent::permissions::SessionApprovalStore::default();
        let policy = mez_agent::permissions::PermissionPolicy::default();
        let permissions =
            crate::security::permissions::ProductPermissionPlanning::new(&policy, &approvals, None);
        let progress = None;
        let inputs = ObservedTurnInputs {
            turn: &dispatch.turn,
            context: &context,
            allowed_actions: &dispatch.allowed_actions,
            interaction_kind: dispatch.interaction_kind,
            model_profile: &dispatch.model_profile,
            permissions: &permissions,
            subagent_scope: None,
            available_mcp_servers: &[],
            available_mcp_tools: &[],
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            progress: &progress,
            turn_limits: mez_agent::AgentTurnLimits {
                provider_retry_policy: mez_agent::ProviderRetryPolicy {
                    initial_delay_ms: 0,
                    max_delay_ms: 0,
                    ..mez_agent::DEFAULT_PROVIDER_RETRY_POLICY
                },
                ..Default::default()
            },
        };
        let execution = execute_observed_provider_turn(&provider, &observer, &inputs, None)
            .await
            .unwrap();
        assert_eq!(execution.response.usage.input_tokens, 39);
        assert_eq!(execution.response.usage.output_tokens, 8);
        assert_eq!(execution.response.usage.cached_input_tokens, Some(8));
        assert_eq!(execution.action_results.len(), 1);
        assert!(matches!(
            execution.response.action_batch.as_ref().unwrap().actions[0].payload,
            mez_agent::AgentActionPayload::Say { .. }
        ));
        {
            let requests = provider.requests.lock().unwrap();
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[0], requests[1]);
            assert_eq!(requests[1], requests[2]);
        }
        while let Ok(observation) = receiver.try_recv() {
            service.checkpoint_provider_wire_usage(&observation);
            service.checkpoint_provider_wire_usage(&observation);
        }
        let expected_usage = execution.response.usage;
        let (handle, actor) =
            crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
                .unwrap();
        let client = async {
            let agent = AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap();
            let mut stale = RuntimeEventBatch::new();
            stale.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
                agent_id: agent.clone(),
                turn_id: dispatch.turn.turn_id.clone(),
                claim_generation: 8,
                execution: Box::new(execution.clone()),
            }));
            assert_eq!(
                handle.submit_runtime_events(stale).await.unwrap().applied,
                0
            );
            assert!(
                handle
                    .agent_turn_is_running(&dispatch.turn.turn_id)
                    .await
                    .unwrap()
            );
            let event = RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
                agent_id: agent,
                turn_id: dispatch.turn.turn_id.clone(),
                claim_generation: 7,
                execution: Box::new(execution),
            });
            let mut batch = RuntimeEventBatch::new();
            batch.push(event.clone());
            assert_eq!(
                handle.submit_runtime_events(batch).await.unwrap().applied,
                1
            );
            let mut batch = RuntimeEventBatch::new();
            batch.push(event);
            assert_eq!(
                handle.submit_runtime_events(batch).await.unwrap().applied,
                0
            );
            let work = handle
                .drain_persistence_side_effects(64)
                .await
                .unwrap()
                .into_iter()
                .find_map(|effect| match effect {
                    RuntimeSideEffect::SettleAgentProviderPersistence { work } => Some(work),
                    _ => None,
                })
                .expect("accepted replacement must complete its persistence phase");
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
                handle.submit_runtime_events(batch).await.unwrap().applied,
                1
            );
            assert!(
                !handle
                    .agent_turn_is_running(&dispatch.turn.turn_id)
                    .await
                    .unwrap()
            );
            handle.shutdown().await.unwrap();
        };
        let ((), exit) = tokio::join!(client, actor.run());
        let service = exit.service;
        let key = mez_agent::ModelTokenUsageKey::new("local-chat", "test-model");
        assert_eq!(
            service.agent_token_usage_for_conversation(&dispatch.turn.conversation_id)[&key],
            expected_usage
        );
        assert!(!std::path::Path::new("NEVER_EXECUTE").exists());
    }
}

/// Repeated authoritative incomplete errors exhaust the captured finite count,
/// return original cumulative expense and settle the actor exactly once.
/// Stale worker failure cannot consume another generation or reopen recovery.
#[tokio::test]
async fn max_messages_actor_repeated_exhaustion_is_terminal_once() {
    let (service, mut dispatch) = issued_fixture().await;
    dispatch.turn_limits.provider_retry_policy.max_attempts = 2;
    dispatch.turn_limits.provider_retry_policy.unlimited = true;
    let provider = IncompleteProvider {
        requests: Mutex::new(Vec::new()),
        failures: usize::MAX,
        stream: false,
    };
    let (sender, mut observations) = tokio::sync::mpsc::channel(8);
    let observer = ProviderWireRequestObserver::new(&dispatch.turn.conversation_id, "%1", sender)
        .with_claim_generation(7);
    let error = run_incomplete_fixture(&provider, &observer, &dispatch, 0, None)
        .await
        .unwrap_err();
    assert_eq!(provider.requests.lock().unwrap().len(), 3);
    assert_eq!(error.provider_incurred_usage().input_tokens, 51);
    assert_eq!(
        error.recovery_facts().unwrap().disposition,
        mez_agent::recovery_attribution::RecoveryDisposition::BudgetExhausted
    );
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        while let Ok(observation) = observations.try_recv() {
            submit_provider_wire_request_observation(&handle, observation)
                .await
                .unwrap();
        }
        let agent = AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap();
        let stale_error = crate::integrations::agent::provider::provider_event_error_from_parts(
            "invalid_state",
            error.message(),
            error.provider_failure_json(),
            None,
        )
        .with_provider_incurred_usage(error.provider_incurred_usage())
        .with_recovery_facts(error.recovery_facts().unwrap().clone());
        let (stale, _) = provider_worker_event(
            agent.clone(),
            dispatch.turn.turn_id.clone(),
            8,
            Ok(Err(stale_error)),
        );
        let mut batch = RuntimeEventBatch::new();
        batch.push(stale);
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            0
        );
        assert!(
            handle
                .agent_turn_is_running(&dispatch.turn.turn_id)
                .await
                .unwrap()
        );
        let (event, _) =
            provider_worker_event(agent, dispatch.turn.turn_id.clone(), 7, Ok(Err(error)));
        let mut batch = RuntimeEventBatch::new();
        batch.push(event.clone());
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            1
        );
        let mut batch = RuntimeEventBatch::new();
        batch.push(event);
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            0
        );
        assert!(
            !handle
                .agent_turn_is_running(&dispatch.turn.turn_id)
                .await
                .unwrap()
        );
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    let service = exit.service;
    let key = mez_agent::ModelTokenUsageKey::new("local-chat", "test-model");
    assert_eq!(
        service.agent_token_usage_for_conversation(&dispatch.turn.conversation_id)[&key]
            .input_tokens,
        51
    );
    assert!(
        service
            .agent_latest_request_usage(&dispatch.turn.conversation_id)
            .is_none()
    );
}

/// A reported incomplete wire checkpoint survives cancellation and stale
/// content ownership. Wrong generation/agent/model cannot steal an issued
/// expense owner; repeated checkpoints and late cumulative final are inert.
#[tokio::test]
async fn max_messages_checkpoint_cancel_stale_and_mismatched_owner() {
    let (mut service, dispatch) = issued_fixture().await;
    let provider = IncompleteProvider {
        requests: Mutex::new(Vec::new()),
        failures: usize::MAX,
        stream: false,
    };
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&dispatch.turn.conversation_id, "%1", sender)
        .with_claim_generation(7);
    let request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    let provider =
        ObservedAsyncModelProvider::new(&provider, &observer, ProviderRequestPurpose::Execution);
    let error = provider.send_request_async(&request).await.unwrap_err();
    let observation = receiver.recv().await.unwrap();
    for mismatch in 0..3 {
        let mut wrong = observation.clone();
        match mismatch {
            0 => wrong.claim_generation = Some(8),
            1 => wrong.agent_id = "foreign".into(),
            _ => wrong.model = "foreign".into(),
        }
        service.checkpoint_provider_wire_usage(&wrong);
    }
    let key = mez_agent::ModelTokenUsageKey::new("local-chat", "test-model");
    assert!(
        service
            .agent_token_usage_for_conversation(&dispatch.turn.conversation_id)
            .is_empty()
    );
    service.clear_claimed_agent_provider_task(&dispatch.turn.turn_id);
    service.checkpoint_provider_wire_usage(&observation);
    service.checkpoint_provider_wire_usage(&observation);
    assert_eq!(
        service.agent_token_usage_for_conversation(&dispatch.turn.conversation_id)[&key],
        error.provider_incurred_usage()
    );
    let agent = AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap();
    assert!(service.settle_provider_failure_usage(
        &agent,
        &dispatch.turn.turn_id,
        7,
        error.provider_incurred_usage()
    ));
    assert!(!service.settle_provider_failure_usage(
        &agent,
        &dispatch.turn.turn_id,
        7,
        error.provider_incurred_usage()
    ));
    assert_eq!(
        service.agent_token_usage_for_conversation(&dispatch.turn.conversation_id)[&key],
        error.provider_incurred_usage()
    );
}

/// The real production relay must apply original response barriers in order
/// even with installed provisional output and a full non-render queue. Repaint
/// pressure may evict pixels, not source ownership or authoritative outcomes.
/// Equal later source/index belongs to response two, stale response-zero work
/// cannot install, and accepted source keeps its actual first-observation stamp.
#[tokio::test(flavor = "current_thread")]
async fn provider_occurrence_relay_pressure_preserves_original_boundaries_and_outcomes() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    use mez_agent::StreamingSayEvent as Event;
    for succeeds in [false, true] {
        let (service, dispatch) = issued_fixture().await;
        let store = service.agent_transcript_store().unwrap().clone();
        let agent = AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap();
        let turn = dispatch.turn.turn_id.clone();
        let (handle, actor) = AsyncRuntimeSessionActor::new(
            service,
            AsyncRuntimeActorConfig {
                side_effect_buffer: 2,
                ..Default::default()
            },
        )
        .unwrap();
        let client = async {
            handle.drain_runtime_side_effects(1024).await.unwrap();
            let first = vec![
                Event::ResponseStarted { response_index: 0 },
                Event::Started {
                    action_index: 0,
                    status: mez_agent::SayStatus::Final,
                    content_type: "text/plain; charset=utf-8".into(),
                },
                Event::TextDelta {
                    action_index: 0,
                    text: "complete replacement".into(),
                },
            ];
            assert_eq!(
                relay_agent_streaming_progress(&handle, &agent, &turn, "%1", 7, first)
                    .await
                    .unwrap()
                    .unwrap()
                    .applied,
                3
            );
            let old_work = handle
                .take_streaming_say_projection_work("%1".into(), turn.clone())
                .await
                .unwrap()
                .unwrap();
            let old_projection =
                RuntimeSessionService::build_agent_streaming_say_projection(old_work.clone())
                    .unwrap();
            let old_entry = old_projection
                .screen
                .normal_styled_content_lines()
                .into_iter()
                .find(|row| row.text.contains("complete replacement"))
                .unwrap()
                .presentation
                .unwrap();
            assert!(
                handle
                    .apply_streaming_say_projection(old_projection)
                    .await
                    .unwrap()
            );
            handle.drain_runtime_side_effects(1024).await.unwrap();
            handle
                .queue_runtime_side_effects(vec![
                    RuntimeSideEffect::DispatchAgentProvider {
                        agent_id: agent.clone(),
                        turn_id: "pressure-one".into(),
                    },
                    RuntimeSideEffect::DispatchAgentProvider {
                        agent_id: agent.clone(),
                        turn_id: "pressure-two".into(),
                    },
                ])
                .await
                .unwrap();
            let barriers = vec![
                Event::ResponseRejected { response_index: 0 },
                Event::ResponseStarted { response_index: 1 },
                Event::ResponseStarted { response_index: 2 },
            ];
            // Observe real admission, not the optional wrapper's name. Queue
            // policy evicts repaint effects without dropping source barriers.
            assert_eq!(
                relay_agent_streaming_progress(&handle, &agent, &turn, "%1", 7, barriers)
                    .await
                    .unwrap()
                    .unwrap()
                    .applied,
                3
            );
            assert!(
                handle
                    .metrics()
                    .await
                    .unwrap()
                    .render_client_side_effects_evicted
                    > 0
            );
            let stale = vec![
                Event::ResponseStarted { response_index: 99 },
                Event::Started {
                    action_index: 0,
                    status: mez_agent::SayStatus::Final,
                    content_type: "text/plain; charset=utf-8".into(),
                },
                Event::TextDelta {
                    action_index: 0,
                    text: "complete replacement".into(),
                },
            ];
            assert_eq!(
                relay_agent_streaming_progress(&handle, &agent, &turn, "%1", 8, stale)
                    .await
                    .unwrap()
                    .unwrap()
                    .applied,
                0
            );
            let body = vec![
                Event::Started {
                    action_index: 0,
                    status: mez_agent::SayStatus::Final,
                    content_type: "text/plain; charset=utf-8".into(),
                },
                Event::TextDelta {
                    action_index: 0,
                    text: "complete replacement".into(),
                },
                Event::TextComplete { action_index: 0 },
                Event::ActionComplete { action_index: 0 },
            ];
            assert_eq!(
                relay_agent_streaming_progress(&handle, &agent, &turn, "%1", 7, body)
                    .await
                    .unwrap()
                    .unwrap()
                    .applied,
                4
            );
            let work = handle
                .take_streaming_say_projection_work("%1".into(), turn.clone())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(work.response_index, 2);
            let projected =
                RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
            let entry = projected
                .screen
                .normal_styled_content_lines()
                .into_iter()
                .find(|row| row.text.contains("complete replacement"))
                .unwrap()
                .presentation
                .unwrap();
            assert_ne!(entry.id, old_entry.id);
            assert_eq!(
                entry.occurrence.basis,
                mez_core::presentation::PresentationOccurrenceBasis::LocalObservation
            );
            assert!(entry.occurrence.unix_millis.is_some());
            assert!(
                !handle
                    .apply_streaming_say_projection(
                        RuntimeSessionService::build_agent_streaming_say_projection(old_work)
                            .unwrap()
                    )
                    .await
                    .unwrap()
            );
            assert!(
                handle
                    .drain_persistence_side_effects(1024)
                    .await
                    .unwrap()
                    .is_empty()
            );
            let queued = handle.drain_runtime_side_effects(1024).await.unwrap();
            for expected in ["pressure-one", "pressure-two"] {
                assert!(queued.iter().any(|effect| matches!(effect, RuntimeSideEffect::DispatchAgentProvider { turn_id, .. } if turn_id == expected)));
            }
            let result = if succeeds {
                let provider = IncompleteProvider {
                    requests: Mutex::new(Vec::new()),
                    failures: 0,
                    stream: false,
                };
                let (sender, mut observations) = tokio::sync::mpsc::channel(8);
                let observer =
                    ProviderWireRequestObserver::new(&dispatch.turn.conversation_id, "%1", sender)
                        .with_claim_generation(7);
                let execution = run_incomplete_fixture(&provider, &observer, &dispatch, 0, None)
                    .await
                    .unwrap();
                while let Ok(observation) = observations.try_recv() {
                    submit_provider_wire_request_observation(&handle, observation)
                        .await
                        .unwrap();
                }
                Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(
                    execution,
                )))
            } else {
                Err(MezError::invalid_state(
                    "offline authoritative failure after pressure",
                ))
            };
            let (event, _) = provider_worker_event(agent.clone(), turn.clone(), 7, Ok(result));
            let mut batch = RuntimeEventBatch::new();
            batch.push(event);
            assert_eq!(
                handle.submit_runtime_events(batch).await.unwrap().applied,
                1
            );
            if succeeds {
                // Acceptance is not durable completion. Execute the actual
                // canonical persistence work and acknowledge its exact outcome
                // through the same actor ingress used by the worker fixture.
                // Ordinary adapters consume queued title/timer work in between
                // these phases. Keep the tiny pressure budget unchanged and
                // drain those original effects, never retry an applied event.
                let mut work = None;
                let mut publication = Vec::new();
                for effect in handle.drain_persistence_side_effects(64).await.unwrap() {
                    match effect {
                        RuntimeSideEffect::SettleAgentProviderPersistence { work: canonical } => {
                            assert!(work.replace(canonical).is_none());
                        }
                        original_unexecuted => publication.push(original_unexecuted),
                    }
                }
                let work =
                    work.expect("accepted source must enter canonical persistence settlement");
                // Claim the canonical route first: a generic drain also owns
                // persistence routes and cannot be used before this handoff.
                handle.drain_runtime_side_effects(1024).await.unwrap();
                // Do not discard publication work claimed beside the canonical
                // job. Hand these unchanged, never-executed effects to the real
                // adapter before the canonical ack can publish later rows.
                if !publication.is_empty() {
                    handle
                        .queue_runtime_side_effects(publication)
                        .await
                        .unwrap();
                    let report =
                        crate::host::async_runtime::run_async_persistence_side_effect_service(
                            &handle,
                            crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
                                max_polls: 2,
                                drain_limit: 64,
                                idle_interval: Duration::from_millis(1),
                            },
                            |polls, _| polls >= 2,
                        )
                        .await
                        .unwrap();
                    assert_eq!(report.failed, 0);
                    handle.drain_runtime_side_effects(1024).await.unwrap();
                }
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
                    handle.submit_runtime_events(batch).await.unwrap().applied,
                    1
                );
                // Final presentation publication is a separate actor adapter
                // route, not proof supplied by the canonical transcript ack.
                let report = crate::host::async_runtime::run_async_persistence_side_effect_service(
                    &handle,
                    crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
                        max_polls: 2,
                        drain_limit: 64,
                        idle_interval: Duration::from_millis(1),
                    },
                    |polls, _| polls >= 2,
                )
                .await
                .unwrap();
                assert_eq!(report.failed, 0);
            }
            assert!(
                !handle.agent_turn_is_running(&turn).await.unwrap(),
                "succeeds={succeeds}"
            );
            handle.shutdown().await.unwrap();
            entry
        };
        let (entry, exit) = tokio::join!(client, actor.run());
        let state = exit.service.agent_turn_ledger().turn(&turn).unwrap().state;
        assert_eq!(
            state,
            if succeeds {
                mez_agent::AgentTurnState::Completed
            } else {
                mez_agent::AgentTurnState::Failed
            }
        );
        if succeeds {
            let rows = store
                .inspect_presentation(&dispatch.turn.conversation_id)
                .unwrap();
            assert!(
                rows.iter()
                    .any(|row| row.presentation.as_ref() == Some(&entry)),
                "expected={entry:?}; actual={rows:?}"
            );
            let usage = exit
                .service
                .agent_token_usage_for_conversation(&dispatch.turn.conversation_id);
            assert_eq!(
                usage[&mez_agent::ModelTokenUsageKey::new("local-chat", "test-model")].input_tokens,
                5
            );
        }
    }
}

/// Reports the parser-backed response boundary without controlling cancellation.
struct NotifyingProvider {
    inner: std::sync::Arc<IncompleteProvider>,
    received: std::sync::Arc<tokio::sync::Notify>,
}

impl AsyncModelProvider for NotifyingProvider {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }
    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        self.inner.api_compatibility()
    }
    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            let result = self.inner.send_request_async(request).await;
            self.received.notify_one();
            result
        })
    }
}

/// Actor /stop during incomplete recovery makes the real production monitor
/// cancel its canonical future automatically, rather than requiring the test
/// to abort a worker. Known expense survives and no replacement action runs.
#[tokio::test]
async fn max_messages_stop_during_backoff_preserves_expense_and_executes_nothing() {
    let (mut service, dispatch) = issued_fixture().await;
    let primary = service
        .attach_primary(
            "cancel-primary",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let provider = std::sync::Arc::new(IncompleteProvider {
        requests: Mutex::new(Vec::new()),
        failures: usize::MAX,
        stream: true,
    });
    let received = std::sync::Arc::new(tokio::sync::Notify::new());
    let worker_provider = NotifyingProvider {
        inner: provider.clone(),
        received: received.clone(),
    };
    let worker_dispatch = dispatch.clone();
    let drains: ProviderObservationDrains = Default::default();
    let monitor = tokio::spawn(monitor_agent_provider_future(
        handle.clone(),
        AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap(),
        dispatch.turn.turn_id.clone(),
        "%1".into(),
        7,
        drains.clone(),
        move |progress, sender, pending| async move {
            let observer = ProviderWireRequestObserver::new(
                &worker_dispatch.turn.conversation_id,
                "%1",
                sender,
            )
            .with_claim_generation(7)
            .with_pending_observations(pending);
            let execution = run_incomplete_fixture(
                &worker_provider,
                &observer,
                &worker_dispatch,
                100_000,
                Some(progress),
            )
            .await?;
            Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(
                execution,
            )))
        },
    ));
    let client = async {
        received.notified().await;
        handle
            .execute_agent_shell_command(primary, "/stop".into())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), monitor)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .is_none()
        );
        assert!(drains.lock().unwrap().is_empty());
        assert!(
            !handle
                .agent_turn_is_running(&dispatch.turn.turn_id)
                .await
                .unwrap()
        );
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    let key = mez_agent::ModelTokenUsageKey::new("local-chat", "test-model");
    let usage = exit
        .service
        .agent_token_usage_for_conversation(&dispatch.turn.conversation_id);
    assert_eq!(usage[&key].input_tokens, 17);
    assert_eq!(usage[&key].output_tokens, 3);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert!(
        exit.service
            .agent_latest_request_usage(&dispatch.turn.conversation_id)
            .is_none()
    );
}

/// Service-owned cleanup survives aborting a monitor while its producer is
/// blocked on a full wire-observation channel. Both reported attempts are
/// flushed only after producer destruction, through ordinary actor ingress,
/// and duplicate journal delivery cannot bill either attempt twice.
#[tokio::test]
async fn max_messages_service_stop_flushes_backpressured_wire_journal() {
    let (service, dispatch) = issued_fixture().await;
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let pending: PendingProviderWireObservations = Default::default();
    let drains: ProviderObservationDrains = Default::default();
    let (sender, _receiver) = tokio::sync::mpsc::channel(1);
    let (completed_sender, completed) = tokio::sync::oneshot::channel();
    let reached = std::sync::Arc::new(tokio::sync::Notify::new());
    let worker_reached = reached.clone();
    let worker_pending = pending.clone();
    let worker_dispatch = dispatch.clone();
    let producer = tokio::spawn(async move {
        let _completed = ProviderCompletionOnDrop(Some(completed_sender));
        let provider = IncompleteProvider {
            requests: Mutex::new(Vec::new()),
            failures: usize::MAX,
            stream: false,
        };
        let observer =
            ProviderWireRequestObserver::new(&worker_dispatch.turn.conversation_id, "%1", sender)
                .with_claim_generation(7)
                .with_pending_observations(worker_pending);
        let request = crate::integrations::agent::context::assemble_model_request(
            &worker_dispatch.model_profile,
            provider.api_compatibility(),
            &worker_dispatch.turn,
            worker_dispatch.context.durable(),
        )
        .unwrap();
        let provider = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Execution,
        );
        let _ = provider.send_request_async(&request).await;
        worker_reached.notify_one();
        let _ = provider.send_request_async(&request).await;
    });
    let abort = producer.abort_handle();
    drains.lock().unwrap().insert(
        ProviderObservationOwner::Inference(dispatch.turn.turn_id.clone(), 7),
        ProviderObservationDrain {
            abort: abort.clone(),
            completed,
            pending: pending.clone(),
            reported: None,
        },
    );
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    spawn_owned_agent_provider_worker(&mut workers, &mut owners, None, async move {
        let _owner = AbortProviderWorkerOnDrop(abort);
        std::future::pending::<Result<AsyncAgentProviderWorkerResult>>().await
    });
    let client = async {
        reached.notified().await;
        // The producer records its second observation synchronously before
        // yielding on the full channel; this bounded handoff is not a delay.
        while pending.lock().unwrap().len() < 2 {
            tokio::task::yield_now().await;
        }
        abort_agent_provider_workers(&mut workers, &handle, &drains, &mut owners)
            .await
            .unwrap();
        let _ = producer.await;
        assert!(pending.lock().unwrap().is_empty());
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    let key = mez_agent::ModelTokenUsageKey::new("local-chat", "test-model");
    assert_eq!(
        exit.service
            .agent_token_usage_for_conversation(&dispatch.turn.conversation_id)[&key]
            .input_tokens,
        34
    );
    assert_eq!(
        exit.service
            .agent_token_usage_for_conversation(&dispatch.turn.conversation_id)[&key]
            .output_tokens,
        6
    );
}

/// Partial checkpoints plus a cumulative final preserve inclusive input,
/// output, reasoning and both optional cache categories. Unknown is not zero:
/// known/unknown ordering must agree in conversation, pane/project partitions
/// and persisted history, and duplicate final delivery remains inert.
#[tokio::test]
async fn max_messages_partial_checkpoint_cache_categories_and_persisted_totals() {
    for (first_cache, last_cache) in [(Some(4), Some(2)), (Some(4), None), (None, Some(2))] {
        let (mut service, dispatch) = issued_fixture().await;
        let root = std::env::temp_dir().join(format!(
            "mez-incomplete-counters-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root).unwrap();
        let store = crate::storage::token_usage::TokenUsageStore::new(root.join("usage.sqlite"));
        service.set_token_usage_store(store.clone());
        let provider = IncompleteProvider {
            requests: Mutex::new(Vec::new()),
            failures: usize::MAX,
            stream: false,
        };
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let observer =
            ProviderWireRequestObserver::new(&dispatch.turn.conversation_id, "%1", sender)
                .with_claim_generation(7);
        let request = crate::integrations::agent::context::assemble_model_request(
            &dispatch.model_profile,
            provider.api_compatibility(),
            &dispatch.turn,
            dispatch.context.durable(),
        )
        .unwrap();
        let provider = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Execution,
        );
        let _ = provider.send_request_async(&request).await;
        let mut observation = receiver.recv().await.unwrap();
        let first = mez_agent::ModelTokenUsage {
            input_tokens: 17,
            output_tokens: 3,
            reasoning_tokens: 1,
            cached_input_tokens: first_cache,
            cache_write_input_tokens: first_cache,
        };
        let last = mez_agent::ModelTokenUsage {
            input_tokens: 5,
            output_tokens: 2,
            reasoning_tokens: 2,
            cached_input_tokens: last_cache,
            cache_write_input_tokens: last_cache,
        };
        observation.usage = Some(first);
        service.checkpoint_provider_wire_usage(&observation);
        service.checkpoint_provider_wire_usage(&observation);
        let mut expected = first;
        expected.add_assign(last);
        let agent = AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap();
        assert!(service.settle_provider_failure_usage(&agent, &dispatch.turn.turn_id, 7, expected));
        assert!(!service.settle_provider_failure_usage(
            &agent,
            &dispatch.turn.turn_id,
            7,
            expected
        ));
        let key = mez_agent::ModelTokenUsageKey::new("local-chat", "test-model");
        assert_eq!(
            service.agent_token_usage_for_conversation(&dispatch.turn.conversation_id)[&key],
            expected
        );
        assert_eq!(
            service.project_usage_for_conversation(&dispatch.turn.conversation_id)[0].usage,
            expected
        );
        assert_eq!(service.project_usage_for_pane("%1")[0].usage, expected);
        let history = store
            .history_snapshot(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                &[1],
                &crate::storage::token_usage::TokenHistoryScope::default(),
            )
            .unwrap();
        assert_eq!(history.windows[&1].values().next().unwrap().usage, expected);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// A finished inference monitor may still be waiting in the service JoinSet.
/// Retirement must adopt that original result once, count its execution and run
/// canonical persistence rather than discard it or fabricate cancellation.
#[tokio::test]
async fn service_retirement_adopts_completed_inference_and_counts_execution() {
    let (service, agent, turn_id) = prepared_fixture("complete before service retirement").await;
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_configured_agent_provider_task(agent.clone(), turn_id.clone())
        .await
        .unwrap()
        .unwrap();
    let generation = dispatch.claim_generation;
    let conversation = dispatch.turn.conversation_id.clone();
    let drains: ProviderObservationDrains = Default::default();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let owner = ProviderWorkerOwner::inference(agent.clone(), &dispatch);
    let monitor = spawn_owned_agent_provider_worker(
        &mut workers,
        &mut owners,
        Some(owner),
        monitor_agent_provider_future(
            handle.clone(),
            agent.clone(),
            turn_id.clone(),
            "%1".into(),
            generation,
            drains.clone(),
            move |progress, sender, pending| async move {
                let provider = IncompleteProvider {
                    requests: Mutex::new(Vec::new()),
                    failures: 0,
                    stream: false,
                };
                let observer =
                    ProviderWireRequestObserver::new(&dispatch.turn.conversation_id, "%1", sender)
                        .with_claim_generation(generation)
                        .with_pending_observations(pending);
                let execution =
                    run_incomplete_fixture(&provider, &observer, &dispatch, 0, Some(progress))
                        .await?;
                Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(
                    execution,
                )))
            },
        ),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !monitor.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        workers.len(),
        1,
        "the original outcome has not been joined or adopted"
    );
    assert_eq!(
        abort_agent_provider_workers(&mut workers, &handle, &drains, &mut owners)
            .await
            .unwrap(),
        1
    );
    assert!(owners.is_empty());
    assert!(drains.lock().unwrap().is_empty());
    let persistence = crate::host::async_runtime::run_async_persistence_side_effect_service(
        &handle,
        crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
            max_polls: 16,
            drain_limit: 64,
            idle_interval: Duration::from_millis(1),
        },
        |polls, _| polls >= 16,
    )
    .await
    .unwrap();
    assert_eq!(persistence.failed, 0);
    assert!(!handle.agent_turn_is_running(&turn_id).await.unwrap());
    handle.shutdown().await.unwrap();
    let mut exit = actor.await.unwrap();
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert!(
        !exit
            .service
            .agent_provider_claim_matches(&agent, &turn_id, generation)
    );
    let usage = exit
        .service
        .agent_token_usage_for_conversation(&conversation);
    assert_eq!(
        usage[&mez_agent::ModelTokenUsageKey::new("local-chat", "test-model")].input_tokens,
        5
    );
    assert_eq!(
        exit.service.terminate_all_pane_processes().unwrap().len(),
        1
    );
}

/// A known-cancelled inference producer must release its genuine claim without
/// waiting for the original lease to expire. Reported partial expense survives,
/// no replacement request runs, and a late exact lease tick remains inert. All
/// native cleanup precedes assertions so the baseline failure also retains fate.
#[tokio::test]
async fn service_cancellation_retires_inference_without_waiting_for_lease() {
    let (service, agent, turn_id) =
        prepared_fixture("cancel the owned provider service request").await;
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_configured_agent_provider_task(agent.clone(), turn_id.clone())
        .await
        .unwrap()
        .unwrap();
    let generation = dispatch.claim_generation;
    let conversation = dispatch.turn.conversation_id.clone();
    let lease = handle
        .drain_timer_side_effects(1024)
        .await
        .unwrap()
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::ScheduleTimer { key, .. }
                if key.kind == crate::runtime::RuntimeTimerKind::ProviderClaim
                    && key.owner_id == turn_id
                    && key.generation == generation =>
            {
                Some(key)
            }
            _ => None,
        })
        .unwrap();
    let provider = std::sync::Arc::new(IncompleteProvider {
        requests: Mutex::new(Vec::new()),
        failures: usize::MAX,
        stream: true,
    });
    let received = std::sync::Arc::new(tokio::sync::Notify::new());
    let worker_provider = NotifyingProvider {
        inner: provider.clone(),
        received: received.clone(),
    };
    let drains: ProviderObservationDrains = Default::default();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let owner = ProviderWorkerOwner::inference(agent.clone(), &dispatch);
    spawn_owned_agent_provider_worker(
        &mut workers,
        &mut owners,
        Some(owner),
        monitor_agent_provider_future(
            handle.clone(),
            agent.clone(),
            turn_id.clone(),
            "%1".into(),
            generation,
            drains.clone(),
            move |progress, sender, pending| async move {
                let observer =
                    ProviderWireRequestObserver::new(&dispatch.turn.conversation_id, "%1", sender)
                        .with_claim_generation(generation)
                        .with_pending_observations(pending);
                let execution = run_incomplete_fixture(
                    &worker_provider,
                    &observer,
                    &dispatch,
                    100_000,
                    Some(progress),
                )
                .await?;
                Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(
                    execution,
                )))
            },
        ),
    );
    received.notified().await;
    assert_eq!(
        abort_agent_provider_workers(&mut workers, &handle, &drains, &mut owners)
            .await
            .unwrap(),
        0
    );
    let running = handle.agent_turn_is_running(&turn_id).await.unwrap();
    if !running {
        let mut late = RuntimeEventBatch::new();
        late.push(RuntimeEvent::Timer(crate::runtime::TimerEvent {
            key: lease,
            now_ms: 0,
        }));
        assert_eq!(handle.submit_runtime_events(late).await.unwrap().applied, 0);
    }
    assert!(owners.is_empty());
    assert!(drains.lock().unwrap().is_empty());
    handle.shutdown().await.unwrap();
    let mut exit = actor.await.unwrap();
    let state = exit
        .service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .state;
    let claimed = exit
        .service
        .agent_provider_claim_matches(&agent, &turn_id, generation);
    let usage = exit
        .service
        .agent_token_usage_for_conversation(&conversation);
    let roots = exit.service.terminate_all_pane_processes().unwrap().len();
    assert_eq!(roots, 1);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert!(
        !running,
        "destroyed inference request must not retain running capacity until its lease"
    );
    assert!(!claimed);
    assert_eq!(state, AgentTurnState::Failed);
    let totals = &usage[&mez_agent::ModelTokenUsageKey::new("local-chat", "test-model")];
    assert_eq!((totals.input_tokens, totals.output_tokens), (17, 3));
}
