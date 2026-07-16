//! Repository handles, transcript sequences, and durable pane references.

use crate::security::audit::AuditLog;
use crate::storage::registry::SessionRegistry;
use crate::storage::snapshot::SnapshotRepository;
use crate::storage::transcript::AgentTranscriptStore;

use super::RuntimePersistenceComponent;

impl RuntimePersistenceComponent {
    /// Returns the attached security audit writer.
    pub(crate) fn audit_log(&self) -> Option<&AuditLog> {
        self.audit_log.as_ref()
    }

    /// Returns mutable access to the attached security audit writer.
    pub(crate) fn audit_log_mut(&mut self) -> Option<&mut AuditLog> {
        self.audit_log.as_mut()
    }

    /// Replaces the attached security audit writer.
    pub(crate) fn set_audit_log(&mut self, audit_log: AuditLog) {
        self.audit_log = Some(audit_log);
    }

    /// Removes the attached security audit writer.
    pub(crate) fn clear_audit_log(&mut self) {
        self.audit_log = None;
    }

    /// Clones the configured snapshot repository handle.
    pub(crate) fn cloned_snapshot_repository(&self) -> Option<SnapshotRepository> {
        self.snapshot_repository.clone()
    }

    /// Attaches the configured snapshot repository.
    pub(crate) fn set_snapshot_repository(&mut self, repository: SnapshotRepository) {
        self.snapshot_repository = Some(repository);
    }

    /// Returns the attached agent transcript store.
    pub(crate) fn transcript_store(&self) -> Option<&AgentTranscriptStore> {
        self.agent_transcript_store.as_ref()
    }

    /// Returns mutable access to the attached agent transcript store.
    pub(crate) fn transcript_store_mut(&mut self) -> Option<&mut AgentTranscriptStore> {
        self.agent_transcript_store.as_mut()
    }

    /// Attaches the agent transcript store.
    pub(crate) fn set_transcript_store(&mut self, store: AgentTranscriptStore) {
        self.agent_transcript_store = Some(store);
    }

    /// Clones the attached agent transcript store handle.
    pub(crate) fn cloned_transcript_store(&self) -> Option<AgentTranscriptStore> {
        self.agent_transcript_store.clone()
    }

    /// Returns the attached live-session registry.
    pub(crate) fn session_registry(&self) -> Option<&SessionRegistry> {
        self.session_registry.as_ref()
    }

    /// Attaches the live-session registry.
    pub(crate) fn set_session_registry(&mut self, registry: SessionRegistry) {
        self.session_registry = Some(registry);
    }

    /// Clones the attached live-session registry handle.
    pub(crate) fn cloned_session_registry(&self) -> Option<SessionRegistry> {
        self.session_registry.clone()
    }

    /// Returns a reserved next transcript sequence.
    pub(crate) fn deferred_transcript_next_sequence(&self, conversation_id: &str) -> Option<u64> {
        self.deferred_transcript_next_sequences
            .get(conversation_id)
            .copied()
    }

    /// Reserves the next transcript sequence after queued writes.
    pub(crate) fn set_deferred_transcript_next_sequence(
        &mut self,
        conversation_id: impl Into<String>,
        sequence: u64,
    ) {
        self.deferred_transcript_next_sequences
            .insert(conversation_id.into(), sequence);
    }

    /// Records one unique durable transcript reference for a pane.
    pub(crate) fn record_pane_transcript_ref(
        &mut self,
        pane_id: impl Into<String>,
        transcript_ref: String,
    ) {
        let refs = self.pane_transcript_refs.entry(pane_id.into()).or_default();
        if !refs.contains(&transcript_ref) {
            refs.push(transcript_ref);
        }
    }

    /// Returns durable transcript references for one pane.
    pub(crate) fn pane_transcript_refs(&self, pane_id: &str) -> Vec<String> {
        self.pane_transcript_refs
            .get(pane_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Removes durable transcript references for one pane.
    pub(crate) fn remove_pane_transcript_refs(&mut self, pane_id: &str) {
        self.pane_transcript_refs.remove(pane_id);
    }

    /// Clears all durable transcript references on session replacement.
    pub(crate) fn clear_pane_transcript_refs(&mut self) {
        self.pane_transcript_refs.clear();
    }
}
