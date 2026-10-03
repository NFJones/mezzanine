//! Restricted, launch-bound external harness registrations.
//!
//! An attached primary explicitly issues a capability for an exact local pane
//! root. Hooks present that capability over authenticated Unix transport without
//! becoming a primary, agent or automation client. Only digests are retained.
//! Registrations are observational: they cannot write input, approve actions or
//! resume native tasks. Lease expiry means unavailable telemetry, not death.
//! Runtime restart invalidates capabilities; no credential is persisted.

use std::collections::BTreeMap;

use base64::Engine;
use mez_agent::messaging::{AgentPresenceStatus, ProjectScopeId};
use mez_core::ids::{AgentId, ClientId, PaneId};
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::control::{AuthenticatedPeer, ControlConnectionState, JsonRpcRequest};
use crate::error::{MezError, Result};
use crate::runtime::processes::RuntimePaneProcessIdentity;
use crate::runtime::{RuntimeSessionService, current_unix_seconds};

const MAX_BINDINGS: usize = 256;
const LAUNCH_SECONDS: u64 = 120;
const LEASE_SECONDS: u64 = 60;
const TOMBSTONE_SECONDS: u64 = 300;

/// Bounded actor-owned credentials and live registrations, never raw tokens.
#[derive(Debug, Default)]
pub(super) struct ExternalAgentRegistry {
    bindings: BTreeMap<[u8; 32], LaunchBinding>,
    next_generation: u64,
}

/// Exact local launch authority and optional registration settlement.
#[derive(Debug)]
struct LaunchBinding {
    uid: u32,
    pane_id: String,
    process: RuntimePaneProcessIdentity,
    project_scope: Option<ProjectScopeId>,
    generation: u64,
    harness: String,
    version: String,
    expires: u64,
    registration: Option<Registration>,
    retired: bool,
}

/// Immutable registration metadata, bound to a single server-issued launch.
#[derive(Debug)]
struct Registration {
    agent_id: AgentId,
    external_session_id: String,
    display_name: String,
    objective: Option<String>,
}

/// Parses bounded inert metadata without echoing rejected payloads.
fn text(params: &serde_json::Value, key: &str, max: usize) -> Result<String> {
    let value = params
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            !value.trim().is_empty()
                && value.len() <= max
                && !value.chars().any(|ch| {
                    ch.is_control()
                        || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
        })
        .ok_or_else(|| {
            MezError::invalid_args(format!("external agent {key} must be bounded inert text"))
        })?;
    Ok(value.to_string())
}

/// Hashes only well-shaped credential input; diagnostics never include it.
fn credential(params: &serde_json::Value) -> Result<[u8; 32]> {
    let token = params
        .get("launch_token")
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            value.len() == 43
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .ok_or_else(|| MezError::forbidden("external launch capability unavailable"))?;
    Ok(Sha256::digest(token.as_bytes()).into())
}

impl RuntimeSessionService {
    /// Issues a short-lived capability only to an attached primary. The launcher
    /// must pass the returned credential privately, never in argv or logs.
    pub(super) fn issue_external_agent_launch(
        &mut self,
        client: &ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        if !self.session.is_attached_primary(client) {
            return Err(MezError::forbidden(
                "external launch issuance requires an attached primary",
            ));
        }
        let params: serde_json::Value = serde_json::from_str(params)
            .map_err(|_| MezError::invalid_args("external launch params must be an object"))?;
        let pane_id = text(&params, "pane_id", 64)?;
        let harness = text(&params, "harness", 64)?;
        if !harness.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        }) {
            return Err(MezError::invalid_args(
                "external harness must be a lowercase identifier",
            ));
        }
        let version = text(&params, "version", 128)?;
        self.reconcile_external_agent_registrations();
        let process = self
            .pane_process_identity(&pane_id)
            .map_err(|_| MezError::conflict("external launch pane root unavailable"))?;
        if process.role != crate::runtime::processes::RuntimePaneProcessRole::AdapterOwnedRoot {
            return Err(MezError::conflict("external launch pane root unavailable"));
        }
        let project_scope = self
            .trusted_project_root_for_pane(&pane_id)
            .map(mez_agent::messaging::ProjectMembership::from_canonical_root)
            .map(|membership| membership.scope_id());
        let registry = self.control.external_agents_mut();
        if registry.bindings.len() >= MAX_BINDINGS {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external launch registry is full",
            ));
        }
        let generation = registry
            .next_generation
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("external launch generation exhausted"))?;
        registry.next_generation = generation;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let digest = Sha256::digest(token.as_bytes()).into();
        let expires = current_unix_seconds().saturating_add(LAUNCH_SECONDS);
        registry.bindings.insert(
            digest,
            LaunchBinding {
                uid: crate::runtime::current_effective_uid(),
                pane_id,
                process,
                project_scope,
                generation,
                harness,
                version,
                expires,
                registration: None,
                retired: false,
            },
        );
        Ok(
            serde_json::json!({"protocol":"external-agent/1", "launch_token":token,
            "generation":generation, "expires_at_unix_seconds":expires,
            "lease_seconds":LEASE_SECONDS})
            .to_string(),
        )
    }

    /// Handles capability-only hook traffic before ordinary client initialization.
    /// Initialized clients and remote peers cannot use this restricted ingress.
    pub(super) fn dispatch_external_agent_request(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<String> {
        self.require_live()?;
        if connection.initialized() {
            return Err(MezError::forbidden(
                "external hooks must use a capability-only connection",
            ));
        }
        let Some(AuthenticatedPeer::UnixUser { uid }) = connection.authenticated_peer() else {
            return Err(MezError::forbidden(
                "external hooks require authenticated Unix transport",
            ));
        };
        crate::control::validate_control_method_params_schema(request)?;
        let params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap_or("{}"))
                .map_err(|_| MezError::invalid_args("external agent params must be an object"))?;
        let digest = credential(&params)?;
        self.reconcile_external_agent_registrations();
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::forbidden("external launch capability unavailable"))?;
        if binding.uid != *uid
            || params.get("generation").and_then(serde_json::Value::as_u64)
                != Some(binding.generation)
        {
            return Err(MezError::forbidden(
                "external launch capability unavailable",
            ));
        }
        if binding.retired {
            let session = text(&params, "external_session_id", 128)?;
            if binding
                .registration
                .as_ref()
                .is_none_or(|registration| registration.external_session_id != session)
            {
                return Err(MezError::conflict("external registration unavailable"));
            }
            if request.method == "agent/external/deregister" {
                return Ok(serde_json::json!({"retired":true,"changed":false,"generation":binding.generation}).to_string());
            }
            return Err(MezError::conflict(
                "external launch has retired; request a new launch",
            ));
        }
        match request.method.as_str() {
            "agent/external/register" => self.register_external_agent(digest, &params),
            "agent/external/renew" => {
                let external_session_id = text(&params, "external_session_id", 128)?;
                let binding = self
                    .control
                    .external_agents_mut()
                    .bindings
                    .get_mut(&digest)
                    .ok_or_else(|| MezError::forbidden("external launch capability unavailable"))?;
                let registration = binding
                    .registration
                    .as_ref()
                    .filter(|registration| registration.external_session_id == external_session_id)
                    .ok_or_else(|| MezError::conflict("external registration unavailable"))?;
                binding.expires = current_unix_seconds().saturating_add(LEASE_SECONDS);
                Ok(serde_json::json!({"agent_id":registration.agent_id.as_str(),"generation":binding.generation,
                    "expires_at_unix_seconds":binding.expires}).to_string())
            }
            "agent/external/deregister" => {
                let external_session_id = text(&params, "external_session_id", 128)?;
                if binding.registration.as_ref().is_none_or(|registration| {
                    registration.external_session_id != external_session_id
                }) {
                    return Err(MezError::conflict("external registration unavailable"));
                }
                self.retire_external_agent_binding(digest);
                Ok(serde_json::json!({"retired":true,"changed":true}).to_string())
            }
            _ => Err(MezError::invalid_args(
                "unsupported external agent operation",
            )),
        }
    }

    /// Registers immutable bounded metadata once; identical reply-loss retries
    /// return the original identity, while conflicting reuse fails closed.
    fn register_external_agent(
        &mut self,
        digest: [u8; 32],
        params: &serde_json::Value,
    ) -> Result<String> {
        let external_session_id = text(params, "external_session_id", 128)?;
        let display_name = text(params, "display_name", 128)?;
        let objective = match params.get("objective") {
            None | Some(serde_json::Value::Null) => None,
            _ => Some(text(params, "objective", 512)?),
        };
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::forbidden("external launch capability unavailable"))?;
        if let Some(registration) = &binding.registration {
            if registration.external_session_id != external_session_id
                || registration.display_name != display_name
                || registration.objective != objective
            {
                return Err(MezError::conflict(
                    "external registration metadata differs from launch settlement",
                ));
            }
            return Ok(serde_json::json!({"agent_id":registration.agent_id.as_str(),"generation":binding.generation,
                "expires_at_unix_seconds":binding.expires,"registered":true,"controls":[]}).to_string());
        }
        let pane_id = binding.pane_id.clone();
        let scope = binding.project_scope.clone();
        let descriptor = self
            .find_pane_descriptor(&pane_id)
            .ok_or_else(|| MezError::conflict("external launch pane unavailable"))?;
        let identity = self
            .control
            .message_service_mut()
            .register_agent_with_objective(
                PaneId::opaque(pane_id),
                Some(descriptor.window_id),
                "external-harness",
                vec!["external-harness".to_string(), "observational".to_string()],
                objective.as_deref(),
            )?;
        self.control
            .message_service_mut()
            .rebind_agent_project_scope(&identity.agent_id, scope)?;
        self.control.message_service_mut().update_presence(
            &identity.agent_id,
            AgentPresenceStatus::Available,
            current_unix_seconds().saturating_mul(1000),
        )?;
        let binding = self
            .control
            .external_agents_mut()
            .bindings
            .get_mut(&digest)
            .ok_or_else(|| MezError::invalid_state("external launch owner disappeared"))?;
        binding.expires = current_unix_seconds().saturating_add(LEASE_SECONDS);
        let response = serde_json::json!({"agent_id":identity.agent_id.as_str(),"generation":binding.generation,
            "expires_at_unix_seconds":binding.expires,"registered":true,"controls":[]}).to_string();
        binding.registration = Some(Registration {
            agent_id: identity.agent_id,
            external_session_id,
            display_name,
            objective,
        });
        Ok(response)
    }

    /// Retires only one exact registration and retains a bounded retry tombstone.
    fn retire_external_agent_binding(&mut self, digest: [u8; 32]) {
        let Some(binding) = self.control.external_agents_mut().bindings.get_mut(&digest) else {
            return;
        };
        if binding.retired {
            return;
        }
        binding.retired = true;
        binding.expires = current_unix_seconds().saturating_add(TOMBSTONE_SECONDS);
        let agent_id = binding
            .registration
            .as_ref()
            .map(|registration| registration.agent_id.clone());
        if let Some(agent_id) = agent_id {
            self.control
                .message_service_mut()
                .retire_observational_identity(&agent_id);
        }
    }

    /// Expires missing renewal or replaced pane roots without claiming death.
    pub(crate) fn reconcile_external_agent_registrations(&mut self) -> usize {
        let now = current_unix_seconds();
        let retired = self
            .control
            .external_agents()
            .bindings
            .iter()
            .filter_map(|(digest, binding)| {
                (!binding.retired
                    && (binding.expires <= now
                        || self.find_pane_descriptor(&binding.pane_id).is_none()
                        || !self
                            .pane_process_identity_is_current(&binding.pane_id, &binding.process)))
                .then_some(*digest)
            })
            .collect::<Vec<_>>();
        for digest in &retired {
            self.retire_external_agent_binding(*digest);
        }
        self.control
            .external_agents_mut()
            .bindings
            .retain(|_, binding| !binding.retired || binding.expires > now);
        retired.len()
    }

    /// Keeps ordinary bounded idle cleanup active while a live lease can expire.
    pub(crate) fn external_agent_cleanup_needed(&self) -> bool {
        !self.control.external_agents().bindings.is_empty()
    }

    /// Retires runtime-only external identities after snapshot replacement.
    /// No capability survives restart, so restored metadata cannot revive a run.
    pub(crate) fn retire_unbound_external_message_identities(&mut self) {
        let identities = self
            .control
            .message_service()
            .discover_agents_filtered_session_wide(
                None,
                None,
                None,
                Some("external-harness"),
                None,
                &[],
            );
        for identity in identities {
            self.control
                .message_service_mut()
                .retire_observational_identity(&identity.agent_id);
        }
        self.control.external_agents_mut().bindings.clear();
    }

    /// Returns current metadata for one registered external identity.
    pub(crate) fn external_agent_metadata(&self, agent_id: &str) -> Option<serde_json::Value> {
        self.external_agent_rows()
            .into_iter()
            .find(|row| row["agent_id"] == agent_id)
    }

    /// Advances an exact lease deadline deterministically in lifecycle tests.
    #[cfg(test)]
    pub(crate) fn expire_external_agent_for_tests(&mut self, agent_id: &str) {
        for binding in self.control.external_agents_mut().bindings.values_mut() {
            if binding
                .registration
                .as_ref()
                .is_some_and(|registration| registration.agent_id.as_str() == agent_id)
            {
                binding.expires = 0;
            }
        }
    }

    /// Supplies administrative metadata independently of native shell sessions.
    pub(crate) fn external_agent_rows(&self) -> Vec<serde_json::Value> {
        let now = current_unix_seconds();
        self.control.external_agents().bindings.values().filter_map(|binding| {
            let registration = binding.registration.as_ref()?;
            if binding.retired || binding.expires <= now || !self.pane_process_identity_is_current(&binding.pane_id, &binding.process) { return None; }
            let descriptor = self.find_pane_descriptor(&binding.pane_id)?;
            Some(serde_json::json!({"agent_id":registration.agent_id.as_str(),"pane_id":binding.pane_id,
                "window_id":descriptor.window_id.as_str(),"kind":"primary","harness":binding.harness,
                "harness_version":binding.version,"display_name":registration.display_name,
                "objective":registration.objective,"generation":binding.generation,
                "external_session_id":registration.external_session_id,"status":"available",
                "presence_source":"renewable-telemetry-lease","controls":[],"native":false}))
        }).collect()
    }

    /// Joins external rows to an already-authorized native control response.
    pub(super) fn append_external_agent_list_rows(&self, response: String) -> String {
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&response) else {
            return response;
        };
        if let Some(rows) = value
            .get_mut("result")
            .and_then(|result| result.get_mut("agents"))
            .and_then(serde_json::Value::as_array_mut)
        {
            rows.extend(self.external_agent_rows());
        }
        value.to_string()
    }
}
