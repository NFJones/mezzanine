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

/// Verifies that invalidating a partially written output frame preserves its
/// emitted prefix and retained tail before forcing the next frame to stand
/// alone.
///
/// Once terminal bytes have been accepted, discarding the remainder can leave
/// a mode transition or clear sequence only partly applied. Invalidation must
/// therefore finish the started frame and make the following frame a full
/// redraw instead of splicing a new frame onto the emitted prefix.
#[tokio::test]
async fn async_fd_attached_terminal_io_invalidation_preserves_started_output() {
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
            128,
        )
        .await
        .unwrap();

    assert!(first.is_partial());
    assert!(io.pending_output_bytes() > 0);
    let mut emitted_prefix = [0u8; 256];
    let emitted_prefix_len = peer.read(&mut emitted_prefix).unwrap();
    let emitted_prefix = &emitted_prefix[..emitted_prefix_len];
    assert!(
        emitted_prefix
            .windows(b"\x1b[?1049l".len())
            .any(|window| window == b"\x1b[?1049l"),
        "{emitted_prefix:?}"
    );

    io.invalidate_output_frame().await.unwrap();
    assert!(io.pending_output_bytes() > 0);

    while io.pending_output_bytes() > 0 {
        io.flush_pending_output(1024).await.unwrap();
    }
    io.write_styled_output_with_modes(
        &["fresh-frame-marker".to_string()],
        &[],
        AttachedTerminalOutputModes::default(),
    )
    .await
    .unwrap();

    drop(io);
    drop(driver_output);
    drop(driver);
    let mut output = emitted_prefix.to_vec();
    peer.read_to_end(&mut output).unwrap();
    let output = String::from_utf8_lossy(&output);
    let retained_tail = output.find("stale-tail-marker").unwrap();
    let fresh_frame = output.find("fresh-frame-marker").unwrap();
    assert!(retained_tail < fresh_frame, "{output:?}");
    assert!(
        output.matches("\u{1b}[2J\u{1b}[H").count() >= 2,
        "{output:?}"
    );
}

/// Verifies that a bounded terminal flush reports backpressure immediately
/// instead of awaiting writability inside the foreground service batch.
///
/// A full host output buffer is temporary flow control, not a fatal terminal
/// error. Returning a retained partial frame lets the service continue polling
/// input and resume output when writable readiness arrives.
#[tokio::test]
async fn async_fd_attached_terminal_io_bounded_flush_does_not_wait_for_writability() {
    let (driver, _peer) = StdUnixStream::pair().unwrap();
    let mut driver_output = driver.try_clone().unwrap();
    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let filler = vec![b'x'; 64 * 1024];
    loop {
        match driver_output.write(&filler) {
            Ok(0) => panic!("socket output made no progress while filling"),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("failed to fill socket output: {error}"),
        }
    }

    let flush = tokio::time::timeout(
        Duration::from_millis(20),
        io.write_styled_output_with_modes_bounded(
            &["blocked-frame".to_string()],
            &[],
            AttachedTerminalOutputModes::default(),
            1024,
        ),
    )
    .await
    .expect("bounded output flush waited for terminal writability")
    .unwrap();

    assert!(flush.is_partial());
    assert_eq!(flush.bytes_written, 0);
    assert!(flush.pending_bytes > 0);
}

/// Verifies that submitting a newer render while an older frame has started
/// cannot discard the older frame's retained tail.
///
/// Terminal frames are byte streams rather than atomic messages. The first
/// frame must finish before the latest coalesced frame begins, otherwise mode
/// and cursor sequences from the two frames can be interleaved.
#[tokio::test]
async fn async_fd_attached_terminal_io_completes_started_frame_before_newer_frame() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();

    let first = io
        .write_styled_output_with_modes_bounded(
            &[format!("{}first-tail-marker", "x".repeat(4096))],
            &[],
            AttachedTerminalOutputModes::default(),
            1,
        )
        .await
        .unwrap();
    assert!(first.is_partial());

    io.write_styled_output_with_modes(
        &["newer-frame-marker".to_string()],
        &[],
        AttachedTerminalOutputModes::default(),
    )
    .await
    .unwrap();

    drop(io);
    drop(driver_output);
    drop(driver);
    let mut output = Vec::new();
    peer.read_to_end(&mut output).unwrap();
    let output = String::from_utf8_lossy(&output);
    let first_tail = output.find("first-tail-marker").unwrap();
    let newer_frame = output.find("newer-frame-marker").unwrap();
    assert!(first_tail < newer_frame, "{output:?}");
}

/// Verifies that presentation restoration abandons a permanently blocked
/// display reset within a fixed deadline.
///
/// Cleanup must not strand the foreground task on the same backpressured file
/// descriptor that caused output delivery to stop. A best-effort reset may be
/// dropped, while termios restoration remains independently actionable.
#[tokio::test]
async fn async_fd_attached_terminal_io_restore_is_bounded_by_backpressure() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let mut driver_output = driver.try_clone().unwrap();
    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    io.enter_presentation().await.unwrap();

    peer.set_nonblocking(true).unwrap();
    let mut scratch = [0u8; 1024];
    loop {
        match peer.read(&mut scratch) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("failed to drain presentation entry: {error}"),
        }
    }
    let filler = vec![b'x'; 64 * 1024];
    loop {
        match driver_output.write(&filler) {
            Ok(0) => panic!("socket output made no progress while filling"),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("failed to fill socket output: {error}"),
        }
    }

    let restore = tokio::time::timeout(Duration::from_millis(100), io.restore_presentation()).await;
    if restore.is_err() {
        std::mem::forget(io);
        panic!("presentation restoration waited indefinitely for output writability");
    }
    restore.unwrap().unwrap();
}

/// Verifies that drop-time presentation cleanup stays nonblocking when the
/// terminal output buffer is full.
///
/// Destructor cleanup cannot await readiness. It must attempt the reset while
/// the descriptor is still nonblocking and then restore the original file
/// status flags, rather than blocking the foreground thread in `write`.
#[tokio::test]
async fn async_fd_attached_terminal_io_drop_is_prompt_under_backpressure() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let mut driver_output = driver.try_clone().unwrap();
    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    io.enter_presentation().await.unwrap();

    peer.set_nonblocking(true).unwrap();
    let mut scratch = [0u8; 1024];
    loop {
        match peer.read(&mut scratch) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("failed to drain presentation entry: {error}"),
        }
    }
    let filler = vec![b'x'; 64 * 1024];
    loop {
        match driver_output.write(&filler) {
            Ok(0) => panic!("socket output made no progress while filling"),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("failed to fill socket output: {error}"),
        }
    }

    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(io);
        drop(driver_output);
        drop(driver);
        let _ = dropped_tx.send(());
    });
    dropped_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("drop-time presentation cleanup blocked on terminal output");
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
