//! Closed-prefix selection with independent physical and model-facing domains.
//!
//! Selective projections unite complete turn and range boundaries into atoms.
//! Their budgets use summary costs while retained/publication counts stay physical.
//! An uncovered exact/open turn remains a barrier even inside a merged atom.
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

/// A physical prefix atom preserves both whole turns and whole selective ranges.
/// Its model cost excludes covered raw rows and includes their summary once.
struct ProjectedAtom {
    /// Exclusive physical row boundary in the active window.
    end: usize,
    /// Cost of the canonical replacement rather than the archived raw bytes.
    words: usize,
    /// Every uncovered original turn remains closed and eligible.
    closed: bool,
}

impl<'a> ManualCompactionSelection<'a> {
    /// Selects a forced prefix using summary-aware costs without cutting either
    /// a complete turn or an authoritative range. Counts remain physical rows;
    /// covered groups cannot authorize skipping an uncovered exact/open barrier.
    pub(super) fn select_projected(
        logical: u64,
        rows: &'a [TranscriptEntry],
        ranges: &[crate::storage::transcript::AgentCompactionRange],
        budget: usize,
        percent: usize,
    ) -> Result<Self> {
        let count = runtime_compact_active_transcript_entry_count(logical, rows.len());
        let active = &rows[rows.len().saturating_sub(count)..];
        let budget_words = runtime_compact_retained_context_tail_budget_words(budget, percent);
        if active.is_empty() {
            return Ok(Self::select(logical, rows, 0, budget, percent));
        }
        let first = active[0].sequence;
        let last = active[count - 1].sequence;
        if ranges.iter().any(|range| {
            (range.first_sequence < first && range.through_sequence >= first)
                || (range.first_sequence <= last && range.through_sequence > last)
        }) {
            return Err(MezError::invalid_state(
                "active raw window splits a selective range",
            ));
        }
        let covered = |sequence| {
            ranges
                .iter()
                .any(|range| (range.first_sequence..=range.through_sequence).contains(&sequence))
        };
        let mut atoms = Vec::new();
        let mut start = 0;
        for end in 1..=count {
            let boundary = end == count
                || (active[end - 1].turn_id != active[end].turn_id
                    && !ranges.iter().any(|range| {
                        range.first_sequence <= active[end - 1].sequence
                            && range.through_sequence >= active[end].sequence
                    }));
            if !boundary {
                continue;
            }
            let source = &active[start..end];
            let words = source
                .iter()
                .map(|row| {
                    if let Some(range) = ranges
                        .iter()
                        .find(|range| range.first_sequence == row.sequence)
                    {
                        AGENT_COMPACT_TRANSCRIPT_ENTRY_CONTEXT_OVERHEAD_WORDS.saturating_add(
                            mez_agent::provider_text_input_token_estimate(&range.summary),
                        )
                    } else if covered(row.sequence) {
                        0
                    } else {
                        runtime_compact_transcript_entry_context_words(row)
                    }
                })
                .fold(0usize, usize::saturating_add);
            let closed = runtime_compact_transcript_execution_groups(source)
                .iter()
                .all(|group| {
                    let uncovered = source[group.clone()]
                        .iter()
                        .filter(|row| !covered(row.sequence))
                        .collect::<Vec<_>>();
                    uncovered.is_empty()
                        || uncovered
                            .iter()
                            .any(|row| row.role == TranscriptRole::Assistant)
                        || uncovered
                            .iter()
                            .all(|row| runtime_compaction_control_entry_is_summarizable(row))
                });
            atoms.push(ProjectedAtom { end, words, closed });
            start = end;
        }
        let closed_prefix = atoms
            .iter()
            .position(|atom| !atom.closed)
            .unwrap_or(atoms.len());
        let mut tail_start = count;
        let mut tail_words = 0usize;
        for (index, atom) in atoms.iter().enumerate().rev() {
            if index < closed_prefix && tail_words.saturating_add(atom.words) >= budget_words {
                break;
            }
            tail_words = tail_words.saturating_add(atom.words);
            tail_start = if index == 0 { 0 } else { atoms[index - 1].end };
        }
        let mut selected = atoms
            .iter()
            .take(closed_prefix)
            .take_while(|atom| atom.end <= tail_start)
            .map(|atom| atom.end)
            .last()
            .unwrap_or(0);
        if selected == 0 && closed_prefix > 0 {
            selected = atoms[0].end;
        }
        let retained_words = if selected == 0 {
            atoms
                .iter()
                .map(|atom| atom.words)
                .fold(0usize, usize::saturating_add)
        } else {
            0
        };
        let no_work = (selected == 0).then_some(if retained_words > budget_words {
            NoWorkReason::IrreducibleExactTail
        } else {
            NoWorkReason::NoEligibleClosedPrefix
        });
        Ok(Self {
            entries: &active[..selected],
            retained_entries: (count - selected) as u64,
            no_work,
            retained_words,
            budget_words,
        })
    }
}
