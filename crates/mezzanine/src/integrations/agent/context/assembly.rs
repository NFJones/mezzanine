//! Product adapter for provider-independent model-request assembly.
//!
//! This module projects the root turn record into the lower request-identity
//! contract and supplies product-owned embedded prompt assets. Context/message
//! shaping, cache identity, provider defaults, and action-surface policy remain
//! in `mez-agent`.

use super::super::AgentTurnRecord;
use super::super::prompt::EmbeddedPromptAssets;
use super::AgentContext;
use mez_agent::{
    AgentRequestAssemblyResult, ModelProfile, ModelRequest, ModelRequestIdentity,
    ProviderApiCompatibility, assemble_model_request_from_context_api_unknown,
    assemble_model_request_from_context_with_api,
};

/// Assembles one provider request for a proven provider API compatibility.
pub fn assemble_model_request(
    profile: &ModelProfile,
    api: ProviderApiCompatibility,
    turn: &AgentTurnRecord,
    context: &AgentContext,
) -> AgentRequestAssemblyResult<ModelRequest> {
    #[cfg(test)]
    REQUEST_CONTEXT_SIZES.with_borrow_mut(|sizes| {
        if let Some(sizes) = sizes {
            sizes.push(context.blocks().len());
        }
    });
    assemble_model_request_from_context_with_api(
        profile,
        api,
        ModelRequestIdentity {
            turn_id: &turn.turn_id,
            agent_id: &turn.agent_id,
            pane_id: &turn.pane_id,
        },
        context,
        &EmbeddedPromptAssets,
    )
}

#[cfg(test)]
thread_local! {
    /// Records only this thread's production assembly calls inside one test scope.
    static REQUEST_CONTEXT_SIZES: std::cell::RefCell<Option<Vec<usize>>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Measures whether real admission projected source blocks, without timing or
/// allocator assumptions. The scoped guard also clears recording after panic.
#[cfg(test)]
pub(crate) fn with_request_context_sizes_for_tests<T>(work: impl FnOnce() -> T) -> (T, Vec<usize>) {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            REQUEST_CONTEXT_SIZES.with_borrow_mut(|sizes| *sizes = None);
        }
    }
    REQUEST_CONTEXT_SIZES.with_borrow_mut(|sizes| {
        assert!(sizes.is_none(), "request projection probe cannot nest");
        *sizes = Some(Vec::new());
    });
    let _clear = Clear;
    let result = work();
    let sizes = REQUEST_CONTEXT_SIZES.with_borrow_mut(|sizes| sizes.take().unwrap());
    (result, sizes)
}

/// Assembles a request without claiming an unproven provider API.
///
/// Provider-native continuity remains excluded, leaving only the neutral
/// projection for failure reporting and synthetic provider fixtures.
pub fn assemble_model_request_fail_closed(
    profile: &ModelProfile,
    turn: &AgentTurnRecord,
    context: &AgentContext,
) -> AgentRequestAssemblyResult<ModelRequest> {
    assemble_model_request_from_context_api_unknown(
        profile,
        ModelRequestIdentity {
            turn_id: &turn.turn_id,
            agent_id: &turn.agent_id,
            pane_id: &turn.pane_id,
        },
        context,
        &EmbeddedPromptAssets,
    )
}
