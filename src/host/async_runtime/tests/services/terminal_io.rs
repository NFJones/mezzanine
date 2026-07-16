//! Async-runtime tests owned by terminal io behavior.

use super::super::*;

/// Verifies that the deterministic async attached-terminal fake behaves like an
/// ordered terminal endpoint: readiness, input truncation, size responses,
/// presentation guards, invalidation, and styled-frame writes are all visible
/// without using wall-clock sleeps. This gives later Tokio client-loop tests a
/// stable fake before production file descriptors are migrated to `AsyncFd`.
#[tokio::test]
async fn async_fake_attached_terminal_io_records_ordered_operations() {
    let mut io = AsyncFakeAttachedTerminalIo::default();
    let readiness = AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    };
    io.push_readiness(vec![readiness]);
    io.push_input(b"abcdef".to_vec());
    io.push_terminal_size(Some(Size::new(100, 30).unwrap()));

    assert_eq!(io.poll_readiness().await.unwrap(), vec![readiness]);
    assert_eq!(io.read_input(3).await.unwrap(), b"abc");
    assert_eq!(
        io.terminal_size().await.unwrap(),
        Some(Size::new(100, 30).unwrap())
    );

    io.enter_presentation().await.unwrap();
    io.invalidate_output_frame().await.unwrap();
    let modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_row: 2,
        cursor_column: 3,
        ..AttachedTerminalOutputModes::default()
    };
    let lines = vec!["hello".to_string(), "world".to_string()];
    let bytes = io
        .write_styled_output_with_modes(&lines, &[], modes)
        .await
        .unwrap();
    io.restore_presentation().await.unwrap();

    assert_eq!(bytes, "helloworld".len());
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.presentation_restores, 1);
    assert_eq!(io.invalidated_output_frames, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, lines);
    assert_eq!(io.written_frames[0].modes.cursor_row, 2);
    assert_eq!(io.written_frames[0].modes.cursor_column, 3);
}

/// Verifies that the shared attached-terminal presentation guard validates the
/// raw-mode descriptor before entering the foreground terminal path. This keeps
/// daemon and control-socket attach clients on one setup boundary and prevents
/// invalid descriptors from partially constructing async fd state that would
/// later be difficult to clean up.
#[test]
fn async_attached_terminal_presentation_guard_rejects_invalid_raw_fd() {
    let error = AsyncAttachedTerminalPresentationGuard::new(-1, -1, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("terminal raw mode file descriptor is invalid"),
        "{error}"
    );
}

/// Verifies that the transitional sync-to-async terminal adapter preserves the
/// existing `AttachedTerminalClientLoopIo` behavior while exposing the new async
/// trait. The adapter is a migration bridge only, so this test protects current
/// behavior while making its replacement with a native Tokio implementation
/// mechanically straightforward.
#[tokio::test]
async fn sync_attached_terminal_io_adapter_preserves_existing_fake_behavior() {
    let mut sync = FakeAttachedTerminalLoopIo::default();
    let readiness = AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Output,
        fd: 1,
        interest: TerminalFdInterest::write(),
        readable: false,
        writable: true,
        hangup: false,
        error: false,
    };
    sync.readiness_batches.push(vec![readiness]);
    sync.input_batches.push(b"input".to_vec());
    let mut adapter = SyncAttachedTerminalIoAdapter::new(sync);

    assert_eq!(adapter.poll_readiness().await.unwrap(), vec![readiness]);
    assert_eq!(adapter.read_input(2).await.unwrap(), b"in");
    let lines = vec!["frame".to_string()];
    let bytes = adapter
        .write_styled_output_with_modes(&lines, &[], AttachedTerminalOutputModes::default())
        .await
        .unwrap();

    let sync = adapter.into_inner();
    assert_eq!(bytes, "frame".len());
    assert_eq!(sync.written_batches, vec![lines]);
}

/// Verifies that the Tokio `AsyncFd` attached-terminal endpoint can read and
/// write through Unix file descriptors without the synchronous terminal polling
/// trait. The test uses a Unix socket pair as a deterministic fd source, which
/// exercises nonblocking flag setup, async input readiness, async output
/// flushing, and terminal-frame encoding without requiring a real foreground
/// TTY.
#[tokio::test]
async fn async_fd_attached_terminal_io_reads_and_writes_socket_pair() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    peer.write_all(b"input").unwrap();

    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let input = io.read_input(5).await.unwrap();
    assert_eq!(input, b"input");

    let lines = vec!["async-frame".to_string()];
    let bytes = io
        .write_styled_output_with_modes(&lines, &[], AttachedTerminalOutputModes::default())
        .await
        .unwrap();
    assert!(bytes > "async-frame".len());

    let mut output = vec![0u8; 4096];
    let read = peer.read(&mut output).unwrap();
    let output = String::from_utf8_lossy(&output[..read]);
    assert!(output.contains("async-frame"), "{output:?}");
}

/// Verifies that the native async terminal endpoint's normal frame-write API
/// completes frames larger than the adaptive bounded-write chunk. Control-socket
/// attach rendering uses this API directly; returning after the first chunk
/// leaves the rest of a scroll or copy-mode repaint retained but never flushed,
/// which appears as large unrendered regions on the attached terminal.
#[tokio::test]
async fn async_fd_attached_terminal_io_unbounded_write_completes_large_frame() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();

    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let large_line = format!(
        "{}tail-marker",
        "x".repeat(DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES + 1024)
    );
    let lines = vec![large_line.clone()];
    let bytes = io
        .write_styled_output_with_modes(&lines, &[], AttachedTerminalOutputModes::default())
        .await
        .unwrap();

    assert!(bytes > DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES);
    assert_eq!(io.pending_output_bytes(), 0);

    let mut output = Vec::new();
    drop(io);
    drop(driver_output);
    drop(driver);
    peer.read_to_end(&mut output).unwrap();
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("tail-marker"), "{output:?}");
}

/// Verifies that invalidating a partially written differential output frame
/// discards the stale remainder before any pending-output flush can emit it.
///
/// A full redraw request can arrive while a bounded foreground-terminal write
/// still has retained bytes from an older differential frame. Those retained
/// bytes are no longer a valid basis for the next frame and must be dropped
/// instead of being flushed before the full-redraw state reset.
#[tokio::test]
async fn async_fd_attached_terminal_io_invalidation_discards_pending_output() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();

    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let lines = vec![format!("{}stale-tail-marker", "x".repeat(4096))];
    let first = io
        .write_styled_output_with_modes_bounded(
            &lines,
            &[],
            AttachedTerminalOutputModes::default(),
            1,
        )
        .await
        .unwrap();

    assert!(first.is_partial());
    assert!(io.pending_output_bytes() > 0);

    io.invalidate_output_frame().await.unwrap();
    assert_eq!(io.pending_output_bytes(), 0);

    let flush = io.flush_pending_output(1024).await.unwrap();
    assert!(flush.completed);
    assert_eq!(flush.bytes_written, 0);

    drop(io);
    drop(driver_output);
    drop(driver);
    let mut output = Vec::new();
    peer.read_to_end(&mut output).unwrap();
    let output = String::from_utf8_lossy(&output);
    assert!(!output.contains("stale-tail-marker"), "{output:?}");
}

/// Verifies that the native async terminal endpoint reports pending input
/// before the always-writable output side of an interactive PTY-like fd pair.
/// This protects foreground attach loops from starving user keystrokes while
/// redraws remain possible on every iteration.
#[tokio::test]
async fn async_fd_attached_terminal_io_prioritizes_input_over_writable_output() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.write_all(b"x").unwrap();

    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let readiness = io.poll_readiness().await.unwrap();

    assert!(
        readiness
            .iter()
            .any(|ready| ready.role == AttachedTerminalFdRole::Input && ready.readable),
        "{readiness:?}"
    );
}

/// Verifies that the native async terminal endpoint's input-focused readiness
/// wait does not wake merely because stdout is writable. This is the attach
/// service idle-CPU guard: redraws should come from actor render notifications
/// or explicit fallback timers, while user input still wakes the service
/// promptly.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_fd_attached_terminal_input_readiness_ignores_writable_output() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();

    let idle = tokio::time::timeout(Duration::from_millis(1), io.poll_input_readiness()).await;
    assert!(idle.is_err(), "writable output should not wake input wait");

    peer.write_all(b"x").unwrap();
    let readiness = tokio::time::timeout(Duration::from_millis(1), io.poll_input_readiness())
        .await
        .unwrap()
        .unwrap();
    assert!(
        readiness
            .iter()
            .any(|ready| ready.role == AttachedTerminalFdRole::Input && ready.readable),
        "{readiness:?}"
    );
}
