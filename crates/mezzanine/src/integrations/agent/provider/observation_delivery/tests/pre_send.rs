//! Inspect the original journal from inside actual native transport polling.
//! Planned identities cannot be ACKed, and unknown expense survives cancellation.

use super::*;
use crate::integrations::agent::provider::{
    OpenAiCompatibleChatCompletionsProvider, OpenAiResponsesProvider,
};
use crate::integrations::http::AsyncProviderHttpTransport;

struct Transport {
    journal: PendingProviderWireObservations,
    captured: Arc<Mutex<Vec<ProviderWireRequestObservation>>>,
    interrupt: bool,
    responses: bool,
}

impl AsyncProviderHttpTransport for Transport {
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
            let planned = self.journal.snapshot();
            *self.captured.lock().unwrap() = planned.clone();
            for observation in planned {
                // A guessed/early actor ACK is not finalized evidence.
                self.journal.acknowledge(&observation.request_id);
            }
            if self.interrupt {
                return std::future::pending().await;
            }
            Ok(mez_agent::ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: if self.responses {
                    serde_json::json!({"model":"model","status":"completed",
                        "usage":{"input_tokens":11,"output_tokens":3},
                        "output_text":"{\"rationale\":\"fixture\",\"actions\":[{\"type\":\"say\",\"status\":\"final\",\"content_type\":\"text/plain; charset=utf-8\",\"text\":\"done\"}]}"}).to_string()
                } else {
                    serde_json::json!({"model":"model","usage":{"prompt_tokens":11,"completion_tokens":3},
                    "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant",
                    "content":"{\"rationale\":\"fixture\",\"actions\":[{\"type\":\"say\",\"status\":\"final\",\"content_type\":\"text/plain; charset=utf-8\",\"text\":\"done\"}]}"}}]}).to_string()
                },
            })
        })
    }
}

#[tokio::test]
async fn native_pre_send_identity_survives_completion_and_empty_cancellation() {
    for (responses, interrupt) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut fixture = fixture();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Transport {
            journal: fixture.pending.clone(),
            captured: captured.clone(),
            interrupt,
            responses,
        };
        if responses {
            let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
                "fixture-key",
                "https://example.test/responses",
                1000,
                Default::default(),
                false,
                transport,
            )
            .unwrap();
            fixture.request.provider = AsyncModelProvider::provider_id(&provider).into();
            check_native(&mut fixture, &provider, &captured, interrupt).await;
        } else {
            let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(transport)
                .unwrap()
                .with_provider_id("fixture")
                .unwrap();
            check_native(&mut fixture, &provider, &captured, interrupt).await;
        }
    }
}

async fn check_native(
    fixture: &mut Fixture,
    provider: &impl AsyncModelProvider,
    captured: &Arc<Mutex<Vec<ProviderWireRequestObservation>>>,
    interrupt: bool,
) {
    let observed = ObservedAsyncModelProvider::new(
        provider,
        &fixture.observer,
        ProviderRequestPurpose::Execution,
    );
    let mut call = Box::pin(tokio::task::unconstrained(
        observed.send_request_async(&fixture.request),
    ));
    assert!(
        fixture.pending.is_empty(),
        "unpolled calls create no attempt"
    );
    assert!(futures_util::poll!(&mut call).is_pending());
    let planned = captured.lock().unwrap().clone();
    assert_eq!(
        planned.len(),
        1,
        "transport began without original wire identity"
    );
    let planned = &planned[0];
    assert_eq!(planned.conversation_id, "conversation");
    assert_eq!(planned.attempt_index, 1);
    assert_eq!(planned.usage, None);
    assert!(fixture.pending.checkpoint(planned).await.is_err());
    assert_eq!(
        fixture.pending.len(),
        1,
        "early ACK removed original identity"
    );
    if interrupt {
        drop(call);
        let retained = fixture.pending.snapshot();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].request_id, planned.request_id);
        assert_eq!(retained[0].usage, None);
        assert_eq!(retained[0].failure_kind.as_deref(), Some("interrupted"));
        assert_eq!(fixture.reported.lock().unwrap().known_observations, 0);
        fixture.pending.checkpoint(&retained[0]).await.unwrap();
        fixture.pending.acknowledge(&planned.request_id);
    } else {
        let completed = fixture.receiver.try_recv().unwrap();
        assert_eq!(completed.request_id, planned.request_id);
        assert_eq!(fixture.pending.snapshot(), vec![completed.clone()]);
        assert_eq!(completed.usage.unwrap().input_tokens, 11);
        fixture.pending.checkpoint(&completed).await.unwrap();
        fixture.pending.acknowledge(&completed.request_id);
        call.await.unwrap();
        assert_eq!(fixture.reported.lock().unwrap().known_observations, 1);
    }
    assert!(fixture.pending.is_empty());
}

struct CountingFactory(Arc<AtomicUsize>);
struct Receipt;
#[tokio::test]
async fn required_plan_without_factory_never_polls_transport() {
    let fixture = fixture();
    fixture.pending.require_budget_receipts().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(Transport {
        journal: fixture.pending.clone(),
        captured: captured.clone(),
        interrupt: true,
        responses: false,
    })
    .unwrap()
    .with_provider_id("fixture")
    .unwrap();
    let observed = ObservedAsyncModelProvider::new(
        &provider,
        &fixture.observer,
        ProviderRequestPurpose::Execution,
    );
    let mut call = Box::pin(tokio::task::unconstrained(
        observed.send_request_async(&fixture.request),
    ));
    assert!(matches!(
        futures_util::poll!(&mut call),
        std::task::Poll::Ready(Err(_))
    ));
    assert!(captured.lock().unwrap().is_empty());
    assert!(fixture.pending.is_empty());
    assert_eq!(fixture.reported.lock().unwrap().known_observations, 0);
}

impl ProviderObservationReceipt for Receipt {
    fn checkpoint(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}
impl ProviderObservationReceiptFactory for CountingFactory {
    fn freeze(
        &self,
        _: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(Receipt))
    }
}

async fn completed_evidence() -> ProviderWireRequestObservation {
    let mut fixture = fixture();
    let observed = ObservedAsyncModelProvider::new(
        &fixture.provider,
        &fixture.observer,
        ProviderRequestPurpose::Execution,
    );
    let (_, mut delivered) =
        crate::integrations::agent::provider::observe_and_acknowledge_for_test(
            observed.send_request_async(&fixture.request),
            &mut fixture.receiver,
            &fixture.pending,
        )
        .await;
    delivered.pop().unwrap()
}

fn plan(completed: &ProviderWireRequestObservation) -> ProviderWireRequestObservation {
    let mut planned = completed.clone();
    planned.usage = None;
    planned.usage_presence = Some(Default::default());
    planned.succeeded = false;
    planned.failure_kind = Some("interrupted".into());
    planned
}

#[tokio::test]
async fn planned_subject_and_factory_conflicts_refuse_before_native_receipt_capture() {
    let completed = completed_evidence().await;
    type Change = fn(&mut ProviderWireRequestObservation);
    let changes: [Change; 7] = [
        |o| o.conversation_id.push_str("-foreign"),
        |o| o.model.push_str("-foreign"),
        |o| o.provider.push_str("-foreign"),
        |o| o.purpose = ProviderRequestPurpose::Memory,
        |o| o.attempt_index += 1,
        |o| o.retry_reason = Some("late-retry".into()),
        |o| o.claim_generation = Some(999),
    ];
    for change in changes {
        let pending = PendingProviderWireObservations::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let factory: Arc<dyn ProviderObservationReceiptFactory> =
            Arc::new(CountingFactory(calls.clone()));
        let planned = plan(&completed);
        pending.plan(planned.clone(), Some(&factory)).unwrap();
        assert!(pending.plan(planned.clone(), Some(&factory)).is_err());
        let mut changed = completed.clone();
        change(&mut changed);
        assert!(!pending.retain_with_receipt(changed, Some(&factory)));
        assert!(!pending.retain_with_receipt(completed.clone(), Some(&factory)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(pending.checkpoint(&completed).await.is_err());
        pending.acknowledge(&planned.request_id);
        assert_eq!(pending.snapshot(), vec![planned]);
    }
    let pending = PendingProviderWireObservations::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let original: Arc<dyn ProviderObservationReceiptFactory> =
        Arc::new(CountingFactory(calls.clone()));
    let replacement: Arc<dyn ProviderObservationReceiptFactory> =
        Arc::new(CountingFactory(calls.clone()));
    let planned = plan(&completed);
    pending.plan(planned.clone(), Some(&original)).unwrap();
    assert!(!pending.retain_with_receipt(completed, Some(&replacement)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    pending.acknowledge(&planned.request_id);
    assert_eq!(pending.snapshot(), vec![planned]);
}

#[tokio::test]
async fn finalized_plan_requires_checkpoint_and_duplicate_capture_is_inert() {
    let completed = completed_evidence().await;
    let pending = PendingProviderWireObservations::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn ProviderObservationReceiptFactory> =
        Arc::new(CountingFactory(calls.clone()));
    let planned = plan(&completed);
    pending.plan(planned.clone(), Some(&factory)).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(pending.checkpoint(&planned).await.is_err());
    pending.acknowledge(&planned.request_id);
    assert_eq!(pending.len(), 1);
    assert!(pending.retain_with_receipt(completed.clone(), Some(&factory)));
    assert!(!pending.retain_with_receipt(completed.clone(), Some(&factory)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    pending.acknowledge(&completed.request_id);
    assert_eq!(pending.len(), 1);
    pending.checkpoint(&completed).await.unwrap();
    pending.acknowledge(&completed.request_id);
    assert!(pending.is_empty());
}
