//! Actual worker-event classification, actor refusal and persistent socket.
//! Only inference is synthetic; request assembly, claims, event settlement,
//! control framing and the healthy sibling's production runner remain real.

use super::*;
use crate::control::{ControlConnectionState, decode_control_frame, encode_control_body};
use crate::host::async_runtime::{
    AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
    serve_async_runtime_control_connection_loop,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

struct FinalProvider;

impl AsyncModelProvider for FinalProvider {
    fn provider_id(&self) -> &str {
        "local-chat"
    }
    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        mez_agent::ProviderApiCompatibility::OpenAiResponses
    }
    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            Ok(ModelResponse {
                provider: request.provider.clone(),
                model: request.model.clone(),
                raw_text: "healthy sibling completed".into(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                provider_transcript_events: Vec::new(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "complete healthy sibling".into(),
                    actions: vec![mez_agent::AgentAction {
                        id: "finish".into(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: "healthy sibling completed".into(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                        },
                    }],
                }),
            })
        })
    }
}

/// Read exactly one complete frame; a partial response or EOF is never success.
async fn round_trip(stream: &mut UnixStream, request: &str) -> serde_json::Value {
    stream
        .write_all(&encode_control_body(request))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(
                read > 0,
                "control connection closed before complete response"
            );
            bytes.extend_from_slice(&chunk[..read]);
            assert!(bytes.len() <= 65536, "bounded control response");
            if let Ok((body, consumed)) = decode_control_frame(&bytes, 65536) {
                assert_eq!(consumed, bytes.len());
                return serde_json::from_str(&body).unwrap();
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn local_request_failure_keeps_control_socket_and_sibling_alive() {
    let (mut service, agent, turn) =
        super::incomplete::prepared_fixture("retain failed original task").await;
    let original = service.agent_turn_contexts()[&turn].clone();
    let primary = service
        .session()
        .clients()
        .iter()
        .find(|client| service.session().is_attached_primary(&client.id))
        .unwrap()
        .id
        .clone();
    let other = service
        .split_pane_with_process(
            &primary,
            mez_mux::layout::SplitDirection::Vertical,
            Some("cat >/dev/null"),
        )
        .unwrap()
        .pane_id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&other)
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "complete unaffected sibling")
        .unwrap();
    let sibling = service
        .pending_agent_provider_tasks()
        .into_iter()
        .find(|task| task.pane_id == other)
        .unwrap();
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let actor = tokio::spawn(actor.run());
    let (mut client, mut server) = UnixStream::pair().unwrap();
    let control_handle = handle.clone();
    let control = tokio::spawn(async move {
        serve_async_runtime_control_connection_loop(
            &mut server,
            &control_handle,
            &mut ControlConnectionState::new(true, true),
            AsyncRuntimeControlConnectionConfig::new(
                65536,
                crate::runtime::current_effective_uid(),
            )
            .unwrap(),
            |served, _| served >= 2,
        )
        .await
    });
    let before = round_trip(&mut client, r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"refusal-control","requested_version":2,"requested_role":"primary","client":{"name":"refusal-control","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}},"authentication":{"mechanism":"peer_credentials"}}}"#).await;
    assert_eq!(before["id"], "init");
    assert!(before.get("error").is_none(), "{before}");
    let dispatch = handle
        .claim_configured_agent_provider_task(agent.clone(), turn.clone())
        .await
        .unwrap()
        .unwrap();
    let mut rejected_profile = dispatch.model_profile.clone();
    rejected_profile.model.clear();
    let error: MezError = crate::integrations::agent::context::assemble_model_request(
        &rejected_profile,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &dispatch.turn,
        &dispatch.context.to_agent_context(),
    )
    .unwrap_err()
    .into();
    assert!(error.is_agent_request_assembly_failure());
    let (event, completed) = provider_worker_event(
        agent,
        turn.clone(),
        dispatch.claim_generation,
        Ok(Err(error)),
    );
    assert!(!completed);
    assert!(matches!(
        event,
        RuntimeEvent::AgentProvider(AgentProviderEvent::LocalRequestFailure { .. })
    ));
    submit_authoritative_runtime_event(&handle, event)
        .await
        .unwrap();
    assert!(!handle.agent_turn_is_running(&turn).await.unwrap());

    let sibling_dispatch = handle
        .claim_configured_agent_provider_task(
            AgentId::opaque(sibling.agent_id).unwrap(),
            sibling.turn_id.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let (sender, _observations) = tokio::sync::mpsc::channel(8);
    let observer = ProviderWireRequestObserver::new(
        sibling_dispatch.turn.conversation_id.clone(),
        &other,
        sender,
    );
    let execution = super::incomplete::run_incomplete_fixture(
        &FinalProvider,
        &observer,
        &sibling_dispatch,
        0,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        execution.terminal_state,
        mez_agent::AgentTurnState::Completed
    );
    let (event, completed) = provider_worker_event(
        AgentId::opaque(sibling_dispatch.turn.agent_id.clone()).unwrap(),
        sibling.turn_id.clone(),
        sibling_dispatch.claim_generation,
        Ok(Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(
            execution,
        )))),
    );
    assert!(completed);
    submit_authoritative_runtime_event(&handle, event)
        .await
        .unwrap();
    // Accepted completion still owns checked transcript/publication work. Run
    // the production persistence service rather than treating event acceptance
    // as terminal settlement or manually finishing the sibling's ledger.
    let persistence = crate::host::async_runtime::run_async_persistence_side_effect_service(
        &handle,
        crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
            max_polls: 32,
            drain_limit: 64,
            idle_interval: Duration::from_millis(1),
        },
        |polls, _| polls >= 32,
    )
    .await
    .unwrap();
    assert_eq!(persistence.failed, 0);
    let after = round_trip(
        &mut client,
        r#"{"jsonrpc":"2.0","id":"after","method":"session/get","params":{}}"#,
    )
    .await;
    assert_eq!(after["id"], "after");
    assert!(after.get("error").is_none(), "{after}");
    assert_eq!(control.await.unwrap().unwrap(), 2);
    assert_eq!(
        handle.lifecycle_state().await.unwrap(),
        RuntimeLifecycleState::Running
    );
    handle.shutdown().await.unwrap();
    let mut exit = actor.await.unwrap();
    exit.service.terminate_all_pane_processes().unwrap();
    assert_eq!(exit.service.agent_turn_contexts()[&turn], original);
    assert!(!exit.service.agent_turn_executions().contains_key(&turn));
    assert_eq!(
        exit.service.agent_turn_ledger().turn(&turn).unwrap().state,
        mez_agent::AgentTurnState::Blocked
    );
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&sibling.turn_id)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Completed
    );
}
