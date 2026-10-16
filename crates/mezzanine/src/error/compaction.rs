//! Content-free auxiliary compaction failure provenance.
//!
//! These observations describe local summary validation, not MAAP negotiation,
//! provider delivery counts or authorization to replay work. No source text,
//! prompt, response, HTTP payload or credential belongs in this record.

/// Semantic measurement unit, never a provider generation-token control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionCostUnit {
    /// Whitespace-delimited words.
    Words,
    /// Conservative UTF-8 input-token estimate.
    EstimatedInputTokens,
}

/// Local auxiliary owner that observed the failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionFailurePhase {
    /// Summarizing the current frozen source or chunk.
    Summary,
    /// Combining private accepted chunk summaries.
    Synthesis,
    /// Checking the complete ordinary candidate before publication.
    CompleteCandidate,
}

/// Typed local outcome; none of these dispositions authorize a retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionFailureDisposition {
    /// Blank/malformed output exhausted its finite local budget.
    EmptyOrMalformedExhausted,
    /// A dimension that remains invalid failed to shrink.
    NoProgress,
    /// Still-invalid output followed the already issued nonzero guidance floor.
    GuidanceFloor,
    /// Independent frozen source validation failed.
    SourceIntegrity,
    /// Complete candidate application or fit failed without publication.
    CandidateRejected,
}

impl CompactionFailureDisposition {
    /// Returns a stable content-free cause for the local correction owner.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::EmptyOrMalformedExhausted => "compaction empty/malformed-summary correction budget exhausted",
            Self::NoProgress => "compaction invalid-summary correction made no size progress",
            Self::GuidanceFloor => "compaction invalid-summary correction remains invalid at semantic guidance floor",
            Self::SourceIntegrity => "compaction frozen source validation failed",
            Self::CandidateRejected => "compaction complete candidate rejected before publication",
        }
    }
}

/// Numeric facts captured by the exact auxiliary owner before terminal wrapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionFailureEvidence {
    /// Phase that observed this failure.
    pub phase: CompactionFailurePhase,
    /// First correction's task generation, if correction actually began.
    pub source_generation: Option<u64>,
    /// Current exact generation, not a wire-delivery count.
    pub task_generation: u64,
    /// Last observed validation defect, absent for an unmeasured fit failure.
    pub defect: Option<mez_agent::context_compaction::ModelCompactionSummaryDefect>,
    /// Original frozen allowance and its actual semantic unit.
    pub frozen_allowance: usize,
    /// Unit of the original frozen allowance.
    pub frozen_unit: CompactionCostUnit,
    /// Independently derived complete-fit allowance in estimated input tokens.
    pub effective_input_tokens: Option<usize>,
    /// Measured invalid cost, if a completed output was validated.
    pub measured_cost: Option<usize>,
    /// Unit of measured_cost, when present.
    pub measured_unit: CompactionCostUnit,
    /// Measured UTF-8 bytes, if known.
    pub measured_bytes: Option<usize>,
    /// Canonical byte bound, independent of all token/word units.
    pub byte_bound: usize,
    /// Last issued advisory target, not an acceptance limit or wire cap.
    pub guidance_target: Option<usize>,
    /// Corrections actually admitted for the current source.
    pub corrections_admitted: u32,
    /// Typed exhaustion/integrity/complete-fit outcome.
    pub disposition: CompactionFailureDisposition,
    /// Canonical OpenAI Responses requests omit max_output_tokens. None means
    /// this owner has no resolved OpenAI wire observation, not false support.
    pub openai_max_output_tokens_transmitted: Option<bool>,
}

impl CompactionFailureEvidence {
    /// Renders only typed enums/numeric measurements; no ordinary MAAP counts.
    pub(crate) fn diagnostic_line(&self) -> String {
        format!("Auxiliary compaction: phase={:?}; source_generation={:?}; task_generation={}; defect={:?}; frozen_allowance={} {:?}; effective_input_tokens={:?}; measured_cost={:?} {:?}; bytes={:?}/{}; guidance_target={:?}; corrections_admitted={}; disposition={:?}; openai_max_output_tokens_transmitted={:?}",
            self.phase, self.source_generation, self.task_generation, self.defect,
            self.frozen_allowance, self.frozen_unit, self.effective_input_tokens,
            self.measured_cost, self.measured_unit, self.measured_bytes, self.byte_bound,
            self.guidance_target, self.corrections_admitted, self.disposition,
            self.openai_max_output_tokens_transmitted)
    }
}
