//! Closed primary input exchange on the retained initialized session connection.
//!
//! Exactly one terminal/step is sent with the caller's original mutation key.
//! No reconnect, retry, render, receipt acknowledgement or target selector exists.
//! A valid acknowledgement means runtime acceptance, not proof every byte reached
//! a process or model. Errors consume the owner under the enclosing view deadline.

use super::*;

const STEP_ID: &str = "outbound-session-step";

/// Closed input envelope; no frontend-controlled method or session target.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepRequest {
    operation: String,
    handle: FrontendHandle,
    columns: u16,
    rows: u16,
    idempotency_key: String,
    input_bytes: Vec<u8>,
}

/// Issues one mutation after validating exact handle, primary role and budgets.
/// The enclosing consumed request owns the total deadline and failure cleanup.
pub(super) async fn deliver_step(
    mut session: InitializedSessionFrontend,
    body: &str,
) -> Result<InitializedSessionFrontend> {
    let request: StepRequest = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("outbound input request invalid"))?;
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
    let remote = serde_json::json!({"jsonrpc":"2.0","id":STEP_ID,"method":"terminal/step",
        "params":{"idempotency_key":request.idempotency_key,
        "client_size":{"columns":request.columns,"rows":request.rows},
        "render":false,"input_bytes":request.input_bytes}})
    .to_string();
    session
        .bridge
        .stream_mut()
        .write_all(&crate::control::encode_control_body(&remote))
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound input write unavailable; outcome unknown")
        })?;
    let response = read_exact_frame(session.bridge.stream_mut()).await?;
    let acknowledgement = project_acknowledgement(&response, request.input_bytes.len())?;
    session
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    let body = serde_json::json!({"handle":request.handle,"session":session.summary,
        "idempotency_key":request.idempotency_key,"acknowledgement":acknowledgement})
    .to_string();
    session
        .connected
        .prepared
        .frontend
        .stream
        .get_mut()
        .write_all(&encode_frame(&ProtocolFrame::new(CONTENT_TYPE, body)))
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound input reply unavailable; outcome unknown")
        })?;
    Ok(session)
}

/// Rejects unsupported role, foreign handles and oversized requests before I/O.
fn validate_request(
    request: &StepRequest,
    handle: &FrontendHandle,
    summary: &serde_json::Value,
) -> Result<()> {
    if request.operation != "step" || request.handle != *handle {
        return Err(MezError::conflict(
            "outbound input operation or handle changed",
        ));
    }
    if summary["granted_role"] != "primary" {
        return Err(MezError::forbidden(
            "outbound input requires initialized primary",
        ));
    }
    if !(1..=4096).contains(&request.columns)
        || !(1..=4096).contains(&request.rows)
        || request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 128
        || request.idempotency_key.chars().any(char::is_control)
        || request.input_bytes.len() > 512
    {
        return Err(MezError::invalid_args("outbound input budget unavailable"));
    }
    Ok(())
}

/// Projects only correlated runtime acceptance and truthful lifecycle flags.
/// Forwarded-byte counts are not inferred from accepted input or returned here.
fn project_acknowledgement(body: &str, input_len: usize) -> Result<serde_json::Value> {
    let response: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("outbound input response invalid; outcome unknown"))?;
    if response["jsonrpc"] != "2.0"
        || response["id"] != STEP_ID
        || response.get("error").is_some()
        || response
            .pointer("/result/input_bytes")
            .and_then(serde_json::Value::as_u64)
            != Some(input_len as u64)
    {
        return Err(MezError::invalid_state(
            "outbound input acknowledgement invalid; outcome unknown",
        ));
    }
    let detached = response
        .pointer("/result/client_detached")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            MezError::invalid_state("outbound input lifecycle unavailable; outcome unknown")
        })?;
    let terminated = response
        .pointer("/result/session_terminated")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            MezError::invalid_state("outbound input lifecycle unavailable; outcome unknown")
        })?;
    Ok(
        serde_json::json!({"input_bytes":input_len,"client_detached":detached,"session_terminated":terminated}),
    )
}

#[cfg(test)]
mod tests;
