//! Product-owned host clipboard process adapters.
//!
//! Generic paste-buffer state lives in `mez_mux::paste`; this module retains
//! platform command discovery and host clipboard process execution.

use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

/// Copies text to the host clipboard using common platform clipboard tools.
///
/// The operation returns `false` instead of surfacing errors because clipboard
/// access is best-effort in headless, SSH, and restricted desktop sessions.
pub fn copy_to_host_clipboard(content: &str) -> bool {
    copy_to_host_clipboard_with_commands(content, &host_clipboard_copy_commands())
}

/// Reads text from the host clipboard using common platform clipboard tools.
///
/// Missing clipboard tools, unavailable display sessions, and command failures
/// are treated as absence so callers can fall back to internal paste buffers.
pub fn read_host_clipboard() -> Option<String> {
    read_host_clipboard_with_commands(&host_clipboard_paste_commands())
}

/// Runtime clipboard access strategy.
///
/// The default strategy talks to common host clipboard tools. Tests can replace
/// it with disabled or fixed implementations so copy/paste behavior remains
/// deterministic and does not mutate a developer's desktop clipboard.
#[derive(Clone)]
pub struct HostClipboard {
    /// Stores the copy backend for this clipboard strategy.
    copy: HostClipboardCopyBackend,
    /// Stores the read backend for this clipboard strategy.
    read: HostClipboardReadBackend,
}

/// Carries the configured host clipboard copy backend.
#[derive(Clone)]
enum HostClipboardCopyBackend {
    /// Uses the ordered command list until one command succeeds.
    Commands(Vec<HostClipboardCommand>),
    /// Uses a fixed function pointer.
    Function(fn(&str) -> bool),
}

/// Carries the configured host clipboard read backend.
#[derive(Clone)]
enum HostClipboardReadBackend {
    /// Uses the ordered command list until one command succeeds.
    Commands(Vec<HostClipboardCommand>),
    /// Uses a fixed function pointer.
    Function(fn() -> Option<String>),
}

impl HostClipboard {
    /// Returns the system clipboard strategy backed by host clipboard tools.
    pub fn system() -> Self {
        Self {
            copy: HostClipboardCopyBackend::Function(copy_to_host_clipboard),
            read: HostClipboardReadBackend::Function(read_host_clipboard),
        }
    }

    /// Returns a strategy that silently ignores copy and paste requests.
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            copy: HostClipboardCopyBackend::Function(disabled_host_clipboard_copy),
            read: HostClipboardReadBackend::Function(disabled_host_clipboard_read),
        }
    }

    /// Returns a strategy backed by caller-supplied command lists.
    ///
    /// # Parameters
    /// - `copy`: The ordered copy commands that receive clipboard content on stdin.
    /// - `read`: The ordered paste commands whose stdout is read as clipboard text.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn commands(copy: Vec<HostClipboardCommand>, read: Vec<HostClipboardCommand>) -> Self {
        Self {
            copy: HostClipboardCopyBackend::Commands(copy),
            read: HostClipboardReadBackend::Commands(read),
        }
    }

    /// Returns a strategy that uses configured commands where provided and
    /// falls back to the platform default command list for omitted directions.
    ///
    /// # Parameters
    /// - `copy`: The optional copy command that receives clipboard content on stdin.
    /// - `read`: The optional paste command whose stdout is read as clipboard text.
    pub fn configured(
        copy: Option<HostClipboardCommand>,
        read: Option<HostClipboardCommand>,
    ) -> Self {
        Self {
            copy: HostClipboardCopyBackend::Commands(
                copy.map(|command| vec![command])
                    .unwrap_or_else(host_clipboard_copy_commands),
            ),
            read: HostClipboardReadBackend::Commands(
                read.map(|command| vec![command])
                    .unwrap_or_else(host_clipboard_paste_commands),
            ),
        }
    }

    /// Returns a strategy backed by explicit function pointers.
    #[cfg(test)]
    pub(crate) fn new(copy: fn(&str) -> bool, read: fn() -> Option<String>) -> Self {
        Self {
            copy: HostClipboardCopyBackend::Function(copy),
            read: HostClipboardReadBackend::Function(read),
        }
    }

    /// Copies text into the configured host clipboard, returning whether it was
    /// accepted by the backend.
    pub fn copy(&self, content: &str) -> bool {
        match &self.copy {
            HostClipboardCopyBackend::Commands(commands) => {
                copy_to_host_clipboard_with_commands(content, commands)
            }
            HostClipboardCopyBackend::Function(copy) => copy(content),
        }
    }

    /// Reads text from the configured host clipboard backend.
    pub fn read(&self) -> Option<String> {
        match &self.read {
            HostClipboardReadBackend::Commands(commands) => {
                read_host_clipboard_with_commands(commands)
            }
            HostClipboardReadBackend::Function(read) => read(),
        }
    }
}

impl Default for HostClipboard {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self::system()
    }
}

impl fmt::Debug for HostClipboard {
    /// Runs the fmt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostClipboard")
            .finish_non_exhaustive()
    }
}

/// Runs the disabled host clipboard copy operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
fn disabled_host_clipboard_copy(_: &str) -> bool {
    false
}

/// Runs the disabled host clipboard read operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
fn disabled_host_clipboard_read() -> Option<String> {
    None
}

/// Carries Host Clipboard Command state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostClipboardCommand {
    /// Stores the executable name or path.
    program: String,
    /// Stores the executable arguments.
    args: Vec<String>,
}

impl HostClipboardCommand {
    /// Returns a host clipboard command from a program and argument vector.
    ///
    /// # Parameters
    /// - `program`: The executable name or path.
    /// - `args`: The command-line arguments supplied after the executable.
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
        }
    }
}

/// Runs the copy to host clipboard with commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn copy_to_host_clipboard_with_commands(
    content: &str,
    commands: &[HostClipboardCommand],
) -> bool {
    commands
        .iter()
        .any(|command| run_clipboard_copy_command(command, content))
}

/// Runs the read host clipboard with commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn read_host_clipboard_with_commands(
    commands: &[HostClipboardCommand],
) -> Option<String> {
    commands.iter().find_map(|command| {
        let output = Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).ok())
            .flatten()
    })
}

/// Runs the run clipboard copy command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn run_clipboard_copy_command(command: &HostClipboardCommand, content: &str) -> bool {
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let command = command.clone();
    let content = content.to_string();
    let spawned = thread::Builder::new()
        .name("mez-host-clipboard-copy".to_string())
        .spawn(move || {
            let Ok(mut child) = Command::new(&command.program)
                .args(&command.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                let _ = started_tx.send(false);
                return;
            };
            let Some(mut stdin) = child.stdin.take() else {
                let _ = started_tx.send(false);
                let _ = child.kill();
                let _ = child.wait();
                return;
            };
            let _ = started_tx.send(true);
            let write_ok = stdin.write_all(content.as_bytes()).is_ok();
            drop(stdin);
            if !write_ok {
                let _ = child.kill();
            }
            let _ = child.wait();
        });
    if spawned.is_err() {
        return false;
    }
    started_rx.recv().unwrap_or(false)
}

/// Runs the host clipboard copy commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn host_clipboard_copy_commands() -> Vec<HostClipboardCommand> {
    vec![
        HostClipboardCommand::new("wl-copy", Vec::new()),
        HostClipboardCommand::new(
            "xclip",
            vec!["-selection".to_string(), "clipboard".to_string()],
        ),
        HostClipboardCommand::new(
            "xsel",
            vec!["--clipboard".to_string(), "--input".to_string()],
        ),
        HostClipboardCommand::new("pbcopy", Vec::new()),
    ]
}

/// Runs the host clipboard paste commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn host_clipboard_paste_commands() -> Vec<HostClipboardCommand> {
    vec![
        HostClipboardCommand::new("wl-paste", vec!["--no-newline".to_string()]),
        HostClipboardCommand::new(
            "xclip",
            vec![
                "-selection".to_string(),
                "clipboard".to_string(),
                "-out".to_string(),
            ],
        ),
        HostClipboardCommand::new(
            "xsel",
            vec!["--clipboard".to_string(), "--output".to_string()],
        ),
        HostClipboardCommand::new("pbpaste", Vec::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::{HostClipboardCommand, read_host_clipboard_with_commands};

    /// Verifies host clipboard paste output is delivered exactly on successful
    /// UTF-8 decode.
    ///
    /// Clipboard contents can contain significant trailing newlines, such as
    /// shell here-doc terminators or intentionally blank final lines. The paste
    /// reader must not trim those bytes before sending the text to the pane.
    #[test]
    fn host_clipboard_read_preserves_trailing_newlines() {
        let commands = vec![HostClipboardCommand::new(
            "sh",
            vec!["-c".to_string(), "printf 'line\\n\\n'".to_string()],
        )];

        assert_eq!(
            read_host_clipboard_with_commands(&commands).as_deref(),
            Some("line\n\n")
        );
    }

    /// Verifies invalid host clipboard UTF-8 does not get lossy replacement
    /// characters pasted into the pane.
    ///
    /// Host paste commands expose byte streams, while the current pane-input
    /// paste path accepts text. Invalid UTF-8 should make that command unusable
    /// so the caller can continue to the next configured clipboard fallback.
    #[test]
    fn host_clipboard_read_skips_invalid_utf8_stdout() {
        let commands = vec![
            HostClipboardCommand::new("sh", vec!["-c".to_string(), "printf '\\377'".to_string()]),
            HostClipboardCommand::new("sh", vec!["-c".to_string(), "printf fallback".to_string()]),
        ];

        assert_eq!(
            read_host_clipboard_with_commands(&commands).as_deref(),
            Some("fallback")
        );
    }
}
