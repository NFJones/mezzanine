//! Regression tests for terminal client io loop behavior.

use crate::terminal::client_loop::{
    AttachedTerminalOutputModes, AttachedTerminalOutputWriteReport,
};
use crate::terminal::screen::TerminalStyleSpan;
use crate::terminal::{
    AttachedTerminalClientLoopConfig, AttachedTerminalClientLoopIo, AttachedTerminalFdReadiness,
    AttachedTerminalFdRole, ClientStatusKind, ClientStatusLine, ClientViewRole, MuxAction,
    RenderedClientView, Result, Size, TerminalClientLoopAction, TerminalClientLoopConfig,
    TerminalCursorStyle, TerminalFdInterest, UiTheme, run_attached_terminal_client_loop,
};

#[derive(Default)]
struct FakeAttachedTerminalLoopIo {
    /// Stores the readiness batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    readiness_batches: Vec<Vec<AttachedTerminalFdReadiness>>,
    /// Stores the input batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    input_batches: Vec<Vec<u8>>,
    /// Stores the written batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    written_batches: Vec<Vec<String>>,
    /// Stores the optional per-call output write budget for this fake.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    write_budget_per_call: Option<usize>,
    /// Stores the retained output bytes waiting for a later flush.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pending_output_bytes: usize,
    /// Stores the fully rendered lines that complete when pending bytes drain.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pending_output_lines: Option<Vec<String>>,
}

impl AttachedTerminalClientLoopIo for FakeAttachedTerminalLoopIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness(&mut self) -> Result<Vec<AttachedTerminalFdReadiness>> {
        if self.readiness_batches.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.readiness_batches.remove(0))
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        if self.input_batches.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = self.input_batches.remove(0);
        input.truncate(max_bytes);
        Ok(input)
    }

    /// Runs the write output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_output(&mut self, lines: &[String]) -> Result<usize> {
        self.written_batches.push(lines.to_vec());
        Ok(lines.iter().map(String::len).sum())
    }

    /// Returns retained output bytes awaiting a later writable terminal pass.
    fn pending_output_bytes(&self) -> usize {
        self.pending_output_bytes
    }

    /// Flushes retained output bytes without accepting a new rendered frame.
    fn flush_pending_output(
        &mut self,
        max_bytes: usize,
    ) -> Result<AttachedTerminalOutputWriteReport> {
        if self.pending_output_bytes == 0 {
            return Ok(AttachedTerminalOutputWriteReport::completed(0));
        }

        let budget = self
            .write_budget_per_call
            .unwrap_or(max_bytes)
            .min(max_bytes);
        let accepted = self.pending_output_bytes.min(budget);
        self.pending_output_bytes = self.pending_output_bytes.saturating_sub(accepted);
        if self.pending_output_bytes == 0 {
            if let Some(lines) = self.pending_output_lines.take() {
                self.written_batches.push(lines);
            }
            Ok(AttachedTerminalOutputWriteReport::completed(accepted))
        } else {
            Ok(AttachedTerminalOutputWriteReport {
                bytes_written: accepted,
                completed: false,
                pending_bytes: self.pending_output_bytes,
            })
        }
    }

    /// Writes at most `max_bytes` of one styled terminal frame.
    fn write_styled_output_with_modes_bounded(
        &mut self,
        lines: &[String],
        _line_style_spans: &[Vec<TerminalStyleSpan>],
        _modes: AttachedTerminalOutputModes,
        max_bytes: usize,
    ) -> Result<AttachedTerminalOutputWriteReport> {
        let total_bytes = lines.iter().map(String::len).sum::<usize>();
        let budget = self
            .write_budget_per_call
            .unwrap_or(max_bytes)
            .min(max_bytes);
        let accepted = total_bytes.min(budget);
        if accepted == total_bytes {
            self.written_batches.push(lines.to_vec());
            return Ok(AttachedTerminalOutputWriteReport::completed(accepted));
        }

        self.pending_output_bytes = total_bytes.saturating_sub(accepted);
        self.pending_output_lines = Some(lines.to_vec());
        Ok(AttachedTerminalOutputWriteReport {
            bytes_written: accepted,
            completed: false,
            pending_bytes: self.pending_output_bytes,
        })
    }
}

/// Verifies attached terminal client loop pumps input output and stops on hangup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_client_loop_pumps_input_output_and_stops_on_hangup() {
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: false,
                writable: false,
                hangup: true,
                error: false,
            }],
        ],
        input_batches: vec![b"\x01c".to_vec()],
        written_batches: Vec::new(),
        ..Default::default()
    };
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(10, 2).unwrap(),
        client_size: Size::new(10, 2).unwrap(),
        lines: vec!["pane      ".to_string(), "old       ".to_string()],
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
    let status = ClientStatusLine {
        kind: ClientStatusKind::Plain,
        text: "ready".to_string(),
    };

    let report = run_attached_terminal_client_loop(
        &mut io,
        || Ok(Some((view.clone(), Some(status.clone())))),
        &TerminalClientLoopConfig::default(),
        AttachedTerminalClientLoopConfig {
            max_iterations: 4,
            max_input_bytes: 64,
        },
    )
    .unwrap();

    assert_eq!(report.iterations, 2);
    assert_eq!(
        report.actions,
        vec![TerminalClientLoopAction::ExecuteMux(MuxAction::NewWindow)]
    );
    assert_eq!(report.output_frames, 1);
    assert_eq!(io.written_batches.len(), 1);
    assert_eq!(io.written_batches[0][1], "ready     ");
    assert_eq!(report.input_hangups, 1);
    assert!(report.error_roles.is_empty());
}

/// Verifies the attached-terminal loop retains partial writes and finishes the
/// frame on a later writable pass instead of spinning inside one iteration.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_client_loop_retains_partial_output_until_next_writable_pass() {
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            }],
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            }],
        ],
        write_budget_per_call: Some(3),
        ..Default::default()
    };
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(6, 1).unwrap(),
        client_size: Size::new(6, 1).unwrap(),
        lines: vec!["abcdef".to_string()],
        line_style_spans: vec![Vec::new()],
        selection: None,
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: false,
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
            max_iterations: 2,
            max_input_bytes: 64,
        },
    )
    .unwrap();

    assert_eq!(report.iterations, 2);
    assert_eq!(report.output_frames, 1);
    assert_eq!(report.bytes_written, 6);
    assert_eq!(report.partial_writes, 1);
    assert_eq!(report.pending_output_bytes, 0);
    assert_eq!(io.written_batches, vec![vec!["abcdef".to_string()]]);
}

/// Verifies attached terminal client loop rejects zero limits.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_client_loop_rejects_zero_limits() {
    let mut io = FakeAttachedTerminalLoopIo::default();
    let error = run_attached_terminal_client_loop(
        &mut io,
        || Ok(None),
        &TerminalClientLoopConfig::default(),
        AttachedTerminalClientLoopConfig {
            max_iterations: 0,
            max_input_bytes: 1,
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies the default attached-client loop can read a large foreground paste
/// as one logical terminal input event. This keeps clipboard paste throughput
/// high enough that ordinary shell/editor pastes are not truncated by a small
/// harness read ceiling.
#[test]
fn attached_terminal_client_loop_default_limits_allow_large_paste_reads() {
    let config = AttachedTerminalClientLoopConfig::default();

    assert!(config.max_iterations >= 128);
    assert!(config.max_input_bytes >= 1024 * 1024);
}
