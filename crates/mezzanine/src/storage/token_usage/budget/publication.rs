//! Off-actor original-operation publication across SQLite and transcript files.
//! Partial success is deliberately retained, never rolled back into a new root.
//! Runtime adoption must still validate live owner, conversation and generation;
//! this storage coordinator cannot grant native/provider/scheduler authority.

use super::*;
use crate::storage::transcript::AgentTranscriptStore;
use membership::BudgetMembership;

impl TokenUsageStore {
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
