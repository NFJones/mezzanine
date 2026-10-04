//! Exact-target user management of live pane agents.
//!
//! Capture and execution are actor-owned and primary-authorized. A target binds
//! the invoking client, pane root incarnation, conversation, task and issued
//! attempts independently of rendered labels or focus. Revalidation rejects
//! stale confirmations rather than redirecting them to replacement work. Stop
//! and close reuse the existing lifecycle owners; admission is never proof
//! that local or remote effects have finished or been undone.

use crate::error::{MezError, Result};
use crate::runtime::processes::RuntimePaneProcessIdentity;
use crate::runtime::{AgentTurnState, RuntimeAgentTurnStop, RuntimeSessionService};
use mez_core::ids::ClientId;

/// Immutable user operation target, kept separate from display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeAgentLifecycleTarget {
    pub(crate) client_id: ClientId,
    pub(crate) pane_id: String,
    pub(crate) agent_id: String,
    pub(crate) conversation_id: String,
    pub(crate) turn_id: Option<String>,
    pub(crate) process: Option<RuntimePaneProcessIdentity>,
    pub(crate) attempts: Vec<(String, String)>,
}

impl RuntimeSessionService {
    /// Captures a live registered pane agent for an attached primary.
    /// Unavailable kernel identity or ambiguous task ownership fails closed.
    pub(crate) fn capture_agent_lifecycle_target(
        &self,
        client_id: &ClientId,
        pane_id: &str,
    ) -> Result<RuntimeAgentLifecycleTarget> {
        self.require_live()?;
        if !self.session.is_attached_primary(client_id) {
            return Err(MezError::forbidden(
                "agent management requires an attached primary client",
            ));
        }
        let (_, pane) = super::runtime_pane_by_id(&self.session, pane_id)?;
        let agent_id = format!("agent-{pane_id}");
        let identity = mez_core::ids::AgentId::opaque(agent_id.clone())
            .ok_or_else(|| MezError::invalid_args("invalid target agent identity"))?;
        if self
            .message_service()
            .registered_identity(&identity)
            .and_then(|identity| identity.pane_id.as_ref())
            .map(|pane| pane.as_str())
            != Some(pane_id)
        {
            return Err(MezError::conflict(
                "target agent registration is unavailable",
            ));
        }
        let session = self
            .agent_shell_store()
            .get(pane_id)
            .ok_or_else(|| MezError::conflict("target agent conversation is unavailable"))?;
        let mut turns = self.agent_turn_ledger().turns().iter().filter(|turn| {
            turn.pane_id == pane_id
                && turn.agent_id == agent_id
                && turn.conversation_id == session.session_id
                && !matches!(
                    turn.state,
                    AgentTurnState::Completed
                        | AgentTurnState::Failed
                        | AgentTurnState::Interrupted
                )
        });
        let turn_id = turns.next().map(|turn| turn.turn_id.clone());
        if turns.next().is_some() {
            return Err(MezError::conflict(
                "target agent has ambiguous active task ownership",
            ));
        }
        let process = if pane.live {
            Some(
                self.pane_process_identity(pane_id)
                    .map_err(|_| MezError::conflict("target pane root identity is unavailable"))?,
            )
        } else {
            None
        };
        let attempts = turn_id
            .as_deref()
            .map(|turn| self.agent_worker_attempts_for_turn(turn))
            .unwrap_or_default();
        Ok(RuntimeAgentLifecycleTarget {
            client_id: client_id.clone(),
            pane_id: pane_id.to_string(),
            agent_id,
            conversation_id: session.session_id.clone(),
            turn_id,
            process,
            attempts,
        })
    }

    /// Revalidates authority and exact ownership without changing pane focus.
    pub(crate) fn validate_agent_lifecycle_target(
        &self,
        client_id: &ClientId,
        target: &RuntimeAgentLifecycleTarget,
    ) -> Result<()> {
        if client_id != &target.client_id {
            return Err(MezError::forbidden(
                "agent management target belongs to another client",
            ));
        }
        if self.capture_agent_lifecycle_target(client_id, &target.pane_id)? != *target {
            return Err(MezError::conflict(
                "agent management target changed; refresh and retry",
            ));
        }
        Ok(())
    }

    /// Admits interruption for only the captured current task.
    /// Returned stop evidence is a request, not worker settlement acknowledgment.
    #[allow(dead_code)] // Consumed by the dependent administrative browser.
    pub(crate) fn interrupt_agent_lifecycle_target(
        &mut self,
        client_id: &ClientId,
        target: &RuntimeAgentLifecycleTarget,
    ) -> Result<RuntimeAgentTurnStop> {
        self.validate_agent_lifecycle_target(client_id, target)?;
        if target.turn_id.is_none() {
            return Err(MezError::invalid_state("target agent has no active task"));
        }
        self.stop_agent_turn_for_pane(&target.pane_id)
    }

    /// Closes an exact target using ordinary live-process/force policy.
    /// The caller owns target-labelled confirmation; this never changes focus.
    #[allow(dead_code)] // Consumed by the dependent administrative browser.
    pub(crate) fn close_agent_lifecycle_target(
        &mut self,
        client_id: &ClientId,
        target: &RuntimeAgentLifecycleTarget,
        force: bool,
    ) -> Result<String> {
        self.validate_agent_lifecycle_target(client_id, target)?;
        let params = serde_json::json!({"pane_id": target.pane_id, "force": force}).to_string();
        self.dispatch_runtime_pane_close(client_id, &params)
    }
}
