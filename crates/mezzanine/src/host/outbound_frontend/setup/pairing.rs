//! Protected invitation pairing on the retained outbound endpoint.
//!
//! Frontend input contains only an exact handle, protected file path and optional
//! alias. File/preflight and publication workers retain endpoint and finite slot
//! ownership until actual exit, even if their waiter is cancelled. One host-only
//! redemption uses the pinned connection; uncertain outcomes never replay.
//! Issued device proof is validated and persisted inside the owner, never returned
//! through local IPC. Successful local response proves publication, not a session
//! allocation. Cancellation cannot recall already-started filesystem writes.

use super::*;
use crate::security::remote::{ParsedIrohInvitation, read_iroh_invitation};
use secrecy::{ExposeSecret, SecretString};
use tokio::io::AsyncWriteExt;

const PAIR_ID: &str = "outbound-host-pair";
const BODY_LIMIT: usize = 1024 * 1024;

/// Closed operation: no caller-supplied token, route or initialization fields.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairRequest {
    operation: String,
    handle: FrontendHandle,
    path: std::path::PathBuf,
    save_as: Option<String>,
}

/// Consumed protected evidence, not yet redeemed or published profile authority.
pub(in crate::host::outbound_frontend) struct PreparedPairing {
    frontend: AdmittedFrontend,
    invitation: ParsedIrohInvitation,
}

/// Reads bounded protected evidence outside the async supervisor. Capacity and
/// endpoint ownership survive a cancelled waiter until that worker actually exits.
pub(super) async fn prepare(frontend: AdmittedFrontend, body: &str) -> Result<PreparedPairing> {
    let request: PairRequest = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_args("outbound pairing request invalid"))?;
    validate_request(&request, &frontend.handle)?;
    let root = frontend._endpoint.frontend_config_root()?.to_path_buf();
    let endpoint = frontend._endpoint.clone();
    let slot = frontend._slot.clone();
    let invitation = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        endpoint.frontend_config_root()?;
        let invitation = read_iroh_invitation(&request.path, request.save_as.as_deref())?;
        require_host_unexpired(&invitation)?;
        RemoteClientProfileStore::under_config_root(&root).preflight_for_outbound(
            &invitation.profile_name,
            invitation.server_addr.id,
            invitation.scope,
        )?;
        endpoint.frontend_config_root()?;
        Ok::<_, MezError>(invitation)
    })
    .await
    .map_err(|_| MezError::invalid_state("outbound invitation worker unavailable"))?
    .map_err(|_| MezError::invalid_state("outbound protected invitation unavailable"))?;
    frontend._endpoint.frontend_config_root()?;
    Ok(PreparedPairing {
        frontend,
        invitation,
    })
}

impl PreparedPairing {
    /// Redeems exactly once, publishes owner-side proof, then sends only the
    /// exact handle and local alias. Failure drops this operation's connection;
    /// neither lost replies nor storage faults authorize repeating redemption.
    pub(in crate::host::outbound_frontend) async fn deliver(mut self) -> Result<()> {
        let budget = self.frontend._endpoint.transport_policy().setup_timeout;
        tokio::time::timeout(budget, async move {
            require_host_unexpired(&self.invitation)?;
            let (connection, compression) = connection::connect_to_pinned(
                &self.frontend._endpoint, &self.invitation.server_addr,
            ).await?;
            require_host_unexpired(&self.invitation)?;
            self.frontend._endpoint.frontend_config_root()?;
            let (send, recv) = connection.connection().open_bi().await
                .map_err(|_| MezError::invalid_state("outbound pairing stream unavailable"))?;
            let mut bridge = crate::runtime::IrohCompressionBridge::spawn(recv, send, compression, BODY_LIMIT)?;
            let body = serde_json::json!({"jsonrpc":"2.0","id":PAIR_ID,"method":"control/initialize","params":{
                "client_name":"remote-cli","requested_version":3,"requested_role":"observer",
                "session_intent":"host_only","client":{"name":"remote-cli","interactive":false,"purpose":"pairing"},
                "authentication":{"mechanism":"extension:iroh_invitation","token":self.invitation.token.expose_secret()}
            }}).to_string();
            bridge.stream_mut().write_all(&crate::control::encode_control_body(&body)).await
                .map_err(|_| MezError::invalid_state("outbound pairing write unavailable; outcome unknown"))?;
            let reply = connection::initialize::read_exact_frame(bridge.stream_mut()).await?;
            let credential = validate_response(&reply, self.invitation.server_addr.id)?;
            self.frontend._endpoint.frontend_config_root()?;
            let name = self.invitation.profile_name.clone();
            let profile = RemoteClientProfile {
                name: name.clone(), server_addr: self.invitation.server_addr,
                role: self.invitation.role, scope: self.invitation.scope,
                device_credential: credential,
            };
            let root = self.frontend._endpoint.frontend_config_root()?.to_path_buf();
            let endpoint = self.frontend._endpoint.clone();
            let slot = self.frontend._slot.clone();
            tokio::task::spawn_blocking(move || {
                let _slot = slot;
                endpoint.frontend_config_root()?;
                RemoteClientProfileStore::under_config_root(&root).save_for_outbound(&profile)?;
                endpoint.frontend_config_root()?;
                Ok::<_, MezError>(())
            }).await.map_err(|_| MezError::invalid_state("outbound pairing publication worker unavailable; outcome unknown"))?
                .map_err(|_| MezError::invalid_state("outbound pairing publication unavailable; inspect profile before retrying"))?;
            self.frontend._endpoint.frontend_config_root()?;
            let reply = serde_json::json!({"handle":self.frontend.handle,"paired":true,"profile":name}).to_string();
            self.frontend.stream.get_mut().write_all(&crate::protocol::framing::encode_frame(
                &ProtocolFrame::new(CONTENT_TYPE, reply),
            )).await.map_err(|_| MezError::invalid_state("outbound pairing delivery unavailable; inspect profile before retrying"))?;
            Ok(())
        }).await.map_err(|_| MezError::invalid_state("outbound pairing timed out; outcome unknown; inspect profile before retrying"))?
    }
}

/// Checks the exact admitted owner and bounded absolute path before file I/O.
fn validate_request(request: &PairRequest, handle: &FrontendHandle) -> Result<()> {
    if request.operation != "pair" || request.handle != *handle {
        return Err(MezError::conflict("outbound pairing owner changed"));
    }
    if !request.path.is_absolute()
        || request.path.as_os_str().len() > 2048
        || request.save_as.as_ref().is_some_and(|name| {
            name.is_empty() || name.len() > 128 || name.chars().any(char::is_control)
        })
    {
        return Err(MezError::invalid_args(
            "outbound pairing path or alias invalid",
        ));
    }
    Ok(())
}

/// Preserves conservative scope and authored expiry before any redemption.
fn require_host_unexpired(invitation: &ParsedIrohInvitation) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| MezError::invalid_state("outbound pairing clock unavailable"))?
        .as_secs();
    if invitation.scope != RemoteClientProfileScope::Host
        || now > invitation.expires_at_unix_seconds
    {
        return Err(MezError::forbidden(
            "outbound host invitation scope or expiry invalid",
        ));
    }
    Ok(())
}

/// Correlates pinned host-only settlement before retaining its issued credential.
/// Unknown metadata and error bodies are never returned or used in diagnostics.
fn validate_response(body: &str, server: iroh::EndpointId) -> Result<SecretString> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("outbound pairing response invalid"))?;
    if value["jsonrpc"] != "2.0"
        || value["id"] != PAIR_ID
        || value.get("error").is_some()
        || value.pointer("/result/selected_version") != Some(&serde_json::json!(3))
        || value.pointer("/result/granted_role") != Some(&serde_json::json!("observer"))
        || value
            .pointer("/result/host/endpoint_id")
            .and_then(serde_json::Value::as_str)
            != Some(server.to_string().as_str())
        || !["session", "lease", "client"].iter().all(|key| {
            value["result"]
                .get(*key)
                .is_some_and(serde_json::Value::is_null)
        })
        || value.pointer("/result/capabilities/features/host_only")
            != Some(&serde_json::Value::Bool(true))
    {
        return Err(MezError::forbidden(
            "outbound pairing rejected or settlement changed; outcome unknown",
        ));
    }
    value
        .pointer("/result/device_credential")
        .and_then(serde_json::Value::as_str)
        .filter(|credential| !credential.is_empty() && credential.len() <= 4096)
        .map(|credential| SecretString::from(credential.to_string()))
        .ok_or_else(|| {
            MezError::invalid_state("outbound pairing credential unavailable; outcome unknown")
        })
}

#[cfg(test)]
mod tests;
