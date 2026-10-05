//! Internal request-driven snapshot foreground lifecycle, not CLI activation.
//!
//! Composes the retained session, receipt-aware writer and bounded input exchange
//! without another terminal encoder. One foreground owner enters presentation,
//! retires its connection on cancellation/EOF/error, and attempts restoration on
//! every explicit return. The caller's concrete terminal guard still owns raw
//! mode and emergency restoration if this entire future is abandoned. This
//! polling fixture path does not implement pushed events, X11, clipboard, local
//! health overlays or the complete production attach scheduling contract.

use super::*;
use crate::host::async_runtime::AsyncAttachedTerminalIo;
use mez_mux::layout::Size;

/// Polling cadence for this internal snapshot path only; it is not advertised
/// as the production event-driven attach loop or configured render-rate policy.
const SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(250);

impl OutboundSessionClient {
    /// Runs an internal primary/observer foreground until EOF or cancellation.
    /// Mutations get distinct invocation-local keys and are never replayed.
    /// Explicit returns attempt presentation restoration, preserving the causal
    /// operation error if both operation and restoration fail. Whole-future drop
    /// cannot await restoration; the caller must retain its terminal guard.
    pub(crate) async fn run_snapshot_foreground<I, C>(
        self,
        terminal: &mut I,
        size: Size,
        request_budget: Duration,
        cancellation: C,
    ) -> Result<()>
    where
        I: AsyncAttachedTerminalIo,
        C: std::future::Future<Output = ()>,
    {
        validate_budget(size.columns, size.rows, request_budget)?;
        tokio::pin!(cancellation);
        let result = {
            // This future owns the session even during entry. Cancellation or
            // entry failure drops it before the common restoration path runs.
            let lifecycle = async {
                tokio::time::timeout(request_budget, terminal.enter_presentation())
                    .await
                    .map_err(|_| {
                        MezError::invalid_state("outbound presentation entry timed out")
                    })??;
                run_active(self, terminal, size, request_budget).await
            };
            tokio::select! {
                biased;
                () = &mut cancellation => Ok(()),
                result = lifecycle => result,
            }
        };
        let restored = tokio::time::timeout(request_budget, terminal.restore_presentation())
            .await
            .map_err(|_| MezError::invalid_state("outbound presentation restoration timed out"))
            .and_then(|result| result);
        match result {
            Ok(()) => restored,
            Err(error) => {
                let _ = restored;
                Err(error)
            }
        }
    }
}

/// Drives one retained connection, presenting exact snapshots before awaiting
/// more input. Read cancellation during an idle poll does not consume bytes;
/// any mutation failure consumes the connection instead of returning a retry.
async fn run_active<I: AsyncAttachedTerminalIo>(
    mut session: OutboundSessionClient,
    terminal: &mut I,
    mut size: Size,
    budget: Duration,
) -> Result<()> {
    let nonce = rand::random::<u128>();
    let mut sequence = 0_u64;
    loop {
        let key = next_key(nonce, &mut sequence)?;
        session = session.present(terminal, &key, budget).await?.0;
        let input = tokio::time::timeout(SNAPSHOT_POLL_INTERVAL, terminal.read_input(512)).await;
        if let Some(updated) = terminal.terminal_size().await? {
            size = updated;
            validate_budget(size.columns, size.rows, budget)?;
        }
        match input {
            Ok(Err(error)) => return Err(error),
            Ok(Ok(bytes)) if bytes.is_empty() => return Ok(()),
            Ok(Ok(bytes)) if session.summary.granted_role == "primary" => {
                let key = next_key(nonce, &mut sequence)?;
                let (updated, acknowledgement) = session
                    .step(size.columns, size.rows, &bytes, &key, budget)
                    .await?;
                session = updated;
                if acknowledgement.client_detached || acknowledgement.session_terminated {
                    return Ok(());
                }
            }
            // Observers drain local bytes without forwarding or acquiring input
            // authority. A timer requests another snapshot, not an input retry.
            Ok(Ok(_)) | Err(_) => {}
        }
        session = session.snapshot(size.columns, size.rows, budget).await?.0;
    }
}

/// Allocates an exact logical-operation key without saturation or PID reuse.
fn next_key(nonce: u128, sequence: &mut u64) -> Result<String> {
    *sequence = sequence.checked_add(1).ok_or_else(|| {
        MezError::invalid_state("outbound foreground operation identity exhausted")
    })?;
    Ok(format!("outbound-{nonce:032x}-{sequence}"))
}

#[cfg(test)]
mod tests;
