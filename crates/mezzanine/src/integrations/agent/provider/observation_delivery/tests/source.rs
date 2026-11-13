//! The original journal, not a final result, selects a per-exchange factory.
//! Pure cancellation controls supplement the native SQLite/actor retry tests.

use super::*;
use crate::integrations::agent::provider::{
    ProviderWireObservationContext, stream_usage_custody::StreamUsageCustody,
};

#[derive(Default)]
struct Source {
    allocations: AtomicUsize,
    freezes: Arc<AtomicUsize>,
    authorizations: Arc<AtomicUsize>,
}

struct Factory {
    subject: ProviderWireRequestObservation,
    freezes: Arc<AtomicUsize>,
    authorizations: Arc<AtomicUsize>,
}

struct Receipt;

impl ProviderObservationReceipt for Receipt {
    fn checkpoint(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

impl ProviderObservationReceiptSource for Source {
    fn for_exchange(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceiptFactory>> {
        self.allocations.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(Factory {
            subject: observation.clone(),
            freezes: self.freezes.clone(),
            authorizations: self.authorizations.clone(),
        }))
    }
}

impl ProviderObservationReceiptFactory for Factory {
    fn before_send<'a>(
        &'a self,
        observation: &'a ProviderWireRequestObservation,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            assert!(self.subject.same_wire_subject(observation));
            self.authorizations.fetch_add(1, Ordering::SeqCst);
            std::future::pending().await
        })
    }

    fn freeze(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        assert!(self.subject.same_wire_subject(observation));
        self.freezes.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(Receipt))
    }
}

#[tokio::test]
async fn per_exchange_source_cancel_and_duplicate_use_original_factory() {
    qualify_cancel_and_duplicate(false).await;
    qualify_cancel_and_duplicate(true).await;
}

async fn qualify_cancel_and_duplicate(newer_plan: bool) {
    let mut fixture = fixture();
    let source = Arc::new(Source::default());
    fixture.observer = fixture
        .observer
        .with_budget_receipt_source(source.clone())
        .unwrap();
    let context = ProviderWireObservationContext::new(
        &fixture.observer,
        ProviderRequestPurpose::Execution,
        "fixture".into(),
        Ok(None),
    );
    let custody = StreamUsageCustody::new(&context, &fixture.request).unwrap();
    {
        let mut authorization = Box::pin(custody.before_send());
        assert!(futures_util::poll!(&mut authorization).is_pending());
    }
    assert_eq!(source.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(source.authorizations.load(Ordering::SeqCst), 1);
    assert_eq!(source.freezes.load(Ordering::SeqCst), 0);
    drop(custody);
    let original = fixture.pending.snapshot().pop().unwrap();
    assert!(original.usage.is_none());
    assert!(context.planned_wire_id().is_none());
    assert_eq!(source.freezes.load(Ordering::SeqCst), 1);
    // Model an older retained finalizer alongside a newer context plan. This
    // is a custody control, not permission to bypass the adapter's ACK barrier.
    let next = newer_plan.then(|| {
        StreamUsageCustody::for_attempt(&context, &fixture.request, 2, Some("fixture")).unwrap()
    });
    let next_id = context.planned_wire_id();
    // The plan has transferred custody. Exact duplicate finalization must
    // recover ONLY the same retained factory, not conflict or allocate again.
    context.retain_wire_observation(&original);
    fixture.pending.checkpoint(&original).await.unwrap();
    fixture.pending.acknowledge(&original.request_id);
    assert_eq!(context.planned_wire_id(), next_id);
    drop(next);
    if newer_plan {
        let second = fixture.pending.snapshot().pop().unwrap();
        assert_eq!(Some(second.request_id.clone()), next_id);
        fixture.pending.checkpoint(&second).await.unwrap();
        fixture.pending.acknowledge(&second.request_id);
    }
    assert!(fixture.pending.is_empty());
    let attempts = if newer_plan { 2 } else { 1 };
    assert_eq!(source.allocations.load(Ordering::SeqCst), attempts);
    assert_eq!(source.freezes.load(Ordering::SeqCst), attempts);
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn per_exchange_source_requires_empty_journal_and_unique_observer_owner() {
    let source = Arc::new(Source::default());
    let (sender, _) = tokio::sync::mpsc::channel(1);
    assert!(
        ProviderWireRequestObserver::new("conversation", "%1", sender)
            .with_budget_receipt_source(source.clone())
            .is_err()
    );
    let fixture = super::fixture();
    let observer = fixture
        .observer
        .with_budget_receipt_source(source.clone())
        .unwrap();
    assert!(observer.with_budget_receipt_source(source.clone()).is_err());
    let fixture = super::fixture();
    let observer = fixture
        .observer
        .with_budget_receipt_source(source.clone())
        .unwrap();
    let factory = Arc::new(Factory {
        subject: ProviderWireObservationContext::new(
            &observer,
            ProviderRequestPurpose::Execution,
            "fixture".into(),
            Ok(None),
        )
        .build_wire_observation(
            &fixture.request,
            1,
            None,
            crate::integrations::agent::provider::FinalWireObservation {
                final_wire_diagnostics: None,
                response_diagnostics: None,
                elapsed_ms: None,
                usage_evidence: crate::integrations::agent::provider::WireUsageEvidence::Concrete(
                    None,
                ),
                usage_presence: Some(Default::default()),
            },
            None,
            false,
            Some("interrupted".into()),
        ),
        freezes: source.freezes.clone(),
        authorizations: source.authorizations.clone(),
    });
    assert!(observer.with_budget_receipts(factory.clone()).is_err());
    let fixture = super::fixture();
    let observer = fixture.observer.with_budget_receipts(factory).unwrap();
    assert!(observer.with_budget_receipt_source(source.clone()).is_err());
    let fixture = super::fixture();
    let context = ProviderWireObservationContext::new(
        &fixture.observer,
        ProviderRequestPurpose::Execution,
        "fixture".into(),
        Ok(None),
    );
    context
        .plan_wire_attempt(&fixture.request, 1, None)
        .unwrap();
    drop(context);
    assert!(
        fixture
            .observer
            .with_budget_receipt_source(source.clone())
            .is_err()
    );
    assert_eq!(source.allocations.load(Ordering::SeqCst), 0);
}
