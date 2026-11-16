//! A genuine DeepSeek adapter-internal retry must own a distinct native attempt.
//! The first receipt and actor ACK precede even planning the second exchange.

use super::*;
use crate::host::async_runtime::client::provider_service::budget_admission::BudgetRequestAdmission;
use crate::integrations::agent::provider::{
    DeepSeekChatCompletionsProvider, ProviderObservationReceiptSource,
};
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Observe allocation order without introducing another custody owner. The
/// production source still creates and retains every native descriptor.
struct CountedSource {
    inner: BudgetRequestAdmission,
    allocations: AtomicUsize,
    store: TokenUsageStore,
}

impl ProviderObservationReceiptSource for CountedSource {
    fn for_exchange(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceiptFactory>> {
        let index = self.allocations.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            rows(&self.store, "budget_receipts"),
            (index * 2) as i64,
            "retry allocated before prior durable expense"
        );
        self.inner.for_exchange(observation)
    }
}

/// Count complete physical submissions, not adapter calls. One owned task
/// retains every socket and is aborted/joined before fixture cleanup.
async fn serve(
    listener: tokio::net::TcpListener,
    calls: Arc<AtomicUsize>,
    store: TokenUsageStore,
    root: mez_agent::token_budget::BudgetRootId,
) -> Result<()> {
    loop {
        let (mut socket, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0; 8192];
            let count = socket.read(&mut buffer).await?;
            if count == 0 || bytes.len() + count > 1024 * 1024 {
                return Err(MezError::invalid_state(
                    "internal retry HTTP request incomplete or oversized",
                ));
            }
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length: usize = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                    .ok_or_else(|| {
                        MezError::invalid_state("internal retry request length unavailable")
                    })?
                    .1
                    .trim()
                    .parse()
                    .map_err(|_| {
                        MezError::invalid_state("internal retry request length invalid")
                    })?;
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let index = calls.fetch_add(1, Ordering::SeqCst);
        assert!(index < 2);
        assert_eq!(rows(&store, "budget_admissions"), (index + 1) as i64);
        assert_eq!(rows(&store, "budget_receipts"), (index * 2) as i64);
        assert_eq!(
            store.budget_account(&root)?.known,
            UsageTotals {
                input: index as u64 * 10,
                output: index as u64 * 4,
            }
        );
        let outstanding: i64 = rusqlite::Connection::open(store.path())?.query_row(
            "SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NULL",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            outstanding, 1,
            "internal retry preceded original retirement"
        );
        let message = if index == 0 {
            serde_json::json!({"role":"assistant", "content":"I can help.", "reasoning_content":"thinking"})
        } else {
            serde_json::json!({"role":"assistant", "content":"", "tool_calls":[{
                "id":"call_1", "type":"function", "function":{
                    "name":mez_agent::DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME,
                    "arguments":serde_json::json!({"rationale":"fixture","status":"final","text":"done"}).to_string()
                }
            }]})
        };
        let body = serde_json::json!({"model":"deepseek-v4-pro", "choices":[{"message":message}],
                "usage":{"prompt_tokens":10 + index * 2, "completion_tokens":4 + index * 2}})
        .to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
    }
}

#[tokio::test]
async fn budget_wire_internal_retry_has_distinct_native_custody_after_durable_ack() {
    qualify(BudgetLimits::default(), true).await;
    qualify(
        BudgetLimits {
            input: TokenLimit::Limited(100),
            output: TokenLimit::Unlimited,
        },
        true,
    )
    .await;
}

/// Equality and one-token crossing independently exhaust either dimension.
/// Physical retirement alone never permits a retry after known usage exhausts
/// either dimension; raising the limit must still preserve original expense.
#[tokio::test]
async fn budget_wire_internal_retry_refusal_preserves_first_expense_and_exposure() {
    for limits in [
        BudgetLimits {
            input: TokenLimit::limited(10).unwrap(),
            output: TokenLimit::Unlimited,
        },
        BudgetLimits {
            input: TokenLimit::limited(9).unwrap(),
            output: TokenLimit::Unlimited,
        },
        BudgetLimits {
            input: TokenLimit::Unlimited,
            output: TokenLimit::limited(4).unwrap(),
        },
        BudgetLimits {
            input: TokenLimit::Unlimited,
            output: TokenLimit::limited(3).unwrap(),
        },
    ] {
        qualify(limits, false).await;
    }
}

async fn qualify(limits: BudgetLimits, allowed: bool) {
    let Prepared {
        service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(limits).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
    let mut server = tokio::spawn(serve(
        listener,
        calls.clone(),
        store.clone(),
        member.root.clone(),
    ));
    let provider = DeepSeekChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
        .unwrap()
        .with_endpoint(endpoint)
        .with_stream(false);
    let mut request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    request.provider = "deepseek".into();
    request.model = "deepseek-v4-pro".into();
    request.reasoning_effort = Some("high".into());
    request.allowed_actions =
        mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::RespondOnly);
    let factory = Arc::new(CountedSource {
        inner: BudgetRequestAdmission::new(dispatch.budget_owner.as_ref().unwrap().clone()),
        allocations: AtomicUsize::new(0),
        store: store.clone(),
    });
    let pending = PendingProviderWireObservations::default();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
        .with_claim_generation(7)
        .with_pending_observations(pending.clone())
        .with_budget_receipt_source(factory.clone())
        .unwrap();
    let observed =
        ObservedAsyncModelProvider::new(&provider, &observer, ProviderRequestPurpose::Execution);
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        let first = tokio::select! {
            biased;
            _ = &mut call => panic!("first exchange bypassed receipt ACK"),
            observation = receiver.recv() => observation.unwrap(),
        };
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(rows(&store, "budget_receipts"), 0);
        assert!(futures_util::poll!(&mut call).is_pending());
        assert_eq!(factory.allocations.load(Ordering::SeqCst), 1);
        // Even positive durable settlement without actor adoption must not
        // allocate the next original descriptor or poll its transport.
        pending.checkpoint(&first).await.unwrap();
        assert_eq!(rows(&store, "budget_receipts"), 2);
        assert!(futures_util::poll!(&mut call).is_pending());
        assert_eq!(factory.allocations.load(Ordering::SeqCst), 1);
        submit_pending_provider_observation(&handle, &pending, first.clone())
            .await
            .unwrap();
        let result = tokio::select! {
            biased;
            result = &mut call => result,
            observation = receiver.recv() => {
                let second = observation.unwrap();
                assert_ne!(first.request_id, second.request_id);
                assert_eq!(second.attempt_index, 2);
                assert_eq!(second.retry_reason.as_deref(), Some("provider_forced_maap"));
                submit_pending_provider_observation(&handle, &pending, second).await.unwrap();
                call.await
            }
        };
        assert_eq!(factory.allocations.load(Ordering::SeqCst), 2);
        // Held Chat admission returns synchronously through stack custody,
        // not the wire channel. Retire only its positively absent receipt;
        // never fabricate another accounting event or release first exposure.
        if !allowed {
            let unknown = pending.snapshot().pop().unwrap();
            assert_eq!(unknown.attempt_index, 2);
            assert!(unknown.usage.is_none());
            pending.checkpoint(&unknown).await.unwrap();
            pending.acknowledge(&unknown.request_id);
        }
        assert!(pending.is_empty());
        (
            result,
            calls.load(Ordering::SeqCst),
            rows(&store, "budget_receipts"),
            rows(&store, "budget_admissions"),
            store.budget_account(&member.root).unwrap().known,
        )
    };
    let control = async {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            std::panic::AssertUnwindSafe(client).catch_unwind(),
        )
        .await;
        server.abort();
        let server_result = tokio::time::timeout(Duration::from_secs(5), &mut server).await;
        handle.shutdown().await.unwrap();
        (result, server_result)
    };
    let ((result, server_result), mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    drop((exit, observer, factory, dispatch, provider, handle, pending));
    assert!(
        matches!(server_result, Ok(Err(error)) if error.is_cancelled()),
        "original HTTP server did not retire cleanly"
    );
    let (result, sends, receipts, exposure, known) = result
        .unwrap()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    if allowed {
        assert!(
            result.is_ok(),
            "internal retry lost original attempt custody: {result:?}"
        );
        assert_eq!((sends, receipts, exposure), (2, 4, 2));
        assert_eq!(
            known,
            UsageTotals {
                input: 22,
                output: 10
            }
        );
    } else {
        let error = result.unwrap_err();
        assert_eq!(error.kind(), MezErrorKind::BudgetSuspended);
        assert_eq!(error.provider_incurred_usage().input_tokens, 10);
        assert_eq!(error.provider_incurred_usage().output_tokens, 4);
        assert_eq!((sends, receipts, exposure), (1, 2, 1));
        assert_eq!(
            known,
            UsageTotals {
                input: 10,
                output: 4
            }
        );
    }
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)", [], |row| row.get(0)).unwrap();
    assert_eq!(outstanding, 0);
    drop((store, server));
    root.cleanup();
}
