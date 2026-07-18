//! Agent-independent terminal multiplexer domain and presentation.
//!
//! This crate owns pane, window, group, session, layout, PTY process, input,
//! copy/readline, theme, command-planning, and multi-surface presentation
//! behavior. It consumes terminal surfaces and shared identifiers but remains
//! independent of the agent harness and product composition crate.

pub mod attached_client;
pub mod clipboard;
pub mod command;
pub mod copy;
mod error;
pub mod host_input;
pub mod input;
pub mod layout;
pub mod overlay;
pub mod paste;
pub mod presentation;
pub mod process;
pub mod readline;
pub mod record_browser;
pub mod render;
pub mod selector;
pub mod session;
pub mod theme;

#[cfg(test)]
mod input_tests;
#[cfg(test)]
mod presentation_tests;

pub use error::{MuxError, MuxErrorKind, Result};
