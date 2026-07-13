//! Runtime environment and pane environment value types.
//!
//! This module owns the runtime process-environment contract that is shared by
//! socket discovery and in-pane command wiring. Keeping these value types out of
//! the central runtime service state makes the socket and pane-environment
//! boundary explicit while preserving the existing behavior of the helpers that
//! consume them.

use std::ffi::OsString;
use std::path::PathBuf;

pub use mez_mux::process::PaneProcessEnvironment as PaneEnvironment;

use super::sockets::current_effective_uid;

/// Separates fields inside the `MEZ` pane-environment value.
pub const MEZ_ENV_FIELD_SEPARATOR: char = '\x1f';
/// Default control socket filename used when no explicit session socket is set.
pub const DEFAULT_SOCKET_NAME: &str = "default.sock";

/// Identifies an auxiliary runtime socket derived from a control socket path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxiliarySocketKind {
    /// Message-service socket used for local agent coordination.
    Message,
    /// Event-service socket used for runtime event subscribers.
    Event,
}

/// Records which environment source selected the runtime socket directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketDirectorySource {
    /// The directory came from `MEZ_TMPDIR`.
    MezTmpdir,
    /// The directory came from `XDG_RUNTIME_DIR`.
    XdgRuntimeDir,
    /// The directory fell back to `/tmp`.
    Tmp,
}

/// Captures the process environment inputs used to resolve runtime paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeEnv {
    /// Optional `MEZ_TMPDIR` override for runtime socket placement.
    pub mez_tmpdir: Option<OsString>,
    /// Optional `XDG_RUNTIME_DIR` fallback for runtime socket placement.
    pub xdg_runtime_dir: Option<OsString>,
    /// Effective user id used for private runtime directory naming.
    pub uid: u32,
}

impl RuntimeEnv {
    /// Builds a runtime environment snapshot from the current process.
    pub fn from_process() -> Self {
        Self {
            mez_tmpdir: std::env::var_os("MEZ_TMPDIR"),
            xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR"),
            uid: current_effective_uid(),
        }
    }
}

/// Runtime socket directory selected for the active session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketDirectory {
    /// Absolute directory path where runtime sockets are stored.
    pub path: PathBuf,
    /// Environment source that selected the directory.
    pub source: SocketDirectorySource,
}
