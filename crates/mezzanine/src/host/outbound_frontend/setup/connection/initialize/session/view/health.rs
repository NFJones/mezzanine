//! Exact-session local health delivery without remote application work.
//!
//! Accepts a closed handle-bound request, samples only the retained connection
//! through the shared tracker, and returns coarse connected/quality facts. Paths,
//! counters and credentials never enter IPC. The enclosing consumed exchange
//! bounds delivery; errors retire this session without reconnect or replay.

use super::*;

/// Exact local owner is the only caller-controlled sampling parameter.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthRequest {
    operation: String,
    handle: FrontendHandle,
}

/// Delivers only connection-local observations under the outer exchange deadline.
pub(super) async fn deliver_health(
    mut session: InitializedSessionFrontend,
    body: &str,
) -> Result<InitializedSessionFrontend> {
    let request: HealthRequest = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("outbound health request invalid"))?;
    validate_request(&request, &session.connected.prepared.frontend.handle)?;
    let (connected, quality) = session.transport_health()?;
    let quality = match quality {
        crate::host::terminal::TerminalIrohStatusQuality::Good => "good",
        crate::host::terminal::TerminalIrohStatusQuality::Degraded => "degraded",
        crate::host::terminal::TerminalIrohStatusQuality::Poor => "poor",
        crate::host::terminal::TerminalIrohStatusQuality::Unknown => "unknown",
    };
    let reply = serde_json::json!({"handle":request.handle,"session":session.summary,
        "connected":connected,"quality":quality})
    .to_string();
    session
        .connected
        .prepared
        .frontend
        ._endpoint
        .frontend_config_root()?;
    session
        .connected
        .prepared
        .frontend
        .stream
        .get_mut()
        .write_all(&encode_frame(&ProtocolFrame::new(CONTENT_TYPE, reply)))
        .await
        .map_err(|_| MezError::invalid_state("outbound health delivery unavailable"))?;
    Ok(session)
}

/// Rejects retargeting or unsupported operations before any sampling.
fn validate_request(request: &HealthRequest, handle: &FrontendHandle) -> Result<()> {
    if request.operation != "health" || request.handle != *handle {
        return Err(MezError::conflict("outbound health request owner changed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
