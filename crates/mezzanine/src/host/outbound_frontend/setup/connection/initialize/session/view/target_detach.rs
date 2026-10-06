//! Fixed primary-authorized client detach on an exact initialized session.
//!
//! This closed administrative operation allows an explicit client ID but no
//! session/route/method replacement. The authenticated runtime remains responsible
//! for administrative authority and target existence. One original-key request
//! is sent; correlated success retires this pipeline, and uncertain delivery
//! never replays the mutation. Other frontends retain independent connections.

use super::*;
use mez_core::ids::ClientId;

const REQUEST_ID: &str = "outbound-target-detach";

/// Only the explicit client target and original key supplement local ownership.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    handle: FrontendHandle,
    client_id: String,
    idempotency_key: String,
}

/// Forwards one fixed mutation and retires the administrative pipeline only
/// after its exact target settlement has been delivered locally. Errors consume
/// this owner; existing disconnect cleanup retires its temporary primary.
pub(super) async fn deliver(
    mut session: InitializedSessionFrontend,
    body: &str,
) -> Result<InitializedSessionFrontend> {
    let request: Request = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("outbound target detach request invalid"))?;
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
    let body = serde_json::json!({"jsonrpc":"2.0","id":REQUEST_ID,"method":"client/detach",
        "params":{"client_id":request.client_id,"idempotency_key":request.idempotency_key}})
    .to_string();
    session
        .bridge
        .stream_mut()
        .write_all(&crate::control::encode_control_body(&body))
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound target detach write unavailable; outcome unknown")
        })?;
    let body = read_exact_frame(session.bridge.stream_mut()).await?;
    project_response(&body, &request.client_id)?;
    session
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    let reply = serde_json::json!({"handle":request.handle,"session":session.summary,
        "idempotency_key":request.idempotency_key,"detached":true,"client_id":request.client_id})
    .to_string();
    session
        .connected
        .prepared
        .frontend
        .stream
        .get_mut()
        .write_all(&encode_frame(&ProtocolFrame::new(CONTENT_TYPE, reply)))
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound target detach delivery unavailable; outcome unknown")
        })?;
    session.detached = true;
    Ok(session)
}

/// Rejects foreign owners and non-primary callers before any remote mutation.
fn validate_request(
    request: &Request,
    handle: &FrontendHandle,
    summary: &serde_json::Value,
) -> Result<()> {
    if request.operation != "detach-target" || request.handle != *handle {
        return Err(MezError::conflict("outbound target detach owner changed"));
    }
    if summary["granted_role"] != "primary" {
        return Err(MezError::forbidden(
            "outbound target detach requires primary",
        ));
    }
    ClientId::parse('c', request.client_id.clone())
        .ok_or_else(|| MezError::invalid_args("outbound target detach client invalid"))?;
    if request.client_id.len() > 128
        || request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 128
        || request.idempotency_key.chars().any(char::is_control)
    {
        return Err(MezError::invalid_args(
            "outbound target detach identity invalid",
        ));
    }
    Ok(())
}

/// Requires truthful success for the exact explicit target, not the caller ID.
fn project_response(body: &str, target: &str) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        MezError::invalid_state("outbound target detach response invalid; outcome unknown")
    })?;
    if value["jsonrpc"] != "2.0"
        || value["id"] != REQUEST_ID
        || value.get("error").is_some()
        || value.pointer("/result/detached") != Some(&serde_json::Value::Bool(true))
        || value
            .pointer("/result/client_id")
            .and_then(serde_json::Value::as_str)
            != Some(target)
    {
        return Err(MezError::invalid_state(
            "outbound target detach rejected or settlement changed; outcome unknown",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
