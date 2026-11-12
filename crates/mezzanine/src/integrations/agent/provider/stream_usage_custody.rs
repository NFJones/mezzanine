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
    presence: Option<mez_agent::ModelTokenUsagePresence>,
    retained_before: u64,
    attempt_index: usize,
    retry_reason: Option<&'static str>,
}

impl<'request, 'observer> StreamUsageCustody<'request, 'observer> {
    pub(super) fn new(
        context: &'request ProviderWireObservationContext<'observer>,
        request: &'request ModelRequest,
    ) -> Self {
        Self::for_attempt(context, request, 1, None)
    }

    /// Each adapter-internal exchange needs its own custody generation and
    /// ordinal. A settled first attempt cannot disarm a later attempt's guard.
    pub(super) fn for_attempt(
        context: &'request ProviderWireObservationContext<'observer>,
        request: &'request ModelRequest,
        attempt_index: usize,
        retry_reason: Option<&'static str>,
    ) -> Self {
        Self {
            context,
            request,
            usage: None,
            presence: None,
            retained_before: context.retained_observations.load(Ordering::Acquire),
            attempt_index,
            retry_reason,
        }
    }

    /// Replace a cumulative prefix, never add frames. Primary source evidence
    /// qualifies zero independently of optional cache fields. Presence is not
    /// proof of final/composite coverage or worker retirement.
    pub(super) fn update_with_presence(
        &mut self,
        usage: mez_agent::ModelTokenUsage,
        presence: mez_agent::ModelTokenUsagePresence,
    ) {
        self.presence = Some(presence);
        if self.usage.is_some()
            || !usage.is_zero()
            || usage.cached_input_tokens.is_some()
            || usage.cache_write_input_tokens.is_some()
            || presence.any()
        {
            self.usage = Some(usage);
        }
    }

    /// Transfer numeric receipt and its source evidence together, never infer
    /// either dimension from a normalized default or a logical retry total.
    pub(super) fn reported_evidence(
        &self,
    ) -> Option<(
        mez_agent::ModelTokenUsage,
        mez_agent::ModelTokenUsagePresence,
    )> {
        self.usage.zip(self.presence)
    }

    /// A source parser has independently qualified at least one numeric field,
    /// possibly only a zero reasoning/cache subset. Primary dimensions remain
    /// independently absent when they were not actually reported.
    pub(super) fn update_reported_with_presence(
        &mut self,
        usage: mez_agent::ModelTokenUsage,
        presence: mez_agent::ModelTokenUsagePresence,
    ) {
        self.usage = Some(usage);
        self.presence = Some(presence);
    }

    pub(super) fn reported_presence(&self) -> Option<mez_agent::ModelTokenUsagePresence> {
        self.presence
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
                self.attempt_index,
                self.retry_reason,
                FinalWireObservation {
                    final_wire_diagnostics: None,
                    response_diagnostics: None,
                    elapsed_ms: None,
                    usage_evidence: super::WireUsageEvidence::Concrete(None),
                    usage_presence: self.presence,
                },
                Some(usage),
                false,
                Some("interrupted".into()),
            );
            self.context.retain_wire_observation(&observation);
        }
    }
}
