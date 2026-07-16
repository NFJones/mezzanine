//! Runtime agent test modules.

use super::*;
use crate::agent::slash::AgentShellCommandOutcome;
use crate::runtime::commands_support;

mod commands;
mod compaction;
mod context;
mod conversations;
mod macros;
mod model_selection;
mod presentation;
mod prompt;
mod scheduling;
mod shell;
mod skills;
