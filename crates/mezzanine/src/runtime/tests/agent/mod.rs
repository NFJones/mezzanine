//! Runtime agent test modules.

use super::*;
use crate::integrations::agent::slash::AgentShellCommandOutcome;
use crate::runtime::commands_support;

mod accounting;
mod commands;
mod compaction;
mod compaction_candidate;
mod compaction_candidate_ranges;
mod compaction_identity;
mod context;
mod context_guidance;
mod conversations;
mod macros;
mod mcp_schema;
mod model_selection;
mod native_directory;
mod native_instructions;
mod native_navigation;
mod overlay_refresh;
mod presentation;
mod prompt;
mod provider_failure_audit;
mod provider_recovery;
mod scheduling;
mod shell;
mod skills;
mod subagent_pane_close;
