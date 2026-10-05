//! Fixed self-detach on one initialized primary connection.
//!
//! Closed local requests cannot supply a client or session target. The broker
//! uses its retained client identity, and the runtime remains the authority for
//! detach. Exactly one original-key mutation is issued; uncertain outcomes are
//! not replayed. The enclosing consumed exchange supplies its total deadline.

use super::*;

const DETACH_ID: &str = "outbound-session-detach";

/// Exact local owner and mutation identity; no target selector is exposed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DetachRequest {
    operation: String,
    handle: FrontendHandle,
    idempotency_key: String,
}

/// Issues one client/detach for the retained primary and validates exact client
/// settlement before local delivery. The frontend consumes its reply and closes;
/// the supervisor then disposes this connection without affecting siblings.
pub(super) async fn deliver_detach(
    mut session: InitializedSessionFrontend,
    body: &str,
) -> Result<InitializedSessionFrontend> {
    let request: DetachRequest = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("outbound detach request invalid"))?;
    validate_request(
        &request,
        &session.connected.prepared.frontend.handle,
        &session.summary,
    )?;
    session
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    let remote = serde_json::json!({"jsonrpc":"2.0","id":DETACH_ID,"method":"client/detach",
        "params":{"idempotency_key":request.idempotency_key,"client_id":session.summary["client_id"]}}).to_string();
    session
        .bridge
        .stream_mut()
        .write_all(&crate::control::encode_control_body(&remote))
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound detach write unavailable; outcome unknown")
        })?;
    let body = read_exact_frame(session.bridge.stream_mut()).await?;
    project_response(&body, &session.summary)?;
    session
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    let reply = serde_json::json!({"handle":request.handle,"session":session.summary,
        "idempotency_key":request.idempotency_key,"detached":true,"client_id":session.summary["client_id"]}).to_string();
    session
        .connected
        .prepared
        .frontend
        .stream
        .get_mut()
        .write_all(&encode_frame(&ProtocolFrame::new(CONTENT_TYPE, reply)))
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound detach delivery unavailable; outcome unknown")
        })?;
    session.detached = true;
    Ok(session)
}

/// Rejects foreign handles, unsupported roles and invalid keys before mutation.
fn validate_request(
    request: &DetachRequest,
    handle: &FrontendHandle,
    summary: &serde_json::Value,
) -> Result<()> {
    if request.operation != "detach" || request.handle != *handle {
        return Err(MezError::conflict("outbound detach owner changed"));
    }
    if summary["granted_role"] != "primary" {
        return Err(MezError::forbidden("outbound self-detach requires primary"));
    }
    if request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 128
        || request.idempotency_key.chars().any(char::is_control)
    {
        return Err(MezError::invalid_args("outbound detach key invalid"));
    }
    Ok(())
}

/// Correlates the runtime reply and requires truthful exact-client settlement.
fn project_response(body: &str, summary: &serde_json::Value) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        MezError::invalid_state("outbound detach response invalid; outcome unknown")
    })?;
    if value["jsonrpc"] != "2.0"
        || value["id"] != DETACH_ID
        || value.get("error").is_some()
        || value.pointer("/result/detached") != Some(&serde_json::Value::Bool(true))
        || value.pointer("/result/client_id") != summary.get("client_id")
    {
        return Err(MezError::invalid_state(
            "outbound detach settlement invalid; outcome unknown",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
