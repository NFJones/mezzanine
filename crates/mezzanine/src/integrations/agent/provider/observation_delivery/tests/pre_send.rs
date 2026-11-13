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
/// A refused finalization must leave stack custody armed on its original ID.
#[test]
fn rejected_finalization_does_not_publish_a_context_handoff() {
    let fixture = fixture();
    let context = crate::integrations::agent::provider::ProviderWireObservationContext::new(
        &fixture.observer,
        ProviderRequestPurpose::Execution,
        "fixture".into(),
        Ok(None),
    );
    context
        .plan_wire_attempt(&fixture.request, 1, None)
        .unwrap();
    let original = fixture.pending.snapshot().pop().unwrap();
    let mut changed = original.clone();
    changed.model.push_str("-foreign");
    context.retain_wire_observation(&changed);
    assert_eq!(context.planned_wire_id(), Some(original.request_id));
    assert_eq!(context.retained_observations.load(Ordering::Acquire), 0);
    assert_eq!(fixture.reported.lock().unwrap().known_observations, 0);
}

#[tokio::test]
async fn rejected_subject_keeps_original_drop_expense_without_clearing_conflict() {
    let fixture = fixture();
    let context = crate::integrations::agent::provider::ProviderWireObservationContext::new(
        &fixture.observer,
        ProviderRequestPurpose::Execution,
        "fixture".into(),
        Ok(None),
    );
    let mut custody =
        crate::integrations::agent::provider::stream_usage_custody::StreamUsageCustody::new(
            &context,
            &fixture.request,
        )
        .unwrap();
    let original = fixture.pending.snapshot().pop().unwrap();
    let usage = mez_agent::ModelTokenUsage {
        input_tokens: 17,
        output_tokens: 5,
        ..Default::default()
    };
    custody.update_with_presence(
        usage,
        mez_agent::ModelTokenUsagePresence {
            input: true,
            output: true,
        },
    );
    let mut changed = original.clone();
    changed.model.push_str("-foreign");
    context.retain_wire_observation(&changed);
    assert_eq!(context.planned_wire_id(), Some(original.request_id.clone()));
    drop(custody);
    let retained = fixture.pending.snapshot().pop().unwrap();
    assert_eq!(retained.request_id, original.request_id);
    assert_eq!(retained.model, original.model);
    assert_eq!(retained.usage, Some(usage));
    assert_eq!(fixture.reported.lock().unwrap().known_observations, 1);
    assert!(fixture.pending.checkpoint(&retained).await.is_err());
    fixture.pending.acknowledge(&retained.request_id);
    assert_eq!(fixture.pending.snapshot(), vec![retained]);
}

/// Capture deliberately reads the journal while blocked: the map mutex must
/// remain available, and an identical competing finalizer must reuse this work.
struct BlockingFactory {
    calls: Arc<AtomicUsize>,
    started: std::sync::mpsc::Sender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
    journal: std::sync::Weak<Journal>,
}

impl ProviderObservationReceiptFactory for BlockingFactory {
    fn freeze(
        &self,
        _: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let journal = PendingProviderWireObservations(self.journal.upgrade().unwrap());
        assert_eq!(journal.len(), 1);
        drop(journal);
        self.started.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        Ok(Arc::new(Receipt))
    }
}

#[tokio::test]
async fn concurrent_finalization_captures_one_original_receipt() {
    for planned in [true, false] {
        qualify_concurrent_finalization(planned).await;
    }
}

async fn qualify_concurrent_finalization(planned: bool) {
    let completed = completed_evidence().await;
    let pending = PendingProviderWireObservations::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let (started, ready) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let factory: Arc<dyn ProviderObservationReceiptFactory> = Arc::new(BlockingFactory {
        calls: calls.clone(),
        started,
        release: Mutex::new(blocked),
        journal: Arc::downgrade(&pending.0),
    });
    if planned {
        pending.plan(plan(&completed), Some(&factory)).unwrap();
    }
    let original = pending.clone();
    let first_factory = factory.clone();
    let first_observation = completed.clone();
    let first = std::thread::spawn(move || {
        original.retain_with_receipt(first_observation, Some(&first_factory))
    });
    ready
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let competing = pending.clone();
    let second_factory = factory.clone();
    let second_observation = completed.clone();
    let second = std::thread::spawn(move || {
        competing.retain_with_receipt(second_observation, Some(&second_factory))
    });
    // On the defective implementation this second callback announces itself
    // immediately. On the repaired implementation it waits on original capture.
    let duplicate = ready
        .recv_timeout(std::time::Duration::from_millis(100))
        .is_ok();
    release.send(()).unwrap();
    if duplicate {
        release.send(()).unwrap();
    }
    let retained = usize::from(first.join().unwrap()) + usize::from(second.join().unwrap());
    assert!(
        !duplicate,
        "a competing finalizer invoked receipt capture twice"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(retained, 1);
    pending.checkpoint(&completed).await.unwrap();
    pending.acknowledge(&completed.request_id);
    assert!(pending.is_empty());
}

struct PanickingFactory(Arc<AtomicUsize>);
impl ProviderObservationReceiptFactory for PanickingFactory {
    fn freeze(
        &self,
        _: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("PRIVATE_CAPTURE_FAULT");
    }
}

#[tokio::test]
async fn capture_panic_retains_final_evidence_and_a_sticky_refusal() {
    let completed = completed_evidence().await;
    let pending = PendingProviderWireObservations::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn ProviderObservationReceiptFactory> =
        Arc::new(PanickingFactory(calls.clone()));
    pending.plan(plan(&completed), Some(&factory)).unwrap();
    assert!(pending.retain_with_receipt(completed.clone(), Some(&factory)));
    assert!(!pending.retain_with_receipt(completed.clone(), Some(&factory)));
    let error = pending.checkpoint(&completed).await.unwrap_err();
    assert!(!error.message().contains("PRIVATE_CAPTURE"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    pending.acknowledge(&completed.request_id);
    assert_eq!(pending.snapshot(), vec![completed]);
    assert!(Arc::strong_count(&factory) >= 2);
}

#[tokio::test]
async fn late_capture_cannot_resurrect_or_finalize_a_successor_plan() {
    let completed = completed_evidence().await;
    let pending = PendingProviderWireObservations::default();
    pending.plan(plan(&completed), None).unwrap();
    assert!(pending.retain_with_receipt(completed.clone(), None));
    let capture = pending
        .entries()
        .get(&completed.request_id)
        .unwrap()
        .capture
        .clone()
        .unwrap();
    let budget = capture.budget.get().unwrap().clone();
    pending.acknowledge(&completed.request_id);
    assert!(!pending.finish_capture(&capture, budget.clone()));
    assert!(pending.is_empty());
    let successor = plan(&completed);
    pending.plan(successor.clone(), None).unwrap();
    assert!(!pending.finish_capture(&capture, budget));
    assert_eq!(pending.snapshot(), vec![successor]);
    assert!(pending.checkpoint(&completed).await.is_err());
    assert!(pending.retain_with_receipt(completed.clone(), None));
    pending.acknowledge(&completed.request_id);
    assert!(pending.is_empty());
}

#[tokio::test]
async fn conflicting_evidence_during_capture_preserves_original_expense_and_fence() {
    let completed = completed_evidence().await;
    let pending = PendingProviderWireObservations::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let (started, ready) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let factory: Arc<dyn ProviderObservationReceiptFactory> = Arc::new(BlockingFactory {
        calls: calls.clone(),
        started,
        release: Mutex::new(blocked),
        journal: Arc::downgrade(&pending.0),
    });
    pending.plan(plan(&completed), Some(&factory)).unwrap();
    let original = pending.clone();
    let first_factory = factory.clone();
    let first_observation = completed.clone();
    let first = std::thread::spawn(move || {
        original.retain_with_receipt(first_observation, Some(&first_factory))
    });
    ready
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let mut changed = completed.clone();
    changed.usage.as_mut().unwrap().output_tokens += 1;
    assert!(!pending.retain_with_receipt(changed, Some(&factory)));
    release.send(()).unwrap();
    assert!(first.join().unwrap());
    assert_eq!(pending.snapshot(), vec![completed.clone()]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(pending.checkpoint(&completed).await.is_err());
    pending.acknowledge(&completed.request_id);
    assert_eq!(pending.len(), 1);
}

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
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(pending.retain_with_receipt(completed.clone(), Some(&factory)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(pending.checkpoint(&completed).await.is_err());
        pending.acknowledge(&planned.request_id);
        assert_eq!(pending.snapshot(), vec![completed.clone()]);
    }
    let pending = PendingProviderWireObservations::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let original: Arc<dyn ProviderObservationReceiptFactory> =
        Arc::new(CountingFactory(calls.clone()));
    let replacement: Arc<dyn ProviderObservationReceiptFactory> =
        Arc::new(CountingFactory(calls.clone()));
    let planned = plan(&completed);
    pending.plan(planned.clone(), Some(&original)).unwrap();
    assert!(!pending.retain_with_receipt(completed.clone(), Some(&replacement)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(pending.retain_with_receipt(completed.clone(), Some(&original)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(pending.checkpoint(&completed).await.is_err());
    pending.acknowledge(&planned.request_id);
    assert_eq!(pending.snapshot(), vec![completed]);
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
