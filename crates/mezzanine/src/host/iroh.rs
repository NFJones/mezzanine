//! Persistent-host ownership for Iroh identity, trust, and host-only setup.
//!
//! This component owns one endpoint identity and one trust database for the
//! complete host rather than for an individual session. Its listener accepts
//! only protocol-v3 `host_only` initialization in this phase. Pairing and
//! profile health therefore cannot allocate, select, or attach a session; the
//! later routing layer will extend the authenticated pre-session boundary for
//! explicit attach and create intents.

use std::future::Future;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinSet;

use crate::control::{decode_control_frame, encode_control_body};
use crate::error::{MezError, MezErrorKind, Result};
use crate::runtime::{
    IrohCompressionBridge, IrohCompressionPolicy, RuntimeIrohCompressionCodec, RuntimeIrohEndpoint,
    RuntimeIrohIdentityPolicy, RuntimeIrohTransportPolicy, bind_runtime_iroh_endpoint,
};
use crate::security::remote::{RemoteEndpointIdentity, RemoteRoleCeiling, RemoteTrustStore};

const HOST_CONTROL_MAX_CONTENT_LENGTH: usize = 1024 * 1024;

/// Stable host endpoint, trust store, and bounded pre-session listener.
#[derive(Debug)]
pub(crate) struct HostIrohRuntime {
    identity: RemoteEndpointIdentity,
    trust: RemoteTrustStore,
    endpoint: RuntimeIrohEndpoint,
}

impl HostIrohRuntime {
    /// Binds one host-scoped endpoint when inbound Iroh is enabled.
    pub(crate) async fn bind(
        config_root: &Path,
        policy: RuntimeIrohTransportPolicy,
    ) -> Result<Option<Self>> {
        if !policy.enabled {
            return Ok(None);
        }
        if policy.identity != RuntimeIrohIdentityPolicy::Host {
            return Err(MezError::config(
                "persistent host Iroh requires transport.iroh.identity = host",
            ));
        }
        let identity = RemoteEndpointIdentity::load_or_create_host(config_root)?;
        let trust = RemoteTrustStore::under_host_config_root(config_root)?;
        let endpoint = bind_runtime_iroh_endpoint(policy, identity.secret_key().clone())
            .await?
            .ok_or_else(|| MezError::invalid_state("enabled host Iroh endpoint was not bound"))?;
        Ok(Some(Self {
            identity,
            trust,
            endpoint,
        }))
    }

    /// Stable public endpoint identity retained across host restarts.
    pub(crate) fn endpoint_id(&self) -> &str {
        self.identity.endpoint_id()
    }

    /// Latest dialable endpoint address.
    pub(crate) fn endpoint_addr(&self) -> Option<iroh::EndpointAddr> {
        self.endpoint.endpoint_addr()
    }

    /// Creates a host-scoped pairing invitation without provisioning a session.
    pub(crate) fn create_invitation(
        &self,
        profile_name: &str,
        role: RemoteRoleCeiling,
        ttl_seconds: u64,
        now_unix_seconds: u64,
    ) -> Result<Value> {
        if profile_name.trim().is_empty() || profile_name.chars().any(char::is_control) {
            return Err(MezError::invalid_args(
                "host Iroh profile name must be non-empty printable text",
            ));
        }
        let server_addr = self
            .endpoint_addr()
            .ok_or_else(|| MezError::invalid_state("host Iroh endpoint has no dialable address"))?;
        let server_addr = foreign_machine_invitation_addr(server_addr, self.endpoint.policy())?;
        let invitation = self.trust.create_invitation(
            self.endpoint_id(),
            role,
            ttl_seconds,
            now_unix_seconds,
        )?;
        Ok(json!({
            "format_version": 1,
            "profile_scope": "host",
            "profile_name": profile_name,
            "invitation_id": invitation.invitation_id,
            "token": invitation.token.expose_secret(),
            "server_endpoint_id": invitation.server_endpoint_id,
            "server_addr": server_addr,
            "role": invitation.role_ceiling.as_str(),
            "expires_at_unix_seconds": invitation.expires_at_unix_seconds,
        }))
    }

    /// Serves bounded host-only initialization until cancellation.
    pub(crate) async fn serve<C>(&self, cancellation: C) -> Result<u64>
    where
        C: Future<Output = ()>,
    {
        let endpoint = self.endpoint.endpoint().clone();
        let policy = self.endpoint.policy().clone();
        let trust = self.trust.clone();
        let server_endpoint_id = self.endpoint_id().to_string();
        let mut tasks = JoinSet::new();
        let mut accepted = 0u64;
        tokio::pin!(cancellation);

        loop {
            tokio::select! {
                () = &mut cancellation => break,
                incoming = endpoint.accept(), if tasks.len() < policy.max_connections => {
                    let Some(incoming) = incoming else { break; };
                    let policy = policy.clone();
                    let trust = trust.clone();
                    let server_endpoint_id = server_endpoint_id.clone();
                    tasks.spawn(async move {
                        serve_host_only_connection(
                            incoming,
                            policy,
                            trust,
                            server_endpoint_id,
                        ).await
                    });
                    accepted = accepted.saturating_add(1);
                }
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    let _connection_result = joined;
                }
            }
        }

        let _ = self.endpoint.shutdown_handle().close().await;
        let drain = async {
            while let Some(joined) = tasks.join_next().await {
                let _connection_result = joined;
            }
            Ok::<(), MezError>(())
        };
        if tokio::time::timeout(policy.setup_timeout, drain)
            .await
            .is_err()
        {
            tasks.abort_all();
        }
        Ok(accepted)
    }
}

async fn serve_host_only_connection(
    incoming: iroh::endpoint::Incoming,
    policy: RuntimeIrohTransportPolicy,
    trust: RemoteTrustStore,
    server_endpoint_id: String,
) -> Result<()> {
    let mut accepting = incoming
        .accept()
        .map_err(|error| MezError::invalid_state(format!("host Iroh accept failed: {error}")))?;
    let alpn = tokio::time::timeout(policy.setup_timeout, accepting.alpn())
        .await
        .map_err(|_| MezError::invalid_state("host Iroh ALPN setup timed out"))?
        .map_err(|error| MezError::invalid_state(format!("host Iroh ALPN failed: {error}")))?;
    let codec = RuntimeIrohCompressionCodec::from_alpn(&alpn)?;
    if !policy.compression_codecs.contains(&codec) {
        return Err(MezError::forbidden("host Iroh negotiated a disabled codec"));
    }
    let compression = IrohCompressionPolicy::new(
        codec,
        policy.compression_min_bytes,
        policy.compression_zstd_level,
        HOST_CONTROL_MAX_CONTENT_LENGTH + 1024,
    )?;
    let connection = tokio::time::timeout(policy.setup_timeout, accepting)
        .await
        .map_err(|_| MezError::invalid_state("host Iroh connection setup timed out"))?
        .map_err(|error| {
            MezError::invalid_state(format!("host Iroh connection failed: {error}"))
        })?;
    connection.set_max_concurrent_bi_streams(iroh::endpoint::VarInt::from_u32(1));
    connection.set_max_concurrent_uni_streams(iroh::endpoint::VarInt::from_u32(0));
    let client_endpoint_id = connection.remote_id().to_string();
    let (send, recv) = tokio::time::timeout(policy.setup_timeout, connection.accept_bi())
        .await
        .map_err(|_| MezError::invalid_state("host Iroh control stream setup timed out"))?
        .map_err(|error| MezError::invalid_state(format!("host Iroh stream failed: {error}")))?;
    let mut bridge =
        IrohCompressionBridge::spawn(recv, send, compression, HOST_CONTROL_MAX_CONTENT_LENGTH)?;
    let request = read_one_control_frame(bridge.stream_mut(), policy.idle_timeout).await?;
    let initialized = match handle_host_only_initialize(
        &request,
        &trust,
        &server_endpoint_id,
        &client_endpoint_id,
    ) {
        Ok(initialized) => initialized,
        Err(error) => HostOnlyInitializeResponse {
            body: host_json_rpc_error(request_id(&request), &error),
            redemption: None,
        },
    };
    let write_result = tokio::time::timeout(
        policy.idle_timeout,
        bridge
            .stream_mut()
            .write_all(&encode_control_body(&initialized.body)),
    )
    .await
    .map_err(|_| MezError::invalid_state("host Iroh response write timed out"))
    .and_then(|result| result.map_err(Into::into));
    if let Err(error) = write_result {
        if let Some(redemption) = initialized.redemption.as_ref() {
            let _ = trust.rollback_invitation_redemption(redemption);
        }
        return Err(error);
    }
    let flush_result = tokio::time::timeout(policy.idle_timeout, bridge.stream_mut().flush())
        .await
        .map_err(|_| MezError::invalid_state("host Iroh response flush timed out"))
        .and_then(|result| result.map_err(Into::into));
    if let Err(error) = flush_result {
        if let Some(redemption) = initialized.redemption.as_ref() {
            let _ = trust.rollback_invitation_redemption(redemption);
        }
        return Err(error);
    }
    bridge.shutdown(policy.setup_timeout).await?;
    connection.close(iroh::endpoint::VarInt::from_u32(0), b"host-only complete");
    Ok(())
}

struct HostOnlyInitializeResponse {
    body: String,
    redemption: Option<crate::security::remote::RemotePairingRedemption>,
}

fn handle_host_only_initialize(
    body: &str,
    trust: &RemoteTrustStore,
    server_endpoint_id: &str,
    client_endpoint_id: &str,
) -> Result<HostOnlyInitializeResponse> {
    let request: Value = serde_json::from_str(body).map_err(|error| {
        MezError::invalid_args(format!("invalid host initialize JSON: {error}"))
    })?;
    if request.get("method").and_then(Value::as_str) != Some("control/initialize") {
        return Err(MezError::forbidden(
            "host Iroh requires control/initialize as the first request",
        ));
    }
    let params = request
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| MezError::invalid_args("host initialize requires params"))?;
    if params.get("requested_version").and_then(Value::as_u64) != Some(3)
        || params.get("session_intent").and_then(Value::as_str) != Some("host_only")
        || params
            .get("session_target")
            .is_some_and(|value| !value.is_null())
        || params
            .get("idempotency_key")
            .is_some_and(|value| !value.is_null())
    {
        return Err(MezError::forbidden(
            "host-only Iroh initialization must use protocol version 3 without a session target",
        ));
    }
    let requested_role = match params.get("requested_role").and_then(Value::as_str) {
        Some("observer") => crate::control::RequestedRole::Observer,
        _ => return Err(MezError::forbidden("host-only Iroh requires observer role")),
    };
    let client_name = params
        .get("client_name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MezError::invalid_args("host initialize requires client_name"))?;
    let authentication = params
        .get("authentication")
        .and_then(Value::as_object)
        .ok_or_else(|| MezError::forbidden("host Iroh requires pairing or device proof"))?;
    let mechanism = authentication
        .get("mechanism")
        .and_then(Value::as_str)
        .ok_or_else(|| MezError::forbidden("host Iroh authentication mechanism is missing"))?;
    let token = SecretString::from(
        authentication
            .get("token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| MezError::forbidden("host Iroh authentication token is missing"))?
            .to_string(),
    );
    let now = current_unix_seconds()?;
    let (principal, issued_credential, redemption) = match mechanism {
        "extension:iroh_invitation" => {
            let preparation = trust.prepare_invitation(
                &token,
                server_endpoint_id,
                client_endpoint_id,
                client_name,
                requested_role,
                now,
            )?;
            let principal = preparation.principal();
            let redemption = trust.commit_invitation(preparation, now)?;
            (
                principal,
                Some(redemption.device_credential.clone()),
                Some(redemption),
            )
        }
        "extension:iroh_device" => (
            trust.resolve_principal(
                server_endpoint_id,
                client_endpoint_id,
                &token,
                requested_role,
                now,
            )?,
            None,
            None,
        ),
        _ => {
            return Err(MezError::forbidden(
                "unsupported host Iroh authentication mechanism",
            ));
        }
    };
    let mut result = json!({
        "selected_version": 3,
        "server": {
            "id": server_endpoint_id,
            "implementation_name": "mez",
            "version": env!("CARGO_PKG_VERSION"),
            "protocol_versions": [3]
        },
        "host": { "endpoint_id": server_endpoint_id },
        "lease": null,
        "session": null,
        "client": null,
        "granted_role": "observer",
        "capabilities": { "methods": ["control/shutdown"], "features": { "host_only": true } },
        "approval_pending": false,
        "observer_request": null,
        "principal_id": principal.trust_record_id,
    });
    if let Some(credential) = issued_credential {
        result["device_credential"] = Value::String(credential.expose_secret().to_string());
    }
    Ok(HostOnlyInitializeResponse {
        body: json!({
            "jsonrpc": "2.0",
            "id": request.get("id").cloned().unwrap_or(Value::Null),
            "result": result,
        })
        .to_string(),
        redemption,
    })
}

async fn read_one_control_frame<S>(stream: &mut S, timeout: std::time::Duration) -> Result<String>
where
    S: tokio::io::AsyncRead + Unpin,
{
    tokio::time::timeout(timeout, async {
        let mut input = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = stream.read(&mut buffer).await?;
            if read == 0 {
                return Err(MezError::invalid_state(
                    "host Iroh stream closed before initialize",
                ));
            }
            input.extend_from_slice(&buffer[..read]);
            if input.len() > HOST_CONTROL_MAX_CONTENT_LENGTH + 8192 {
                return Err(MezError::invalid_args(
                    "host Iroh initialize frame exceeds limit",
                ));
            }
            if let Ok((body, consumed)) =
                decode_control_frame(&input, HOST_CONTROL_MAX_CONTENT_LENGTH)
            {
                if consumed != input.len() {
                    return Err(MezError::invalid_args(
                        "host Iroh accepts exactly one setup frame",
                    ));
                }
                return Ok(body);
            }
        }
    })
    .await
    .map_err(|_| MezError::invalid_state("host Iroh initialize read timed out"))?
}

fn request_id(body: &str) -> Value {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("id").cloned())
        .unwrap_or(Value::Null)
}

fn host_json_rpc_error(id: Value, error: &MezError) -> String {
    let code = match error.kind() {
        MezErrorKind::InvalidArgs => -32602,
        MezErrorKind::Forbidden => -32002,
        MezErrorKind::Conflict => -32006,
        MezErrorKind::NotFound => -32005,
        _ => -32004,
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": error.message(),
            "data": { "mezzanine_code": format!("{:?}", error.kind()).to_lowercase() }
        }
    })
    .to_string()
}

fn current_unix_seconds() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| MezError::invalid_state("system clock is before the Unix epoch"))
}

/// Retains only routes suitable for an invitation transferred to another host.
fn foreign_machine_invitation_addr(
    mut endpoint_addr: iroh::EndpointAddr,
    policy: &RuntimeIrohTransportPolicy,
) -> Result<iroh::EndpointAddr> {
    endpoint_addr.addrs.retain(|addr| match addr {
        iroh::TransportAddr::Relay(_) => true,
        iroh::TransportAddr::Ip(addr) => {
            policy.bind_port != 0
                && addr.port() == policy.bind_port
                && !addr.ip().is_loopback()
                && !addr.ip().is_unspecified()
        }
        _ => false,
    });
    if endpoint_addr.is_empty() {
        return Err(MezError::invalid_state(
            "host Iroh listener has no foreign-machine route; configure a stable bind_port with a non-loopback address or enable a relay",
        ));
    }
    Ok(endpoint_addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "mez-host-iroh-{label}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    /// Host identity and trust are stable across restart and isolated from
    /// legacy per-session security roots.
    #[test]
    fn host_security_is_stable_locked_and_session_independent() {
        let root = test_root("security");
        let host = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
        let endpoint_id = host.endpoint_id().to_string();
        assert!(RemoteEndpointIdentity::load_or_create_host(&root).is_err());
        let session = RemoteEndpointIdentity::load_or_create(&root, "$1").unwrap();
        assert_ne!(session.endpoint_id(), endpoint_id);
        let host_trust = RemoteTrustStore::under_host_config_root(&root).unwrap();
        let session_trust = RemoteTrustStore::under_config_root(&root, "$1").unwrap();
        assert_ne!(host_trust.directory(), session_trust.directory());
        drop(host);
        let restarted = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
        assert_eq!(restarted.endpoint_id(), endpoint_id);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Host-only initialization commits trust while returning no session or
    /// lease and rejects every session-routing intent before allocation.
    #[test]
    fn host_only_initialize_pairs_without_session_authority() {
        let root = test_root("initialize");
        let identity = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
        let trust = RemoteTrustStore::under_host_config_root(&root).unwrap();
        let now = current_unix_seconds().unwrap();
        let invitation = trust
            .create_invitation(
                identity.endpoint_id(),
                RemoteRoleCeiling::Observer,
                600,
                now,
            )
            .unwrap();
        let client_id = iroh::SecretKey::generate().public().to_string();
        let request = json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "control/initialize",
            "params": {
                "client_name": "test-client",
                "requested_version": 3,
                "requested_role": "observer",
                "session_intent": "host_only",
                "authentication": {
                    "mechanism": "extension:iroh_invitation",
                    "token": invitation.token.expose_secret()
                }
            }
        })
        .to_string();
        let response =
            handle_host_only_initialize(&request, &trust, identity.endpoint_id(), &client_id)
                .unwrap();
        let response: Value = serde_json::from_str(&response.body).unwrap();
        assert!(response["result"]["session"].is_null());
        assert!(response["result"]["lease"].is_null());
        assert!(response["result"]["device_credential"].is_string());
        assert_eq!(trust.list_records().unwrap().len(), 1);

        let attach = request.replace("host_only", "attach");
        assert!(
            handle_host_only_initialize(&attach, &trust, identity.endpoint_id(), &client_id,)
                .is_err()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The bound host front door must isolate an invalid routing intent, then
    /// pair and reconnect the same client endpoint without ever returning
    /// session or lease authority.
    #[tokio::test(flavor = "current_thread")]
    async fn host_listener_isolates_invalid_intent_and_reconnects_device() {
        let root = test_root("listener");
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            identity: RuntimeIrohIdentityPolicy::Host,
            compression_codecs: vec![RuntimeIrohCompressionCodec::None],
            setup_timeout: std::time::Duration::from_secs(2),
            idle_timeout: std::time::Duration::from_secs(2),
            ..RuntimeIrohTransportPolicy::default()
        };
        let host = HostIrohRuntime::bind(&root, policy.clone())
            .await
            .unwrap()
            .unwrap();
        assert!(
            host.create_invitation(
                "unreachable",
                RemoteRoleCeiling::Observer,
                600,
                current_unix_seconds().unwrap()
            )
            .is_err()
        );
        let invitation = host
            .trust
            .create_invitation(
                host.endpoint_id(),
                RemoteRoleCeiling::Observer,
                600,
                current_unix_seconds().unwrap(),
            )
            .unwrap();
        let server_addr = host.endpoint_addr().unwrap();
        let client =
            crate::runtime::bind_runtime_iroh_client_endpoint(&policy, iroh::SecretKey::generate())
                .await
                .unwrap();
        let stop = std::sync::Arc::new(tokio::sync::Notify::new());
        let server_stop = stop.clone();

        let server = host.serve(async move { server_stop.notified().await });
        let client_work = async {
            let invalid = exchange_test_host_initialize(
                &client,
                &server_addr,
                "extension:iroh_invitation",
                invitation.token.expose_secret(),
                "attach",
            )
            .await;
            assert!(invalid.get("error").is_some(), "{invalid}");

            let paired = exchange_test_host_initialize(
                &client,
                &server_addr,
                "extension:iroh_invitation",
                invitation.token.expose_secret(),
                "host_only",
            )
            .await;
            assert!(paired["result"]["session"].is_null(), "{paired}");
            assert!(paired["result"]["lease"].is_null(), "{paired}");
            let credential = paired["result"]["device_credential"]
                .as_str()
                .unwrap()
                .to_string();

            let checked = exchange_test_host_initialize(
                &client,
                &server_addr,
                "extension:iroh_device",
                &credential,
                "host_only",
            )
            .await;
            assert!(checked["result"]["session"].is_null(), "{checked}");
            assert!(checked["result"]["lease"].is_null(), "{checked}");
            stop.notify_one();
        };

        let (served, ()) = tokio::join!(server, client_work);
        assert_eq!(served.unwrap(), 3);
        assert_eq!(host.trust.list_records().unwrap().len(), 1);
        client.close().await;
        drop(host);
        let restarted = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
        assert_eq!(restarted.endpoint_id(), server_addr.id.to_string());
        let _ = std::fs::remove_dir_all(root);
    }

    async fn exchange_test_host_initialize(
        client: &iroh::Endpoint,
        server_addr: &iroh::EndpointAddr,
        mechanism: &str,
        token: &str,
        intent: &str,
    ) -> Value {
        let connection = client
            .connect(server_addr.clone(), crate::runtime::MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        let (send, recv) = connection.open_bi().await.unwrap();
        let compression = IrohCompressionPolicy::new(
            RuntimeIrohCompressionCodec::None,
            512,
            3,
            HOST_CONTROL_MAX_CONTENT_LENGTH + 1024,
        )
        .unwrap();
        let mut bridge =
            IrohCompressionBridge::spawn(recv, send, compression, HOST_CONTROL_MAX_CONTENT_LENGTH)
                .unwrap();
        let request = json!({
            "jsonrpc": "2.0",
            "id": "test-init",
            "method": "control/initialize",
            "params": {
                "client_name": "test-client",
                "requested_version": 3,
                "requested_role": "observer",
                "session_intent": intent,
                "authentication": { "mechanism": mechanism, "token": token }
            }
        })
        .to_string();
        bridge
            .stream_mut()
            .write_all(&encode_control_body(&request))
            .await
            .unwrap();
        bridge.stream_mut().shutdown().await.unwrap();
        let response =
            read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(2))
                .await
                .unwrap();
        let _ = bridge.shutdown(std::time::Duration::from_secs(2)).await;
        connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
        serde_json::from_str(&response).unwrap()
    }
}
