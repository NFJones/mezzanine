//! Terminal Paste implementation.
//!
//! This module owns the terminal paste boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{BTreeMap, DEFAULT_PASTE_BUFFER_LIMIT_BYTES, MezError, Result, SystemTime, UNIX_EPOCH};
use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

// Paste buffer storage and validation.

/// Carries Paste Buffer state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteBuffer {
    /// Stores the name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bytes: usize,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub created_at_unix_seconds: u64,
    /// Stores the origin value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub origin: Option<String>,
    /// Stores the preview value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub preview: String,
}

/// Carries Paste Buffer Entry state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PasteBufferEntry {
    /// Stores the content value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) content: String,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) created_at_unix_seconds: u64,
    /// Stores the sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) sequence: u64,
    /// Stores the origin value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) origin: Option<String>,
}

/// Carries Paste Buffers state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteBuffers {
    /// Stores the limit bytes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) limit_bytes: usize,
    /// Stores the next sequence value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_sequence: u64,
    /// Stores the buffers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) buffers: BTreeMap<String, PasteBufferEntry>,
}

impl PasteBuffers {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(limit_bytes: usize) -> Result<Self> {
        if limit_bytes == 0 {
            return Err(MezError::invalid_args(
                "paste buffer byte limit must be greater than zero",
            ));
        }
        Ok(Self {
            limit_bytes,
            next_sequence: 1,
            buffers: BTreeMap::new(),
        })
    }

    /// Runs the default limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn default_limit() -> Self {
        Self::new(DEFAULT_PASTE_BUFFER_LIMIT_BYTES).expect("default paste buffer limit is non-zero")
    }

    /// Runs the set operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set(&mut self, name: impl Into<String>, content: impl Into<String>) -> Result<()> {
        self.set_with_origin(name, content, None)
    }

    /// Runs the set with origin operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_with_origin(
        &mut self,
        name: impl Into<String>,
        content: impl Into<String>,
        origin: Option<String>,
    ) -> Result<()> {
        let name = name.into();
        validate_paste_buffer_name(&name)?;
        let content = content.into();
        if content.len() > self.limit_bytes {
            return Err(MezError::invalid_args(
                "paste buffer content exceeds configured byte limit",
            ));
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.buffers.insert(
            name,
            PasteBufferEntry {
                content,
                created_at_unix_seconds: current_unix_seconds(),
                sequence,
                origin,
            },
        );
        Ok(())
    }

    /// Creates a paste buffer without overwriting an existing buffer.
    ///
    /// The buffer name is validated with the same rules as `set_with_origin`.
    /// The return value is `true` when a new buffer was inserted and `false`
    /// when a buffer with that name already existed.
    pub fn create_with_origin(
        &mut self,
        name: impl Into<String>,
        content: impl Into<String>,
        origin: Option<String>,
    ) -> Result<bool> {
        let name = name.into();
        validate_paste_buffer_name(&name)?;
        if self.buffers.contains_key(&name) {
            return Ok(false);
        }
        let content = content.into();
        if content.len() > self.limit_bytes {
            return Err(MezError::invalid_args(
                "paste buffer content exceeds configured byte limit",
            ));
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.buffers.insert(
            name,
            PasteBufferEntry {
                content,
                created_at_unix_seconds: current_unix_seconds(),
                sequence,
                origin,
            },
        );
        Ok(true)
    }

    /// Runs the get operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.buffers.get(name).map(|entry| entry.content.as_str())
    }

    /// Runs the delete operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn delete(&mut self, name: &str) -> bool {
        self.buffers.remove(name).is_some()
    }

    /// Runs the most recent name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn most_recent_name(&self) -> Option<&str> {
        self.buffers
            .iter()
            .max_by_key(|(_, entry)| entry.sequence)
            .map(|(name, _)| name.as_str())
    }

    /// Runs the delete most recent operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn delete_most_recent(&mut self) -> Option<String> {
        let name = self.most_recent_name()?.to_string();
        if self.delete(&name) { Some(name) } else { None }
    }

    /// Runs the list operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn list(&self) -> Vec<PasteBuffer> {
        self.buffers
            .iter()
            .map(|(name, entry)| PasteBuffer {
                name: name.clone(),
                bytes: entry.content.len(),
                created_at_unix_seconds: entry.created_at_unix_seconds,
                origin: entry.origin.clone(),
                preview: paste_buffer_preview(&entry.content),
            })
            .collect()
    }
}

/// Runs the current unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Runs the paste buffer preview operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn paste_buffer_preview(content: &str) -> String {
    let mut preview = content
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(40)
        .collect::<String>();
    if content.chars().count() > 40 {
        preview.push_str("...");
    }
    preview
}

/// Runs the validate paste buffer name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_paste_buffer_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(MezError::invalid_args("paste buffer name is invalid"));
    }
    Ok(())
}

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
fn disabled_host_clipboard_copy(_: &str) -> bool {
    false
}

/// Runs the disabled host clipboard read operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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
        output.status.success().then(|| {
            String::from_utf8_lossy(&output.stdout)
                .trim_end_matches(['\r', '\n'])
                .to_string()
        })
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
