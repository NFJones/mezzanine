//! Agent Context implementation.
//!
//! This module owns the agent context boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use mez_agent::AgentContext;

mod assembly;

pub use assembly::{assemble_model_request, assemble_model_request_with_retained_tail_percent};
