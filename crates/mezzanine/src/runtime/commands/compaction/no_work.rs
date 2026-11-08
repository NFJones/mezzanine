//! Content-free manual diagnostics; canonical planning owns source selection.

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
