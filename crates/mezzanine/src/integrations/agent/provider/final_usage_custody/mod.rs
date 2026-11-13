//! Final counter evidence, distinct from transport retirement and prefixes.
//!
//! Only a native adapter's recognized unary or explicit terminal stream report
//! creates this value. Final presence qualifies each dimension independently;
//! cache/reasoning subsets alone never become complete primary totals.

use super::*;
use mez_agent::token_budget::UsageSnapshot;

#[cfg(test)]
mod tests;

/// A completed unary chat object, not an incremental chunk or usage-only frame.
pub(super) fn chat_body_is_terminal(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    value
        .get("choices")
        .and_then(|v| v.as_array())
        .is_some_and(|choices| {
            !choices.is_empty()
                && choices.iter().all(|choice| {
                    choice
                        .get("finish_reason")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| !s.is_empty())
                })
        })
}

/// Not reconstructible from an observation, receipt, success flag or ACK.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ProviderFinalUsage {
    observation: ProviderWireRequestObservation,
    usage: mez_agent::ModelTokenUsage,
    presence: mez_agent::ModelTokenUsagePresence,
    // None is an explicit retained regression refusal, NOT unknown coverage.
    terminal: Option<UsageSnapshot>,
}

impl ProviderFinalUsage {
    pub(crate) fn matches_subject(&self, observation: &ProviderWireRequestObservation) -> bool {
        self.observation.same_wire_subject(observation)
    }

    pub(crate) fn terminal_snapshot(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<UsageSnapshot> {
        if !self.matches_subject(observation)
            || observation.usage != Some(self.usage)
            || observation.usage_presence != Some(self.presence)
        {
            return Err(MezError::conflict("final usage evidence changed"));
        }
        self.terminal
            .ok_or_else(|| MezError::conflict("final stream usage regressed"))
    }
}

impl ProviderWireObservationContext<'_> {
    /// Called after successful HTTP completion and recognized unary parsing,
    /// before fallible output/MAAP parsing. No I/O or exposure release occurs.
    pub(super) fn finalize_unary_usage(
        &self,
        usage: mez_agent::ModelTokenUsage,
        presence: mez_agent::ModelTokenUsagePresence,
    ) -> Result<()> {
        self.finalize_usage(
            usage,
            presence,
            Some(UsageSnapshot {
                input: presence.input.then_some(usage.input_tokens),
                output: presence.output.then_some(usage.output_tokens),
                terminal: true,
            }),
        )
    }

    /// Terminal stream fields are distinct from the accumulated lower bound.
    /// Storage still requires each qualified final total to match that bound;
    /// contradictory totals refuse instead of erasing expense or releasing it.
    pub(super) fn finalize_stream_usage(
        &self,
        usage: mez_agent::ModelTokenUsage,
        presence: mez_agent::ModelTokenUsagePresence,
        terminal: (
            mez_agent::ModelTokenUsage,
            mez_agent::ModelTokenUsagePresence,
        ),
    ) -> Result<()> {
        self.finalize_usage(
            usage,
            presence,
            Some(UsageSnapshot {
                input: terminal.1.input.then_some(terminal.0.input_tokens),
                output: terminal.1.output.then_some(terminal.0.output_tokens),
                terminal: true,
            }),
        )
    }

    /// Transfer before callback suspension. Absence of a usable final report
    /// must never erase a positively witnessed numeric-integrity refusal.
    pub(super) fn refuse_stream_usage(
        &self,
        usage: mez_agent::ModelTokenUsage,
        presence: mez_agent::ModelTokenUsagePresence,
    ) -> Result<()> {
        self.finalize_usage(usage, presence, None)
    }

    fn finalize_usage(
        &self,
        usage: mez_agent::ModelTokenUsage,
        presence: mez_agent::ModelTokenUsagePresence,
        terminal: Option<UsageSnapshot>,
    ) -> Result<()> {
        let plan = self
            .planned_wire
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| MezError::invalid_state("final usage requires original plan"))?;
        let Some(factory) = plan.factory else {
            return Ok(());
        };
        let proof = ProviderFinalUsage {
            observation: plan.observation.clone(),
            usage,
            presence,
            terminal,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            factory.usage_finalized(&plan.observation, proof)
        }))
        .unwrap_or_else(|_| Err(MezError::invalid_state("final usage transfer panicked")));
        if result.is_err()
            && let Some(pending) = &self.observer.pending
        {
            pending.refuse_native_evidence(&plan.observation);
        }
        result
    }
}
