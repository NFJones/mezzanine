//! Mixed native shell/spawn through durable bridge input and real continuation.
//!
//! Only the first authored response is offline. The final configured provider
//! performs an actual loopback HTTP exchange after the original writer ACK;
//! no live provider or child's inference is executed by this fixture.

use super::*;
use crate::host::async_runtime::*;
use crate::integrations::agent::{actions::AgentTurnRunner, provider::AsyncModelProvider};
use mez_agent::{AgentAction, AgentActionPayload, AgentTurnExecution, ModelResponse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct InitialPlan(ModelResponse);
impl AsyncModelProvider for InitialPlan {
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }
    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        mez_agent::ProviderApiCompatibility::OpenAiChatCompletions
    }
    fn send_request_async<'a>(
        &'a self,
        _: &'a mez_agent::ModelRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = crate::Result<ModelResponse>> + Send + 'a>,
    > {
        Box::pin(async { Ok(self.0.clone()) })
    }
}

async fn run<P: AsyncModelProvider>(
    dispatch: &crate::runtime::RuntimeAgentProviderDispatch,
    provider: &P,
) -> AgentTurnExecution {
    let permissions = crate::security::permissions::ProductPermissionPlanning::new(
        &dispatch.permission_policy,
        &dispatch.session_approvals,
        dispatch.path_scopes.as_ref(),
    )
    .with_shell_classification(&dispatch.shell_classification)
    .with_sandbox_first_local_prompts(dispatch.sandbox_first_local_prompts)
    .with_macro_bridge_recipients(dispatch.macro_bridge_recipients.clone());
    AgentTurnRunner {
        provider,
        model_profile: dispatch.model_profile.clone(),
        permissions: &permissions,
        subagent_scope: dispatch.subagent_scope.as_ref(),
        subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: dispatch.available_mcp_servers.clone(),
        available_mcp_tools: &dispatch.available_mcp_tools,
        memory_actions_enabled: dispatch.memory_actions_enabled,
        issue_actions_enabled: dispatch.issue_actions_enabled,
    }
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
    .unwrap()
}

async fn writer(handle: &AsyncRuntimeSessionHandle) {
    let report = run_async_persistence_side_effect_service(
        handle,
        AsyncRuntimeSideEffectServiceConfig {
            max_polls: 100,
            drain_limit: 32,
            idle_interval: Duration::from_millis(1),
        },
        |_, _| false,
    )
    .await
    .unwrap();
    assert_eq!(report.failed, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_native_bridge_mixed_spawn_finishes_one_configured_continuation() {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    let fixture = Fixture::new(true);
    let (handle, mut actor, root, turn, _, _) =
        super::native_actions::actor_fixture_with_transcript(
            &fixture,
            mez_agent::ApprovalPolicy::AutoAllow,
            true,
        )
        .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    actor.service.replace_config_layers(vec![ConfigLayer {
        name: "durable-bridge-continuation".into(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\nrouting=false\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"native\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"{endpoint}\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[providers.runtime-batch.options]\nmaap_output=\"structured_json\"\nstructured_output=\"json_schema\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=128000\n[permissions]\napproval_policy=\"auto-allow\"\nsandbox=\"policy-only\"\n"),
    }]).unwrap();
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
    let agent = mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap();
    let first = actor
        .service
        .claim_configured_agent_provider_task(&agent, &turn.turn_id)
        .unwrap()
        .unwrap();
    let provider = InitialPlan(ModelResponse {
        provider: "runtime-batch".into(),
        model: "test".into(),
        raw_text: "run original shell and spawn reviewer".into(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        provider_transcript_events: vec![],
        action_batch: Some(mez_agent::MaapBatch {
            rationale: "keep the original native sibling owner".into(),
            actions: vec![
                AgentAction {
                    id: "durable-shell".into(),
                    payload: AgentActionPayload::ShellCommand {
                        summary: "Execute once".into(),
                        command: format!(
                            "printf 'durable-shell-once\\n' >> {}",
                            shlex::try_quote(fixture.path.join("effect").to_str().unwrap())
                                .unwrap()
                        ),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                },
                AgentAction {
                    id: "durable-child".into(),
                    payload: AgentActionPayload::SpawnAgent {
                        role: "default".into(),
                        placement: "new-window".into(),
                        cooperation_mode: "explore-only".into(),
                        read_scopes: None,
                        write_scopes: None,
                        session_mode: None,
                        size: None,
                        reasoning_effort: None,
                        lifetime: mez_agent::SubagentLifetime::Persistent,
                        objective: Some("Review assigned work".into()),
                        task_prompt: "Wait for a review assignment".into(),
                    },
                },
            ],
        }),
    });
    let execution = run(&first, &provider).await;
    let applied = actor
        .service
        .apply_agent_provider_execution_async_for_tests(&turn, &first.model_profile, execution)
        .await
        .unwrap();
    assert_eq!(
        applied.action_results[0].status,
        mez_agent::ActionStatus::Running
    );
    assert!(!actor.service.agent_provider_task_is_pending(&turn.turn_id));
    let mut references = 0;
    for _ in 0..32 {
        actor.poll_message_custody_for_tests();
        actor.observe_message_custody_for_tests().await.unwrap();
        references = actor.service.agent_turn_contexts()[&turn.turn_id]
            .blocks()
            .iter()
            .filter(|block| {
                block.source == mez_agent::ContextSourceKind::PeerMessage
                    && block.content.contains("subagent task started")
            })
            .count();
        if references == 1
            && !actor
                .service
                .native_input_pending_for_conversation(&turn.conversation_id)
        {
            break;
        }
    }
    assert_eq!(references, 1);
    assert!(!actor.service.agent_provider_task_is_pending(&turn.turn_id));
    let server = tokio::spawn(async move {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            header.push(byte[0]);
            assert!(header.len() < 65536);
        }
        let length: usize = String::from_utf8(header)
            .unwrap()
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse().unwrap())
            })
            .unwrap();
        assert!(length < 1024 * 1024);
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let content = serde_json::json!({"rationale":"consume the original successful result","actions":[{
            "type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"durable workflow complete"
        }]}).to_string();
        let response = serde_json::json!({"id":"durable-final","model":"test","choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}]}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        request
    });
    let client = async {
        assert!(
            handle
                .claim_configured_agent_provider_task(agent.clone(), turn.turn_id.clone())
                .await
                .unwrap()
                .is_none()
        );
        let dispatch = handle
            .claim_native_shell_action(turn.turn_id.clone(), "durable-shell".into())
            .await
            .unwrap()
            .unwrap();
        let outcome = tokio::task::spawn_blocking(move || {
            crate::runtime::execute_native_shell_dispatch(dispatch)
        })
        .await
        .unwrap();
        let mut events = crate::runtime::RuntimeEventBatch::new();
        events.push(crate::runtime::RuntimeEvent::NativeShell(outcome.clone()));
        assert_eq!(
            handle.submit_runtime_events(events).await.unwrap().applied,
            1
        );
        let mut events = crate::runtime::RuntimeEventBatch::new();
        events.push(crate::runtime::RuntimeEvent::NativeShell(outcome));
        assert_eq!(
            handle.submit_runtime_events(events).await.unwrap().applied,
            0
        );
        writer(&handle).await;
        let final_dispatch = handle
            .claim_configured_agent_provider_task(agent.clone(), turn.turn_id.clone())
            .await
            .unwrap()
            .expect("actual post-ACK continuation");
        let result_blocks = final_dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .filter(|block| {
                block.source == mez_agent::ContextSourceKind::ActionResult
                    && block.content.contains("durable-shell")
                    && block.content.contains("succeeded")
            })
            .count();
        assert_eq!(result_blocks, 1);
        let crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) =
            &final_dispatch.provider
        else {
            panic!("configured loopback provider missing")
        };
        let completed = run(&final_dispatch, provider).await;
        assert_eq!(
            completed.terminal_state,
            mez_agent::AgentTurnState::Completed
        );
        let mut events = crate::runtime::RuntimeEventBatch::new();
        events.push(crate::runtime::RuntimeEvent::AgentProvider(
            crate::runtime::AgentProviderEvent::Completed {
                agent_id: agent.clone(),
                turn_id: turn.turn_id.clone(),
                claim_generation: final_dispatch.claim_generation,
                execution: Box::new(completed),
            },
        ));
        assert_eq!(
            handle.submit_runtime_events(events).await.unwrap().applied,
            1
        );
        writer(&handle).await;
        assert!(
            handle
                .claim_configured_agent_provider_task(agent, turn.turn_id.clone())
                .await
                .unwrap()
                .is_none()
        );
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    let request = server.await.unwrap();
    let state = exit
        .service
        .agent_turn_ledger()
        .turn(&turn.turn_id)
        .unwrap()
        .state;
    let children = exit.service.agent_turn_ledger().turns().len() - 1;
    let effect = std::fs::read_to_string(fixture.path.join("effect")).unwrap();
    exit.service.terminate_all_pane_processes().unwrap();
    drop((exit, handle));
    wait_until_unlocked(&fixture.path).await;
    std::fs::remove_dir_all(&fixture.path).unwrap();
    assert!(!fixture.path.exists());
    assert_eq!(state, mez_agent::AgentTurnState::Completed);
    assert_eq!(children, 1);
    assert_eq!(effect, "durable-shell-once\n");
    assert!(request["messages"].to_string().contains("durable-shell"));
}
