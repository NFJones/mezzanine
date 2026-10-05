//! Negotiated event/input wait retaining one consumed request owner.
//!
//! Event polling consumes the client, so local input cannot cancel that poll
//! and then reuse its desynchronized stream. If input wins, bounded bytes are
//! retained while the exact reply settles. If events win, the cancellation-safe
//! terminal read is dropped without consuming input. Errors retire the client;
//! no detached workers, reconnects, setup retries or input replay are introduced.

use super::*;

/// Waits for one finite event reply or local input, preserving session ownership
/// through settlement in either ordering. Input latency includes at most the
/// negotiated poll wait plus its bounded transport settlement.
pub(super) async fn negotiated<I: AsyncAttachedTerminalIo>(
    session: OutboundSessionClient,
    terminal: &mut I,
    budget: Duration,
) -> Result<(OutboundSessionClient, Option<Vec<u8>>, AttachRenderAction)> {
    let event = session.poll_events(25, budget);
    tokio::pin!(event);
    tokio::select! {
        biased;
        input = terminal.read_input(512) => {
            let input = input?;
            let (session, action, _) = event.await?;
            Ok((session, Some(input), action))
        }
        result = &mut event => {
            let (session, action, _) = result?;
            Ok((session, None, action))
        }
    }
}
