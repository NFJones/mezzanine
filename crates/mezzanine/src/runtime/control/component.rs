//! Runtime control protocol, messaging, and event-log state ownership.
//!
//! This component owns replay/idempotency state and the canonical message and
//! lifecycle-event services used by control clients and observer fanout.

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
        message_service: MessageService,
        event_log: Option<EventLog>,
    ) -> Self {
        Self {
            idempotency,
            message_service,
            event_log,
            approval_bindings: BTreeMap::new(),
            unix_event_bindings: BTreeMap::new(),
        }
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
