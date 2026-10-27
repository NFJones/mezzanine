//! Sealed acknowledgement of the existing same-pane loop controller's restore.
//!
//! This qualifies closing original completion custody across a code-owned parent
//! rebind. It cannot authorize parent work, arbitrary replacement adoption, or a
//! restoration of its own. The existing bind/cleanup return installs the ack
//! before later fallible paint/objective operations; there is no lookup gap.

use super::*;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
mod tests;

/// Private original controller and group evidence, never reconstructed from CID.
pub(super) struct OwnLoopRestore {
    group_identity: Arc<()>,
    handoff_identity: Arc<()>,
    loop_id: String,
    pane_id: String,
    original_cid: String,
    expected_running_turn: String,
    parent_cid: String,
    parent_entries: u64,
    /// Original parent archive frontier protected by the same controller proof.
    parent_high_water: u64,
    parent_lineage: Option<String>,
    bind_entered: OnceLock<()>,
    installed: OnceLock<()>,
    #[cfg(test)]
    metadata_contract: OnceLock<bool>,
    #[cfg(test)]
    actual_snapshot: OnceLock<mez_agent::AgentShellSession>,
    #[cfg(test)]
    unacked_parent_rejected: OnceLock<bool>,
    #[cfg(test)]
    foreign_checks: OnceLock<(bool, bool)>,
    #[cfg(test)]
    rollback_equal: OnceLock<bool>,
    #[cfg(test)]
    bind_attempts: AtomicU64,
}

impl OwnLoopRestore {
    /// The exact current shell required by the pre-effect running-turn guard.
    fn original_session_matches(&self, session: &mez_agent::AgentShellSession) -> bool {
        session.pane_id == self.pane_id
            && session.session_id == self.original_cid
            && session.running_turn_id.as_deref() == Some(self.expected_running_turn.as_str())
    }

    /// CID text is insufficient without actual install and both strong scopes.
    fn installed_parent_matches(
        &self,
        session: &mez_agent::AgentShellSession,
        group: &Arc<()>,
        handoff: &Arc<()>,
        original_cid: &str,
    ) -> bool {
        self.installed.get().is_some()
            && Arc::ptr_eq(&self.group_identity, group)
            && Arc::ptr_eq(&self.handoff_identity, handoff)
            && session.pane_id == self.pane_id
            && self.original_cid == original_cid
            && session.session_id == self.parent_cid
            && session
                .running_turn_id
                .as_deref()
                .is_none_or(|turn| turn == self.expected_running_turn)
    }
}

/// Isolated source facts, not production binding or cleanup permission.
#[cfg(test)]
pub(crate) struct LoopRestoreTestFacts {
    pub(crate) ack: bool,
    pub(crate) bind_attempts: u64,
    pub(crate) metadata_only: Option<bool>,
    pub(crate) unacked_parent_rejected: Option<bool>,
    pub(crate) foreign_snapshot_rejected: Option<bool>,
    pub(crate) foreign_scope_rejected: Option<bool>,
    pub(crate) rollback_equal: Option<bool>,
}

/// Opaque acknowledgement carrier held across the existing bind operation.
pub(crate) struct CompletionProviderLoopRestore(Arc<OwnLoopRestore>);

/// Matches generic durable-bind metadata values; no general permission-order
/// claim follows from None. Ordinary qualified rediscovery remains unchanged.
/// Input is only an actual successful exact-running-turn restore result.
fn contract_restored_metadata(
    mut actual: mez_agent::AgentShellSession,
) -> mez_agent::AgentShellSession {
    actual.allowed_actions = None;
    actual.display_name = None;
    actual.project_scope = None;
    actual
}

impl RuntimeSessionService {
    /// Observes the real private loop's frozen parent fields before claiming.
    #[cfg(test)]
    pub(crate) fn loop_parent_metadata_for_tests(
        &self,
        turn_id: &str,
    ) -> Option<(mez_agent::AllowedActionSet, Option<String>)> {
        let turn = self.agent_loop_turn(turn_id)?;
        let state = self.agent_loop_state_by_id(&turn.loop_id)?;
        Some((
            state.parent_allowed_actions.clone(),
            state.parent_display_name.clone(),
        ))
    }

    /// Reads only saved exact-group observations; no live post-drain recapture.
    #[cfg(test)]
    pub(crate) fn provider_loop_restore_facts_for_tests(
        &self,
        turn_id: &str,
        expected_group: &str,
    ) -> Option<LoopRestoreTestFacts> {
        let group = self.agent.completion_settlements.groups.get(turn_id)?;
        if group.group.as_str() != expected_group {
            return None;
        }
        let proof = group.provider_handoff.as_ref()?.own_loop_restore.as_ref()?;
        Some(LoopRestoreTestFacts {
            ack: proof.installed.get().is_some(),
            bind_attempts: proof.bind_attempts.load(Ordering::Relaxed),
            metadata_only: proof.metadata_contract.get().copied(),
            unacked_parent_rejected: proof.unacked_parent_rejected.get().copied(),
            foreign_snapshot_rejected: proof.foreign_checks.get().map(|checks| checks.0),
            foreign_scope_rejected: proof.foreign_checks.get().map(|checks| checks.1),
            rollback_equal: proof.rollback_equal.get().copied(),
        })
    }

    /// Records the actual supported rollback result, not a hand-set CID or ack.
    #[cfg(test)]
    pub(crate) fn observe_completion_provider_loop_rollback(
        restore: Option<&CompletionProviderLoopRestore>,
        equal: bool,
    ) {
        if let Some(restore) = restore {
            let _one_actual_rollback = restore.0.rollback_equal.set(equal);
        }
    }

    /// Performs only a newly armed negative standalone controller restoration.
    /// Every ownership check precedes mutation; the generic active-turn fence
    /// remains intact. The existing caller still prepares/commits cleanup and
    /// rolls back its untouched previousSession on ANY error from this operation.
    pub(crate) fn restore_completion_provider_loop_parent(
        &mut self,
        restore: &CompletionProviderLoopRestore,
    ) -> Result<()> {
        let proof = &restore.0;
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(&proof.expected_running_turn)
            .ok_or_else(|| {
                MezError::invalid_state("negative loop restore original group is unavailable")
            })?;
        let owner = group.provider_handoff.as_ref().ok_or_else(|| {
            MezError::invalid_state("negative loop restore handoff is unavailable")
        })?;
        let state = self.agent_loop_state_by_id(&proof.loop_id).ok_or_else(|| {
            MezError::invalid_state("negative loop restore controller is unavailable")
        })?;
        if !Arc::ptr_eq(&proof.group_identity, &group.identity)
            || !Arc::ptr_eq(&proof.handoff_identity, &owner.identity)
            || !owner
                .own_loop_restore
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, proof))
            || owner.rejection.is_none()
            || owner.settlement != CompletionMilestone::Entered
            || !owner.context_consumers_open
            || !owner.source_frozen
            || group.task_result != CompletionMilestone::Entered
            || group.turn.turn_id != proof.expected_running_turn
            || group.turn.pane_id != proof.pane_id
            || group.turn.conversation_id != proof.original_cid
            || state.routed_parent_turn_id.is_some()
            || state.invoking_pane_id != proof.pane_id
            || state.execution_pane_id != proof.pane_id
            || state.parent_conversation_id != proof.parent_cid
            || state.parent_transcript_entries != proof.parent_entries
            || state.parent_transcript_high_water != proof.parent_high_water
            || state.parent_prompt_cache_lineage_id != proof.parent_lineage
            || !self
                .agent_loop_turn(&proof.expected_running_turn)
                .is_some_and(|turn| turn.loop_id == proof.loop_id && turn.pane_id == proof.pane_id)
            || !self
                .agent_turn_ledger()
                .turn(&proof.expected_running_turn)
                .is_some_and(|turn| {
                    turn.agent_id == group.turn.agent_id
                        && turn.pane_id == proof.pane_id
                        && turn.conversation_id == proof.original_cid
                        && turn.state == AgentTurnState::Running
                })
            || !self
                .agent_shell_store()
                .get(&proof.pane_id)
                .is_some_and(|session| proof.original_session_matches(session))
            || proof.installed.get().is_some()
        {
            return Err(MezError::invalid_state(
                "negative loop restore does not own its exact controller/running-turn binding",
            ));
        }
        proof.bind_entered.set(()).map_err(|_| {
            MezError::invalid_state("negative loop restore already entered; replay prohibited")
        })?;
        #[cfg(test)]
        proof.bind_attempts.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        if self.take_provider_handoff_fault_for_tests("loop_wrong_turn") {
            // Wrong expected argument to the REAL lower guard; no live turn/CID
            // is edited and no data is installed when the guard rejects it.
            return self
                .agent_shell_store_mut()
                .restore_conversation_for_running_turn_with_lineage(
                    &proof.pane_id,
                    "unowned-loop-turn",
                    &proof.parent_cid,
                    mez_agent::AgentTranscriptPosition {
                        logical_high_water: proof.parent_high_water,
                        active_raw_entries: proof.parent_entries,
                    },
                    proof.parent_lineage.clone(),
                )
                .map(|_| ())
                .map_err(MezError::from);
        }

        let actual = self
            .agent_shell_store_mut()
            .restore_conversation_for_running_turn_with_lineage(
                &proof.pane_id,
                &proof.expected_running_turn,
                &proof.parent_cid,
                mez_agent::AgentTranscriptPosition {
                    logical_high_water: proof.parent_high_water,
                    active_raw_entries: proof.parent_entries,
                },
                proof.parent_lineage.clone(),
            )?
            .clone();
        #[cfg(test)]
        let before_reset = actual.clone();
        let contracted = contract_restored_metadata(actual);
        #[cfg(test)]
        if self.take_provider_handoff_fault_for_tests("loop_metadata_reset") {
            return Err(MezError::invalid_state(
                "injected negative loop metadata reset failure",
            ));
        }
        // restore_session is rollback-named and validates ONLY pane identity.
        // It is NOT the proof above. Use ONLY this actual return, immediately,
        // to reset the same three metadata fields as generic durable binding.
        // No CID, running turn, lineage, persistence or history field is assigned.
        let _installed = self
            .agent_shell_store_mut()
            .restore_session(&proof.pane_id, contracted)?;
        #[cfg(test)]
        {
            let mut comparable = _installed.clone();
            comparable.allowed_actions = before_reset.allowed_actions.clone();
            comparable.display_name = before_reset.display_name.clone();
            comparable.project_scope = before_reset.project_scope.clone();
            let only_metadata_reset = comparable == before_reset
                && _installed.allowed_actions.is_none()
                && _installed.display_name.is_none()
                && _installed.project_scope.is_none()
                && _installed.running_turn_id.as_deref()
                    == Some(proof.expected_running_turn.as_str())
                && _installed.session_id == proof.parent_cid;
            let _one_actual_reset = proof.metadata_contract.set(only_metadata_reset);
            let _unacked_rejection =
                proof
                    .unacked_parent_rejected
                    .set(!proof.installed_parent_matches(
                        _installed,
                        &proof.group_identity,
                        &proof.handoff_identity,
                        &proof.original_cid,
                    ));
            let _actual_lower_snapshot = proof.actual_snapshot.set(_installed.clone());
        }
        Ok(())
    }

    /// Reads only the exact group's actual shared installed acknowledgement.
    #[cfg(test)]
    pub(crate) fn provider_loop_restore_ack_for_tests(
        &self,
        turn_id: &str,
        expected_group: &str,
    ) -> bool {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .is_some_and(|group| {
                group.group.as_str() == expected_group
                    && group
                        .provider_handoff
                        .as_ref()
                        .and_then(|owner| owner.own_loop_restore.as_ref())
                        .is_some_and(|restore| restore.installed.get().is_some())
            })
    }

    /// Samples only a real same-pane controller before its own restoration.
    /// Diagnostic loop JSON and public pane/conversation labels grant nothing.
    pub(crate) fn prepare_completion_provider_loop_restore(
        &mut self,
        pane_id: &str,
        state: &crate::runtime::RuntimeAgentLoopState,
    ) -> Result<Option<CompletionProviderLoopRestore>> {
        if state.routed_parent_turn_id.is_some()
            || state.invoking_pane_id != pane_id
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
            expected_running_turn: group.turn.turn_id.clone(),
            parent_cid: state.parent_conversation_id.clone(),
            parent_entries: state.parent_transcript_entries,
            parent_high_water: state.parent_transcript_high_water,
            parent_lineage: state.parent_prompt_cache_lineage_id.clone(),
            bind_entered: OnceLock::new(),
            installed: OnceLock::new(),
            #[cfg(test)]
            metadata_contract: OnceLock::new(),
            #[cfg(test)]
            actual_snapshot: OnceLock::new(),
            #[cfg(test)]
            unacked_parent_rejected: OnceLock::new(),
            #[cfg(test)]
            foreign_checks: OnceLock::new(),
            #[cfg(test)]
            rollback_equal: OnceLock::new(),
            #[cfg(test)]
            bind_attempts: AtomicU64::new(0),
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
            let _installed_now = restore.0.installed.set(()).is_ok();
            #[cfg(test)]
            if _installed_now && let Some(actual) = restore.0.actual_snapshot.get() {
                // Local disconnected negative inputs only. Never install or
                // mutate a runtime CID/turn/ack to manufacture these checks.
                let mut foreign_store = mez_agent::AgentShellStore::default();
                let foreign = foreign_store
                    .enter_or_resume("%foreign-snapshot")
                    .expect("valid isolated pane");
                let foreign_snapshot = !restore.0.installed_parent_matches(
                    foreign,
                    &restore.0.group_identity,
                    &restore.0.handoff_identity,
                    &restore.0.original_cid,
                );
                let foreign_scope = !restore.0.installed_parent_matches(
                    actual,
                    &Arc::new(()),
                    &restore.0.handoff_identity,
                    &restore.0.original_cid,
                );
                let _actual_scope_checks = restore
                    .0
                    .foreign_checks
                    .set((foreign_snapshot, foreign_scope));
            }
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
                restore.installed_parent_matches(
                    session,
                    &group.identity,
                    &owner.identity,
                    &group.turn.conversation_id,
                )
            })
        })
    }
}
