//! Exact-session publication discovery without remote requests or channel allocation.
//!
//! A closed request contains only the retained handle. Supervision alone installs
//! a local basename while owning its listener; unnegotiated sessions report None.
//! The enclosing control exchange bounds delivery and consumes uncertain owners.
//! No route proof, cookie, absolute path or remote authority crosses this reply.

use super::*;

/// Exact local ownership is the sole discovery request parameter.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    handle: FrontendHandle,
}

/// Returns only validated supervised publication spelling with pinned ownership.
/// Discovery does not reserve capacity or advance channel occurrences.
pub(super) async fn deliver(
    mut session: InitializedSessionFrontend,
    body: &str,
) -> Result<InitializedSessionFrontend> {
    let request: Request = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("outbound X11 discovery request invalid"))?;
    if request.operation != "x11-discovery"
        || request.handle != session.connected.prepared.frontend.handle
    {
        return Err(MezError::conflict("outbound X11 discovery owner changed"));
    }
    if let Some(name) = &session.x11_socket_name {
        if session.x11_route.is_none() || session.summary["granted_role"] != "primary" {
            return Err(MezError::forbidden("outbound X11 discovery unavailable"));
        }
        crate::host::outbound_frontend::x11_discovery::validate_socket_name(name)?;
    }
    session
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    let reply = serde_json::json!({"handle":request.handle,"session":session.summary,
        "version":1,"socket_name":session.x11_socket_name})
    .to_string();
    session
        .connected
        .prepared
        .frontend
        .stream
        .get_mut()
        .write_all(&encode_frame(&ProtocolFrame::new(CONTENT_TYPE, reply)))
        .await
        .map_err(|_| MezError::invalid_state("outbound X11 discovery delivery unavailable"))?;
    Ok(session)
}
