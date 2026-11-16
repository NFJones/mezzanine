//! Real provider-driver failure scope with two genuine independent accounts.
//! A uses a parsed offline native candidate; B is an already-admitted gated
//! request worker, not a paid/physical inference or fabricated billing receipt.

use super::*;
use crate::host::async_runtime::AsyncRuntimeSessionActor;
use futures_util::FutureExt;
use std::sync::atomic::{AtomicBool, AtomicU64};

struct NativeResponse(String);
impl AsyncProviderHttpTransport for NativeResponse {
    fn send_async<'a>(
        &'a self,
        _: &'a mez_agent::ProviderHttpRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            Ok(mez_agent::ProviderHttpResponse {
                status_code: 200, headers: Default::default(),
                body: serde_json::json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":self.0}}],"usage":{"prompt_tokens":0,"completion_tokens":0}}).to_string(),
            })
        })
    }
}

#[tokio::test]
async fn budget_activity_local_capture_and_reader_failure_keep_provider_service_live() {
    for panic_reader in [false, true] {
        Box::pin(qualify(panic_reader)).await;
    }
}

async fn qualify(panic_reader: bool) {
    let Prepared {
        mut service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits::default()).await;
    let primary = service
        .session()
        .clients()
        .iter()
        .find(|client| service.session().is_attached_primary(&client.id))
        .unwrap()
        .id
        .clone();
    service.permission_policy_mut().set_approval_bypass(true);
    let counter = root.0.as_ref().unwrap().join("local-refusal-native-count");
    let body = serde_json::json!({"rationale":"native service boundary","actions":[{"type":"shell_command","summary":"count original native candidate","command":format!("printf x >> '{}'", counter.display())}]}).to_string();
    let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(NativeResponse(body))
        .unwrap()
        .with_provider_id("local-chat")
        .unwrap();
    let request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    let response = provider.send_request_async(&request).await.unwrap();
    let action_results = response
        .action_batch
        .as_ref()
        .unwrap()
        .actions
        .iter()
        .map(|action| mez_agent::ActionResult::running(&dispatch.turn, action, Vec::new(), None))
        .collect();
    let execution = mez_agent::AgentTurnExecution {
        request,
        latest_response_usage: response.usage,
        response,
        routing_token_usage_by_model: Default::default(),
        action_results,
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    let turn_a = dispatch.turn.turn_id.clone();
    let completion = RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
        agent_id: AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap(),
        turn_id: turn_a.clone(),
        claim_generation: dispatch.claim_generation,
        execution: Box::new(execution),
    });

    let pane_b = service
        .session_mut_for_tests()
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    let conversation_b = service
        .agent_shell_store_mut()
        .enter_or_resume(pane_b.as_str())
        .unwrap()
        .session_id
        .clone();
    service.cancel_agent_session_title_task(&conversation_b);
    service
        .schedule_runtime_agent_session_title(&conversation_b, Some("Independent original title"))
        .unwrap();
    let title_id = service
        .pending_agent_session_title_dispatch_id(&conversation_b)
        .unwrap()
        .to_owned();
    let preparation = service
        .prepare_runtime_budget_auxiliary(&crate::runtime::RuntimeBudgetAuxiliarySource::Title {
            conversation: conversation_b.clone(),
            dispatch: title_id.clone(),
        })
        .unwrap()
        .unwrap();
    let prepared = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(prepared).unwrap();
    let dispatch_b = service
        .claim_agent_session_title_dispatch(&conversation_b, &title_id)
        .unwrap()
        .unwrap();
    assert_ne!(
        dispatch_b.budget_owner.as_ref().unwrap().member.root,
        member.root
    );
    service.inject_budget_activity_fault(turn_a.clone(), panic_reader);
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let retirement = AgentProviderWorkRetirement::new(handle.clone());
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    {
        let mut state = retirement.state.lock().await;
        let ProviderServiceState {
            workers, owners, ..
        } = &mut *state;
        let owner = ProviderWorkerOwner::title(&dispatch_b);
        assert!(
            spawn_owned_agent_provider_worker(workers, owners, Some(owner), async move {
                let _original = dispatch_b;
                let _ = entered.send(());
                let _ = released.await;
                done.store(true, Ordering::SeqCst);
                Ok(None)
            })
            .is_some()
        );
    }
    let stop = Arc::new(AtomicBool::new(false));
    let driver_stop = stop.clone();
    let polls = Arc::new(AtomicU64::new(0));
    let driver_polls = polls.clone();
    let control = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(completion);
        let adoption = handle.submit_runtime_events(batch).await.unwrap();
        drop(dispatch);
        let driver_handle = handle.clone();
        let driver = tokio::spawn(async move {
            run_async_agent_provider_service_owned(
                &driver_handle,
                AsyncAgentProviderServiceConfig::new(1).unwrap(),
                move |count, _| {
                    driver_polls.store(count, Ordering::SeqCst);
                    driver_stop.load(Ordering::SeqCst)
                },
                retirement,
            )
            .await
        });
        let exercise = std::panic::AssertUnwindSafe(async {
            if adoption.applied != 1 {
                return Err("original provider completion was not adopted");
            }
            tokio::time::timeout(Duration::from_secs(5), entered_rx)
                .await
                .map_err(|_| "independent worker did not start")?
                .map_err(|_| "independent worker disappeared")?;
            // A successful return after at least two driver turns crosses the
            // real local-failure dispatch and its subsequent service iteration.
            tokio::time::timeout(Duration::from_secs(5), async {
                while polls.load(Ordering::SeqCst) < 2 && !driver.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| "provider service did not continue after local refusal")?;
            if driver.is_finished() {
                return Err("local budget failure stopped provider service");
            }
            Ok(())
        })
        .catch_unwind()
        .await;
        let _ = release.send(());
        let completed = tokio::time::timeout(Duration::from_secs(5), async {
            while !finished.load(Ordering::SeqCst) && !driver.is_finished() {
                tokio::task::yield_now().await;
            }
            finished.load(Ordering::SeqCst)
        })
        .await
        .unwrap_or(false);
        stop.store(true, Ordering::SeqCst);
        let driver_result = driver.await.unwrap();
        handle
            .execute_agent_shell_command(primary, "/stop".into())
            .await
            .unwrap();
        handle.shutdown().await.unwrap();
        (exercise, completed, driver_result)
    };
    let ((exercise, completed, driver_result), mut exit) = tokio::join!(control, actor.run());
    let fault_pending = exit.service.budget_activity_fault_pending_for_tests();
    exit.service.terminate_all_pane_processes().unwrap();
    let native_count = std::fs::read(&counter).unwrap_or_default();
    drop((exit, handle, store));
    root.cleanup();
    assert!(!fault_pending, "service never consumed the root-A fault");
    assert!(
        driver_result.is_ok(),
        "root-local failure escaped: {driver_result:?}"
    );
    assert!(
        completed,
        "independent original worker was aborted before useful completion"
    );
    match exercise {
        Ok(result) => result.unwrap(),
        Err(panic) => std::panic::resume_unwind(panic),
    }
    assert!(
        native_count.is_empty(),
        "refused unissued candidate executed"
    );
}
