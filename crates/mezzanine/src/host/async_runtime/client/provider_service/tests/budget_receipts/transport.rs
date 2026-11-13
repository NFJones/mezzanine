//! Real loopback HTTP -> exact local future destruction -> native factory ->
//! durable subtotal + terminal-unknown retirement -> actor ACK. Reported partial
//! counters never become complete merely because the local transport retired.

use super::*;
use crate::host::async_runtime::client::provider_service::budget_admission::BudgetAttemptAdmission;
use crate::integrations::agent::provider::{
    OpenAiResponsesProvider, ProviderObservationReceipt, ProviderTransportRetirement,
};
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use std::task::{Context, Poll};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// This transparent fixture wrapper observes destruction of the real Reqwest
/// future, including scoped SSE callbacks, without replacing network I/O.
struct Tracked<'a> {
    inner: Option<
        Pin<
            Box<
                dyn Future<Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>>
                    + Send
                    + 'a,
            >,
        >,
    >,
    dropped: Arc<AtomicUsize>,
}
impl Future for Tracked<'_> {
    type Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().inner.as_mut().unwrap().as_mut().poll(cx)
    }
}
impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        drop(self.inner.take());
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct Transport {
    dropped: Arc<AtomicUsize>,
    event: Arc<tokio::sync::Notify>,
    blocked_callback: bool,
    callback_dropped: Arc<AtomicUsize>,
}

struct CallbackGuard(Arc<AtomicUsize>);
impl Drop for CallbackGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl AsyncProviderHttpTransport for Transport {
    fn send_async<'a>(
        &'a self,
        request: &'a mez_agent::ProviderHttpRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(Tracked {
            inner: Some(ReqwestProviderHttpTransport.send_async(request)),
            dropped: self.dropped.clone(),
        })
    }
    fn send_async_with_sse_events<'a>(
        &'a self,
        request: &'a mez_agent::ProviderHttpRequest,
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
        Box::pin(Tracked {
            inner: Some(Box::pin(async move {
                let mut observed = |event| {
                    let callback = on_event(event);
                    let event = self.event.clone();
                    let blocked = self.blocked_callback;
                    let guard = blocked.then(|| CallbackGuard(self.callback_dropped.clone()));
                    Box::pin(async move {
                        let _guard = guard;
                        callback.await;
                        event.notify_one();
                        if blocked {
                            std::future::pending::<()>().await;
                        }
                    }) as Pin<Box<dyn Future<Output = ()> + Send>>
                };
                ReqwestProviderHttpTransport
                    .send_async_with_sse_events(request, &mut observed)
                    .await
            })),
            dropped: self.dropped.clone(),
        })
    }
}

struct Factory {
    native: BudgetAttemptAdmission,
    dropped: Arc<AtomicUsize>,
    retired: AtomicUsize,
    callback_dropped: Option<Arc<AtomicUsize>>,
    stages: std::sync::OnceLock<(
        crate::runtime::RuntimeBudgetReceiptWork,
        crate::runtime::RuntimeBudgetReceiptWork,
    )>,
}
impl ProviderObservationReceiptFactory for Factory {
    fn plan(&self, o: &ProviderWireRequestObservation) -> Result<()> {
        self.native.plan(o)
    }
    fn before_send<'a>(
        &'a self,
        o: &'a ProviderWireRequestObservation,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        self.native.before_send(o)
    }
    fn transport_retired(
        &self,
        o: &ProviderWireRequestObservation,
        proof: ProviderTransportRetirement,
    ) {
        assert_eq!(
            self.dropped.load(Ordering::SeqCst),
            1,
            "native factory preceded real transport destruction"
        );
        if let Some(callback) = &self.callback_dropped {
            assert_eq!(
                callback.load(Ordering::SeqCst),
                1,
                "active callback outlived transport witness"
            );
        }
        assert!(!self.native.has_transport_retirement(o));
        self.native.transport_retired(o, proof);
        assert!(self.native.has_transport_retirement(o));
        self.retired.fetch_add(1, Ordering::SeqCst);
    }
    fn freeze(
        &self,
        o: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        assert!(
            self.native.has_transport_retirement(o),
            "receipt preceded local transport retirement"
        );
        let receipt = self.native.freeze(o)?;
        assert!(
            self.stages
                .set(self.native.receipt_stages_for_test(o))
                .is_ok()
        );
        Ok(receipt)
    }
}

#[tokio::test]
async fn budget_wire_loopback_transport_retirement_precedes_native_receipt_and_ack() {
    for (responses, stream) in [(false, false), (true, false), (true, true)] {
        for cancel in [false, true] {
            qualify(responses, stream, cancel, false, 0).await;
        }
    }
}

#[tokio::test]
async fn budget_wire_loopback_transport_destroys_actively_blocked_sse_callback() {
    qualify(true, true, true, true, 0).await;
}

/// The original real-HTTP request stays at its actor ACK barrier while exact
/// terminal bookkeeping fails, loses its result, or outlives a canceled waiter.
#[tokio::test]
async fn budget_wire_loopback_retirement_writer_faults_keep_original_custody() {
    for fault in 1..=3 {
        qualify(true, false, false, false, fault).await;
    }
}

async fn qualify(responses: bool, stream: bool, cancel: bool, blocked_callback: bool, fault: u8) {
    let Prepared {
        service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits::default()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (entered, received) = tokio::sync::oneshot::channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read the actual entire submitted request before gating the response.
        // Bounded generated input only; no provider/user payload is printed.
        let mut bytes = Vec::new();
        let end = loop {
            if let Some(index) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                break index + 4;
            }
            let mut chunk = [0; 4096];
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0 && bytes.len() + read <= 1024 * 1024);
            bytes.extend_from_slice(&chunk[..read]);
        };
        let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        assert!(end + length <= 1024 * 1024);
        while bytes.len() < end + length {
            let mut chunk = [0; 4096];
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
        }
        entered.send(()).unwrap();
        if stream {
            let partial = serde_json::json!({"type":"response.in_progress","response":{"usage":{"input_tokens":11,"output_tokens":3}}});
            let text = serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string();
            let delta = serde_json::json!({"type":"response.output_text.delta","delta":text});
            let terminal = serde_json::json!({"type":"response.completed","response":{"model":"test-model","status":"completed","output_text":text,"usage":{"input_tokens":11,"output_tokens":3}}});
            let data = if cancel {
                format!("data: {partial}\n\n")
            } else {
                format!("data: {partial}\n\ndata: {delta}\n\ndata: {terminal}\n\n")
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{data}\r\n", data.len()).as_bytes()).await.unwrap();
            if !cancel {
                socket.write_all(b"0\r\n\r\n").await.unwrap();
            }
        } else if !cancel {
            let text = serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string();
            let body = if responses { serde_json::json!({"model":"test-model","status":"completed","output_text":text,"usage":{"input_tokens":11,"output_tokens":3}}) }
                else { serde_json::json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}],"usage":{"prompt_tokens":11,"completion_tokens":3}}) }.to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
        // Keep the exact server fixture owned until the client has explicitly
        // observed/destroyed its request future, even on ordinary success.
        let _ = gate.await;
    });
    let dropped = Arc::new(AtomicUsize::new(0));
    let event = Arc::new(tokio::sync::Notify::new());
    let callback_dropped = Arc::new(AtomicUsize::new(0));
    let transport = Transport {
        dropped: dropped.clone(),
        event: event.clone(),
        blocked_callback,
        callback_dropped: callback_dropped.clone(),
    };
    let (chat, response) = if responses {
        (
            None,
            Some(
                OpenAiResponsesProvider::with_endpoint_headers_and_stream(
                    "fixture-key",
                    &endpoint,
                    10_000,
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
                    .unwrap()
                    .with_endpoint(endpoint),
            ),
            None,
        )
    };
    let provider: &dyn AsyncModelProvider = match &chat {
        Some(p) => p,
        None => response.as_ref().unwrap(),
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
    let factory = Arc::new(Factory {
        native: BudgetAttemptAdmission::new(work),
        dropped: dropped.clone(),
        retired: AtomicUsize::new(0),
        callback_dropped: blocked_callback.then(|| callback_dropped.clone()),
        stages: Default::default(),
    });
    let pending = PendingProviderWireObservations::default();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
        .with_claim_generation(7)
        .with_pending_observations(pending.clone())
        .with_budget_receipts(factory.clone())
        .unwrap();
    let chat_observed = chat.as_ref().map(|provider| {
        ObservedAsyncModelProvider::new(provider, &observer, ProviderRequestPurpose::Execution)
    });
    let responses_observed = response.as_ref().map(|provider| {
        ObservedAsyncModelProvider::new(provider, &observer, ProviderRequestPurpose::Execution)
    });
    let observed: &dyn AsyncModelProvider = match &chat_observed {
        Some(provider) => provider,
        None => responses_observed.as_ref().unwrap(),
    };
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        tokio::select! { biased; _ = &mut call => panic!("exchange finished before request received"), r = received => r.unwrap(), }
        if cancel {
            if stream {
                tokio::select! { biased; _ = &mut call => panic!("partial SSE completed"), () = event.notified() => {} }
            }
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            assert_eq!(factory.retired.load(Ordering::SeqCst), 0);
            assert_eq!(callback_dropped.load(Ordering::SeqCst), 0);
            drop(call);
        } else {
            let observation = tokio::select! { biased; _ = &mut call => panic!("exchange bypassed actor ACK"), o = receiver.recv() => o.unwrap() };
            assert_eq!(factory.retired.load(Ordering::SeqCst), 1);
            assert!(factory.native.has_transport_retirement(&observation));
            assert_eq!(rows(&store, "budget_receipts"), 0);
            if fault != 0 {
                let (prefix, terminal) = factory.stages.get().unwrap();
                prefix.commit_owner().commit().await.unwrap();
                assert_eq!(rows(&store, "budget_receipts"), 1);
                let owner = terminal.commit_owner();
                match fault {
                    1 => {
                        let db = rusqlite::Connection::open(store.path()).unwrap();
                        db.execute_batch("CREATE TRIGGER refuse_retirement BEFORE UPDATE OF retired_receipt ON budget_admissions BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
                        assert!(pending.checkpoint(&observation).await.is_err());
                        assert_eq!(rows(&store, "budget_receipts"), 1);
                        assert_eq!(db.query_row("SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NULL", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
                        db.execute_batch("DROP TRIGGER refuse_retirement;").unwrap();
                        assert!(
                            pending.checkpoint(&observation).await.is_err(),
                            "refusal must remain sticky without explicit bookkeeping recovery"
                        );
                        assert!(owner.reconcile().await.unwrap().is_none());
                        owner.retry_after_absence().await.unwrap();
                    }
                    2 => {
                        let execute = terminal.clone();
                        assert!(
                            owner
                                .commit_with_for_test(move || {
                                    execute.execute()?;
                                    panic!("fixture terminal result lost after physical commit");
                                })
                                .await
                                .is_err()
                        );
                        assert_eq!(rows(&store, "budget_receipts"), 2);
                        assert!(owner.reconcile().await.unwrap().is_some());
                    }
                    3 => {
                        let execute = terminal.clone();
                        let borrowed = owner.clone();
                        let (started, ready) = tokio::sync::oneshot::channel();
                        let (release_writer, blocked) = std::sync::mpsc::channel();
                        let waiter = tokio::spawn(async move {
                            borrowed
                                .commit_with_for_test(move || {
                                    let _ = started.send(());
                                    blocked.recv().map_err(|_| {
                                        MezError::invalid_state("fixture writer cleanup")
                                    })?;
                                    execute.execute()
                                })
                                .await
                        });
                        ready.await.unwrap();
                        waiter.abort();
                        assert!(waiter.await.unwrap_err().is_cancelled());
                        assert_eq!(rows(&store, "budget_receipts"), 1);
                        {
                            let checkpoint = pending.checkpoint(&observation);
                            tokio::pin!(checkpoint);
                            assert!(futures_util::poll!(&mut checkpoint).is_pending());
                        }
                        release_writer.send(()).unwrap();
                        assert!(owner.reconcile().await.unwrap().is_some());
                    }
                    _ => unreachable!(),
                }
                assert!(futures_util::poll!(&mut call).is_pending());
                assert_eq!(factory.retired.load(Ordering::SeqCst), 1);
                assert_eq!(pending.len(), 1);
            }
            submit_pending_provider_observation(&handle, &pending, observation)
                .await
                .unwrap();
            assert!(call.await.unwrap().action_batch.is_some());
        }
        if cancel {
            let observation = pending.snapshot().pop().unwrap();
            assert!(factory.native.has_transport_retirement(&observation));
            assert_eq!(observation.usage.is_some(), stream);
            submit_pending_provider_observation(&handle, &pending, observation)
                .await
                .unwrap();
        }
        assert!(pending.is_empty());
        assert_eq!(
            rows(&store, "budget_receipts"),
            2,
            "retired transport must durably transfer subtotal and terminal-unknown evidence"
        );
        assert_eq!(
            (
                dropped.load(Ordering::SeqCst),
                factory.retired.load(Ordering::SeqCst)
            ),
            (1, 1)
        );
        assert_eq!(
            rows(&store, "budget_admissions"),
            1,
            "retirement must preserve the original replay-only grant"
        );
        let db = rusqlite::Connection::open(store.path()).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let snapshot: String = db
            .query_row("SELECT snapshot FROM budget_attempts", [], |r| r.get(0))
            .unwrap();
        let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        assert!(snapshot["terminal"]["input"].is_null());
        assert!(snapshot["terminal"]["output"].is_null());
        let known = store.budget_account(&member.root).unwrap().known;
        assert_eq!(
            known,
            if cancel && !stream {
                UsageTotals::default()
            } else {
                UsageTotals {
                    input: 11,
                    output: 3,
                }
            }
        );
    };
    let control = async {
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            std::panic::AssertUnwindSafe(client).catch_unwind(),
        )
        .await;
        let _ = release.send(());
        let mut server = server;
        let server_result = match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
            Ok(result) => result.is_ok(),
            Err(_) => {
                server.abort();
                let _ = server.await;
                false
            }
        };
        handle.shutdown().await.unwrap();
        (result, server_result)
    };
    let (result, mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    drop((
        exit, observer, factory, dispatch, chat, response, handle, pending,
    ));
    root.cleanup();
    let (result, server_result) = result;
    assert!(
        server_result,
        "loopback server failed or exceeded its bounded join"
    );
    if let Err(panic) = result.unwrap() {
        std::panic::resume_unwind(panic);
    }
}
