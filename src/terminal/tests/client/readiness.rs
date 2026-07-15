//! Regression tests for terminal client readiness behavior.

use crate::terminal::fd::duration_to_timespec;
use crate::terminal::{
    AttachedTerminalClientLoopConfig, AttachedTerminalClientLoopIo, AttachedTerminalFd,
    AttachedTerminalFdLoopIo, AttachedTerminalFdRole, ClientViewRole, Duration, RenderedClientView,
    TerminalClientLoopAction, TerminalClientLoopConfig, TerminalCursorStyle, TerminalFdInterest,
    TerminalRawModeGuard, poll_attached_terminal_fd_readiness, run_attached_terminal_client_loop,
};
use mez_mux::input::MuxAction;
use mez_mux::layout::Size;
use mez_mux::theme::UiTheme;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

fn pipe_pair() -> std::io::Result<(File, File)> {
    let (read_end, write_end) = rustix::pipe::pipe().map_err(std::io::Error::from)?;
    Ok((File::from(read_end), File::from(write_end)))
}

/// Verifies attached terminal fd loop io reads and writes unix fds.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_fd_loop_io_reads_and_writes_unix_fds() {
    let (mut input_writer, input_reader) = UnixStream::pair().unwrap();
    let (output_writer, mut output_reader) = UnixStream::pair().unwrap();
    input_writer.write_all(b"\x01c").unwrap();
    output_reader
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let mut io = AttachedTerminalFdLoopIo::new(
        input_reader.as_raw_fd(),
        output_writer.as_raw_fd(),
        None,
        Some(Duration::ZERO),
    )
    .unwrap();
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(8, 2).unwrap(),
        client_size: Size::new(8, 2).unwrap(),
        lines: vec!["pane    ".to_string(), "        ".to_string()],
        line_style_spans: vec![Vec::new(), Vec::new()],
        selection: None,
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: true,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        focus_events: false,
        alternate_screen: false,
        host_mouse_reporting: true,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
    };

    let report = run_attached_terminal_client_loop(
        &mut io,
        || Ok(Some((view.clone(), None))),
        &TerminalClientLoopConfig::default(),
        AttachedTerminalClientLoopConfig {
            max_iterations: 1,
            max_input_bytes: 64,
        },
    )
    .unwrap();
    let mut output = [0u8; 128];
    let output_len = output_reader.read(&mut output).unwrap();
    let rendered = String::from_utf8_lossy(&output[..output_len]);

    assert_eq!(
        report.actions,
        vec![TerminalClientLoopAction::ExecuteMux(MuxAction::NewWindow)]
    );
    assert_eq!(report.output_frames, 1);
    assert!(report.bytes_written > 0);
    assert!(rendered.starts_with(
        "\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[2J\x1b[H"
    ));
    assert!(rendered.contains("pane"));
    assert!(rendered.ends_with("\x1b[?25l\x1b[0m\x1b[2 q\x1b[1;1H\x1b[?25h"));
}

/// Verifies that attached TTY output writability is sampled after the blocking
/// input poll rather than making the client loop wake immediately while idle.
/// Terminal output fds are usually writable, so including them in the blocking
/// poll turns the renderer into a fixed-rate busy loop.
#[test]
fn attached_terminal_fd_loop_io_blocks_until_input_poll_timeout_when_output_is_writable() {
    let (_input_writer, input_reader) = UnixStream::pair().unwrap();
    let (output_writer, _output_reader) = UnixStream::pair().unwrap();
    let mut io = AttachedTerminalFdLoopIo::new(
        input_reader.as_raw_fd(),
        output_writer.as_raw_fd(),
        None,
        Some(Duration::from_millis(25)),
    )
    .unwrap();

    let started = std::time::Instant::now();
    let readiness = io.poll_readiness().unwrap();

    assert!(started.elapsed() >= Duration::from_millis(10));
    assert!(readiness.iter().any(|ready| {
        ready.role == AttachedTerminalFdRole::Output && ready.writable && ready.is_ready()
    }));
    assert!(
        readiness
            .iter()
            .all(|ready| ready.role != AttachedTerminalFdRole::Input || !ready.readable)
    );
}

/// Verifies that already-ready input is returned without waiting for the
/// output writability sample. This protects against regressions where a
/// second blocking poll delays input processing behind a quiet descriptor.
#[test]
fn attached_terminal_fd_loop_io_returns_ready_input_without_second_poll_delay() {
    let (mut input_writer, input_reader) = UnixStream::pair().unwrap();
    let (output_writer, _output_reader) = UnixStream::pair().unwrap();
    input_writer.write_all(b"x").unwrap();
    let mut io = AttachedTerminalFdLoopIo::new(
        input_reader.as_raw_fd(),
        output_writer.as_raw_fd(),
        None,
        Some(Duration::from_millis(250)),
    )
    .unwrap();

    let started = std::time::Instant::now();
    let readiness = io.poll_readiness().unwrap();

    assert!(
        started.elapsed() < Duration::from_millis(100),
        "poll_readiness delayed ready input behind the output sample"
    );
    assert!(readiness.iter().any(|ready| {
        ready.role == AttachedTerminalFdRole::Input && ready.readable && ready.is_ready()
    }));
    assert!(readiness.iter().any(|ready| {
        ready.role == AttachedTerminalFdRole::Output && ready.writable && ready.is_ready()
    }));
}

/// Verifies attached terminal fd rejects negative fd and empty interest.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_fd_rejects_negative_fd_and_empty_interest() {
    assert_eq!(
        AttachedTerminalFd::input(-1, TerminalFdInterest::read())
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert_eq!(
        AttachedTerminalFd::output(1, TerminalFdInterest::default())
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
}

/// Verifies terminal raw mode rejects invalid fd before termios calls.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_raw_mode_rejects_invalid_fd_before_termios_calls() {
    let error = TerminalRawModeGuard::enable(-1).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies attached terminal readiness reports readable input.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_readable_input() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer.write_all(b"x").unwrap();
    let descriptor =
        AttachedTerminalFd::input(reader.as_raw_fd(), TerminalFdInterest::read()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert_eq!(readiness[0].role, AttachedTerminalFdRole::Input);
    assert_eq!(readiness[0].fd, reader.as_raw_fd());
    assert!(readiness[0].readable);
    assert!(!readiness[0].writable);
    assert!(!readiness[0].hangup);
    assert!(!readiness[0].error);
    assert!(readiness[0].is_ready());
}

/// Verifies attached terminal readiness reports writable output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_writable_output() {
    let (stream, _peer) = UnixStream::pair().unwrap();
    let descriptor =
        AttachedTerminalFd::output(stream.as_raw_fd(), TerminalFdInterest::write()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert_eq!(readiness[0].role, AttachedTerminalFdRole::Output);
    assert!(readiness[0].writable);
    assert!(!readiness[0].readable);
    assert!(!readiness[0].hangup);
    assert!(!readiness[0].error);
}

/// Verifies attached terminal readiness preserves control fd order.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_preserves_control_fd_order() {
    let (mut writer, input) = UnixStream::pair().unwrap();
    let (control, _control_peer) = UnixStream::pair().unwrap();
    writer.write_all(b"x").unwrap();
    let descriptors = [
        AttachedTerminalFd::control(control.as_raw_fd(), TerminalFdInterest::write()).unwrap(),
        AttachedTerminalFd::input(input.as_raw_fd(), TerminalFdInterest::read()).unwrap(),
    ];

    let readiness =
        poll_attached_terminal_fd_readiness(&descriptors, Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 2);
    assert_eq!(readiness[0].role, AttachedTerminalFdRole::Control);
    assert_eq!(readiness[0].interest, TerminalFdInterest::write());
    assert!(readiness[0].writable);
    assert_eq!(readiness[1].role, AttachedTerminalFdRole::Input);
    assert_eq!(readiness[1].interest, TerminalFdInterest::read());
    assert!(readiness[1].readable);
}

/// Verifies attached terminal readiness timeout returns not ready.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_timeout_returns_not_ready() {
    let (stream, _peer) = UnixStream::pair().unwrap();
    let descriptor =
        AttachedTerminalFd::input(stream.as_raw_fd(), TerminalFdInterest::read()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert!(!readiness[0].is_ready());
}

/// Verifies attached terminal readiness reports hangup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_hangup() {
    let (stream, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let descriptor =
        AttachedTerminalFd::input(stream.as_raw_fd(), TerminalFdInterest::read()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert!(readiness[0].hangup);
    assert!(!readiness[0].error);
}

/// Verifies attached terminal readiness reports pipe error.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_pipe_error() {
    let (read_end, write_end) = pipe_pair().unwrap();
    drop(read_end);
    let descriptor =
        AttachedTerminalFd::output(write_end.as_raw_fd(), TerminalFdInterest::write()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert!(readiness[0].error);
}

/// Verifies attached terminal readiness rejects invalid fd.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_rejects_invalid_fd() {
    let error = AttachedTerminalFd::control(-1, TerminalFdInterest::read()).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies attached terminal readiness timeout conversion preserves precision.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_timeout_conversion_preserves_precision() {
    let zero = duration_to_timespec(Duration::ZERO).unwrap();
    assert_eq!(zero.tv_sec, 0);
    assert_eq!(zero.tv_nsec, 0);

    let one_nano = duration_to_timespec(Duration::from_nanos(1)).unwrap();
    assert_eq!(one_nano.tv_sec, 0);
    assert_eq!(one_nano.tv_nsec, 1);

    let two_millis = duration_to_timespec(Duration::from_millis(2)).unwrap();
    assert_eq!(two_millis.tv_sec, 0);
    assert_eq!(two_millis.tv_nsec, 2_000_000);
}
