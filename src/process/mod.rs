//! Pane process and pseudoterminal management.
//!
//! Pane processes run behind pseudoterminals so interactive programs see normal
//! terminal semantics. This module owns the crate-backed PTY calls needed to
//! spawn the resolved shell, optionally replace it with an explicit command
//! through shell `exec`, set the pane environment, and propagate resize events.

/// Exposes the manager module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod manager;
/// Exposes the pane module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod pane;
/// Exposes the pty module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod pty;
/// Exposes the signals module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod signals;
/// Exposes the spawn module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod spawn;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use manager::PaneProcessManager;
pub use pane::PaneProcess;
pub(crate) use pane::{PTY_INPUT_WRITE_CHUNK_BYTES, write_pty_fd_nonblocking_io};
pub use spawn::{
    pane_command_plan, shell_command_from_argv, spawn_pane_process,
    spawn_pane_process_with_start_directory,
};
pub use types::{ExitedPaneProcess, PaneCommandPlan, PaneExitStatus, PaneProcessOutput};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
