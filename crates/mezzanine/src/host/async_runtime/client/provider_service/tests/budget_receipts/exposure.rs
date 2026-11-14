//! Physical HTTP qualification of unlimited -> finite admission. Three genuine
//! exchanges overlap before policy publication; each original journal must settle
//! before another finite-root exchange can issue. This is not scheduler fairness
//! or production activation qualification.

use super::*;
use crate::host::async_runtime::client::provider_service::budget_admission::BudgetRequestAdmission;
use crate::integrations::http::ReqwestProviderHttpTransport;
use crate::runtime::RuntimeBudgetRequestOwner;
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Provider = OpenAiCompatibleChatCompletionsProvider<ReqwestProviderHttpTransport>;

/// Keep the real producer handle through assertion unwinds. The enclosing test
/// aborts and joins all remaining producers before retiring its private store.
struct Exchange {
    worker: Option<tokio::task::JoinHandle<Result<ModelResponse>>>,
    pending: PendingProviderWireObservations,
    receiver: tokio::sync::mpsc::Receiver<ProviderWireRequestObservation>,
}

impl Exchange {
    fn start(
        provider: Arc<Provider>,
        request: ModelRequest,
        owner: RuntimeBudgetRequestOwner,
    ) -> Self {
        let pending = PendingProviderWireObservations::default();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let observer = ProviderWireRequestObserver::new(&owner.member.conversation, "%1", sender)
            .with_claim_generation(7)
            .with_pending_observations(pending.clone())
            .with_budget_receipt_source(Arc::new(BudgetRequestAdmission::new(owner)))
            .unwrap();
        let worker = tokio::spawn(async move {
            ObservedAsyncModelProvider::new(
                &*provider,
                &observer,
                ProviderRequestPurpose::Execution,
            )
            .send_request_async(&request)
            .await
        });
        Self {
            worker: Some(worker),
            pending,
            receiver,
        }
    }

    async fn finish(&mut self) -> Result<ModelResponse> {
        // Await through a borrow: cancellation of this waiter must not detach
        // the physical producer from the enclosing cleanup owner.
        let result = self.worker.as_mut().unwrap().await;
        self.worker = None;
        match result {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => panic!("exposure producer was cancelled: {error}"),
        }
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}

/// A completed panic must leave no consumed handle for teardown to poll again.
/// This regression needs no native process, database, or temporary directory.
#[tokio::test]
async fn budget_exposure_producer_panic_releases_completed_join_custody() {
    let (_, receiver) = tokio::sync::mpsc::channel(1);
    let mut exchange = Exchange {
        worker: Some(tokio::spawn(async {
            panic!("injected exposure producer panic")
        })),
        pending: Default::default(),
        receiver,
    };
    let result = std::panic::AssertUnwindSafe(exchange.finish())
        .catch_unwind()
        .await;
    assert!(result.is_err());
    assert!(
        exchange.worker.is_none(),
        "completed join remained in cleanup custody"
    );
}

/// Gate only after reading a complete real request. The listener continues to
/// accept while earlier responses are blocked, so the overlap oracle is network
/// traffic, not task counts, permits, or elapsed-time negative assertions.
async fn serve(
    listener: tokio::net::TcpListener,
    entered: tokio::sync::mpsc::Sender<tokio::sync::oneshot::Sender<()>>,
    mut stopped: tokio::sync::oneshot::Receiver<()>,
    sends: Arc<AtomicUsize>,
    sockets: Arc<tokio::sync::Mutex<JoinSet<()>>>,
) {
    loop {
        let (mut socket, _) = tokio::select! {
            biased;
            _ = &mut stopped => break,
            result = listener.accept() => result.unwrap(),
        };
        let entered = entered.clone();
        let sends = sends.clone();
        sockets.lock().await.spawn(async move {
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
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let length: usize = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
            }).unwrap();
            assert!(end + length <= 1024 * 1024);
            while bytes.len() < end + length {
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0 && bytes.len() + read <= 1024 * 1024);
                bytes.extend_from_slice(&chunk[..read]);
            }
            let (release, gate) = tokio::sync::oneshot::channel();
            sends.fetch_add(1, Ordering::SeqCst);
            if entered.send(release).await.is_err() || gate.await.is_err() {
                return;
            }
            let body = serde_json::json!({"model":"test-model", "choices":[{
                "index":0,"finish_reason":"stop","message":{"role":"assistant",
                "content":serde_json::json!({"rationale":"fixture","actions":[{
                    "type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"
                }]}).to_string()}
            }],"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
    }
}

/// The enclosing fixture, not the abortable listener, retains all socket joins.
/// Keep the first failure but drain siblings before returning it to the caller.
async fn retire_handlers(sockets: &tokio::sync::Mutex<JoinSet<()>>) -> Result<()> {
    let mut sockets = sockets.lock().await;
    sockets.abort_all();
    let mut failure = None;
    while let Some(result) = sockets.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
            && failure.is_none()
        {
            failure = Some(MezError::invalid_state(format!(
                "loopback handler failed: {error}"
            )));
        }
    }
    failure.map_or(Ok(()), Err)
}

/// Handler failure is reported only after every already-running sibling joins.
#[tokio::test]
async fn budget_exposure_handler_panic_still_joins_all_siblings() {
    struct Retired(Arc<AtomicUsize>);
    impl Drop for Retired {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let sockets = tokio::sync::Mutex::new(JoinSet::new());
    let retired = Arc::new(AtomicUsize::new(0));
    for _ in 0..3 {
        let (entered, ready) = tokio::sync::oneshot::channel();
        let retired = retired.clone();
        sockets.lock().await.spawn(async move {
            let _guard = Retired(retired);
            entered.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
    }
    let (entered, ready) = tokio::sync::oneshot::channel();
    sockets.lock().await.spawn(async move {
        entered.send(()).unwrap();
        panic!("injected loopback handler panic");
    });
    ready.await.unwrap();
    assert!(retire_handlers(&sockets).await.is_err());
    assert!(sockets.lock().await.is_empty());
    assert_eq!(retired.load(Ordering::SeqCst), 3);
}

async fn publish(store: TokenUsageStore, limits: BudgetLimits) {
    tokio::task::spawn_blocking(move || {
        let projection = store.budget_policy_projection().unwrap();
        let previous = projection.committed.unwrap();
        let operation = BudgetPolicyPublication {
            namespace: projection.namespace,
            id: BudgetReceiptId::new("finite-exposure-policy").unwrap(),
            expected_revision: projection.revision,
            previous_digest: previous.digest,
            proposed: BudgetConfigPolicy {
                source: previous.source,
                digest: "c".repeat(64),
                limits,
            },
            supersedes: None,
        };
        store.begin_budget_policy_publication(&operation).unwrap();
        store
            .reconcile_budget_policy_publication(&operation, &operation.proposed.digest)
            .unwrap();
    })
    .await
    .unwrap();
}

/// A refused exchange has positive native absence, not an inferred completion
/// or a zero-valued expense receipt. Its own journal can be checkpointed/ACKed
/// without touching any concurrently issued exchange.
async fn refused(
    exchange: &mut Exchange,
    store: &TokenUsageStore,
    sends: &AtomicUsize,
    expected: usize,
) {
    assert_eq!(
        exchange.finish().await.unwrap_err().kind(),
        MezErrorKind::BudgetSuspended
    );
    let unknown = exchange.pending.snapshot();
    assert_eq!(unknown.len(), 1);
    assert!(unknown[0].usage.is_none());
    exchange.pending.checkpoint(&unknown[0]).await.unwrap();
    exchange.pending.acknowledge(&unknown[0].request_id);
    assert!(exchange.pending.is_empty());
    assert_eq!(sends.load(Ordering::SeqCst), expected);
    assert_eq!(rows(store, "budget_admissions"), expected as i64);
}

async fn qualify(limits: BudgetLimits, producer_panic: bool) {
    let Prepared {
        service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits::default()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = Arc::new(
        Provider::without_auth(ReqwestProviderHttpTransport)
            .unwrap()
            .with_provider_id("local-chat")
            .unwrap()
            .with_endpoint(format!("http://{}/v1", listener.local_addr().unwrap())),
    );
    let request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    let owner = dispatch.budget_owner.as_ref().unwrap();
    let sends = Arc::new(AtomicUsize::new(0));
    let (entered, mut received) = tokio::sync::mpsc::channel(16);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let sockets = Arc::new(tokio::sync::Mutex::new(JoinSet::new()));
    let mut server = tokio::spawn(serve(
        listener,
        entered,
        stopped,
        sends.clone(),
        sockets.clone(),
    ));
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let mut exchanges = Vec::new();
    let control = async {
        let client = async {
            let mut gates = Vec::new();
            for _ in 0..3 {
                exchanges.push(Exchange::start(
                    provider.clone(),
                    request.clone(),
                    owner.clone(),
                ));
                gates.push(received.recv().await.unwrap());
            }
            assert_eq!(
                sends.load(Ordering::SeqCst),
                3,
                "unlimited requests did not overlap"
            );
            assert!(
                exchanges
                    .iter()
                    .all(|exchange| !exchange.worker.as_ref().unwrap().is_finished())
            );
            assert_eq!(rows(&store, "budget_receipts"), 0);
            if producer_panic {
                let (_, receiver) = tokio::sync::mpsc::channel(1);
                exchanges.push(Exchange {
                    worker: Some(tokio::spawn(async { std::panic::panic_any(ProducerPanic) })),
                    pending: Default::default(),
                    receiver,
                });
                let _ = exchanges.last_mut().unwrap().finish().await;
                unreachable!("injected producer panic must unwind the client");
            }
            publish(store.clone(), limits).await;
            exchanges.push(Exchange::start(
                provider.clone(),
                request.clone(),
                owner.clone(),
            ));
            refused(exchanges.last_mut().unwrap(), &store, &sends, 3).await;
            for (index, gate) in gates.into_iter().enumerate() {
                gate.send(()).unwrap();
                let observation = exchanges[index].receiver.recv().await.unwrap();
                // Real HTTP completion alone does not retire native exposure. This
                // check also covers the last remaining exchange before its commit.
                exchanges.push(Exchange::start(
                    provider.clone(),
                    request.clone(),
                    owner.clone(),
                ));
                refused(exchanges.last_mut().unwrap(), &store, &sends, 3).await;
                let exchange = &mut exchanges[index];
                submit_pending_provider_observation(&handle, &exchange.pending, observation)
                    .await
                    .unwrap();
                assert!(exchange.finish().await.unwrap().action_batch.is_some());
                assert!(exchange.pending.is_empty());
                assert_eq!(rows(&store, "budget_receipts"), ((index + 1) * 2) as i64);
                assert_eq!(
                    store.budget_account(&member.root).unwrap().known,
                    UsageTotals {
                        input: 11 * (index as u64 + 1),
                        output: 3 * (index as u64 + 1)
                    }
                );
            }
            // Only the complete original set's retirement permits serial mode.
            let fourth = exchanges.len();
            exchanges.push(Exchange::start(
                provider.clone(),
                request.clone(),
                owner.clone(),
            ));
            let gate = received.recv().await.unwrap();
            assert_eq!(sends.load(Ordering::SeqCst), 4);
            exchanges.push(Exchange::start(
                provider.clone(),
                request.clone(),
                owner.clone(),
            ));
            refused(exchanges.last_mut().unwrap(), &store, &sends, 4).await;
            gate.send(()).unwrap();
            let exchange = &mut exchanges[fourth];
            let observation = exchange.receiver.recv().await.unwrap();
            submit_pending_provider_observation(&handle, &exchange.pending, observation)
                .await
                .unwrap();
            assert!(exchange.finish().await.unwrap().action_batch.is_some());
            assert_eq!(rows(&store, "budget_receipts"), 8);
            assert_eq!(
                store.budget_account(&member.root).unwrap().known,
                UsageTotals {
                    input: 44,
                    output: 12
                }
            );
        };
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            std::panic::AssertUnwindSafe(client).catch_unwind(),
        )
        .await;
        // Retire actual producer futures before joining their original receipt
        // writers. No assertion path detaches a physical request or deletes a
        // fixture whose accounting retirement is still unproven.
        for exchange in &mut exchanges {
            if let Some(worker) = exchange.worker.take() {
                worker.abort();
                let _ = worker.await;
            }
        }
        let cleanup = tokio::time::timeout(Duration::from_secs(10), async {
            for exchange in &exchanges {
                for observation in exchange.pending.snapshot() {
                    exchange.pending.checkpoint(&observation).await?;
                    exchange.pending.acknowledge(&observation.request_id);
                }
            }
            Ok::<(), MezError>(())
        })
        .await;
        let _ = stop.send(());
        let served = match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
            Ok(result) => result,
            Err(_) => {
                server.abort();
                server.await
            }
        };
        let handlers =
            tokio::time::timeout(Duration::from_secs(10), retire_handlers(&sockets)).await;
        handle.shutdown().await.unwrap();
        (result, cleanup, served, handlers)
    };
    let ((result, cleanup, served, handlers), mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    drop((exit, exchanges, handle, provider, dispatch));
    if matches!(cleanup, Ok(Ok(()))) && handlers.is_ok() {
        root.cleanup();
    }
    cleanup
        .expect("original receipt cleanup timed out")
        .expect("original receipt retirement failed");
    served.unwrap();
    handlers.expect("handler joins timed out").unwrap();
    match result.expect("finite exposure qualification timed out") {
        Err(panic) if producer_panic => assert!(panic.is::<ProducerPanic>()),
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(()) => assert!(!producer_panic, "producer panic was swallowed"),
    }
}

#[derive(Debug)]
struct ProducerPanic;

/// Exercise real fixture teardown while three HTTP exchanges are still gated.
/// A fourth producer's panic must not prevent their joins, native accounting,
/// socket retirement, actor shutdown, process teardown or exact root removal.
#[tokio::test]
async fn budget_exposure_producer_panic_retires_live_http_siblings() {
    qualify(BudgetLimits::default(), true).await;
}

#[tokio::test]
async fn budget_wire_loopback_finite_install_drains_entire_unlimited_exposure_set() {
    for limits in [
        BudgetLimits {
            input: TokenLimit::Limited(1000),
            output: TokenLimit::Unlimited,
        },
        BudgetLimits {
            input: TokenLimit::Unlimited,
            output: TokenLimit::Limited(1000),
        },
    ] {
        qualify(limits, false).await;
    }
}
