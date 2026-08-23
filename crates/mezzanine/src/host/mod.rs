//! Host shell, terminal, and asynchronous runtime adapters.
//!
//! Raw descriptors, PTYs, Unix sockets, subprocess discovery, Tokio workers,
//! and concrete effect execution are isolated from deterministic lower engines.

pub(crate) mod async_runtime;
#[allow(
    dead_code,
    reason = "the sleep-inhibition backend is staged before its dependent agent-turn lifecycle integration"
)]
pub(crate) mod power_inhibition;
pub(crate) mod process;
pub(crate) mod session;
pub(crate) mod shell;
pub(crate) mod terminal;
