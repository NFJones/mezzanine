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
    /// Auxiliary provider inference failed rather than completing a summary.
    ProviderInference,
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
    /// Existing bounded transport policy cannot admit another exact retry.
    TransportExhausted,
    /// Provider generation cutoff remains after the bounded auxiliary helper.
    OutputLimit,
    /// Context splitting cannot form a smaller eligible private request.
    ContextFloor,
    /// Other nonretryable provider failure, not a semantic rejection.
    ProviderRejected,
}

impl CompactionFailureDisposition {
    /// Returns a stable content-free cause for the local correction owner.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::EmptyOrMalformedExhausted => {
                "compaction empty/malformed-summary correction budget exhausted"
            }
            Self::NoProgress => "compaction invalid-summary correction made no size progress",
            Self::GuidanceFloor => {
                "compaction invalid-summary correction remains invalid at semantic guidance floor"
            }
            Self::SourceIntegrity => "compaction frozen source validation failed",
            Self::CandidateRejected => "compaction complete candidate rejected before publication",
            Self::TransportExhausted => "compaction auxiliary transport retry exhausted",
            Self::OutputLimit => "compaction auxiliary provider generation limit",
            Self::ContextFloor => "compaction auxiliary context split floor",
            Self::ProviderRejected => "compaction auxiliary provider request rejected",
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
    pub frozen_allowance: Option<usize>,
    /// Unit of the original frozen allowance.
    pub frozen_unit: CompactionCostUnit,
    /// Independently derived advisory final-fit target in estimated input
    /// tokens, not semantic acceptance authority; complete request fit decides.
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
    /// Unit attached to the issued target, not inferred from final-fit guidance.
    pub guidance_unit: CompactionCostUnit,
    /// Progress on the measured dimension relative to a comparable predecessor;
    /// absent when there is no such observation, never inferred as zero retries.
    pub size_progress: Option<bool>,
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
        format!(
            "Auxiliary compaction: phase={:?}; source_generation={:?}; task_generation={}; defect={:?}; frozen_allowance={:?} {:?}; final_fit_guidance_input_tokens={:?}; measured_cost={:?} {:?}; bytes={:?}/{}; guidance_target={:?} {:?}; size_progress={:?}; corrections_admitted={}; disposition={:?}; openai_max_output_tokens_transmitted={:?}",
            self.phase,
            self.source_generation,
            self.task_generation,
            self.defect,
            self.frozen_allowance,
            self.frozen_unit,
            self.effective_input_tokens,
            self.measured_cost,
            self.measured_unit,
            self.measured_bytes,
            self.byte_bound,
            self.guidance_target,
            self.guidance_unit,
            self.size_progress,
            self.corrections_admitted,
            self.disposition,
            self.openai_max_output_tokens_transmitted
        )
    }
}
