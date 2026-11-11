//! Exact local policy outcomes and exclusive live-binding custody. These worker
//! ports accept only runtime-authenticated calls; identifiers do not prove human
//! authority or process retirement. No timer/age can reclaim a binding here.

use super::*;
use rusqlite::{OptionalExtension, params};

/// One complete authenticated root-policy operation; absent fields stay unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetPolicyOperation {
    pub id: String,
    pub namespace: String,
    pub root: BudgetRootId,
    pub input: Option<BudgetOverride>,
    pub output: Option<BudgetOverride>,
}

impl TokenUsageStore {
    /// Reads existing exact custody without creating missing membership. The
    /// native coordinator qualifies this observation under its lifetime lock.
    pub(crate) fn budget_binding_owner(
        &self,
        member: &membership::BudgetMembership,
    ) -> Result<Option<String>> {
        let connection = self.open_budget()?;
        let (original, phase) = membership::membership(&connection, &member.conversation)?
            .ok_or_else(|| MezError::conflict("budget binding membership unavailable"))?;
        if original != *member || phase == BudgetMembershipState::Pending {
            return Err(MezError::conflict("budget binding publication unavailable"));
        }
        let owner: Option<String> = connection
            .query_row(
                "SELECT owner FROM budget_bindings WHERE conversation=?1",
                [&member.conversation],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(owner) = &owner {
            validate_id(owner)?;
        }
        Ok(owner)
    }

    /// Changes only the exact observed owner while actual native lifetime
    /// custody is held. Public ids alone cannot construct this lease. Missing
    /// or changed observations refuse; no expense, policy or attempt is reset.
    pub(crate) fn transfer_native_budget_binding(
        &self,
        member: &membership::BudgetMembership,
        owner: &str,
        previous: Option<&str>,
        lease: &crate::storage::transcript::BudgetBindingLease,
    ) -> Result<()> {
        validate_id(owner)?;
        if !lease.matches(member, owner) {
            return Err(MezError::conflict(
                "native budget binding lease subject mismatch",
            ));
        }
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (original, phase) = membership::membership(&tx, &member.conversation)?
            .ok_or_else(|| MezError::conflict("native budget membership unavailable"))?;
        if original != *member || phase == BudgetMembershipState::Pending {
            return Err(MezError::conflict(
                "native budget membership is not published",
            ));
        }
        let actual: Option<String> = tx
            .query_row(
                "SELECT owner FROM budget_bindings WHERE conversation=?1",
                [&member.conversation],
                |r| r.get(0),
            )
            .optional()?;
        if actual.as_deref() != previous {
            return Err(MezError::conflict(
                "native budget database custody changed before transfer",
            ));
        }
        lease.revalidate()?;
        tx.execute("INSERT INTO budget_bindings(conversation,owner) VALUES(?1,?2) ON CONFLICT(conversation) DO UPDATE SET owner=excluded.owner",
            params![member.conversation, owner])?;
        tx.commit()?;
        Ok(())
    }

    /// Commits one root override operation without resetting lifetime usage.
    /// A lost reply re-queries the original operation, not today's account view.
    /// This repository method does not itself authenticate a human or wake work.
    pub(crate) fn commit_budget_policy(
        &self,
        operation: &BudgetPolicyOperation,
    ) -> Result<BudgetAccount> {
        validate_id(&operation.id)?;
        validate_id(&operation.namespace)?;
        if operation.input.is_none() && operation.output.is_none() {
            return Err(MezError::invalid_args("empty budget policy operation"));
        }
        // Serialization validates BOTH finite fields before any writer admission.
        let body = encode(operation)?;
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if namespace(&tx)? != operation.namespace {
            return Err(MezError::conflict("budget policy namespace mismatch"));
        }
        if let Some((prior, outcome)) = tx
            .query_row(
                "SELECT body,outcome FROM budget_operations WHERE id=?1",
                [&operation.id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if decode::<BudgetPolicyOperation>(&prior)? != *operation {
                return Err(MezError::conflict("budget policy changed operation replay"));
            }
            let account: BudgetAccount = decode(&outcome)?;
            if account.root != operation.root {
                return Err(MezError::conflict("budget policy outcome root mismatch"));
            }
            return Ok(account);
        }
        let mut account = account(&tx, &operation.root)?;
        if let Some(value) = operation.input {
            account.input_override = value;
        }
        if let Some(value) = operation.output {
            account.output_override = value;
        }
        account.policy_revision = account
            .policy_revision
            .checked_add(1)
            .ok_or_else(|| MezError::conflict("budget policy revision overflow"))?;
        let outcome = encode(&account)?;
        tx.execute(
            "UPDATE budget_roots SET body=?1 WHERE id=?2",
            params![outcome, operation.root.as_str()],
        )?;
        tx.execute(
            "INSERT INTO budget_operations(id,root,body,outcome) VALUES(?1,?2,?3,?4)",
            params![operation.id, operation.root.as_str(), body, outcome],
        )?;
        tx.commit()?;
        Ok(account)
    }

    /// Reserves one exact binding after sidecar publication. The runtime owns
    /// native liveness, publication and generation proof, not this owner string.
    /// A second owner refuses regardless of age. Exact replay is idempotent.
    pub(crate) fn claim_budget_binding(
        &self,
        member: &membership::BudgetMembership,
        owner: &str,
    ) -> Result<bool> {
        validate_id(owner)?;
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (original, phase) = membership::membership(&tx, &member.conversation)?
            .ok_or_else(|| MezError::conflict("budget binding membership unavailable"))?;
        if original != *member || phase == BudgetMembershipState::Pending {
            return Err(MezError::conflict("budget binding publication unavailable"));
        }
        let old: Option<String> = tx
            .query_row(
                "SELECT owner FROM budget_bindings WHERE conversation=?1",
                [&member.conversation],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            if old != owner {
                return Err(MezError::conflict("budget conversation already bound"));
            }
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO budget_bindings(conversation,owner) VALUES(?1,?2)",
            params![member.conversation, owner],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Releases only exact original custody after runtime-qualified retirement.
    /// Absence is inert; foreign owner refuses. This never terminalizes attempts.
    pub(crate) fn release_budget_binding(&self, conversation: &str, owner: &str) -> Result<bool> {
        validate_id(owner)?;
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT owner FROM budget_bindings WHERE conversation=?1",
                [conversation],
                |r| r.get(0),
            )
            .optional()?;
        let Some(old) = old else {
            return Ok(false);
        };
        if old != owner {
            return Err(MezError::conflict(
                "budget binding retirement owner mismatch",
            ));
        }
        tx.execute(
            "DELETE FROM budget_bindings WHERE conversation=?1 AND owner=?2",
            params![conversation, owner],
        )?;
        tx.commit()?;
        Ok(true)
    }
}
