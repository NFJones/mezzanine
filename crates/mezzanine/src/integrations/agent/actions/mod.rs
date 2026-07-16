//! Agent actions implementation.
//!
//! This module owns the agent actions boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

mod execution;
mod planning;
mod recovery;
mod runner;
mod transcript;

#[cfg(test)]
pub use execution::{discover_tools_through_pane_shell, execute_shell_action_through_pane};
pub use execution::{execute_mcp_action_through_runtime, execute_mcp_action_through_runtime_async};
pub use runner::AgentTurnRunner;
pub use transcript::next_transcript_sequence;
#[cfg(test)]
pub use transcript::persist_turn_execution_transcript;

// Shell/MCP executors, action execution, and transcript persistence.

/// Maximum previous-response bytes included in a terminal failure summary prompt.
const FAILURE_SUMMARY_RAW_TEXT_LIMIT_BYTES: usize = 8 * 1024;
