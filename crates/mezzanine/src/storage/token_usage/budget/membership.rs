//! Immutable membership and exact publication phase. No age-based binding
//! reclamation or inferred ancestry is admitted by this repository.

use super::*;
use rusqlite::{OptionalExtension, params};

/// Immutable registration payload, retained through all publication phases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetMembership {
    pub namespace: String,
    pub root: BudgetRootId,
    pub conversation: String,
    pub parent: Option<String>,
    pub operation: String,
    pub started_at: u64,
    pub legacy_coverage_gap: bool,
}

/// Database phase is not proof of the actual filesystem publication by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetMembershipState {
    Pending,
    Published,
    Active,
}
impl BudgetMembershipState {
    fn number(self) -> i64 {
        match self {
            Self::Pending => 0,
            Self::Published => 1,
            Self::Active => 2,
        }
    }
    fn from_number(n: i64) -> Result<Self> {
        match n {
            0 => Ok(Self::Pending),
            1 => Ok(Self::Published),
            2 => Ok(Self::Active),
            _ => Err(MezError::invalid_state("invalid budget membership phase")),
        }
    }
}

impl BudgetMembership {
    pub(crate) fn validate(&self) -> Result<()> {
        validate_id(&self.namespace)?;
        validate_id(&self.operation)?;
        mez_agent::transcript::validate_conversation_id(&self.conversation)?;
        if self.conversation.len() > 256 {
            return Err(MezError::invalid_args(
                "budget conversation identity too long",
            ));
        }
        if let Some(parent) = &self.parent {
            mez_agent::transcript::validate_conversation_id(parent)?;
            if parent == &self.conversation || parent.len() > 256 {
                return Err(MezError::invalid_args("invalid budget parent identity"));
            }
        }
        Ok(())
    }
}

impl TokenUsageStore {
    /// Registers one original root or descendant before publishing sidecars.
    /// Parent must already be active; immutable insertion prevents cycles. Exact
    /// replay returns its original phase; changed replay never updates ownership.
    pub(crate) fn prepare_budget_membership(
        &self,
        member: &BudgetMembership,
    ) -> Result<BudgetMembershipState> {
        member.validate()?;
        let body = encode(member)?;
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if namespace(&tx)? != member.namespace {
            return Err(MezError::conflict("budget namespace mismatch"));
        }
        if let Some((existing, state)) = membership(&tx, &member.conversation)? {
            if existing != *member {
                return Err(MezError::conflict("budget membership changed replay"));
            }
            return Ok(state);
        }
        let pending: i64 = tx.query_row(
            "SELECT COUNT(*) FROM budget_memberships WHERE state IN (0,1)",
            [],
            |r| r.get(0),
        )?;
        if pending >= 4096 {
            return Err(MezError::invalid_state(
                "budget pending membership capacity reached",
            ));
        }
        if let Some(parent) = &member.parent {
            let (original, state) = membership(&tx, parent)?
                .ok_or_else(|| MezError::conflict("budget parent unavailable"))?;
            if original.root != member.root
                || original.namespace != member.namespace
                || state != BudgetMembershipState::Active
            {
                return Err(MezError::conflict(
                    "budget parent is not active in original root",
                ));
            }
        } else {
            let original = BudgetAccount {
                root: member.root.clone(),
                known: UsageTotals::default(),
                input_override: BudgetOverride::Inherit,
                output_override: BudgetOverride::Inherit,
                accounting_revision: 0,
                policy_revision: 0,
                started_at: member.started_at,
                legacy_coverage_gap: member.legacy_coverage_gap,
            };
            tx.execute(
                "INSERT INTO budget_roots(id,body) VALUES(?1,?2)",
                params![member.root.as_str(), encode(&original)?],
            )?;
        }
        tx.execute("INSERT INTO budget_memberships(conversation,root,operation,state,body) VALUES(?1,?2,?3,0,?4)",
            params![member.conversation, member.root.as_str(), member.operation, body])?;
        tx.commit()?;
        Ok(BudgetMembershipState::Pending)
    }

    /// Observes current immutable membership; absent is not a fresh root.
    pub(crate) fn budget_membership(
        &self,
        conversation: &str,
    ) -> Result<Option<(BudgetMembership, BudgetMembershipState)>> {
        membership(&self.open_budget()?, conversation)
    }

    /// Advances one adjacent phase only after the runtime's exact qualified
    /// publication receipt. This method supplies no filesystem/binding witness.
    pub(crate) fn advance_budget_membership(
        &self,
        member: &BudgetMembership,
        expected: BudgetMembershipState,
        next: BudgetMembershipState,
    ) -> Result<BudgetMembershipState> {
        member.validate()?;
        if next.number() != expected.number() + 1 {
            return Err(MezError::invalid_args(
                "budget membership phases must be adjacent",
            ));
        }
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (original, phase) = membership(&tx, &member.conversation)?
            .ok_or_else(|| MezError::conflict("budget membership unavailable"))?;
        if original != *member || namespace(&tx)? != member.namespace {
            return Err(MezError::conflict("budget membership operation mismatch"));
        }
        if phase == next {
            return Ok(next);
        }
        if phase != expected {
            return Err(MezError::conflict("budget membership phase mismatch"));
        }
        tx.execute(
            "UPDATE budget_memberships SET state=?1 WHERE conversation=?2",
            params![next.number(), member.conversation],
        )?;
        tx.commit()?;
        Ok(next)
    }
}

pub(super) fn membership(
    connection: &Connection,
    conversation: &str,
) -> Result<Option<(BudgetMembership, BudgetMembershipState)>> {
    let row: Option<(String, String, String, i64)> = connection
        .query_row(
            "SELECT body,root,operation,state FROM budget_memberships WHERE conversation=?1",
            [conversation],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    row.map(|(body, root, operation, state)| {
        let member: BudgetMembership = decode(&body)?;
        member.validate()?;
        if member.conversation != conversation
            || member.root.as_str() != root
            || member.operation != operation
            || member.namespace != namespace(connection)?
        {
            return Err(MezError::conflict(
                "stored budget membership identity mismatch",
            ));
        }
        Ok((member, BudgetMembershipState::from_number(state)?))
    })
    .transpose()
}
