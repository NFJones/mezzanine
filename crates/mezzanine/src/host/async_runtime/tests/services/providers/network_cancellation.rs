//! Real approved-network cancellation while actor progress ingress is held.
//!
//! The runtime issues the dispatch and its cancellation fence. An intentionally
//! unpolled actor retains the admitted progress request; stopping the real turn
//! must retire the worker/body and reject that same queued source afterward.
//! No provider, credential, paid dispatch or remote-effect certainty is faked.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A runtime stop cancels an issued network dispatch even while its actor
/// acknowledgment is held. The queued reply receiver and unfinished real HTTP
/// body both close; the queued exact-attempt progress cannot revive the turn.
#[tokio::test(flavor = "current_thread")]
async fn async_approved_network_stop_drops_held_ingress_and_real_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/cancel", listener.local_addr().unwrap());
    let peer = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 4096];
        assert!(stream.read(&mut bytes).await.unwrap() > 0);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100\r\nConnection: close\r\n\r\nprovisional body\n").await.unwrap();
        assert_eq!(stream.read(&mut bytes).await.unwrap(), 0);
    };
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "fetch a local fixture")
        .unwrap();
    let task = service.pending_agent_provider_tasks()[0].clone();
    service.remove_pending_agent_provider_task(&task.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == task.turn_id)
        .unwrap()
        .clone();
    let action = mez_agent::AgentAction {
        id: "fetch-cancel".into(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url,
            format: None,
            max_bytes: None,
        },
    };
    let mut execution = native_shell_provider_execution(&task, String::new());
    execution.request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::NetworkFetch);
    execution.response.raw_text = "fetching local fixture".into();
    execution.response.action_batch = Some(mez_agent::MaapBatch {
        rationale: "qualify scoped network cancellation".into(),
        actions: vec![action.clone()],
    });
    execution.action_results = vec![mez_agent::ActionResult::running(
        &turn,
        &action,
        vec!["network queued".into()],
        None,
    )];
    service
        .apply_agent_provider_completed_event(
            &AgentId::opaque(task.agent_id).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();
    let dispatch = service
        .claim_approved_external_action(&turn.turn_id, &action.id)
        .unwrap()
        .unwrap();
    let cancellation = dispatch.cancellation.clone();
    let (handle, mut actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let worker =
        crate::host::async_runtime::client::execute_approved_external_action(handle, dispatch);
    let retire = async {
        // Admission, not a timer, proves the worker is awaiting real ingress.
        actor.request_ingress_notify.notified().await;
        let queued = {
            let mut ingress = actor.request_ingress.lock().unwrap();
            ingress
                .normal_requests
                .pop_front()
                .or_else(|| ingress.interactive_requests.pop_front())
                .or_else(|| ingress.urgent_requests.pop_front())
                .or_else(|| ingress.maintenance_requests.pop_front())
                .unwrap()
        };
        let crate::host::async_runtime::AsyncRuntimeRequest::SubmitRuntimeEvents { batch, reply } =
            queued.envelope.request
        else {
            panic!("held ingress must be the worker's provisional event");
        };
        assert_eq!(batch.events.len(), 1);
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        assert!(cancellation.flag().load(Ordering::SeqCst));
        (batch, reply)
    };
    let (worker_result, (batch, reply), ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(worker, retire, peer)
    })
    .await
    .unwrap();
    assert!(worker_result.unwrap().is_none());
    assert!(
        reply.is_closed(),
        "cancellation must drop the scoped acknowledgment receiver"
    );
    for event in batch.events {
        let RuntimeEvent::ApprovedExternalActionProgress(progress) = event else {
            panic!("expected provisional progress")
        };
        assert!(
            !actor
                .service
                .apply_approved_external_action_progress(progress)
                .unwrap()
        );
    }
    assert!(!actor.service.agent_provider_task_is_pending(&turn.turn_id));
    assert!(!actor.service.agent_turn_is_running(&turn.turn_id));
    actor.service.terminate_all_pane_processes().unwrap();
}
