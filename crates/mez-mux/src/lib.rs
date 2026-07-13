//! Agent-independent terminal multiplexer domain and presentation.
//!
//! This crate will own pane, window, group, layout, process, input-routing, and
//! multi-surface presentation behavior. It may consume terminal surfaces and
//! shared contracts, but it must not depend on the agent harness or product
//! composition crate. The initial empty facade records that direction before
//! effect-driven mux boundaries are extracted from the root package.

mod error;
pub mod layout;
pub mod process;
pub mod session;

pub use error::{MuxError, MuxErrorKind, Result};
