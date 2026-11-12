//! Worker-only config-derived policy projection and exact publication receipts.
//!
//! The primary config remains the only authored global policy. This repository
//! neither reads nor writes that file and cannot attest its physical durability.
//! The publisher must hold the namespace/config writer, validate the complete
//! document, durably publish it, then supply its actually observed digest. A
//! pending or conflicted projection never permits autonomous admission. Recovery
//! reconciles bookkeeping only, never rewrites config or replays provider work.

use super::*;
use mez_agent::token_budget::{BudgetLimits, BudgetReceiptId};
use rusqlite::{OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetConfigPolicy {
    /// Opaque identity of the original primary config, not a caller path.
    pub source: String,
    /// Exact SHA-256 of the validated complete primary document.
    pub digest: String,
    pub limits: BudgetLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetPolicyPublication {
    pub namespace: String,
    pub id: BudgetReceiptId,
    pub expected_revision: u64,
    pub previous_digest: String,
    pub proposed: BudgetConfigPolicy,
    /// Only an explicitly diagnosed third-digest conflict may be superseded.
    pub supersedes: Option<BudgetReceiptId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum BudgetPolicyPublicationStatus {
    Updating,
    Unapplied,
    Committed,
    Conflict { observed_digest: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetPolicyPublicationOutcome {
    pub id: BudgetReceiptId,
    pub revision: u64,
    pub status: BudgetPolicyPublicationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetPolicyProjection {
    pub namespace: String,
    pub revision: u64,
    /// Absence means uninitialized, NOT an unlimited-policy authorization.
    pub committed: Option<BudgetConfigPolicy>,
    pub pending: Option<BudgetPolicyPublication>,
}

impl BudgetPolicyProjection {
    pub(crate) fn admission_policy(&self) -> Option<BudgetLimits> {
        if self.pending.is_some() {
            None
        } else {
            self.committed.as_ref().map(|p| p.limits)
        }
    }

    fn advance(&mut self) -> Result<()> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| MezError::conflict("budget projection revision overflow"))?;
        Ok(())
    }
}

impl BudgetPolicyPublication {
    fn validate(&self) -> Result<()> {
        validate_id(&self.namespace)?;
        validate_id(&self.proposed.source)?;
        validate_digest(&self.previous_digest)?;
        validate_digest(&self.proposed.digest)?;
        // Encoding also checks the shared external quantity ceiling.
        encode(self).map(|_| ())
    }
}

pub(super) fn initialize_schema(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE budget_policy_projection (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            body TEXT NOT NULL CHECK(length(CAST(body AS BLOB))<=4096));
         CREATE TABLE budget_policy_publications (
            id TEXT PRIMARY KEY NOT NULL CHECK(length(id) BETWEEN 1 AND 256),
            body TEXT NOT NULL CHECK(length(CAST(body AS BLOB))<=4096),
            outcome TEXT NOT NULL CHECK(length(CAST(outcome AS BLOB))<=4096));",
    )?;
    let projection = BudgetPolicyProjection {
        namespace: namespace(tx)?,
        revision: 0,
        committed: None,
        pending: None,
    };
    tx.execute(
        "INSERT INTO budget_policy_projection(singleton,body) VALUES(1,?1)",
        [encode(&projection)?],
    )?;
    Ok(())
}

impl TokenUsageStore {
    /// Shared revision/state inspection, not a cache-derived permission grant.
    pub(crate) fn budget_policy_projection(&self) -> Result<BudgetPolicyProjection> {
        let mut connection = self.open_budget()?;
        let tx = connection.transaction()?;
        read_projection(&tx)
    }

    /// Query the original operation after an uncertain reply. This does not
    /// retry any file mutation or replace its outcome with the newest policy.
    pub(crate) fn budget_policy_publication_outcome(
        &self,
        id: &BudgetReceiptId,
    ) -> Result<Option<BudgetPolicyPublicationOutcome>> {
        let connection = self.open_budget()?;
        Ok(read_publication(&connection, id)?.map(|(_, outcome)| outcome))
    }

    /// Atomically install an updating fence before the publisher changes config.
    /// Exact operation replay returns its retained outcome, even after later ops.
    pub(crate) fn begin_budget_policy_publication(
        &self,
        operation: &BudgetPolicyPublication,
    ) -> Result<BudgetPolicyPublicationOutcome> {
        operation.validate()?;
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if namespace(&tx)? != operation.namespace {
            return Err(MezError::conflict("budget policy namespace mismatch"));
        }
        if let Some((original, outcome)) = read_publication(&tx, &operation.id)? {
            if original != *operation {
                return Err(MezError::conflict("budget policy changed replay"));
            }
            return Ok(outcome);
        }
        let mut projection = read_projection(&tx)?;
        if projection.revision != operation.expected_revision {
            return Err(MezError::conflict("budget policy revision changed"));
        }
        if let Some(pending) = &projection.pending {
            let (_, previous) = read_publication(&tx, &pending.id)?
                .ok_or_else(|| MezError::conflict("budget pending publication missing"))?;
            if operation.supersedes.as_ref() != Some(&pending.id)
                || pending.proposed.source != operation.proposed.source
                || !matches!(&previous.status, BudgetPolicyPublicationStatus::Conflict { observed_digest }
                    if observed_digest == &operation.previous_digest)
            {
                return Err(MezError::conflict(
                    "budget policy publication still pending",
                ));
            }
        } else {
            if operation.supersedes.is_some() {
                return Err(MezError::conflict("budget policy conflict source changed"));
            }
            if let Some(current) = &projection.committed
                && (current.source != operation.proposed.source
                    || current.digest != operation.previous_digest)
            {
                return Err(MezError::conflict("budget policy original config changed"));
            }
        }
        projection.advance()?;
        projection.pending = Some(operation.clone());
        let outcome = BudgetPolicyPublicationOutcome {
            id: operation.id.clone(),
            revision: projection.revision,
            status: BudgetPolicyPublicationStatus::Updating,
        };
        tx.execute(
            "INSERT INTO budget_policy_publications(id,body,outcome) VALUES(?1,?2,?3)",
            params![operation.id.as_str(), encode(operation)?, encode(&outcome)?],
        )?;
        write_projection(&tx, &projection)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Reconcile the original operation under the publisher's held writer.
    /// Old digest is unapplied, new is committed, third stays fenced. Supplying a
    /// digest is not file durability proof; the concrete publisher owns that ACK.
    pub(crate) fn reconcile_budget_policy_publication(
        &self,
        operation: &BudgetPolicyPublication,
        observed_digest: &str,
    ) -> Result<BudgetPolicyPublicationOutcome> {
        operation.validate()?;
        validate_digest(observed_digest)?;
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if namespace(&tx)? != operation.namespace {
            return Err(MezError::conflict("budget policy namespace mismatch"));
        }
        let (original, mut outcome) = read_publication(&tx, &operation.id)?
            .ok_or_else(|| MezError::conflict("budget policy publication unavailable"))?;
        if original != *operation {
            return Err(MezError::conflict("budget policy changed replay"));
        }
        if outcome.status != BudgetPolicyPublicationStatus::Updating {
            return Ok(outcome);
        }
        let mut projection = read_projection(&tx)?;
        if projection.pending.as_ref() != Some(operation) || projection.revision != outcome.revision
        {
            return Err(MezError::conflict("budget pending publication changed"));
        }
        projection.advance()?;
        outcome.revision = projection.revision;
        if observed_digest == operation.proposed.digest {
            projection.committed = Some(operation.proposed.clone());
            projection.pending = None;
            outcome.status = BudgetPolicyPublicationStatus::Committed;
        } else if observed_digest == operation.previous_digest && operation.supersedes.is_none() {
            projection.pending = None;
            outcome.status = BudgetPolicyPublicationStatus::Unapplied;
        } else {
            // A superseding operation's old digest is the concurrent edit,
            // not the document backing the retained committed policy. Keep
            // that conflict fenced until a validated new projection commits.
            outcome.status = BudgetPolicyPublicationStatus::Conflict {
                observed_digest: observed_digest.into(),
            };
        }
        tx.execute(
            "UPDATE budget_policy_publications SET outcome=?1 WHERE id=?2",
            params![encode(&outcome)?, operation.id.as_str()],
        )?;
        write_projection(&tx, &projection)?;
        tx.commit()?;
        Ok(outcome)
    }
}

fn validate_digest(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(MezError::invalid_args("invalid budget config digest"));
    }
    Ok(())
}

pub(super) fn read_projection(connection: &Connection) -> Result<BudgetPolicyProjection> {
    let body: String = connection.query_row(
        "SELECT body FROM budget_policy_projection WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let projection: BudgetPolicyProjection = decode(&body)?;
    if projection.namespace != namespace(connection)? {
        return Err(MezError::conflict("budget projection namespace mismatch"));
    }
    if let Some(policy) = &projection.committed {
        validate_id(&policy.source)?;
        validate_digest(&policy.digest)?;
    }
    if let Some(pending) = &projection.pending {
        pending.validate()?;
        if pending.namespace != projection.namespace {
            return Err(MezError::conflict("budget pending namespace mismatch"));
        }
        let (original, outcome) = read_publication(connection, &pending.id)?
            .ok_or_else(|| MezError::conflict("budget pending publication missing"))?;
        if original != *pending
            || outcome.revision != projection.revision
            || !matches!(
                outcome.status,
                BudgetPolicyPublicationStatus::Updating
                    | BudgetPolicyPublicationStatus::Conflict { .. }
            )
        {
            return Err(MezError::conflict("budget pending receipt mismatch"));
        }
    }
    Ok(projection)
}

fn read_publication(
    connection: &Connection,
    id: &BudgetReceiptId,
) -> Result<Option<(BudgetPolicyPublication, BudgetPolicyPublicationOutcome)>> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT body,outcome FROM budget_policy_publications WHERE id=?1",
            [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(body, outcome)| {
        let operation: BudgetPolicyPublication = decode(&body)?;
        operation.validate()?;
        let outcome: BudgetPolicyPublicationOutcome = decode(&outcome)?;
        if &operation.id != id || outcome.id != *id || operation.namespace != namespace(connection)?
        {
            return Err(MezError::conflict(
                "budget policy receipt identity mismatch",
            ));
        }
        if let BudgetPolicyPublicationStatus::Conflict { observed_digest } = &outcome.status {
            validate_digest(observed_digest)?;
        }
        Ok((operation, outcome))
    })
    .transpose()
}

fn write_projection(
    tx: &rusqlite::Transaction<'_>,
    projection: &BudgetPolicyProjection,
) -> Result<()> {
    tx.execute(
        "UPDATE budget_policy_projection SET body=?1 WHERE singleton=1",
        [encode(projection)?],
    )?;
    Ok(())
}
