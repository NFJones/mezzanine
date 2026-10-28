//! Persistent-host ownership for Iroh identity, trust, and host-only setup.
//!
//! This component owns one endpoint identity and one trust database for the
//! complete host rather than for an individual session. Its listener accepts
//! protocol-v3 initialization before explicit authenticated session routing.
//! Pairing and host-only health cannot allocate a session. The listener drains
//! all connection ownership before returning a typed failure scope; isolated
//! endpoint loss does not grant local shutdown or replacement identity authority.

use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinSet;
use tokio_util::bytes::BytesMut;
use tokio_util::codec::Decoder;

use crate::control::{
    AuthenticatedPeer, AuthenticationMechanism, CONTROL_CONTENT_TYPE, ControlConnectionState,
    SessionIntent, decode_control_frame, encode_control_body, initialize_params_from_json,
};
use crate::error::{MezError, MezErrorKind, Result};
use crate::host::administration::{HostAuditLog, administration_request_fingerprint};
use crate::host::async_runtime::{
    AsyncRuntimeControlConnectionConfig,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots_hooks_and_cancellation,
};
use crate::host::router::HostSessionRouter;
use crate::protocol::framing::ProtocolFrameCodec;
use crate::runtime::{
    IrohCompressionBridge, IrohCompressionMetrics, IrohCompressionPolicy,
    RuntimeIrohCompressionCodec, RuntimeIrohDiagnostics, RuntimeIrohEndpoint,
    RuntimeIrohIdentityPolicy, RuntimeIrohTransportPolicy, RuntimeLifecycleState,
    bind_runtime_iroh_endpoint, serve_host_routed_iroh_event_stream,
};
use crate::security::audit::{AuditActor, AuditRecord};
use crate::security::remote::{
    RemoteEndpointIdentity, RemoteHostRoutingAuthority, RemotePrincipal, RemoteRoleCeiling,
    RemoteTrustStore,
};
use crate::storage::lease::{RemoteSessionLease, RemoteSessionLeaseState};

mod lifecycle;
pub(crate) use lifecycle::HostIrohServeError;
use lifecycle::{HostIrohShutdownReport, drain_host_iroh_connection_tasks};

const HOST_CONTROL_MAX_CONTENT_LENGTH: usize = 1024 * 1024;

#[cfg(test)]
pub(super) mod initialize_diagnostics;

/// Emits the final lifecycle record for one established remote client connection.
struct RemoteClientConnectionLog {
    endpoint_id: String,
    route: String,
}

/// Marks the complete interval in which the persistent-host listener is live.
struct HostIrohListenerDiagnosticsGuard {
    diagnostics: RuntimeIrohDiagnostics,
}

impl HostIrohListenerDiagnosticsGuard {
    fn new(diagnostics: RuntimeIrohDiagnostics) -> Self {
        diagnostics.listener_started();
        Self { diagnostics }
    }
}

impl Drop for HostIrohListenerDiagnosticsGuard {
    fn drop(&mut self) {
        self.diagnostics.listener_stopped();
    }
}

/// Records every pre-session attempt as either one setup success or one setup
/// failure, including errors returned through `?` before initialization.
struct HostIrohSetupDiagnosticsGuard {
    diagnostics: RuntimeIrohDiagnostics,
    started: std::time::Instant,
    established: bool,
}

impl HostIrohSetupDiagnosticsGuard {
    fn new(diagnostics: RuntimeIrohDiagnostics, started: std::time::Instant) -> Self {
        Self {
            diagnostics,
            started,
            established: false,
        }
    }

    fn connection_started(
        &mut self,
        connection: &iroh::endpoint::Connection,
    ) -> crate::runtime::RuntimeIrohConnectionGuard {
        self.established = true;
        self.diagnostics
            .connection_started(connection, self.started.elapsed())
    }
}

impl Drop for HostIrohSetupDiagnosticsGuard {
    fn drop(&mut self) {
        if !self.established {
            self.diagnostics.record_rejected(self.started.elapsed());
        }
    }
}

impl Drop for RemoteClientConnectionLog {
    fn drop(&mut self) {
        eprintln!(
            "mez host: remote client disconnected: endpoint {}, route {}",
            self.endpoint_id, self.route
        );
    }
}

/// Renders the privacy-safe network route category observed for a remote client.
fn remote_client_route(remote_addr: iroh::endpoint::IncomingAddr) -> &'static str {
    match remote_addr {
        iroh::endpoint::IncomingAddr::Ip(_) => "direct",
        iroh::endpoint::IncomingAddr::Relay { .. } => "relay",
        iroh::endpoint::IncomingAddr::Custom(_) => "custom",
        _ => "unknown",
    }
}

/// Stable host endpoint, trust store, and bounded pre-session listener.
#[derive(Debug)]
pub(crate) struct HostIrohRuntime {
    identity: RemoteEndpointIdentity,
    trust: RemoteTrustStore,
    endpoint: RuntimeIrohEndpoint,
    audit_log: Option<HostAuditLog>,
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
    /// Reports transport liveness independently of enabled configuration.
    pub(crate) fn available(&self) -> bool {
        !self.endpoint.is_closed()
    }

    /// Refuses secret-bearing invitation responses after transport closure.
    fn require_available(&self) -> Result<()> {
        if self.available() {
            Ok(())
        } else {
            Err(MezError::invalid_state(
                "host Iroh transport is unavailable",
            ))
        }
    }

    /// Returns the configured default lifetime for host pairing invitations.
    pub(crate) fn invitation_ttl_seconds(&self) -> u64 {
        self.policy.invitation_ttl.as_secs()
    }

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
        self.require_available()?;
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

    /// Creates or resumes the exact invitation bound to one durable host
    /// administration request without persisting its bearer token.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_idempotent_invitation(
        &self,
        profile_name: &str,
        role: RemoteRoleCeiling,
        authority: RemoteHostRoutingAuthority,
        ttl_seconds: u64,
        now_unix_seconds: u64,
        idempotency_key: &str,
        request_fingerprint: &str,
    ) -> Result<Value> {
        if profile_name.trim().is_empty() || profile_name.chars().any(char::is_control) {
            return Err(MezError::invalid_args(
                "host Iroh profile name must be non-empty printable text",
            ));
        }
        let (invitation_id, token) =
            self.invitation_replay_material(idempotency_key, request_fingerprint);
        self.require_available()?;
        let server_addr = foreign_machine_invitation_addr(self.endpoint.addr(), &self.policy)?;
        let invitation = self.trust.create_host_invitation_idempotent(
            &self.endpoint_id,
            role,
            authority,
            ttl_seconds,
            now_unix_seconds,
            invitation_id,
            token,
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

    /// Restores the secret-bearing invitation response from secret-free
    /// durable replay metadata and the retained host endpoint identity.
    pub(crate) fn restore_invitation_response(
        &self,
        mut response: Value,
        idempotency_key: &str,
        params: &serde_json::Map<String, Value>,
    ) -> Result<Value> {
        self.require_available()?;
        let request_fingerprint = administration_request_fingerprint("remote/invite", params)?;
        let (invitation_id, token) =
            self.invitation_replay_material(idempotency_key, &request_fingerprint);
        let object = response.as_object_mut().ok_or_else(|| {
            MezError::invalid_state("persisted invitation replay response must be an object")
        })?;
        if object.get("invitation_id").and_then(Value::as_str) != Some(invitation_id.as_str()) {
            return Err(MezError::invalid_state(
                "persisted invitation replay identity does not match the administration request",
            ));
        }
        object.insert(
            "token".to_string(),
            Value::String(token.expose_secret().to_string()),
        );
        Ok(response)
    }

    fn invitation_replay_material(
        &self,
        idempotency_key: &str,
        request_fingerprint: &str,
    ) -> (String, SecretString) {
        let secret_key = self.endpoint.secret_key().to_bytes();
        let mut invitation_digest = Sha256::new();
        invitation_digest.update(b"mezzanine-host-invitation-id-v1\0");
        invitation_digest.update(secret_key);
        invitation_digest.update(idempotency_key.as_bytes());
        invitation_digest.update(b"\0");
        invitation_digest.update(request_fingerprint.as_bytes());
        let invitation_digest = invitation_digest.finalize();
        let invitation_id = format!(
            "invite-{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&invitation_digest[..16])
        );

        let mut token_digest = Sha256::new();
        token_digest.update(b"mezzanine-host-invitation-token-v1\0");
        token_digest.update(secret_key);
        token_digest.update(idempotency_key.as_bytes());
        token_digest.update(b"\0");
        token_digest.update(request_fingerprint.as_bytes());
        let token = SecretString::from(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_digest.finalize()),
        );
        (invitation_id, token)
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
            audit_log: None,
        }))
    }

    /// Shares the serialized host audit writer with invitation redemption.
    pub(crate) fn set_audit_log(&mut self, audit_log: HostAuditLog) {
        self.audit_log = Some(audit_log);
    }

    /// Stable public endpoint identity retained across host restarts.
    pub(crate) fn endpoint_id(&self) -> &str {
        self.identity.endpoint_id()
    }

    /// Latest dialable address, unavailable after the endpoint has closed.
    pub(crate) fn endpoint_addr(&self) -> Option<iroh::EndpointAddr> {
        (!self.endpoint.endpoint().is_closed())
            .then(|| self.endpoint.endpoint_addr())
            .flatten()
    }

    /// Exposes the fixture-owned endpoint for actual closure and drain observations.
    /// This grants no replacement identity or process-retirement authority.
    #[cfg(test)]
    pub(crate) fn endpoint_for_test(&self) -> &RuntimeIrohEndpoint {
        &self.endpoint
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

    /// Test-only TTL fixture issuer using an already bound ephemeral socket.
    /// Qualifies an actual non-loopback advertised port against retained sockets
    /// instead of probing/releasing a port. Only the fixture issuer's policy is
    /// changed; production endpoint configuration and route guards stay intact.
    #[cfg(test)]
    pub(crate) fn ephemeral_invitation_fixture_issuer(&self) -> Result<HostIrohInvitationIssuer> {
        let mut issuer = self.invitation_issuer();
        if issuer.policy.bind_port != 0 {
            return Err(MezError::invalid_args(
                "invitation fixture requires an ephemeral endpoint",
            ));
        }
        let sockets = self.endpoint.endpoint().bound_sockets();
        let addr = self.endpoint.endpoint().addr();
        let port = addr
            .ip_addrs()
            .find(|addr| {
                addr.port() != 0
                    && !addr.ip().is_loopback()
                    && !addr.ip().is_unspecified()
                    && sockets.iter().any(|socket| socket.port() == addr.port())
            })
            .map(|addr| addr.port())
            .ok_or_else(|| {
                MezError::invalid_state(
                    "invitation fixture has no retained non-loopback socket route",
                )
            })?;
        issuer.policy.bind_port = port;
        Ok(issuer)
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
        self.serve_inner(None, cancellation)
            .await
            .map_err(HostIrohServeError::into_error)
    }

    /// Serves host-only setup and authenticated session routing through one endpoint.
    pub(crate) async fn serve_routed<C>(
        &self,
        router: HostSessionRouter,
        cancellation: C,
    ) -> std::result::Result<u64, HostIrohServeError>
    where
        C: Future<Output = ()>,
    {
        self.serve_inner(Some(router), cancellation).await
    }

    /// Accepts bounded remote work, then classifies failure only after owned
    /// endpoint closure and all connection-task joins have been observed.
    async fn serve_inner<C>(
        &self,
        router: Option<HostSessionRouter>,
        cancellation: C,
    ) -> std::result::Result<u64, HostIrohServeError>
    where
        C: Future<Output = ()>,
    {
        let endpoint = self.endpoint.endpoint().clone();
        let policy = self.endpoint.policy().clone();
        let trust = self.trust.clone();
        let server_endpoint_id = self.endpoint_id().to_string();
        let diagnostics = self.endpoint.diagnostics();
        let audit_log = self.audit_log.clone();
        let mut tasks = JoinSet::new();
        let mut accepted = 0u64;
        let mut endpoint_closed_unexpectedly = false;
        let mut shutdown_report = HostIrohShutdownReport::default();
        let mut remote_capacity_saturated = false;
        let _listener_diagnostics_guard =
            HostIrohListenerDiagnosticsGuard::new(diagnostics.clone());
        eprintln!("mez host: listening for remote clients on Iroh endpoint {server_endpoint_id}");
        tokio::pin!(cancellation);

        loop {
            tokio::select! {
                () = &mut cancellation => break,
                incoming = endpoint.accept(), if tasks.len() < policy.max_connections => {
                    let Some(incoming) = incoming else {
                        endpoint_closed_unexpectedly = !self.endpoint.is_intentionally_closed();
                        break;
                    };
                    let remote_route = remote_client_route(incoming.remote_addr());
                    let max_connections = policy.max_connections;
                    let policy = policy.clone();
                    let trust = trust.clone();
                    let server_endpoint_id = server_endpoint_id.clone();
                    let router = router.clone();
                    let diagnostics = diagnostics.clone();
                    let result_diagnostics = diagnostics.clone();
                    let audit_log = audit_log.clone();
                    let authenticated_endpoint_id = std::sync::Arc::new(std::sync::OnceLock::new());
                    tasks.spawn(async move {
                        let result = serve_host_only_connection(
                            incoming,
                            policy,
                            trust,
                            server_endpoint_id,
                            router,
                            diagnostics,
                            audit_log,
                            remote_route,
                            &authenticated_endpoint_id,
                        ).await;
                        result_diagnostics.record_result(&result);
                        match result {
                            Ok(()) => {}
                            Err(error) => match authenticated_endpoint_id.get() {
                                Some(endpoint_id) => eprintln!(
                                    "mez host: remote client connection failed: endpoint {endpoint_id}, route {remote_route}, class {}, error {error}",
                                    host_error_name(error.kind())
                                ),
                                None => eprintln!(
                                    "mez host: remote client connection failed before authentication: route {remote_route}, class {}, error {error}",
                                    host_error_name(error.kind())
                                ),
                            },
                        }
                    });
                    accepted = accepted.saturating_add(1);
                    if tasks.len() == max_connections {
                        eprintln!(
                            "mez host: remote client capacity saturated: active {}, limit {}; new clients will wait",
                            tasks.len(), max_connections
                        );
                        remote_capacity_saturated = true;
                    }
                }
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(joined) = joined {
                        shutdown_report.record_task_completion(joined, false);
                        if remote_capacity_saturated && tasks.len() < policy.max_connections {
                            eprintln!(
                                "mez host: remote client capacity recovered: active {}, limit {}",
                                tasks.len(), policy.max_connections
                            );
                            remote_capacity_saturated = false;
                        }
                    }
                }
            }
        }

        shutdown_report.endpoint_close_failed = !self.endpoint.shutdown_handle().close().await;
        drain_host_iroh_connection_tasks(&mut tasks, policy.setup_timeout, &mut shutdown_report)
            .await;
        shutdown_report.finish_scoped(accepted, endpoint_closed_unexpectedly)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "transport setup, trust, routing, diagnostics, audit, and peer route are independent connection handoff inputs"
)]
async fn serve_host_only_connection(
    incoming: iroh::endpoint::Incoming,
    policy: RuntimeIrohTransportPolicy,
    trust: RemoteTrustStore,
    server_endpoint_id: String,
    router: Option<HostSessionRouter>,
    diagnostics: RuntimeIrohDiagnostics,
    audit_log: Option<HostAuditLog>,
    remote_route: &str,
    authenticated_endpoint_id: &std::sync::OnceLock<String>,
) -> Result<()> {
    let setup_started = std::time::Instant::now();
    let mut setup_diagnostics =
        HostIrohSetupDiagnosticsGuard::new(diagnostics.clone(), setup_started);
    let setup_deadline = tokio::time::Instant::now() + policy.setup_timeout;
    let mut accepting = incoming
        .accept()
        .map_err(|error| MezError::invalid_state(format!("host Iroh accept failed: {error}")))?;
    let alpn = tokio::time::timeout_at(setup_deadline, accepting.alpn())
        .await
        .map_err(|_| MezError::invalid_state("host Iroh ALPN setup timed out"))?
        .map_err(|error| MezError::invalid_state(format!("host Iroh ALPN failed: {error}")))?;
    let control_setup_deadline = tokio::time::Instant::now() + policy.setup_timeout;
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
    let connection = tokio::time::timeout_at(control_setup_deadline, accepting)
        .await
        .map_err(|_| MezError::invalid_state("host Iroh connection setup timed out"))?
        .map_err(|error| {
            MezError::invalid_state(format!("host Iroh connection failed: {error}"))
        })?;
    connection.set_max_concurrent_bi_streams(iroh::endpoint::VarInt::from_u32(1));
    connection.set_max_concurrent_uni_streams(iroh::endpoint::VarInt::from_u32(0));
    let client_endpoint_id = connection.remote_id().to_string();
    let _ = authenticated_endpoint_id.set(client_endpoint_id.clone());
    eprintln!(
        "mez host: remote client connected: endpoint {client_endpoint_id}, route {remote_route}"
    );
    let _connection_log = RemoteClientConnectionLog {
        endpoint_id: client_endpoint_id.clone(),
        route: remote_route.to_owned(),
    };
    let (send, recv) = tokio::time::timeout_at(control_setup_deadline, connection.accept_bi())
        .await
        .map_err(|_| MezError::invalid_state("host Iroh control stream setup timed out"))?
        .map_err(|error| MezError::invalid_state(format!("host Iroh stream failed: {error}")))?;
    let compression_metrics = IrohCompressionMetrics::new(compression.codec());
    let mut bridge = IrohCompressionBridge::spawn_with_metrics(
        recv,
        send,
        compression,
        compression_metrics.clone(),
        HOST_CONTROL_MAX_CONTENT_LENGTH,
    )?;
    let request = tokio::time::timeout_at(
        control_setup_deadline,
        read_one_control_frame(bridge.stream_mut(), policy.idle_timeout),
    )
    .await
    .map_err(|_| MezError::invalid_state("host Iroh initialize read timed out"))??;
    let connection_guard = setup_diagnostics.connection_started(&connection);
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
            compression_metrics,
            diagnostics,
            connection_guard,
            &policy,
        )
        .await;
    }
    let _connection_guard = connection_guard;
    let initialized = match handle_host_only_initialize_with_audit(
        &request,
        &trust,
        &server_endpoint_id,
        &client_endpoint_id,
        audit_log.as_ref(),
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
    shutdown_host_iroh_connection(
        bridge,
        &connection,
        policy.setup_timeout,
        0,
        b"host-only complete",
    )
    .await
}

/// Finishes the host control stream before closing its connection, then
/// settles the inbound bridge pump after connection close makes peer EOF
/// observable. All phases share one deadline so a non-cooperative peer cannot
/// keep a host connection task alive indefinitely.
async fn shutdown_host_iroh_connection(
    mut bridge: IrohCompressionBridge,
    connection: &iroh::endpoint::Connection,
    timeout: std::time::Duration,
    close_code: u32,
    close_reason: &'static [u8],
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    let finish_result = bridge.finish_outbound_until(deadline).await;
    let outbound_result = bridge.settle_outbound_until(deadline).await;
    connection.close(iroh::endpoint::VarInt::from_u32(close_code), close_reason);
    let bridge_result = bridge.settle_until(deadline).await;
    finish_result?;
    outbound_result?;
    bridge_result
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
    compression_metrics: IrohCompressionMetrics,
    diagnostics: RuntimeIrohDiagnostics,
    connection_guard: crate::runtime::RuntimeIrohConnectionGuard,
    policy: &RuntimeIrohTransportPolicy,
) -> Result<()> {
    let response_id = request_id(&request_body);
    let mut initialization_sent = false;
    let mut attachment_cleanup = None;
    let result = serve_routed_initialize_inner(
        request_body,
        trust,
        server_endpoint_id,
        client_endpoint_id,
        router,
        &connection,
        &mut bridge,
        compression,
        compression_metrics,
        diagnostics,
        connection_guard,
        policy,
        &mut initialization_sent,
        &mut attachment_cleanup,
    )
    .await;
    // The outer owner survives all early errors; dropping this future transfers
    // teardown to the actor without spawning an untracked asynchronous task.
    let attachment_cleanup_result = match attachment_cleanup.take() {
        Some(cleanup) => cleanup.close(policy.setup_timeout).await,
        None => Ok(()),
    };
    let error_write_result: Result<()> = if let Err(error) = result.as_ref()
        && !initialization_sent
    {
        async {
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
        }
        .await
    } else {
        Ok(())
    };
    let bridge_result = shutdown_host_iroh_connection(
        bridge,
        &connection,
        policy.setup_timeout,
        u32::from(result.is_err()),
        b"routed control complete",
    )
    .await;
    if attachment_cleanup_result.is_err()
        && let Err(error) = result.as_ref()
    {
        return Err(MezError::new(
            error.kind(),
            format!("{error}; routed attachment cleanup also failed"),
        ));
    }
    result?;
    attachment_cleanup_result?;
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
    compression_metrics: IrohCompressionMetrics,
    diagnostics: RuntimeIrohDiagnostics,
    connection_guard: crate::runtime::RuntimeIrohConnectionGuard,
    policy: &RuntimeIrohTransportPolicy,
    initialization_sent: &mut bool,
    attachment_cleanup: &mut Option<Box<crate::host::async_runtime::ControlConnectionCleanupLease>>,
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
    #[cfg(test)]
    let mut setup_timing = initialize_diagnostics::InitializeDiagnostics::new();
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
    #[cfg(test)]
    setup_timing.advance("routing");
    let mut provisioning = None;
    let mut binding = match intent {
        SessionIntent::Create => {
            let prepared = router
                .prepare_remote(
                    &principal,
                    crate::host::router::RemoteSessionCreateRequest {
                        name: session_name,
                        idempotency_key: init.idempotency_key.clone().ok_or_else(|| {
                            MezError::invalid_args("create intent requires idempotency_key")
                        })?,
                        size,
                    },
                )
                .await?;
            let binding = crate::host::router::RemoteSessionBinding {
                lease: prepared.lease().clone(),
                runtime: prepared.runtime()?.clone(),
            };
            provisioning = Some(prepared);
            binding
        }
        SessionIntent::ResolveOrCreate => {
            let prepared = router
                .prepare_resolve_or_create_remote(
                    &principal,
                    crate::host::router::RemoteSessionCreateRequest {
                        name: session_name,
                        idempotency_key: init.idempotency_key.clone().ok_or_else(|| {
                            MezError::invalid_args(
                                "resolve_or_create intent requires idempotency_key",
                            )
                        })?,
                        size,
                    },
                )
                .await?;
            let binding = crate::host::router::RemoteSessionBinding {
                lease: prepared.lease().clone(),
                runtime: prepared.runtime()?.clone(),
            };
            provisioning = Some(prepared);
            binding
        }
        SessionIntent::Attach => {
            router
                .resolve_remote(&principal, init.session_target_json.as_deref())
                .await?
        }
        SessionIntent::Default => router.resolve_remote(&principal, None).await?,
        SessionIntent::HostOnly => unreachable!("host-only intent returned above"),
    };

    #[cfg(test)]
    let lose_outbound_reply = params.get("client_name").and_then(Value::as_str)
        == Some("test-outbound-lose-committed-reply");
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
    connection_state.bind_x11_connection_id(format!("iroh-{}", connection.stable_id()))?;
    #[cfg(test)]
    setup_timing.advance("actor-admission");
    let mut initialized = binding
        .runtime
        .actor()
        .initialize_routed_control_connection(
            encode_control_body(&actor_request.to_string()),
            HOST_CONTROL_MAX_CONTENT_LENGTH,
            connection_state,
        )
        .await?;
    *attachment_cleanup = initialized.connection_cleanup.take();
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
    let actor_initialized = response.get("error").is_none();
    #[cfg(test)]
    setup_timing.advance("lease-settlement");
    if actor_initialized && let Some(prepared) = provisioning.take() {
        binding = prepared.commit()?;
    }
    if !actor_initialized {
        drop(provisioning.take());
    }
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
    // The request-local seam closes the actual raw bridge peer after attachment
    // and provisioning commit; production has no client-controlled failpoint.
    #[cfg(test)]
    if request_id.as_str() == Some("test-fail-routed-response") || lose_outbound_reply {
        bridge.fail_raw_peer_for_test().await;
    }
    #[cfg(test)]
    setup_timing.advance("reply-publication");
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
    #[cfg(test)]
    setup_timing.complete();
    if !actor_initialized {
        return Ok(());
    }

    let mut connection_state = initialized.connection;
    let connection_cleanup = attachment_cleanup.as_ref().ok_or_else(|| {
        MezError::invalid_state("routed initialization omitted attachment cleanup ownership")
    })?;
    connection_cleanup.own_disconnect(&mut connection_state);
    if let Some(route) = connection_state.take_x11_route_start() {
        route.activate(connection.clone(), compression, compression_metrics.clone())?;
    }
    let client_id = connection_state
        .caller_client_id()
        .cloned()
        .ok_or_else(|| {
            MezError::invalid_state("routed Iroh initialization omitted client identity")
        })?;
    binding
        .runtime
        .actor()
        .set_host_routed_iroh_diagnostics(diagnostics)
        .await?;
    let sampler = Arc::new(Mutex::new(connection_guard.sampler(compression_metrics)));
    if let Ok(mut sampler) = sampler.lock() {
        sampler.sample(connection, &client_id);
    }
    let periodic_sampler = sampler.clone();
    let periodic_connection = connection.clone();
    let mut sample_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Ok(mut sampler) = periodic_sampler.lock() {
                sampler.sample_current(&periodic_connection);
            }
        }
    });
    let (event_stop_tx, event_stop_rx) = tokio::sync::watch::channel(false);
    let event_task = connection_state.take_event_stream_start().map(
        |(client_id, version, client_clipboard_write, push_render)| {
            #[cfg(test)]
            if let Some(fault) = request_id
                .as_str()
                .filter(|id| id.starts_with("test-event-task-"))
            {
                let panic = fault.ends_with("panic");
                return tokio::spawn(async move {
                    if panic {
                        panic!("private injected event panic payload");
                    }
                    Err(MezError::invalid_args("Iroh render transfer exceeds limit"))
                });
            }
            tokio::spawn(serve_host_routed_iroh_event_stream(
                (*connection).clone(),
                binding.runtime.actor().clone(),
                client_id,
                version,
                client_clipboard_write,
                push_render,
                compression,
                policy.setup_timeout,
                policy.idle_timeout,
                event_stop_rx,
            ))
        },
    );
    let mut event_task = crate::runtime::IrohEventTask::new(event_task);
    let control_config =
        AsyncRuntimeControlConnectionConfig::new(HOST_CONTROL_MAX_CONTENT_LENGTH, 0)?;
    let authority_principal = principal.clone();
    let authority_lease = binding.lease.clone();
    let request_trust = trust.clone();
    let request_router = router.clone();
    let request_server_endpoint_id = server_endpoint_id.to_string();
    let mut trust_changes = trust.authority_changes();
    let mut lease_changes = router.authority_changes();
    let cancellation_trust = trust.clone();
    let cancellation_router = router.clone();
    let cancellation_principal = principal.clone();
    let cancellation_lease = binding.lease.clone();
    let cancellation_server_endpoint_id = server_endpoint_id.to_string();
    let authority_cancelled = async move {
        loop {
            if cancellation_trust
                .validate_bound_principal(&cancellation_server_endpoint_id, &cancellation_principal)
                .and_then(|()| {
                    cancellation_router
                        .validate_bound_lease(&cancellation_principal, &cancellation_lease)
                })
                .is_err()
            {
                return;
            }
            tokio::select! {
                changed = trust_changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                changed = lease_changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    };
    let control =
        serve_authenticated_async_runtime_control_connection_loop_with_snapshots_hooks_and_cancellation(
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
            move |_| {
                request_trust.validate_bound_principal(
                    &request_server_endpoint_id,
                    &authority_principal,
                )?;
                request_router.validate_bound_lease(&authority_principal, &authority_lease)
            },
            |_| Ok(()),
            authority_cancelled,
        );
    let (control_result, _) = event_task.supervise(control).await;
    let x11_route_result = connection_state.deactivate_x11_route();
    sample_task.abort();
    let _ = (&mut sample_task).await;
    let _ = event_stop_tx.send(true);
    let event_result = event_task
        .settle_until(tokio::time::Instant::now() + policy.setup_timeout)
        .await;
    crate::runtime::merge_iroh_event_result(control_result, event_result)?;
    x11_route_result?;
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

#[derive(Debug)]
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
    handle_host_only_initialize_with_audit(
        body,
        trust,
        server_endpoint_id,
        client_endpoint_id,
        None,
    )
}

fn handle_host_only_initialize_with_audit(
    body: &str,
    trust: &RemoteTrustStore,
    server_endpoint_id: &str,
    client_endpoint_id: &str,
    audit_log: Option<&HostAuditLog>,
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
            append_pairing_audit(
                audit_log,
                client_endpoint_id,
                &principal.trust_record_id,
                None,
                "attempted",
            )?;
            let redemption = match trust.commit_invitation(preparation, now) {
                Ok(redemption) => redemption,
                Err(error) => {
                    append_pairing_audit(
                        audit_log,
                        client_endpoint_id,
                        &principal.trust_record_id,
                        None,
                        "failed",
                    )?;
                    return Err(error);
                }
            };
            if let Err(error) = append_pairing_audit(
                audit_log,
                client_endpoint_id,
                &principal.trust_record_id,
                Some(redemption.invitation_id()),
                "succeeded",
            ) {
                trust.rollback_invitation_redemption(&redemption)?;
                return Err(error);
            }
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
    let mut methods = Vec::new();
    if principal.host_routing.session_list {
        methods.push("host/session/list");
    }
    if principal.host_routing.session_kill {
        methods.push("host/session/kill");
    }
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
        "capabilities": { "methods": methods, "features": { "host_only": true } },
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

fn append_pairing_audit(
    audit_log: Option<&HostAuditLog>,
    client_endpoint_id: &str,
    trust_record_id: &str,
    invitation_id: Option<&str>,
    outcome: &str,
) -> Result<()> {
    let Some(audit_log) = audit_log else {
        return Ok(());
    };
    let mut audit_log = audit_log
        .lock()
        .map_err(|_| MezError::invalid_state("host audit lock was poisoned"))?;
    let Some(audit_log) = audit_log.as_mut() else {
        return Ok(());
    };
    let mut record = AuditRecord::new(
        "host",
        AuditActor {
            kind: "remote_endpoint".to_string(),
            id: client_endpoint_id.to_string(),
        },
        "trust_administration",
        "invitation_redeem",
    )
    .with_metadata("trust_record_id", trust_record_id)
    .with_metadata("client_endpoint_id", client_endpoint_id);
    if let Some(invitation_id) = invitation_id {
        record = record.with_metadata("invitation_id", invitation_id);
    }
    record.outcome = outcome.to_string();
    audit_log.append(record.sanitized())?;
    Ok(())
}

async fn read_optional_control_frame<S>(
    stream: &mut S,
    timeout: std::time::Duration,
) -> Result<Option<String>>
where
    S: tokio::io::AsyncRead + Unpin,
{
    tokio::time::timeout(timeout, async {
        let mut input = BytesMut::new();
        let mut decoder = ProtocolFrameCodec::new(HOST_CONTROL_MAX_CONTENT_LENGTH)?;
        let mut buffer = [0u8; 8192];
        loop {
            if let Some(body) = decode_host_control_frame(
                &mut decoder,
                &mut input,
                "host Iroh accepts one host-only follow-up frame",
            )? {
                return Ok(Some(body));
            }
            let read = stream.read(&mut buffer).await?;
            if read == 0 {
                if input.is_empty() {
                    return Ok(None);
                }
                return Err(host_control_frame_eof_error(
                    &input,
                    "host Iroh stream closed during a follow-up request",
                ));
            }
            input.extend_from_slice(&buffer[..read]);
            if input.len() > HOST_CONTROL_MAX_CONTENT_LENGTH + 8192 {
                return Err(MezError::invalid_args(
                    "host Iroh follow-up frame exceeds limit",
                ));
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
        Some("host/session/kill") => {
            let params = request
                .get("params")
                .and_then(Value::as_object)
                .ok_or_else(|| MezError::invalid_args("host/session/kill requires params"))?;
            let target = params
                .get("target")
                .and_then(Value::as_str)
                .filter(|target| !target.is_empty())
                .ok_or_else(|| MezError::invalid_args("host/session/kill requires target"))?;
            if params.get("force").and_then(Value::as_bool) != Some(true) {
                return Err(MezError::invalid_args(
                    "host/session/kill requires force=true",
                ));
            }
            params
                .get("idempotency_key")
                .and_then(Value::as_str)
                .filter(|key| !key.is_empty())
                .ok_or_else(|| {
                    MezError::invalid_args("host/session/kill requires idempotency_key")
                })?;
            router
                .force_kill_remote(principal, target)
                .await
                .map(|lease| {
                    json!({
                        "killed": true,
                        "lease_id": lease.lease_id,
                        "session_id": lease.session_id,
                        "state": remote_lease_state_name(lease.state),
                    })
                })
        }
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
        "expires_at_unix_seconds": lease.expires_at_unix_seconds,
    })
}

fn remote_lease_state_name(state: RemoteSessionLeaseState) -> &'static str {
    match state {
        RemoteSessionLeaseState::Pending => "pending",
        RemoteSessionLeaseState::Active => "active",
        RemoteSessionLeaseState::Recoverable => "failed",
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
        let mut input = BytesMut::new();
        let mut decoder = ProtocolFrameCodec::new(HOST_CONTROL_MAX_CONTENT_LENGTH)?;
        let mut buffer = [0u8; 8192];
        loop {
            if let Some(body) = decode_host_control_frame(
                &mut decoder,
                &mut input,
                "host Iroh accepts exactly one setup frame",
            )? {
                return Ok(body);
            }
            let read = stream.read(&mut buffer).await?;
            if read == 0 {
                return Err(host_control_frame_eof_error(
                    &input,
                    "host Iroh stream closed before initialize",
                ));
            }
            input.extend_from_slice(&buffer[..read]);
            if input.len() > HOST_CONTROL_MAX_CONTENT_LENGTH + 8192 {
                return Err(MezError::invalid_args(
                    "host Iroh initialize frame exceeds limit",
                ));
            }
        }
    })
    .await
    .map_err(|_| MezError::invalid_state("host Iroh initialize read timed out"))?
}

/// Incrementally decodes one control frame while preserving terminal framing
/// failures and rejecting bytes buffered after the single allowed frame.
fn decode_host_control_frame(
    decoder: &mut ProtocolFrameCodec,
    input: &mut BytesMut,
    trailing_frame_message: &str,
) -> Result<Option<String>> {
    let Some(frame) = decoder.decode(input)? else {
        return Ok(None);
    };
    if frame.content_type != CONTROL_CONTENT_TYPE {
        return Err(MezError::invalid_args(
            "unexpected control frame content type",
        ));
    }
    if !input.is_empty() {
        return Err(MezError::invalid_args(trailing_frame_message));
    }
    Ok(Some(frame.body))
}

/// Converts incomplete buffered input at EOF into the decoder's specific
/// framing error while retaining the established empty-stream diagnostic.
fn host_control_frame_eof_error(input: &[u8], empty_stream_message: &str) -> MezError {
    if input.is_empty() {
        return MezError::invalid_state(empty_stream_message);
    }
    match decode_control_frame(input, HOST_CONTROL_MAX_CONTENT_LENGTH) {
        Err(error) => error,
        Ok(_) => MezError::invalid_state(empty_stream_message),
    }
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
        MezErrorKind::RateLimited => -32011,
        _ => -32004,
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": error.message(),
            "data": { "mezzanine_code": host_error_name(error.kind()) }
        }
    })
    .to_string()
}

fn host_error_name(kind: MezErrorKind) -> &'static str {
    match kind {
        MezErrorKind::InvalidArgs => "invalid_params",
        MezErrorKind::InvalidState => "invalid_state",
        MezErrorKind::Config | MezErrorKind::Io => "internal_error",
        MezErrorKind::Conflict => "conflict",
        MezErrorKind::NotFound => "not_found",
        MezErrorKind::Forbidden => "forbidden",
        MezErrorKind::RateLimited => "rate_limited",
        MezErrorKind::NotImplemented => "method_not_found",
    }
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
mod tests;
