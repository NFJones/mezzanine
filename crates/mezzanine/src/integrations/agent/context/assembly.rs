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
