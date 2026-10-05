//! Connection-local sampler defaults and real loopback selected-path evidence.
use super::*;

/// Initial quality remains unknown until the caller samples its exact retained
/// connection. Loopback sampling preserves the established classifier and moves
/// the next deadline without inventing measurements or changing connection state.
#[tokio::test]
async fn iroh_health_tracker_samples_only_retained_connection() {
    use iroh::endpoint::presets::Minimal;
    let server = iroh::Endpoint::builder(Minimal)
        .alpns(vec![crate::runtime::MEZZANINE_IROH_ALPN.to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    let client = iroh::Endpoint::builder(Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    let (connection, peer) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            client.connect(server.addr(), crate::runtime::MEZZANINE_IROH_ALPN),
            async { server.accept().await.unwrap().await.unwrap() }
        )
    })
    .await
    .unwrap();
    let connection = connection.unwrap();
    let mut tracker = AttachIrohHealthTracker::default();
    assert_eq!(tracker.quality(), TerminalIrohStatusQuality::Unknown);
    assert!(tracker.previous.is_none());
    let started = tokio::time::Instant::now();
    tracker.sample(&connection);
    assert!(tracker.deadline() >= started + AttachIrohHealthTracker::REFRESH_INTERVAL);
    if let Some(sample) = &tracker.previous {
        assert_eq!(sample.jitter_micros, 0);
        assert_eq!(
            tracker.quality(),
            crate::runtime::classify_runtime_iroh_connection_quality(
                sample.rtt_micros,
                0,
                0,
                0,
                std::time::Duration::ZERO
            )
        );
    } else {
        assert_eq!(tracker.quality(), TerminalIrohStatusQuality::Unknown);
    }
    tracker.sample(&connection);
    assert!(connection.close_reason().is_none());
    connection.close(iroh::endpoint::VarInt::from_u32(0), b"fixture complete");
    drop(peer);
    client.close().await;
    server.close().await;
}
