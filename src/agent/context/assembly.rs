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
    assemble_model_request_from_context,
};

/// Assembles one provider request with the default retained-tail setting.
pub fn assemble_model_request(
    profile: &ModelProfile,
    turn: &AgentTurnRecord,
    context: &AgentContext,
) -> AgentRequestAssemblyResult<ModelRequest> {
    assemble_model_request_from_context(
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
