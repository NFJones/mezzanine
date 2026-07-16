//! Product adapter for provider-independent automatic model sizing.
//!
//! `mez_agent::auto_sizing` owns request construction, context bounding,
//! response validation, profile selection, and fallback policy. This module
//! invokes concrete product providers, preserves provider diagnostics, and
//! attaches router token usage to runtime accounting.

#[cfg(test)]
use super::RuntimeAutoSizingDecision;
use super::{
    AgentContext, AgentTurnRecord, AsyncModelProvider, MezError, ModelProfile, ModelRequest,
    ModelTokenUsage, ModelTokenUsageKey, Result, RuntimeAutoSizingDispatch, RuntimeProviderConfig,
};

/// Result of one internal auto-sizing router provider invocation.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeAutoSizingExecution {
    /// Effective profile selected for the user-visible provider turn.
    pub(crate) selected_profile: ModelProfile,
    /// Parsed router decision when the router returned valid JSON.
    #[cfg(test)]
    pub(crate) decision: Option<RuntimeAutoSizingDecision>,
    /// Fallback reason when routing could not select a valid target.
    #[cfg(test)]
    pub(crate) fallback: Option<String>,
    /// Provider/model key for the router request.
    pub(crate) router_token_usage_key: ModelTokenUsageKey,
    /// Token usage reported by the router request.
    pub(crate) router_token_usage: ModelTokenUsage,
}

impl RuntimeAutoSizingExecution {
    /// Returns router usage as a per-model accounting map.
    pub(crate) fn token_usage_by_model(
        &self,
    ) -> std::collections::BTreeMap<ModelTokenUsageKey, ModelTokenUsage> {
        if self.router_token_usage.is_zero() {
            return std::collections::BTreeMap::new();
        }
        std::collections::BTreeMap::from([(
            self.router_token_usage_key.clone(),
            self.router_token_usage,
        )])
    }
}

/// Executes an internal auto-sizing request with an async product provider.
pub(crate) async fn runtime_execute_auto_sizing_with_async_provider<P: AsyncModelProvider>(
    provider: &P,
    auto_sizing: &RuntimeAutoSizingDispatch,
    turn: &AgentTurnRecord,
    context: &AgentContext,
) -> Result<RuntimeAutoSizingExecution> {
    let request = match mez_agent::auto_sizing_request(auto_sizing, turn, context) {
        Ok(request) => request,
        Err(error) => {
            return Ok(runtime_auto_sizing_execution_from_selection(
                auto_sizing,
                mez_agent::auto_sizing_fallback_selection(auto_sizing, error.message()),
                ModelTokenUsage::default(),
            ));
        }
    };
    match provider.send_request_async(&request).await {
        Ok(response) => Ok(runtime_auto_sizing_execution_from_selection(
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
    P: crate::agent::provider::ModelProvider,
>(
    provider: &P,
    auto_sizing: &RuntimeAutoSizingDispatch,
    turn: &AgentTurnRecord,
    context: &AgentContext,
) -> Result<RuntimeAutoSizingExecution> {
    let request = match mez_agent::auto_sizing_request(auto_sizing, turn, context) {
        Ok(request) => request,
        Err(error) => {
            return Ok(runtime_auto_sizing_execution_from_selection(
                auto_sizing,
                mez_agent::auto_sizing_fallback_selection(auto_sizing, error.message()),
                ModelTokenUsage::default(),
            ));
        }
    };
    match provider.send_request(&request) {
        Ok(response) => Ok(runtime_auto_sizing_execution_from_selection(
            auto_sizing,
            mez_agent::auto_sizing_selection_from_response(auto_sizing, &response),
            response.usage,
        )),
        Err(error) => Err(runtime_auto_sizing_provider_error(auto_sizing, error)),
    }
}

/// Applies the effective provider request identity to actor-owned profile state.
pub(crate) fn runtime_apply_auto_sizing_execution_profile(
    current: ModelProfile,
    request: &ModelRequest,
) -> ModelProfile {
    mez_agent::apply_auto_sizing_execution_profile(current, request)
}

/// Returns provider-compatible reasoning levels for one target profile.
pub(crate) fn runtime_auto_sizing_reasoning_levels_for_profile(
    provider_config: Option<&RuntimeProviderConfig>,
    profile: &ModelProfile,
) -> Vec<String> {
    mez_agent::auto_sizing_reasoning_levels_for_profile(provider_config, profile)
}

/// Projects a lower selection and provider usage into runtime accounting state.
fn runtime_auto_sizing_execution_from_selection(
    auto_sizing: &RuntimeAutoSizingDispatch,
    selection: mez_agent::AutoSizingSelection,
    router_token_usage: ModelTokenUsage,
) -> RuntimeAutoSizingExecution {
    RuntimeAutoSizingExecution {
        selected_profile: selection.selected_profile,
        #[cfg(test)]
        decision: selection.decision,
        #[cfg(test)]
        fallback: selection.fallback,
        router_token_usage_key: ModelTokenUsageKey::new(
            auto_sizing.router_profile.provider.clone(),
            auto_sizing.router_profile.model.clone(),
        ),
        router_token_usage,
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
