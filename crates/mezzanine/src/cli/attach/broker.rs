//! Concrete terminal ownership for active-broker paired-host attachments.
//!
//! The independently validated setup owns one exact remote session through IPC,
//! never an endpoint key. A concrete terminal guard retains raw-mode and emergency
//! reset responsibility across foreground cancellation. Explicit return attempts
//! bounded restoration even after output or transport failure; the causal error
//! takes precedence. Closing this frontend cannot terminate the shared broker.
//! Explicit X11 owns channel retirement and client-local credentials alongside
//! foreground restoration. This adapter does not launch brokers, pair identities,
//! retry creation or negotiate pushed renders. Discovery precedes this owner.

use super::{AsRawFd, AsyncAttachedTerminalPresentationGuard, MezError, Result, Size, io};
use crate::cli::control_client::broker_attach::BrokerAttachment;

/// Drives the exact prepared attachment on stdin/stdout with retained cleanup.
/// Signals retire only this consumed frontend. Backend clipboard work already
/// started remains best-effort; neither restoration nor cancellation replays it.
pub(super) async fn run(attachment: BrokerAttachment, size: Size) -> Result<()> {
    let BrokerAttachment {
        session,
        clipboard,
        budget,
        primary,
        x11,
    } = attachment;
    let mut terminal = AsyncAttachedTerminalPresentationGuard::new(
        io::stdin().as_raw_fd(),
        io::stdout().as_raw_fd(),
        None,
    )?;
    let result = if let Some(x11) = x11 {
        Box::pin(x11.prepared.run_broker_attachment(
            x11.opener,
            x11.limit,
            x11.budget,
            budget,
            |stop| {
                session.run_clipboard_foreground(
                    terminal.io_mut(),
                    size,
                    budget,
                    crate::cli::x11::broker_attachment_cancelled(stop),
                    clipboard,
                )
            },
            cancelled(),
        ))
        .await
    } else if primary {
        Box::pin(session.run_clipboard_foreground(
            terminal.io_mut(),
            size,
            budget,
            cancelled(),
            clipboard,
        ))
        .await
    } else {
        Box::pin(session.run_snapshot_foreground(terminal.io_mut(), size, budget, cancelled()))
            .await
    };
    let restored = tokio::time::timeout(budget, terminal.restore())
        .await
        .map_err(|_| MezError::invalid_state("broker attachment terminal restoration timed out"))
        .and_then(|result| result);
    match result {
        Ok(()) => restored,
        Err(error) => {
            let _ = restored;
            Err(error)
        }
    }
}

/// Retains foreground signal streams without spawning a cancellation worker.
/// Terminal Ctrl-C in raw mode remains ordinary forwarded input; OS signals
/// terminate only this local frontend and use the common restoration path.
async fn cancelled() {
    use tokio::signal::unix::{SignalKind, signal};
    if let (Ok(mut interrupt), Ok(mut terminate), Ok(mut hangup)) = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
    ) {
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
            _ = hangup.recv() => {},
        }
    } else {
        let _ = tokio::signal::ctrl_c().await;
    }
}
