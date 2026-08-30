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
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use super::command::ResolvedExternalEditorCommand;
use crate::error::{MezError, Result};

pub(super) const INTERNAL_EDITOR_ARGUMENT: &str = "--mez-internal-external-editor";
const MANIFEST_VERSION: u8 = 1;
const MANIFEST_MAX_BYTES: u64 = 256 * 1024;
const RUNNER_FAILURE_EXIT_CODE: u8 = 125;

#[derive(Debug, Serialize, Deserialize)]
struct ExternalEditorRunnerManifest {
    version: u8,
    candidates: Vec<Vec<String>>,
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
        let mut command = Command::new(&candidate[0]);
        command
            .args(&candidate[1..])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => continue,
        };
        let status = child.wait().map_err(|_| "editor-wait")?;
        if let Some(code) = status.code() {
            return Ok(u8::try_from(code).unwrap_or(1));
        }
        let signal = status.signal().unwrap_or(1);
        return Ok(u8::try_from(128_i32.saturating_add(signal)).unwrap_or(255));
    }
    Err("editor-spawn")
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
