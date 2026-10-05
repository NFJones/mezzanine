//! Host shell, terminal, and asynchronous runtime adapters.
//!
//! Raw descriptors, PTYs, Unix sockets, subprocess discovery, Tokio workers,
//! and concrete effect execution are isolated from deterministic lower engines.

mod administration;
pub(crate) mod async_runtime;
#[allow(
    dead_code,
    reason = "the persistent local host integrates the completed host-Iroh owner in the next architecture phase"
)]
pub(crate) mod iroh;
#[allow(
    dead_code,
    reason = "outbound frontend IPC integration follows the shared endpoint lifetime checkpoint"
)]
pub(crate) mod outbound_endpoint;
pub(crate) mod ownership;
#[allow(
    dead_code,
    reason = "the sleep-inhibition backend is staged before its dependent agent-turn lifecycle integration"
)]
pub(crate) mod power_inhibition;
pub(crate) mod process;
pub(crate) mod router;
pub(crate) mod server;
pub(crate) mod session;
pub(crate) mod shell;
pub(crate) mod terminal;
