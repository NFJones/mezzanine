//! Owner-side initialization over one pinned connection.
//!
//! Private proof never enters the local response. Exactly one remote initialize
//! is issued: failures after writing are potentially ambiguous and never replay
//! application work. Separate validators bind host-only or session settlement;
//! event and X11 forwarding remain excluded. Reads consume exactly
//! one bounded frame, preserving bytes of subsequent frames in the bridge.

use super::*;
use crate::control::CONTROL_CONTENT_TYPE;
use crate::protocol::framing::{ProtocolFrame, decode_frame, encode_frame};
use crate::runtime::IrohCompressionBridge;
use secrecy::ExposeSecret;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BODY_LIMIT: usize = 1024 * 1024;
const REQUEST_ID: &str = "outbound-host-init";

/// Validated host-only connection with its local frontend and bridge retained.
/// No raw peer response is available for forwarding; only allowlisted facts.
pub(crate) struct InitializedHostFrontend {
    connected: ConnectedFrontend,
    bridge: IrohCompressionBridge,
    summary: serde_json::Value,
}

mod listing;
mod session;

impl ConnectedFrontend {
    /// Sends one host-only initialize under a single setup deadline. Unsupported
    /// session/stream modes reject before opening an application stream.
    pub(crate) async fn initialize_host_only(self) -> Result<InitializedHostFrontend> {
        let params = initialize_params_from_json(&self.prepared.initialize.to_string())?;
        if self.prepared.profile.scope != RemoteClientProfileScope::Host
            || params.session_intent != Some(SessionIntent::HostOnly)
            || params.requested_role != RequestedRole::Observer
            || params.event_stream_version.is_some()
            || params.x11_forwarding.is_some()
        {
            return Err(MezError::forbidden(
                "outbound host-only initialization required",
            ));
        }
        let (connected, bridge, summary) = self
            .initialize_once(|body, connected| {
                validate_host_response(body, connected.prepared.profile.server_addr.id)
            })
            .await?;
        Ok(InitializedHostFrontend {
            connected,
            bridge,
            summary,
        })
    }

    /// Issues exactly one owner-authenticated initialize and validates its reply
    /// before transferring the retained stream/connection. The validator cannot
    /// trigger retries, and no raw response is emitted on local IPC.
    async fn initialize_once<F>(
        self,
        validate: F,
    ) -> Result<(Self, IrohCompressionBridge, serde_json::Value)>
    where
        F: FnOnce(&str, &Self) -> Result<serde_json::Value>,
    {
        let deadline = self
            .prepared
            .frontend
            ._endpoint
            .transport_policy()
            .setup_timeout;
        tokio::time::timeout(deadline, async move {
            self.prepared.frontend._endpoint.frontend_config_root()?;
            let (send, recv) =
                self.connection.connection().open_bi().await.map_err(|_| {
                    MezError::invalid_state("outbound initialize stream unavailable")
                })?;
            let mut bridge =
                IrohCompressionBridge::spawn(recv, send, self.compression, BODY_LIMIT)?;
            let mut initialize = self.prepared.initialize.clone();
            initialize["authentication"] = serde_json::json!({
                "mechanism":"extension:iroh_device",
                "token":self.prepared.profile.device_credential.expose_secret()
            });
            let body = serde_json::json!({"jsonrpc":"2.0", "id":REQUEST_ID,
                "method":"control/initialize", "params":initialize})
            .to_string();
            bridge
                .stream_mut()
                .write_all(&encode_frame(&ProtocolFrame::new(
                    CONTROL_CONTENT_TYPE,
                    body,
                )))
                .await
                .map_err(|_| {
                    MezError::invalid_state(
                        "outbound initialize write unavailable; outcome unknown",
                    )
                })?;
            let response = read_exact_frame(bridge.stream_mut()).await?;
            let summary = validate(&response, &self)?;
            self.prepared.frontend._endpoint.frontend_config_root()?;
            Ok((self, bridge, summary))
        })
        .await
        .map_err(|_| {
            MezError::invalid_state("outbound initialization timed out; outcome unknown")
        })?
    }
}

/// Reads bounded headers and exactly the declared body, never discarding trailing
/// peer frames. Errors contain no peer payload or credentials.
async fn read_exact_frame(stream: &mut tokio::io::DuplexStream) -> Result<String> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        if bytes.len() >= 8192 {
            return Err(MezError::invalid_state(
                "outbound response header exceeds limit",
            ));
        }
        let byte = stream.read_u8().await.map_err(|_| {
            MezError::invalid_state("outbound initialize response unavailable; outcome unknown")
        })?;
        bytes.push(byte);
    }
    let header_len = bytes.len();
    // Use the shared codec to validate headers and declared length before body
    // allocation, including duplicate Content-Length and oversized bodies.
    use tokio_util::codec::Decoder;
    let mut codec = ProtocolFrameCodec::new(BODY_LIMIT)?;
    let mut header = tokio_util::bytes::BytesMut::from(bytes.as_slice());
    codec
        .decode(&mut header)
        .map_err(|_| MezError::invalid_state("outbound response framing invalid"))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| MezError::invalid_state("outbound response framing invalid"))?;
    let length = text
        .split("\r\n")
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .ok_or_else(|| MezError::invalid_state("outbound response framing invalid"))?;
    bytes.resize(header_len + length, 0);
    stream
        .read_exact(&mut bytes[header_len..])
        .await
        .map_err(|_| MezError::invalid_state("outbound response incomplete; outcome unknown"))?;
    let (frame, _) = decode_frame(&bytes, BODY_LIMIT)
        .map_err(|_| MezError::invalid_state("outbound response framing invalid"))?;
    if frame.content_type != CONTROL_CONTENT_TYPE {
        return Err(MezError::invalid_state(
            "outbound response content type unsupported",
        ));
    }
    Ok(frame.body)
}

/// Validates correlated host-only facts and returns a closed content-free
/// projection. Unexpected credentials never leave the owner, even on errors.
fn validate_host_response(body: &str, server: iroh::EndpointId) -> Result<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("outbound initialize response invalid"))?;
    if value["jsonrpc"] != "2.0" || value["id"] != REQUEST_ID || value.get("error").is_some() {
        return Err(MezError::forbidden(
            "outbound initialize rejected or uncorrelated",
        ));
    }
    let result = value
        .get("result")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("outbound initialize result unavailable"))?;
    if result.get("selected_version") != Some(&serde_json::json!(3))
        || result.get("granted_role") != Some(&serde_json::json!("observer"))
        || value
            .pointer("/result/host/endpoint_id")
            .and_then(serde_json::Value::as_str)
            != Some(server.to_string().as_str())
        || result.get("session") != Some(&serde_json::Value::Null)
        || result.get("lease") != Some(&serde_json::Value::Null)
        || result.get("client") != Some(&serde_json::Value::Null)
        || value.pointer("/result/capabilities/features/host_only")
            != Some(&serde_json::Value::Bool(true))
        || result.contains_key("device_credential")
    {
        return Err(MezError::forbidden(
            "outbound initialize settlement invalid",
        ));
    }
    Ok(serde_json::json!({"selected_version":3,"granted_role":"observer","host_only":true}))
}

#[cfg(test)]
mod tests;
