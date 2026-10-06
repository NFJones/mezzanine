//! Dedicated broker-stream tests use synthetic local TCP X peers, not a desktop.
use super::*;
use crate::cli::x11::resolve_local_x11_display;
use crate::runtime::x11::{X11Cookie, validate_x11_setup_cookie};

/// Waiting for an application to request X11 must not consume the packet-setup
/// budget or dial the local target. Advancing virtual time past that budget
/// leaves one owned future pending; cancellation closes its dedicated stream.
/// Once even a single setup byte arrives, incomplete setup remains bounded.
#[tokio::test(start_paused = true)]
async fn broker_x11_client_idle_demand_wait_preserves_bounded_setup() {
    let display = resolve_local_x11_display("127.0.0.1:19").unwrap();
    let forwarder = X11ClientForwarder::new_for_test(
        display,
        X11Cookie::new([17; 16]),
        X11Cookie::new([52; 16]),
    );
    let (stream, mut broker) = tokio::io::duplex(4096);
    let mut relay = Box::pin(forwarder.relay_broker_stream(stream, Duration::from_millis(100)));
    assert!(matches!(
        futures_util::poll!(&mut relay),
        std::task::Poll::Pending
    ));
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        matches!(futures_util::poll!(&mut relay), std::task::Poll::Pending),
        "idle demand must not be treated as failed packet setup"
    );
    broker.write_all(b"l").await.unwrap();
    assert!(matches!(
        futures_util::poll!(&mut relay),
        std::task::Poll::Pending
    ));
    tokio::time::advance(Duration::from_millis(101)).await;
    assert!(
        relay.await.is_err(),
        "partial setup must retain its finite deadline"
    );
    let mut bytes = Vec::new();
    broker.read_to_end(&mut bytes).await.unwrap();
    assert!(bytes.is_empty());

    let (stream, mut broker) = tokio::io::duplex(4096);
    let mut relay = Box::pin(forwarder.relay_broker_stream(stream, Duration::from_millis(100)));
    assert!(matches!(
        futures_util::poll!(&mut relay),
        std::task::Poll::Pending
    ));
    drop(relay);
    assert_eq!(
        broker.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

/// Constructs one fixed MIT setup in either supported byte order.
fn setup(order: u8, cookie: u8) -> Vec<u8> {
    let mut bytes = vec![0; 48];
    bytes[0] = order;
    let number = |value: u16| {
        if order == b'l' {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        }
    };
    bytes[2..4].copy_from_slice(&number(11));
    bytes[6..8].copy_from_slice(&number(18));
    bytes[8..10].copy_from_slice(&number(16));
    bytes[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
    bytes[32..48].fill(cookie);
    bytes
}

/// Fake proof is substituted only on the frozen local target; raw application
/// bytes survive a coalesced setup tail. Both byte orders and reverse response
/// after broker half-close work without sending the real credential upstream.
#[tokio::test]
async fn broker_x11_client_substitutes_locally_and_preserves_half_close() {
    for order in *b"lB" {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let display =
            resolve_local_x11_display(&format!("127.0.0.1:{}", port.checked_sub(6000).unwrap()))
                .unwrap();
        let forwarder = X11ClientForwarder::new_for_test(
            display,
            X11Cookie::new([17; 16]),
            X11Cookie::new([52; 16]),
        );
        let (stream, mut broker) = tokio::io::duplex(4096);
        let remote = async {
            broker
                .write_all(&[setup(order, 17), b"ping".to_vec()].concat())
                .await
                .unwrap();
            broker.shutdown().await.unwrap();
            let mut reply = Vec::new();
            broker.read_to_end(&mut reply).await.unwrap();
            assert_eq!(reply, b"pong");
        };
        let local = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 48];
            socket.read_exact(&mut bytes).await.unwrap();
            validate_x11_setup_cookie(&bytes, &X11Cookie::new([52; 16])).unwrap();
            assert_eq!(bytes[0], order);
            let mut tail = Vec::new();
            socket.read_to_end(&mut tail).await.unwrap();
            assert_eq!(tail, b"ping");
            socket.write_all(b"pong").await.unwrap();
            socket.shutdown().await.unwrap();
        };
        let (relayed, (), ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                forwarder.relay_broker_stream(stream, Duration::from_secs(2)),
                remote,
                local
            )
        })
        .await
        .unwrap();
        relayed.unwrap();
    }
}

/// Invalid or incomplete setup must not dial a local X server. Cancellation
/// after local setup closes both owned streams while leaving caller credential
/// lifetime untouched; no tail or setup is automatically replayed.
#[tokio::test]
async fn broker_x11_client_rejects_setup_and_cancels_owned_streams() {
    for case in ["cookie", "partial", "cancel"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let display =
            resolve_local_x11_display(&format!("127.0.0.1:{}", port.checked_sub(6000).unwrap()))
                .unwrap();
        let forwarder = X11ClientForwarder::new_for_test(
            display,
            X11Cookie::new([17; 16]),
            X11Cookie::new([52; 16]),
        );
        let (stream, mut broker) = tokio::io::duplex(4096);
        if case == "partial" {
            broker.write_all(b"l").await.unwrap();
        } else {
            broker
                .write_all(&setup(b'l', if case == "cookie" { 18 } else { 17 }))
                .await
                .unwrap();
        }
        if case == "cancel" {
            let mut relay = Box::pin(forwarder.relay_broker_stream(stream, Duration::from_secs(2)));
            let mut local = tokio::select! {
                result = &mut relay => panic!("relay ended before setup barrier: {}", result.is_ok()),
                socket = listener.accept() => socket.unwrap().0,
            };
            let mut header = [0; 48];
            tokio::select! {
                result = &mut relay => panic!("relay ended before local setup: {}", result.is_ok()),
                result = local.read_exact(&mut header) => { result.unwrap(); },
            }
            validate_x11_setup_cookie(&header, &X11Cookie::new([52; 16])).unwrap();
            drop(relay);
            let mut bytes = Vec::new();
            tokio::time::timeout(Duration::from_secs(1), local.read_to_end(&mut bytes))
                .await
                .unwrap()
                .unwrap();
            assert!(bytes.is_empty());
        } else {
            let error = forwarder
                .relay_broker_stream(stream, Duration::from_millis(100))
                .await
                .unwrap_err();
            assert_eq!(
                error.kind(),
                if case == "cookie" {
                    crate::error::MezErrorKind::Forbidden
                } else {
                    crate::error::MezErrorKind::InvalidState
                }
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err()
            );
        }
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), broker.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(bytes.is_empty());
    }
}
