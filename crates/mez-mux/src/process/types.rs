//! Data types shared by pane process spawning and lifecycle management.
//!
//! These structures describe command plans, process output, and normalized exit
//! status without owning PTY or runtime resources.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Pacing policy for one runtime-generated shell delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellInputPacing {
    /// Generated shell source whose complete records use wrapper progress.
    GeneratedSource,
    /// Deferred payload records acknowledged by the shell receiver.
    ReceiverAcknowledged,
}

/// Typed shell input retained across runtime and PTY ownership boundaries.
///
/// Shell deliveries remain distinct from ordinary user input so adapters can
/// preserve complete-record pacing, strict priority, and transaction identity
/// without inspecting or logging payload bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInputDelivery {
    /// Bytes delivered to the pane shell.
    pub bytes: Vec<u8>,
    /// Whether this delivery precedes later input for the same pane.
    pub priority: bool,
    /// Complete-record pacing contract selected by the renderer.
    pub pacing: ShellInputPacing,
    /// Optional transaction or delivery identity used for scoped handling.
    pub delivery_id: Option<String>,
    /// Whether the rendered receiver negotiated per-record acknowledgements.
    pub receiver_acknowledgements: bool,
}

impl ShellInputDelivery {
    /// Builds non-priority generated wrapper source.
    pub fn generated_source(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            priority: false,
            pacing: ShellInputPacing::GeneratedSource,
            delivery_id: None,
            receiver_acknowledgements: false,
        }
    }

    /// Builds a priority deferred payload bound to one transaction marker.
    pub fn receiver_acknowledged(
        bytes: Vec<u8>,
        delivery_id: impl Into<String>,
        receiver_acknowledgements: bool,
    ) -> Self {
        Self {
            bytes,
            priority: true,
            pacing: ShellInputPacing::ReceiverAcknowledged,
            delivery_id: Some(delivery_id.into()),
            receiver_acknowledgements,
        }
    }
}

/// Dependency-neutral executable selected for a pane process launch.
///
/// Product adapters remain responsible for discovering and classifying the
/// user's shell; the process subsystem only consumes the selected path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneProcessLaunch {
    program: PathBuf,
    environment: Vec<(OsString, OsString)>,
    interactive_arguments: Vec<OsString>,
}

impl PaneProcessLaunch {
    /// Creates a launch contract for the selected shell executable.
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            environment: Vec::new(),
            interactive_arguments: vec![OsString::from("-i")],
        }
    }

    /// Returns the executable used to start the pane process.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Replaces the arguments used when starting an ordinary interactive pane.
    ///
    /// Explicit pane commands retain their `-c` invocation. Product adapters
    /// use this for shell-specific startup requirements without making the
    /// mux infer shell policy from an executable path.
    pub fn with_interactive_arguments(
        mut self,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        self.interactive_arguments = arguments.into_iter().map(Into::into).collect();
        self
    }

    /// Returns the arguments used to start an ordinary interactive pane.
    pub fn interactive_arguments(&self) -> &[OsString] {
        &self.interactive_arguments
    }

    /// Adds one explicit environment override for the pane process.
    ///
    /// Product adapters use this narrow launch boundary for process-specific
    /// compatibility state while the mux remains unaware of shell policy.
    pub fn with_environment_variable(
        mut self,
        key: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.environment.push((key.into(), value.into()));
        self
    }

    /// Returns explicit environment overrides applied during process spawn.
    pub fn environment(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.environment
            .iter()
            .map(|(key, value)| (key.as_os_str(), value.as_os_str()))
    }
}

/// Environment values injected into a newly spawned pane process.
///
/// Product adapters construct these values from runtime socket and session
/// state; the process subsystem only applies the explicit launch contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneProcessEnvironment {
    /// Structured `MEZ` value containing socket, session, window, pane, and term.
    pub mez: String,
    /// Session id exported separately for simple shell access.
    pub session: String,
    /// Window id exported separately for simple shell access.
    pub window: String,
    /// Pane id exported separately for simple shell access.
    pub pane: String,
    /// Terminal type exported for the pane process.
    pub term: String,
}

/// Carries Pane Exit Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneExitStatus {
    /// Stores the code value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub code: Option<i32>,
    /// Stores the signal value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub signal: Option<i32>,
    /// Stores the success value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub success: bool,
}

impl PaneExitStatus {
    /// Converts a platform exit status into Mezzanine's normalized status.
    pub fn from_exit_status(status: std::process::ExitStatus) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;

            Self {
                code: status.code(),
                signal: status.signal(),
                success: status.success(),
            }
        }

        #[cfg(not(unix))]
        {
            Self {
                code: status.code(),
                signal: None,
                success: status.success(),
            }
        }
    }

    /// Converts a portable-pty exit status into Mezzanine's normalized status.
    pub fn from_portable_exit_status(status: portable_pty::ExitStatus) -> Self {
        let code = i32::try_from(status.exit_code()).ok();
        let signal = status
            .signal()
            .and_then(super::signals::signal_number_from_portable_name);
        Self {
            code,
            signal,
            success: status.success(),
        }
    }

    /// Returns true when the process exited successfully.
    pub fn success(&self) -> bool {
        self.success
    }

    /// Serializes the normalized status as the object used by pane state and
    /// lifecycle event payloads.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"code":{},"signal":{},"success":{}}}"#,
            optional_i32_json(self.code),
            optional_i32_json(self.signal),
            self.success
        )
    }

    /// Returns a concise frame-template value for `pane.exit_status`.
    pub fn frame_value(&self) -> String {
        if let Some(code) = self.code {
            format!("exit={code}")
        } else if let Some(signal) = self.signal {
            format!("signal={signal}")
        } else if self.success {
            "success".to_string()
        } else {
            "unknown".to_string()
        }
    }
}

/// Runs the optional i32 json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_i32_json(value: Option<i32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

/// Carries Exited Pane Process state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitedPaneProcess {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: PaneExitStatus,
}

/// Carries Pane Process Output state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneProcessOutput {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the bytes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bytes: Vec<u8>,
}

/// Carries Pane Command Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCommandPlan {
    /// Stores the program value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub program: String,
    /// Stores the args value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub args: Vec<String>,
}
