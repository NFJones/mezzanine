//! Explicit acknowledgement of receipts from the last delivered snapshot.
//!
//! Delivery itself never arms presentation. The authenticated frontend must
//! report its completed output write with the exact retained IDs and mutation
//! key. This owner forwards one fixed acknowledgement on the initialized
//! connection, not arbitrary control or input; uncertain failures consume it.

use super::*;

const ACK_ID: &str = "outbound-presentation-ack";

/// Closed post-commit request, scoped to the retained local stream.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgementRequest {
    operation: String,
    handle: FrontendHandle,
    idempotency_key: String,
    presentation_ids: Vec<u64>,
}

/// Forwards exactly one explicitly requested acknowledgement after validating
/// delivered receipt ownership. The enclosing consumed exchange owns timeout.
pub(super) async fn deliver_acknowledgement(
    mut session: InitializedSessionFrontend,
    body: &str,
) -> Result<InitializedSessionFrontend> {
    let request: AcknowledgementRequest = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("outbound presentation request invalid"))?;
    validate_request(
        &request,
        &session.connected.prepared.frontend.handle,
        &session.delivered_receipts,
    )?;
    session
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    let remote = serde_json::json!({"jsonrpc":"2.0","id":ACK_ID,
    "method":"terminal/presentation/acknowledge","params":{
        "idempotency_key":request.idempotency_key,"presentation_ids":request.presentation_ids
    }})
    .to_string();
    session
        .bridge
        .stream_mut()
        .write_all(&crate::control::encode_control_body(&remote))
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound presentation write unavailable; outcome unknown")
        })?;
    let response = read_exact_frame(session.bridge.stream_mut()).await?;
    let acknowledged = project_acknowledgement(&response)?;
    session
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    let reply = serde_json::json!({"handle":request.handle,"session":session.summary,
        "idempotency_key":request.idempotency_key,"presentation_ids":request.presentation_ids,
        "acknowledged":acknowledged})
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
            MezError::invalid_state("outbound presentation reply unavailable; outcome unknown")
        })?;
    if acknowledged {
        session.delivered_receipts.clear();
    }
    Ok(session)
}

/// Rejects foreign, empty or stale receipt lists and malformed mutation keys
/// before remote I/O. Receipt order remains the delivered producer order.
fn validate_request(
    request: &AcknowledgementRequest,
    handle: &FrontendHandle,
    delivered: &[u64],
) -> Result<()> {
    crate::host::terminal::wire_receipts::validate_receipts(&request.presentation_ids)?;
    if request.operation != "acknowledge"
        || request.handle != *handle
        || request.presentation_ids.is_empty()
        || request.presentation_ids != delivered
    {
        return Err(MezError::conflict(
            "outbound presentation receipt owner changed",
        ));
    }
    if request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 128
        || request.idempotency_key.chars().any(char::is_control)
    {
        return Err(MezError::invalid_args("outbound presentation key invalid"));
    }
    Ok(())
}

/// Preserves a correlated boolean result, including truthful stale-receipt false.
fn project_acknowledgement(body: &str) -> Result<bool> {
    let response: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        MezError::invalid_state("outbound presentation response invalid; outcome unknown")
    })?;
    if response["jsonrpc"] != "2.0" || response["id"] != ACK_ID || response.get("error").is_some() {
        return Err(MezError::invalid_state(
            "outbound presentation response uncorrelated; outcome unknown",
        ));
    }
    response
        .pointer("/result/acknowledged")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            MezError::invalid_state("outbound presentation settlement invalid; outcome unknown")
        })
}

#[cfg(test)]
mod tests;
