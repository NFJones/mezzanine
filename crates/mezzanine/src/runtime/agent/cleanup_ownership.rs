//! Purpose-specific ownership for destructive delegation cleanup.
//!
//! Structural lineage is presentation and delegation-limit metadata, not a
//! teardown capability. Only a completed local delegation captures these
//! runtime-only edges. Each edge binds both conversations and the exclusive
//! manager or generation-qualified adapter that owns each native process.
//! Nothing here is serialized or reconstructed from saved lineage. Native
//! ownership transitions must preserve a proven owner, never infer one from
//! an unreadable executable, a missing process, a label, or a root/depth field.

use super::*;
use crate::runtime::PaneProcessInstance;

/// Instance-owned runtime capability state, never serialized or shared.
///
/// Live proofs and pending qualification form one lifecycle boundary: pending
/// records cannot authorize effects, and only exact native owners can publish
/// live edges. The enclosing agent component owns this box exclusively, which
/// avoids copying inline map storage through large actor state transitions.
#[derive(Default)]
pub(super) struct RuntimeSubagentCleanupState {
    /// Exact live edges permitted to participate in destructive cleanup.
    pub(super) ownership: BTreeMap<String, RuntimeSubagentCleanupOwnership>,
    /// Nonauthorizing successful-restore qualification awaiting native owners.
    pub(super) pending_restore: BTreeMap<String, PendingCleanupRestore>,
}

/// Current exclusive native owner of one captured conversation binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CleanupProcessOwner {
    /// The synchronous manager retains its occupied pane and actual Child.
    /// PID equality is only a consistency check within that ownership, not
    /// independent authorization to adopt another process.
    Manager(u32),
    /// An exact live async adapter generation, never a pane-id-only callback.
    Adapter(PaneProcessInstance),
}

/// One exact logical conversation and its locally owned native process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CleanupBinding {
    /// Pane-derived identity whose current shell supplies the binding.
    agent_id: String,
    /// Immutable conversation captured at actual delegation commit.
    conversation_id: String,
    /// Exclusive runtime owner, qualified before any cleanup effects.
    process: CleanupProcessOwner,
}

/// Original immediate delegation obligation; parent replacement never rewrites it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RuntimeSubagentCleanupOwnership {
    /// Original parent, including the conversation that actually delegated.
    parent: CleanupBinding,
    /// Exact child binding established by that delegation.
    child: CleanupBinding,
}

/// Nonauthorizing qualification from one completely successful binding restore.
/// Both endpoints must have been installed by that operation; process startup
/// may complete later and is the only point that can publish a live edge.
#[derive(Clone, Debug)]
pub(super) struct PendingCleanupRestore {
    /// Exact parent endpoint installed by this restore operation.
    parent_agent_id: String,
    /// Parent conversation installed by that same operation.
    parent_conversation_id: String,
    /// Child conversation installed by that same operation.
    child_conversation_id: String,
    /// First actual parent owner; later handoff may move but never replace it.
    parent_process: Option<CleanupProcessOwner>,
    /// First actual child owner; an intervening native install cancels the edge.
    child_process: Option<CleanupProcessOwner>,
}

impl RuntimeSessionService {
    /// Starts a binding restore without allowing a prior partial operation's
    /// pending qualifications to attach to later native process installations.
    pub(crate) fn begin_subagent_cleanup_restore(&mut self) {
        self.agent.subagent_cleanup.pending_restore.clear();
    }

    /// Invalidates pending restore qualification when either endpoint is rebound.
    /// Unlike live cleanup obligations, pending records cannot survive a later
    /// standalone resume, even if it happens to name the same saved conversation.
    pub(super) fn retire_pending_cleanup_restore_for_agent(&mut self, agent_id: &str) {
        self.agent
            .subagent_cleanup
            .pending_restore
            .retain(|child, pending| child != agent_id && pending.parent_agent_id != agent_id);
    }

    /// Qualifies only successfully installed parent and persistent-child
    /// endpoints from the same completed restore operation. Input checkpoint
    /// membership alone is insufficient, and standalone resume never calls this.
    pub(crate) fn qualify_subagent_cleanup_restore(
        &mut self,
        installed: &BTreeSet<(String, String)>,
    ) {
        self.agent.subagent_cleanup.pending_restore.clear();
        for (child, persistent) in &self.agent.persistent_subagents {
            if installed.contains(&(child.clone(), persistent.conversation_id.clone()))
                && installed.contains(&(
                    persistent.parent_agent_id.clone(),
                    persistent.parent_conversation_id.clone(),
                ))
            {
                let parent_process = persistent
                    .parent_agent_id
                    .strip_prefix("agent-")
                    .and_then(|pane| self.cleanup_process_owner(pane));
                let child_process = child
                    .strip_prefix("agent-")
                    .and_then(|pane| self.cleanup_process_owner(pane));
                self.agent.subagent_cleanup.pending_restore.insert(
                    child.clone(),
                    PendingCleanupRestore {
                        parent_agent_id: persistent.parent_agent_id.clone(),
                        parent_conversation_id: persistent.parent_conversation_id.clone(),
                        child_conversation_id: persistent.conversation_id.clone(),
                        parent_process,
                        child_process,
                    },
                );
            }
        }
        self.finalize_pending_subagent_cleanup_restore();
    }

    /// Publishes a qualified restored edge only after both current native owners
    /// exist and both installed conversations still match. Called again by the
    /// native startup owner after installation, never by a saved-lineage loader.
    pub(crate) fn finalize_pending_subagent_cleanup_restore(&mut self) {
        let pending = self.agent.subagent_cleanup.pending_restore.clone();
        for (child_agent_id, mut pending) in pending {
            let child_pane = child_agent_id.strip_prefix("agent-");
            let parent_pane = pending.parent_agent_id.strip_prefix("agent-");
            let bindings_match = child_pane
                .and_then(|pane| self.agent_shell_store().get(pane))
                .is_some_and(|shell| shell.session_id == pending.child_conversation_id)
                && parent_pane
                    .and_then(|pane| self.agent_shell_store().get(pane))
                    .is_some_and(|shell| shell.session_id == pending.parent_conversation_id);
            if !bindings_match {
                self.agent
                    .subagent_cleanup
                    .pending_restore
                    .remove(&child_agent_id);
                continue;
            }
            let child = self.capture_cleanup_binding(&child_agent_id);
            let parent = self.capture_cleanup_binding(&pending.parent_agent_id);
            if child.as_ref().is_some_and(|binding| {
                pending
                    .child_process
                    .as_ref()
                    .is_some_and(|owner| *owner != binding.process)
            }) || parent.as_ref().is_some_and(|binding| {
                pending
                    .parent_process
                    .as_ref()
                    .is_some_and(|owner| *owner != binding.process)
            }) {
                self.agent
                    .subagent_cleanup
                    .pending_restore
                    .remove(&child_agent_id);
                continue;
            }
            if let Some(child) = &child {
                pending.child_process = Some(child.process.clone());
            }
            if let Some(parent) = &parent {
                pending.parent_process = Some(parent.process.clone());
            }
            if child.is_some() && parent.is_some() {
                self.record_subagent_cleanup_ownership(&child_agent_id);
                self.agent
                    .subagent_cleanup
                    .pending_restore
                    .remove(&child_agent_id);
            } else {
                self.agent
                    .subagent_cleanup
                    .pending_restore
                    .insert(child_agent_id, pending);
            }
        }
    }

    /// Revokes proofs tied to a superseded manager installation, even if its
    /// replacement reuses a conversation or PID. Called immediately after a
    /// successful native spawn, never for a delayed adapter Spawned notice.
    pub(crate) fn invalidate_subagent_cleanup_native_install(&mut self, pane_id: &str) {
        let agent_id = format!("agent-{pane_id}");
        self.agent
            .subagent_cleanup
            .ownership
            .retain(|_, edge| edge.parent.agent_id != agent_id && edge.child.agent_id != agent_id);
        self.agent
            .subagent_cleanup
            .pending_restore
            .retain(|child, pending| {
                !(child == &agent_id && pending.child_process.is_some()
                    || pending.parent_agent_id == agent_id && pending.parent_process.is_some())
            });
    }

    /// Migrates existing incoming/outgoing endpoint proofs across the actual
    /// manager-to-adapter handoff. This preserves the original conversations;
    /// it cannot create a delegation or adopt a different process.
    pub(crate) fn transfer_subagent_cleanup_to_adapter(
        &mut self,
        pane_id: &str,
        primary_pid: u32,
        instance: &PaneProcessInstance,
    ) {
        if self.adapter_owned_pane_process_instance(pane_id).as_ref() != Some(instance)
            || self.primary_pid_for_live_pane_process(pane_id) != Some(primary_pid)
        {
            return;
        }
        self.move_subagent_cleanup_process_owner(
            pane_id,
            CleanupProcessOwner::Manager(primary_pid),
            CleanupProcessOwner::Adapter(instance.clone()),
        );
    }

    /// Restores only a cancelled test handoff's exact already-owned process.
    #[cfg(test)]
    pub(crate) fn return_subagent_cleanup_to_manager(
        &mut self,
        pane_id: &str,
        instance: &PaneProcessInstance,
        primary_pid: u32,
    ) {
        self.move_subagent_cleanup_process_owner(
            pane_id,
            CleanupProcessOwner::Adapter(instance.clone()),
            CleanupProcessOwner::Manager(primary_pid),
        );
    }

    /// Moves captured endpoint owners and restore pins without changing any
    /// logical relationship. Callers must supply an actual ownership witness.
    fn move_subagent_cleanup_process_owner(
        &mut self,
        pane_id: &str,
        previous: CleanupProcessOwner,
        next: CleanupProcessOwner,
    ) {
        let agent_id = format!("agent-{pane_id}");
        for edge in self.agent.subagent_cleanup.ownership.values_mut() {
            for endpoint in [&mut edge.parent, &mut edge.child] {
                if endpoint.agent_id == agent_id && endpoint.process == previous {
                    endpoint.process = next.clone();
                }
            }
        }
        for (child, pending) in &mut self.agent.subagent_cleanup.pending_restore {
            if pending.parent_agent_id == agent_id
                && pending.parent_process.as_ref() == Some(&previous)
            {
                pending.parent_process = Some(next.clone());
            }
            if child == &agent_id && pending.child_process.as_ref() == Some(&previous) {
                pending.child_process = Some(next.clone());
            }
        }
    }

    /// Observes only existing manager/adapter ownership, without OS fallback.
    fn cleanup_process_owner(&self, pane_id: &str) -> Option<CleanupProcessOwner> {
        if let Some(pid) = self.manager_owned_pane_process_pid(pane_id) {
            return Some(CleanupProcessOwner::Manager(pid));
        }
        self.adapter_owned_pane_process_instance(pane_id)
            .map(CleanupProcessOwner::Adapter)
    }

    /// Captures an installed conversation with a current local native owner.
    fn capture_cleanup_binding(&self, agent_id: &str) -> Option<CleanupBinding> {
        let pane_id = agent_id.strip_prefix("agent-")?;
        self.find_pane_descriptor(pane_id)?;
        let shell = self.agent_shell_store().get(pane_id)?;
        Some(CleanupBinding {
            agent_id: agent_id.to_string(),
            conversation_id: shell.session_id.clone(),
            process: self.cleanup_process_owner(pane_id)?,
        })
    }

    /// Publishes cleanup ownership only after actual local child setup succeeds.
    /// Missing bindings or native ownership leave no destructive authority.
    pub(crate) fn record_subagent_cleanup_ownership(&mut self, child_agent_id: &str) {
        self.agent.subagent_cleanup.ownership.remove(child_agent_id);
        let Some(lineage) = self.subagent_lineage(child_agent_id) else {
            return;
        };
        if self
            .agent
            .restored_subagent_lineage
            .contains(child_agent_id)
        {
            return;
        }
        let Some(parent) = self.capture_cleanup_binding(&lineage.parent_agent_id) else {
            return;
        };
        let Some(child) = self.capture_cleanup_binding(child_agent_id) else {
            return;
        };
        if let Some(persistent) = self.persistent_subagent(child_agent_id)
            && (persistent.parent_agent_id != parent.agent_id
                || persistent.parent_conversation_id != parent.conversation_id
                || persistent.conversation_id != child.conversation_id)
        {
            return;
        }
        self.agent.subagent_cleanup.ownership.insert(
            child_agent_id.to_string(),
            RuntimeSubagentCleanupOwnership { parent, child },
        );
    }

    /// Validates one original edge against both current process owners and the
    /// exact child conversation. A fence retains the original parent's cleanup
    /// obligation on the same owner, never ownership by its new conversation.
    fn validated_subagent_cleanup_edge(
        &self,
        child_agent_id: &str,
        replacing: Option<(&str, &str)>,
    ) -> Option<&RuntimeSubagentCleanupOwnership> {
        if self
            .agent
            .restored_subagent_lineage
            .contains(child_agent_id)
        {
            return None;
        }
        let edge = self.agent.subagent_cleanup.ownership.get(child_agent_id)?;
        if self
            .subagent_lineage(child_agent_id)
            .is_some_and(|lineage| lineage.parent_agent_id != edge.parent.agent_id)
            || self.capture_cleanup_binding(child_agent_id).as_ref() != Some(&edge.child)
        {
            return None;
        }
        let parent = self.capture_cleanup_binding(&edge.parent.agent_id)?;
        let replacing_original = replacing.is_some_and(|(agent, conversation)| {
            agent == edge.parent.agent_id && conversation == edge.parent.conversation_id
        });
        if parent.process != edge.parent.process
            || (parent.conversation_id != edge.parent.conversation_id
                && !self.subagent_descendant_is_fenced(child_agent_id)
                && !replacing_original)
        {
            return None;
        }
        Some(edge)
    }

    /// Returns the validated hop count below a locally bound parent. Every hop
    /// must be a current cleanup edge; a historical or replaced intermediate
    /// stops traversal even when the leaf itself has live lineage.
    fn subagent_cleanup_distance(
        &self,
        child_agent_id: &str,
        parent_agent_id: &str,
        original_parent_conversation: Option<&str>,
    ) -> Option<usize> {
        if child_agent_id == parent_agent_id {
            return None;
        }
        let mut visited = BTreeSet::new();
        let mut current = child_agent_id;
        let mut distance = 0usize;
        let mut found = None;
        while visited.insert(current) {
            let Some(edge) = self.validated_subagent_cleanup_edge(
                current,
                original_parent_conversation.map(|conversation| (parent_agent_id, conversation)),
            ) else {
                return found;
            };
            distance = distance.checked_add(1)?;
            if edge.parent.agent_id == parent_agent_id {
                if original_parent_conversation
                    .is_some_and(|conversation| conversation != edge.parent.conversation_id)
                {
                    return None;
                }
                found = Some(distance);
            }
            // Fenced cleanup may terminate at its original parent's pane, but
            // cannot bridge that pane's replacement conversation to an ancestor.
            let parent_pane = edge.parent.agent_id.strip_prefix("agent-")?;
            if self.agent_shell_store().get(parent_pane)?.session_id != edge.parent.conversation_id
            {
                return found;
            }
            current = &edge.parent.agent_id;
        }
        None
    }

    /// Revokes task execution authority while synchronously retaining an
    /// existing fence and its valid cleanup-only obligation. This operation
    /// contains no fallible persistence or scheduler admission, so later
    /// settlement errors cannot expose an unfenced task or erase cleanup proof.
    pub(crate) fn retire_subagent_execution_authority(&mut self, child_agent_id: &str) {
        let fence = self
            .agent
            .fenced_subagent_descendants
            .get(child_agent_id)
            .cloned();
        let ownership = fence.as_ref().and_then(|_| {
            self.validated_subagent_cleanup_edge(child_agent_id, None)
                .cloned()
        });
        self.remove_subagent_authority_state(child_agent_id);
        if let Some(fence) = fence {
            self.agent
                .fenced_subagent_descendants
                .insert(child_agent_id.to_string(), fence);
        }
        if let Some(ownership) = ownership {
            self.restore_fenced_subagent_cleanup_ownership(child_agent_id, ownership);
        }
    }

    /// Retains only the cleanup obligation captured before fenced task
    /// settlement. No lineage, scope, permission or result-routing authority is
    /// restored. A rebound child or changed native owner cannot inherit it.
    pub(super) fn restore_fenced_subagent_cleanup_ownership(
        &mut self,
        child_agent_id: &str,
        ownership: RuntimeSubagentCleanupOwnership,
    ) {
        if self.subagent_descendant_is_fenced(child_agent_id)
            && !self
                .agent
                .restored_subagent_lineage
                .contains(child_agent_id)
            && self.capture_cleanup_binding(child_agent_id).as_ref() == Some(&ownership.child)
            && self
                .capture_cleanup_binding(&ownership.parent.agent_id)
                .is_some_and(|binding| binding.process == ownership.parent.process)
        {
            self.agent
                .subagent_cleanup
                .ownership
                .insert(child_agent_id.to_string(), ownership);
        }
    }

    /// Selects deepest-first descendants from validated cleanup edges, not
    /// persisted root/depth/label metadata. An optional original conversation
    /// restricts replacement fencing to the delegation owner being replaced.
    pub(super) fn subagent_cleanup_descendant_agent_ids(
        &self,
        parent_agent_id: &str,
        original_parent_conversation: Option<&str>,
    ) -> Vec<String> {
        if self.capture_cleanup_binding(parent_agent_id).is_none() {
            return Vec::new();
        }
        let mut descendants = self
            .agent
            .subagent_cleanup
            .ownership
            .keys()
            .filter_map(|child| {
                self.subagent_cleanup_distance(child, parent_agent_id, original_parent_conversation)
                    .map(|distance| (distance, child.clone()))
            })
            .collect::<Vec<_>>();
        descendants.sort_by(|(left_depth, left), (right_depth, right)| {
            right_depth.cmp(left_depth).then_with(|| left.cmp(right))
        });
        descendants.into_iter().map(|(_, child)| child).collect()
    }

    /// Rejects stale conversation callbacks before any descendant or direct
    /// macro-child closure, including a separately resumed child binding.
    pub(crate) fn subagent_cleanup_child_is_owned_by_turn(
        &self,
        child_agent_id: &str,
        turn: &AgentTurnRecord,
    ) -> bool {
        self.validated_subagent_cleanup_edge(child_agent_id, None)
            .is_some_and(|edge| {
                edge.parent.agent_id == turn.agent_id
                    && edge.parent.conversation_id == turn.conversation_id
                    && self
                        .agent_shell_store()
                        .get(&turn.pane_id)
                        .is_some_and(|shell| shell.session_id == turn.conversation_id)
            })
    }
}
