//! Core library for Mezzanine.
//!
//! The crate is organized around the logical subsystems defined by
//! `SPEC.md`. Implementation logic lives in testable subsystem modules
//! rather than the binary entry point.

/// Exposes the agent module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod agent;
/// Exposes the async runtime module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod async_runtime;
/// Exposes the audit module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod audit;
/// Exposes the auth module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod auth;
/// Exposes the cli module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod cli;
/// Exposes the command module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod command;
/// Exposes the config module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod config;
/// Exposes the control module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod control;
/// Exposes the error module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod error;
/// Exposes the event module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod event;
/// Exposes the framing module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod framing;
/// Exposes the hooks module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod hooks;
/// Exposes shared identifier validation helpers.
///
/// The nested module keeps cross-subsystem identifier predicates isolated while
/// callers retain subsystem-specific error messages.
pub(crate) mod identifiers;
/// Preserves the product crate's identifier facade during workspace extraction.
pub use mez_core::ids;
/// Exposes the instructions module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod instructions;
/// Exposes the issues module boundary.
///
/// The nested module keeps local issue tracking isolated while this declaration
/// makes the boundary available to CLI, runtime commands, and agent actions.
pub mod issues;
/// Exposes the layout module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod layout;
/// Exposes the macros module boundary.
///
/// The nested module keeps agent macro discovery and parsing isolated while
/// this declaration makes the boundary available to the crate.
pub mod macros;
/// Exposes the mcp module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod mcp;
/// Exposes the memory module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod memory;
/// Exposes the message module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod message;
/// Exposes the permissions module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod permissions;
/// Exposes the process module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
/// Exposes the project module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod project;
/// Exposes the readline module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod readline;
/// Exposes the registry module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod registry;
/// Exposes the runtime module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod runtime;
/// Exposes the scheduler module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod scheduler;
/// Exposes the selector module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod selector;
/// Exposes the session module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod session;
/// Exposes the shell module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod shell;
/// Exposes the skills module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod skills;
/// Exposes the snapshot module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod snapshot;
/// Exposes shared server-sent event parsing helpers.
///
/// The nested module keeps provider and integration streaming parsers aligned
/// without hiding their transport-specific completion policies.
pub(crate) mod sse;
/// Exposes the subagent module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod subagent;
/// Exposes the terminal module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod terminal;
/// Exposes shared test support for crate-internal regression suites.
///
/// The module is compiled only for tests and keeps high-reuse fixtures out of
/// large subsystem test files.
#[cfg(test)]
pub(crate) mod test_support;
/// Exposes the transcript module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
pub mod transcript;

pub use error::{MezError, MezErrorKind, Result};
