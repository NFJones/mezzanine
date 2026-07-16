//! Async pane process I/O driver boundary.
//!
//! Tokio-native pane process I/O workers.
//!
//! The runtime actor owns pane state, while one Tokio task owns each live pane
//! process and PTY master. The backend reads and writes the PTY through
//! nonblocking file descriptors, converts output and side-effect completions
//! into typed runtime events, and keeps I/O failure handling local to the pane
//! instead of letting one pane block the whole runtime.

use super::{
    AsyncRuntimeService, AsyncRuntimeServiceExit, AsyncRuntimeSessionHandle, Duration, MezError,
    PaneEvent, ProcessEvent, Result, RuntimeEvent, RuntimeEventBatch, RuntimeEventIngressReport,
    RuntimeLifecycleState, RuntimeSideEffect, Size, is_terminal_runtime_lifecycle_state,
};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::pin::Pin;
use tokio::io::unix::AsyncFd;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, timeout};

/// Maximum time to wait for one PTY input chunk to become writable.
///
/// Agent shell transactions must never be able to wedge the async pane worker
/// while delivering generated input. A stalled PTY write is surfaced as a pane
/// write failure so runtime transaction timeouts and diagnostics can proceed.
const PANE_INPUT_WRITE_READY_TIMEOUT: Duration = Duration::from_secs(10);

mod driver;
mod driver_service;
#[cfg(test)]
mod fake;
mod helpers;
mod process_service;
mod pty;
mod service_types;
mod side_effects;
mod supervisor;

pub(super) use super::{PaneExitStatus, PaneProcess};
pub use driver::{
    AsyncPaneForegroundProcess, AsyncPaneIoFuture, AsyncPaneProcessDriver,
    AsyncPaneProcessDriverConfig, AsyncPaneProcessIo,
};
pub use driver_service::{
    AsyncPaneProcessDriverServiceConfig, AsyncPaneProcessDriverServiceReport,
    run_async_pane_process_driver_service,
};
#[cfg(test)]
pub use fake::AsyncFakePaneProcessIo;
pub use process_service::run_async_pane_process_service;
pub use pty::AsyncPtyPaneProcessIo;
pub use service_types::{
    AsyncPaneIoSideEffectServiceConfig, AsyncPaneIoSideEffectServiceReport,
    AsyncPaneProcessServiceConfig, AsyncPaneProcessServiceReport,
    AsyncPaneProcessSupervisorServiceConfig, AsyncPaneProcessSupervisorServiceReport,
};
pub use side_effects::{
    build_async_pane_io_side_effect_service, run_async_pane_io_side_effect_service,
};
pub use supervisor::{
    build_async_pane_process_service, build_async_pane_process_supervisor_service,
    run_async_pane_process_supervisor_service,
};

#[cfg(test)]
mod tests;
