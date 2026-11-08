//! Content-free observations retained with an auxiliary compaction failure.
//! Phase and counters describe the original operation; they never authorize
//! retries, prove wire transmission, or reconstruct accepted model history.

/// Bounded provenance from the exact operation before its actor owner retires.
/// No source text, model response, HTTP payload or credential belongs here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionFailureEvidence {
    pub(crate) phase: &'static str,
    pub(crate) operation_epoch: u64,
    pub(crate) dispatch_generation: u64,
    pub(crate) model_calls: u64,
    pub(crate) serial_depth: usize,
    pub(crate) elapsed_seconds: u64,
}

impl CompactionFailureEvidence {
    /// Renders fixed labels and numeric observations without MAAP recovery facts.
    pub(crate) fn diagnostic_line(&self) -> String {
        format!(
            "Auxiliary compaction: phase={}; operation={}; dispatch={}; model_calls={}; serial_depth={}; elapsed_seconds={}",
            self.phase,
            self.operation_epoch,
            self.dispatch_generation,
            self.model_calls,
            self.serial_depth,
            self.elapsed_seconds,
        )
    }
}
