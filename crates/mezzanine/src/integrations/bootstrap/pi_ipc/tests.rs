//! Private inherited-stream framing and ownership-loss regression coverage.
//!
//! All endpoints are test-owned socket pairs; no daemon credentials, vendor
//! session files or provider requests are used. Payload errors stay content-free.

use super::super::pi_session;
use super::*;
use tokio::io::AsyncWriteExt;

/// Start activation preserves later bytes in the inherited stream and waits
/// through idle silence. No running-only, malformed or truncated first fact
/// may create a registration; the partial-frame deadline is not extended.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn pi_ipc_start_gate_requires_activation_and_preserves_following_frame() {
    let (mut stream, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let (sent, arrived) = tokio::sync::oneshot::channel();
    let producer = async {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        writer.write_all(b"{\"type\":\"session_start\",\"reason\":\"startup\"}\n{\"type\":\"agent_start\"}\n").await.unwrap();
        sent.send(()).unwrap();
    };
    let consumer = async {
        let fact = wait_for_start(&mut stream, "bound").await.unwrap();
        arrived.await.unwrap();
        assert_eq!(fact, pi::Observation::SessionStarted { reason: "startup" });
        let mut suffix = [0; 23];
        stream.read_exact(&mut suffix).await.unwrap();
        assert_eq!(&suffix, b"{\"type\":\"agent_start\"}\n");
    };
    tokio::join!(producer, consumer);
    for bytes in [
        b"{\"type\":\"agent_start\"}\n".as_slice(),
        b"{\n",
        b"{\"type\":\"session_start\",\"reason\":\"startup\"}",
    ] {
        let (mut stream, mut writer) = tokio::net::UnixStream::pair().unwrap();
        writer.write_all(bytes).await.unwrap();
        writer.shutdown().await.unwrap();
        assert!(wait_for_start(&mut stream, "bound").await.is_err());
    }
}

/// Exact callback frames cannot smuggle routing, credentials or vendor content,
/// even through unit event variants, duplicate fields or alternate event names.
#[test]
fn pi_ipc_wire_rejects_content_and_ambiguous_event_fields() {
    assert_eq!(
        observation("bound", br#"{"type":"agent_start"}"#).unwrap(),
        pi::Observation::Running
    );
    for invalid in [
        br#"{"type":"agent_start","prompt":"PRIVATE"}"#.as_slice(),
        br#"{"type":"agent_settled","session":"other"}"#,
        br#"{"type":"session_start","reason":"startup","path":"PRIVATE"}"#,
        br#"{"type":"agent_start","type":"agent_settled"}"#,
        br#"{"type":"session_shutdown","reason":"quit","reason":"reload"}"#,
        br#"{"type":"message_end"}"#,
        br#"{"type":"ui_prompt_start","reason":"approval","kind":"confirm"}"#,
        b"[]",
        b"",
    ] {
        let error = observation("bound", invalid).unwrap_err();
        assert_eq!(error.message(), "Pi observer stream unavailable");
    }
    assert!(observation("bound", &vec![b'x'; MAX_FRAME + 1]).is_err());
    assert!(observation("PRIVATE\n", br#"{"type":"agent_start"}"#).is_err());
}

/// Truncated, oversized and malformed frames fail before admitting callback
/// work; idle worker ownership is released when its ingress receiver closes.
#[tokio::test(flavor = "current_thread")]
async fn pi_ipc_invalid_frames_and_receiver_loss_release_stream() {
    for bytes in [
        b"{\n".to_vec(),
        b"{\"type\":\"agent_start\"}".to_vec(),
        vec![b'x'; MAX_FRAME + 1],
    ] {
        let (stream, mut writer) = tokio::net::UnixStream::pair().unwrap();
        let (ingress, _inputs) = pi_session::channel();
        let (_stop, cancellation) = watch::channel(false);
        let producer = async {
            writer.write_all(&bytes).await.unwrap();
            writer.shutdown().await.unwrap();
        };
        let (result, ()) = tokio::join!(serve(stream, "bound", 1, ingress, cancellation), producer);
        assert_eq!(
            result.unwrap_err().message(),
            "Pi observer stream unavailable"
        );
    }
    let (stream, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let (ingress, inputs) = pi_session::channel();
    let (_stop, cancellation) = watch::channel(false);
    drop(inputs);
    serve(stream, "bound", 1, ingress, cancellation)
        .await
        .unwrap();
    let mut byte = [0];
    assert_eq!(writer.read(&mut byte).await.unwrap(), 0);
}

/// Flooding complete inert frames cannot allocate unbounded callback work.
/// Once the finite coordinator queue is full the bridge releases the stream;
/// earlier accepted frames remain bounded rather than being replayed.
#[tokio::test(flavor = "current_thread")]
async fn pi_ipc_queue_pressure_terminates_bounded_ingress() {
    let (stream, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let (ingress, _inputs) = pi_session::channel();
    let (_stop, cancellation) = watch::channel(false);
    let producer = async {
        let frames = b"{\"type\":\"agent_start\"}\n".repeat(33);
        writer.write_all(&frames).await.unwrap();
        writer.shutdown().await.unwrap();
    };
    let (result, ()) = tokio::join!(serve(stream, "bound", 1, ingress, cancellation), producer);
    assert_eq!(
        result.unwrap_err().message(),
        "Pi observer stream unavailable"
    );
}

/// Completing bytes queued after the deadline must fail even when the read is
/// ready before the bridge is polled again. Tokio timeout alone polls its inner
/// future first, so ready I/O cannot be treated as proof of timely completion.
#[tokio::test(start_paused = true)]
async fn pi_ipc_ready_read_cannot_bypass_elapsed_deadline() {
    let end = Instant::now() + FRAME_DEADLINE;
    tokio::time::advance(FRAME_DEADLINE + Duration::from_millis(1)).await;
    assert!(
        read_before_deadline(Some(end), std::future::ready(Ok(24)))
            .await
            .is_err()
    );
}

/// Real socket completion queued after a parked partial read must be rejected.
/// This complements the deterministic ready-future boundary regression above.
#[tokio::test(start_paused = true)]
async fn pi_ipc_overdue_ready_completion_is_rejected() {
    use std::future::Future;
    use std::task::Poll;

    let (stream, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let (ingress, _inputs) = pi_session::channel();
    let (_stop, cancellation) = watch::channel(false);
    writer.write_all(b"{").await.unwrap();
    stream.readable().await.unwrap();
    let mut bridge = Box::pin(serve(stream, "bound", 1, ingress, cancellation));
    // Consume the ready prefix and park on its incomplete frame. Do not poll
    // the bridge while advancing time and queuing the otherwise valid suffix.
    std::future::poll_fn(|cx| {
        assert!(bridge.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    tokio::time::advance(FRAME_DEADLINE + Duration::from_millis(1)).await;
    writer
        .write_all(b"\"type\":\"agent_start\"}\n")
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_millis(1), bridge).await;
    assert!(
        matches!(result, Ok(Err(_))),
        "late ready frame was not rejected"
    );
}

/// A slow partial frame has one total deadline, whereas clean idle silence is
/// allowed. Explicit cancellation settles the idle bridge without a frame.
#[tokio::test(start_paused = true)]
async fn pi_ipc_partial_deadline_and_idle_cancellation_are_distinct() {
    use std::future::Future;
    use std::task::Poll;

    let (stream, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let (ingress, _inputs) = pi_session::channel();
    let (_stop, cancellation) = watch::channel(false);
    writer.write_all(b"{").await.unwrap();
    stream.readable().await.unwrap();
    let mut bridge = Box::pin(serve(stream, "bound", 1, ingress, cancellation));
    std::future::poll_fn(|cx| {
        assert!(bridge.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    for _ in 0..3 {
        tokio::time::advance(Duration::from_millis(80)).await;
        writer.write_all(b" ").await.unwrap();
        std::future::poll_fn(|cx| {
            assert!(bridge.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    tokio::time::advance(Duration::from_millis(11)).await;
    // Writer remains alive and open: truncated EOF cannot satisfy this check.
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(1), bridge).await,
        Ok(Err(_))
    ));
    let (stream, _writer) = tokio::net::UnixStream::pair().unwrap();
    let (ingress, _inputs) = pi_session::channel();
    let (stop, cancellation) = watch::channel(false);
    let cancel = async {
        tokio::time::sleep(FRAME_DEADLINE + Duration::from_millis(50)).await;
        stop.send(true).unwrap();
    };
    let (result, ()) = tokio::join!(serve(stream, "bound", 1, ingress, cancellation), cancel);
    result.unwrap();
}
