//! Consumed client setup and line snapshots over the retained authenticated IPC.
//!
//! The client sends no device proof or route addresses. Setup consumes readiness
//! once; the first snapshot pins validated session/client/lease identities and
//! every later snapshot must preserve them. Framed buffers remain intact when
//! widening the response codec. Errors consume the owner, never replaying setup
//! or returning a desynchronized connection. This is not a terminal renderer,
//! input/event transport, presentation acknowledgement, or ordinary CLI attach.

use super::*;
use crate::control::{RequestedRole, initialize_params_from_json};
use mez_core::ids::{ClientId, SessionId};

const SNAPSHOT_LIMIT: usize = 1024 * 1024;

/// Closed inert settlement facts, not independently acquired remote authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionSummary {
    selected_version: u32,
    granted_role: String,
    session_id: String,
    lease_id: String,
    client_id: String,
}

/// Strict snapshot envelope; arbitrary broker metadata cannot be forwarded.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    handle: FrontendHandle,
    session: SessionSummary,
    lines: Vec<String>,
    line_style_spans: serde_json::Value,
    cursor: serde_json::Value,
    output_modes: serde_json::Value,
    presentation_ids: Vec<u64>,
    render_rate_limit_fps: Option<u64>,
    view_identity: Option<String>,
    event_cutoff: Option<u64>,
    iroh_status_slot: Option<serde_json::Value>,
}

/// Client pinned to its first exact session settlement, with one retained stream.
pub(crate) struct OutboundSessionClient {
    client: OutboundFrontendClient,
    summary: SessionSummary,
    styles: Vec<Vec<mez_terminal::TerminalStyleSpan>>,
    modes: mez_mux::presentation::AttachedTerminalOutputModes,
    receipts: Vec<u64>,
    lines: Vec<String>,
    events_negotiated: bool,
    render_rate_limit_fps: Option<u64>,
    view_identity: Option<String>,
    event_cutoff: Option<u64>,
    /// Geometry belonging to the exact retained snapshot, not the current TTY.
    snapshot_size: (u16, u16),
    /// Set only by successful complete-output presentation and receipt settlement.
    committed_view: Option<(String, u16, u16)>,
    /// Optional server-owned presentation slot, not measured connection health.
    iroh_status_slot: Option<crate::host::terminal::TerminalIrohStatusSlot>,
    /// Last successfully painted health decoration; absent until settled output.
    painted_health: Option<crate::host::terminal::TerminalIrohStatusQuality>,
    /// Local presentation clock retained across snapshots, never decoded from IPC.
    cursor_blink_epoch: std::time::Instant,
    /// Effective cursor visibility from the last receipt-settled output frame.
    painted_cursor: Option<bool>,
    /// Exact-session transfer decoder present only for explicit primary v2 setup.
    clipboard_receiver: Option<crate::host::outbound_frontend::clipboard_wire::ClipboardReceiver>,
}

mod acknowledge;
mod conditional;
mod detach;
mod events;
mod foreground;
mod health;
mod items;
pub(crate) use items::FrontendItem;
mod present;
mod setup;
mod step;
mod target_detach;
#[allow(
    dead_code,
    reason = "ordinary X11 activation follows channel-opening qualification"
)]
mod x11_channel;
#[allow(unused_imports, reason = "broker X11 attachment integration is staged")]
pub(crate) use x11_channel::X11ChannelOpener;
mod x11_discovery;

impl OutboundFrontendClient {
    /// Consumes readiness once and sends credential-free setup plus initial view.
    /// Returns the exact session owner and a line snapshot, never retrying setup.
    pub(crate) async fn start_session(
        mut self,
        profile: &str,
        initialize: serde_json::Value,
        columns: u16,
        rows: u16,
        deadline: Duration,
    ) -> Result<(OutboundSessionClient, Vec<String>)> {
        let (body, params) =
            setup::encode_setup(&self.handle, profile, &initialize, columns, rows, deadline)?;
        let role = match params.requested_role {
            RequestedRole::Primary => "primary",
            RequestedRole::Observer => "observer",
            _ => return Err(MezError::forbidden("outbound session role unsupported")),
        };
        tokio::time::timeout(deadline, async move {
            self.discovery.validate()?;
            self.stream
                .send(ProtocolFrame::new(CONTENT_TYPE, body))
                .await?;
            // Preserve read/write buffers and peer ownership, not into_inner().
            *self.stream.codec_mut() = ProtocolFrameCodec::new(SNAPSHOT_LIMIT)?;
            let snapshot = self.exchange_snapshot(columns, rows).await?;
            if snapshot.session.granted_role != role {
                return Err(MezError::forbidden("outbound session role changed"));
            }
            if let Some(target) = params.session_target_json.as_deref() {
                let target: serde_json::Value = serde_json::from_str(target)
                    .map_err(|_| MezError::invalid_state("outbound retained target invalid"))?;
                if target
                    .get("session_id")
                    .filter(|id| !id.is_null())
                    .is_some_and(|id| id.as_str() != Some(snapshot.session.session_id.as_str()))
                    || target
                        .get("lease_id")
                        .filter(|id| !id.is_null())
                        .is_some_and(|id| id.as_str() != Some(snapshot.session.lease_id.as_str()))
                {
                    return Err(MezError::forbidden("outbound session target changed"));
                }
            }
            Ok((
                OutboundSessionClient {
                    clipboard_receiver: (params.event_stream_version == Some(2)
                        && params.requested_role == RequestedRole::Primary)
                        .then(|| {
                            crate::host::outbound_frontend::clipboard_wire::ClipboardReceiver::new(
                                self.handle.clone(),
                                snapshot.session.clone(),
                            )
                        }),
                    client: self,
                    modes: snapshot_modes(&snapshot, columns, rows)?,
                    iroh_status_slot: snapshot_status_slot(&snapshot, columns, rows)?,
                    snapshot_size: (columns, rows),
                    committed_view: None,
                    painted_health: None,
                    cursor_blink_epoch: std::time::Instant::now(),
                    painted_cursor: None,
                    events_negotiated: params.event_stream_version == Some(1),
                    render_rate_limit_fps: snapshot.render_rate_limit_fps,
                    view_identity: snapshot.view_identity,
                    event_cutoff: snapshot.event_cutoff,
                    lines: snapshot.lines.clone(),
                    receipts: snapshot.presentation_ids,
                    summary: snapshot.session,
                    styles: crate::host::terminal::wire_styles::bounded_style_rows(
                        &snapshot.line_style_spans,
                        snapshot.lines.len(),
                        columns,
                    )?,
                },
                snapshot.lines,
            ))
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound setup timed out; outcome unknown"))?
    }

    /// Exchanges one fixed view request, without changing identity or method.
    async fn exchange_snapshot(&mut self, columns: u16, rows: u16) -> Result<Snapshot> {
        self.discovery.validate()?;
        self.stream
            .send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({"handle":self.handle,"columns":columns,"rows":rows}).to_string(),
            ))
            .await?;
        let frame = self
            .stream
            .next()
            .await
            .transpose()?
            .ok_or_else(|| MezError::invalid_state("outbound snapshot unavailable"))?;
        if frame.content_type != CONTENT_TYPE {
            return Err(MezError::invalid_state(
                "outbound snapshot content type unsupported",
            ));
        }
        let snapshot: Snapshot = serde_json::from_str(&frame.body)
            .map_err(|_| MezError::invalid_state("outbound snapshot invalid"))?;
        validate_snapshot(&snapshot, &self.handle, rows)?;
        snapshot_modes(&snapshot, columns, rows)?;
        snapshot_status_slot(&snapshot, columns, rows)?;
        crate::host::terminal::wire_styles::bounded_style_rows(
            &snapshot.line_style_spans,
            snapshot.lines.len(),
            columns,
        )?;
        self.discovery.validate()?;
        Ok(snapshot)
    }
}

impl OutboundSessionClient {
    /// Consumes one subsequent snapshot request and returns the unchanged owner
    /// only on a correlated success. Failures cannot retarget or replay setup.
    pub(crate) async fn snapshot(
        mut self,
        columns: u16,
        rows: u16,
        deadline: Duration,
    ) -> Result<(Self, Vec<String>)> {
        validate_budget(columns, rows, deadline)?;
        tokio::time::timeout(deadline, async move {
            let snapshot = self.client.exchange_snapshot(columns, rows).await?;
            if snapshot.session != self.summary {
                return Err(MezError::conflict("outbound session settlement changed"));
            }
            self.styles = crate::host::terminal::wire_styles::bounded_style_rows(
                &snapshot.line_style_spans,
                snapshot.lines.len(),
                columns,
            )?;
            self.modes = snapshot_modes(&snapshot, columns, rows)?;
            self.iroh_status_slot = snapshot_status_slot(&snapshot, columns, rows)?;
            self.receipts = snapshot.presentation_ids;
            self.lines = snapshot.lines.clone();
            self.render_rate_limit_fps = snapshot.render_rate_limit_fps;
            self.view_identity = snapshot.view_identity;
            self.event_cutoff = snapshot.event_cutoff;
            self.snapshot_size = (columns, rows);
            self.committed_view = None;
            self.painted_health = None;
            self.painted_cursor = None;
            Ok((self, snapshot.lines))
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound snapshot timed out"))?
    }

    /// Returns inert exact identities, never credentials or execution authority.
    pub(crate) fn summary(&self) -> &SessionSummary {
        &self.summary
    }

    /// Borrows decoded cell styles aligned with the last returned snapshot.
    /// This grants no renderer or presentation-acknowledgement authority.
    pub(crate) fn line_style_spans(&self) -> &[Vec<mez_terminal::TerminalStyleSpan>] {
        &self.styles
    }

    /// Returns validated presentation modes for the last snapshot without
    /// applying them or granting terminal-input/presentation receipt authority.
    pub(crate) fn output_modes(&self) -> mez_mux::presentation::AttachedTerminalOutputModes {
        self.modes
    }

    /// Returns receipt IDs for the last snapshot, without acknowledging delivery.
    /// Only a renderer's completed terminal write may justify the explicit ACK.
    pub(crate) fn presentation_ids(&self) -> &[u64] {
        &self.receipts
    }

    /// Borrows server revision evidence for the last snapshot, without claiming
    /// that output committed or authorizing conditional reuse of a baseline.
    pub(crate) fn revision_evidence(&self) -> (Option<&str>, Option<u64>) {
        (self.view_identity.as_deref(), self.event_cutoff)
    }

    /// Returns the validated slot belonging to the last snapshot. Decoding it
    /// neither measures connection health nor composes local output.
    pub(crate) fn iroh_status_slot(&self) -> Option<crate::host::terminal::TerminalIrohStatusSlot> {
        self.iroh_status_slot
    }
}

/// Uses the same optional slot and cell bounds as the broker projection.
fn snapshot_status_slot(
    snapshot: &Snapshot,
    columns: u16,
    rows: u16,
) -> Result<Option<crate::host::terminal::TerminalIrohStatusSlot>> {
    crate::host::terminal::wire_status::bounded_status_slot(
        snapshot.iroh_status_slot.as_ref(),
        snapshot.lines.len(),
        columns,
        rows,
    )
}

/// Uses the same viewport and mode interpretation as the broker projection.
fn snapshot_modes(
    snapshot: &Snapshot,
    columns: u16,
    rows: u16,
) -> Result<mez_mux::presentation::AttachedTerminalOutputModes> {
    crate::host::terminal::wire_modes::bounded_view_output_modes(
        &serde_json::json!({"cursor":snapshot.cursor,"output_modes":snapshot.output_modes}),
        columns,
        rows,
    )
}

/// Keeps local request dimensions and deadlines within the server contract.
fn validate_budget(columns: u16, rows: u16, deadline: Duration) -> Result<()> {
    if !(1..=4096).contains(&columns)
        || !(1..=4096).contains(&rows)
        || !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&deadline)
    {
        return Err(MezError::invalid_args(
            "outbound snapshot budget unavailable",
        ));
    }
    Ok(())
}

/// Checks closed snapshot identities and row count before exposing any lines.
fn validate_snapshot(snapshot: &Snapshot, handle: &FrontendHandle, rows: u16) -> Result<()> {
    crate::host::terminal::wire_receipts::validate_receipts(&snapshot.presentation_ids)?;
    if snapshot
        .view_identity
        .as_deref()
        .is_some_and(|value| !crate::host::terminal::wire_identity::valid_view_identity(value))
    {
        return Err(MezError::invalid_state(
            "outbound snapshot identity invalid",
        ));
    }
    let summary = &snapshot.session;
    if snapshot.handle != *handle
        || summary.selected_version != 3
        || !matches!(summary.granted_role.as_str(), "primary" | "observer")
        || SessionId::parse('$', summary.session_id.clone()).is_none()
        || ClientId::parse('c', summary.client_id.clone()).is_none()
        || !summary.lease_id.starts_with("lease-")
        || summary.lease_id.len() <= 6
        || summary.lease_id.len() > 128
        || summary.lease_id.chars().any(char::is_control)
        || snapshot.lines.len() > usize::from(rows)
    {
        return Err(MezError::forbidden("outbound snapshot ownership invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
