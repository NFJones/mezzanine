//! Genuine task joins, including a blocking producer which cannot be stopped
//! merely by aborting its Tokio handle. No provider, process or filesystem I/O.

use super::super::ProviderProducerCompletion;
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn producer_join_survives_canceled_waiters_and_early_destruction_notice() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let producer = ProviderProducer::new(tokio::task::spawn_blocking(move || {
        count.fetch_add(1, Ordering::SeqCst);
        let _ = started.send(());
        gate.recv().unwrap();
        vec![11, 3]
    }));
    ready.await.unwrap();
    let (notice, completed) = tokio::sync::oneshot::channel();
    let completion = ProviderProducerCompletion::from(completed).with_join(producer.retirement());
    // Even a ready destruction signal is not the still-running task's join.
    notice.send(()).unwrap();
    {
        let observe = completion.observe();
        tokio::pin!(observe);
        assert!(futures_util::poll!(&mut observe).is_pending());
    }
    {
        let result = producer.result();
        tokio::pin!(result);
        assert!(futures_util::poll!(&mut result).is_pending());
    }
    producer.abort();
    assert!(
        !producer.is_finished(),
        "blocking work was not actually joined"
    );
    let sibling = ProviderProducer::new(tokio::spawn(async { 7 }));
    assert_eq!(sibling.result().await.unwrap().unwrap(), 7);
    release.send(()).unwrap();
    completion.observe().await;
    completion.observe().await;
    assert_eq!(producer.result().await.unwrap().unwrap(), vec![11, 3]);
    assert!(producer.result().await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn producer_join_preserves_original_panic_and_unpolled_cancellation() {
    let producer = ProviderProducer::new(tokio::spawn(async {
        panic!("fixture private provider panic");
    }));
    producer.retirement().observe().await;
    assert!(producer.result().await.unwrap().unwrap_err().is_panic());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let producer = ProviderProducer::new(tokio::spawn(async move {
        count.fetch_add(1, Ordering::SeqCst);
    }));
    producer.abort();
    producer.retirement().observe().await;
    assert!(producer.result().await.unwrap().unwrap_err().is_cancelled());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
