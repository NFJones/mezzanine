//! Test-only host initialization timing with no payload or identity retention.
//!
//! Static stages distinguish trust, routing, actor admission and publication.
//! Slow completed stages and unfinished cancellation are observations, not proof
//! of a cause. Instrumentation changes no deadline, task, retry or ownership.

/// One initialization attempt's local timing and static stage.
pub(in crate::host) struct InitializeDiagnostics {
    started: std::time::Instant,
    stage_started: std::time::Instant,
    stage: &'static str,
    complete: bool,
}

impl InitializeDiagnostics {
    /// Begins timing before protected principal validation.
    pub(in crate::host) fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            stage_started: now,
            stage: "trust",
            complete: false,
        }
    }

    /// Reports a slow finished stage before entering the next fixed label.
    pub(in crate::host) fn advance(&mut self, stage: &'static str) {
        self.report_slow();
        self.stage = stage;
        self.stage_started = std::time::Instant::now();
    }

    /// Records successful publication without emitting an incomplete report.
    pub(in crate::host) fn complete(&mut self) {
        self.report_slow();
        self.complete = true;
    }

    /// Emits only static stage and durations when a completed stage was slow.
    fn report_slow(&self) {
        if let Some((stage, stage_ms, total_ms)) = self.slow_report() {
            eprintln!(
                "host fixture slow initialize: stage={} stage_ms={} total_ms={}",
                stage, stage_ms, total_ms
            );
        }
    }

    /// Returns content-free timing only after the fixed slow-stage threshold.
    fn slow_report(&self) -> Option<(&'static str, u128, u128)> {
        (self.stage_started.elapsed() >= std::time::Duration::from_millis(100))
            .then(|| self.timing())
    }

    /// Completed reply publication must not be reported as incomplete setup.
    fn incomplete_report(&self) -> Option<(&'static str, u128, u128)> {
        (!self.complete).then(|| self.timing())
    }

    /// Retains no request content, paths or identities in diagnostic evidence.
    fn timing(&self) -> (&'static str, u128, u128) {
        (
            self.stage,
            self.stage_started.elapsed().as_millis(),
            self.started.elapsed().as_millis(),
        )
    }
}

impl Drop for InitializeDiagnostics {
    fn drop(&mut self) {
        if let Some((stage, stage_ms, total_ms)) = self.incomplete_report() {
            eprintln!(
                "host fixture incomplete initialize: stage={} stage_ms={} total_ms={}",
                stage, stage_ms, total_ms
            );
        }
    }
}

#[cfg(test)]
mod tests;
