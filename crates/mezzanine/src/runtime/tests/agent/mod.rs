//! Runtime agent test modules.

use super::*;
use crate::integrations::agent::slash::AgentShellCommandOutcome;
use crate::runtime::commands_support;

mod accounting;
mod commands;
mod compaction;
mod compaction_candidate;
mod context;
mod conversations;
mod macros;
mod mcp_schema;
mod model_selection;
mod overlay_refresh;
mod presentation;
mod prompt;
mod provider_failure_audit;
mod provider_recovery;
mod scheduling;
mod shell;
mod skills;
mod subagent_pane_close;
