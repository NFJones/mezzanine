//! Pure provider-failure recovery selection shared by runtime adapters.
//!
//! Inputs are captured after exact claim/turn validation. This module neither
//! consumes budgets nor schedules work: existing negotiation and retry owners
//! remain authoritative, and adapters apply a decision exactly once.

use crate::error::MezError;
use crate::integrations::agent::actions::recovery::maap_provider_error_is_repairable;
use crate::runtime::RuntimeSessionService;
use mez_agent::ProviderErrorRetryClass;

/// Maximum accepted output-limit stages: continuation, then fresh recovery.
pub(crate) const PROVIDER_OUTPUT_RECOVERY_LIMIT: u32 = 2;

/// Captured eligibility supplied by the existing budget and owner authorities.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderFailureRecoveryEligibility {
    /// False for stale claims or turns that no longer own the request.
    pub(crate) current_owner: bool,
    /// Remaining malformed-output negotiation capacity.
    pub(crate) repair_available: bool,
    /// Remaining context-limit capacity in the retry owner.
    pub(crate) context_available: bool,
    /// One-based next output-limit stage.
    pub(crate) output_attempt: u32,
    /// Remaining transport capacity in the retry owner.
    pub(crate) transport_available: bool,
}

/// Selected recovery effect; no variant implies that execution succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderFailureRecoveryDecision {
    /// Ignore stale work without accounting, settlement, or retry effects.
    Ignore,
    /// Request bounded malformed-MAAP repair before generic retry handling.
    RepairMaap,
    /// Compact accepted context before rebuilding the rejected request.
    ContextLimit,
    /// Continue safe partial output or start the final fresh recovery stage.
    OutputLimit { attempt: u32 },
    /// Retry transport through the existing retry scheduler.
    RetryTransport,
    /// Non-retryable, insufficient evidence, or exhausted recovery capacity.
    Terminal,
}

/// Selects one recovery from trusted classification and captured eligibility.
/// Repair precedes limit/transport recovery; output recovery requires typed
/// partial-state evidence and never falls through into generic transport retry.
pub(crate) fn decide_provider_failure_recovery(
    error: &MezError,
    retry_class: ProviderErrorRetryClass,
    eligibility: ProviderFailureRecoveryEligibility,
) -> ProviderFailureRecoveryDecision {
    use ProviderFailureRecoveryDecision as Decision;
    if !eligibility.current_owner {
        return Decision::Ignore;
    }
    if eligibility.repair_available && maap_provider_error_is_repairable(error) {
        return Decision::RepairMaap;
    }
    match retry_class {
        ProviderErrorRetryClass::ContextLimit if eligibility.context_available => {
            Decision::ContextLimit
        }
        ProviderErrorRetryClass::OutputLimit
            if (1..=PROVIDER_OUTPUT_RECOVERY_LIMIT).contains(&eligibility.output_attempt)
                && error.provider_output_limit_state().is_some() =>
        {
            Decision::OutputLimit {
                attempt: eligibility.output_attempt,
            }
        }
        ProviderErrorRetryClass::RetryableTransport if eligibility.transport_available => {
            Decision::RetryTransport
        }
        _ => Decision::Terminal,
    }
}

impl RuntimeSessionService {
    /// Captures recovery eligibility from existing turn and retry-budget owners.
    /// The caller must separately validate its exact provider claim generation.
    pub(crate) fn provider_failure_recovery_eligibility(
        &self,
        agent_id: &str,
        turn_id: &str,
    ) -> ProviderFailureRecoveryEligibility {
        let current_owner = self.agent_turn_ledger().turn(turn_id).is_some_and(|turn| {
            turn.agent_id == agent_id
                && turn.state == mez_agent::AgentTurnState::Running
                && self
                    .agent_shell_store()
                    .get(&turn.pane_id)
                    .is_some_and(|session| session.session_id == turn.conversation_id)
        });
        let scheduler = &self.agent.provider_retry_scheduler;
        let attempt = scheduler.attempt(turn_id);
        let policy = scheduler.policy();
        ProviderFailureRecoveryEligibility {
            current_owner,
            repair_available: self.next_agent_maap_repair_attempt(turn_id)
                <= u32::try_from(mez_agent::DEFAULT_MAAP_REPAIR_ATTEMPT_LIMIT).unwrap_or(u32::MAX),
            context_available: policy.should_retry(attempt, ProviderErrorRetryClass::ContextLimit),
            output_attempt: self.next_agent_output_limit_recovery_attempt(turn_id),
            transport_available: policy
                .should_retry(attempt, ProviderErrorRetryClass::RetryableTransport),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::agent::provider::{
        provider_error_retry_class, provider_event_error_from_parts,
    };

    /// Direct errors and serialized worker envelopes must select the same
    /// recovery under identical captured budgets, without consuming attempts.
    #[test]
    fn provider_failure_recovery_envelope_parity_and_exhaustion() {
        use ProviderFailureRecoveryDecision as Decision;
        let available = ProviderFailureRecoveryEligibility {
            current_owner: true,
            repair_available: true,
            context_available: true,
            output_attempt: 1,
            transport_available: true,
        };
        for (message, payload, expected) in [
            (
                "context length exceeded",
                Some(r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#),
                Decision::ContextLimit,
            ),
            (
                "provider HTTP request failed",
                Some(r#"{"status_code":429}"#),
                Decision::RetryTransport,
            ),
            (
                "provider MAAP output is malformed: invalid JSON",
                None,
                Decision::RepairMaap,
            ),
            (
                "authentication failed",
                Some(r#"{"status_code":401}"#),
                Decision::Terminal,
            ),
            (
                "incomplete response: max_output_tokens",
                Some(r#"{"incomplete_details":{"reason":"max_output_tokens"}}"#),
                Decision::Terminal,
            ),
        ] {
            let mut direct = MezError::invalid_state(message);
            if let Some(payload) = payload {
                direct = direct.with_provider_failure_json(payload);
            }
            let envelope = provider_event_error_from_parts(
                "invalid_state",
                direct.message(),
                direct.provider_failure_json(),
                None,
            );
            assert_eq!(
                decide_provider_failure_recovery(
                    &direct,
                    provider_error_retry_class(&direct),
                    available
                ),
                expected,
                "{message}"
            );
            assert_eq!(
                decide_provider_failure_recovery(
                    &envelope,
                    provider_error_retry_class(&envelope),
                    available
                ),
                expected,
                "{message}"
            );
            let exhausted = ProviderFailureRecoveryEligibility {
                repair_available: false,
                context_available: false,
                output_attempt: 3,
                transport_available: false,
                ..available
            };
            assert_eq!(
                decide_provider_failure_recovery(
                    &direct,
                    provider_error_retry_class(&direct),
                    exhausted
                ),
                Decision::Terminal
            );
            assert_eq!(
                decide_provider_failure_recovery(
                    &direct,
                    provider_error_retry_class(&direct),
                    ProviderFailureRecoveryEligibility {
                        current_owner: false,
                        ..available
                    }
                ),
                Decision::Ignore
            );
        }
    }

    /// Output recovery requires typed safe partial state and permits only its
    /// two owned stages even when generic transport retry capacity remains.
    #[test]
    fn provider_failure_recovery_output_state_and_stage_bounds() {
        let error = MezError::invalid_state("incomplete response: max_output_tokens")
            .with_provider_output_limit_state(mez_agent::ProviderOutputLimitState::new(
                "openai",
                "test",
                "max_output_tokens",
                None,
                "safe partial",
                0,
                1,
                mez_agent::ModelTokenUsage::default(),
                mez_agent::ProviderOutputLimitContinuationDisposition::ContinueVisibleText,
            ));
        for attempt in 0..=3 {
            let decision = decide_provider_failure_recovery(
                &error,
                ProviderErrorRetryClass::OutputLimit,
                ProviderFailureRecoveryEligibility {
                    current_owner: true,
                    repair_available: false,
                    context_available: true,
                    output_attempt: attempt,
                    transport_available: true,
                },
            );
            assert_eq!(
                decision,
                if (1..=2).contains(&attempt) {
                    ProviderFailureRecoveryDecision::OutputLimit { attempt }
                } else {
                    ProviderFailureRecoveryDecision::Terminal
                }
            );
        }
    }
}
