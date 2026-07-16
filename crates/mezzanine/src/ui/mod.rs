//! Product command, readline, and selector adapters.
//!
//! Neutral command and input engines live in `mez-mux`; this boundary supplies
//! cross-product dispatch, prompt kinds, and runtime/filesystem candidates.

pub(crate) mod command;
pub(crate) mod readline;
pub(crate) mod selector;
