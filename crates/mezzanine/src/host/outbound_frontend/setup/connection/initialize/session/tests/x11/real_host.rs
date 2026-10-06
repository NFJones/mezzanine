//! Application-byte qualification through the real hosted session X11 proxy.
//!
//! Fixed shell input publishes only the disposable pane's DISPLAY for test
//! dialing. The proxy authenticates the synthetic fake cookie and forwards via
//! real host QUIC routing, broker supervision and client-local substitution.
//! A synthetic local TCP peer replaces a desktop X server; no provider work or
//! physical clipboard effects are performed. Input and initialization are never
//! retried, and all streams remain directly owned by the fixture.

use super::*;
use crate::host::outbound_frontend::client::OutboundSessionClient;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Executes one fixed environment-report command, then sends exact setup and
/// ping/pong bytes through the actual proxy. The session stays usable afterwards;
/// source identity and the paired endpoint are not replaced during forwarding.
pub(in crate::host::outbound_frontend::setup::connection::initialize::session::tests) async fn qualify_proxy_bytes(
    session: OutboundSessionClient,
    root: &Path,
    socket_name: &str,
    budget: Duration,
) -> OutboundSessionClient {
    let report = root.join("proxy-display");
    let command = format!(
        "printf '%s' \"$DISPLAY\" > {}\n",
        mez_agent::shell_quote(report.to_str().unwrap())
    );
    assert!(command.len() <= 512);
    let (session, acknowledgement) = session
        .step(
            80,
            24,
            command.as_bytes(),
            "report-host-proxy-display",
            budget,
        )
        .await
        .unwrap();
    assert_eq!(acknowledgement.input_bytes, command.len());
    let display = tokio::time::timeout(budget, async {
        loop {
            if let Ok(value) = std::fs::read_to_string(&report)
                && let Some(number) = value
                    .strip_prefix("127.0.0.1:")
                    .and_then(|value| value.strip_suffix(".0"))
                    .and_then(|value| value.parse::<u16>().ok())
            {
                break number;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fixed report command must execute before proxy dialing");
    let proxy_port = 6000_u16.checked_add(display).unwrap();
    let local = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_port = local.local_addr().unwrap().port();
    let forwarder = crate::cli::x11::X11ClientForwarder::new_for_test(
        crate::cli::x11::resolve_local_x11_display(&format!(
            "127.0.0.1:{}",
            local_port.checked_sub(6000).unwrap(),
        ))
        .unwrap(),
        crate::runtime::x11::X11Cookie::new([17; 16]),
        crate::runtime::x11::X11Cookie::new([52; 16]),
    );
    let opener = session.x11_channel_opener(socket_name, 1).unwrap();
    let channel = opener.open(budget).await.unwrap();
    assert_eq!(channel.occurrence(), 1);
    let mut setup = vec![0; 48];
    setup[0] = b'l';
    setup[2..4].copy_from_slice(&11_u16.to_le_bytes());
    setup[6..8].copy_from_slice(&18_u16.to_le_bytes());
    setup[8..10].copy_from_slice(&16_u16.to_le_bytes());
    setup[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
    setup[32..48].fill(17);
    let expected = setup.clone();
    let application = async {
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
            .await
            .unwrap();
        socket
            .write_all(&[setup, b"proxy-ping".to_vec()].concat())
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"proxy-pong");
    };
    let local_peer = async {
        let (mut socket, _) = local.accept().await.unwrap();
        let mut setup = [0; 48];
        socket.read_exact(&mut setup).await.unwrap();
        let mut expected = expected;
        expected[32..48].fill(52);
        assert_eq!(setup.as_slice(), expected);
        let mut input = Vec::new();
        socket.read_to_end(&mut input).await.unwrap();
        assert_eq!(input, b"proxy-ping");
        socket.write_all(b"proxy-pong").await.unwrap();
        socket.shutdown().await.unwrap();
    };
    let (result, (), ()) = tokio::time::timeout(budget, async {
        tokio::join!(
            Box::pin(forwarder.relay_broker_stream(channel, budget)),
            Box::pin(application),
            Box::pin(local_peer),
        )
    })
    .await
    .unwrap();
    result.unwrap();
    drop(opener);
    let (session, connected, _) = session.sample_transport_health(budget).await.unwrap();
    assert!(
        connected,
        "completed X11 bytes must preserve retained control"
    );
    std::fs::remove_file(report).unwrap();
    session
}
