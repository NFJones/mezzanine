//! Explicit host-session administrative detach through the retained broker.
//!
//! This operation resolves only an existing session and requires the protected
//! profile's primary ceiling. It never creates a runtime or acquires another
//! endpoint identity. One caller key reaches the fixed target mutation; uncertain
//! settlement cannot trigger reconnect, fallback or replay. Disconnect cleanup
//! retires the temporary administrative primary, not unrelated frontends.

use super::*;

/// Returns host-profile settlement, or None for the unchanged legacy transport.
/// Host administration requires both stable IDs and an already running broker;
/// missing broker discovery is a diagnostic, not permission for v2 host routing.
pub(in crate::cli) async fn try_detach(
    target: &crate::cli::ControlTargetSelection,
    env: &crate::cli::CliEnv,
    session_id: Option<&str>,
    client_id: Option<&str>,
    key: &str,
) -> Result<Option<String>> {
    let crate::cli::ControlTargetSelection::IrohProfile(alias) = target else {
        if session_id.is_some() {
            return Err(MezError::invalid_args(
                "remote session detach requires a paired host profile",
            ));
        }
        return Ok(None);
    };
    let paths = env.config_paths()?;
    let layers = crate::cli::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    if !policy.outbound_enabled {
        return Err(MezError::config(
            "outbound Iroh connections are disabled by transport.iroh.outbound_enabled",
        ));
    }
    let profile = RemoteClientProfileStore::under_config_root(paths.root())
        .load(alias)?
        .ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "Iroh client profile not found",
            )
        })?;
    if profile.scope != RemoteClientProfileScope::Host {
        if session_id.is_some() {
            return Err(MezError::invalid_args(
                "--session-id requires a host-scoped Iroh profile",
            ));
        }
        return Ok(None);
    }
    let session_id = session_id.ok_or_else(|| {
        MezError::invalid_args("host-profile detach requires --session-id and --client-id")
    })?;
    let client_id = client_id.ok_or_else(|| {
        MezError::invalid_args("host-profile detach requires --session-id and --client-id")
    })?;
    mez_core::ids::SessionId::parse('$', session_id.to_string())
        .ok_or_else(|| MezError::invalid_args("remote detach session identity invalid"))?;
    mez_core::ids::ClientId::parse('c', client_id.to_string())
        .ok_or_else(|| MezError::invalid_args("remote detach client identity invalid"))?;
    if session_id.len() > 128
        || client_id.len() > 128
        || key.is_empty()
        || key.len() > 128
        || key.chars().any(char::is_control)
    {
        return Err(MezError::invalid_args("remote detach identity invalid"));
    }
    ensure_iroh_attach_role_allowed(profile.role, "primary")?;
    let client = crate::host::outbound_frontend::client::OutboundFrontendClient::connect(
        paths.root(), policy.setup_timeout,
    ).await.map_err(|error| {
        if matches!(error.io_kind(), Some(std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)) {
            MezError::invalid_state("host-profile detach requires an active outbound broker; attach to the existing session first")
        } else { error }
    })?;
    // Existing remote-primary admission independently verifies durable trust.
    // Administrative control follows the established primary terminal descriptor
    // contract without forwarding terminal input, events or clipboard effects.
    let initialize = serde_json::json!({"client_name":"remote-detach-admin",
        "requested_version":3,"requested_role":"primary","session_intent":"attach",
        "session_target":{"session_id":session_id},"detach_primary_on_disconnect":true,
        "client":{"name":"remote-detach-admin","interactive":true,
            "terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}});
    let (session, _) =
        Box::pin(client.start_session(alias, initialize, 80, 24, policy.setup_timeout)).await?;
    session
        .detach_target(client_id, key, policy.setup_timeout)
        .await?;
    Ok(Some(
        serde_json::json!({"jsonrpc":"2.0","id":"cli",
        "result":{"detached":true,"client_id":client_id}})
        .to_string(),
    ))
}

#[cfg(test)]
mod tests;
