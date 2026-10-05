//! Complete-frame output evidence before remote acknowledgement is permitted.
//!
//! Deterministic writers exercise partial flushes and stale receipt rejection;
//! the concrete fd writer uses a disposable Unix stream, not an interactive TTY.

use super::*;
use crate::host::async_runtime::{AsyncTerminalIoFuture, AsyncTerminalOutputWriteReport};
use crate::host::terminal::AttachedTerminalFdReadiness;
use mez_mux::presentation::AttachedTerminalOutputModes;
use mez_terminal::TerminalStyleSpan;

/// Finite partial writer with explicit completion receipt evidence. It accepts
/// one frame only and exposes a failure seam before the tail commits.
#[derive(Default)]
struct PartialWriter {
    pending: usize,
    committed: Vec<u64>,
    queued_receipts: Vec<u64>,
    frames: usize,
    flushes: usize,
    fail_flush: bool,
    wrong_receipts: bool,
    persistent_input: bool,
    zero_progress: bool,
    input_reads: usize,
    readiness_polls: usize,
    lines: Vec<String>,
    styles: Vec<Vec<TerminalStyleSpan>>,
    completion: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    modes: AttachedTerminalOutputModes,
}

impl AsyncAttachedTerminalIo for PartialWriter {
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        if self.persistent_input {
            self.readiness_polls += 1;
            return Box::pin(async {
                Ok(vec![AttachedTerminalFdReadiness {
                    role: crate::host::terminal::AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: crate::host::terminal::TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                }])
            });
        }
        Box::pin(std::future::pending())
    }

    fn read_input<'a>(&'a mut self, _max_bytes: usize) -> AsyncTerminalIoFuture<'a, Vec<u8>> {
        self.input_reads += 1;
        Box::pin(std::future::pending())
    }

    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        _lines: &'a [String],
        _spans: &'a [Vec<TerminalStyleSpan>],
        _modes: AttachedTerminalOutputModes,
    ) -> AsyncTerminalIoFuture<'a, usize> {
        Box::pin(async {
            Err(MezError::invalid_state(
                "fixture requires receipt-aware output",
            ))
        })
    }

    fn write_owned_styled_output_with_modes_bounded_and_receipts<'a>(
        &'a mut self,
        lines: Vec<String>,
        spans: Vec<Vec<TerminalStyleSpan>>,
        modes: AttachedTerminalOutputModes,
        receipts: Vec<u64>,
        _max_bytes: usize,
    ) -> AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        Box::pin(async move {
            self.frames += 1;
            self.lines = lines;
            self.styles = spans;
            self.modes = modes;
            self.pending = 2;
            self.queued_receipts = receipts;
            Ok(AsyncTerminalOutputWriteReport {
                bytes_written: usize::from(!self.zero_progress),
                completed: false,
                pending_bytes: 2,
            })
        })
    }

    fn pending_output_bytes(&self) -> usize {
        self.pending
    }

    fn flush_pending_output<'a>(
        &'a mut self,
        _max_bytes: usize,
    ) -> AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        Box::pin(async move {
            self.flushes += 1;
            if self.fail_flush {
                return Err(MezError::invalid_state("fixture tail write failed"));
            }
            self.pending -= 1;
            if self.pending == 0 {
                self.committed = if self.wrong_receipts {
                    vec![99]
                } else {
                    std::mem::take(&mut self.queued_receipts)
                };
                if let Some(completion) = &self.completion {
                    completion.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            Ok(AsyncTerminalOutputWriteReport {
                bytes_written: 1,
                completed: self.pending == 0,
                pending_bytes: self.pending,
            })
        })
    }

    fn take_committed_presentation_ids(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.committed)
    }
}

mod status;

/// Persistent unread input must not starve a previously stalled output frame.
/// The writer can progress on its next bounded flush even though shared readiness
/// keeps reporting Input. Commitment preserves that input and submits one frame.
#[tokio::test]
async fn outbound_output_commit_progresses_without_consuming_unread_input() {
    let mut writer = PartialWriter {
        persistent_input: true,
        zero_progress: true,
        ..Default::default()
    };
    tokio::time::timeout(
        Duration::from_millis(150),
        commit_snapshot(
            &mut writer,
            &["retained frame".to_string()],
            &[vec![]],
            AttachedTerminalOutputModes::default(),
            &[7],
        ),
    )
    .await
    .expect("unread input must not starve bounded output flushes")
    .unwrap();
    assert_eq!((writer.frames, writer.flushes, writer.pending), (1, 2, 0));
    assert_eq!(writer.input_reads, 0);
    assert!(writer.committed.is_empty());
}

/// Partial writes must flush the original tail without submitting another frame.
/// Failed tails, wrong receipt evidence and preexisting writer ownership cannot
/// justify any acknowledgement, even if some output bytes were accepted.
#[tokio::test]
async fn outbound_output_commit_requires_complete_exact_frame_evidence() {
    let lines = vec!["retained snapshot".to_string()];
    let styles = vec![vec![]];
    let modes = AttachedTerminalOutputModes::default();
    let mut writer = PartialWriter::default();
    commit_snapshot(&mut writer, &lines, &styles, modes, &[7, 8])
        .await
        .unwrap();
    assert_eq!((writer.frames, writer.flushes, writer.pending), (1, 2, 0));
    assert!(writer.committed.is_empty());
    for mut writer in [
        PartialWriter {
            fail_flush: true,
            ..Default::default()
        },
        PartialWriter {
            wrong_receipts: true,
            ..Default::default()
        },
        PartialWriter {
            pending: 1,
            ..Default::default()
        },
        PartialWriter {
            committed: vec![9],
            ..Default::default()
        },
    ] {
        assert!(
            commit_snapshot(&mut writer, &lines, &styles, modes, &[7, 8])
                .await
                .is_err()
        );
        assert!(writer.frames <= 1);
    }
}

/// The production fd writer reports receipts only after the encoded frame is
/// fully written to its local endpoint. This is byte-commit qualification, not
/// proof that an interactive user or terminal emulator saw the screen.
#[tokio::test]
async fn outbound_output_commit_uses_production_fd_writer() {
    use crate::host::async_runtime::AsyncAttachedTerminalFdLoopIo;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let (driver, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut writer =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let lines = vec!["snapshot-marker".to_string()];
    commit_snapshot(
        &mut writer,
        &lines,
        &[vec![]],
        AttachedTerminalOutputModes::default(),
        &[7],
    )
    .await
    .unwrap();
    assert_eq!(writer.pending_output_bytes(), 0);
    assert!(writer.take_committed_presentation_ids().is_empty());
    drop(writer);
    drop(driver_output);
    drop(driver);
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).unwrap();
    assert!(
        bytes
            .windows(b"snapshot-marker".len())
            .any(|window| window == b"snapshot-marker")
    );
}
