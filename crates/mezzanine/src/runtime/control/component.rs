//! Runtime control protocol, messaging, and event-log state ownership.
//!
//! This component owns replay/idempotency state and the canonical message and
//! lifecycle-event services used by control clients and observer fanout.
//! Disposable external registrations are separate from original native source
//! anchors and the single accounting projection/readiness owners. Losing only
//! registrations cannot reset expense baselines or accounting reservations;
//! reconstructing this whole component invalidates all live activation authority.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::control::ControlIdempotencyCache;
use crate::protocol::event::EventLog;
use base64::Engine;
use mez_agent::messaging::MessageService;
use mez_core::ids::ClientId;
use rand::Rng;
use sha2::{Digest, Sha256};

const UNIX_EVENT_BINDING_TOKEN_BYTES: usize = 32;
const UNIX_EVENT_BINDING_TTL_SECONDS: u64 = 60;

/// Owns control replay, messaging, and event-fanout state.
#[derive(Debug)]
pub(crate) struct RuntimeControlComponent {
    idempotency: ControlIdempotencyCache,
    message_service: MessageService,
    event_log: Option<EventLog>,
    approval_bindings: BTreeMap<String, ApprovalBinding>,
    unix_event_bindings: BTreeMap<[u8; 32], UnixEventBinding>,
    external_agents: super::external_agents::ExternalAgentRegistry,
    /// Same-service native authority survives disposable registration loss only.
    external_sources: super::external_enrollment::LiveSourceAnchors,
    /// Single accounting owner survives registration loss; no shadow expense.
    external_usage: super::external_usage::ExternalUsageProjection,
    /// Original readiness reservations/cache, distinct from lifecycle credentials.
    external_usage_begins: super::external_usage_requests::ExternalUsageBegins,
}

/// Exact-client authority retained for one short-lived Unix event handshake.
///
/// The map key is a SHA-256 digest of the bearer token so runtime debug output
/// and diagnostics never retain the raw credential.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnixEventBinding {
    client_id: ClientId,
    peer_uid: u32,
    expires_at_unix_seconds: u64,
}

/// Runtime-owned facts captured when a pending approval is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApprovalBinding {
    /// Project root derived from the pane working directory at prompt time.
    pub(crate) project_root: PathBuf,
    /// Pane working directory observed at prompt time.
    pub(crate) working_directory: PathBuf,
    /// Exact command digest derived from the prompted action summary.
    pub(crate) command_sha256: String,
}

impl RuntimeControlComponent {
    /// Builds control ownership from constructor-provided services.
    pub(crate) fn new(
        idempotency: ControlIdempotencyCache,
        mut message_service: MessageService,
        event_log: Option<EventLog>,
    ) -> Self {
        // External launches are runtime-only. Imported MMP state cannot revive
        // an identity without the exact capability owner that registered it.
        for identity in message_service.discover_agents_filtered_session_wide(
            None,
            None,
            None,
            Some("external-harness"),
            None,
            &[],
        ) {
            message_service.retire_observational_identity(&identity.agent_id);
        }
        let external_agents = super::external_agents::ExternalAgentRegistry::default();
        let external_sources = super::external_enrollment::LiveSourceAnchors::new(
            external_agents.enrollments.ancestry_budget.clone(),
        );
        Self {
            idempotency,
            message_service,
            event_log,
            approval_bindings: BTreeMap::new(),
            unix_event_bindings: BTreeMap::new(),
            external_agents,
            external_sources,
            external_usage: Default::default(),
            external_usage_begins: Default::default(),
        }
    }

    /// Returns bounded external launch and registration state.
    pub(super) fn external_agents(&self) -> &super::external_agents::ExternalAgentRegistry {
        &self.external_agents
    }

    /// Returns actor-owned external lifecycle state for mutation.
    pub(super) fn external_agents_mut(
        &mut self,
    ) -> &mut super::external_agents::ExternalAgentRegistry {
        &mut self.external_agents
    }

    /// Returns memory-only source authority, never reconstructed from storage.
    pub(super) fn external_sources(&self) -> &super::external_enrollment::LiveSourceAnchors {
        &self.external_sources
    }

    /// Returns the actor-serialized native source/observer fencing owner.
    pub(super) fn external_sources_mut(
        &mut self,
    ) -> &mut super::external_enrollment::LiveSourceAnchors {
        &mut self.external_sources
    }

    /// Returns the single retained checkpoint/reset-baseline/reservation owner.
    pub(super) fn external_usage(&self) -> &super::external_usage::ExternalUsageProjection {
        &self.external_usage
    }

    /// Mutates accounting independently of disposable registration credentials.
    pub(super) fn external_usage_mut(
        &mut self,
    ) -> &mut super::external_usage::ExternalUsageProjection {
        &mut self.external_usage
    }

    /// Returns original readiness metadata and its exact pending owner.
    pub(super) fn external_usage_begins(
        &self,
    ) -> &super::external_usage_requests::ExternalUsageBegins {
        &self.external_usage_begins
    }

    /// Mutates bounded readiness without recreating reservation generations.
    pub(super) fn external_usage_begins_mut(
        &mut self,
    ) -> &mut super::external_usage_requests::ExternalUsageBegins {
        &mut self.external_usage_begins
    }

    /// Mints one short-lived Unix event binding for an initialized client.
    ///
    /// The returned token is the only raw copy retained by the caller. Runtime
    /// state stores only its digest and exact client/peer binding.
    pub(crate) fn mint_unix_event_binding(
        &mut self,
        client_id: ClientId,
        peer_uid: u32,
        now_unix_seconds: u64,
    ) -> (String, u64) {
        self.unix_event_bindings
            .retain(|_, binding| binding.expires_at_unix_seconds >= now_unix_seconds);
        let mut token_bytes = [0u8; UNIX_EVENT_BINDING_TOKEN_BYTES];
        rand::rng().fill_bytes(&mut token_bytes);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_bytes);
        let digest = unix_event_binding_digest(&token);
        let expires_at_unix_seconds =
            now_unix_seconds.saturating_add(UNIX_EVENT_BINDING_TTL_SECONDS);
        self.unix_event_bindings.insert(
            digest,
            UnixEventBinding {
                client_id,
                peer_uid,
                expires_at_unix_seconds,
            },
        );
        (token, expires_at_unix_seconds)
    }

    /// Consumes one matching Unix event binding exactly once.
    ///
    /// Unknown, expired, and wrong-peer credentials share one redacted error
    /// at the service boundary; this owner never returns token material.
    pub(crate) fn consume_unix_event_binding(
        &mut self,
        token: &str,
        peer_uid: u32,
        now_unix_seconds: u64,
    ) -> Option<ClientId> {
        let digest = unix_event_binding_digest(token);
        let binding = self.unix_event_bindings.get(&digest)?;
        if binding.peer_uid != peer_uid || binding.expires_at_unix_seconds < now_unix_seconds {
            return None;
        }
        self.unix_event_bindings
            .remove(&digest)
            .map(|binding| binding.client_id)
    }

    /// Removes every unconsumed event binding owned by one detached client.
    pub(crate) fn remove_unix_event_bindings_for_client(&mut self, client_id: &ClientId) {
        self.unix_event_bindings
            .retain(|_, binding| &binding.client_id != client_id);
    }

    /// Returns the idempotency cache for read-only diagnostics.
    #[cfg(test)]
    pub(crate) fn idempotency(&self) -> &ControlIdempotencyCache {
        &self.idempotency
    }

    /// Returns the idempotency cache for request dispatch mutation.
    pub(crate) fn idempotency_mut(&mut self) -> &mut ControlIdempotencyCache {
        &mut self.idempotency
    }

    /// Returns the canonical message service.
    pub(crate) fn message_service(&self) -> &MessageService {
        &self.message_service
    }

    /// Returns the canonical message service for queue and presence mutation.
    pub(crate) fn message_service_mut(&mut self) -> &mut MessageService {
        &mut self.message_service
    }

    /// Returns the optional lifecycle event log.
    pub(crate) fn event_log(&self) -> Option<&EventLog> {
        self.event_log.as_ref()
    }

    /// Returns the lifecycle event log for append operations.
    pub(crate) fn event_log_mut(&mut self) -> Option<&mut EventLog> {
        self.event_log.as_mut()
    }

    /// Replaces the lifecycle event log after a staged control transaction.
    pub(crate) fn replace_event_log(&mut self, event_log: Option<EventLog>) {
        self.event_log = event_log;
    }

    /// Retains runtime-owned facts observed when an approval was queued.
    pub(crate) fn insert_approval_binding(
        &mut self,
        approval_id: String,
        project_root: PathBuf,
        working_directory: PathBuf,
        command_sha256: String,
    ) {
        self.approval_bindings.insert(
            approval_id,
            ApprovalBinding {
                project_root,
                working_directory,
                command_sha256,
            },
        );
    }

    /// Returns runtime-owned facts captured when an approval was queued.
    pub(crate) fn approval_binding(&self, approval_id: &str) -> Option<&ApprovalBinding> {
        self.approval_bindings.get(approval_id)
    }

    /// Releases the runtime-owned binding after its approval settles.
    pub(crate) fn remove_approval_binding(&mut self, approval_id: &str) {
        self.approval_bindings.remove(approval_id);
    }

    /// Clears runtime-only approval bindings during session replacement.
    pub(crate) fn clear_approval_bindings(&mut self) {
        self.approval_bindings.clear();
    }
}

fn unix_event_binding_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_id(value: &str) -> ClientId {
        ClientId::parse('c', value.to_string()).unwrap()
    }

    #[test]
    fn unix_event_bindings_expire_and_are_removed_with_their_client() {
        let mut control = RuntimeControlComponent::new(
            ControlIdempotencyCache::default(),
            MessageService::default(),
            None,
        );
        let expiring_client = client_id("c1");
        let detached_client = client_id("c2");
        let peer_uid = 1000;

        let (expired_token, expires_at) =
            control.mint_unix_event_binding(expiring_client, peer_uid, 100);
        assert_eq!(expires_at, 160);
        assert_eq!(
            control.consume_unix_event_binding(&expired_token, peer_uid, 161),
            None
        );

        let (detached_token, _) =
            control.mint_unix_event_binding(detached_client.clone(), peer_uid, 200);
        control.remove_unix_event_bindings_for_client(&detached_client);
        assert_eq!(
            control.consume_unix_event_binding(&detached_token, peer_uid, 200),
            None
        );
    }
}
