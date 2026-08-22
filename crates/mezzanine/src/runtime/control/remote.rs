//! Local-only remote transport administration over the control protocol.
//!
//! These methods are deliberately unavailable to Iroh peers, including paired
//! primaries. The local Unix recovery path owns invitation creation, client
//! inspection, rename, and revocation until a separate delegation design is
//! approved.

use secrecy::{ExposeSecret, SecretString};

use super::{
    AuditActor, AuditRecord, ControlConnectionState, MezError, Result, RuntimeSessionService,
};
use crate::control::{AuthenticatedPeer, JsonRpcRequest, RequestedRole};
use crate::security::remote::{
    RemotePairingPreparation, RemotePairingRedemption, RemotePrincipal, RemoteRoleCeiling,
    RemoteTrustRecord, RemoteTrustStore,
};

/// Remote application authority validated without mutating live session or trust state.
pub(super) enum PreparedRemoteInitializeAuthority {
    /// Single-use invitation awaiting commit after ordinary initialization succeeds.
    Invitation {
        store: RemoteTrustStore,
        preparation: RemotePairingPreparation,
    },
    /// Durable device proof validated without updating usage metadata.
    Device {
        store: RemoteTrustStore,
        server_endpoint_id: String,
        endpoint_id: String,
        token: SecretString,
        requested_role: RequestedRole,
        principal: RemotePrincipal,
    },
}

impl PreparedRemoteInitializeAuthority {
    /// Returns provisional authority for cloned-state control initialization.
    pub(super) fn principal(&self) -> RemotePrincipal {
        match self {
            Self::Invitation { preparation, .. } => preparation.principal(),
            Self::Device { principal, .. } => principal.clone(),
        }
    }

    /// Revalidates and commits trust only after generic initialization succeeds.
    pub(super) fn commit(
        self,
        now_unix_seconds: u64,
    ) -> Result<CommittedRemoteInitializeAuthority> {
        match self {
            Self::Invitation { store, preparation } => {
                let principal = preparation.principal();
                let redemption = store.commit_invitation(preparation, now_unix_seconds)?;
                Ok(CommittedRemoteInitializeAuthority {
                    store,
                    principal,
                    device_credential: Some(redemption.device_credential.clone()),
                    invitation_redemption: Some(redemption),
                })
            }
            Self::Device {
                store,
                server_endpoint_id,
                endpoint_id,
                token,
                requested_role,
                principal,
            } => {
                let committed = store.resolve_principal(
                    &server_endpoint_id,
                    &endpoint_id,
                    &token,
                    requested_role,
                    now_unix_seconds,
                )?;
                if committed != principal {
                    return Err(MezError::invalid_state(
                        "remote device authority changed during initialization",
                    ));
                }
                Ok(CommittedRemoteInitializeAuthority {
                    store,
                    principal: committed,
                    device_credential: None,
                    invitation_redemption: None,
                })
            }
        }
    }
}

/// Trust state committed for one successful staged initialization.
pub(super) struct CommittedRemoteInitializeAuthority {
    store: RemoteTrustStore,
    principal: RemotePrincipal,
    device_credential: Option<SecretString>,
    invitation_redemption: Option<RemotePairingRedemption>,
}

impl CommittedRemoteInitializeAuthority {
    /// Returns the committed authority for consistency checks.
    pub(super) fn principal(&self) -> &RemotePrincipal {
        &self.principal
    }

    /// Returns the first-pairing credential projected into the success response.
    pub(super) fn device_credential(&self) -> Option<&SecretString> {
        self.device_credential.as_ref()
    }

    /// Restores a consumed invitation when later runtime publication fails.
    pub(super) fn rollback(&self) -> Result<()> {
        let Some(redemption) = self.invitation_redemption.as_ref() else {
            return Ok(());
        };
        self.store.rollback_invitation_redemption(redemption)
    }
}

impl RuntimeSessionService {
    /// Validates Iroh transport evidence without mutating live authority or trust.
    pub(super) fn prepare_remote_initialize_authority(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<Option<PreparedRemoteInitializeAuthority>> {
        let endpoint_id = match connection.authenticated_peer() {
            Some(AuthenticatedPeer::UnixUser { .. }) | None => return Ok(None),
            Some(AuthenticatedPeer::IrohEndpoint { endpoint_id }) => endpoint_id.clone(),
        };
        if request.method != "control/initialize" {
            return Err(MezError::forbidden(
                "Iroh connection must initialize before control requests",
            ));
        }
        let params = remote_params(request)?;
        let requested_role = match required_remote_string(&params, "requested_role")? {
            "observer" => RequestedRole::Observer,
            "primary" => RequestedRole::Primary,
            _ => {
                return Err(MezError::forbidden(
                    "Iroh control permits only primary or observer roles",
                ));
            }
        };
        let client_name = required_remote_string(&params, "client_name")?;
        let authentication = params
            .get("authentication")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| MezError::forbidden("Iroh control requires pairing or device proof"))?;
        let mechanism = required_remote_string(authentication, "mechanism")?;
        let token =
            SecretString::from(required_remote_string(authentication, "token")?.to_string());
        let config_root = self
            .integration
            .config_root()
            .ok_or_else(|| MezError::invalid_state("remote trust requires a config root"))?
            .to_path_buf();
        let session_id = self.session.id.to_string();
        let server_endpoint_id = self
            .integration
            .ensure_remote_endpoint_identity(&session_id)?
            .endpoint_id()
            .to_string();
        let store = RemoteTrustStore::under_config_root(&config_root, &session_id)?;
        match mechanism {
            "extension:iroh_invitation" => {
                let preparation = store.prepare_invitation(
                    &token,
                    &server_endpoint_id,
                    &endpoint_id,
                    client_name,
                    requested_role,
                    super::current_unix_seconds(),
                )?;
                Ok(Some(PreparedRemoteInitializeAuthority::Invitation {
                    store,
                    preparation,
                }))
            }
            "extension:iroh_device" => {
                let principal = store.validate_principal(
                    &server_endpoint_id,
                    &endpoint_id,
                    &token,
                    requested_role,
                )?;
                Ok(Some(PreparedRemoteInitializeAuthority::Device {
                    store,
                    server_endpoint_id,
                    endpoint_id,
                    token,
                    requested_role,
                    principal,
                }))
            }
            _ => Err(MezError::forbidden(
                "unsupported Iroh control authentication mechanism",
            )),
        }
    }

    /// Runs one staged remote initialization outside the ordinary Unix dispatch frame.
    pub(super) fn dispatch_prepared_remote_initialize(
        &mut self,
        body: &str,
        request: &JsonRpcRequest,
        connection: &mut ControlConnectionState,
        prepared: PreparedRemoteInitializeAuthority,
    ) -> String {
        let primary_before = self.session.primary_client_id().cloned();
        let observer_count_before = self.session.observers().len();
        let mut staged_session = self.session.clone();
        let mut staged_connection = connection.clone();
        if let Err(error) = staged_connection.bind_remote_principal(prepared.principal()) {
            return super::runtime_json_rpc_error(
                request.id.as_str(),
                error.kind(),
                error.message(),
            );
        }
        let response = super::dispatch_control_request_for_connection(
            body,
            &mut staged_session,
            &mut staged_connection,
            self.control.idempotency_mut(),
        );
        if !response.contains(r#""result""#) {
            if let Err(error) = self.append_runtime_remote_initialize_rejection_audit(
                request,
                connection,
                "initialize_failed",
            ) {
                return super::runtime_json_rpc_error(
                    request.id.as_str(),
                    error.kind(),
                    error.message(),
                );
            }
            return response;
        }
        let committed = match prepared.commit(super::current_unix_seconds()) {
            Ok(committed) => committed,
            Err(error) => {
                if let Err(audit_error) = self.append_runtime_remote_initialize_rejection_audit(
                    request,
                    connection,
                    "trust_commit_failed",
                ) {
                    return super::runtime_json_rpc_error(
                        request.id.as_str(),
                        audit_error.kind(),
                        audit_error.message(),
                    );
                }
                return super::runtime_json_rpc_error(
                    request.id.as_str(),
                    error.kind(),
                    error.message(),
                );
            }
        };
        if staged_connection.remote_principal() != Some(committed.principal()) {
            let _ = committed.rollback();
            return super::runtime_json_rpc_error(
                request.id.as_str(),
                crate::error::MezErrorKind::InvalidState,
                "remote authority changed during initialization",
            );
        }
        let original_session = std::mem::replace(&mut self.session, staged_session);
        let original_connection = std::mem::replace(connection, staged_connection);
        let response =
            match self.append_remote_pairing_credential(response, committed.device_credential()) {
                Ok(response) => response,
                Err(error) => {
                    self.session = original_session;
                    *connection = original_connection;
                    let _ = committed.rollback();
                    return super::runtime_json_rpc_error(
                        request.id.as_str(),
                        error.kind(),
                        error.message(),
                    );
                }
            };
        if let Err(error) = self.apply_runtime_initialize_side_effects(
            request,
            primary_before.as_ref(),
            observer_count_before,
        ) {
            self.session = original_session;
            *connection = original_connection;
            let _ = committed.rollback();
            return super::runtime_json_rpc_error(
                request.id.as_str(),
                error.kind(),
                error.message(),
            );
        }
        if let Err(error) = self.append_runtime_remote_initialize_success_audit(&committed) {
            self.session = original_session;
            *connection = original_connection;
            let _ = committed.rollback();
            return super::runtime_json_rpc_error(
                request.id.as_str(),
                error.kind(),
                error.message(),
            );
        }
        response
    }

    /// Appends one secret-safe audit record for a rejected remote initialization.
    pub(super) fn append_runtime_remote_initialize_rejection_audit(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
        reason: &str,
    ) -> Result<()> {
        let Some(AuthenticatedPeer::IrohEndpoint { endpoint_id }) = connection.authenticated_peer()
        else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "iroh_endpoint".to_string(),
                id: endpoint_id.clone(),
            },
            "remote_trust",
            "pairing_rejected",
        )
        .with_metadata("mechanism", remote_initialize_mechanism_name(request))
        .with_metadata("reason", reason);
        record.outcome = "rejected".to_string();
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Appends one secret-safe audit record after committed remote initialization.
    pub(super) fn append_runtime_remote_initialize_success_audit(
        &mut self,
        committed: &CommittedRemoteInitializeAuthority,
    ) -> Result<()> {
        let principal = committed.principal();
        let (action, mechanism) = if committed.invitation_redemption.is_some() {
            ("invitation_redeemed", "invitation")
        } else {
            ("device_authenticated", "device")
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "iroh_endpoint".to_string(),
                id: principal.endpoint_id.clone(),
            },
            "remote_trust",
            action,
        )
        .with_metadata("mechanism", mechanism)
        .with_metadata("trust_record_id", principal.trust_record_id.clone())
        .with_metadata("role", requested_role_name(principal.requested_role));
        record.outcome = "succeeded".to_string();
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Adds a first-pairing device credential only to a successful initialize response.
    pub(super) fn append_remote_pairing_credential(
        &self,
        response: String,
        credential: Option<&SecretString>,
    ) -> Result<String> {
        let Some(credential) = credential else {
            return Ok(response);
        };
        let mut value: serde_json::Value = serde_json::from_str(&response).map_err(|error| {
            MezError::invalid_state(format!("invalid initialize response: {error}"))
        })?;
        let result = value
            .get_mut("result")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| MezError::invalid_state("pairing initialize did not succeed"))?;
        result.insert(
            "device_credential".to_string(),
            serde_json::Value::String(credential.expose_secret().to_string()),
        );
        serde_json::to_string(&value).map_err(|error| {
            MezError::invalid_state(format!("failed to encode pairing response: {error}"))
        })
    }

    /// Dispatches one local-only remote administration request.
    pub(super) fn dispatch_runtime_remote_request(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<String> {
        if !matches!(
            connection.authenticated_peer(),
            Some(AuthenticatedPeer::UnixUser { .. })
        ) {
            return Err(MezError::forbidden(
                "remote trust administration requires the local Unix control transport",
            ));
        }
        let config_root = self
            .integration
            .config_root()
            .ok_or_else(|| MezError::invalid_state("remote trust requires a config root"))?
            .to_path_buf();
        let session_id = self.session.id.to_string();
        let cache_key = if remote_administration_mutates(&request.method) {
            let params = remote_params(request)?;
            let idempotency_key = required_remote_string(&params, "idempotency_key")?;
            let caller_client_id = connection.caller_client_id().ok_or_else(|| {
                MezError::forbidden("remote administration has no initialized local client")
            })?;
            let cache_key = format!("{caller_client_id}:{idempotency_key}");
            if let Some(response) = self.control.idempotency_mut().cached_response(
                &cache_key,
                &request.method,
                &request.params,
            )? {
                return Ok(response);
            }
            Some(cache_key)
        } else {
            None
        };

        let result = match request.method.as_str() {
            "remote/status" => {
                let endpoint_id = self
                    .integration
                    .ensure_remote_endpoint_identity(&session_id)?
                    .endpoint_id()
                    .to_string();
                let structured = crate::runtime::runtime_effective_config_value(
                    self.integration.config_layers(),
                )?;
                let policy =
                    crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
                let diagnostics = self.integration.remote_iroh_diagnostics();
                Ok(serde_json::json!({
                    "enabled": policy.enabled,
                    "listener_active": diagnostics.listener_active,
                    "endpoint_id": endpoint_id,
                    "endpoint_addr": self.integration.remote_endpoint_addr(),
                    "address_lookup": policy.address_lookup.as_str(),
                    "relay_mode": policy.relay.as_str(),
                    "direct_connections": policy.direct_connections,
                    "port_mapping": policy.port_mapping,
                    "proxy_from_env": policy.proxy_from_env,
                    "system_ca_store": policy.system_ca_store,
                    "active_remote_connections": diagnostics.active_connections,
                    "connections_accepted": diagnostics.connections_accepted,
                    "connections_rejected": diagnostics.connections_rejected,
                    "setup_successes": diagnostics.setup_successes,
                    "setup_failures": diagnostics.setup_failures,
                    "setup_latency_average_millis": diagnostics.average_setup_latency_millis(),
                    "setup_latency_max_millis": diagnostics.setup_latency_max_millis,
                    "connections_completed": diagnostics.connections_completed,
                    "connections_failed": diagnostics.connections_failed,
                    "shutdown_aborts": diagnostics.shutdown_aborts,
                    "last_connection_path": diagnostics.last_path_name(),
                    "path_counts": {
                        "direct": diagnostics.direct_connections,
                        "relay": diagnostics.relay_connections,
                        "custom": diagnostics.custom_connections,
                        "unknown": diagnostics.unknown_connections,
                    },
                })
                .to_string())
            }
            "remote/invite" => {
                let params = remote_params(request)?;
                let role = match params
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("observer")
                {
                    "observer" => RemoteRoleCeiling::Observer,
                    "primary" => RemoteRoleCeiling::Primary,
                    _ => {
                        return Err(MezError::invalid_args(
                            "remote/invite role must be observer or primary",
                        ));
                    }
                };
                let ttl_seconds = params
                    .get("expires_seconds")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(600);
                let endpoint_id = self
                    .integration
                    .ensure_remote_endpoint_identity(&session_id)?
                    .endpoint_id()
                    .to_string();
                let endpoint_addr = self.integration.remote_endpoint_addr().cloned();
                if let Some(endpoint_addr) = endpoint_addr.as_ref()
                    && endpoint_addr.id.to_string() != endpoint_id
                {
                    return Err(MezError::invalid_state(
                        "bound Iroh listener identity does not match remote trust identity",
                    ));
                }
                let invitation = RemoteTrustStore::under_config_root(&config_root, &session_id)?
                    .create_invitation(
                        &endpoint_id,
                        role,
                        ttl_seconds,
                        super::current_unix_seconds(),
                    )?;
                Ok(serde_json::json!({
                    "invitation_id": invitation.invitation_id,
                    "token": invitation.token.expose_secret(),
                    "server_endpoint_id": invitation.server_endpoint_id,
                    "server_addr": endpoint_addr,
                    "profile_name": session_id,
                    "role": invitation.role_ceiling.as_str(),
                    "expires_at_unix_seconds": invitation.expires_at_unix_seconds,
                })
                .to_string())
            }
            "remote/client/list" => {
                let records = RemoteTrustStore::under_config_root(&config_root, &session_id)?
                    .list_records()?;
                let clients = records
                    .iter()
                    .map(remote_trust_record_json)
                    .collect::<Vec<_>>();
                Ok(serde_json::json!({ "clients": clients }).to_string())
            }
            "remote/client/rename" => {
                let params = remote_params(request)?;
                let record_id = required_remote_string(&params, "client_id")?;
                let label = required_remote_string(&params, "label")?;
                let record = RemoteTrustStore::under_config_root(&config_root, &session_id)?
                    .rename_record(record_id, label)?;
                Ok(remote_trust_record_json(&record).to_string())
            }
            "remote/client/revoke" => {
                let params = remote_params(request)?;
                let record_id = required_remote_string(&params, "client_id")?;
                let reason = params.get("reason").and_then(serde_json::Value::as_str);
                let record = RemoteTrustStore::under_config_root(&config_root, &session_id)?
                    .revoke_record(record_id, reason, super::current_unix_seconds())?;
                Ok(remote_trust_record_json(&record).to_string())
            }
            _ => Err(MezError::not_implemented(format!(
                "unknown remote administration method `{}`",
                request.method
            ))),
        }?;
        self.append_runtime_remote_administration_audit(request, connection, &result)?;
        if let Some(cache_key) = cache_key {
            self.control.idempotency_mut().remember_response(
                cache_key,
                request.method.clone(),
                request.params.clone(),
                result.clone(),
            );
        }
        Ok(result)
    }

    /// Appends one secret-safe audit record for a local trust mutation.
    fn append_runtime_remote_administration_audit(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
        result: &str,
    ) -> Result<()> {
        let Some(action) = request.method.strip_prefix("remote/") else {
            return Ok(());
        };
        let action = match action {
            "invite" => "invite_created",
            "client/rename" => "client_renamed",
            "client/revoke" => "client_revoked",
            _ => return Ok(()),
        };
        let caller_client_id = connection.caller_client_id().ok_or_else(|| {
            MezError::forbidden("remote administration has no initialized local client")
        })?;
        let value: serde_json::Value = serde_json::from_str(result).map_err(|error| {
            MezError::invalid_state(format!("invalid remote administration result: {error}"))
        })?;
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "client".to_string(),
                id: caller_client_id.to_string(),
            },
            "remote_trust",
            action,
        );
        record.outcome = "succeeded".to_string();
        if let Some(invitation_id) = value
            .get("invitation_id")
            .and_then(serde_json::Value::as_str)
        {
            record = record.with_metadata("invitation_id", invitation_id);
        }
        if let Some(client_id) = value.get("id").and_then(serde_json::Value::as_str) {
            record = record.with_metadata("client_id", client_id);
        }
        if let Some(role) = value.get("role").and_then(serde_json::Value::as_str) {
            record = record.with_metadata("role", role);
        }
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }
}

fn remote_initialize_mechanism_name(request: &JsonRpcRequest) -> &'static str {
    let Ok(params) = remote_params(request) else {
        return "unknown";
    };
    match params
        .get("authentication")
        .and_then(serde_json::Value::as_object)
        .and_then(|authentication| authentication.get("mechanism"))
        .and_then(serde_json::Value::as_str)
    {
        Some("extension:iroh_invitation") => "invitation",
        Some("extension:iroh_device") => "device",
        _ => "unknown",
    }
}

fn requested_role_name(role: RequestedRole) -> &'static str {
    match role {
        RequestedRole::Primary => "primary",
        RequestedRole::Observer => "observer",
        RequestedRole::Agent => "agent",
        RequestedRole::Automation => "automation",
    }
}

fn remote_administration_mutates(method: &str) -> bool {
    matches!(
        method,
        "remote/invite" | "remote/client/rename" | "remote/client/revoke"
    )
}

fn remote_params(request: &JsonRpcRequest) -> Result<serde_json::Map<String, serde_json::Value>> {
    let params = request
        .params
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("remote method requires a params object"))?;
    serde_json::from_str::<serde_json::Value>(params)
        .map_err(|error| MezError::invalid_args(format!("invalid remote params: {error}")))?
        .as_object()
        .cloned()
        .ok_or_else(|| MezError::invalid_args("remote params must be an object"))
}

fn required_remote_string<'a>(
    params: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str> {
    params
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| MezError::invalid_args(format!("remote method requires {field}")))
}

fn remote_trust_record_json(record: &RemoteTrustRecord) -> serde_json::Value {
    serde_json::json!({
        "id": record.id,
        "endpoint_id": record.endpoint_id,
        "label": record.label,
        "role": record.role_ceiling.as_str(),
        "created_at_unix_seconds": record.created_at_unix_seconds,
        "last_used_at_unix_seconds": record.last_used_at_unix_seconds,
        "revoked_at_unix_seconds": record.revoked_at_unix_seconds,
        "revocation_reason": record.revocation_reason,
        "credential_version": record.credential_version,
    })
}
