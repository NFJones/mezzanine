//! Provider-neutral progression for terminal failure summaries.
//!
//! Product adapters may ask a model to characterize a terminal provider or
//! controller failure. Those requests still need bounded transport retries,
//! malformed-output repair, provider-identity enforcement, and response
//! validation. This module owns that state without depending on concrete
//! provider I/O, product errors, MAAP request construction, or execution
//! projection.

use crate::{AgentTurnRecoveryBudget, ProviderErrorRetryClass};

/// Decision produced after a failure-summary provider call fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFailureSummaryProviderDecision {
    /// Retry the unchanged summary request after a runtime-retryable failure.
    RetryProvider {
        /// One-based transport retry accepted for this summary request.
        attempt: usize,
    },
    /// Replace the summary request with a malformed-output repair request.
    RecoverMalformedOutput {
        /// One-based malformed-output repair accepted for this summary request.
        attempt: usize,
    },
    /// Stop best-effort summary negotiation and preserve the original failure.
    Reject,
}

/// Decision produced after a failure-summary provider response is validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFailureSummaryResponseDecision {
    /// Accept the validated response as the terminal failure summary.
    Accept,
    /// Replace the request with a repair request for the malformed response.
    RecoverMalformedResponse {
        /// One-based malformed-response repair accepted for this summary request.
        attempt: usize,
    },
    /// Reject a response attributed to a different provider.
    RejectProviderIdentity,
    /// Reject a malformed response after its repair budget is exhausted.
    RejectMalformedResponse,
}

/// State for one best-effort terminal failure-summary negotiation.
///
/// Transport retry and malformed-output repair budgets are intentionally
/// independent. Retrying the same provider request must not consume the repair
/// capacity used to replace a malformed request or response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFailureSummaryNegotiation<Request> {
    request: Request,
    provider_retry_budget: AgentTurnRecoveryBudget,
    repair_budget: AgentTurnRecoveryBudget,
}

impl<Request> AgentFailureSummaryNegotiation<Request> {
    /// Starts summary negotiation from a product-constructed request.
    pub const fn new(request: Request, provider_retry_limit: usize, repair_limit: usize) -> Self {
        Self {
            request,
            provider_retry_budget: AgentTurnRecoveryBudget::new(provider_retry_limit),
            repair_budget: AgentTurnRecoveryBudget::new(repair_limit),
        }
    }

    /// Returns the current request for the next provider call.
    pub const fn request(&self) -> &Request {
        &self.request
    }

    /// Replaces the current request with a product-constructed repair request.
    pub fn replace_request(&mut self, request: Request) {
        self.request = request;
    }

    /// Classifies a failed provider call and advances the matching bounded budget.
    ///
    /// Runtime-retryable failures first retry the unchanged request. Other
    /// malformed provider output may consume the independent repair budget.
    pub const fn advance_provider_failure(
        &mut self,
        retry_class: ProviderErrorRetryClass,
        repairable_malformed_output: bool,
    ) -> AgentFailureSummaryProviderDecision {
        if matches!(
            retry_class,
            ProviderErrorRetryClass::ContextLimit
                | ProviderErrorRetryClass::OutputLimit
                | ProviderErrorRetryClass::RetryableTransport
        ) && self.provider_retry_budget.record_attempt()
        {
            return AgentFailureSummaryProviderDecision::RetryProvider {
                attempt: self.provider_retry_budget.attempts(),
            };
        }
        if repairable_malformed_output && self.repair_budget.record_attempt() {
            return AgentFailureSummaryProviderDecision::RecoverMalformedOutput {
                attempt: self.repair_budget.attempts(),
            };
        }
        AgentFailureSummaryProviderDecision::Reject
    }

    /// Classifies a completed provider response and advances malformed-response repair.
    ///
    /// Provider identity is checked before response validity. A response from a
    /// different provider is terminal for this best-effort negotiation and does
    /// not consume repair capacity.
    pub fn advance_provider_response(
        &mut self,
        expected_provider: &str,
        actual_provider: &str,
        response_is_valid: bool,
    ) -> AgentFailureSummaryResponseDecision {
        if expected_provider != actual_provider {
            return AgentFailureSummaryResponseDecision::RejectProviderIdentity;
        }
        if response_is_valid {
            return AgentFailureSummaryResponseDecision::Accept;
        }
        if self.repair_budget.record_attempt() {
            return AgentFailureSummaryResponseDecision::RecoverMalformedResponse {
                attempt: self.repair_budget.attempts(),
            };
        }
        AgentFailureSummaryResponseDecision::RejectMalformedResponse
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies transport retries and malformed-output repairs use independent
    /// budgets so one transient call failure does not remove repair capacity.
    #[test]
    fn failure_summary_negotiation_separates_retry_and_repair_budgets() {
        let mut negotiation = AgentFailureSummaryNegotiation::new("summary", 1, 1);

        assert_eq!(
            negotiation
                .advance_provider_failure(ProviderErrorRetryClass::RetryableTransport, false,),
            AgentFailureSummaryProviderDecision::RetryProvider { attempt: 1 }
        );
        assert_eq!(
            negotiation.advance_provider_failure(ProviderErrorRetryClass::NonRetryable, true),
            AgentFailureSummaryProviderDecision::RecoverMalformedOutput { attempt: 1 }
        );
        assert_eq!(
            negotiation.advance_provider_failure(ProviderErrorRetryClass::NonRetryable, true),
            AgentFailureSummaryProviderDecision::Reject
        );
    }

    /// Verifies valid responses are accepted, provider mismatches are terminal,
    /// and malformed responses consume only the bounded response-repair budget.
    #[test]
    fn failure_summary_negotiation_advances_provider_responses() {
        let mut negotiation = AgentFailureSummaryNegotiation::new("summary", 1, 1);

        assert_eq!(
            negotiation.advance_provider_response("openai", "other", false),
            AgentFailureSummaryResponseDecision::RejectProviderIdentity
        );
        assert_eq!(
            negotiation.advance_provider_response("openai", "openai", false),
            AgentFailureSummaryResponseDecision::RecoverMalformedResponse { attempt: 1 }
        );
        assert_eq!(
            negotiation.advance_provider_response("openai", "openai", false),
            AgentFailureSummaryResponseDecision::RejectMalformedResponse
        );

        let mut valid = AgentFailureSummaryNegotiation::new("summary", 0, 0);
        assert_eq!(
            valid.advance_provider_response("openai", "openai", true),
            AgentFailureSummaryResponseDecision::Accept
        );
    }
}
