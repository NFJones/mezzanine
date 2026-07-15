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
    AgentRequestAssemblyResult, DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT, ModelProfile,
    ModelRequest, ModelRequestIdentity, assemble_model_request_from_context,
};

/// Assembles one provider request with the default retained-tail setting.
pub fn assemble_model_request(
    profile: &ModelProfile,
    turn: &AgentTurnRecord,
    context: &AgentContext,
) -> AgentRequestAssemblyResult<ModelRequest> {
    assemble_model_request_with_retained_tail_percent(
        profile,
        turn,
        context,
        DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT,
    )
}

/// Adapts root turn identity and embedded prompt assets into lower assembly.
///
/// The retained-tail setting remains accepted for runtime call compatibility;
/// explicit compaction consumes it before ordinary request assembly.
pub fn assemble_model_request_with_retained_tail_percent(
    profile: &ModelProfile,
    turn: &AgentTurnRecord,
    context: &AgentContext,
    _retained_tail_percent: usize,
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
