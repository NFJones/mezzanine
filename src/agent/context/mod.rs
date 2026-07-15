//! Agent Context implementation.
//!
//! This module owns the agent context boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use mez_agent::{
    AgentContext, ContextBlock, ModelContextCompactionReport, model_context_block_header,
};

mod appenders;
mod assembly;
mod compaction;
mod evidence;
mod skills;

pub use appenders::{
    append_mcp_context, append_memory_context, append_permission_policy_context,
    append_project_guidance_context, append_scheduler_context, invoked_mcp_tools_for_context,
    set_project_guidance_context,
};
pub use assembly::{assemble_model_request, assemble_model_request_with_retained_tail_percent};
pub use compaction::{
    compact_model_context_for_budget, compact_model_context_for_budget_with_retained_tail_percent,
    model_context_text_word_count,
};
use mez_agent::{
    AllowedAction, AllowedActionSet, ContextSourceKind, ModelInteractionKind, ModelMessage,
    ModelMessageRole, ModelRequest,
};
pub use skills::constrain_skill_actions_for_loaded_context;

/// Maximum bytes from one context block copied into a provider request.
const MODEL_CONTEXT_BLOCK_LIMIT_BYTES: usize = 128 * 1024;
/// Marker used for deterministic local compaction summaries in provider context.
const MODEL_CONTEXT_COMPACTED_PREFIX: &str = "[context compacted]";
/// Default raw suffix percent retained after local context compaction.
pub const DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT: usize = 10;
