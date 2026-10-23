//! Bounded production diagnostics for retired outbound frontend pipelines.
//!
//! Supervision owns one future per admitted local stream. When that future
//! retires with an error, the supervising owner records exactly one line on the
//! broker's existing private stderr sink. Records carry a static stage label and
//! a static error class only: they never include frames, request bodies, profile
//! aliases, device proofs, invitation tokens, credentials, terminal rows or
//! input, clipboard/X11 payloads, or arbitrary remote error text.
//!
//! A peer that leaves before sending any setup request is expected local
//! lifecycle, not a failed request, and reports nothing. Caller cancellation
//! drops the owned future without a report, and a failure never retries,
//! reconnects, replays input or disturbs sibling pipelines.

use crate::error::{MezError, MezErrorKind};

/// Broker error text emitted when a peer disconnects before sending setup.
///
/// The setup owner and this classifier share the constant, so an expected local
/// disconnect is recognised from one pinned text instead of a duplicated
/// literal in two modules.
pub(crate) const SETUP_UNAVAILABLE_MESSAGE: &str = "outbound frontend setup unavailable";

/// Hello admission reports this when a local peer closed before sending.
///
/// A same-user client that connects and disconnects without a hello is an
/// expected pre-setup disconnect, so it must stay as quiet as the setup case.
const HELLO_UNAVAILABLE_MESSAGE: &str = "outbound frontend hello unavailable";

/// Static stage labels for bounded pipeline failure evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PipelineStage {
    /// Authenticating and admitting one accepted local stream.
    Admit,
    /// Receiving the strict setup request and resolving the profile.
    Profile,
    /// Delivering pairing evidence through the pairing owner.
    PairingDelivery,
    /// Connecting to the initialized remote session.
    Connect,
    /// Initializing a host-only, X11, clipboard or display session.
    Initialize,
    /// Delivering the host-only session list.
    HostOnlyDelivery,
    /// Supervising an X11 session.
    X11Supervision,
    /// Delivering the first display view.
    FirstView,
    /// Delivering a later display view.
    ViewDelivery,
}

impl PipelineStage {
    /// Returns the stable diagnostic label; never payload or peer material.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::Profile => "profile",
            Self::PairingDelivery => "pairing-delivery",
            Self::Connect => "connect",
            Self::Initialize => "initialize",
            Self::HostOnlyDelivery => "host-only-delivery",
            Self::X11Supervision => "x11-supervision",
            Self::FirstView => "first-view",
            Self::ViewDelivery => "view-delivery",
        }
    }
}

/// One retired pipeline's static failure evidence.
pub(super) struct PipelineFailure {
    stage: PipelineStage,
    class: &'static str,
    expected_local_disconnect: bool,
}

impl PipelineFailure {
    /// Classifies one pipeline error without retaining any error text.
    pub(super) fn classify(stage: PipelineStage, error: &MezError) -> Self {
        let expected_local_disconnect = error.kind() == MezErrorKind::InvalidState
            && ((stage == PipelineStage::Admit && error.message() == HELLO_UNAVAILABLE_MESSAGE)
                || (stage == PipelineStage::Profile
                    && error.message() == SETUP_UNAVAILABLE_MESSAGE));
        Self {
            stage,
            class: error_class(error),
            expected_local_disconnect,
        }
    }

    /// Returns the bounded private stderr line for an unexpected retirement.
    ///
    /// Expected pre-setup disconnects return `None`, so a normal local client is
    /// not reported as a failed request.
    pub(super) fn report_line(&self) -> Option<String> {
        (!self.expected_local_disconnect).then(|| {
            format!(
                "mez host: outbound frontend retired: stage={} class={}",
                self.stage.label(),
                self.class
            )
        })
    }

    /// Returns the static stage for focused regressions.
    #[cfg(test)]
    pub(super) fn stage(&self) -> PipelineStage {
        self.stage
    }
}

/// Emits one bounded private diagnostic for a failed pipeline.
pub(super) fn report_failure(failure: &PipelineFailure) {
    let Some(line) = failure.report_line() else {
        return;
    };
    #[cfg(test)]
    record_reported_failure(&line);
    // Reporting one failed peer must never become a supervisor failure: the
    // broker diagnostic sink is an append-only log that can be full or fail, and
    // a panicking writer would unwind the shared listener and drop sibling
    // pipelines. Sink errors are therefore ignored here.
    write_private_diagnostic(&line);
}

/// Writes one bounded line to the private sink, ignoring sink failures.
fn write_private_diagnostic(line: &str) {
    use std::io::Write as _;
    let mut sink = std::io::stderr();
    let _ = sink.write_all(line.as_bytes());
    let _ = sink.write_all(b"\n");
    let _ = sink.flush();
}

#[cfg(test)]
static REPORTED_FAILURES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Returns and clears failures recorded by this test process.
///
/// Only one focused supervisor regression inspects this sink, so parallel tests
/// cannot consume another test's record.
#[cfg(test)]
pub(super) fn take_reported_failures() -> Vec<String> {
    let mut reported = REPORTED_FAILURES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::take(&mut *reported)
}

#[cfg(test)]
fn record_reported_failure(line: &str) {
    let mut reported = REPORTED_FAILURES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if reported.len() < 64 {
        reported.push(line.to_string());
    }
}

/// Maps one error kind to a static class; no message or remote text survives.
fn error_class(error: &MezError) -> &'static str {
    match error.kind() {
        MezErrorKind::InvalidArgs => "invalid_args",
        MezErrorKind::InvalidState => "invalid_state",
        MezErrorKind::Config => "config",
        MezErrorKind::Io => io_class(error.io_kind()),
        MezErrorKind::Conflict => "conflict",
        MezErrorKind::NotFound => "not_found",
        MezErrorKind::Forbidden => "forbidden",
        MezErrorKind::RateLimited => "rate_limited",
        MezErrorKind::NotImplemented => "not_implemented",
    }
}

/// Maps one stdlib I/O kind to a static class without retaining the error.
fn io_class(kind: Option<std::io::ErrorKind>) -> &'static str {
    match kind {
        Some(std::io::ErrorKind::UnexpectedEof) => "io_unexpected_eof",
        Some(std::io::ErrorKind::BrokenPipe) => "io_broken_pipe",
        Some(std::io::ErrorKind::ConnectionReset) => "io_connection_reset",
        Some(std::io::ErrorKind::ConnectionAborted) => "io_connection_aborted",
        Some(std::io::ErrorKind::TimedOut) => "io_timed_out",
        Some(std::io::ErrorKind::WouldBlock) => "io_would_block",
        Some(_) => "io_other",
        None => "io",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HELLO_UNAVAILABLE_MESSAGE, PipelineFailure, PipelineStage, SETUP_UNAVAILABLE_MESSAGE,
    };
    use crate::MezError;

    /// A same-user client that connects and disconnects before sending a hello
    /// is an expected pre-setup disconnect and must not be reported as a failed
    /// request.
    #[test]
    fn pre_hello_disconnect_stays_quiet() {
        let failure = PipelineFailure::classify(
            PipelineStage::Admit,
            &MezError::invalid_state(HELLO_UNAVAILABLE_MESSAGE),
        );
        assert!(failure.report_line().is_none());
        let failure = PipelineFailure::classify(
            PipelineStage::Profile,
            &MezError::invalid_state(SETUP_UNAVAILABLE_MESSAGE),
        );
        assert!(failure.report_line().is_none());
    }

    /// Any other admission failure is still reported with a static stage and
    /// class, and without carrying the error text.
    #[test]
    fn unexpected_admission_failure_is_reported_without_payload() {
        let failure = PipelineFailure::classify(
            PipelineStage::Admit,
            &MezError::invalid_args("SENTINEL admission detail"),
        );
        let line = failure
            .report_line()
            .expect("unexpected failure is reported");
        assert!(line.contains("stage=admit"), "{line}");
        assert!(line.contains("class=invalid_args"), "{line}");
        assert!(!line.contains("SENTINEL"), "{line}");
    }
}
