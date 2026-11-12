//! Transfer an already-decoded stream prefix when its request future is lost.
//!
//! This is stack custody, not another accounting ledger or an async Drop task.
//! Normal observation retention disarms it before cancellable delivery. On
//! cancellation/unwind it synchronously hands one immutable observation to the
//! existing retirement journal and constant-space auxiliary expense owner.
//! It supplies neither budget permission nor a durable receipt acknowledgment.

use super::{FinalWireObservation, ModelRequest, ProviderWireObservationContext};
use std::sync::atomic::Ordering;

pub(super) struct StreamUsageCustody<'request, 'observer> {
    context: &'request ProviderWireObservationContext<'observer>,
    request: &'request ModelRequest,
    usage: Option<mez_agent::ModelTokenUsage>,
    retained_before: u64,
}

impl<'request, 'observer> StreamUsageCustody<'request, 'observer> {
    pub(super) fn new(
        context: &'request ProviderWireObservationContext<'observer>,
        request: &'request ModelRequest,
    ) -> Self {
        Self {
            context,
            request,
            usage: None,
            retained_before: context.retained_observations.load(Ordering::Acquire),
        }
    }

    /// Replace the cumulative prefix, never add frames. Legacy numeric usage
    /// cannot qualify a zero-only report or independent dimension completeness.
    pub(super) fn update(&mut self, usage: mez_agent::ModelTokenUsage) {
        if self.usage.is_some() || !usage.is_zero() {
            self.usage = Some(usage);
        }
    }

    /// A decoder with explicit receipt presence can also qualify a zero-only
    /// report. This still does not establish independent dimension coverage.
    pub(super) fn update_reported(&mut self, usage: mez_agent::ModelTokenUsage) {
        self.usage = Some(usage);
    }

    /// Normal retention must receive the same qualified presence before it can
    /// disarm the fallback, even when every numeric counter is zero.
    pub(super) fn reported_usage(&self) -> Option<mez_agent::ModelTokenUsage> {
        self.usage
    }
}

impl Drop for StreamUsageCustody<'_, '_> {
    fn drop(&mut self) {
        // The existing normal path retained this attempt before awaiting its
        // channel/ACK. A later cancellation must not add its prefix again.
        if self.context.retained_observations.load(Ordering::Acquire) != self.retained_before {
            return;
        }
        if let Some(usage) = self.usage.take() {
            let observation = self.context.build_wire_observation(
                self.request,
                1,
                None,
                FinalWireObservation {
                    final_wire_diagnostics: None,
                    response_diagnostics: None,
                    elapsed_ms: None,
                    reported_usage: None,
                },
                Some(usage),
                false,
                Some("interrupted".into()),
            );
            self.context.retain_wire_observation(&observation);
        }
    }
}
