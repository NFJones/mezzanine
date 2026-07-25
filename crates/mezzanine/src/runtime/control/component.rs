//! Runtime control protocol, messaging, and event-log state ownership.
//!
//! This component owns replay/idempotency state and the canonical message and
//! lifecycle-event services used by control clients and observer fanout.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::control::ControlIdempotencyCache;
use crate::protocol::event::EventLog;
use crate::runtime::SandboxToolchainKind;
use mez_agent::messaging::MessageService;

/// Owns control replay, messaging, and event-fanout state.
#[derive(Debug)]
pub(crate) struct RuntimeControlComponent {
    idempotency: ControlIdempotencyCache,
    message_service: MessageService,
    event_log: Option<EventLog>,
    approval_bindings: BTreeMap<String, ApprovalBinding>,
    pending_toolchain_mutations: BTreeMap<String, PendingToolchainMutation>,
}

/// Runtime-owned external toolchain mutation awaiting direct primary input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingToolchainMutation {
    /// Opaque request identifier shown to the primary client.
    pub(crate) id: String,
    /// Canonical operation submitted by the external CLI.
    pub(crate) operation: String,
    /// Allowlisted toolchain kind selected by the request.
    pub(crate) kind: SandboxToolchainKind,
    /// Digest of the normalized operation and kind.
    pub(crate) digest: String,
    /// Config generation captured when the request was submitted.
    pub(crate) config_generation: u64,
    /// Active pane whose primary UI must settle the request.
    pub(crate) pane_id: String,
    /// Authenticated control client that submitted the request.
    pub(crate) submitted_by_client_id: String,
    /// Wall-clock expiry used to reject stale confirmations.
    pub(crate) expires_at_unix_seconds: u64,
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
            pending_toolchain_mutations: BTreeMap::new(),
        }
    }

    /// Stores one normalized external toolchain mutation request.
    pub(crate) fn insert_pending_toolchain_mutation(&mut self, request: PendingToolchainMutation) {
        self.pending_toolchain_mutations
            .insert(request.id.clone(), request);
    }

    /// Returns one pending toolchain mutation without settling it.
    pub(crate) fn pending_toolchain_mutation(
        &self,
        request_id: &str,
    ) -> Option<&PendingToolchainMutation> {
        self.pending_toolchain_mutations.get(request_id)
    }

    /// Removes one pending toolchain mutation after a terminal decision.
    pub(crate) fn remove_pending_toolchain_mutation(
        &mut self,
        request_id: &str,
    ) -> Option<PendingToolchainMutation> {
        self.pending_toolchain_mutations.remove(request_id)
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
        self.pending_toolchain_mutations.clear();
    }
}
