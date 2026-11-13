//! A genuine DeepSeek adapter-internal retry must own a distinct native attempt.
//! The first receipt and actor ACK precede even planning the second exchange.

use super::*;
use crate::host::async_runtime::client::provider_service::budget_admission::BudgetRequestAdmission;
use crate::integrations::agent::provider::{
    DeepSeekChatCompletionsProvider, ProviderObservationReceiptSource,
};
use mez_agent::token_budget::TokenLimit;

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
            index as i64,
            "retry allocated before prior durable expense"
        );
        self.inner.for_exchange(observation)
    }
}

struct RetryTransport {
    calls: Arc<AtomicUsize>,
    store: TokenUsageStore,
}

impl AsyncProviderHttpTransport for RetryTransport {
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
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(index < 2);
            assert_eq!(rows(&self.store, "budget_admissions"), (index + 1) as i64);
            assert_eq!(rows(&self.store, "budget_receipts"), index as i64);
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
            Ok(mez_agent::ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body:
                    serde_json::json!({"model":"deepseek-v4-pro", "choices":[{"message":message}],
                    "usage":{"prompt_tokens":10 + index * 2, "completion_tokens":4 + index * 2}})
                    .to_string(),
            })
        })
    }
}

#[tokio::test]
async fn budget_wire_internal_retry_has_distinct_native_custody_after_durable_ack() {
    qualify(BudgetLimits::default(), true).await;
}

/// Equality and one-token crossing independently exhaust either dimension.
/// A higher finite limit is also held here because receipt ACK is deliberately
/// NOT yet physical-retirement authority; this component must not release it.
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
        BudgetLimits {
            input: TokenLimit::limited(100).unwrap(),
            output: TokenLimit::Unlimited,
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
    let provider = DeepSeekChatCompletionsProvider::new(
        "fixture-key",
        RetryTransport {
            calls: calls.clone(),
            store: store.clone(),
        },
    )
    .unwrap();
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
        inner: BudgetRequestAdmission::new(
            dispatch.budget_owner.as_ref().unwrap().clone(),
            mez_agent::ModelTokenUsageKey::new(&request.provider, &request.model),
            BudgetAttemptPurpose::Execution,
        ),
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
        assert_eq!(rows(&store, "budget_receipts"), 1);
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
        let result = tokio::time::timeout(Duration::from_secs(10), client).await;
        handle.shutdown().await.unwrap();
        result
    };
    let (result, mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    drop((exit, observer, factory, dispatch, provider, handle, pending));
    root.cleanup();
    let (result, sends, receipts, exposure, known) = result.unwrap();
    if allowed {
        assert!(
            result.is_ok(),
            "internal retry lost original attempt custody: {result:?}"
        );
        assert_eq!((sends, receipts, exposure), (2, 2, 2));
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
        assert_eq!((sends, receipts, exposure), (1, 1, 1));
        assert_eq!(
            known,
            UsageTotals {
                input: 10,
                output: 4
            }
        );
    }
}
