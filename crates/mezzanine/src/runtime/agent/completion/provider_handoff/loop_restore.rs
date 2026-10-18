//! Sealed acknowledgement of the existing same-pane loop controller's restore.
//!
//! This qualifies closing original completion custody across a code-owned parent
//! rebind. It cannot authorize parent work, arbitrary replacement adoption, or a
//! restoration of its own. The existing bind/cleanup return installs the ack
//! before later fallible paint/objective operations; there is no lookup gap.

use super::*;
use std::sync::{Arc, OnceLock};

/// Private original controller and group evidence, never reconstructed from CID.
pub(super) struct OwnLoopRestore {
    group_identity: Arc<()>,
    handoff_identity: Arc<()>,
    loop_id: String,
    pane_id: String,
    original_cid: String,
    parent_cid: String,
    installed: OnceLock<()>,
}

/// Opaque acknowledgement carrier held across the existing bind operation.
pub(crate) struct CompletionProviderLoopRestore(Arc<OwnLoopRestore>);

impl RuntimeSessionService {
    /// Samples only a real same-pane controller before its own restoration.
    /// Diagnostic loop JSON and public pane/conversation labels grant nothing.
    pub(crate) fn prepare_completion_provider_loop_restore(
        &mut self,
        pane_id: &str,
        state: &crate::runtime::RuntimeAgentLoopState,
    ) -> Result<Option<CompletionProviderLoopRestore>> {
        if state.invoking_pane_id != pane_id
            || state.execution_pane_id != pane_id
            || self.agent_loop_state_by_id(&state.loop_id) != Some(state)
        {
            return Ok(None);
        }
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(None);
        };
        let Some(turn_id) = session.running_turn_id.clone() else {
            return Ok(None);
        };
        let Some(group) = self.agent.completion_settlements.groups.get(&turn_id) else {
            return Ok(None);
        };
        let Some(owner) = group
            .provider_handoff
            .as_ref()
            .filter(|owner| owner.rejection.is_some() && owner.context_consumers_open)
        else {
            return Ok(None);
        };
        if session.session_id != group.turn.conversation_id
            || group.turn.pane_id != pane_id
            || !self
                .agent_loop_turn(&turn_id)
                .is_some_and(|turn| turn.loop_id == state.loop_id && turn.pane_id == pane_id)
        {
            return Err(MezError::invalid_state(
                "negative loop restoration does not own its original controller/binding",
            ));
        }
        if let Some(restore) = &owner.own_loop_restore {
            if restore.loop_id != state.loop_id
                || restore.parent_cid != state.parent_conversation_id
            {
                return Err(MezError::invalid_state(
                    "negative loop restoration target changed",
                ));
            }
            return Ok(Some(CompletionProviderLoopRestore(restore.clone())));
        }
        let restore = Arc::new(OwnLoopRestore {
            group_identity: group.identity.clone(),
            handoff_identity: owner.identity.clone(),
            loop_id: state.loop_id.clone(),
            pane_id: pane_id.to_string(),
            original_cid: group.turn.conversation_id.clone(),
            parent_cid: state.parent_conversation_id.clone(),
            installed: OnceLock::new(),
        });
        if let Some(owner) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&turn_id)
            .and_then(|group| group.provider_handoff.as_mut())
        {
            owner.own_loop_restore = Some(restore.clone());
        }
        Ok(Some(CompletionProviderLoopRestore(restore)))
    }

    /// Called directly after the existing bind/cleanup commit succeeds. The
    /// shared owner was installed before mutation; setting the one-shot ack
    /// cannot fail through a post-install owner lookup or grant replacement work.
    pub(crate) fn acknowledge_completion_provider_loop_restore(
        restore: Option<CompletionProviderLoopRestore>,
    ) {
        if let Some(restore) = restore {
            let _same_installed_return = restore.0.installed.set(());
        }
    }

    /// Original binding or exact private installed own-restore receipt only.
    /// Missing shell state permits no parent action; this is old-source custody.
    pub(in crate::runtime::agent) fn provider_refusal_shell_binding_owned(
        &self,
        turn_id: &str,
    ) -> bool {
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return false;
        };
        let Some(session) = self.agent_shell_store().get(&group.turn.pane_id) else {
            return true;
        };
        if session.session_id == group.turn.conversation_id {
            return true;
        }
        group.provider_handoff.as_ref().is_some_and(|owner| {
            owner.own_loop_restore.as_ref().is_some_and(|restore| {
                restore.installed.get().is_some()
                    && Arc::ptr_eq(&restore.group_identity, &group.identity)
                    && Arc::ptr_eq(&restore.handoff_identity, &owner.identity)
                    && restore.pane_id == group.turn.pane_id
                    && restore.original_cid == group.turn.conversation_id
                    && session.session_id == restore.parent_cid
            })
        })
    }
}
