//! Normal provider-produced native actions on an internally registered disk root.
//!
//! Actual actor claim/preparation and lower policy planning precede completion
//! ingress. A real SQLite gate separates pending permission/source ownership from
//! accepted action results; no provider network, fake success or claim setter is
//! used. Full product root activation and restart folding remain separate work.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorExit, AsyncRuntimeSessionHandle};
use crate::integrations::agent::{actions::AgentTurnRunner, provider::AsyncModelProvider};
use crate::runtime::{AgentProviderEvent, RuntimeEvent, RuntimeEventBatch};
use mez_agent::{
    ActionStatus, AgentAction, AgentActionPayload, AgentTurnExecution, AgentTurnRecord,
    ModelResponse,
};

/// One offline response through the actual provider runner and captured policy.
struct OfflineMessage {
    response: ModelResponse,
    calls: AtomicUsize,
}
impl AsyncModelProvider for OfflineMessage {
    /// Matches the genuinely configured fixture provider.
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }
    /// Uses the configured request grammar without contacting a provider.
    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        mez_agent::ProviderApiCompatibility::OpenAiChatCompletions
    }
    /// Returns one authored response and counts every actual inference attempt.
    fn send_request_async<'a>(
        &'a self,
        _: &'a mez_agent::ModelRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = crate::Result<ModelResponse>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.response.clone())
        })
    }
}

/// Creates the actual live native pane before prompt/claim ownership is captured.
/// The task is selected from real pending-provider work, never a synthetic ledger
/// status. Normal teardown explicitly retires this fixture's actual process.
async fn actor_fixture(
    fixture: &Fixture,
    policy: mez_agent::ApprovalPolicy,
) -> (
    AsyncRuntimeSessionHandle,
    crate::host::async_runtime::AsyncRuntimeSessionActor,
    MmpCustodyRoot,
    AgentTurnRecord,
    SenderIdentity,
    mez_core::ids::ClientId,
) {
    actor_fixture_with_transcript(fixture, policy, false).await
}

/// Installs optional transcript ownership before creating the actual turn, as
/// the product does, so provider completion sees the original archive binding.
async fn actor_fixture_with_transcript(
    fixture: &Fixture,
    policy: mez_agent::ApprovalPolicy,
    transcript: bool,
) -> (
    AsyncRuntimeSessionHandle,
    crate::host::async_runtime::AsyncRuntimeSessionActor,
    MmpCustodyRoot,
    AgentTurnRecord,
    SenderIdentity,
    mez_core::ids::ClientId,
) {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    if transcript {
        service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
            fixture.path.join("transcripts"),
        ));
    }
    let config_root = fixture.path.join("config");
    service.set_config_root(config_root.clone());
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&config_root),
    ));
    service.replace_config_layers(vec![ConfigLayer{name:"native-action-storage".into(),path:None,format:ConfigFormat::Toml,scope:ConfigScope::Primary,trusted:true,
        text:"[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"native\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=128000\n[permissions]\napproval_policy=\"ask\"\nsandbox=\"policy-only\"\n".into()}]).unwrap();
    let primary = service
        .attach_primary(
            "native-action",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service.set_pane_readiness("%1", mez_agent::PaneReadinessState::Ready);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "/shell-mode native --global")
        .unwrap();
    service.set_pane_approval_policy_override("%1", Some(policy));
    service
        .execute_agent_shell_command(&primary, "send one original stored message")
        .unwrap();
    let task = service
        .pending_agent_provider_tasks()
        .into_iter()
        .next()
        .expect("actual pending provider task");
    let turn = service
        .agent_turn_ledger()
        .turn(&task.turn_id)
        .unwrap()
        .clone();
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", vec![]);
    service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap();
    let (handle, mut actor) = crate::host::async_runtime::AsyncRuntimeSessionActor::new(
        service,
        crate::host::async_runtime::AsyncRuntimeActorConfig::default(),
    )
    .unwrap();
    actor.register_message_custody(root.clone()).unwrap();
    (handle, actor, root, turn, target, primary)
}

/// Plans the original send using an actual actor-issued dispatch and policy.
async fn plan(
    dispatch: crate::runtime::RuntimeAgentProviderDispatch,
    target: &SenderIdentity,
    tracked: bool,
) -> (
    OfflineMessage,
    crate::runtime::RuntimeAgentProviderDispatch,
    AgentTurnExecution,
) {
    let provider = OfflineMessage {
        response: ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "one original stored send".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "one stored native action".into(),
                actions: vec![AgentAction {
                    id: "stored-action".into(),
                    payload: AgentActionPayload::SendMessage {
                        recipient: format!("agent:{}", target.agent_id),
                        scope: Some("session".into()),
                        content_type: "text/plain; charset=utf-8".into(),
                        payload: "original authorized action body".into(),
                        correlation_id: None,
                        expect_reply: tracked,
                    },
                }],
            }),
            provider_transcript_events: vec![],
        },
        calls: AtomicUsize::new(0),
    };
    let permissions = crate::security::permissions::ProductPermissionPlanning::new(
        &dispatch.permission_policy,
        &dispatch.session_approvals,
        dispatch.path_scopes.as_ref(),
    )
    .with_shell_classification(&dispatch.shell_classification)
    .with_sandbox_first_local_prompts(dispatch.sandbox_first_local_prompts)
    .with_macro_bridge_recipients(dispatch.macro_bridge_recipients.clone());
    let runner = AgentTurnRunner {
        provider: &provider,
        model_profile: dispatch.model_profile.clone(),
        permissions: &permissions,
        subagent_scope: dispatch.subagent_scope.as_ref(),
        subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: dispatch.available_mcp_servers.clone(),
        available_mcp_tools: &dispatch.available_mcp_tools,
        memory_actions_enabled: dispatch.memory_actions_enabled,
        issue_actions_enabled: dispatch.issue_actions_enabled,
    };
    let execution = runner
        .run_turn_async_ref_with_previous_request_and_progress(
            &mut mez_agent::AgentTurnLedger::new(false),
            dispatch.turn.clone(),
            dispatch.context.durable(),
            dispatch.allowed_actions.clone(),
            dispatch.interaction_kind,
            dispatch.context.previous_request(),
            None,
            dispatch.turn_limits,
        )
        .await
        .unwrap();
    (provider, dispatch, execution)
}

/// Delivers the exact admitted provider generation through normal event ingress.
async fn complete(
    handle: &AsyncRuntimeSessionHandle,
    dispatch: &crate::runtime::RuntimeAgentProviderDispatch,
    execution: AgentTurnExecution,
) {
    let mut events = RuntimeEventBatch::new();
    events.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
        agent_id: mez_core::ids::AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap(),
        turn_id: dispatch.turn.turn_id.clone(),
        claim_generation: dispatch.claim_generation,
        execution: Box::new(execution),
    }));
    assert_eq!(
        handle.submit_runtime_events(events).await.unwrap().applied,
        1
    );
}

/// Waits for actual lower publication through actor requests; no synthetic label
/// or status setter qualifies the action's accepted result.
async fn await_delivery(handle: &AsyncRuntimeSessionHandle, target: &SenderIdentity) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if handle
                .message_fanout_ready_for(target.agent_id.clone(), u64::MAX, 10)
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// Checks actual original result/context after normal completion and shutdown.
fn accepted(exited: &AsyncRuntimeActorExit, turn: &AgentTurnRecord, tracked: bool) {
    let execution = exited
        .service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .expect("original execution retained until next claim");
    let result = execution
        .action_results
        .iter()
        .find(|r| r.action_type == "send_message")
        .unwrap();
    assert_eq!(result.status, ActionStatus::Succeeded);
    let value: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(value["delivery_status"], "accepted");
    assert_eq!(value["expect_reply"], tracked);
    assert_eq!(
        exited.service.tracked_message_request_usage().0.0,
        usize::from(tracked)
    );
    assert!(
        exited
            .service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(|block| block.content.contains("delivery_status"))
    );
}

/// Actual tracked and untracked native actions must park their provider capacity
/// while disk is gated and retain their original result as pending. After real
/// commit, the same source publishes success exactly once with no extra inference.
async fn action_control(tracked: bool) {
    let fixture = Fixture::new(false);
    let (handle, actor, _, turn, target, _) =
        actor_fixture(&fixture, mez_agent::ApprovalPolicy::AutoAllow).await;
    let runner = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_configured_agent_provider_task(
            mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn.turn_id.clone(),
        )
        .await
        .unwrap();
    let dispatch = match dispatch {
        Some(dispatch) => dispatch,
        None => {
            let shutdown = handle.shutdown().await;
            let mut exited = runner.await.unwrap();
            let state = exited
                .service
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .cloned();
            let screen = exited
                .service
                .agent_pane_screen(&turn.pane_id)
                .map(|screen| screen.normal_content_lines());
            exited.service.terminate_all_pane_processes().unwrap();
            panic!(
                "actual provider claim refused: state={state:?}, shutdown={shutdown:?}, screen={screen:?}"
            );
        }
    };
    let (provider, dispatch, execution) = plan(dispatch, &target, tracked).await;
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    complete(&handle, &dispatch, execution).await;
    if handle
        .message_fanout_ready_for(target.agent_id.clone(), u64::MAX, 10)
        .await
        .unwrap()
        .is_some()
    {
        let shutdown = handle.shutdown().await;
        let mut exited = runner.await.unwrap();
        exited.service.terminate_all_pane_processes().unwrap();
        panic!("native action published before its actual storage write: {shutdown:?}");
    }
    if tokio::time::timeout(Duration::from_secs(10), fixture.probe.started.notified())
        .await
        .is_err()
    {
        let shutdown = handle.shutdown().await;
        let mut exited = runner.await.unwrap();
        let results = exited
            .service
            .retained_completion_execution_for_tests(&turn.turn_id)
            .map(|execution| {
                execution
                    .action_results
                    .iter()
                    .map(|result| (result.action_type, result.status))
                    .collect::<Vec<_>>()
            });
        let screen = exited
            .service
            .agent_pane_screen(&turn.pane_id)
            .map(|screen| screen.normal_content_lines());
        exited.service.terminate_all_pane_processes().unwrap();
        panic!(
            "native storage did not issue: results={results:?} shutdown={shutdown:?} screen={screen:?}"
        );
    }
    assert_eq!(fixture.disk(), (0, 0));
    assert!(
        handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .iter()
            .all(|task| task.turn_id != turn.turn_id)
    );
    assert!(
        handle
            .message_fanout_ready_for(target.agent_id.clone(), u64::MAX, 10)
            .await
            .unwrap()
            .is_none()
    );
    assert!(handle.lifecycle_state().await.is_ok());
    fixture.probe.release();
    await_delivery(&handle, &target).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("original parked source must fairly reacquire provider capacity after acceptance");
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    accepted(&exited, &turn, tracked);
    exited.service.terminate_all_pane_processes().unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
}

/// Tracked normal policy-planned sends install their exact request receipt before
/// the action's accepted result. No wait authority exists while storage is gated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracked_native_action_waits_for_storage_then_publishes_original_result() {
    action_control(true).await;
}

/// Untracked normal policy-planned sends use the same actor-owned durable lane
/// without inventing a request receipt or replaying the original action batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untracked_native_action_waits_for_storage_then_publishes_original_result() {
    action_control(false).await;
}

/// Issues a genuine raw holder so a native source must wait before reservation.
/// Its watcher stays live independently of the spawned original reply observer.
async fn hold_root(
    handle: &AsyncRuntimeSessionHandle,
    target: &SenderIdentity,
) -> tokio::task::JoinHandle<crate::Result<crate::host::async_runtime::AsyncMessageInputResult>> {
    let (connection, owner, lease) = super::raw_actor::hello(handle).await;
    let input = crate::protocol::message::encode_mmp_body(
        &serde_json::json!({
        "protocol":"mmp/1","type":"send","id":"capacity-holder","time":"original-holder",
        "scope":"session","sender":{"agent_id":connection.agent_id.as_ref().unwrap().as_str()},
        "recipient":{"agent_id":target.agent_id.as_str()},"ttl_ms":null,"correlation_id":null,
        "content_type":"text/plain; charset=utf-8","payload":"capacity holder"})
        .to_string(),
    );
    let sender = handle.clone();
    tokio::spawn(async move {
        let _lease = lease;
        sender
            .handle_message_input_with_owner(input, 4096, connection, 1, Some(owner))
            .await
    })
}

/// Waits for this source rather than mistaking the older holder for completion.
async fn await_source_delivery(handle: &AsyncRuntimeSessionHandle, target: &SenderIdentity) {
    assert!(source_delivery_arrived(handle, target).await);
}

/// Observes an original source without consuming it, allowing fixture cleanup
/// and bounded diagnostic collection if the expected delivery never arrives.
async fn source_delivery_arrived(
    handle: &AsyncRuntimeSessionHandle,
    target: &SenderIdentity,
) -> bool {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(batch) = handle
                .message_fanout_ready_for(target.agent_id.clone(), u64::MAX, 10)
                .await
                .unwrap()
                && batch
                    .batch
                    .batch
                    .messages
                    .iter()
                    .any(|m| m.envelope.payload == "original authorized action body")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}

/// Executes actual client-affine control against this fixture's attached client.
/// Decision and idempotency receipts come from the real control dispatcher.
async fn control(
    handle: &AsyncRuntimeSessionHandle,
    primary: &mez_core::ids::ClientId,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let input = crate::control::encode_control_body(
        &serde_json::json!({"jsonrpc":"2.0","id":method,"method":method,"params":params})
            .to_string(),
    );
    let response = handle
        .handle_control_input_for_connection(
            input,
            8192,
            crate::control::ControlConnectionState::trusted_existing_client(primary.clone()),
        )
        .await
        .unwrap();
    let body = crate::control::decode_control_frame(&response.output, 8192)
        .unwrap()
        .0;
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(value.get("error").is_none(), "{value}");
    value
}

/// A source waiting behind a real write retains semantics, not an already built
/// stale envelope. Selection samples its clock and current telemetry only after
/// capacity returns, while the original authorized body is delivered once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_action_waiting_for_capacity_refreshes_only_selected_envelope_metadata() {
    let fixture = Fixture::new(false);
    let (handle, actor, _, turn, target, primary) =
        actor_fixture_with_transcript(&fixture, mez_agent::ApprovalPolicy::AutoAllow, true).await;
    let runner = tokio::spawn(actor.run());
    let holder = hold_root(&handle, &target).await;
    tokio::time::timeout(Duration::from_secs(10), fixture.probe.started.notified())
        .await
        .unwrap();
    let dispatch = handle
        .claim_configured_agent_provider_task(
            mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn.turn_id.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let (provider, dispatch, execution) = plan(dispatch, &target, false).await;
    complete(&handle, &dispatch, execution).await;
    for effect in handle.drain_persistence_side_effects(64).await.unwrap() {
        if let crate::runtime::RuntimeSideEffect::SettleAgentProviderPersistence { work } = effect {
            let outcome = tokio::task::spawn_blocking(move || {
                crate::runtime::execute_agent_provider_persistence_work(*work)
            })
            .await
            .unwrap()
            .unwrap();
            let mut events = RuntimeEventBatch::new();
            events.push(RuntimeEvent::AgentProvider(
                AgentProviderEvent::PersistenceSettled {
                    outcome: Box::new(outcome),
                },
            ));
            handle.submit_runtime_events(events).await.unwrap();
        }
    }
    let objective = handle
        .execute_agent_shell_command(
            primary,
            "/objective Changed while waiting for capacity".into(),
        )
        .await
        .unwrap();
    assert!(objective.contains("source=user"), "{objective}");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let selected_after = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    fixture.probe.release();
    holder.await.unwrap().unwrap();
    if !source_delivery_arrived(&handle, &target).await {
        let shutdown = handle.shutdown().await;
        let mut exited = runner.await.unwrap();
        let results = exited
            .service
            .retained_completion_execution_for_tests(&turn.turn_id)
            .map(|execution| {
                execution
                    .action_results
                    .iter()
                    .map(|result| {
                        (
                            result.status,
                            result
                                .error
                                .as_ref()
                                .map(|error| (&error.code, &error.message)),
                        )
                    })
                    .collect::<Vec<_>>()
            });
        let diagnostic = format!(
            "source not delivered: attempts={} shutdown={shutdown:?} results={results:?} turn={:?} screen={:?}",
            fixture.probe.attempts.load(Ordering::SeqCst),
            exited.service.agent_turn_ledger().turn(&turn.turn_id),
            exited
                .service
                .agent_pane_screen(&turn.pane_id)
                .map(|screen| screen.normal_content_lines())
        );
        exited.service.terminate_all_pane_processes().unwrap();
        panic!("{diagnostic}");
    }
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    accepted(&exited, &turn, false);
    let mail = exited
        .service
        .message_service()
        .receive_subscribed(&target.agent_id, u64::MAX, 10)
        .unwrap();
    let source = mail
        .messages
        .iter()
        .find(|m| m.envelope.payload == "original authorized action body")
        .unwrap();
    assert!(
        source
            .envelope
            .time
            .strip_prefix("runtime:")
            .unwrap()
            .parse::<u64>()
            .unwrap()
            >= selected_after
    );
    assert_eq!(
        source.envelope.sender.objective,
        exited
            .service
            .message_service()
            .registered_identity(&source.envelope.sender.agent_id)
            .unwrap()
            .objective
    );
    assert_eq!(
        source.envelope.sender.objective.as_deref(),
        Some("Changed while waiting for capacity")
    );
    assert_eq!(
        mail.messages
            .iter()
            .filter(|m| m.envelope.payload == "original authorized action body")
            .count(),
        1
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    exited.service.terminate_all_pane_processes().unwrap();
}

/// Exact decided approval is captured while storage remains occupied and no
/// provider permit is required to retain it. Ask policy stays enabled; resuming
/// the same decision cannot redispatch approval or create a second source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_approved_action_retains_original_grant_before_storage_capacity() {
    let fixture = Fixture::new(false);
    let (handle, actor, _, turn, target, primary) =
        actor_fixture(&fixture, mez_agent::ApprovalPolicy::Ask).await;
    let runner = tokio::spawn(actor.run());
    let holder = hold_root(&handle, &target).await;
    tokio::time::timeout(Duration::from_secs(10), fixture.probe.started.notified())
        .await
        .unwrap();
    let dispatch = handle
        .claim_configured_agent_provider_task(
            mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn.turn_id.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let (provider, dispatch, execution) = plan(dispatch, &target, true).await;
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    complete(&handle, &dispatch, execution).await;
    let list = control(
        &handle,
        &primary,
        "approval/list",
        serde_json::json!({"state":"pending"}),
    )
    .await;
    let approval = list["result"]["approvals"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let decision = serde_json::json!({"approval_id":approval,"decision":"approve","idempotency_key":"stored-source-original-approval"});
    control(&handle, &primary, "approval/decide", decision.clone()).await;
    control(&handle, &primary, "approval/decide", decision).await;
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.disk(), (0, 0));
    let list = control(
        &handle,
        &primary,
        "approval/list",
        serde_json::json!({"state":"pending"}),
    )
    .await;
    assert!(list["result"]["approvals"].as_array().unwrap().is_empty());
    assert!(
        handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .is_empty()
    );
    fixture.probe.release();
    holder.await.unwrap().unwrap();
    await_source_delivery(&handle, &target).await;
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    accepted(&exited, &turn, true);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    exited.service.terminate_all_pane_processes().unwrap();
}

/// Actual commit followed by worker-result loss must reconcile the same record
/// and publish one original action result without another inference or envelope.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_action_reconciles_actual_worker_loss_without_replaying_source() {
    let fixture = Fixture::new(true);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    let (handle, actor, root, turn, target, _) =
        actor_fixture(&fixture, mez_agent::ApprovalPolicy::AutoAllow).await;
    let runner = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_configured_agent_provider_task(
            mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn.turn_id.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let (provider, dispatch, execution) = plan(dispatch, &target, true).await;
    complete(&handle, &dispatch, execution).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                root.observe().await.unwrap(),
                CustodyWorkerStatus::RetryAt(_)
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let committed = fixture.disk();
    await_source_delivery(&handle, &target).await;
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    accepted(&exited, &turn, true);
    assert_eq!(fixture.disk(), committed);
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    exited.service.terminate_all_pane_processes().unwrap();
}

/// Physical final-marker failure remains ambiguous until the root's actual
/// shutdown reconciliation qualifies noncommit and complete abort. Only then may
/// native/lower charges release and the original result become rejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_action_qualified_refusal_preserves_original_rejected_result() {
    let fixture = Fixture::new(true);
    let (handle, actor, root, turn, target, _) =
        actor_fixture(&fixture, mez_agent::ApprovalPolicy::AutoAllow).await;
    rusqlite::Connection::open(fixture.path.join("custody.sqlite")).unwrap().execute_batch(
        "CREATE TRIGGER fail_action_marker BEFORE INSERT ON records WHEN NEW.key LIKE 'a1:%:commit' BEGIN SELECT RAISE(ABORT,'action marker failure'); END;").unwrap();
    let runner = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_configured_agent_provider_task(
            mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn.turn_id.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let (provider, dispatch, execution) = plan(dispatch, &target, true).await;
    complete(&handle, &dispatch, execution).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                root.observe().await.unwrap(),
                CustodyWorkerStatus::RetryAt(_)
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    let execution = exited
        .service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Failed);
    assert_eq!(
        exited.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    assert!(
        !exited
            .service
            .message_service()
            .has_unpublished_acceptance()
    );
    assert!(
        exited
            .service
            .message_service()
            .receive_subscribed(&target.agent_id, u64::MAX, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(fixture.disk().1, 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    exited.service.terminate_all_pane_processes().unwrap();
}

/// A real approved send remains accepted when its later original-result
/// publication fails. Shutdown reports the original error and never retries the
/// committed write or dispatches the provider a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approved_native_action_retains_commit_after_late_publication_failure() {
    let fixture = Fixture::new(false);
    let (handle, mut actor, _, turn, target, primary) =
        actor_fixture(&fixture, mez_agent::ApprovalPolicy::Ask).await;
    actor
        .service
        .fail_next_approved_message_late_publication_for_tests();
    let runner = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_configured_agent_provider_task(
            mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn.turn_id.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let (provider, dispatch, execution) = plan(dispatch, &target, true).await;
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    complete(&handle, &dispatch, execution).await;
    let list = control(
        &handle,
        &primary,
        "approval/list",
        serde_json::json!({"state":"pending"}),
    )
    .await;
    let approval = list["result"]["approvals"][0]["id"].as_str().unwrap();
    control(
        &handle,
        &primary,
        "approval/decide",
        serde_json::json!({
            "approval_id":approval,"decision":"approve","idempotency_key":"late-stored-source"
        }),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), fixture.probe.started.notified())
        .await
        .unwrap();
    assert_eq!(fixture.disk(), (0, 0));
    fixture.probe.release();
    await_source_delivery(&handle, &target).await;
    let error = handle.shutdown().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected approved message late publication failure")
    );
    let mut exited = runner.await.unwrap();
    let execution = exited
        .service
        .retained_completion_execution_for_tests(&turn.turn_id)
        .unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("accepted")
    );
    let mail = exited
        .service
        .message_service()
        .receive_subscribed(&target.agent_id, u64::MAX, 10)
        .unwrap();
    assert_eq!(
        mail.messages
            .iter()
            .filter(|m| m.envelope.payload == "original authorized action body")
            .count(),
        1
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    exited.service.terminate_all_pane_processes().unwrap();
}

/// Stop cancels a source still queued behind an unrelated real write. Starting
/// a new conversation cannot inherit that grant or issue its message later.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopped_queued_native_action_does_not_follow_new_conversation() {
    let fixture = Fixture::new(false);
    let (handle, actor, _, turn, target, primary) =
        actor_fixture(&fixture, mez_agent::ApprovalPolicy::AutoAllow).await;
    let runner = tokio::spawn(actor.run());
    let holder = hold_root(&handle, &target).await;
    tokio::time::timeout(Duration::from_secs(10), fixture.probe.started.notified())
        .await
        .unwrap();
    let dispatch = handle
        .claim_configured_agent_provider_task(
            mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn.turn_id.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let (provider, dispatch, execution) = plan(dispatch, &target, true).await;
    complete(&handle, &dispatch, execution).await;
    handle
        .execute_agent_shell_command(primary.clone(), "/stop".into())
        .await
        .unwrap();
    handle
        .execute_agent_shell_command(primary, "/new".into())
        .await
        .unwrap();
    fixture.probe.release();
    holder.await.unwrap().unwrap();
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    let mail = exited
        .service
        .message_service()
        .receive_subscribed(&target.agent_id, u64::MAX, 10)
        .unwrap();
    assert!(
        mail.messages
            .iter()
            .all(|m| m.envelope.payload != "original authorized action body")
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        exited.service.tracked_message_request_usage(),
        ((0, 0, 0), 0)
    );
    exited.service.terminate_all_pane_processes().unwrap();
}

/// Stop cannot declare an already-issued physical write absent. Its original
/// failed backing retains the late committed result, while the actor joins the
/// same worker and never repeats the original send or provider call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopped_issued_native_action_retains_actual_late_commit() {
    let fixture = Fixture::new(false);
    let (handle, actor, _, turn, target, primary) =
        actor_fixture(&fixture, mez_agent::ApprovalPolicy::AutoAllow).await;
    let runner = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_configured_agent_provider_task(
            mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn.turn_id.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let (provider, dispatch, execution) = plan(dispatch, &target, true).await;
    complete(&handle, &dispatch, execution).await;
    tokio::time::timeout(Duration::from_secs(10), fixture.probe.started.notified())
        .await
        .unwrap();
    let stop = handle
        .execute_agent_shell_command(primary, "/stop".into())
        .await
        .unwrap();
    assert!(stop.contains("unprojected phase evidence"));
    assert_eq!(fixture.disk(), (0, 0));
    fixture.probe.release();
    await_source_delivery(&handle, &target).await;
    let shutdown = handle.shutdown().await;
    let mut exited = runner.await.unwrap();
    let execution = exited
        .service
        .retained_completion_execution_for_tests(&turn.turn_id)
        .unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains("accepted")
    );
    assert!(
        shutdown
            .unwrap_err()
            .to_string()
            .contains("unprojected phase evidence")
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    exited.service.terminate_all_pane_processes().unwrap();
}
