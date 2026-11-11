//! Off-actor original-operation publication across SQLite and transcript files.
//! Partial success is deliberately retained, never rolled back into a new root.
//! Runtime adoption must still validate live owner, conversation and generation;
//! this storage coordinator cannot grant native/provider/scheduler authority.

use super::*;
use crate::storage::transcript::AgentTranscriptStore;
use membership::BudgetMembership;

impl TokenUsageStore {
    /// Original-store restore preflight. This performs no publication or expense
    /// mutation and returns genuine lifetime custody, not a decoded capability.
    pub(crate) fn restore_native_budget_membership(
        &self,
        transcripts: &AgentTranscriptStore,
        reference: &mez_agent::token_budget::BudgetCheckpointReference,
    ) -> Result<(
        BudgetMembership,
        crate::storage::transcript::BudgetBindingLease,
    )> {
        if self.budget_namespace()? != reference.namespace.as_str() {
            return Err(MezError::conflict("budget restore namespace mismatch"));
        }
        let (member, lease) = transcripts.reacquire_native_budget_binding(reference)?;
        if self.budget_membership(&reference.conversation)?
            != Some((member.clone(), BudgetMembershipState::Active))
            || self.budget_binding_owner(&member)?.as_deref() != Some(reference.owner.as_str())
        {
            return Err(MezError::conflict(
                "budget restore database provenance mismatch",
            ));
        }
        Ok((member, lease))
    }

    /// Native runtime publication retains actual kernel lifetime custody through
    /// actor adoption and every issued worker. A new owner can recover only after
    /// all original holders close the same pinned inode; persisted labels and
    /// age confer no right to reclaim. Intent -> DB CAS -> completed mirror ->
    /// Active are separate durable boundaries, not a cross-store transaction.
    pub(crate) fn publish_native_budget_membership(
        &self,
        transcripts: &AgentTranscriptStore,
        member: &BudgetMembership,
        owner: &str,
        retained: Option<&crate::storage::transcript::BudgetBindingLease>,
    ) -> Result<crate::storage::transcript::BudgetBindingLease> {
        validate_id(owner)?;
        let reference = transcripts.conversation_budget_membership(&member.conversation)?;
        if reference
            .as_ref()
            .is_some_and(|original| original != member)
        {
            return Err(MezError::conflict(
                "native budget publication metadata changed",
            ));
        }
        let phase = self.prepare_budget_membership(member)?;
        if phase != BudgetMembershipState::Pending && reference.is_none() {
            return Err(MezError::conflict(
                "native published budget metadata is missing",
            ));
        }
        transcripts.publish_conversation_budget_membership(member)?;
        if phase == BudgetMembershipState::Pending {
            self.advance_budget_membership(
                member,
                BudgetMembershipState::Pending,
                BudgetMembershipState::Published,
            )?;
        }
        let previous = self.budget_binding_owner(member)?;
        let lease = transcripts.prepare_native_budget_binding(
            member,
            owner,
            previous.as_deref(),
            phase == BudgetMembershipState::Active,
            retained,
        )?;
        self.transfer_native_budget_binding(member, owner, previous.as_deref(), &lease)?;
        transcripts.complete_native_budget_binding(&lease)?;
        if phase != BudgetMembershipState::Active {
            self.advance_budget_membership(
                member,
                BudgetMembershipState::Published,
                BudgetMembershipState::Active,
            )?;
        }
        Ok(lease)
    }

    /// Publishes the exact metadata and binding companion before activation.
    /// Repeating after an uncertain write resumes this same operation only.
    /// No provider or effect is retried. The owner is runtime-generated custody,
    /// not a supplied process id, age-based lease or proof of caller authority.
    pub(crate) fn publish_budget_membership(
        &self,
        transcripts: &AgentTranscriptStore,
        member: &BudgetMembership,
        owner: &str,
    ) -> Result<()> {
        validate_id(owner)?;
        let reference = transcripts.conversation_budget_membership(&member.conversation)?;
        if reference
            .as_ref()
            .is_some_and(|original| original != member)
        {
            return Err(MezError::conflict("budget publication metadata changed"));
        }
        let phase = self.prepare_budget_membership(member)?;
        if phase != BudgetMembershipState::Pending && reference.is_none() {
            return Err(MezError::conflict("published budget metadata is missing"));
        }
        if phase == BudgetMembershipState::Active
            && !transcripts.has_budget_binding_mirror(member, owner)?
        {
            return Err(MezError::conflict(
                "active budget binding mirror is missing",
            ));
        }
        transcripts.publish_conversation_budget_membership(member)?;
        if phase == BudgetMembershipState::Pending {
            self.advance_budget_membership(
                member,
                BudgetMembershipState::Pending,
                BudgetMembershipState::Published,
            )?;
        }
        self.claim_budget_binding(member, owner)?;
        transcripts.publish_budget_binding_mirror(member, owner)?;
        if phase != BudgetMembershipState::Active {
            self.advance_budget_membership(
                member,
                BudgetMembershipState::Published,
                BudgetMembershipState::Active,
            )?;
        }
        Ok(())
    }
}
