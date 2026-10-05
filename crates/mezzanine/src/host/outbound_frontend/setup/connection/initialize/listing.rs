//! Fixed read-only host listing on one owner-authenticated host-only connection.
//!
//! No caller method, target or credential is forwarded. A closed local handle
//! request produces only validated lease summaries. All waits share a deadline;
//! failure consumes this frontend/connection without reconnect or replay.

use super::*;

const LIST_ID: &str = "outbound-host-list";

/// Exact local handle is the only frontend-controlled listing parameter.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRequest {
    handle: FrontendHandle,
}

impl InitializedHostFrontend {
    /// Consumes one host-list request, delivers a bounded closed reply, then
    /// retires this management connection independently of sibling attachments.
    pub(crate) async fn deliver_list(mut self) -> Result<()> {
        let budget = self
            .connected
            .prepared
            .frontend
            ._endpoint
            .transport_policy()
            .setup_timeout;
        tokio::time::timeout(budget, async move {
            let frame = self.connected.prepared.frontend.stream.next().await.transpose()?
                .ok_or_else(|| MezError::invalid_state("outbound host-list request unavailable"))?;
            if frame.content_type != CONTENT_TYPE { return Err(MezError::invalid_args("outbound host-list type unsupported")); }
            let request: ListRequest = serde_json::from_str(&frame.body)
                .map_err(|_| MezError::invalid_args("outbound host-list request invalid"))?;
            if request.handle != self.connected.prepared.frontend.handle { return Err(MezError::conflict("outbound host-list handle changed")); }
            self.connected.prepared.frontend._endpoint.frontend_config_root()?;
            let body = serde_json::json!({"jsonrpc":"2.0","id":LIST_ID,"method":"host/session/list","params":{}}).to_string();
            self.bridge.stream_mut().write_all(&crate::control::encode_control_body(&body)).await
                .map_err(|_| MezError::invalid_state("outbound host-list write unavailable"))?;
            let body = read_exact_frame(self.bridge.stream_mut()).await?;
            let sessions = project_response(&body)?;
            self.connected.prepared.frontend._endpoint.frontend_config_root()?;
            let reply = serde_json::json!({"handle":request.handle,"host":self.summary,"sessions":sessions}).to_string();
            if reply.len() > BODY_LIMIT { return Err(MezError::invalid_state("outbound host-list reply exceeds limit")); }
            self.connected.prepared.frontend.stream.get_mut().write_all(&encode_frame(&ProtocolFrame::new(CONTENT_TYPE, reply))).await
                .map_err(|_| MezError::invalid_state("outbound host-list delivery unavailable"))?;
            Ok(())
        }).await.map_err(|_| MezError::invalid_state("outbound host-list timed out"))?
    }
}

/// Correlates the host reply before projecting only allowlisted lease facts.
fn project_response(
    body: &str,
) -> Result<Vec<crate::host::outbound_frontend::listing::ListedSession>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("outbound host-list response invalid"))?;
    if value["jsonrpc"] != "2.0" || value["id"] != LIST_ID || value.get("error").is_some() {
        return Err(MezError::forbidden(
            "outbound host-list rejected or uncorrelated",
        ));
    }
    crate::host::outbound_frontend::listing::project_sessions(
        value
            .pointer("/result/sessions")
            .ok_or_else(|| MezError::invalid_state("outbound host-list sessions unavailable"))?,
    )
}

#[cfg(test)]
mod tests;
