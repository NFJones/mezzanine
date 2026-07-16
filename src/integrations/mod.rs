//! Concrete agent, hook, macro, MCP, and skill integrations.
//!
//! This boundary owns product transports, subprocesses, credentials, embedded
//! assets, and filesystem discovery over lower provider-independent contracts.

pub(crate) mod agent;
pub(crate) mod hooks;
pub(crate) mod macros;
pub(crate) mod mcp;
pub(crate) mod skills;
