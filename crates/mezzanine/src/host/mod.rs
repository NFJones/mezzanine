//! Host shell, terminal, and asynchronous runtime adapters.
//!
//! Raw descriptors, PTYs, Unix sockets, subprocess discovery, Tokio workers,
//! and concrete effect execution are isolated from deterministic lower engines.

pub(crate) mod async_runtime;
pub(crate) mod process;
pub(crate) mod shell;
pub(crate) mod terminal;
