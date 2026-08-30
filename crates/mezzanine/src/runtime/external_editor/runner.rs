//! Hidden fallback-aware external-editor child runner.
//!
//! The pane shell launches this Mezzanine-owned process as its foreground job.
//! It reads one owner-only manifest, starts configured editors with inherited
//! terminal streams, falls back only when spawning fails, and mirrors the exit
//! status of the first editor that actually starts.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::command::ResolvedExternalEditorCommand;
use crate::error::{MezError, Result};

pub(super) const INTERNAL_EDITOR_ARGUMENT: &str = "--mez-internal-external-editor";
const MANIFEST_VERSION: u8 = 1;
const MANIFEST_MAX_BYTES: u64 = 256 * 1024;
const RUNNER_FAILURE_EXIT_CODE: u8 = 125;
/// Poll cadence while waiting for either child exit or a blocked lifecycle signal.
const EDITOR_SIGNAL_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Grace period after forwarding an interrupt or graceful termination signal.
const EDITOR_SIGNAL_GRACE_PERIOD: Duration = Duration::from_millis(200);

#[derive(Debug, Serialize, Deserialize)]
struct ExternalEditorRunnerManifest {
    version: u8,
    candidates: Vec<Vec<String>>,
}

/// Thread-local lifecycle signal mask restored when runner work completes.
struct RunnerSignalMask {
    blocked: libc::sigset_t,
    previous: libc::sigset_t,
}

impl RunnerSignalMask {
    /// Blocks signals that must be forwarded to the managed editor process group.
    fn block() -> std::result::Result<Self, &'static str> {
        // SAFETY: both signal sets are initialized before use, and
        // `pthread_sigmask` changes only the calling runner thread.
        unsafe {
            let mut blocked = std::mem::zeroed::<libc::sigset_t>();
            let mut previous = std::mem::zeroed::<libc::sigset_t>();
            if libc::sigemptyset(&mut blocked) != 0
                || libc::sigaddset(&mut blocked, libc::SIGHUP) != 0
                || libc::sigaddset(&mut blocked, libc::SIGINT) != 0
                || libc::sigaddset(&mut blocked, libc::SIGTERM) != 0
                || libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous) != 0
            {
                return Err("signal-mask");
            }
            Ok(Self { blocked, previous })
        }
    }

    /// Consumes one pending lifecycle signal without blocking child polling.
    fn take_pending(&self) -> std::result::Result<Option<i32>, &'static str> {
        // SAFETY: both sets are initialized before inspection. `sigpending`
        // and `sigwait` are POSIX APIs available on Linux and macOS; the
        // latter cannot block after membership proves one blocked signal is
        // already pending for this process.
        unsafe {
            let mut pending = std::mem::zeroed::<libc::sigset_t>();
            if libc::sigpending(&mut pending) != 0 {
                return Err("signal-pending");
            }
            let has_lifecycle_signal = [libc::SIGHUP, libc::SIGINT, libc::SIGTERM]
                .into_iter()
                .any(|signal| libc::sigismember(&pending, signal) == 1);
            if !has_lifecycle_signal {
                return Ok(None);
            }
            let mut signal = 0;
            if libc::sigwait(&self.blocked, &mut signal) != 0 {
                return Err("signal-wait");
            }
            Ok(Some(signal))
        }
    }

    /// Returns the signal mask that editor children must inherit at exec.
    fn previous(&self) -> libc::sigset_t {
        self.previous
    }
}

impl Drop for RunnerSignalMask {
    /// Restores the runner thread's prior signal mask.
    fn drop(&mut self) {
        // SAFETY: `previous` was populated by the successful blocking call.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut());
        }
    }
}

/// Best-effort ownership guard for one private editor process group.
struct EditorProcessGroup {
    process_group_id: i32,
    armed: bool,
}

impl EditorProcessGroup {
    /// Arms group cleanup for a successfully spawned editor child.
    fn new(child: &Child) -> std::result::Result<Self, &'static str> {
        Ok(Self {
            process_group_id: i32::try_from(child.id()).map_err(|_| "editor-pid")?,
            armed: true,
        })
    }

    /// Sends one signal to every process in the private editor group.
    fn signal(&self, signal: i32) {
        // SAFETY: a negative pid targets only the private process group whose
        // leader id came from the successfully spawned direct child.
        unsafe {
            libc::kill(-self.process_group_id, signal);
        }
    }

    /// Prevents drop cleanup after the direct child has been reaped.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for EditorProcessGroup {
    /// Prevents descendants from escaping when runner waiting fails early.
    fn drop(&mut self) {
        if self.armed {
            self.signal(libc::SIGKILL);
        }
    }
}

/// Serializes resolved editor candidates into one bounded inert artifact.
pub(super) fn external_editor_runner_manifest(
    commands: &[ResolvedExternalEditorCommand],
) -> Result<Vec<u8>> {
    let manifest = ExternalEditorRunnerManifest {
        version: MANIFEST_VERSION,
        candidates: commands
            .iter()
            .map(|command| {
                std::iter::once(command.executable.clone())
                    .chain(command.arguments.iter().cloned())
                    .collect()
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|error| {
        MezError::invalid_state(format!("failed to encode editor runner manifest: {error}"))
    })?;
    if bytes.len() as u64 > MANIFEST_MAX_BYTES {
        return Err(MezError::invalid_args(
            "external editor runner manifest exceeds the size limit",
        ));
    }
    Ok(bytes)
}

/// Dispatches the exact hidden external-editor process mode.
pub(crate) fn run_internal_process(arguments: &[OsString]) -> Option<u8> {
    let mode = arguments.get(1)?.to_str()?;
    if mode != INTERNAL_EDITOR_ARGUMENT {
        return None;
    }
    let result = if arguments.len() == 3 {
        run_manifest(Path::new(&arguments[2]))
    } else {
        Err("invalid-arguments")
    };
    Some(result.unwrap_or(RUNNER_FAILURE_EXIT_CODE))
}

fn run_manifest(path: &Path) -> std::result::Result<u8, &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "manifest-metadata")?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > MANIFEST_MAX_BYTES
    {
        return Err("manifest-safety");
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| "manifest-open")?
        .take(MANIFEST_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "manifest-read")?;
    if bytes.len() as u64 > MANIFEST_MAX_BYTES {
        return Err("manifest-size");
    }
    let manifest: ExternalEditorRunnerManifest =
        serde_json::from_slice(&bytes).map_err(|_| "manifest-decode")?;
    validate_manifest(&manifest)?;

    for candidate in manifest.candidates {
        let signal_mask = RunnerSignalMask::block()?;
        let mut command = Command::new(&candidate[0]);
        command
            .args(&candidate[1..])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        command.process_group(0);
        let child_signal_mask = signal_mask.previous();
        // SAFETY: this closure runs after fork and before exec. It performs
        // only the async-signal-safe `pthread_sigmask` operation using a fully
        // initialized mask captured by value.
        unsafe {
            command.pre_exec(move || {
                let result = libc::pthread_sigmask(
                    libc::SIG_SETMASK,
                    &child_signal_mask,
                    std::ptr::null_mut(),
                );
                if result != 0 {
                    return Err(std::io::Error::from_raw_os_error(result));
                }
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => continue,
        };
        let mut process_group = EditorProcessGroup::new(&child)?;
        let status = wait_for_editor(&mut child, &process_group, &signal_mask)?;
        process_group.disarm();
        if let Some(code) = status.code() {
            return Ok(u8::try_from(code).unwrap_or(1));
        }
        let signal = status.signal().unwrap_or(1);
        return Ok(u8::try_from(128_i32.saturating_add(signal)).unwrap_or(255));
    }
    Err("editor-spawn")
}

/// Waits for editor completion while forwarding runner lifecycle signals.
fn wait_for_editor(
    child: &mut Child,
    process_group: &EditorProcessGroup,
    signal_mask: &RunnerSignalMask,
) -> std::result::Result<ExitStatus, &'static str> {
    loop {
        if let Some(status) = child.try_wait().map_err(|_| "editor-wait")? {
            return Ok(status);
        }
        let Some(signal) = signal_mask.take_pending()? else {
            thread::sleep(EDITOR_SIGNAL_POLL_INTERVAL);
            continue;
        };
        process_group.signal(signal);
        if let Some(status) = wait_for_editor_grace(child)? {
            process_group.signal(libc::SIGKILL);
            return Ok(status);
        }
        if signal != libc::SIGTERM {
            process_group.signal(libc::SIGTERM);
            if let Some(status) = wait_for_editor_grace(child)? {
                process_group.signal(libc::SIGKILL);
                return Ok(status);
            }
        }
        process_group.signal(libc::SIGKILL);
        return child.wait().map_err(|_| "editor-wait");
    }
}

/// Polls the direct editor child during one bounded signal grace period.
fn wait_for_editor_grace(
    child: &mut Child,
) -> std::result::Result<Option<ExitStatus>, &'static str> {
    let deadline = Instant::now() + EDITOR_SIGNAL_GRACE_PERIOD;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().map_err(|_| "editor-wait")? {
            return Ok(Some(status));
        }
        thread::sleep(EDITOR_SIGNAL_POLL_INTERVAL);
    }
    Ok(None)
}

fn validate_manifest(
    manifest: &ExternalEditorRunnerManifest,
) -> std::result::Result<(), &'static str> {
    if manifest.version != MANIFEST_VERSION || !(1..=16).contains(&manifest.candidates.len()) {
        return Err("manifest-contract");
    }
    for candidate in &manifest.candidates {
        let Some(executable) = candidate.first() else {
            return Err("candidate-empty");
        };
        if !Path::new(executable).is_absolute()
            || candidate.iter().any(|argument| {
                argument.contains('\0') || argument.bytes().any(|byte| byte.is_ascii_control())
            })
        {
            return Err("candidate-invalid");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(root: &Path, candidates: Vec<Vec<String>>) -> std::path::PathBuf {
        fs::create_dir_all(root).unwrap();
        let path = root.join("manifest.json");
        fs::write(
            &path,
            serde_json::to_vec(&ExternalEditorRunnerManifest {
                version: MANIFEST_VERSION,
                candidates,
            })
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    /// Verifies spawn failure advances to the next candidate, while a started
    /// editor's nonzero exit is returned without trying later fallbacks.
    #[test]
    fn runner_distinguishes_spawn_failure_from_editor_failure() {
        let root =
            std::env::temp_dir().join(format!("mez-editor-runner-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let missing = root.join("missing-editor").to_string_lossy().into_owned();
        let manifest = write_manifest(
            &root,
            vec![
                vec![missing],
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "exit 0".to_string(),
                ],
            ],
        );
        assert_eq!(run_manifest(&manifest), Ok(0));

        let manifest = write_manifest(
            &root,
            vec![
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "exit 7".to_string(),
                ],
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "exit 0".to_string(),
                ],
            ],
        );
        assert_eq!(run_manifest(&manifest), Ok(7));
        let _ = fs::remove_dir_all(root);
    }
}
