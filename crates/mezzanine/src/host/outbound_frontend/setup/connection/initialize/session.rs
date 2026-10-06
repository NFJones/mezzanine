//! Validated session initialization with exact client/lease transport ownership.
//!
//! Reuses the single owner-authenticated exchange. No automatic retry is allowed
//! after writing, since creation may already be committed remotely. Session and
//! client evidence stays scoped to its connection; labels cannot grant authority.
//! Optional version-one events remain bound to this exact retained connection.
//! A separate version-two primary admission validates clipboard capability and
//! requires item-aware consumption; ordinary supervision remains version-one.
//! Later versions and X11 reject before sending. Raw replies never enter IPC.

use super::*;
use mez_core::ids::{ClientId, SessionId};

/// One initialized session connection and its exact validated inert identities.
/// Private ownership prevents another setup from retargeting this connection.
pub(crate) struct InitializedSessionFrontend {
    connected: ConnectedFrontend,
    bridge: IrohCompressionBridge,
    summary: serde_json::Value,
    delivered_receipts: Vec<u64>,
    /// Last successfully delivered view identity and its exact requested geometry.
    delivered_view: Option<(String, u16, u16)>,
    /// Selected-path evidence owned by this connection, never a sibling endpoint.
    health: crate::host::terminal::iroh_health::AttachIrohHealthTracker,
    /// Successful self-detach permanently retires this initialized owner.
    detached: bool,
    /// True only after explicit v2 primary capability and preface validation.
    clipboard_enabled: bool,
    /// Local transfer occurrence; never reused within this initialized owner.
    clipboard_transfer: u64,
    events: Option<
        crate::host::outbound_frontend::events::OutboundEventReader<iroh::endpoint::RecvStream>,
    >,
}

mod view;

impl ConnectedFrontend {
    /// Initializes one host-routed session using its original invocation key.
    /// Unsupported modes reject before opening a stream; failed replies retain
    /// ambiguity and never replay creation, input, or initialization.
    pub(crate) async fn initialize_session(self) -> Result<InitializedSessionFrontend> {
        self.initialize_session_mode(false).await
    }

    /// Admits only explicitly requested version-two primary clipboard sessions.
    /// Returned capability is validated before accepting the exact v2 preface.
    /// This separate transition does not activate ordinary supervisor forwarding
    /// or write a host clipboard; callers must consume typed event items.
    pub(crate) async fn initialize_clipboard_session(self) -> Result<InitializedSessionFrontend> {
        self.initialize_session_mode(true).await
    }

    /// Shares single-attempt authenticated initialization without broadening the
    /// existing redraw-only path. Capability rejection never retries creation.
    async fn initialize_session_mode(self, clipboard: bool) -> Result<InitializedSessionFrontend> {
        let params = initialize_params_from_json(&self.prepared.initialize.to_string())?;
        validate_event_mode(&params, clipboard)?;
        if self.prepared.profile.scope != RemoteClientProfileScope::Host
            || !matches!(
                params.session_intent,
                Some(
                    SessionIntent::Create
                        | SessionIntent::ResolveOrCreate
                        | SessionIntent::Attach
                        | SessionIntent::Default
                )
            )
            || params.x11_forwarding.is_some()
        {
            return Err(MezError::forbidden(
                "outbound session initialization mode unsupported",
            ));
        }
        #[cfg(test)]
        let mut diagnostics =
            crate::host::outbound_frontend::listener::setup_diagnostics::SetupDiagnostics::new();
        #[cfg(test)]
        diagnostics.advance("initialize-reply");
        let (connected, bridge, summary) = self
            .initialize_once(|body, connected| {
                validate_session_response(body, connected.prepared.profile.server_addr.id, &params)
            })
            .await?;
        #[cfg(test)]
        diagnostics.advance("event-preface");
        let events = if params.event_stream_version.is_some() {
            let endpoint = &connected.prepared.frontend._endpoint;
            endpoint.frontend_config_root()?;
            let events = if clipboard {
                crate::host::outbound_frontend::events::OutboundEventReader::accept_clipboard(
                    connected.connection.connection(),
                    connected.compression,
                    endpoint.transport_policy().setup_timeout,
                )
                .await?
            } else {
                crate::host::outbound_frontend::events::OutboundEventReader::accept(
                    connected.connection.connection(),
                    connected.compression,
                    endpoint.transport_policy().setup_timeout,
                )
                .await?
            };
            endpoint.frontend_config_root()?;
            Some(events)
        } else {
            None
        };
        #[cfg(test)]
        diagnostics.complete();
        Ok(InitializedSessionFrontend {
            connected,
            bridge,
            summary,
            delivered_receipts: Vec::new(),
            delivered_view: None,
            health: Default::default(),
            detached: false,
            clipboard_enabled: clipboard,
            clipboard_transfer: 0,
            events,
        })
    }
}

impl InitializedSessionFrontend {
    /// Reads one negotiated typed event or clipboard effect without IPC or host
    /// clipboard writes. Cancellation retains incremental state; EOF/errors
    /// require retirement of this exact owner, never reconnection or replay.
    pub(crate) async fn next_event_item(
        &mut self,
    ) -> Result<Option<crate::host::outbound_frontend::events::OutboundEventItem>> {
        self.connected
            .prepared
            .frontend
            ._endpoint
            .frontend_config_root()?;
        let reader = self
            .events
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("outbound session did not negotiate events"))?;
        let item = reader.next_item().await?;
        self.connected
            .prepared
            .frontend
            ._endpoint
            .frontend_config_root()?;
        Ok(item)
    }

    /// Reports exact self-detach settlement so supervision can dispose this
    /// pipeline after reply delivery instead of admitting another request.
    pub(crate) fn is_detached(&self) -> bool {
        self.detached
    }

    /// Samples only this retained connection when its refresh deadline is due.
    /// Disconnection returns unknown without borrowing another path or reviving
    /// ownership. Callers own reply delivery, redraw and terminal retirement.
    pub(crate) fn transport_health(
        &mut self,
    ) -> Result<(bool, crate::host::terminal::TerminalIrohStatusQuality)> {
        self.connected
            .prepared
            .frontend
            ._endpoint
            .frontend_config_root()?;
        let connection = self.connected.connection.connection();
        if connection.close_reason().is_some() {
            return Ok((
                false,
                crate::host::terminal::TerminalIrohStatusQuality::Unknown,
            ));
        }
        if self.health.deadline() <= tokio::time::Instant::now() {
            self.health.sample(connection);
        }
        Ok((true, self.health.quality()))
    }

    /// Reads only negotiated version-one events on this retained connection.
    /// Idle cancellation preserves reader state. Errors/EOF require the caller
    /// to retire this session owner; no reconnect, replay or IPC occurs here.
    pub(crate) async fn next_event(
        &mut self,
    ) -> Result<
        Option<(
            crate::host::terminal::wire_events::AttachRenderAction,
            Option<u64>,
        )>,
    > {
        self.connected
            .prepared
            .frontend
            ._endpoint
            .frontend_config_root()?;
        let reader = self
            .events
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("outbound session did not negotiate events"))?;
        let event = reader.next().await?;
        self.connected
            .prepared
            .frontend
            ._endpoint
            .frontend_config_root()?;
        Ok(event)
    }
}

/// Accepts only correlated active-lease and attached-client settlement. Stable
/// explicit ID targets must match exactly; name/default authority remains with
/// the authenticated host router, not independently inferred by this projection.
fn validate_session_response(
    body: &str,
    server: iroh::EndpointId,
    params: &crate::control::InitializeParams,
) -> Result<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("outbound session response invalid"))?;
    if value["jsonrpc"] != "2.0" || value["id"] != REQUEST_ID || value.get("error").is_some() {
        return Err(MezError::forbidden(
            "outbound session rejected or uncorrelated",
        ));
    }
    let result = value
        .get("result")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("outbound session result unavailable"))?;
    let role = match params.requested_role {
        RequestedRole::Primary => "primary",
        RequestedRole::Observer => "observer",
        _ => return Err(MezError::forbidden("outbound session role unsupported")),
    };
    let client = value
        .pointer("/result/client/id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| ClientId::parse('c', id.to_string()))
        .ok_or_else(|| MezError::invalid_state("outbound session client identity unavailable"))?;
    let session = value
        .pointer("/result/session/id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| SessionId::parse('$', id.to_string()))
        .ok_or_else(|| MezError::invalid_state("outbound session identity unavailable"))?;
    let lease = value
        .pointer("/result/lease/lease_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| {
            id.starts_with("lease-")
                && id.len() <= 128
                && id.len() > 6
                && !id.chars().any(char::is_control)
        })
        .ok_or_else(|| MezError::invalid_state("outbound session lease identity unavailable"))?;
    if result.get("selected_version") != Some(&serde_json::json!(3))
        || result.get("granted_role") != Some(&serde_json::json!(role))
        || value
            .pointer("/result/host/endpoint_id")
            .and_then(serde_json::Value::as_str)
            != Some(server.to_string().as_str())
        || value
            .pointer("/result/lease/session_id")
            .and_then(serde_json::Value::as_str)
            != Some(session.as_str())
        || value
            .pointer("/result/lease/state")
            .and_then(serde_json::Value::as_str)
            != Some("active")
        || value.pointer("/result/capabilities/features/host_only")
            == Some(&serde_json::Value::Bool(true))
        || result.contains_key("device_credential")
        || result
            .get("x11_forwarding")
            .is_some_and(|route| !route.is_null())
    {
        return Err(MezError::forbidden("outbound session settlement invalid"));
    }
    if let Some(target) = params.session_target_json.as_deref() {
        let target: serde_json::Value = serde_json::from_str(target)
            .map_err(|_| MezError::invalid_state("outbound retained target invalid"))?;
        if target
            .get("session_id")
            .filter(|id| !id.is_null())
            .is_some_and(|id| id.as_str() != Some(session.as_str()))
            || target
                .get("lease_id")
                .filter(|id| !id.is_null())
                .is_some_and(|id| id.as_str() != Some(lease))
        {
            return Err(MezError::forbidden("outbound session target mismatch"));
        }
    }
    if params.event_stream_version == Some(2)
        && (params.requested_role != RequestedRole::Primary
            || value.pointer("/result/capabilities/features/client_clipboard_write")
                != Some(&serde_json::Value::Bool(true)))
    {
        return Err(MezError::forbidden(
            "outbound clipboard capability unavailable",
        ));
    }
    Ok(serde_json::json!({"selected_version":3,"granted_role":role,
        "session_id":session.as_str(),"lease_id":lease,"client_id":client.as_str()}))
}

/// Rejects unsupported negotiation before opening an application stream. The
/// separate clipboard transition cannot be selected by a received remote frame.
fn validate_event_mode(params: &crate::control::InitializeParams, clipboard: bool) -> Result<()> {
    let valid = if clipboard {
        params.event_stream_version == Some(2) && params.requested_role == RequestedRole::Primary
    } else {
        params
            .event_stream_version
            .is_none_or(|version| version == 1)
    };
    if !valid {
        return Err(MezError::forbidden(
            "outbound event negotiation mode unsupported",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
