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

use crate::control::{
    AuthenticatedPeer, AuthenticationMechanism, ControlConnectionState, SessionIntent,
    decode_control_frame, encode_control_body, initialize_params_from_json,
};
use crate::error::{MezError, MezErrorKind, Result};
use crate::host::async_runtime::{
    AsyncRuntimeControlConnectionConfig,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots_and_post_flush,
};
use crate::host::router::HostSessionRouter;
use crate::runtime::{
    IrohCompressionBridge, IrohCompressionPolicy, RuntimeIrohCompressionCodec, RuntimeIrohEndpoint,
    RuntimeIrohIdentityPolicy, RuntimeIrohTransportPolicy, RuntimeLifecycleState,
    bind_runtime_iroh_endpoint, serve_host_routed_iroh_event_stream,
};
use crate::security::remote::{
    RemoteEndpointIdentity, RemoteHostRoutingAuthority, RemotePrincipal, RemoteRoleCeiling,
    RemoteTrustStore,
};
use crate::storage::lease::{RemoteSessionLease, RemoteSessionLeaseState};

const HOST_CONTROL_MAX_CONTENT_LENGTH: usize = 1024 * 1024;

/// Stable host endpoint, trust store, and bounded pre-session listener.
#[derive(Debug)]
pub(crate) struct HostIrohRuntime {
    identity: RemoteEndpointIdentity,
    trust: RemoteTrustStore,
    endpoint: RuntimeIrohEndpoint,
}

/// Cloneable local-administration view of one live host Iroh endpoint.
#[derive(Debug, Clone)]
pub(crate) struct HostIrohInvitationIssuer {
    endpoint: iroh::Endpoint,
    endpoint_id: String,
    policy: RuntimeIrohTransportPolicy,
    trust: RemoteTrustStore,
}

impl HostIrohInvitationIssuer {
    /// Creates one host-scoped invitation with explicit routing authority.
    pub(crate) fn create_invitation(
        &self,
        profile_name: &str,
        role: RemoteRoleCeiling,
        authority: RemoteHostRoutingAuthority,
        ttl_seconds: u64,
        now_unix_seconds: u64,
    ) -> Result<Value> {
        if profile_name.trim().is_empty() || profile_name.chars().any(char::is_control) {
            return Err(MezError::invalid_args(
                "host Iroh profile name must be non-empty printable text",
            ));
        }
        let server_addr = foreign_machine_invitation_addr(self.endpoint.addr(), &self.policy)?;
        let invitation = self.trust.create_host_invitation(
            &self.endpoint_id,
            role,
            authority,
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
            "routing": invitation.host_routing,
            "expires_at_unix_seconds": invitation.expires_at_unix_seconds,
        }))
    }

    pub(crate) fn list_clients(&self) -> Result<Vec<crate::security::remote::RemoteTrustRecord>> {
        self.trust.list_records()
    }

    pub(crate) fn rename_client(
        &self,
        record_id: &str,
        label: &str,
    ) -> Result<crate::security::remote::RemoteTrustRecord> {
        self.trust.rename_record(record_id, label)
    }

    pub(crate) fn revoke_client(
        &self,
        record_id: &str,
        reason: Option<&str>,
        now_unix_seconds: u64,
    ) -> Result<crate::security::remote::RemoteTrustRecord> {
        self.trust
            .revoke_record(record_id, reason, now_unix_seconds)
    }

    pub(crate) fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }
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

    /// Returns the local-administration view for invitations and trust records.
    pub(crate) fn invitation_issuer(&self) -> HostIrohInvitationIssuer {
        HostIrohInvitationIssuer {
            endpoint: self.endpoint.endpoint().clone(),
            endpoint_id: self.endpoint_id().to_string(),
            policy: self.endpoint.policy().clone(),
            trust: self.trust.clone(),
        }
    }

    /// Creates a host-scoped pairing invitation without provisioning a session.
    pub(crate) fn create_invitation(
        &self,
        profile_name: &str,
        role: RemoteRoleCeiling,
        ttl_seconds: u64,
        now_unix_seconds: u64,
    ) -> Result<Value> {
        self.invitation_issuer().create_invitation(
            profile_name,
            role,
            RemoteHostRoutingAuthority::default(),
            ttl_seconds,
            now_unix_seconds,
        )
    }

    /// Serves bounded host-only initialization until cancellation.
    pub(crate) async fn serve<C>(&self, cancellation: C) -> Result<u64>
    where
        C: Future<Output = ()>,
    {
        self.serve_inner(None, cancellation).await
    }

    /// Serves host-only setup and authenticated session routing through one endpoint.
    pub(crate) async fn serve_routed<C>(
        &self,
        router: HostSessionRouter,
        cancellation: C,
    ) -> Result<u64>
    where
        C: Future<Output = ()>,
    {
        self.serve_inner(Some(router), cancellation).await
    }

    async fn serve_inner<C>(
        &self,
        router: Option<HostSessionRouter>,
        cancellation: C,
    ) -> Result<u64>
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
                    let router = router.clone();
                    tasks.spawn(async move {
                        serve_host_only_connection(
                            incoming,
                            policy,
                            trust,
                            server_endpoint_id,
                            router,
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
    router: Option<HostSessionRouter>,
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
    if let Some(router) = router.as_ref()
        && request_session_intent(&request).as_deref() != Some("host_only")
    {
        return serve_routed_initialize(
            request,
            &trust,
            &server_endpoint_id,
            &client_endpoint_id,
            router.clone(),
            connection,
            bridge,
            compression,
            &policy,
        )
        .await;
    }
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
            principal: None,
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
    if let (Some(router), Some(principal)) = (router, initialized.principal.as_ref())
        && let Some(request) =
            read_optional_control_frame(bridge.stream_mut(), policy.idle_timeout).await?
    {
        serve_host_only_request(
            bridge.stream_mut(),
            &request,
            &router,
            principal,
            policy.idle_timeout,
        )
        .await?;
    }
    bridge.shutdown(policy.setup_timeout).await?;
    connection.close(iroh::endpoint::VarInt::from_u32(0), b"host-only complete");
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "host trust, routing, transport ownership, compression, and bounded lifecycle are independent handoff inputs"
)]
async fn serve_routed_initialize(
    request_body: String,
    trust: &RemoteTrustStore,
    server_endpoint_id: &str,
    client_endpoint_id: &str,
    router: HostSessionRouter,
    connection: iroh::endpoint::Connection,
    mut bridge: IrohCompressionBridge,
    compression: IrohCompressionPolicy,
    policy: &RuntimeIrohTransportPolicy,
) -> Result<()> {
    let response_id = request_id(&request_body);
    let mut initialization_sent = false;
    let result = serve_routed_initialize_inner(
        request_body,
        trust,
        server_endpoint_id,
        client_endpoint_id,
        router,
        &connection,
        &mut bridge,
        compression,
        policy,
        &mut initialization_sent,
    )
    .await;
    let error_write_result: Result<()> = if let Err(error) = result.as_ref()
        && !initialization_sent
    {
        let response = host_json_rpc_error(response_id, error);
        tokio::time::timeout(
            policy.idle_timeout,
            bridge
                .stream_mut()
                .write_all(&encode_control_body(&response)),
        )
        .await
        .map_err(|_| MezError::invalid_state("host routed error response timed out"))??;
        tokio::time::timeout(policy.idle_timeout, bridge.stream_mut().flush())
            .await
            .map_err(|_| MezError::invalid_state("host routed error flush timed out"))??;
        Ok(())
    } else {
        Ok(())
    };
    let bridge_result = bridge.shutdown(policy.setup_timeout).await;
    connection.close(
        iroh::endpoint::VarInt::from_u32(u32::from(result.is_err())),
        b"routed control complete",
    );
    result?;
    error_write_result?;
    bridge_result
}

#[allow(
    clippy::too_many_arguments,
    reason = "host trust, routing, borrowed transport state, compression, and initialization state are independent handoff inputs"
)]
async fn serve_routed_initialize_inner(
    request_body: String,
    trust: &RemoteTrustStore,
    server_endpoint_id: &str,
    client_endpoint_id: &str,
    router: HostSessionRouter,
    connection: &iroh::endpoint::Connection,
    bridge: &mut IrohCompressionBridge,
    compression: IrohCompressionPolicy,
    policy: &RuntimeIrohTransportPolicy,
    initialization_sent: &mut bool,
) -> Result<()> {
    let request: Value = serde_json::from_str(&request_body).map_err(|error| {
        MezError::invalid_args(format!("invalid host initialize JSON: {error}"))
    })?;
    if request.get("method").and_then(Value::as_str) != Some("control/initialize") {
        return Err(MezError::forbidden(
            "host Iroh requires control/initialize as the first request",
        ));
    }
    let request_id = request.get("id").cloned().unwrap_or(Value::Null);
    let params = request
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| MezError::invalid_args("host initialize requires params"))?;
    let init = initialize_params_from_json(&Value::Object(params.clone()).to_string())?;
    let intent = init
        .session_intent
        .ok_or_else(|| MezError::invalid_args("host routing requires session_intent"))?;
    if intent == SessionIntent::HostOnly {
        return Err(MezError::invalid_state(
            "host-only initialization reached the session router",
        ));
    }
    let authentication = init
        .authentication
        .as_ref()
        .ok_or_else(|| MezError::forbidden("host Iroh requires durable device proof"))?;
    if authentication.mechanism != AuthenticationMechanism::Extension("iroh_device".to_string()) {
        return Err(MezError::forbidden(
            "pair the host before requesting a remote session",
        ));
    }
    let token = SecretString::from(
        authentication
            .token
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| MezError::forbidden("host Iroh device credential is missing"))?
            .to_string(),
    );
    let principal = trust.resolve_principal(
        server_endpoint_id,
        client_endpoint_id,
        &token,
        init.requested_role,
        current_unix_seconds()?,
    )?;
    let size = init
        .client
        .as_ref()
        .and_then(|client| client.terminal.as_ref())
        .map(|terminal| mez_mux::layout::Size::new(terminal.columns, terminal.rows))
        .transpose()?
        .unwrap_or(mez_mux::layout::Size::new(80, 24)?);
    let session_name = init
        .client
        .as_ref()
        .and_then(|client| client.metadata_json.as_deref())
        .and_then(|metadata| serde_json::from_str::<Value>(metadata).ok())
        .and_then(|metadata| {
            metadata
                .get("session_name")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let binding = match intent {
        SessionIntent::Create => {
            router
                .create_remote(
                    &principal,
                    crate::host::router::RemoteSessionCreateRequest {
                        name: session_name,
                        idempotency_key: init.idempotency_key.clone().ok_or_else(|| {
                            MezError::invalid_args("create intent requires idempotency_key")
                        })?,
                        size,
                    },
                )
                .await?
        }
        SessionIntent::Attach => {
            router.resolve_remote(&principal, init.session_target_json.as_deref())?
        }
        SessionIntent::Default => router.resolve_remote(&principal, None)?,
        SessionIntent::HostOnly => unreachable!("host-only intent returned above"),
    };

    let mut actor_params = params;
    actor_params.insert("requested_version".to_string(), Value::from(2));
    actor_params.remove("session_intent");
    actor_params.remove("idempotency_key");
    actor_params.remove("authentication");
    actor_params.insert(
        "session_target".to_string(),
        json!({"session_id": binding.lease.session_id}),
    );
    let actor_request = json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "control/initialize",
        "params": actor_params,
    });
    let peer = AuthenticatedPeer::iroh_endpoint(client_endpoint_id);
    let mut connection_state = ControlConnectionState::new(false, false);
    connection_state.bind_authenticated_peer(peer.clone())?;
    connection_state.bind_remote_principal(principal.clone())?;
    let initialized = binding
        .runtime
        .actor()
        .handle_control_input_for_connection(
            encode_control_body(&actor_request.to_string()),
            HOST_CONTROL_MAX_CONTENT_LENGTH,
            connection_state,
        )
        .await?;
    let (actor_body, consumed) =
        decode_control_frame(&initialized.output, HOST_CONTROL_MAX_CONTENT_LENGTH)?;
    if consumed != initialized.output.len() {
        return Err(MezError::invalid_state(
            "routed actor initialization returned multiple responses",
        ));
    }
    let mut response: Value = serde_json::from_str(&actor_body).map_err(|_| {
        MezError::invalid_state("routed actor initialization returned invalid JSON")
    })?;
    if let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) {
        result.insert("selected_version".to_string(), Value::from(3));
        result.insert(
            "host".to_string(),
            json!({"endpoint_id": server_endpoint_id}),
        );
        result.insert(
            "lease".to_string(),
            json!({
                "lease_id": binding.lease.lease_id,
                "session_id": binding.lease.session_id,
                "name": binding.lease.name,
                "state": "active",
            }),
        );
        result.insert(
            "principal_id".to_string(),
            Value::String(principal.trust_record_id.clone()),
        );
        if let Some(server) = result.get_mut("server").and_then(Value::as_object_mut) {
            server.insert("protocol_versions".to_string(), json!([3]));
        }
    }
    tokio::time::timeout(
        policy.idle_timeout,
        bridge
            .stream_mut()
            .write_all(&encode_control_body(&response.to_string())),
    )
    .await
    .map_err(|_| MezError::invalid_state("host routed initialize response timed out"))??;
    tokio::time::timeout(policy.idle_timeout, bridge.stream_mut().flush())
        .await
        .map_err(|_| MezError::invalid_state("host routed initialize flush timed out"))??;
    *initialization_sent = true;
    if response.get("error").is_some() {
        return Ok(());
    }

    let mut connection_state = initialized.connection;
    let (event_stop_tx, event_stop_rx) = tokio::sync::watch::channel(false);
    let mut event_task = connection_state
        .take_event_stream_start()
        .map(|(client_id, version)| {
            tokio::spawn(serve_host_routed_iroh_event_stream(
                (*connection).clone(),
                binding.runtime.actor().clone(),
                client_id,
                version,
                compression,
                policy.setup_timeout,
                policy.idle_timeout,
                event_stop_rx,
            ))
        });
    let control_config =
        AsyncRuntimeControlConnectionConfig::new(HOST_CONTROL_MAX_CONTENT_LENGTH, 0)?;
    let control_result =
        serve_authenticated_async_runtime_control_connection_loop_with_snapshots_and_post_flush(
            bridge.stream_mut(),
            peer,
            binding.runtime.actor(),
            &mut connection_state,
            control_config,
            None,
            |_, state| {
                matches!(
                    state,
                    RuntimeLifecycleState::Stopping
                        | RuntimeLifecycleState::Killed
                        | RuntimeLifecycleState::Failed
                )
            },
            |_| Ok(()),
        )
        .await;
    let _ = event_stop_tx.send(true);
    if let Some(mut task) = event_task.take()
        && tokio::time::timeout(policy.setup_timeout, &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }
    control_result?;
    Ok(())
}

fn request_session_intent(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|request| {
            request
                .pointer("/params/session_intent")?
                .as_str()
                .map(str::to_string)
        })
}

struct HostOnlyInitializeResponse {
    body: String,
    redemption: Option<crate::security::remote::RemotePairingRedemption>,
    principal: Option<RemotePrincipal>,
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
        principal: Some(principal),
    })
}

async fn read_optional_control_frame<S>(
    stream: &mut S,
    timeout: std::time::Duration,
) -> Result<Option<String>>
where
    S: tokio::io::AsyncRead + Unpin,
{
    tokio::time::timeout(timeout, async {
        let mut input = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = stream.read(&mut buffer).await?;
            if read == 0 {
                if input.is_empty() {
                    return Ok(None);
                }
                return Err(MezError::invalid_state(
                    "host Iroh stream closed during a follow-up request",
                ));
            }
            input.extend_from_slice(&buffer[..read]);
            if input.len() > HOST_CONTROL_MAX_CONTENT_LENGTH + 8192 {
                return Err(MezError::invalid_args(
                    "host Iroh follow-up frame exceeds limit",
                ));
            }
            if let Ok((body, consumed)) =
                decode_control_frame(&input, HOST_CONTROL_MAX_CONTENT_LENGTH)
            {
                if consumed != input.len() {
                    return Err(MezError::invalid_args(
                        "host Iroh accepts one host-only follow-up frame",
                    ));
                }
                return Ok(Some(body));
            }
        }
    })
    .await
    .map_err(|_| MezError::invalid_state("host Iroh follow-up read timed out"))?
}

async fn serve_host_only_request<S>(
    stream: &mut S,
    body: &str,
    router: &HostSessionRouter,
    principal: &RemotePrincipal,
    timeout: std::time::Duration,
) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let request: Value = serde_json::from_str(body)
        .map_err(|error| MezError::invalid_args(format!("invalid host request JSON: {error}")))?;
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result: Result<Value> = match request.get("method").and_then(Value::as_str) {
        Some("host/session/list") => router.list_remote(principal).map(|leases| {
            json!({
                "sessions": leases.iter().map(remote_lease_summary).collect::<Vec<_>>()
            })
        }),
        Some(_) => Err(MezError::forbidden(
            "host-only remote control permits only advertised host methods",
        )),
        None => Err(MezError::invalid_args("host request method is required")),
    };
    let response = match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error) => serde_json::from_str::<Value>(&host_json_rpc_error(id, &error))
            .map_err(|_| MezError::invalid_state("failed to encode host error response"))?,
    };
    tokio::time::timeout(
        timeout,
        stream.write_all(&encode_control_body(&response.to_string())),
    )
    .await
    .map_err(|_| MezError::invalid_state("host Iroh follow-up write timed out"))??;
    tokio::time::timeout(timeout, stream.flush())
        .await
        .map_err(|_| MezError::invalid_state("host Iroh follow-up flush timed out"))??;
    Ok(())
}

fn remote_lease_summary(lease: &RemoteSessionLease) -> Value {
    json!({
        "lease_id": lease.lease_id,
        "session_id": lease.session_id,
        "name": lease.name,
        "state": remote_lease_state_name(lease.state),
        "created_at_unix_seconds": lease.created_at_unix_seconds,
    })
}

fn remote_lease_state_name(state: RemoteSessionLeaseState) -> &'static str {
    match state {
        RemoteSessionLeaseState::Pending => "pending",
        RemoteSessionLeaseState::Active => "active",
        RemoteSessionLeaseState::Recoverable => "recoverable",
        RemoteSessionLeaseState::Released => "released",
        RemoteSessionLeaseState::Revoked => "revoked",
        RemoteSessionLeaseState::Failed => "failed",
    }
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
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    use crate::host::router::HostSessionRouterConfig;
    use crate::host::shell::{ResolvedShell, ShellSource};
    use crate::security::remote::RemoteSessionAttachScope;

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

    /// A capability-bearing host invitation pairs without provisioning, then
    /// routes create replay, conflict, explicit attach, default selection, and
    /// principal-filtered listing through one persistent front door. A second
    /// paired device without routing authority receives structured denials.
    #[tokio::test(flavor = "current_thread")]
    async fn routed_host_end_to_end_enforces_intent_idempotency_and_authority() {
        let root = test_root("routed");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            identity: RuntimeIrohIdentityPolicy::Host,
            compression_codecs: vec![RuntimeIrohCompressionCodec::None],
            setup_timeout: std::time::Duration::from_secs(3),
            idle_timeout: std::time::Duration::from_secs(3),
            ..RuntimeIrohTransportPolicy::default()
        };
        let host = HostIrohRuntime::bind(&root, policy.clone())
            .await
            .unwrap()
            .unwrap();
        let router = HostSessionRouter::new(HostSessionRouterConfig {
            runtime_root: root.join("runtime"),
            owner_uid: crate::runtime::current_effective_uid(),
            config_root: root.join("config"),
            config_layers: vec![ConfigLayer {
                name: "host-iroh-test".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[agents]\nshell_mode = \"pane\"\n[permissions]\nsandbox = \"policy-only\"\n"
                    .to_string(),
            }],
            shell: ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
            max_sessions: 8,
            max_live_sessions: 8,
        });
        let invitation = host
            .trust
            .create_host_invitation(
                host.endpoint_id(),
                RemoteRoleCeiling::Observer,
                RemoteHostRoutingAuthority {
                    session_create: true,
                    session_list: true,
                    session_attach_scope: RemoteSessionAttachScope::Own,
                    max_active_leases: 2,
                    max_live_sessions: 2,
                    lease_lifetime_ceiling_seconds: None,
                },
                600,
                current_unix_seconds().unwrap(),
            )
            .unwrap();
        let denied_invitation = host
            .trust
            .create_host_invitation(
                host.endpoint_id(),
                RemoteRoleCeiling::Observer,
                RemoteHostRoutingAuthority::default(),
                600,
                current_unix_seconds().unwrap(),
            )
            .unwrap();
        let server_addr = host.endpoint_addr().unwrap();
        let client =
            crate::runtime::bind_runtime_iroh_client_endpoint(&policy, iroh::SecretKey::generate())
                .await
                .unwrap();
        let denied_client =
            crate::runtime::bind_runtime_iroh_client_endpoint(&policy, iroh::SecretKey::generate())
                .await
                .unwrap();
        let stop = std::sync::Arc::new(tokio::sync::Notify::new());
        let server_stop = stop.clone();
        let server_router = router.clone();

        let server = host.serve_routed(server_router, async move { server_stop.notified().await });
        let client_work = async {
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
            assert!(router.snapshots().await.unwrap().is_empty());
            let credential = paired["result"]["device_credential"]
                .as_str()
                .unwrap()
                .to_string();

            let created = exchange_test_routed_initialize(
                &client,
                &server_addr,
                &credential,
                "create",
                Some("create-one"),
                None,
                Some("owned"),
            )
            .await;
            assert_eq!(created["result"]["selected_version"], 3, "{created}");
            assert_eq!(created["result"]["lease"]["state"], "active", "{created}");
            let lease_id = created["result"]["lease"]["lease_id"]
                .as_str()
                .unwrap()
                .to_string();
            let session_id = created["result"]["lease"]["session_id"]
                .as_str()
                .unwrap()
                .to_string();

            let replay = exchange_test_routed_initialize(
                &client,
                &server_addr,
                &credential,
                "create",
                Some("create-one"),
                None,
                Some("owned"),
            )
            .await;
            assert_eq!(replay["result"]["lease"]["lease_id"], lease_id, "{replay}");
            assert_eq!(router.snapshots().await.unwrap().len(), 1);

            let conflict = exchange_test_routed_initialize(
                &client,
                &server_addr,
                &credential,
                "create",
                Some("create-one"),
                None,
                Some("changed"),
            )
            .await;
            assert_eq!(
                conflict["error"]["data"]["mezzanine_code"], "conflict",
                "{conflict}"
            );

            let attached = exchange_test_routed_initialize(
                &client,
                &server_addr,
                &credential,
                "attach",
                None,
                Some(json!({"session_id": session_id})),
                None,
            )
            .await;
            assert_eq!(
                attached["result"]["lease"]["lease_id"], lease_id,
                "{attached}"
            );
            let selected_default = exchange_test_routed_initialize(
                &client,
                &server_addr,
                &credential,
                "default",
                None,
                None,
                None,
            )
            .await;
            assert_eq!(
                selected_default["result"]["lease"]["lease_id"], lease_id,
                "{selected_default}"
            );
            assert_eq!(router.snapshots().await.unwrap().len(), 1);

            let listed = exchange_test_host_list(&client, &server_addr, &credential).await;
            assert_eq!(
                listed["result"]["sessions"].as_array().unwrap().len(),
                1,
                "{listed}"
            );
            assert_eq!(
                listed["result"]["sessions"][0]["lease_id"], lease_id,
                "{listed}"
            );

            let denied_pair = exchange_test_host_initialize(
                &denied_client,
                &server_addr,
                "extension:iroh_invitation",
                denied_invitation.token.expose_secret(),
                "host_only",
            )
            .await;
            let denied_credential = denied_pair["result"]["device_credential"].as_str().unwrap();
            let denied_create = exchange_test_routed_initialize(
                &denied_client,
                &server_addr,
                denied_credential,
                "create",
                Some("denied-create"),
                None,
                Some("denied"),
            )
            .await;
            assert_eq!(
                denied_create["error"]["data"]["mezzanine_code"], "forbidden",
                "{denied_create}"
            );
            let denied_list =
                exchange_test_host_list(&denied_client, &server_addr, denied_credential).await;
            assert_eq!(
                denied_list["error"]["data"]["mezzanine_code"], "forbidden",
                "{denied_list}"
            );
            assert_eq!(router.snapshots().await.unwrap().len(), 1);
            stop.notify_one();
        };

        let (served, ()) = tokio::join!(server, client_work);
        assert_eq!(served.unwrap(), 10);
        router
            .shutdown_all(true, std::time::Duration::from_secs(2))
            .await
            .unwrap();
        client.close().await;
        denied_client.close().await;
        drop(host);
        let _ = fs::remove_dir_all(root);
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

    async fn exchange_test_routed_initialize(
        client: &iroh::Endpoint,
        server_addr: &iroh::EndpointAddr,
        credential: &str,
        intent: &str,
        idempotency_key: Option<&str>,
        target: Option<Value>,
        session_name: Option<&str>,
    ) -> Value {
        let mut client_metadata = json!({
            "name": "test-client",
            "interactive": false,
            "terminal": {"columns": 80, "rows": 24, "term": "xterm-256color"}
        });
        if let Some(session_name) = session_name {
            client_metadata["metadata"] = json!({"session_name": session_name});
        }
        let mut params = json!({
            "client_name": "test-client",
            "requested_version": 3,
            "requested_role": "observer",
            "session_intent": intent,
            "client": client_metadata,
            "authentication": {
                "mechanism": "extension:iroh_device",
                "token": credential
            }
        });
        if let Some(idempotency_key) = idempotency_key {
            params["idempotency_key"] = Value::String(idempotency_key.to_string());
        }
        if let Some(target) = target {
            params["session_target"] = target;
        }
        exchange_test_initialize_params(client, server_addr, params).await
    }

    async fn exchange_test_initialize_params(
        client: &iroh::Endpoint,
        server_addr: &iroh::EndpointAddr,
        params: Value,
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
            "id": "test-routed-init",
            "method": "control/initialize",
            "params": params,
        })
        .to_string();
        bridge
            .stream_mut()
            .write_all(&encode_control_body(&request))
            .await
            .unwrap();
        bridge.stream_mut().shutdown().await.unwrap();
        let response =
            read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(3))
                .await
                .unwrap();
        let _ = bridge.shutdown(std::time::Duration::from_secs(3)).await;
        connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
        serde_json::from_str(&response).unwrap()
    }

    async fn exchange_test_host_list(
        client: &iroh::Endpoint,
        server_addr: &iroh::EndpointAddr,
        credential: &str,
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
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": "test-list-init",
            "method": "control/initialize",
            "params": {
                "client_name": "test-client",
                "requested_version": 3,
                "requested_role": "observer",
                "session_intent": "host_only",
                "authentication": {
                    "mechanism": "extension:iroh_device",
                    "token": credential
                }
            }
        })
        .to_string();
        bridge
            .stream_mut()
            .write_all(&encode_control_body(&initialize))
            .await
            .unwrap();
        bridge.stream_mut().flush().await.unwrap();
        let initialized =
            read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(3))
                .await
                .unwrap();
        let initialized: Value = serde_json::from_str(&initialized).unwrap();
        assert!(initialized.get("result").is_some(), "{initialized}");
        let list = json!({
            "jsonrpc": "2.0",
            "id": "test-list",
            "method": "host/session/list",
            "params": {}
        })
        .to_string();
        bridge
            .stream_mut()
            .write_all(&encode_control_body(&list))
            .await
            .unwrap();
        bridge.stream_mut().shutdown().await.unwrap();
        let response =
            read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(3))
                .await
                .unwrap();
        let _ = bridge.shutdown(std::time::Duration::from_secs(3)).await;
        connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
        serde_json::from_str(&response).unwrap()
    }
}
