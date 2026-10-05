//! Fixed destructive host operation on an authenticated management connection.
//!
//! Validates exact local handle, target and original mutation key before remote
//! I/O. The host independently enforces destructive routing authority. One
//! correlated reply projects only revoked-lease facts; uncertain failures consume
//! the connection without reconnect, fallback or replay. The enclosing consumed
//! management exchange supplies the total timeout and sibling isolation.

use super::*;
use crate::host::outbound_frontend::killing::{
    KillSettlement, project_settlement, validate_request,
};

const KILL_ID: &str = "outbound-host-kill";

/// Closed request for one explicit force-kill, never arbitrary remote control.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KillRequest {
    operation: String,
    handle: FrontendHandle,
    target: String,
    idempotency_key: String,
}

/// Issues exactly one host/session/kill with force=true and the original key.
pub(super) async fn deliver_kill(mut owner: InitializedHostFrontend, body: &str) -> Result<()> {
    let request: KillRequest = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("outbound kill request invalid"))?;
    validate_local_request(&request, &owner.connected.prepared.frontend.handle)?;
    owner
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    let body = serde_json::json!({"jsonrpc":"2.0","id":KILL_ID,"method":"host/session/kill",
        "params":{"target":request.target,"force":true,"idempotency_key":request.idempotency_key}})
    .to_string();
    owner
        .bridge
        .stream_mut()
        .write_all(&crate::control::encode_control_body(&body))
        .await
        .map_err(|_| MezError::invalid_state("outbound kill write unavailable; outcome unknown"))?;
    let body = read_exact_frame(owner.bridge.stream_mut()).await?;
    let settlement = project_response(&body, &request.target)?;
    owner
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    let reply = serde_json::json!({"handle":request.handle,"host":owner.summary,
        "target":request.target,"idempotency_key":request.idempotency_key,"settlement":settlement})
    .to_string();
    owner
        .connected
        .prepared
        .frontend
        .stream
        .get_mut()
        .write_all(&encode_frame(&ProtocolFrame::new(CONTENT_TYPE, reply)))
        .await
        .map_err(|_| MezError::invalid_state("outbound kill reply unavailable; outcome unknown"))?;
    Ok(())
}

/// Rejects foreign stream ownership before remote mutation.
fn validate_local_request(request: &KillRequest, handle: &FrontendHandle) -> Result<()> {
    if request.operation != "kill" || request.handle != *handle {
        return Err(MezError::conflict("outbound kill owner changed"));
    }
    validate_request(&request.target, &request.idempotency_key)
}

/// Correlates remote revocation evidence without exporting peer diagnostics.
fn project_response(body: &str, target: &str) -> Result<KillSettlement> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("outbound kill response invalid; outcome unknown"))?;
    if value["jsonrpc"] != "2.0" || value["id"] != KILL_ID || value.get("error").is_some() {
        return Err(MezError::invalid_state(
            "outbound kill rejected or uncorrelated; outcome unknown",
        ));
    }
    project_settlement(&value["result"], target)
}

#[cfg(test)]
mod tests;
