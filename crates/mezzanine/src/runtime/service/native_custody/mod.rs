//! Private current-session evidence for original-root native custody recovery.
//!
//! This path hydrates actual private transcripts with fresh provisional epochs,
//! retaining the old active checkpoint unchanged. Only original-store-qualified
//! opaque results may replace those exact provisional epochs before publication.
//! Public labels or generic restored UUID bytes cannot construct this owner.

use super::*;
use crate::storage::transcript::NativeMetadataOrigin;
use mez_agent::messaging::{
    MessageNativeContinuitySnapshot, MessageNativeIncarnation, MessageRecoveredNativeBinding,
    ProjectScopeId,
};

/// One actual private context with a provisional same-runtime epoch fence.
struct Binding {
    agent: String,
    pane: String,
    conversation: String,
    window: String,
    project: Option<ProjectScopeId>,
    original: [u8; 16],
    provisional: MessageNativeIncarnation,
}

/// Nonserializable original private source and actual staged shell observations.
pub(crate) struct PreparedNativeCustodyContext {
    session: String,
    origin: NativeMetadataOrigin,
    bindings: Vec<Binding>,
}
impl PreparedNativeCustodyContext {
    /// Includes original checkpoint/decoded projection headroom in one budget.
    pub(crate) fn metadata_bytes(&self) -> usize {
        self.origin.metadata_bytes()
    }
    /// Revalidates the retained private original through cancellable startup.
    pub(crate) fn revalidate(&self) -> Result<()> {
        self.origin.revalidate()
    }
    /// Borrows only requested native endpoints, never raw socket registrations.
    pub(crate) fn agents(&self) -> impl Iterator<Item = &str> {
        self.bindings.iter().map(|b| b.agent.as_str())
    }
    /// Matches immutable identity, original epoch and actual current pane/context.
    /// Missing legacy evidence, sparse/conflicting fields and external harnesses
    /// stay unavailable; saved availability flags cannot open the endpoint.
    pub(crate) fn matches(&self, fact: &MessageNativeContinuitySnapshot) -> bool {
        self.bindings.iter().any(|b| {
            fact.native.agent_id == b.agent
                && fact.native.shell_incarnation == b.original
                && fact.native.owner.conversation_id() == b.conversation
                && fact.identity.pane_id.as_deref() == Some(b.pane.as_str())
                && fact.identity.window_id.as_deref() == Some(b.window.as_str())
                && fact.matches_project_scope(&b.project)
                && fact.identity.role.is_some()
                && fact
                    .identity
                    .capabilities
                    .iter()
                    .any(|c| c == "agent-harness")
                && !fact
                    .identity
                    .capabilities
                    .iter()
                    .any(|c| c == "external-harness")
        })
    }
    /// Checks exact provisional runtime allocations before consuming any result.
    /// Foreign/stale adopters cannot install an original epoch by matching labels.
    pub(crate) fn validate_runtime(&self, runtime: &RuntimeSessionService) -> Result<()> {
        if runtime.session().id.as_str() != self.session {
            return Err(changed());
        }
        for b in &self.bindings {
            let shell = runtime
                .agent_shell_store()
                .get(&b.pane)
                .ok_or_else(changed)?;
            let pane = runtime.find_pane_descriptor(&b.pane).ok_or_else(changed)?;
            if shell.ephemeral
                || shell.session_id != b.conversation
                || shell.native_message_incarnation != b.provisional
                || pane.window_id.as_str() != b.window
                || shell.project_scope.as_ref().map(|p| p.scope_id()) != b.project
            {
                return Err(changed());
            }
        }
        Ok(())
    }
    /// Installs matching qualified epochs and checkpoints under the original private
    /// source fence. The caller still withholds all external services.
    pub(crate) fn install(
        &self,
        runtime: &mut RuntimeSessionService,
        bindings: &[MessageRecoveredNativeBinding],
    ) -> Result<()> {
        self.origin.revalidate()?;
        self.validate_runtime(runtime)?;
        for binding in bindings {
            if !self.bindings.iter().any(|b| {
                b.agent == binding.agent_id().as_str()
                    && b.original == binding.incarnation().snapshot_identity()
            }) {
                return Err(changed());
            }
        }
        for binding in bindings {
            let b = self
                .bindings
                .iter()
                .find(|b| b.agent == binding.agent_id().as_str())
                .ok_or_else(changed)?;
            let mut shell = runtime
                .agent_shell_store()
                .get(&b.pane)
                .cloned()
                .ok_or_else(changed)?;
            shell.native_message_incarnation = binding.incarnation().clone();
            runtime
                .agent_shell_store_mut()
                .restore_session(&b.pane, shell)?;
        }
        runtime.checkpoint_native_custody_metadata(&self.origin)?;
        Ok(())
    }
}

impl RuntimeSessionService {
    /// Initializes an empty actual private source before first durable admission.
    /// Existing bindings refuse fresh setup instead of being overwritten.
    pub(crate) fn initialize_native_custody_metadata(&mut self) -> Result<()> {
        self.persistence
            .cloned_transcript_store()
            .ok_or_else(changed)?
            .initialize_native_session_metadata(self.session().id.as_str())
    }

    /// Hydrates private original contexts without publishing fresh epoch bytes.
    /// Missing/oversized/corrupt active metadata refuses this dedicated startup;
    /// generic restore retains its existing fresh-identity behavior.
    pub(crate) fn prepare_native_custody_context(
        &mut self,
    ) -> Result<PreparedNativeCustodyContext> {
        let store = self
            .persistence
            .cloned_transcript_store()
            .ok_or_else(changed)?;
        let session = self.session().id.to_string();
        let (origin, records) = store.capture_native_session_metadata(&session)?;
        let mut saved = Vec::new();
        for record in &records {
            if let Some(original) = record.native_message_incarnation {
                saved.push((
                    record.pane_id.clone(),
                    record.conversation_id.clone(),
                    original,
                ));
            }
        }
        self.begin_subagent_cleanup_restore();
        self.restore_agent_sessions_from_records(store, records, false)?;
        origin.revalidate()?;
        let mut bindings = Vec::new();
        for (pane, conversation, original) in saved {
            let Some(shell) = self.agent_shell_store().get(&pane) else {
                continue;
            };
            let Some(descriptor) = self.find_pane_descriptor(&pane) else {
                continue;
            };
            if shell.session_id != conversation || shell.ephemeral {
                continue;
            }
            bindings.push(Binding {
                agent: format!("agent-{pane}"),
                pane,
                conversation,
                window: descriptor.window_id.to_string(),
                project: shell.project_scope.as_ref().map(|p| p.scope_id()),
                original,
                provisional: shell.native_message_incarnation.clone(),
            });
        }
        Ok(PreparedNativeCustodyContext {
            session,
            origin,
            bindings,
        })
    }
}

/// Content-free startup continuity diagnostic.
fn changed() -> MezError {
    MezError::invalid_state("native custody private context changed or unavailable")
}
