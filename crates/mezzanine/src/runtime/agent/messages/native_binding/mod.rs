//! Actual native shell attribution captured before durable message admission.
//!
//! Public pane/agent labels select candidates only. The current runtime shell,
//! layout, exact MMP registration and shell incarnation together supply native
//! ownership. Independent conversation bindings get new incarnations; exact
//! rollback preserves them. Ephemeral children suspend their durable parent's
//! delivery without retiring its pending mail. No saved record recreates this
//! live association; trusted restart qualification is a separate boundary.

use crate::Result;
use crate::runtime::RuntimeSessionService;
use mez_agent::messaging::MessageNativeRegistration;
use mez_core::ids::AgentId;

impl RuntimeSessionService {
    /// Refreshes the complete trusted native view before source freeze or live
    /// context selection. Resident-only traffic keeps its existing behavior.
    pub(crate) fn refresh_native_message_bindings(&mut self) -> Result<()> {
        if self.message_restore_pending() || self.message_service().payload_observer().is_none() {
            return Ok(());
        }
        let mut bindings = Vec::new();
        for shell in self.agent_shell_store().sessions() {
            let Some(id) = AgentId::opaque(format!("agent-{}", shell.pane_id)) else {
                continue;
            };
            let Some(identity) = self.message_service().registered_identity(&id) else {
                continue;
            };
            if identity.pane_id.as_ref().map(|id| id.as_str()) != Some(shell.pane_id.as_str())
                || identity
                    .capabilities
                    .iter()
                    .any(|value| value == "external-harness")
                || self.find_pane_descriptor(&shell.pane_id).is_none()
            {
                continue;
            }
            let Some(registration) = self.message_service().capture_sender_registration(&id) else {
                continue;
            };
            let current_parent = self
                .message_service()
                .native_parent_for_registration(&registration, &shell.native_message_incarnation);
            let conversation_id = if shell.ephemeral {
                current_parent
                    .map(|owner| owner.conversation_id())
                    .unwrap_or(&shell.session_id)
            } else {
                &shell.session_id
            };
            bindings.push(MessageNativeRegistration {
                registration,
                incarnation: shell.native_message_incarnation.clone(),
                conversation_id: conversation_id.to_string(),
                admission_available: !shell.ephemeral || current_parent.is_some(),
                delivery_available: !shell.ephemeral,
            });
        }
        self.message_service_mut()
            .reconcile_native_conversations(&bindings)?;
        Ok(())
    }
}
