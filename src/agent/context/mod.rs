//! Agent Context implementation.
//!
//! This module owns the agent context boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use mez_agent::{AgentContext, ContextBlock, model_context_block_header};

mod appenders;
mod assembly;
mod evidence;
mod skills;

pub use appenders::{
    append_mcp_context, append_memory_context, append_permission_policy_context,
    append_project_guidance_context, append_scheduler_context, invoked_mcp_tools_for_context,
    set_project_guidance_context,
};
pub use assembly::{assemble_model_request, assemble_model_request_with_retained_tail_percent};
use mez_agent::{
    AllowedAction, AllowedActionSet, ContextSourceKind,
    DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT, ModelInteractionKind, ModelMessage,
    ModelMessageRole, ModelRequest,
};
pub use skills::constrain_skill_actions_for_loaded_context;
