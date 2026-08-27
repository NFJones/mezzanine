//! Product-owned host clipboard process adapters.
//!
//! Generic paste-buffer state lives in `mez_mux::paste`; this module retains
//! platform command discovery and host clipboard process execution.

use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokio::io::AsyncReadExt;

/// Default total deadline for one ordered host-clipboard read attempt.
pub const DEFAULT_HOST_CLIPBOARD_READ_TIMEOUT: Duration = Duration::from_millis(250);
/// Default maximum host-clipboard payload retained for one paste.
pub const DEFAULT_HOST_CLIPBOARD_READ_MAX_BYTES: usize = 1024 * 1024;

/// Copies text to the host clipboard using common platform clipboard tools.
///
/// The operation returns `false` instead of surfacing errors because clipboard
/// access is best-effort in headless, SSH, and restricted desktop sessions.
pub fn copy_to_host_clipboard(content: &str) -> bool {
    copy_to_host_clipboard_with_commands(content, &host_clipboard_copy_commands())
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
    /// Total deadline shared by all ordered paste command attempts.
    read_timeout: Duration,
    /// Maximum accepted host clipboard payload size.
    read_max_bytes: usize,
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
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "function backend is retained for deterministic test adapters"
        )
    )]
    Function(fn() -> Option<String>),
}

/// Immutable worker input for one bounded host-clipboard acquisition.
///
/// The plan is detached from runtime presentation state so an async worker can
/// execute it without retaining or borrowing the serialized runtime actor.
#[derive(Clone)]
pub struct HostClipboardReadPlan {
    /// Concrete command or test-function backend to execute.
    backend: HostClipboardReadBackend,
    /// Total deadline shared by all backend work.
    timeout: Duration,
    /// Maximum accepted payload size.
    max_bytes: usize,
}

#[cfg(test)]
impl HostClipboardReadPlan {
    /// Returns the total deadline shared by all ordered clipboard backends.
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Returns the maximum accepted host clipboard payload size.
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

impl fmt::Debug for HostClipboardReadPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let backend = match &self.backend {
            HostClipboardReadBackend::Commands(_) => "commands",
            HostClipboardReadBackend::Function(_) => "function",
        };
        formatter
            .debug_struct("HostClipboardReadPlan")
            .field("backend", &backend)
            .field("timeout", &self.timeout)
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

impl PartialEq for HostClipboardReadPlan {
    fn eq(&self, other: &Self) -> bool {
        let backends_equal = match (&self.backend, &other.backend) {
            (
                HostClipboardReadBackend::Commands(left),
                HostClipboardReadBackend::Commands(right),
            ) => left == right,
            (
                HostClipboardReadBackend::Function(left),
                HostClipboardReadBackend::Function(right),
            ) => *left as usize == *right as usize,
            _ => false,
        };
        backends_equal && self.timeout == other.timeout && self.max_bytes == other.max_bytes
    }
}

impl Eq for HostClipboardReadPlan {}

impl HostClipboard {
    /// Returns the system clipboard strategy backed by host clipboard tools.
    pub fn system() -> Self {
        Self {
            copy: HostClipboardCopyBackend::Function(copy_to_host_clipboard),
            read: HostClipboardReadBackend::Commands(host_clipboard_paste_commands()),
            read_timeout: DEFAULT_HOST_CLIPBOARD_READ_TIMEOUT,
            read_max_bytes: DEFAULT_HOST_CLIPBOARD_READ_MAX_BYTES,
        }
    }

    /// Returns a strategy that silently ignores copy and paste requests.
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            copy: HostClipboardCopyBackend::Function(disabled_host_clipboard_copy),
            read: HostClipboardReadBackend::Function(disabled_host_clipboard_read),
            read_timeout: DEFAULT_HOST_CLIPBOARD_READ_TIMEOUT,
            read_max_bytes: DEFAULT_HOST_CLIPBOARD_READ_MAX_BYTES,
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
            read_timeout: DEFAULT_HOST_CLIPBOARD_READ_TIMEOUT,
            read_max_bytes: DEFAULT_HOST_CLIPBOARD_READ_MAX_BYTES,
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
            read_timeout: DEFAULT_HOST_CLIPBOARD_READ_TIMEOUT,
            read_max_bytes: DEFAULT_HOST_CLIPBOARD_READ_MAX_BYTES,
        }
    }

    /// Returns a strategy backed by explicit function pointers.
    #[cfg(test)]
    pub(crate) fn new(copy: fn(&str) -> bool, read: fn() -> Option<String>) -> Self {
        Self {
            copy: HostClipboardCopyBackend::Function(copy),
            read: HostClipboardReadBackend::Function(read),
            read_timeout: DEFAULT_HOST_CLIPBOARD_READ_TIMEOUT,
            read_max_bytes: DEFAULT_HOST_CLIPBOARD_READ_MAX_BYTES,
        }
    }

    /// Overrides the finite deadline and accepted payload size for reads.
    ///
    /// Zero values are clamped to one millisecond and one byte respectively so
    /// every resulting plan retains meaningful finite bounds.
    pub fn with_read_limits(mut self, timeout: Duration, max_bytes: usize) -> Self {
        self.read_timeout = timeout.max(Duration::from_millis(1));
        self.read_max_bytes = max_bytes.max(1);
        self
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

    /// Returns immutable bounded worker input for a host clipboard read.
    pub fn read_plan(&self) -> HostClipboardReadPlan {
        HostClipboardReadPlan {
            backend: self.read.clone(),
            timeout: self.read_timeout,
            max_bytes: self.read_max_bytes,
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

/// Executes one host-clipboard read plan with a total deadline and byte cap.
///
/// Command backends are tried in platform preference order. Each child starts
/// in a private process group on Unix, stdout is drained without retaining
/// bytes beyond the configured cap, and timeout cleanup terminates descendants
/// before reaping the direct child. Any failure is represented as absence so
/// runtime completion handling can select the internal paste-buffer fallback.
pub async fn read_host_clipboard_plan_async(plan: HostClipboardReadPlan) -> Option<String> {
    match plan.backend {
        HostClipboardReadBackend::Function(read) => {
            let result = tokio::time::timeout(plan.timeout, tokio::task::spawn_blocking(read))
                .await
                .ok()?
                .ok()??;
            (result.len() <= plan.max_bytes).then_some(result)
        }
        HostClipboardReadBackend::Commands(commands) => {
            read_host_clipboard_commands_async(commands, plan.timeout, plan.max_bytes).await
        }
    }
}

/// Best-effort process-group guard for a host clipboard paste helper.
struct HostClipboardProcessGroupGuard {
    #[cfg(unix)]
    process_group_id: Option<i32>,
    armed: bool,
}

impl HostClipboardProcessGroupGuard {
    /// Arms cleanup for the private process group of one spawned child.
    fn new(child: &tokio::process::Child) -> Self {
        Self {
            #[cfg(unix)]
            process_group_id: child.id().and_then(|id| i32::try_from(id).ok()),
            armed: true,
        }
    }

    /// Prevents cleanup after normal child completion and reaping.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Terminates the complete private process group when supported.
    fn terminate(&self) {
        if !self.armed {
            return;
        }
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id {
            // SAFETY: the pid belongs to a child successfully started in its
            // own process group, and a negative pid targets only that group.
            unsafe {
                libc::kill(-process_group_id, libc::SIGKILL);
            }
        }
    }
}

impl Drop for HostClipboardProcessGroupGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Runs ordered command backends under one shared deadline.
async fn read_host_clipboard_commands_async(
    commands: Vec<HostClipboardCommand>,
    timeout: Duration,
    max_bytes: usize,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    for command in commands {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let mut process = tokio::process::Command::new(&command.program);
        process
            .args(&command.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            process.as_std_mut().process_group(0);
        }
        let Ok(mut child) = process.spawn() else {
            continue;
        };
        let mut process_group = HostClipboardProcessGroupGuard::new(&child);
        let Some(stdout) = child.stdout.take() else {
            process_group.terminate();
            let _ = child.start_kill();
            let _ = child.wait().await;
            process_group.disarm();
            continue;
        };
        let completed = tokio::time::timeout(remaining, async {
            tokio::join!(
                child.wait(),
                read_bounded_clipboard_stdout(stdout, max_bytes)
            )
        })
        .await;
        let Ok((status, output)) = completed else {
            process_group.terminate();
            let _ = child.start_kill();
            let _ = child.wait().await;
            process_group.disarm();
            return None;
        };
        process_group.disarm();
        let Ok(status) = status else {
            continue;
        };
        let Ok((bytes, overflowed)) = output else {
            continue;
        };
        if status.success()
            && !overflowed
            && let Ok(content) = String::from_utf8(bytes)
        {
            return Some(content);
        }
    }
    None
}

/// Drains helper stdout while retaining at most the configured byte count.
async fn read_bounded_clipboard_stdout(
    mut stdout: tokio::process::ChildStdout,
    max_bytes: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::with_capacity(max_bytes.min(8192));
    let mut overflowed = false;
    let mut chunk = [0u8; 8192];
    loop {
        let read = stdout.read(&mut chunk).await?;
        if read == 0 {
            return Ok((retained, overflowed));
        }
        let remaining = max_bytes.saturating_sub(retained.len());
        let accepted = remaining.min(read);
        retained.extend_from_slice(&chunk[..accepted]);
        overflowed |= accepted < read;
    }
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

/// PowerShell command body that decodes redirected WSL stdin as UTF-8 before
/// writing the resulting text to the Windows host clipboard.
const WSL_POWERSHELL_COPY_SCRIPT: &str = "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false); Set-Clipboard -Value ([Console]::In.ReadToEnd())";

/// Returns whether the current Linux process is running under WSL.
///
/// Environment markers cover ordinary launches, while the kernel release
/// fallback covers stripped environments such as service and multiplexer
/// processes that do not retain `WSL_INTEROP` or `WSL_DISTRO_NAME`.
fn is_windows_subsystem_for_linux() -> bool {
    std::env::var_os("WSL_INTEROP").is_some()
        || std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .is_ok_and(|release| kernel_release_is_wsl(release.as_str()))
}

/// Returns whether one kernel release identifies Microsoft's WSL kernel.
fn kernel_release_is_wsl(release: &str) -> bool {
    let release = release.to_ascii_lowercase();
    release.contains("microsoft") || release.contains("wsl")
}

/// Runs the host clipboard copy commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn host_clipboard_copy_commands() -> Vec<HostClipboardCommand> {
    host_clipboard_copy_commands_for_environment(is_windows_subsystem_for_linux())
}

/// Builds the ordered default copy commands for one detected environment.
fn host_clipboard_copy_commands_for_environment(
    windows_subsystem_for_linux: bool,
) -> Vec<HostClipboardCommand> {
    let mut commands = Vec::new();
    if windows_subsystem_for_linux {
        commands.push(HostClipboardCommand::new(
            "powershell.exe",
            vec![
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                WSL_POWERSHELL_COPY_SCRIPT.to_string(),
            ],
        ));
    }
    commands.extend([
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
    ]);
    commands
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
    use super::{
        HostClipboard, HostClipboardCommand, WSL_POWERSHELL_COPY_SCRIPT,
        host_clipboard_copy_commands_for_environment, kernel_release_is_wsl,
        read_host_clipboard_plan_async,
    };
    use std::time::Duration;

    /// Verifies WSL defaults address the Windows host clipboard before trying
    /// Linux display-server helpers and explicitly decode redirected UTF-8.
    ///
    /// Iroh clipboard effects execute on the attaching client, so a WSL client
    /// must bridge to Windows rather than relying on unavailable X11/Wayland.
    #[test]
    fn wsl_copy_defaults_target_windows_host_clipboard_with_utf8() {
        let commands = host_clipboard_copy_commands_for_environment(true);

        assert_eq!(
            commands.first(),
            Some(&HostClipboardCommand::new(
                "powershell.exe",
                vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    WSL_POWERSHELL_COPY_SCRIPT.to_string(),
                ],
            ))
        );
        assert!(WSL_POWERSHELL_COPY_SCRIPT.contains("InputEncoding"));
        assert!(WSL_POWERSHELL_COPY_SCRIPT.contains("Set-Clipboard"));
        assert_eq!(commands[1].program, "wl-copy");
    }

    /// Verifies ordinary Linux and macOS command discovery does not acquire a
    /// Windows-only dependency, and stripped WSL environments remain detected
    /// from the Microsoft kernel release marker.
    #[test]
    fn non_wsl_copy_defaults_remain_portable_and_kernel_detection_is_case_insensitive() {
        let commands = host_clipboard_copy_commands_for_environment(false);

        assert_eq!(
            commands.first().map(|command| command.program.as_str()),
            Some("wl-copy")
        );
        assert!(
            commands
                .iter()
                .all(|command| command.program != "powershell.exe")
        );
        assert!(kernel_release_is_wsl("6.6.87.2-MICROSOFT-standard-WSL2"));
        assert!(!kernel_release_is_wsl("6.8.0-52-generic"));
    }

    /// Verifies host clipboard paste output is delivered exactly on successful
    /// UTF-8 decode.
    ///
    /// Clipboard contents can contain significant trailing newlines, such as
    /// shell here-doc terminators or intentionally blank final lines. The paste
    /// reader must not trim those bytes before sending the text to the pane.
    #[tokio::test]
    async fn host_clipboard_read_preserves_trailing_newlines() {
        let clipboard = HostClipboard::commands(
            Vec::new(),
            vec![HostClipboardCommand::new(
                "sh",
                vec!["-c".to_string(), "printf 'line\\n\\n'".to_string()],
            )],
        );

        assert_eq!(
            read_host_clipboard_plan_async(clipboard.read_plan())
                .await
                .as_deref(),
            Some("line\n\n")
        );
    }

    /// Verifies invalid host clipboard UTF-8 does not get lossy replacement
    /// characters pasted into the pane.
    ///
    /// Host paste commands expose byte streams, while the current pane-input
    /// paste path accepts text. Invalid UTF-8 should make that command unusable
    /// so the caller can continue to the next configured clipboard fallback.
    #[tokio::test]
    async fn host_clipboard_read_skips_invalid_utf8_stdout() {
        let clipboard = HostClipboard::commands(
            Vec::new(),
            vec![
                HostClipboardCommand::new(
                    "sh",
                    vec!["-c".to_string(), "printf '\\377'".to_string()],
                ),
                HostClipboardCommand::new(
                    "sh",
                    vec!["-c".to_string(), "printf fallback".to_string()],
                ),
            ],
        );

        assert_eq!(
            read_host_clipboard_plan_async(clipboard.read_plan())
                .await
                .as_deref(),
            Some("fallback")
        );
    }

    /// Verifies output beyond the accepted byte count is discarded and does
    /// not become a partial clipboard paste.
    #[tokio::test]
    async fn host_clipboard_read_rejects_oversized_output() {
        let clipboard = HostClipboard::commands(
            Vec::new(),
            vec![HostClipboardCommand::new(
                "sh",
                vec!["-c".to_string(), "printf 12345".to_string()],
            )],
        )
        .with_read_limits(Duration::from_secs(1), 4);

        assert_eq!(
            read_host_clipboard_plan_async(clipboard.read_plan()).await,
            None
        );
    }

    /// Verifies a helper that does not exit is terminated at the total read
    /// deadline instead of retaining an actor or worker indefinitely.
    #[tokio::test]
    async fn host_clipboard_read_times_out_hung_helper() {
        let clipboard = HostClipboard::commands(
            Vec::new(),
            vec![HostClipboardCommand::new(
                "sh",
                vec!["-c".to_string(), "sleep 5".to_string()],
            )],
        )
        .with_read_limits(Duration::from_millis(20), 1024);

        let started = tokio::time::Instant::now();
        assert_eq!(
            read_host_clipboard_plan_async(clipboard.read_plan()).await,
            None
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
