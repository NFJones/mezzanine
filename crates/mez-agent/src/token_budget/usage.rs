//! Per-attempt monotonic cumulative snapshots, not additive stream fragments.
//! Storage reconciles these under the original root/attempt in one transaction.
//! Failed reconciliation leaves both prefix state and root totals unchanged.

use super::{BudgetAttemptId, BudgetRootId};

/// Checked lifetime totals/deltas. Unknown categories are represented separately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsageTotals {
    /// Inclusive input; no separate cache subtraction/addition.
    pub input: u64,
    /// Reported output; reasoning is already a subset.
    pub output: u64,
}
impl UsageTotals {
    /// Computes both fields before returning; overflow never partially mutates.
    pub fn checked_add(self, delta: Self) -> Result<Self, BudgetUsageError> {
        Ok(Self {
            input: self
                .input
                .checked_add(delta.input)
                .ok_or(BudgetUsageError::Overflow)?,
            output: self
                .output
                .checked_add(delta.output)
                .ok_or(BudgetUsageError::Overflow)?,
        })
    }
}

/// Completeness of each dimension, independent of a known subtotal or true zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsageCoverage {
    /// A terminal snapshot explicitly reported the complete input count.
    pub input_complete: bool,
    /// A terminal snapshot explicitly reported the complete output count.
    pub output_complete: bool,
}

/// One normalized cumulative observation from a concrete attempt. None means
/// unreported, NOT zero. Terminal missing dimensions retain known prefixes but
/// remain incomplete. A final aggregate replaces prefixes rather than adding them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsageSnapshot {
    /// Known cumulative inclusive input at this observation.
    pub input: Option<u64>,
    /// Known cumulative reported output at this observation.
    pub output: Option<u64>,
    /// Local attempt retired; remote billing may still be unknown.
    pub terminal: bool,
}

/// Reconciliation refusal; caller must retain original evidence and fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetUsageError {
    /// Receipt root/attempt does not match its original owner.
    Ownership,
    /// A later cumulative counter moved backwards.
    Regression,
    /// New/different observations arrived after the terminal snapshot.
    Finalized,
    /// Authoritative unsigned arithmetic overflowed.
    Overflow,
}
impl std::fmt::Display for BudgetUsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ownership => "budget usage ownership mismatch",
            Self::Regression => "budget cumulative usage regressed",
            Self::Finalized => "budget terminal usage snapshot changed",
            Self::Overflow => "budget lifetime usage overflow",
        })
    }
}
impl std::error::Error for BudgetUsageError {}

/// Pure state for one original attempt/usage partition. Product storage owns
/// receipt-ID replay/conflict detection and aggregates multiple partitions only
/// when their disjointness is proven. Never feed a total and its parts separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptUsage {
    root: BudgetRootId,
    attempt: BudgetAttemptId,
    known: UsageSnapshot,
    terminal: Option<UsageSnapshot>,
}
impl AttemptUsage {
    /// Starts an unobserved attempt. No expense or completeness is invented.
    pub fn new(root: BudgetRootId, attempt: BudgetAttemptId) -> Self {
        Self {
            root,
            attempt,
            known: UsageSnapshot::default(),
            terminal: None,
        }
    }
    /// Known inclusive subtotal, including partial observations.
    pub fn known(&self) -> UsageTotals {
        UsageTotals {
            input: self.known.input.unwrap_or(0),
            output: self.known.output.unwrap_or(0),
        }
    }
    /// Retained known dimensions, distinguishing an observed zero from absence.
    /// Completeness comes from `coverage`, not from this prefix's presence.
    pub fn observed(&self) -> UsageSnapshot {
        self.known
    }
    /// Unknown final dimensions remain incomplete even if a prefix was known.
    pub fn coverage(&self) -> UsageCoverage {
        self.terminal
            .map_or(UsageCoverage::default(), |s| UsageCoverage {
                input_complete: s.input.is_some(),
                output_complete: s.output.is_some(),
            })
    }
    /// Whether the local attempt has transferred its terminal evidence.
    pub fn is_terminal(&self) -> bool {
        self.terminal.is_some()
    }
    /// Proposes a checked root-total and attempt update together. On any refusal
    /// the caller's originals remain unchanged. Storage must commit both returned
    /// values in the same transaction before acknowledging the receipt.
    pub fn reconcile(
        &self,
        root: &BudgetRootId,
        attempt: &BudgetAttemptId,
        snapshot: UsageSnapshot,
        totals: UsageTotals,
    ) -> Result<(Self, UsageTotals), BudgetUsageError> {
        let mut next = self.clone();
        let delta = next.observe(root, attempt, snapshot)?;
        Ok((next, totals.checked_add(delta)?))
    }
    /// Reconciles monotonically and returns ONLY the new delta. Duplicate terminal
    /// snapshots are inert. Validate/add root totals transactionally before storing
    /// this updated state; this pure method alone is not a durable acknowledgement.
    pub fn observe(
        &mut self,
        root: &BudgetRootId,
        attempt: &BudgetAttemptId,
        snapshot: UsageSnapshot,
    ) -> Result<UsageTotals, BudgetUsageError> {
        if &self.root != root || &self.attempt != attempt {
            return Err(BudgetUsageError::Ownership);
        }
        if let Some(final_snapshot) = self.terminal {
            return if snapshot == final_snapshot {
                Ok(UsageTotals::default())
            } else {
                Err(BudgetUsageError::Finalized)
            };
        }
        let next = UsageSnapshot {
            input: snapshot.input.or(self.known.input),
            output: snapshot.output.or(self.known.output),
            terminal: snapshot.terminal,
        };
        let known = self.known();
        let delta = UsageTotals {
            input: next
                .input
                .unwrap_or(0)
                .checked_sub(known.input)
                .ok_or(BudgetUsageError::Regression)?,
            output: next
                .output
                .unwrap_or(0)
                .checked_sub(known.output)
                .ok_or(BudgetUsageError::Regression)?,
        };
        self.known = next;
        if snapshot.terminal {
            self.terminal = Some(snapshot);
        }
        Ok(delta)
    }
}
