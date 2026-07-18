//! Product adapter for provider-independent automatic model sizing.
//!
//! `mez_agent::auto_sizing` owns request construction, context bounding,
//! response validation, profile selection, fallback policy, usage projection,
//! and routed-worker conversion. This module invokes concrete product
//! providers, passes observed usage into the lower contract, and preserves
//! product provider diagnostics.

use super::{
    AgentContext, AgentTurnRecord, AsyncModelProvider, MezError, ModelTokenUsage, Result,
    RuntimeAutoSizingDispatch,
};
use mez_agent::AutoSizingExecution;

/// Executes an internal auto-sizing request with an async product provider.
pub(crate) async fn runtime_execute_auto_sizing_with_async_provider<P: AsyncModelProvider>(
    provider: &P,
    auto_sizing: &RuntimeAutoSizingDispatch,
    turn: &AgentTurnRecord,
    context: &AgentContext,
) -> Result<AutoSizingExecution> {
    let request = match mez_agent::auto_sizing_request(auto_sizing, turn, context) {
        Ok(request) => request,
        Err(error) => {
            return Ok(AutoSizingExecution::from_selection(
                auto_sizing,
                mez_agent::auto_sizing_fallback_selection(auto_sizing, error.message()),
                ModelTokenUsage::default(),
            ));
        }
    };
    match provider.send_request_async(&request).await {
        Ok(response) => Ok(AutoSizingExecution::from_selection(
            auto_sizing,
            mez_agent::auto_sizing_selection_from_response(auto_sizing, &response),
            response.usage,
        )),
        Err(error) => Err(runtime_auto_sizing_provider_error(auto_sizing, error)),
    }
}

/// Executes an internal auto-sizing request with a synchronous test provider.
#[cfg(test)]
pub(crate) fn runtime_execute_auto_sizing_with_provider<
    P: crate::integrations::agent::provider::ModelProvider,
>(
    provider: &P,
    auto_sizing: &RuntimeAutoSizingDispatch,
    turn: &AgentTurnRecord,
    context: &AgentContext,
) -> Result<AutoSizingExecution> {
    let request = match mez_agent::auto_sizing_request(auto_sizing, turn, context) {
        Ok(request) => request,
        Err(error) => {
            return Ok(AutoSizingExecution::from_selection(
                auto_sizing,
                mez_agent::auto_sizing_fallback_selection(auto_sizing, error.message()),
                ModelTokenUsage::default(),
            ));
        }
    };
    match provider.send_request(&request) {
        Ok(response) => Ok(AutoSizingExecution::from_selection(
            auto_sizing,
            mez_agent::auto_sizing_selection_from_response(auto_sizing, &response),
            response.usage,
        )),
        Err(error) => Err(runtime_auto_sizing_provider_error(auto_sizing, error)),
    }
}

/// Adds auto-sizing route identity while preserving provider failure metadata.
fn runtime_auto_sizing_provider_error(
    auto_sizing: &RuntimeAutoSizingDispatch,
    error: MezError,
) -> MezError {
    let message = format!(
        "auto-sizing router request failed for profile `{}` provider `{}` model `{}`: {}",
        auto_sizing.router_profile_name,
        auto_sizing.router_profile.provider,
        auto_sizing.router_profile.model,
        error.message()
    );
    let mut routed_error = MezError::new(error.kind(), message);
    if let Some(raw_text) = error.provider_raw_text() {
        routed_error = routed_error.with_provider_raw_text(raw_text.to_string());
    }
    if let Some(failure_json) = error.provider_failure_json() {
        routed_error = routed_error.with_provider_failure_json(failure_json.to_string());
    }
    routed_error
}
