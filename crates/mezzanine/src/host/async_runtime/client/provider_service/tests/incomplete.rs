//! Offline exact-inference replacement and issued-expense qualification.
//!
//! Synthetic unary/SSE errors cross the real product runner and observation
//! adapters, but no provider, credential, vendor, or user transcript is used.
//! Actor tests preserve claim fencing, cancellation expense and final dedup.

use super::*;
use crate::runtime::RuntimeSessionService;
use std::sync::Mutex;

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

/// Creates an auth-free configured native dispatch; the endpoint is never used.
async fn issued_fixture() -> (RuntimeSessionService, RuntimeAgentProviderDispatch) {
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
    service
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
    let turn = service
        .start_agent_prompt_turn("%1", "replace incomplete inference")
        .unwrap();
    let preparation = service.prepare_agent_provider_work(&turn.turn_id).unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
    let mut dispatch = service
        .claim_configured_agent_provider_task(&agent, &turn.turn_id)
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
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0], requests[1]);
        assert_eq!(requests[1], requests[2]);
        drop(requests);
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
            assert!(
                !handle
                    .agent_turn_is_running(&dispatch.turn.turn_id)
                    .await
                    .unwrap()
            );
            handle.shutdown().await.unwrap();
        };
        let ((), exit) = tokio::join!(client, actor.run());
        let service = exit.unwrap().service;
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
    let service = exit.unwrap().service;
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
    let exit = exit.unwrap();
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
        (dispatch.turn.turn_id.clone(), 7),
        ProviderObservationDrain {
            abort: abort.clone(),
            completed,
            pending: pending.clone(),
        },
    );
    let mut workers = JoinSet::new();
    workers.spawn(async move {
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
        abort_agent_provider_workers(&mut workers, &handle, &drains)
            .await
            .unwrap();
        let _ = producer.await;
        assert!(pending.lock().unwrap().is_empty());
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    let exit = exit.unwrap();
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
                crate::runtime::current_unix_seconds(),
                &[1],
                &crate::storage::token_usage::TokenHistoryScope::default(),
            )
            .unwrap();
        assert_eq!(history.windows[&1].values().next().unwrap().usage, expected);
        std::fs::remove_dir_all(root).unwrap();
    }
}
