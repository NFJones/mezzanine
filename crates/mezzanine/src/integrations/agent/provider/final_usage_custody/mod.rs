//! Final unary counter evidence, distinct from transport retirement and prefixes.
//!
//! Only a native adapter's recognized, fully received successful unary report
//! creates this value. Primary presence qualifies each dimension independently;
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
            return Err(MezError::conflict("final unary usage evidence changed"));
        }
        Ok(UsageSnapshot {
            input: self.presence.input.then_some(self.usage.input_tokens),
            output: self.presence.output.then_some(self.usage.output_tokens),
            terminal: true,
        })
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
