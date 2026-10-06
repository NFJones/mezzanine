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
            let (session, action, id) = event.await?;
            let action = settled_action(&session, action, id);
            Ok((session, Some(input), action))
        }
        result = &mut event => {
            let (session, action, id) = result?;
            let action = settled_action(&session, action, id);
            Ok((session, None, action))
        }
    }
}

/// Waits for a complete item exchange without cancelling and reusing its stream
/// when local input arrives first. Effect application belongs to the foreground
/// after validation, not this race; event-first leaves input unread.
pub(super) async fn items<I: AsyncAttachedTerminalIo>(
    session: OutboundSessionClient,
    terminal: &mut I,
    budget: Duration,
) -> Result<(OutboundSessionClient, Option<Vec<u8>>, FrontendItem)> {
    let item = session.poll_items(25, budget);
    tokio::pin!(item);
    tokio::select! {
        biased;
        input = terminal.read_input(512) => {
            let input = input?;
            let (session, item) = item.await?;
            Ok((session, Some(input), item))
        }
        result = &mut item => {
            let (session, item) = result?;
            Ok((session, None, item))
        }
    }
}

/// Suppresses only identified ordinary redraws represented by exact committed
/// output. Received metadata, unsettled receipts or foreign geometry cannot
/// establish coverage; immediate/invalidation actions remain independently live.
pub(super) fn settled_action(
    session: &OutboundSessionClient,
    action: AttachRenderAction,
    event_id: Option<u64>,
) -> AttachRenderAction {
    let committed = session.committed_view.as_ref().is_some_and(|base| {
        session.receipts.is_empty()
            && (base.1, base.2) == session.snapshot_size
            && session.view_identity.as_deref() == Some(base.0.as_str())
    });
    if committed
        && action == AttachRenderAction::View
        && matches!((event_id, session.event_cutoff), (Some(id), Some(cutoff)) if id <= cutoff)
    {
        AttachRenderAction::None
    } else {
        action
    }
}
