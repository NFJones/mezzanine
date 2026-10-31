//! One-shot non-executable completion diagnostics for original filesystem facts.
//!
//! Definitions are captured with the original queued group and matching uses
//! only its validated semantic outcome. Neither a later pane mode nor a newer
//! configuration can turn this gate into executable work. Entered diagnostic
//! failures retain first-error custody and cannot replay earlier diagnostics.
//! Acceptance is not audit, publication, archive or producer-transfer evidence.

use super::*;

impl RuntimeSessionService {
    /// Classifies only an unentered original context-committed receipt. The
    /// immutable native domain never calls the general executable hook runner.
    /// A duplicate accepted gate is inert; an entered unproven gate fails closed.
    pub(super) fn complete_filesystem_hooks(
        &mut self,
        key: &ExternalCompletionKey,
    ) -> Result<bool> {
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(key)
            .ok_or_else(|| MezError::invalid_state("filesystem hook owner unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        if matches!(
            receipt.phase,
            FilesystemCompletionPhase::HooksAccepted
                | FilesystemCompletionPhase::AuditEntered
                | FilesystemCompletionPhase::AuditNotRequired
                | FilesystemCompletionPhase::AuditAppendAccepted
                | FilesystemCompletionPhase::AuditPhysicalAccepted
                | FilesystemCompletionPhase::PublicationHeld
                | FilesystemCompletionPhase::PublicationAccepted
        ) {
            return Ok(false);
        }
        if receipt.phase != FilesystemCompletionPhase::ContextCommitted {
            return Err(MezError::conflict(
                "filesystem hook gate is not unentered context-committed work",
            ));
        }
        let payload = receipt.completed_hook_payload.as_deref().ok_or_else(|| {
            MezError::invalid_state("filesystem canonical hook payload unavailable")
        })?;
        let plans = crate::integrations::hooks::plan_event(
            &receipt.completed_hooks,
            HookEvent::PostShellCommand,
            payload,
        )?
        .plans;
        let paint = self
            .agent_shell_store()
            .get(&receipt.owner.pane_id)
            .is_some_and(|session| session.session_id == receipt.owner.conversation_id)
            && self
                .agent
                .completion_settlements
                .groups
                .get(&key.0)
                .is_some_and(|group| !group.paint_revoked);
        self.filesystem_completion_phase(key, FilesystemCompletionPhase::HooksEntered)?;
        self.complete_native_filesystem_hook_plans(plans, paint)?;
        self.filesystem_completion_phase(key, FilesystemCompletionPhase::HooksAccepted)?;
        Ok(true)
    }
}
