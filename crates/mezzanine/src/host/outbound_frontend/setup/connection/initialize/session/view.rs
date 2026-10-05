//! Consumed display-only view delivery on one initialized session connection.
//!
//! Local requests cannot name a different session, client, method or credential.
//! The response projects rendered lines only, not arbitrary peer metadata. This
//! snapshot is not the full attached renderer: styles/events/X11/input and
//! presentation acknowledgement remain separate unfinished contracts. Failure
//! consumes the connection rather than permitting replay on a desynchronized
//! request stream; success returns the same exact owner for a subsequent request.

use super::*;

const VIEW_REQUEST_ID: &str = "outbound-session-view";

mod step;

/// Closed display request; geometry is the only frontend-controlled parameter.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewRequest {
    handle: FrontendHandle,
    columns: u16,
    rows: u16,
}

impl InitializedSessionFrontend {
    /// Consumes one local request and delivers a bounded line snapshot under a
    /// total deadline. No terminal input or presentation receipt is acknowledged.
    /// On success the returned owner remains bound to the same session/client.
    pub(crate) async fn deliver_view(mut self) -> Result<Self> {
        let deadline = self
            .connected
            .prepared
            .frontend
            ._endpoint
            .transport_policy()
            .setup_timeout;
        tokio::time::timeout(deadline, async move {
            let frame = self
                .connected
                .prepared
                .frontend
                .stream
                .next()
                .await
                .transpose()?
                .ok_or_else(|| {
                    MezError::invalid_state("outbound local view request unavailable")
                })?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_args(
                    "outbound local view content type unsupported",
                ));
            }
            let envelope: serde_json::Value = serde_json::from_str(&frame.body)
                .map_err(|_| MezError::invalid_args("outbound local request invalid"))?;
            if envelope.get("operation").is_some() {
                return step::deliver_step(self, &frame.body).await;
            }
            let request: ViewRequest = serde_json::from_str(&frame.body)
                .map_err(|_| MezError::invalid_args("outbound local view request invalid"))?;
            if request.handle != self.connected.prepared.frontend.handle {
                return Err(MezError::conflict("outbound frontend handle changed"));
            }
            if !(1..=4096).contains(&request.columns) || !(1..=4096).contains(&request.rows) {
                return Err(MezError::invalid_args(
                    "outbound local view geometry unavailable",
                ));
            }
            self.connected
                .prepared
                .frontend
                ._endpoint
                .frontend_config_root()?;
            let body = serde_json::json!({"jsonrpc":"2.0","id":VIEW_REQUEST_ID,
                "method":"terminal/view","params":{"client_size":{
                    "columns":request.columns,"rows":request.rows}}})
            .to_string();
            self.bridge
                .stream_mut()
                .write_all(&crate::control::encode_control_body(&body))
                .await
                .map_err(|_| MezError::invalid_state("outbound view write unavailable"))?;
            let response = read_exact_frame(self.bridge.stream_mut()).await?;
            let lines = project_view_lines(&response, &request, &self.summary)?;
            let styles = project_view_styles(&response, lines.len(), request.columns)?;
            let modes = project_view_modes(&response, request.columns, request.rows)?;
            self.connected
                .prepared
                .frontend
                ._endpoint
                .frontend_config_root()?;
            let body = serde_json::json!({"handle":request.handle,"session":self.summary,
                "lines":lines,"line_style_spans":styles,
                "cursor":modes["cursor"],"output_modes":modes["output_modes"]})
            .to_string();
            if body.len() > BODY_LIMIT {
                return Err(MezError::invalid_state(
                    "outbound local view response exceeds limit",
                ));
            }
            // The hello/setup decoder retains its small inbound limit. Sending
            // a larger snapshot uses bounded wire encoding, not a widened decoder.
            let stream = self.connected.prepared.frontend.stream.get_mut();
            stream
                .write_all(&encode_frame(&ProtocolFrame::new(CONTENT_TYPE, body)))
                .await
                .map_err(|_| MezError::invalid_state("outbound local view delivery unavailable"))?;
            Ok(self)
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound view delivery timed out"))?
    }
}

/// Validates correlation, role and requested client geometry before projecting
/// only bounded terminal lines. Unknown peer fields are never forwarded.
fn project_view_lines(
    body: &str,
    request: &ViewRequest,
    session: &serde_json::Value,
) -> Result<Vec<String>> {
    let response: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("outbound view response invalid"))?;
    if response["jsonrpc"] != "2.0"
        || response["id"] != VIEW_REQUEST_ID
        || response.get("error").is_some()
    {
        return Err(MezError::forbidden(
            "outbound view rejected or uncorrelated",
        ));
    }
    let view = response
        .pointer("/result/view")
        .ok_or_else(|| MezError::invalid_state("outbound view unavailable"))?;
    if view["role"] != session["granted_role"]
        || view
            .pointer("/client_size/columns")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(request.columns))
        || view
            .pointer("/client_size/rows")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(request.rows))
    {
        return Err(MezError::forbidden(
            "outbound view ownership or geometry mismatch",
        ));
    }
    let lines = view["lines"]
        .as_array()
        .filter(|lines| lines.len() <= usize::from(request.rows))
        .ok_or_else(|| MezError::invalid_state("outbound view lines exceed geometry"))?;
    lines
        .iter()
        .map(|line| {
            line.as_str()
                .map(str::to_string)
                .ok_or_else(|| MezError::invalid_state("outbound view line invalid"))
        })
        .collect()
}

/// Retains only decoded, geometry-bounded rendition fields from the peer view.
fn project_view_styles(body: &str, line_count: usize, columns: u16) -> Result<serde_json::Value> {
    let response: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("outbound view response invalid"))?;
    let value = response
        .pointer("/result/view/line_style_spans")
        .ok_or_else(|| MezError::invalid_state("outbound view styles unavailable"))?;
    let rows = crate::host::terminal::wire_styles::bounded_style_rows(value, line_count, columns)?;
    Ok(crate::host::terminal::wire_styles::style_rows_value(&rows))
}

/// Projects shared decoded presentation facts only after viewport validation.
fn project_view_modes(body: &str, columns: u16, rows: u16) -> Result<serde_json::Value> {
    let response: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("outbound view response invalid"))?;
    let view = response
        .pointer("/result/view")
        .ok_or_else(|| MezError::invalid_state("outbound view unavailable"))?;
    let modes = crate::host::terminal::wire_modes::bounded_view_output_modes(view, columns, rows)?;
    Ok(crate::host::terminal::wire_modes::output_modes_view_value(
        modes,
    ))
}

#[cfg(test)]
mod tests;
