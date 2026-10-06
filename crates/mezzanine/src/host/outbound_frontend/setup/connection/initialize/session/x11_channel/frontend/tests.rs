//! Concurrent reservation identity and capacity without remote application work.
//!
//! Barrier-released threads share one source and retain every returned permit.
//! Synthetic route evidence tests allocation only, not application authority.

use super::*;

/// Simultaneous reservations allocate distinct occurrences from one shared
/// watermark, retaining capacity until disposal. Saturation and exhaustion
/// cannot advance the watermark or leak permits. Closing the parent lease
/// prevents a retained source from authorizing additional relay work.
#[tokio::test]
async fn outbound_x11_source_concurrent_reservations_share_identity_and_capacity() {
    let (root, owner, server, lease, peer, sibling, sibling_peer) =
        super::super::tests::fixture().await;
    let slots = Arc::new(Semaphore::new(8));
    let occurrence = Arc::new(AtomicU64::new(0));
    let source = Arc::new(X11RelaySource {
        endpoint: owner.clone(),
        connection: lease.connection().clone(),
        route: super::super::tests::route(),
        cookie: crate::runtime::x11::X11Cookie::new([17; 16]),
        compression: IrohCompressionPolicy::new(
            crate::runtime::RuntimeIrohCompressionCodec::None,
            512,
            3,
            1024 * 1024,
        )
        .unwrap(),
        handle: FrontendHandle {
            owner: "f".repeat(32),
            generation: 1,
        },
        summary: serde_json::from_value(serde_json::json!({
            "selected_version":3,"granted_role":"primary",
            "session_id":"$1","lease_id":"lease-one","client_id":"c1"
        }))
        .unwrap(),
        slots: slots.clone(),
        occurrence: occurrence.clone(),
    });
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let reservations = std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                let source = source.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    source.reserve().unwrap()
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut ids = reservations
        .iter()
        .map(|reservation| reservation.occurrence)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, (1..=8).collect::<Vec<_>>());
    assert_eq!(occurrence.load(Ordering::Relaxed), 8);
    assert_eq!(slots.available_permits(), 0);
    assert_eq!(
        source.reserve().err().unwrap().kind(),
        MezErrorKind::RateLimited
    );
    assert_eq!(occurrence.load(Ordering::Relaxed), 8);
    drop(reservations);
    assert_eq!(slots.available_permits(), 8);
    occurrence.store(u64::MAX, Ordering::Relaxed);
    assert_eq!(
        source.reserve().err().unwrap().kind(),
        MezErrorKind::Conflict
    );
    assert_eq!(slots.available_permits(), 8);
    assert_eq!(occurrence.load(Ordering::Relaxed), u64::MAX);
    drop(lease);
    assert!(
        source
            .reserve()
            .err()
            .unwrap()
            .message()
            .contains("parent connection retired")
    );
    assert_eq!(slots.available_permits(), 8);
    assert!(sibling.connection().close_reason().is_none());
    drop((source, peer, sibling, sibling_peer));
    owner
        .retire_and_shutdown()
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}
