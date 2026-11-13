//! Exercise the exact boxed future's destruction, not a task-status proxy.

use super::*;
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Counts {
    dropped: AtomicUsize,
    retired: AtomicUsize,
    panic_callback: AtomicBool,
}

struct Factory(Arc<Counts>);
impl ProviderObservationReceiptFactory for Factory {
    fn transport_retired(
        &self,
        observation: &ProviderWireRequestObservation,
        proof: ProviderTransportRetirement,
    ) {
        assert!(proof.matches(observation));
        assert_eq!(self.0.dropped.load(Ordering::SeqCst), 1);
        self.0.retired.fetch_add(1, Ordering::SeqCst);
        assert!(
            !self.0.panic_callback.load(Ordering::SeqCst),
            "private fixture retirement panic"
        );
    }
    fn freeze(
        &self,
        _: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        Ok(Arc::new(Receipt))
    }
}

struct Receipt;
impl ProviderObservationReceipt for Receipt {
    fn checkpoint(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

struct Transport {
    counts: Arc<Counts>,
    ready: bool,
    panic_drop: bool,
}
impl Future for Transport {
    type Output = u8;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<u8> {
        if self.ready {
            Poll::Ready(7)
        } else {
            Poll::Pending
        }
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::SeqCst);
        assert!(!self.panic_drop, "fixture destruction failed");
    }
}

fn owner(counts: Arc<Counts>) -> WireTransportOwner {
    let (sender, _) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new("conversation", "%1", sender)
        .with_pending_observations(Default::default())
        .with_budget_receipts(Arc::new(Factory(counts)))
        .unwrap();
    let request = mez_agent::epoch_compaction::provider::request_from_source(
        &mez_agent::ModelProfile {
            provider: "fixture".into(),
            model: "model".into(),
            ..Default::default()
        },
        "%1",
        "conversation",
        "fixture".into(),
        mez_agent::AllowedActionSet::say_only(),
    );
    let context = ProviderWireObservationContext::new(
        &observer,
        ProviderRequestPurpose::Execution,
        "fixture".into(),
        Ok(None),
    );
    context.plan_wire_attempt(&request, 1, None).unwrap();
    let owner = context.transport_owner().unwrap();
    // A second wrapper cannot borrow this same plan's retirement authority.
    assert!(context.transport_owner().is_err());
    owner
}

#[tokio::test]
async fn wire_transport_witness_follows_actual_future_drop_once() {
    for (ready, poll) in [(false, false), (false, true), (true, true)] {
        let counts = Arc::new(Counts::default());
        let mut future = Box::pin(WireTransport::new(
            Box::pin(Transport {
                counts: counts.clone(),
                ready,
                panic_drop: false,
            }),
            Some(owner(counts.clone())),
        ));
        if ready {
            assert_eq!(future.as_mut().await, 7);
            assert_eq!(counts.retired.load(Ordering::SeqCst), 1);
        } else if poll {
            assert!(futures_util::poll!(&mut future).is_pending());
            assert_eq!(counts.retired.load(Ordering::SeqCst), 0);
        }
        drop(future);
        assert_eq!(counts.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(counts.retired.load(Ordering::SeqCst), 1);
    }
}

/// A panic after partial callback transfer must remain local and keep the first
/// original expense, while forbidding receipt checkpoint/ACK and any replay.
#[tokio::test]
async fn wire_transport_consumer_panic_keeps_original_expense_and_fences_ack() {
    let counts = Arc::new(Counts::default());
    counts.panic_callback.store(true, Ordering::SeqCst);
    let owner = owner(counts.clone());
    let pending = owner.pending.as_ref().unwrap().clone();
    let factory = owner.factory.clone().unwrap();
    let mut observation = owner.observation.clone();
    let future = WireTransport::new(
        Box::pin(Transport {
            counts: counts.clone(),
            ready: false,
            panic_drop: false,
        }),
        Some(owner),
    );
    drop(future);
    observation.usage = Some(mez_agent::ModelTokenUsage {
        input_tokens: 17,
        ..Default::default()
    });
    observation.usage_presence = Some(mez_agent::ModelTokenUsagePresence {
        input: true,
        output: false,
    });
    assert!(pending.retain_with_receipt(observation.clone(), Some(&factory)));
    assert_eq!(pending.snapshot(), vec![observation.clone()]);
    assert!(pending.checkpoint(&observation).await.is_err());
    pending.acknowledge(&observation.request_id);
    assert_eq!(pending.snapshot(), vec![observation]);
    assert_eq!(counts.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(counts.retired.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn wire_transport_panicking_destruction_cannot_attest_retirement() {
    for ready in [false, true] {
        let counts = Arc::new(Counts::default());
        let mut future = Box::pin(WireTransport::new(
            Box::pin(Transport {
                counts: counts.clone(),
                ready,
                panic_drop: true,
            }),
            Some(owner(counts.clone())),
        ));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if ready {
                let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
                let _ = future.as_mut().poll(&mut cx);
            } else {
                drop(future);
            }
        }));
        assert!(result.is_err());
        assert_eq!(counts.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(counts.retired.load(Ordering::SeqCst), 0);
    }
}
