//! Runtime actions test modules.

use super::*;
use mez_agent::outcome::runtime_unrecovered_failure_output_lines;

mod config;
mod deferred_logs;
mod failure_recovery;
mod issues;
mod mcp;
mod mcp_cancellation;
mod memory;
mod messaging;
mod native_integrations;
mod native_lifecycle;
mod network;
mod patch;
mod shell;
mod shell_protocol;
