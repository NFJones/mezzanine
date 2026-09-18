//! Bounded response watchdog for the attached primary client loop.
//!
//! The interactive client writes a terminal-step request and then awaits the
//! daemon response without any bound, so a stalled actor reads as a frozen
//! terminal with no feedback. This module owns the state machine that decides
//! when a local busy hint is due and how its elapsed time is rendered. It never
//! disconnects: the caller keeps awaiting the response, paints the hint when one
//! is due, and clears it when the response arrives.

use std::time::{Duration, Instant};

use mez_terminal::TerminalStyleSpan;

/// Delay before the first busy hint is painted.
pub(super) const ATTACH_RESPONSE_HINT_THRESHOLD: Duration = Duration::from_millis(500);

/// Interval the caller waits between watchdog checks while a response is due.
pub(super) const ATTACH_RESPONSE_HINT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Tracks how long one daemon response has been outstanding.
#[derive(Debug, Clone)]
pub(super) struct AttachResponseWatchdog {
    started_at: Instant,
    threshold: Duration,
    painted_seconds: Option<u64>,
}

impl AttachResponseWatchdog {
    /// Starts a watchdog for one request using the default threshold.
    pub(super) fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            threshold: ATTACH_RESPONSE_HINT_THRESHOLD,
            painted_seconds: None,
        }
    }

    /// Starts a watchdog with an explicit threshold for focused tests.
    #[cfg(test)]
    pub(super) fn with_threshold(started_at: Instant, threshold: Duration) -> Self {
        Self {
            started_at,
            threshold,
            painted_seconds: None,
        }
    }

    /// Returns the interval the caller should wait between checks.
    pub(super) fn poll_interval(&self) -> Duration {
        ATTACH_RESPONSE_HINT_POLL_INTERVAL
    }

    /// Returns the hint to paint when it is due, otherwise `None`.
    ///
    /// The hint is repainted once per elapsed second so an operator watching a
    /// slow daemon sees the wait advance, and it is never returned twice for the
    /// same second.
    pub(super) fn pending_hint(&mut self, now: Instant) -> Option<String> {
        let elapsed = now.saturating_duration_since(self.started_at);
        if elapsed < self.threshold {
            return None;
        }
        let seconds = elapsed.as_secs();
        if self.painted_seconds == Some(seconds) {
            return None;
        }
        self.painted_seconds = Some(seconds);
        Some(format!("waiting for daemon ({seconds}s)"))
    }

    /// Records that the caller is clearing the hint, reporting whether one was
    /// painted so the caller knows to repaint its frame.
    pub(super) fn clear(&mut self) -> bool {
        self.painted_seconds.take().is_some()
    }
}

/// Overlays one busy-hint row onto a cached client frame.
///
/// The caller keeps the last composed frame so a stalled daemon can be answered
/// with local feedback: this replaces exactly one row with the already-fitted
/// hint text and drops that row's style spans so server styling cannot bleed
/// into locally authored text. Other rows, their spans, and the frame length are
/// left untouched, and a row outside the frame is a no-op.
pub(super) fn compose_operator_hint(
    lines: &[String],
    line_style_spans: &[Vec<TerminalStyleSpan>],
    row: usize,
    hint: &str,
) -> (Vec<String>, Vec<Vec<TerminalStyleSpan>>) {
    let mut hinted_lines = lines.to_vec();
    let mut hinted_spans = line_style_spans.to_vec();
    let Some(line) = hinted_lines.get_mut(row) else {
        return (hinted_lines, hinted_spans);
    };
    *line = hint.to_string();
    if let Some(row_spans) = hinted_spans.get_mut(row) {
        row_spans.clear();
    }
    (hinted_lines, hinted_spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the hint stays hidden before the threshold, appears with the
    /// elapsed seconds afterwards, advances once per second, and stops
    /// repeating within a second.
    #[test]
    fn attach_response_watchdog_hint_threshold_and_progress() {
        let start = Instant::now();
        let mut watchdog =
            AttachResponseWatchdog::with_threshold(start, Duration::from_millis(500));
        assert_eq!(
            watchdog.pending_hint(start + Duration::from_millis(499)),
            None
        );
        assert_eq!(
            watchdog.pending_hint(start + Duration::from_millis(500)),
            Some("waiting for daemon (0s)".to_string())
        );
        assert_eq!(
            watchdog.pending_hint(start + Duration::from_millis(900)),
            None
        );
        assert_eq!(
            watchdog.pending_hint(start + Duration::from_millis(1_000)),
            Some("waiting for daemon (1s)".to_string())
        );
        assert_eq!(watchdog.poll_interval(), ATTACH_RESPONSE_HINT_POLL_INTERVAL);
    }

    /// Verifies clearing reports whether a hint was painted and resets the
    /// watchdog so a later request starts hidden again.
    #[test]
    fn attach_response_watchdog_clear_reports_painted_state() {
        let start = Instant::now();
        let mut unpainted = AttachResponseWatchdog::new(start);
        assert!(!unpainted.clear(), "a fresh watchdog painted nothing");

        let mut painted = AttachResponseWatchdog::with_threshold(start, Duration::from_millis(1));
        assert!(
            painted
                .pending_hint(start + Duration::from_millis(2))
                .is_some()
        );
        assert!(painted.clear(), "clearing reports the painted hint");
        assert!(!painted.clear(), "the hint is cleared only once");
    }

    /// Verifies the hint overlay replaces exactly one row, drops that row's
    /// spans, leaves the rest of the frame intact, and ignores an out-of-range
    /// row.
    #[test]
    fn attach_response_watchdog_composes_hint_overlay() {
        let lines = vec!["server row one".to_string(), "server row two".to_string()];
        let spans = vec![
            vec![],
            vec![TerminalStyleSpan {
                start: 0,
                length: 1,
                rendition: mez_terminal::GraphicRendition::default(),
            }],
        ];

        let (hinted_lines, hinted_spans) =
            compose_operator_hint(&lines, &spans, 1, "waiting for daemon (1s)");
        assert_eq!(hinted_lines[0], "server row one");
        assert_eq!(hinted_lines[1], "waiting for daemon (1s)");
        assert_eq!(hinted_lines.len(), lines.len());
        assert!(
            hinted_spans[1].is_empty(),
            "local text carries no server spans"
        );
        assert_eq!(hinted_spans[0], spans[0]);

        let (untouched_lines, untouched_spans) =
            compose_operator_hint(&lines, &spans, 7, "waiting for daemon (2s)");
        assert_eq!(untouched_lines, lines);
        assert_eq!(untouched_spans, spans);
    }
}
