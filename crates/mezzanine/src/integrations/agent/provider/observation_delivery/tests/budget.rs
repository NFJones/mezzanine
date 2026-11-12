//! Cancellation and conflict fences for journal-owned checkpoint work. The
//! controllable receipt is an ordering oracle, not a simulated durable store;
//! the host's budget_wire tests separately use native SQLite and real actor ACK.

use super::*;
use std::sync::atomic::AtomicBool;

#[derive(Default)]
struct GateReceipt {
    ready: AtomicBool,
    wake: Notify,
    calls: AtomicUsize,
}
impl ProviderObservationReceipt for GateReceipt {
    fn checkpoint(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            loop {
                let wake = self.wake.notified();
                tokio::pin!(wake);
                wake.as_mut().enable();
                if self.ready.load(Ordering::Acquire) {
                    return Ok(());
                }
                wake.await;
            }
        })
    }
}
struct Factory(Arc<GateReceipt>);
impl ProviderObservationReceiptFactory for Factory {
    fn freeze(
        &self,
        _: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        Ok(self.0.clone())
    }
}

async fn observation() -> ProviderWireRequestObservation {
    let mut fixture = fixture();
    let provider = ObservedAsyncModelProvider::new(
        &fixture.provider,
        &fixture.observer,
        ProviderRequestPurpose::Execution,
    );
    let mut call = Box::pin(tokio::task::unconstrained(
        provider.send_request_async(&fixture.request),
    ));
    let observation = tokio::select! {
        biased;
        _ = &mut call => panic!("journal must wait for actor ACK"),
        observation = fixture.receiver.recv() => observation.unwrap(),
    };
    fixture.pending.acknowledge(&observation.request_id);
    call.await.unwrap();
    observation
}

#[tokio::test]
async fn budget_checkpoint_cancellation_and_commit_still_require_exact_actor_ack() {
    let mut fixture = fixture();
    let receipt = Arc::new(GateReceipt::default());
    fixture.observer = fixture
        .observer
        .with_budget_receipts(Arc::new(Factory(receipt.clone())))
        .unwrap();
    let provider = ObservedAsyncModelProvider::new(
        &fixture.provider,
        &fixture.observer,
        ProviderRequestPurpose::Execution,
    );
    let mut call = Box::pin(tokio::task::unconstrained(
        provider.send_request_async(&fixture.request),
    ));
    let observation = tokio::select! {
        biased;
        _ = &mut call => panic!("provider advanced before checkpoint"),
        observation = fixture.receiver.recv() => observation.unwrap(),
    };
    fixture.pending.acknowledge(&observation.request_id);
    assert_eq!(fixture.pending.len(), 1);
    {
        let checkpoint = fixture.pending.checkpoint(&observation);
        tokio::pin!(checkpoint);
        assert!(futures_util::poll!(&mut checkpoint).is_pending());
    }
    assert_eq!(receipt.calls.load(Ordering::SeqCst), 1);
    assert!(futures_util::poll!(&mut call).is_pending());
    receipt.ready.store(true, Ordering::Release);
    receipt.wake.notify_waiters();
    fixture.pending.checkpoint(&observation).await.unwrap();
    assert_eq!(
        fixture.pending.len(),
        1,
        "durable ACK is not actor adoption"
    );
    assert!(futures_util::poll!(&mut call).is_pending());
    let calls = receipt.calls.load(Ordering::SeqCst);
    fixture.pending.checkpoint(&observation).await.unwrap();
    assert_eq!(receipt.calls.load(Ordering::SeqCst), calls);
    fixture.pending.acknowledge("foreign-observation");
    assert_eq!(fixture.pending.len(), 1);
    fixture.pending.acknowledge(&observation.request_id);
    call.await.unwrap();
    assert!(fixture.pending.is_empty());
}

#[tokio::test]
async fn budget_checkpoint_conflict_keeps_original_receipt_and_refuses_actor_ack() {
    let original = observation().await;
    let pending = PendingProviderWireObservations::default();
    pending.require_budget_receipts().unwrap();
    let receipt = Arc::new(GateReceipt::default());
    let factory: Arc<dyn ProviderObservationReceiptFactory> = Arc::new(Factory(receipt.clone()));
    pending.retain_with_receipt(original.clone(), Some(&factory));
    let mut checkpoint = Box::pin(pending.checkpoint(&original));
    assert!(futures_util::poll!(&mut checkpoint).is_pending());
    let mut changed = original.clone();
    changed.usage.as_mut().unwrap().output_tokens += 1;
    pending.retain_with_receipt(changed, Some(&factory));
    receipt.ready.store(true, Ordering::Release);
    receipt.wake.notify_waiters();
    assert!(checkpoint.await.is_err());
    pending.acknowledge(&original.request_id);
    assert_eq!(pending.snapshot(), vec![original.clone()]);
    assert!(pending.checkpoint(&original).await.is_err());
    assert!(
        Arc::strong_count(&receipt) >= 3,
        "conflict must preserve original checkpoint custody"
    );
}

#[tokio::test]
async fn budget_checkpoint_missing_factory_cannot_become_unbudgeted() {
    let original = observation().await;
    let pending = PendingProviderWireObservations::default();
    pending.require_budget_receipts().unwrap();
    assert!(pending.checkpoint(&original).await.is_err());
    pending.retain(original.clone());
    assert!(pending.checkpoint(&original).await.is_err());
    pending.acknowledge(&original.request_id);
    assert_eq!(pending.snapshot(), vec![original]);
}

#[tokio::test]
async fn budget_checkpoint_is_retained_before_a_saturated_channel_can_cancel() {
    let filler = observation().await;
    let mut fixture = fixture();
    let receipt = Arc::new(GateReceipt::default());
    fixture.observer = fixture
        .observer
        .with_budget_receipts(Arc::new(Factory(receipt.clone())))
        .unwrap();
    while fixture.observer.sender.try_send(filler.clone()).is_ok() {}
    {
        let provider = ObservedAsyncModelProvider::new(
            &fixture.provider,
            &fixture.observer,
            ProviderRequestPurpose::Execution,
        );
        let mut call = Box::pin(tokio::task::unconstrained(
            provider.send_request_async(&fixture.request),
        ));
        assert!(futures_util::poll!(&mut call).is_pending());
        assert_eq!(fixture.pending.len(), 1);
        drop(call);
    }
    let original = fixture.pending.snapshot().pop().unwrap();
    assert_eq!(original.usage.unwrap().input_tokens, 17);
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    receipt.ready.store(true, Ordering::Release);
    fixture.pending.checkpoint(&original).await.unwrap();
    fixture.pending.acknowledge(&original.request_id);
    assert!(fixture.pending.is_empty());
}
