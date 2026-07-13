//! Product compatibility facade for mux-owned pane process management.
//!
//! Pane process and PTY ownership lives in `mez-mux`. Mezzanine retains this
//! facade while runtime and persistence consumers migrate to the lower-crate
//! API directly.

pub use mez_mux::process::{
    ExitedPaneProcess, PTY_INPUT_WRITE_CHUNK_BYTES, PaneCommandPlan, PaneExitStatus, PaneProcess,
    PaneProcessEnvironment, PaneProcessLaunch, PaneProcessManager, PaneProcessOutput,
    pane_command_plan, shell_command_from_argv, spawn_pane_process,
    spawn_pane_process_with_start_directory, write_pty_fd_nonblocking_io,
};
