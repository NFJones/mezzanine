//! Complete-record progress state for generated shell input deliveries.
//!
//! The async pane worker owns one delivery at a time. This state retains the
//! exact unsent record suffix across partial PTY writes, distinguishes wrapper
//! output waits from receiver acknowledgements, and bounds progress without
//! inspecting or logging payload bytes.

use super::{Duration, Instant, PaneProcessInstance, RuntimeSideEffect};
use mez_mux::process::{
    PTY_INPUT_WRITE_CHUNK_BYTES, SHELL_INPUT_RECORD_ACK_BYTE, ShellInputDelivery, ShellInputPacing,
    shell_input_record_requires_ack,
};

/// Maximum time one shell-input record may make no transport progress.
pub(super) const SHELL_INPUT_RECORD_PROGRESS_TIMEOUT: Duration = Duration::from_secs(10);

/// Output condition required before the next complete record may be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShellInputProgressWait {
    /// Any fresh pane output advances generated wrapper delivery.
    OutputActivity,
    /// One fresh receiver record acknowledgement is required.
    Acknowledgement,
}

/// Target retained with one active shell delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShellInputDeliveryTarget {
    /// Exact adapter-owned process generation.
    Instance(PaneProcessInstance),
    /// Compatibility fallback identified by pane id.
    Pane(String),
}

/// Active complete-record delivery retained by one pane worker.
#[derive(Debug, Clone)]
pub(super) struct PendingShellInputDelivery {
    /// Process target that owns this delivery.
    pub(super) target: ShellInputDeliveryTarget,
    /// Typed bytes and pacing metadata.
    pub(super) delivery: ShellInputDelivery,
    /// First byte of the current record not yet accepted by the PTY.
    accepted: usize,
    /// First byte of the current complete record.
    record_start: usize,
    /// Exclusive end of the current complete record.
    record_end: usize,
    /// Output condition armed only after the complete record is accepted.
    wait: Option<ShellInputProgressWait>,
    /// Deadline for current-record progress or its acknowledgement.
    deadline: Instant,
}

impl PendingShellInputDelivery {
    /// Classifies one typed shell delivery without consuming unrelated effects.
    pub(super) fn from_effect(effect: &RuntimeSideEffect) -> Option<Self> {
        let (target, delivery) = match effect {
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: super::PaneProcessIoEffect::WriteShellInput { delivery },
            } => (
                ShellInputDeliveryTarget::Instance(instance.clone()),
                delivery.clone(),
            ),
            RuntimeSideEffect::WritePaneShellInput { pane_id, delivery } => (
                ShellInputDeliveryTarget::Pane(pane_id.clone()),
                delivery.clone(),
            ),
            _ => return None,
        };
        let record_end = next_record_end(&delivery.bytes, 0);
        Some(Self {
            target,
            delivery,
            accepted: 0,
            record_start: 0,
            record_end,
            wait: None,
            deadline: Instant::now() + SHELL_INPUT_RECORD_PROGRESS_TIMEOUT,
        })
    }

    /// Returns the exact suffix of the current complete record to retry.
    pub(super) fn pending_record_suffix(&self) -> &[u8] {
        &self.delivery.bytes[self.accepted..self.record_end]
    }

    /// Records PTY acceptance without advancing to a later record prematurely.
    pub(super) fn record_write(
        &mut self,
        written: usize,
        supports_acknowledgements: bool,
    ) -> Result<(), &'static str> {
        let remaining = self.record_end.saturating_sub(self.accepted);
        self.accepted = self.accepted.saturating_add(written.min(remaining));
        if self.accepted < self.record_end {
            self.deadline = Instant::now() + SHELL_INPUT_RECORD_PROGRESS_TIMEOUT;
            return Ok(());
        }
        self.wait = record_wait(
            &self.delivery,
            self.current_record(),
            supports_acknowledgements,
            self.record_end == self.delivery.bytes.len(),
            cfg!(target_os = "macos"),
        )?;
        self.deadline = Instant::now() + SHELL_INPUT_RECORD_PROGRESS_TIMEOUT;
        if self.wait.is_none() {
            self.advance_after_record();
        }
        Ok(())
    }

    /// Applies fresh pane output and returns whether it satisfied the wait.
    pub(super) fn observe_output(&mut self, output_seen: bool, acknowledgements: usize) -> bool {
        let satisfied = match self.wait {
            Some(ShellInputProgressWait::OutputActivity) => output_seen,
            Some(ShellInputProgressWait::Acknowledgement) => acknowledgements > 0,
            None => false,
        };
        if satisfied {
            self.wait = None;
            self.advance_after_record();
        }
        satisfied
    }

    /// Reports whether the final record and its required wait have completed.
    pub(super) fn is_complete(&self) -> bool {
        self.accepted == self.delivery.bytes.len() && self.wait.is_none()
    }

    /// Reports whether the worker must wait before writing another record.
    pub(super) fn is_waiting(&self) -> bool {
        self.wait.is_some()
    }

    /// Reports whether fresh receiver acknowledgements belong to this delivery.
    pub(super) fn is_waiting_for_acknowledgement(&self) -> bool {
        self.wait == Some(ShellInputProgressWait::Acknowledgement)
    }

    /// Reports whether the current record exceeded its bounded progress window.
    pub(super) fn timed_out(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// Returns the remaining bounded progress window for worker sleep planning.
    pub(super) fn remaining_progress_time(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }

    /// Returns the non-sensitive delivery identity for diagnostics/cancellation.
    pub(super) fn delivery_id(&self) -> Option<&str> {
        self.delivery.delivery_id.as_deref()
    }

    /// Reports whether a cancellation identity owns this pending delivery.
    pub(super) fn matches_delivery_id(&self, delivery_id: &str) -> bool {
        self.delivery_id() == Some(delivery_id)
    }

    /// Returns the pane id targeted by this delivery.
    pub(super) fn pane_id(&self) -> &str {
        match &self.target {
            ShellInputDeliveryTarget::Instance(instance) => &instance.pane_id,
            ShellInputDeliveryTarget::Pane(pane_id) => pane_id,
        }
    }

    /// Returns the complete current record, including its newline when present.
    fn current_record(&self) -> &[u8] {
        &self.delivery.bytes[self.record_start..self.record_end]
    }

    /// Advances to the next record after the current record and wait settle.
    fn advance_after_record(&mut self) {
        if self.record_end < self.delivery.bytes.len() {
            self.accepted = self.record_end;
            self.record_start = self.record_end;
            self.record_end = next_record_end(&self.delivery.bytes, self.accepted);
            self.deadline = Instant::now() + SHELL_INPUT_RECORD_PROGRESS_TIMEOUT;
        }
    }
}

/// Selects the wait required after one complete record is accepted.
fn record_wait(
    delivery: &ShellInputDelivery,
    record: &[u8],
    supports_acknowledgements: bool,
    final_record: bool,
    pacing_enabled: bool,
) -> Result<Option<ShellInputProgressWait>, &'static str> {
    if !pacing_enabled {
        return Ok(None);
    }
    match delivery.pacing {
        ShellInputPacing::ReceiverAcknowledged => {
            if !delivery.receiver_acknowledgements || !supports_acknowledgements {
                return Err("receiver-acknowledged shell delivery was not negotiated");
            }
            Ok(Some(ShellInputProgressWait::Acknowledgement))
        }
        ShellInputPacing::GeneratedSource if final_record => Ok(None),
        ShellInputPacing::GeneratedSource if supports_acknowledgements => {
            if shell_input_record_requires_ack(record) {
                Ok(Some(ShellInputProgressWait::Acknowledgement))
            } else {
                Ok(Some(ShellInputProgressWait::OutputActivity))
            }
        }
        ShellInputPacing::GeneratedSource => Ok(None),
    }
}

/// Returns the end of one newline-terminated record within the PTY chunk bound.
fn next_record_end(bytes: &[u8], start: usize) -> usize {
    let limit = start
        .saturating_add(PTY_INPUT_WRITE_CHUNK_BYTES)
        .min(bytes.len());
    bytes[start..limit]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(limit, |index| start + index + 1)
}

/// Counts receiver acknowledgement bytes in one pane-output batch.
pub(super) fn shell_input_acknowledgement_count(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .filter(|byte| **byte == SHELL_INPUT_RECORD_ACK_BYTE)
        .count()
}

/// Removes receiver-owned acknowledgement bytes while preserving visible output.
pub(super) fn filter_shell_input_acknowledgements(bytes: &mut Vec<u8>) -> usize {
    let acknowledgements = shell_input_acknowledgement_count(bytes);
    bytes.retain(|byte| *byte != SHELL_INPUT_RECORD_ACK_BYTE);
    acknowledgements
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies receiver payload records require negotiated acknowledgements,
    /// including the final sentinel record that releases later pane input.
    #[test]
    fn receiver_records_require_acknowledgements_through_final_record() {
        let delivery = ShellInputDelivery::receiver_acknowledged(
            b"first\nfinal\n".to_vec(),
            "delivery-1",
            true,
        );
        assert_eq!(
            record_wait(&delivery, b"first\n", true, false, true),
            Ok(Some(ShellInputProgressWait::Acknowledgement))
        );
        assert_eq!(
            record_wait(&delivery, b"final\n", true, true, true),
            Ok(Some(ShellInputProgressWait::Acknowledgement))
        );
        assert_eq!(
            record_wait(&delivery, b"final\n", false, true, true),
            Err("receiver-acknowledged shell delivery was not negotiated")
        );
    }

    /// Verifies unrelated output cannot advance strict receiver delivery but a
    /// fresh acknowledgement advances exactly one complete record.
    #[test]
    fn receiver_delivery_ignores_unrelated_output() {
        let effect = RuntimeSideEffect::WritePaneShellInput {
            pane_id: "%1".to_string(),
            delivery: ShellInputDelivery::receiver_acknowledged(
                b"first\nfinal\n".to_vec(),
                "delivery-1",
                true,
            ),
        };
        let mut pending = PendingShellInputDelivery::from_effect(&effect).unwrap();
        pending.record_write(b"first\n".len(), true).unwrap();
        assert!(pending.is_waiting());
        assert!(!pending.observe_output(true, 0));
        assert_eq!(pending.pending_record_suffix(), b"");
        assert!(pending.observe_output(true, 1));
        assert_eq!(pending.pending_record_suffix(), b"final\n");
    }

    /// Verifies a partial PTY write retries only the exact current-record
    /// suffix and does not arm an acknowledgement wait prematurely.
    #[test]
    fn partial_write_retains_current_record_suffix() {
        let effect = RuntimeSideEffect::WritePaneShellInput {
            pane_id: "%1".to_string(),
            delivery: ShellInputDelivery::receiver_acknowledged(
                b"abcdef\nfinal\n".to_vec(),
                "delivery-1",
                true,
            ),
        };
        let mut pending = PendingShellInputDelivery::from_effect(&effect).unwrap();
        pending.record_write(2, true).unwrap();
        assert_eq!(pending.pending_record_suffix(), b"cdef\n");
        assert!(!pending.is_waiting());
        pending.record_write(5, true).unwrap();
        assert!(pending.is_waiting());
    }

    /// Verifies generated wrapper source retains its historical contract: an
    /// intermediate record waits for progress while the final record does not.
    #[test]
    fn generated_source_does_not_wait_after_final_record() {
        let delivery = ShellInputDelivery::generated_source(b"first\nfinal\n".to_vec());
        assert_eq!(
            record_wait(&delivery, b"first\n", true, false, true),
            Ok(Some(ShellInputProgressWait::OutputActivity))
        );
        assert_eq!(
            record_wait(&delivery, b"final\n", true, true, true),
            Ok(None)
        );
        assert_eq!(
            record_wait(&delivery, b"first\n", true, false, false),
            Ok(None)
        );
    }

    /// Verifies acknowledgement filtering preserves visible bytes and reports
    /// every receiver progress byte in a coalesced output read.
    #[test]
    fn acknowledgement_filter_handles_visible_output_batches() {
        let mut bytes = b"visible\x1emore\x1e".to_vec();
        assert_eq!(filter_shell_input_acknowledgements(&mut bytes), 2);
        assert_eq!(bytes, b"visiblemore");
    }
}
