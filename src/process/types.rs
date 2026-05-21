//! Data types shared by pane process spawning and lifecycle management.
//!
//! These structures describe command plans, process output, and normalized exit
//! status without owning PTY or runtime resources.

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
