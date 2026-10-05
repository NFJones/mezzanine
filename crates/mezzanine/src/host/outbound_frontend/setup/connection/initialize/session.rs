//! Validated session initialization with exact client/lease transport ownership.
//!
//! Reuses the single owner-authenticated exchange. No automatic retry is allowed
//! after writing, since creation may already be committed remotely. Session and
//! client evidence stays scoped to its connection; labels cannot grant authority.
//! Optional version-one events remain bound to this exact retained connection;
//! later event versions and X11 are rejected before sending. Frontend event
//! forwarding is separate. Raw remote responses never enter local IPC.

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
        let params = initialize_params_from_json(&self.prepared.initialize.to_string())?;
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
            || params
                .event_stream_version
                .is_some_and(|version| version != 1)
            || params.x11_forwarding.is_some()
        {
            return Err(MezError::forbidden(
                "outbound session initialization mode unsupported",
            ));
        }
        let (connected, bridge, summary) = self
            .initialize_once(|body, connected| {
                validate_session_response(body, connected.prepared.profile.server_addr.id, &params)
            })
            .await?;
        let events = if params.event_stream_version == Some(1) {
            let endpoint = &connected.prepared.frontend._endpoint;
            endpoint.frontend_config_root()?;
            let events = crate::host::outbound_frontend::events::OutboundEventReader::accept(
                connected.connection.connection(),
                connected.compression,
                endpoint.transport_policy().setup_timeout,
            )
            .await?;
            endpoint.frontend_config_root()?;
            Some(events)
        } else {
            None
        };
        Ok(InitializedSessionFrontend {
            connected,
            bridge,
            summary,
            delivered_receipts: Vec::new(),
            delivered_view: None,
            events,
        })
    }
}

impl InitializedSessionFrontend {
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
    Ok(serde_json::json!({"selected_version":3,"granted_role":role,
        "session_id":session.as_str(),"lease_id":lease,"client_id":client.as_str()}))
}

#[cfg(test)]
mod tests;
