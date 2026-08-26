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

const HOST_CONTROL_MAX_CONTENT_LENGTH: usize = 1024 * 1024;

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

/// Privacy-safe aggregate of persistent-host Iroh cleanup degradation.
#[derive(Debug, Default)]
struct HostIrohShutdownReport {
    endpoint_close_failed: bool,
    task_join_failures: usize,
    unexpected_task_cancellations: usize,
    forced_aborts: usize,
}

impl HostIrohShutdownReport {
    /// Records one connection-task join without retaining panic payloads or
    /// peer-identifying context.
    fn record_task_completion(
        &mut self,
        joined: std::result::Result<(), tokio::task::JoinError>,
        cancellation_expected: bool,
    ) {
        let Err(error) = joined else {
            return;
        };
        if error.is_cancelled() {
            if !cancellation_expected {
                self.unexpected_task_cancellations =
                    self.unexpected_task_cancellations.saturating_add(1);
            }
        } else {
            self.task_join_failures = self.task_join_failures.saturating_add(1);
        }
    }

    /// Returns the accepted count only for a fully clean listener and cleanup
    /// outcome; otherwise returns one deterministic aggregate service error.
    fn finish(self, accepted: u64, endpoint_closed_unexpectedly: bool) -> Result<u64> {
        let mut failures = Vec::new();
        if endpoint_closed_unexpectedly {
            failures.push("persistent host Iroh listener closed unexpectedly".to_string());
        }
        if self.endpoint_close_failed {
            failures.push("endpoint close timed out".to_string());
        }
        if self.task_join_failures != 0 {
            failures.push(format!(
                "{} connection task joins failed",
                self.task_join_failures
            ));
        }
        if self.unexpected_task_cancellations != 0 {
            failures.push(format!(
                "{} connection tasks were cancelled unexpectedly",
                self.unexpected_task_cancellations
            ));
        }
        if self.forced_aborts != 0 {
            failures.push(format!(
                "{} connection tasks required forced abort",
                self.forced_aborts
            ));
        }
        if failures.is_empty() {
            return Ok(accepted);
        }
        Err(MezError::invalid_state(format!(
            "{} after accepting {accepted} connections",
            failures.join("; ")
        )))
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
        shutdown_report.finish(accepted, endpoint_closed_unexpectedly)
    }
}

/// Drains every persistent-host connection task within policy, then aborts and
/// joins all remaining work while retaining privacy-safe degradation counts.
async fn drain_host_iroh_connection_tasks(
    tasks: &mut JoinSet<()>,
    timeout: std::time::Duration,
    report: &mut HostIrohShutdownReport,
) {
    let drain = async {
        while let Some(joined) = tasks.join_next().await {
            report.record_task_completion(joined, false);
        }
    };
    if tokio::time::timeout(timeout, drain).await.is_ok() {
        return;
    }
    report.forced_aborts = report.forced_aborts.saturating_add(tasks.len());
    tasks.abort_all();
    while let Some(joined) = tasks.join_next().await {
        report.record_task_completion(joined, true);
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
    let connection = tokio::time::timeout_at(setup_deadline, accepting)
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
    let (send, recv) = tokio::time::timeout_at(setup_deadline, connection.accept_bi())
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
        setup_deadline,
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
    compression_metrics: IrohCompressionMetrics,
    diagnostics: RuntimeIrohDiagnostics,
    connection_guard: crate::runtime::RuntimeIrohConnectionGuard,
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
        compression_metrics,
        diagnostics,
        connection_guard,
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
    compression_metrics: IrohCompressionMetrics,
    diagnostics: RuntimeIrohDiagnostics,
    connection_guard: crate::runtime::RuntimeIrohConnectionGuard,
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
        SessionIntent::Attach => {
            router
                .resolve_remote(&principal, init.session_target_json.as_deref())
                .await?
        }
        SessionIntent::Default => router.resolve_remote(&principal, None).await?,
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
    let actor_initialized = response.get("error").is_none();
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
    if !actor_initialized {
        return Ok(());
    }

    let mut connection_state = initialized.connection;
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
        AsyncRuntimeControlConnectionConfig::new(HOST_CONTROL_MAX_CONTENT_LENGTH, 0)?
            .with_application_idle_timeout(policy.idle_timeout);
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
    let control_result =
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
        )
        .await;
    sample_task.abort();
    let _ = (&mut sample_task).await;
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
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    use crate::host::router::HostSessionRouterConfig;
    use crate::host::shell::{ResolvedShell, ShellSource};
    use crate::security::audit::{AuditConfig, AuditLog};
    use crate::security::remote::RemoteSessionAttachScope;

    use super::*;

    fn test_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "mez-host-iroh-{label}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    /// Endpoint-close degradation and a connection-task panic must both remain
    /// visible after cleanup without exposing the panic payload.
    #[tokio::test(flavor = "current_thread")]
    async fn host_shutdown_report_aggregates_close_and_task_failures() {
        let mut tasks = JoinSet::new();
        tasks.spawn(async { panic!("private panic payload") });
        let joined = tasks.join_next().await.unwrap();
        let mut report = HostIrohShutdownReport {
            endpoint_close_failed: true,
            ..HostIrohShutdownReport::default()
        };
        report.record_task_completion(joined, false);

        let error = report.finish(7, true).unwrap_err();

        assert_eq!(error.kind(), MezErrorKind::InvalidState);
        assert!(
            error
                .message()
                .contains("persistent host Iroh listener closed unexpectedly"),
            "{error:?}"
        );
        assert!(
            error.message().contains("endpoint close timed out"),
            "{error:?}"
        );
        assert!(
            error.message().contains("1 connection task joins failed"),
            "{error:?}"
        );
        assert!(
            error.message().contains("accepting 7 connections"),
            "{error:?}"
        );
        assert!(
            !error.message().contains("private panic payload"),
            "{error:?}"
        );
    }

    /// A connection task that exceeds the bounded drain is aborted, joined,
    /// and reported instead of being detached or misreported as clean.
    #[tokio::test(flavor = "current_thread")]
    async fn host_shutdown_report_records_forced_abort_and_joins_task() {
        let mut tasks = JoinSet::new();
        tasks.spawn(std::future::pending::<()>());
        let mut report = HostIrohShutdownReport::default();

        drain_host_iroh_connection_tasks(
            &mut tasks,
            std::time::Duration::from_millis(10),
            &mut report,
        )
        .await;

        assert!(tasks.is_empty());
        assert_eq!(report.forced_aborts, 1);
        assert_eq!(report.unexpected_task_cancellations, 0);
        let error = report.finish(1, false).unwrap_err();
        assert!(
            error
                .message()
                .contains("1 connection tasks required forced abort"),
            "{error:?}"
        );
    }

    /// Fully clean endpoint and task cleanup preserves the accepted count.
    #[test]
    fn host_shutdown_report_accepts_clean_completion() {
        assert_eq!(
            HostIrohShutdownReport::default().finish(3, false).unwrap(),
            3
        );
    }

    /// A complete malformed setup header is terminal and must fail before the
    /// much longer setup timeout even when the peer keeps its stream open.
    #[tokio::test(flavor = "current_thread")]
    async fn host_setup_reader_rejects_malformed_header_immediately() {
        let (mut peer, mut host) = tokio::io::duplex(1024);
        peer.write_all(b"Content-Length: invalid\r\n\r\n")
            .await
            .unwrap();

        let error = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            read_one_control_frame(&mut host, std::time::Duration::from_secs(5)),
        )
        .await
        .expect("terminal malformed setup frame should fail before timeout")
        .unwrap_err();

        assert_eq!(error.kind(), MezErrorKind::InvalidArgs);
        assert!(
            error.message().contains("invalid Content-Length"),
            "{error:?}"
        );
    }

    /// Follow-up frame decoding must propagate a terminal duplicate length
    /// error without retaining the authenticated connection until idle expiry.
    #[tokio::test(flavor = "current_thread")]
    async fn host_follow_up_reader_rejects_malformed_header_immediately() {
        let (mut peer, mut host) = tokio::io::duplex(1024);
        peer.write_all(b"Content-Length: 2\r\nContent-Length: 3\r\n\r\n{}!")
            .await
            .unwrap();

        let error = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            read_optional_control_frame(&mut host, std::time::Duration::from_secs(5)),
        )
        .await
        .expect("terminal malformed follow-up frame should fail before timeout")
        .unwrap_err();

        assert_eq!(error.kind(), MezErrorKind::InvalidArgs);
        assert!(
            error.message().contains("duplicate Content-Length"),
            "{error:?}"
        );
    }

    /// EOF after a declared but incomplete body must retain the framing error
    /// instead of replacing it with a generic stream-closure diagnostic.
    #[tokio::test(flavor = "current_thread")]
    async fn host_setup_reader_reports_truncated_frame_at_eof() {
        let (mut peer, mut host) = tokio::io::duplex(1024);
        peer.write_all(b"Content-Length: 4\r\n\r\n{").await.unwrap();
        peer.shutdown().await.unwrap();

        let error = read_one_control_frame(&mut host, std::time::Duration::from_secs(1))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), MezErrorKind::InvalidArgs);
        assert!(
            error.message().contains("incomplete protocol frame body"),
            "{error:?}"
        );
    }

    /// A valid setup frame split across reads remains incomplete until its
    /// final bytes arrive and is then decoded exactly once.
    #[tokio::test(flavor = "current_thread")]
    async fn host_setup_reader_accepts_fragmented_valid_frame() {
        let (mut peer, mut host) = tokio::io::duplex(1024);
        let encoded = encode_control_body(r#"{"jsonrpc":"2.0","id":1}"#);
        let split_at = encoded.len() - 3;
        let writer = async {
            peer.write_all(&encoded[..split_at]).await.unwrap();
            tokio::task::yield_now().await;
            peer.write_all(&encoded[split_at..]).await.unwrap();
        };

        let (body, ()) = tokio::join!(
            read_one_control_frame(&mut host, std::time::Duration::from_secs(1)),
            writer,
        );

        assert_eq!(body.unwrap(), r#"{"jsonrpc":"2.0","id":1}"#);
    }

    /// Routine connection logs expose route intent without retaining peer
    /// addresses or relay topology.
    #[test]
    fn remote_client_route_reports_privacy_safe_category() {
        let address = "192.0.2.42:443".parse().unwrap();

        assert_eq!(
            remote_client_route(iroh::endpoint::IncomingAddr::Ip(address)),
            "direct"
        );
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
        let audit_path = root.join("host-audit.jsonl");
        let audit_log =
            std::sync::Arc::new(std::sync::Mutex::new(Some(AuditLog::new(AuditConfig {
                enabled: true,
                path: audit_path.clone(),
                hash_chain: false,
                required: true,
            }))));
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
        let response = handle_host_only_initialize_with_audit(
            &request,
            &trust,
            identity.endpoint_id(),
            &client_id,
            Some(&audit_log),
        )
        .unwrap();
        let response: Value = serde_json::from_str(&response.body).unwrap();
        assert!(response["result"]["session"].is_null());
        assert!(response["result"]["lease"].is_null());
        assert!(response["result"]["device_credential"].is_string());
        assert_eq!(trust.list_records().unwrap().len(), 1);
        let audit = fs::read_to_string(audit_path).unwrap();
        assert!(audit.contains(r#""action":"invitation_redeem""#), "{audit}");
        assert!(audit.contains(r#""outcome":"attempted""#), "{audit}");
        assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
        assert!(!audit.contains(invitation.token.expose_secret()), "{audit}");

        let attach = request.replace("host_only", "attach");
        assert!(
            handle_host_only_initialize(&attach, &trust, identity.endpoint_id(), &client_id,)
                .is_err()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Required audit denial must prevent invitation authority from being
    /// committed when the host cannot write its pairing attribution record.
    #[test]
    fn host_only_pairing_rolls_back_when_required_audit_is_unavailable() {
        let root = test_root("pairing-audit-denied");
        let identity = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
        let trust = RemoteTrustStore::under_host_config_root(&root).unwrap();
        let invitation = trust
            .create_invitation(
                identity.endpoint_id(),
                RemoteRoleCeiling::Observer,
                600,
                current_unix_seconds().unwrap(),
            )
            .unwrap();
        let client_id = iroh::SecretKey::generate().public().to_string();
        let request = json!({
            "jsonrpc": "2.0",
            "id": "audit-denied",
            "method": "control/initialize",
            "params": {
                "client_name": "audit-denied-client",
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
        let audit_log =
            std::sync::Arc::new(std::sync::Mutex::new(Some(AuditLog::new(AuditConfig {
                enabled: false,
                path: root.join("disabled-audit.jsonl"),
                hash_chain: false,
                required: true,
            }))));

        let error = handle_host_only_initialize_with_audit(
            &request,
            &trust,
            identity.endpoint_id(),
            &client_id,
            Some(&audit_log),
        )
        .unwrap_err();
        assert_eq!(error.kind(), MezErrorKind::Forbidden);
        assert!(trust.list_records().unwrap().is_empty());

        let _ = fs::remove_dir_all(root);
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
            setup_timeout: std::time::Duration::from_secs(10),
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
        let diagnostics = host.endpoint.diagnostics();
        let stop = std::sync::Arc::new(tokio::sync::Notify::new());
        let server_stop = stop.clone();

        let server = host.serve(async move { server_stop.notified().await });
        let client_work = async {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !diagnostics.snapshot().listener_active {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("live persistent-host listener should report active");
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
        let snapshot = diagnostics.snapshot();
        assert!(!snapshot.listener_active);
        assert_eq!(snapshot.active_connections, 0);
        assert_eq!(snapshot.connections_accepted, 3);
        assert_eq!(snapshot.connections_rejected, 0);
        assert_eq!(snapshot.setup_successes, 3);
        assert_eq!(snapshot.setup_failures, 0);
        assert_eq!(
            snapshot
                .connections_completed
                .saturating_add(snapshot.connections_failed),
            3
        );
        assert_eq!(host.trust.list_records().unwrap().len(), 1);
        client.close().await;
        drop(host);
        let restarted = RemoteEndpointIdentity::load_or_create_host(&root).unwrap();
        assert_eq!(restarted.endpoint_id(), server_addr.id.to_string());
        let _ = std::fs::remove_dir_all(root);
    }

    /// Endpoint loss outside the host shutdown handle must fail the persistent
    /// listener instead of looking like a clean zero-connection completion.
    #[tokio::test(flavor = "current_thread")]
    async fn host_listener_reports_unexpected_endpoint_closure() {
        let root = test_root("unexpected-endpoint-closure");
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            identity: RuntimeIrohIdentityPolicy::Host,
            setup_timeout: std::time::Duration::from_secs(10),
            ..RuntimeIrohTransportPolicy::default()
        };
        let host = HostIrohRuntime::bind(&root, policy).await.unwrap().unwrap();
        let endpoint = host.endpoint.endpoint().clone();
        let diagnostics = host.endpoint.diagnostics();
        endpoint.close().await;

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            host.serve(std::future::pending()),
        )
        .await
        .expect("closed host endpoint should terminate its listener")
        .unwrap_err();
        assert_eq!(error.kind(), MezErrorKind::InvalidState);
        assert!(
            error
                .message()
                .contains("persistent host Iroh listener closed unexpectedly"),
            "{error:?}"
        );
        assert!(!diagnostics.snapshot().listener_active);

        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// Closing through the runtime shutdown handle marks endpoint exhaustion
    /// intentional and therefore retains clean host-listener completion.
    #[tokio::test(flavor = "current_thread")]
    async fn host_listener_accepts_intentional_endpoint_shutdown() {
        let root = test_root("intentional-endpoint-closure");
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            identity: RuntimeIrohIdentityPolicy::Host,
            setup_timeout: std::time::Duration::from_secs(10),
            ..RuntimeIrohTransportPolicy::default()
        };
        let host = HostIrohRuntime::bind(&root, policy).await.unwrap().unwrap();
        let diagnostics = host.endpoint.diagnostics();
        let shutdown = host.endpoint.shutdown_handle();
        let (served, closed) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(host.serve(std::future::pending()), shutdown.close())
        })
        .await
        .expect("intentional host endpoint shutdown should remain bounded");
        assert!(closed);
        assert_eq!(served.unwrap(), 0);
        assert!(!diagnostics.snapshot().listener_active);

        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// A peer that opens its control stream but withholds the mandatory first
    /// frame must release its sole admission slot within the setup deadline so
    /// a valid peer can initialize without waiting for application-idle expiry.
    #[tokio::test(flavor = "current_thread")]
    async fn host_connection_times_out_when_first_frame_is_withheld() {
        let root = test_root("withheld-first-frame");
        let endpoint_policy = RuntimeIrohTransportPolicy {
            enabled: true,
            identity: RuntimeIrohIdentityPolicy::Host,
            compression_codecs: vec![RuntimeIrohCompressionCodec::None],
            max_connections: 1,
            setup_timeout: std::time::Duration::from_secs(10),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let host = HostIrohRuntime::bind(&root, endpoint_policy.clone())
            .await
            .unwrap()
            .unwrap();
        let server_addr = host.endpoint_addr().unwrap();
        let client = crate::runtime::bind_runtime_iroh_client_endpoint(
            &endpoint_policy,
            iroh::SecretKey::generate(),
        )
        .await
        .unwrap();
        let mut handler_policy = endpoint_policy;
        handler_policy.setup_timeout = std::time::Duration::from_millis(250);
        let client_endpoint = client.clone();
        let client_setup = tokio::spawn(async move {
            let connection = client_endpoint
                .connect(server_addr.clone(), crate::runtime::MEZZANINE_IROH_ALPN)
                .await
                .unwrap();
            let (mut send, recv) = connection.open_bi().await.unwrap();
            send.write_all(&[0]).await.unwrap();
            send.flush().await.unwrap();
            (connection, (send, recv))
        });
        let incoming = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            host.endpoint.endpoint().accept(),
        )
        .await
        .expect("host should accept the silent peer")
        .expect("host endpoint should remain open");
        let remote_route = remote_client_route(incoming.remote_addr());
        let diagnostics = host.endpoint.diagnostics();
        let authenticated_endpoint_id = std::sync::OnceLock::new();
        let handler = serve_host_only_connection(
            incoming,
            handler_policy,
            host.trust.clone(),
            host.endpoint_id().to_string(),
            None,
            diagnostics.clone(),
            None,
            remote_route,
            &authenticated_endpoint_id,
        );
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let (server_result, client_result) = tokio::join!(handler, client_setup);
            let (connection, streams) = client_result.unwrap();
            drop(streams);
            connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
            server_result
        })
        .await
        .expect("silent first frame must not outlive the setup deadline")
        .unwrap_err();
        assert!(
            error.to_string().contains("initialize read timed out"),
            "{error}"
        );
        assert!(
            authenticated_endpoint_id.get().is_some(),
            "post-connect failures must retain the authenticated endpoint identity"
        );
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.active_connections, 0);
        assert_eq!(snapshot.connections_accepted, 0);
        assert_eq!(snapshot.connections_rejected, 1);
        assert_eq!(snapshot.setup_successes, 0);
        assert_eq!(snapshot.setup_failures, 1);

        client.close().await;
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// A capability-bearing host invitation pairs without provisioning, then
    /// routes create replay, conflict, explicit attach, default selection, and
    /// principal-filtered listing through one persistent front door. The live
    /// routed attachment must expose its exact-client Iroh transport statistics
    /// through `show-iroh-status`. A second paired device without routing
    /// authority receives structured denials.
    #[tokio::test(flavor = "current_thread")]
    async fn routed_host_end_to_end_enforces_intent_idempotency_and_authority() {
        let root = test_root("routed");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            identity: RuntimeIrohIdentityPolicy::Host,
            compression_codecs: vec![RuntimeIrohCompressionCodec::None],
            setup_timeout: std::time::Duration::from_secs(10),
            idle_timeout: std::time::Duration::from_secs(3),
            ..RuntimeIrohTransportPolicy::default()
        };
        let host = HostIrohRuntime::bind(&root, policy.clone())
            .await
            .unwrap()
            .unwrap();
        let diagnostics = host.endpoint.diagnostics();
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
            recovery_policy: crate::host::router::HostRecoveryPolicy::Lazy,
            default_session_policy:
                crate::host::router::HostDefaultSessionPolicy::MostRecentAttachable,
            default_lease_lifetime_seconds: 0,
        });
        let invitation = host
            .trust
            .create_host_invitation(
                host.endpoint_id(),
                RemoteRoleCeiling::Primary,
                RemoteHostRoutingAuthority {
                    session_create: true,
                    session_kill: false,
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
            assert_eq!(
                paired["result"]["capabilities"]["methods"],
                json!(["host/session/list"]),
                "{paired}"
            );
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
            assert_eq!(
                denied_pair["result"]["capabilities"]["methods"],
                json!([]),
                "{denied_pair}"
            );
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

            let (persistent_connection, mut persistent_bridge, persistent_attach) =
                open_test_routed_attach(&client, &server_addr, &credential, &session_id).await;
            assert_eq!(
                persistent_attach["result"]["lease"]["lease_id"], lease_id,
                "{persistent_attach}"
            );
            let status_request = json!({
                "jsonrpc": "2.0",
                "id": "test-routed-iroh-status",
                "method": "terminal/command",
                "params": {
                    "idempotency_key": "test-routed-iroh-status",
                    "input": "show-iroh-status"
                }
            })
            .to_string();
            persistent_bridge
                .stream_mut()
                .write_all(&encode_control_body(&status_request))
                .await
                .unwrap();
            persistent_bridge.stream_mut().flush().await.unwrap();
            let status_response = read_one_control_frame(
                persistent_bridge.stream_mut(),
                std::time::Duration::from_secs(3),
            )
            .await
            .unwrap();
            assert!(
                status_response.contains("| State | connected |"),
                "{status_response}"
            );
            assert!(status_response.contains("| RTT |"), "{status_response}");
            assert!(
                !status_response.contains("this client has no correlated live Iroh connection"),
                "{status_response}"
            );
            let detach_request = json!({
                "jsonrpc": "2.0",
                "id": "test-routed-detach",
                "method": "terminal/step",
                "params": {
                    "idempotency_key": "test-routed-detach",
                    "render": false,
                    "input_bytes": [1, 100]
                }
            })
            .to_string();
            persistent_bridge
                .stream_mut()
                .write_all(&encode_control_body(&detach_request))
                .await
                .unwrap();
            persistent_bridge.stream_mut().flush().await.unwrap();
            let detach_response = read_one_control_frame(
                persistent_bridge.stream_mut(),
                std::time::Duration::from_secs(3),
            )
            .await
            .unwrap();
            let detach_response: Value = serde_json::from_str(&detach_response).unwrap();
            assert_eq!(
                detach_response["result"]["client_detached"], true,
                "{detach_response}"
            );
            assert_eq!(
                detach_response["result"]["session_terminated"], false,
                "{detach_response}"
            );
            let _ = persistent_bridge
                .shutdown(std::time::Duration::from_secs(2))
                .await;
            persistent_connection.close(
                iroh::endpoint::VarInt::from_u32(0),
                b"detached test client complete",
            );

            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let snapshots = router.snapshots().await.unwrap();
                    if snapshots.len() == 1
                        && snapshots[0].state
                            == crate::host::session::SessionSupervisorState::Running
                        && snapshots[0].runtime_state
                            == Some(crate::runtime::RuntimeLifecycleState::Detached)
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("detached routed session should remain supervised and reattachable");
            assert_eq!(
                router.get_lease(&lease_id).unwrap().state,
                RemoteSessionLeaseState::Active
            );

            let (persistent_connection, persistent_bridge, persistent_attach) =
                open_test_routed_attach(&client, &server_addr, &credential, &session_id).await;
            assert_eq!(
                persistent_attach["result"]["lease"]["lease_id"], lease_id,
                "{persistent_attach}"
            );
            drop(persistent_bridge);
            persistent_connection.close(
                iroh::endpoint::VarInt::from_u32(1),
                b"abrupt routed client loss",
            );
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let snapshots = router.snapshots().await.unwrap();
                    if snapshots.len() == 1
                        && snapshots[0].state
                            == crate::host::session::SessionSupervisorState::Running
                        && snapshots[0].runtime_state
                            == Some(crate::runtime::RuntimeLifecycleState::Detached)
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("abrupt routed client loss should leave the session reattachable");
            assert_eq!(
                router.get_lease(&lease_id).unwrap().state,
                RemoteSessionLeaseState::Active
            );

            let (persistent_connection, mut persistent_bridge, persistent_attach) =
                open_test_routed_attach(&client, &server_addr, &credential, &session_id).await;
            assert_eq!(
                persistent_attach["result"]["lease"]["lease_id"], lease_id,
                "{persistent_attach}"
            );
            let record_id = host
                .trust
                .list_records()
                .unwrap()
                .into_iter()
                .find(|record| record.host_routing.session_create && !record.revoked())
                .unwrap()
                .id;
            host.trust
                .revoke_record(
                    &record_id,
                    Some("test active connection fence"),
                    current_unix_seconds().unwrap(),
                )
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                let mut buffer = [0u8; 1024];
                loop {
                    match persistent_bridge.stream_mut().read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            })
            .await
            .expect("trust revocation should close an idle routed connection");
            let _ = persistent_bridge
                .shutdown(std::time::Duration::from_secs(2))
                .await;
            persistent_connection.close(
                iroh::endpoint::VarInt::from_u32(0),
                b"revoked test complete",
            );

            let revoked_reconnect = exchange_test_routed_initialize(
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
                revoked_reconnect["error"]["data"]["mezzanine_code"], "forbidden",
                "{revoked_reconnect}"
            );
            stop.notify_one();
        };

        let (served, ()) = tokio::join!(server, client_work);
        assert_eq!(served.unwrap(), 14);
        let snapshot = diagnostics.snapshot();
        assert!(!snapshot.listener_active);
        assert_eq!(snapshot.active_connections, 0);
        assert_eq!(snapshot.connections_accepted, 14);
        assert_eq!(snapshot.setup_successes, 14);
        assert_eq!(snapshot.connections_rejected, snapshot.setup_failures);
        assert_eq!(
            snapshot
                .connections_completed
                .saturating_add(snapshot.connections_failed),
            14
        );
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
            "interactive": true,
            "terminal": {"columns": 80, "rows": 24, "term": "xterm-256color"}
        });
        if let Some(session_name) = session_name {
            client_metadata["metadata"] = json!({"session_name": session_name});
        }
        let mut params = json!({
            "client_name": "test-client",
            "requested_version": 3,
            "requested_role": "primary",
            "detach_primary_on_disconnect": true,
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

    async fn open_test_routed_attach(
        client: &iroh::Endpoint,
        server_addr: &iroh::EndpointAddr,
        credential: &str,
        session_id: &str,
    ) -> (iroh::endpoint::Connection, IrohCompressionBridge, Value) {
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
            "id": "test-persistent-attach",
            "method": "control/initialize",
            "params": {
                "client_name": "test-client",
                "requested_version": 3,
                "requested_role": "primary",
                "detach_primary_on_disconnect": true,
                "session_intent": "attach",
                "session_target": {"session_id": session_id},
                "client": {
                    "name": "test-client",
                    "interactive": true,
                    "terminal": {"columns": 80, "rows": 24, "term": "xterm-256color"}
                },
                "authentication": {
                    "mechanism": "extension:iroh_device",
                    "token": credential
                }
            }
        })
        .to_string();
        bridge
            .stream_mut()
            .write_all(&encode_control_body(&request))
            .await
            .unwrap();
        bridge.stream_mut().flush().await.unwrap();
        let response =
            read_one_control_frame(bridge.stream_mut(), std::time::Duration::from_secs(3))
                .await
                .unwrap();
        (connection, bridge, serde_json::from_str(&response).unwrap())
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
