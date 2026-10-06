//! Typed no-work outcomes from the existing contiguous closed-prefix selector.
//!
//! Selection does not read storage, queue a model, publish a summary or skip an
//! exact/open leading barrier. Budget values describe retained-tail size; once
//! forced selection found no eligible prefix, fitting that budget is not the
//! eligibility explanation. Missing logical/durable source remains distinct,
//! and irreducible retained source retains its established diagnostic code.

use super::*;

/// Stable, content-free manual no-work explanation, never completed compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoWorkReason {
    /// No logical replay entries exist for the bound conversation.
    NoTranscriptEntries,
    /// Logical entries exist but no installed durable source is available.
    NoDurableTranscript,
    /// Forced selection still has no eligible contiguous closed prefix.
    NoEligibleClosedPrefix,
    /// Exact/open retained source cannot meet the requested retention allowance.
    IrreducibleExactTail,
}

impl NoWorkReason {
    /// Returns existing skip codes plus the eligibility-specific replacement for
    /// misleading budget-fit feedback. Codes contain no transcript content.
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::NoTranscriptEntries => "no-transcript-entries",
            Self::NoDurableTranscript => "no-durable-transcript",
            Self::NoEligibleClosedPrefix => "no-eligible-closed-prefix",
            Self::IrreducibleExactTail => "irreducible-exact-retained-tail",
        }
    }

    /// Explains source eligibility without claiming a model ran or a summary
    /// completed. Retained size metrics may be appended separately by the caller.
    pub(super) fn status(self) -> &'static str {
        match self {
            Self::NoTranscriptEntries => {
                "agent: compact skipped; no transcript entries are available"
            }
            Self::NoDurableTranscript => {
                "agent: compact skipped; no durable transcript entries are available"
            }
            Self::NoEligibleClosedPrefix => {
                "agent: compact skipped; no eligible closed transcript prefix is available; leading protected or unfinished groups must remain exact"
            }
            Self::IrreducibleExactTail => {
                "agent: compaction skipped; exact unfinished transcript tail exceeds retention budget"
            }
        }
    }
}

/// Borrowed selected prefix and exact retained counts from the established
/// selector, with typed no-work ownership instead of a budget-fit guess.
pub(super) struct ManualCompactionSelection<'a> {
    /// Existing selector's contiguous prefix; no rows are reordered or skipped.
    pub(super) entries: &'a [TranscriptEntry],
    /// Active raw rows remaining after this selection.
    pub(super) retained_entries: u64,
    /// Present only when no model input was selected.
    pub(super) no_work: Option<NoWorkReason>,
    /// Content-free retained-tail size and allowance for diagnostics.
    pub(super) retained_words: usize,
    pub(super) budget_words: usize,
}

impl<'a> ManualCompactionSelection<'a> {
    /// Uses existing final-prefix semantics and classifies only their empty
    /// result. It never replaces the forced-retention policy or source closure.
    pub(super) fn select(
        logical: u64,
        rows: &'a [TranscriptEntry],
        retained: u64,
        budget: usize,
        percent: usize,
    ) -> Self {
        let entries = runtime_compact_transcript_entries_for_summary(logical, rows, retained);
        let retained_entries = u64::try_from(
            runtime_compact_active_transcript_entry_count(logical, rows.len())
                .saturating_sub(entries.len()),
        )
        .unwrap_or(u64::MAX);
        let budget_words = runtime_compact_retained_context_tail_budget_words(budget, percent);
        let retained_words = if entries.is_empty() {
            runtime_compact_retained_transcript_tail_context_words(logical, rows, retained_entries)
        } else {
            0
        };
        let no_work = if !entries.is_empty() {
            None
        } else if logical == 0 {
            Some(NoWorkReason::NoTranscriptEntries)
        } else if rows.is_empty() {
            Some(NoWorkReason::NoDurableTranscript)
        } else if retained_words > budget_words {
            Some(NoWorkReason::IrreducibleExactTail)
        } else {
            Some(NoWorkReason::NoEligibleClosedPrefix)
        };
        Self {
            entries,
            retained_entries,
            no_work,
            retained_words,
            budget_words,
        }
    }
}
