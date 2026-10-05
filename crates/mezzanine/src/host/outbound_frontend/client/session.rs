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
}

mod acknowledge;
mod events;
mod foreground;
mod present;
mod step;

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
        validate_budget(columns, rows, deadline)?;
        if profile.is_empty() || profile.len() > 128 || profile.chars().any(char::is_control) {
            return Err(MezError::invalid_args("outbound profile alias invalid"));
        }
        if initialize.get("authentication").is_some() {
            return Err(MezError::forbidden(
                "outbound client must not supply credentials",
            ));
        }
        let params = initialize_params_from_json(&initialize.to_string())?;
        let role = match params.requested_role {
            RequestedRole::Primary => "primary",
            RequestedRole::Observer => "observer",
            _ => return Err(MezError::forbidden("outbound session role unsupported")),
        };
        let body =
            serde_json::json!({"handle":self.handle,"profile":profile,"initialize":initialize})
                .to_string();
        if body.len() > HELLO_LIMIT {
            return Err(MezError::invalid_args("outbound setup exceeds limit"));
        }
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
                    client: self,
                    modes: snapshot_modes(&snapshot, columns, rows)?,
                    events_negotiated: params.event_stream_version == Some(1),
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
            self.receipts = snapshot.presentation_ids;
            self.lines = snapshot.lines.clone();
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
