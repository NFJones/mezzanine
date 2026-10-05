//! Test-only setup-stage timing without payloads, identities or shared state.
//!
//! Records only static stage labels and elapsed durations for the original
//! pipeline. Drop reports incomplete setup, including cancellation. It neither
//! retries work nor changes deadlines, admission or transport ownership.

/// One original pipeline's static setup stage and elapsed timing evidence.
pub(in crate::host::outbound_frontend) struct SetupDiagnostics {
    started: std::time::Instant,
    stage_started: std::time::Instant,
    stage: &'static str,
    complete: bool,
}

impl SetupDiagnostics {
    /// Starts timing at admission without inspecting local or remote content.
    pub(in crate::host::outbound_frontend) fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            stage_started: now,
            stage: "admit",
            complete: false,
        }
    }

    /// Advances to a caller-supplied static stage; no task or I/O is started.
    pub(in crate::host::outbound_frontend) fn advance(&mut self, stage: &'static str) {
        self.stage = stage;
        self.stage_started = std::time::Instant::now();
    }

    /// Suppresses diagnostics after the first successful delivery.
    pub(in crate::host::outbound_frontend) fn complete(&mut self) {
        self.complete = true;
    }

    /// Returns only static-stage and duration evidence for unfinished setup.
    /// Successful completion has no report, including host-only delivery.
    fn report(&self) -> Option<(&'static str, u128, u128)> {
        (!self.complete).then(|| {
            (
                self.stage,
                self.stage_started.elapsed().as_millis(),
                self.started.elapsed().as_millis(),
            )
        })
    }
}

impl Drop for SetupDiagnostics {
    fn drop(&mut self) {
        if let Some((stage, stage_ms, total_ms)) = self.report() {
            eprintln!(
                "outbound fixture incomplete setup: stage={} stage_ms={} total_ms={}",
                stage, stage_ms, total_ms
            );
        }
    }
}

#[cfg(test)]
mod tests;
