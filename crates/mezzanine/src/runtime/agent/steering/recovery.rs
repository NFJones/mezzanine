//! Bounded checkpoint projection and execution-inert restart reconciliation.
//!
//! Live receipt owners remain authoritative. A checkpoint contains display-only
//! occurrence evidence, never input, process handles or scheduling authority.
//! Pending evidence is retained ahead of bounded terminal history; restart
//! converts it to uncertainty because publication can precede local admission.

use std::collections::BTreeSet;

use mez_agent::transcript::{
    STEERING_RECOVERY_BYTES, STEERING_RECOVERY_ENTRIES, SteeringRecoveryReceipt,
    SteeringRecoveryStatus, validate_steering_recovery,
};

use super::{OWNER_CAPACITY, Receipt, RuntimeSessionService, Status};
use crate::error::Result;

/// Projects one occurrence without its executable input or root identity.
fn project(receipt: &Receipt, turn: Option<&str>) -> SteeringRecoveryReceipt {
    SteeringRecoveryReceipt {
        id: receipt.id.clone(),
        turn_id: turn.map(str::to_string),
        event_sequence: turn.map(|_| receipt.sequence),
        display: receipt.display.clone(),
        status: match receipt.status {
            Status::Pending => SteeringRecoveryStatus::Pending,
            Status::Admitted(generation) => SteeringRecoveryStatus::Admitted(generation),
            Status::NotSent => SteeringRecoveryStatus::NotSent,
        },
    }
}

impl RuntimeSessionService {
    /// Captures only inert recovered evidence for transactional resume rollback.
    pub(crate) fn snapshot_restored_steering_recovery(
        &self,
    ) -> std::collections::BTreeMap<(String, String), Vec<SteeringRecoveryReceipt>> {
        self.agent.restored_steering_recovery.clone()
    }

    /// Restores an exact pre-resume snapshot, including any evicted owner.
    pub(crate) fn replace_restored_steering_recovery(
        &mut self,
        snapshot: std::collections::BTreeMap<(String, String), Vec<SteeringRecoveryReceipt>>,
    ) {
        self.agent.restored_steering_recovery = snapshot;
    }

    /// Publishes the newest inert receipt snapshot through existing checkpoint
    /// ownership. Failure retains live receipts; callers must not retry input.
    /// Adapter checkpoints already provide generation-fenced bounded retry.
    pub(crate) fn publish_steering_recovery_checkpoint(&mut self) {
        if self.checkpoint_agent_session_metadata().is_err() {
            let _ = self.append_lifecycle_event(
                crate::runtime::EventKind::Diagnostic,
                r#"{"diagnostic":"steering recovery checkpoint unavailable; accepted input retained, restart evidence may be incomplete"}"#.to_string(),
            );
        }
    }

    /// Captures pending evidence plus bounded terminal history for one exact
    /// pane/conversation. Duplicate actor copies use the transferred turn owner.
    /// Pending exhaustion rejects the checkpoint rather than silently dropping it.
    pub(crate) fn steering_recovery_checkpoint(
        &self,
        pane: &str,
        conversation: &str,
    ) -> Result<Vec<SteeringRecoveryReceipt>> {
        let mut candidates = Vec::new();
        for owner in self.agent.steering_receipts.values().filter(|owner| {
            owner.turn.pane_id == pane && owner.turn.conversation_id == conversation
        }) {
            candidates.extend(
                owner
                    .entries
                    .iter()
                    .map(|entry| project(entry, Some(&owner.turn.turn_id))),
            );
        }
        for entries in self
            .agent
            .pending_deferred_steering
            .iter()
            .chain(self.agent.settled_deferred_steering.iter())
            .chain(self.agent.settled_compaction_steering.iter())
            .filter(|((owner, source, _), _)| owner == pane && source == conversation)
            .map(|(_, entries)| entries)
        {
            candidates.extend(entries.iter().map(|entry| project(entry, None)));
        }
        if let Some(entries) = self.agent.agent_compaction_steering.get(pane) {
            candidates.extend(
                entries
                    .iter()
                    .filter(|entry| entry.conversation == conversation)
                    .map(|entry| project(&entry.receipt, None)),
            );
        }
        if let Some(entries) = self
            .agent
            .restored_steering_recovery
            .get(&(pane.into(), conversation.into()))
        {
            candidates.extend(entries.iter().cloned());
        }
        let mut ids = BTreeSet::new();
        candidates.retain(|entry| ids.insert(entry.id.clone()));
        let (mut pending, terminal): (Vec<_>, Vec<_>) = candidates
            .into_iter()
            .partition(|entry| entry.status == SteeringRecoveryStatus::Pending);
        validate_steering_recovery(&pending)?;
        let mut bytes = pending
            .iter()
            .map(|entry| entry.display.len())
            .sum::<usize>();
        for entry in terminal {
            if pending.len() < STEERING_RECOVERY_ENTRIES
                && bytes.saturating_add(entry.display.len()) <= STEERING_RECOVERY_BYTES
            {
                bytes += entry.display.len();
                pending.push(entry);
            }
        }
        validate_steering_recovery(&pending)?;
        Ok(pending)
    }

    /// Hydrates only inert evidence. No canonical event, input, provider task,
    /// approval or process authority is reconstructed by this operation.
    pub(crate) fn restore_steering_recovery(
        &mut self,
        pane: &str,
        conversation: &str,
        receipts: &[SteeringRecoveryReceipt],
    ) -> Result<()> {
        validate_steering_recovery(receipts)?;
        let key = (pane.to_string(), conversation.to_string());
        if !self.agent.restored_steering_recovery.contains_key(&key)
            && self.agent.restored_steering_recovery.len() >= OWNER_CAPACITY
        {
            self.agent.restored_steering_recovery.pop_first();
        }
        self.agent.restored_steering_recovery.insert(
            key,
            receipts
                .iter()
                .cloned()
                .map(SteeringRecoveryReceipt::after_restart)
                .collect(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
