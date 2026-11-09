//! Content-free source/admission diagnostics; canonical planning owns selection.

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
    /// Fixed framing plus stable/protected source exceeds ordinary capacity.
    ProtectedInputExceedsBudget,
    /// No prior head or optional source can be removed under the raw allowance.
    RetainedRawTailUnchanged,
    /// Automatic pressure cannot recompact the same published source. Explicit
    /// manual selection does not use this gate.
    UnchangedPublishedSource { head_revision: u64 },
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
            Self::ProtectedInputExceedsBudget => "protected-input-exceeds-budget",
            Self::RetainedRawTailUnchanged => "retained-raw-tail-unchanged",
            Self::UnchangedPublishedSource { .. } => "unchanged-published-source",
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
            Self::ProtectedInputExceedsBudget => {
                "agent: context unavailable; fixed request framing and protected source exceed model input capacity"
            }
            Self::RetainedRawTailUnchanged => {
                "agent: compaction skipped; raw tail is unchanged under the selected retention allowance"
            }
            Self::UnchangedPublishedSource { .. } => {
                "agent: automatic compaction skipped; published source has no fresh eligible closed history"
            }
        }
    }
}
