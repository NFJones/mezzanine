//! Provider-independent agent turn negotiation state.
//!
//! This module owns bounded recovery, durable request promotion, provider
//! response accounting, and response/failure progression shared by the
//! canonical production orchestrator and focused policy tests.

/// Bounded recovery state shared by portable and product-adapted turn loops.
///
/// The budget records only accepted recovery attempts. Callers may reset it
/// after a valid continuation response starts a new negotiation phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentTurnRecoveryBudget {
    limit: usize,
    attempts: usize,
}
impl AgentTurnRecoveryBudget {
    /// Creates an unused recovery budget with the supplied attempt limit.
    pub const fn new(limit: usize) -> Self {
        Self { limit, attempts: 0 }
    }

    /// Returns the number of accepted recovery attempts.
    pub const fn attempts(self) -> usize {
        self.attempts
    }

    /// Records one recovery attempt when capacity remains.
    pub const fn record_attempt(&mut self) -> bool {
        if self.attempts >= self.limit {
            return false;
        }
        self.attempts = self.attempts.saturating_add(1);
        true
    }

    /// Starts a fresh recovery phase with the original limit.
    pub const fn reset(&mut self) {
        self.attempts = 0;
    }
}

/// Provider-negotiation state shared by portable and product turn runners.
///
/// The state keeps the durable request separate from ephemeral repair and
/// capability-continuation requests while owning the bounded recovery budget.
/// Concrete request and response types remain adapter-defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTurnResponseDecision {
    /// The provider response is valid and may proceed to batch validation.
    Accept,
    /// The response omitted its required action batch and may be repaired.
    RecoverMissingActionBatch {
        /// One-based recovery attempt accepted for this response.
        attempt: usize,
    },
    /// The response cannot continue through the provider negotiation loop.
    Reject(crate::ProviderResponseAcceptance),
}

/// Provider-failure progression shared by portable and product turn runners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTurnProviderFailureDecision {
    /// Retry malformed provider output with an ephemeral repair request.
    RecoverMalformedOutput {
        /// One-based recovery attempt accepted for this failure.
        attempt: usize,
    },
    /// Return context, output, or transport failures to runtime recovery.
    ReturnToRuntime,
    /// Summarize a terminal provider failure through the product adapter.
    Summarize,
}

/// Provider-negotiation state shared by portable and product turn runners.
///
/// The state keeps the durable request separate from ephemeral repair and
/// capability-continuation requests while owning the bounded recovery budget.
/// Concrete request and response types remain adapter-defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTurnNegotiation<Request> {
    durable_request: Request,
    recovery_budget: AgentTurnRecoveryBudget,
    response_progress: crate::ProviderResponseProgress,
}

impl<Request> AgentTurnNegotiation<Request> {
    /// Starts provider negotiation from the request that may be persisted.
    pub fn new(durable_request: Request, recovery_limit: usize) -> Self {
        Self {
            durable_request,
            recovery_budget: AgentTurnRecoveryBudget::new(recovery_limit),
            response_progress: crate::ProviderResponseProgress::default(),
        }
    }

    /// Returns the request currently eligible for durable transcript context.
    pub const fn durable_request(&self) -> &Request {
        &self.durable_request
    }

    /// Replaces the durable request after an accepted non-repair response.
    pub fn promote_durable_request(&mut self, request: Request) {
        self.durable_request = request;
    }

    /// Records one recoverable negotiation attempt when capacity remains.
    pub const fn record_recovery_attempt(&mut self) -> bool {
        self.recovery_budget.record_attempt()
    }

    /// Returns the number of accepted recovery attempts in this phase.
    pub const fn recovery_attempts(&self) -> usize {
        self.recovery_budget.attempts()
    }

    /// Classifies one provider failure and advances bounded repair state.
    ///
    /// Malformed output may consume the shared repair budget. Context,
    /// output, and retryable transport failures remain visible to runtime
    /// recovery, while all other failures proceed to product summarization.
    pub const fn advance_provider_failure(
        &mut self,
        repairable_malformed_output: bool,
        retry_class: crate::ProviderErrorRetryClass,
    ) -> AgentTurnProviderFailureDecision {
        if repairable_malformed_output && self.record_recovery_attempt() {
            return AgentTurnProviderFailureDecision::RecoverMalformedOutput {
                attempt: self.recovery_attempts(),
            };
        }
        match retry_class {
            crate::ProviderErrorRetryClass::ContextLimit
            | crate::ProviderErrorRetryClass::OutputLimit
            | crate::ProviderErrorRetryClass::RetryableTransport => {
                AgentTurnProviderFailureDecision::ReturnToRuntime
            }
            crate::ProviderErrorRetryClass::NonRetryable => {
                AgentTurnProviderFailureDecision::Summarize
            }
        }
    }

    /// Starts a fresh recovery phase after a valid continuation decision.
    pub const fn reset_recovery(&mut self) {
        self.recovery_budget.reset();
    }

    /// Borrows the recovery budget for canonical MAAP continuation planning.
    pub const fn recovery_budget_mut(&mut self) -> &mut AgentTurnRecoveryBudget {
        &mut self.recovery_budget
    }

    /// Records accounting from one completed provider response.
    pub fn observe_response(
        &mut self,
        usage: crate::ModelTokenUsage,
        latest_request_usage: Option<crate::ModelTokenUsage>,
        quota_usage: &[crate::ProviderQuotaUsage],
    ) {
        self.response_progress
            .observe(usage, latest_request_usage, quota_usage);
    }

    /// Records and classifies one completed provider response.
    ///
    /// Accepted non-repair requests become durable. Missing action batches
    /// consume the shared recovery budget, while provider identity failures
    /// remain terminal decisions for the product adapter to summarize.
    #[allow(clippy::too_many_arguments)]
    pub fn advance_provider_response(
        &mut self,
        response_request: &Request,
        expected_provider: &str,
        actual_provider: &str,
        repair_response: bool,
        has_action_batch: bool,
        usage: crate::ModelTokenUsage,
        latest_request_usage: Option<crate::ModelTokenUsage>,
        quota_usage: &[crate::ProviderQuotaUsage],
    ) -> AgentTurnResponseDecision
    where
        Request: Clone,
    {
        self.observe_response(usage, latest_request_usage, quota_usage);
        let acceptance = crate::accept_provider_response(
            expected_provider,
            actual_provider,
            repair_response,
            has_action_batch,
        );
        match acceptance {
            crate::ProviderResponseAcceptance::Accept {
                promote_durable_request,
            } => {
                if promote_durable_request {
                    self.promote_durable_request(response_request.clone());
                }
                AgentTurnResponseDecision::Accept
            }
            crate::ProviderResponseAcceptance::MissingActionBatch
                if self.record_recovery_attempt() =>
            {
                AgentTurnResponseDecision::RecoverMissingActionBatch {
                    attempt: self.recovery_attempts(),
                }
            }
            rejection => AgentTurnResponseDecision::Reject(rejection),
        }
    }

    /// Returns cumulative usage across all completed provider responses.
    pub fn cumulative_response_usage(&self) -> crate::ModelTokenUsage {
        self.response_progress.cumulative_usage()
    }

    /// Returns usage from the latest completed provider response.
    pub fn latest_response_usage(&self) -> crate::ModelTokenUsage {
        self.response_progress.latest_response_usage()
    }

    /// Returns the latest non-empty provider quota observation.
    pub fn latest_quota_usage(&self) -> &[crate::ProviderQuotaUsage] {
        self.response_progress.latest_quota_usage()
    }

    /// Consumes the state and returns the durable request.
    pub fn into_durable_request(self) -> Request {
        self.durable_request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies one negotiation owner keeps durable request promotion,
    /// bounded recovery, and cumulative provider accounting synchronized.
    #[test]
    fn turn_negotiation_owns_recovery_and_response_progress() {
        let mut negotiation = AgentTurnNegotiation::new("initial", 1);
        let quota = crate::ProviderQuotaUsage {
            name: "requests".to_string(),
            used_basis_points: 5_000,
            limit: 20,
            remaining: 10,
            reset: None,
        };

        assert!(negotiation.record_recovery_attempt());
        assert!(!negotiation.record_recovery_attempt());
        negotiation.observe_response(
            crate::ModelTokenUsage {
                input_tokens: 3,
                output_tokens: 5,
                ..crate::ModelTokenUsage::default()
            },
            None,
            std::slice::from_ref(&quota),
        );
        negotiation.observe_response(
            crate::ModelTokenUsage {
                input_tokens: 7,
                output_tokens: 11,
                ..crate::ModelTokenUsage::default()
            },
            Some(crate::ModelTokenUsage {
                input_tokens: 2,
                output_tokens: 3,
                ..crate::ModelTokenUsage::default()
            }),
            &[],
        );
        negotiation.promote_durable_request("accepted");

        assert_eq!(negotiation.durable_request(), &"accepted");
        assert_eq!(negotiation.recovery_attempts(), 1);
        assert_eq!(negotiation.cumulative_response_usage().input_tokens, 10);
        assert_eq!(negotiation.cumulative_response_usage().output_tokens, 16);
        assert_eq!(negotiation.latest_response_usage().input_tokens, 2);
        assert_eq!(negotiation.latest_response_usage().output_tokens, 3);
        assert_eq!(negotiation.latest_quota_usage(), &[quota]);
    }

    /// Verifies canonical response progression promotes durable requests and
    /// owns the bounded missing-action-batch recovery decision.
    #[test]
    fn turn_negotiation_advances_provider_responses() {
        let mut negotiation = AgentTurnNegotiation::new("initial", 1);
        assert_eq!(
            negotiation.advance_provider_response(
                &"accepted",
                "openai",
                "openai",
                false,
                true,
                crate::ModelTokenUsage::default(),
                None,
                &[],
            ),
            AgentTurnResponseDecision::Accept
        );
        assert_eq!(negotiation.durable_request(), &"accepted");

        assert_eq!(
            negotiation.advance_provider_response(
                &"accepted",
                "openai",
                "openai",
                false,
                false,
                crate::ModelTokenUsage::default(),
                None,
                &[],
            ),
            AgentTurnResponseDecision::RecoverMissingActionBatch { attempt: 1 }
        );
        assert_eq!(
            negotiation.advance_provider_response(
                &"accepted",
                "openai",
                "openai",
                false,
                false,
                crate::ModelTokenUsage::default(),
                None,
                &[],
            ),
            AgentTurnResponseDecision::Reject(
                crate::ProviderResponseAcceptance::MissingActionBatch
            )
        );
    }

    /// Verifies provider failures share bounded malformed-output recovery while
    /// preserving runtime retries and terminal summarization as distinct paths.
    #[test]
    fn turn_negotiation_advances_provider_failures() {
        let mut negotiation = AgentTurnNegotiation::new("initial", 1);

        assert_eq!(
            negotiation
                .advance_provider_failure(true, crate::ProviderErrorRetryClass::NonRetryable,),
            AgentTurnProviderFailureDecision::RecoverMalformedOutput { attempt: 1 }
        );
        assert_eq!(
            negotiation
                .advance_provider_failure(true, crate::ProviderErrorRetryClass::NonRetryable,),
            AgentTurnProviderFailureDecision::Summarize
        );
        assert_eq!(
            negotiation.advance_provider_failure(
                false,
                crate::ProviderErrorRetryClass::RetryableTransport,
            ),
            AgentTurnProviderFailureDecision::ReturnToRuntime
        );
        assert_eq!(
            negotiation
                .advance_provider_failure(false, crate::ProviderErrorRetryClass::ContextLimit,),
            AgentTurnProviderFailureDecision::ReturnToRuntime
        );
    }
}
