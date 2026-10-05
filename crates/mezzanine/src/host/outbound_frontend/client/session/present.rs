//! Exact retained-snapshot output commitment before remote receipt settlement.
//!
//! Uses the existing attached-terminal encoder/writer, not another renderer.
//! The client owns snapshot rows, styles, modes and receipt IDs together; callers
//! cannot substitute unrelated rows for a receipt-bearing frame. The consumed
//! operation rejects preexisting pending output or committed receipts, finishes
//! bounded writes without accepting another frame, then checks exact committed
//! IDs. Only that evidence allows explicit acknowledgement. Failure/cancellation
//! consumes the connection and sends no automatic replay; terminal restoration
//! and any already-started output tail remain the caller's responsibility.

use super::*;
use crate::host::async_runtime::{
    AsyncAttachedTerminalIo, DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
};

impl OutboundSessionClient {
    /// Presents this exact retained snapshot under one total output/ACK budget.
    /// Only completed output with exact writer-reported receipts can acknowledge
    /// remote presentation. This does not enter/restore terminal mode or drive
    /// input/events, and returns the unchanged owner only on validated settlement.
    pub(crate) async fn present<I: AsyncAttachedTerminalIo>(
        mut self,
        terminal: &mut I,
        key: &str,
        budget: Duration,
    ) -> Result<(Self, bool)> {
        validate_budget(1, 1, budget)?;
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err(MezError::invalid_args("outbound presentation key invalid"));
        }
        let expires = tokio::time::Instant::now() + budget;
        tokio::time::timeout_at(expires, async move {
            self.committed_view = None;
            self.painted_health = None;
            self.painted_cursor = None;
            self.client.discovery.validate()?;
            let mut painted_health = None;
            let (lines, styles) = if self.iroh_status_slot.is_some() {
                let remaining = expires.saturating_duration_since(tokio::time::Instant::now());
                let (owner, connected, quality) = self.sample_transport_health(remaining).await?;
                self = owner;
                if !connected {
                    return Err(MezError::invalid_state(
                        "outbound connection closed before presentation; reattach required",
                    ));
                }
                painted_health = Some(quality);
                crate::host::terminal::iroh_pill::compose(
                    &self.lines,
                    &self.styles,
                    self.iroh_status_slot,
                    connected,
                    quality,
                )
            } else {
                (self.lines.clone(), self.styles.clone())
            };
            let modes = crate::host::terminal::wire_modes::with_cursor_blink_epoch(
                self.modes,
                self.cursor_blink_epoch,
            );
            commit_snapshot(terminal, &lines, &styles, modes, &self.receipts).await?;
            self.client.discovery.validate()?;
            let remaining = expires.saturating_duration_since(tokio::time::Instant::now());
            if remaining < Duration::from_millis(100) {
                return Err(MezError::invalid_state(
                    "outbound output committed but acknowledgement budget exhausted",
                ));
            }
            let (mut owner, acknowledged) = self.acknowledge_presented(key, remaining).await?;
            if acknowledged {
                owner.painted_health = painted_health;
                owner.painted_cursor = Some(mez_mux::attached_client::cursor_phase_visible(modes));
                owner.committed_view = owner
                    .view_identity
                    .clone()
                    .map(|identity| (identity, owner.snapshot_size.0, owner.snapshot_size.1));
            }
            Ok((owner, acknowledged))
        })
        .await
        .map_err(|_| {
            MezError::invalid_state(
                "outbound presentation timed out; output or acknowledgement may be incomplete",
            )
        })?
    }
}

impl OutboundSessionClient {
    /// Refreshes only the decoration of previously committed, receipt-settled
    /// output. A changed sample writes the exact retained base with no receipts
    /// and sends no ACK or view request. Errors consume this owner; callers still
    /// own cleanup of started output and invalidate after external writer use.
    pub(crate) async fn repaint_transport_status<I: AsyncAttachedTerminalIo>(
        mut self,
        terminal: &mut I,
        budget: Duration,
    ) -> Result<(Self, bool)> {
        validate_budget(1, 1, budget)?;
        if self.iroh_status_slot.is_none()
            || self.painted_health.is_none()
            || !self.receipts.is_empty()
        {
            return Err(MezError::conflict(
                "outbound status repaint requires settled output",
            ));
        }
        tokio::time::timeout(budget, async move {
            let (owner, connected, quality) = self.sample_transport_health(budget).await?;
            self = owner;
            if !connected {
                return Err(MezError::invalid_state(
                    "outbound status connection closed; reattach required",
                ));
            }
            if self.painted_health == Some(quality) {
                return Ok((self, false));
            }
            let (lines, styles) = crate::host::terminal::iroh_pill::compose(
                &self.lines,
                &self.styles,
                self.iroh_status_slot,
                connected,
                quality,
            );
            let modes = crate::host::terminal::wire_modes::with_cursor_blink_epoch(
                self.modes,
                self.cursor_blink_epoch,
            );
            commit_snapshot(terminal, &lines, &styles, modes, &[]).await?;
            self.client.discovery.validate()?;
            self.painted_health = Some(quality);
            self.painted_cursor = Some(mez_mux::attached_client::cursor_phase_visible(modes));
            Ok((self, true))
        })
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound status repaint timed out; output may be incomplete")
        })?
    }
}

impl OutboundSessionClient {
    /// Repaints a changed local cursor phase on receipt-settled output without
    /// remote requests or acknowledgements. It preserves the last painted status
    /// decoration, server rows and revision. Writer changes require explicit
    /// invalidation; failure consumes ownership and leaves tail cleanup to callers.
    pub(crate) async fn repaint_cursor_phase<I: AsyncAttachedTerminalIo>(
        mut self,
        terminal: &mut I,
        budget: Duration,
    ) -> Result<(Self, bool)> {
        validate_budget(1, 1, budget)?;
        if self.painted_cursor.is_none()
            || !self.receipts.is_empty()
            || (self.iroh_status_slot.is_some() && self.painted_health.is_none())
        {
            return Err(MezError::conflict(
                "outbound cursor repaint requires settled output",
            ));
        }
        tokio::time::timeout(budget, async move {
            self.client.discovery.validate()?;
            let modes = crate::host::terminal::wire_modes::with_cursor_blink_epoch(
                self.modes,
                self.cursor_blink_epoch,
            );
            let visible = mez_mux::attached_client::cursor_phase_visible(modes);
            if self.painted_cursor == Some(visible) {
                return Ok((self, false));
            }
            let (lines, styles) = if let Some(quality) = self.painted_health {
                crate::host::terminal::iroh_pill::compose(
                    &self.lines,
                    &self.styles,
                    self.iroh_status_slot,
                    true,
                    quality,
                )
            } else {
                (self.lines.clone(), self.styles.clone())
            };
            commit_snapshot(terminal, &lines, &styles, modes, &[]).await?;
            self.client.discovery.validate()?;
            self.painted_cursor = Some(visible);
            Ok((self, true))
        })
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound cursor repaint timed out; output may be incomplete")
        })?
    }
}

/// Commits one snapshot through the existing writer, never replacing a started
/// frame. The adapter must report exact receipt commitment, not only byte counts.
async fn commit_snapshot<I: AsyncAttachedTerminalIo>(
    terminal: &mut I,
    lines: &[String],
    styles: &[Vec<mez_terminal::TerminalStyleSpan>],
    modes: mez_mux::presentation::AttachedTerminalOutputModes,
    receipts: &[u64],
) -> Result<()> {
    if terminal.pending_output_bytes() != 0
        || !terminal.take_committed_presentation_ids().is_empty()
    {
        return Err(MezError::conflict(
            "outbound presentation writer already owns another frame",
        ));
    }
    let limit = DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES;
    let mut report = terminal
        .write_owned_styled_output_with_modes_bounded_and_receipts(
            lines.to_vec(),
            styles.to_vec(),
            modes,
            receipts.to_vec(),
            limit,
        )
        .await?;
    while report.is_partial() {
        if report.bytes_written == 0 {
            // Mixed readiness can repeatedly prefer unread input. Keep input
            // untouched and pace bounded nonblocking flush attempts instead;
            // the enclosing presentation deadline bounds a stalled output.
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        report = terminal.flush_pending_output(limit).await?;
    }
    if terminal.pending_output_bytes() != 0
        || terminal.take_committed_presentation_ids() != receipts
    {
        return Err(MezError::invalid_state(
            "outbound presentation commitment evidence changed",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
