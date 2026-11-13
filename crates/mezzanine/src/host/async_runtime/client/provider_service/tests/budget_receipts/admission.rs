//! Actual Chat/Responses transport polling behind original native authorization.
//! These tests install the staged factory explicitly; production activation,
//! fair service scheduling and physical exposure release are not asserted.

use super::*;
use crate::host::async_runtime::client::provider_service::budget_admission::BudgetAttemptAdmission;
use crate::integrations::agent::provider::OpenAiResponsesProvider;
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;

struct GateTransport {
    pending: PendingProviderWireObservations,
    store: TokenUsageStore,
    calls: Arc<AtomicUsize>,
    responses: bool,
}

fn text() -> String {
    serde_json::json!({"rationale":"fixture", "actions":[{
        "type":"say", "status":"final", "content_type":"text/plain; charset=utf-8", "text":"done"
    }]})
    .to_string()
}

impl GateTransport {
    fn entered(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(rows(&self.store, "budget_admissions"), 1);
        assert_eq!(
            self.pending.len(),
            1,
            "authorization lost pre-send journal custody"
        );
        let original = self.pending.snapshot().pop().unwrap();
        self.pending.acknowledge(&original.request_id);
        assert_eq!(
            self.pending.len(),
            1,
            "planned identity was ACKed as a receipt"
        );
    }
}

impl AsyncProviderHttpTransport for GateTransport {
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
            self.entered();
            Ok(mez_agent::ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: if self.responses {
                    serde_json::json!({"model":"test-model", "status":"completed", "output_text":text(),
                        "usage":{"input_tokens":11,"output_tokens":3}}).to_string()
                } else {
                    serde_json::json!({"model":"test-model", "usage":{"prompt_tokens":11,"completion_tokens":3},
                        "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text()}}]}).to_string()
                },
            })
        })
    }

    fn send_async_with_sse_events<'a>(
        &'a self,
        _: &'a mez_agent::ProviderHttpRequest,
        on_event: &'a mut (
                    dyn FnMut(mez_agent::SseEvent) -> Pin<Box<dyn Future<Output = ()> + Send>>
                        + Send
                ),
    ) -> Pin<
        Box<
            dyn Future<Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.entered();
            assert!(self.responses);
            for value in [
                serde_json::json!({"type":"response.in_progress","response":{"usage":{"input_tokens":11,"output_tokens":3}}}),
                serde_json::json!({"type":"response.output_text.delta","delta":text()}),
                serde_json::json!({"type":"response.completed","response":{"model":"test-model","status":"completed","output_text":text(),"usage":{"input_tokens":11,"output_tokens":3}}}),
            ] {
                on_event(mez_agent::SseEvent {
                    name: None,
                    data: value.to_string(),
                })
                .await;
            }
            Ok(mez_agent::ProviderHttpResponse {
                status_code: 200,
                headers: std::collections::BTreeMap::from([(
                    "content-type".into(),
                    "text/event-stream".into(),
                )]),
                body: String::new(),
            })
        })
    }
}

async fn boundary(responses: bool, stream: bool, held: bool) {
    let limits = BudgetLimits {
        input: if held {
            TokenLimit::limited(0).unwrap()
        } else {
            TokenLimit::Unlimited
        },
        output: TokenLimit::Unlimited,
    };
    let Prepared {
        service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(limits).await;
    let pending = PendingProviderWireObservations::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let transport = GateTransport {
        pending: pending.clone(),
        store: store.clone(),
        calls: calls.clone(),
        responses,
    };
    let (chat, responses_provider) = if responses {
        (
            None,
            Some(
                OpenAiResponsesProvider::with_endpoint_headers_and_stream(
                    "fixture-key",
                    "https://example.test/responses",
                    1000,
                    Default::default(),
                    stream,
                    transport,
                )
                .unwrap(),
            ),
        )
    } else {
        (
            Some(
                OpenAiCompatibleChatCompletionsProvider::without_auth(transport)
                    .unwrap()
                    .with_provider_id("local-chat")
                    .unwrap(),
            ),
            None,
        )
    };
    let provider: &dyn AsyncModelProvider = if let Some(provider) = &chat {
        provider
    } else {
        responses_provider.as_ref().unwrap()
    };
    let mut request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    request.provider = provider.provider_id().into();
    let work = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new(&request.provider, &request.model),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    let factory = Arc::new(BudgetAttemptAdmission::new(work.clone()));
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
        .with_claim_generation(7)
        .with_pending_observations(pending.clone())
        .with_budget_receipts(factory.clone())
        .unwrap();
    let chat_observed = chat.as_ref().map(|provider| {
        ObservedAsyncModelProvider::new(provider, &observer, ProviderRequestPurpose::Execution)
    });
    let responses_observed = responses_provider.as_ref().map(|provider| {
        ObservedAsyncModelProvider::new(provider, &observer, ProviderRequestPurpose::Execution)
    });
    let observed: &dyn AsyncModelProvider = if let Some(provider) = &chat_observed {
        provider
    } else {
        responses_observed.as_ref().unwrap()
    };
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        assert_eq!(
            rows(&store, "budget_admissions"),
            0,
            "fixture preauthorized its exchange"
        );
        if held {
            // Responses offers its failed observation before returning; Chat
            // retains the same unknown evidence synchronously in stack Drop.
            // Both must keep their existing actor-handoff contract.
            let error = tokio::select! {
                result = &mut call => result.unwrap_err(),
                observation = receiver.recv() => {
                    let observation = observation.unwrap();
                    assert_eq!(calls.load(Ordering::SeqCst), 0);
                    assert!(observation.usage.is_none());
                    submit_pending_provider_observation(&handle, &pending, observation).await.unwrap();
                    call.await.unwrap_err()
                }
            };
            assert_eq!(error.kind(), MezErrorKind::BudgetSuspended);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(rows(&store, "budget_admissions"), 0);
            if let Some(unknown) = pending.snapshot().pop() {
                assert!(unknown.usage.is_none());
                pending.checkpoint(&unknown).await.unwrap();
                pending.acknowledge(&unknown.request_id);
            }
            assert!(pending.is_empty());
            assert_eq!(rows(&store, "budget_receipts"), 0);
        } else {
            let observation = tokio::select! {
                biased;
                _ = &mut call => panic!("provider advanced before original durable receipt"),
                observation = receiver.recv() => observation.unwrap(),
            };
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(rows(&store, "budget_receipts"), 0);
            submit_pending_provider_observation(&handle, &pending, observation.clone())
                .await
                .unwrap();
            assert!(call.await.unwrap().action_batch.is_some());
            assert_eq!(
                store.budget_account(&member.root).unwrap().known,
                UsageTotals {
                    input: 11,
                    output: 3
                }
            );
            assert_eq!(rows(&store, "budget_receipts"), 2);
            assert_eq!(
                rows(&store, "budget_admissions"),
                1,
                "retirement erased the original replay-only grant"
            );
            let next = Arc::new(BudgetAttemptAdmission::new(work.clone()));
            let mut changed = observation.clone();
            changed.request_id.push_str("-next");
            assert!(next.plan(&changed).is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    };
    let control = async {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            std::panic::AssertUnwindSafe(client).catch_unwind(),
        )
        .await;
        handle.shutdown().await.unwrap();
        result
    };
    let (result, mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    drop((
        exit,
        observer,
        factory,
        work,
        dispatch,
        chat,
        responses_provider,
        handle,
    ));
    root.cleanup();
    if let Err(panic) = result.expect("native admission boundary did not settle") {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn budget_wire_actual_admission_controls_chat_and_responses_physical_polls() {
    for (responses, stream) in [(false, false), (true, false), (true, true)] {
        for held in [false, true] {
            boundary(responses, stream, held).await;
        }
    }
}

#[tokio::test]
async fn budget_wire_cancelled_authorization_keeps_original_worker_and_unknown_receipt() {
    let Prepared {
        mut service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits::default()).await;
    let pending = PendingProviderWireObservations::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(GateTransport {
        pending: pending.clone(),
        store: store.clone(),
        calls: calls.clone(),
        responses: false,
    })
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
    let work = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new(&request.provider, &request.model),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    let factory = Arc::new(BudgetAttemptAdmission::new(work.clone()));
    let (sender, _receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
        .with_pending_observations(pending.clone())
        .with_budget_receipts(factory.clone())
        .unwrap();
    let observed =
        ObservedAsyncModelProvider::new(&provider, &observer, ProviderRequestPurpose::Execution);
    let connection = rusqlite::Connection::open(store.path()).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE;").unwrap();
    {
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        assert!(futures_util::poll!(&mut call).is_pending());
        assert_eq!(pending.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let unknown = pending.snapshot().pop().unwrap();
    assert!(unknown.usage.is_none());
    {
        let checkpoint = pending.checkpoint(&unknown);
        tokio::pin!(checkpoint);
        assert!(
            futures_util::poll!(&mut checkpoint).is_pending(),
            "canceled observer abandoned authorization writer"
        );
    }
    connection.execute_batch("ROLLBACK;").unwrap();
    pending.checkpoint(&unknown).await.unwrap();
    assert_eq!(rows(&store, "budget_admissions"), 1);
    assert_eq!(rows(&store, "budget_receipts"), 1);
    assert_eq!(
        store.budget_account(&member.root).unwrap().known,
        UsageTotals::default()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    pending.acknowledge(&unknown.request_id);
    assert!(pending.is_empty());
    service.terminate_all_pane_processes().unwrap();
    drop((connection, observer, factory, work, dispatch, service));
    root.cleanup();
}
