//! Remote snapshot parsing and text-file state for semantic patches.
//!
//! The apply-patch read phase emits a deterministic marker-framed snapshot of
//! every touched path. This module parses that transport format, normalizes
//! regular files into text-file state for hunk matching, and exposes the final
//! file-change representation consumed by the shell transaction generator.

use super::{
    APPLY_PATCH_CONTENT_BEGIN_MARKER, APPLY_PATCH_CONTENT_END_MARKER,
    APPLY_PATCH_FILE_BEGIN_MARKER, APPLY_PATCH_FILE_END_MARKER, APPLY_PATCH_READ_BEGIN_MARKER,
    APPLY_PATCH_READ_END_MARKER, apply_patch_parse_error,
};
use crate::error::{MezError, Result};
use base64::Engine;
use std::collections::BTreeMap;

/// One path snapshot emitted by the apply-patch read phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApplyPatchSnapshot {
    /// The model-authored relative path.
    pub(super) path: String,
    /// The shell-resolved absolute path.
    pub(super) resolved_path: String,
    /// File-system state observed during the read phase.
    state: ApplyPatchSnapshotState,
}

/// File-system state for one apply-patch snapshot path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ApplyPatchSnapshotState {
    /// A regular file and its exact bytes.
    Regular(Vec<u8>),
    /// No file or symlink exists at the target path.
    Missing,
    /// The target exists but is not a regular file.
    NonRegular,
    /// The resolved path escapes the current working directory.
    OutsideCwd,
    /// The shell could not resolve the target path.
    Error,
}

/// Verified file change generated from a parsed patch and remote snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApplyPatchFileChange {
    /// The model-authored relative path.
    pub(super) path: String,
    /// The shell-resolved absolute path captured during the read phase.
    pub(super) resolved_path: String,
    /// Original file state used for concurrency checks.
    pub(super) original: ApplyPatchOriginalState,
    /// Final bytes to write, or `None` when deleting the path.
    pub(super) final_bytes: Option<Vec<u8>>,
}

/// Original file state used by the write phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ApplyPatchOriginalState {
    /// Original bytes of an existing regular file.
    Regular(Vec<u8>),
    /// The file was absent when the read phase ran.
    Missing,
}

/// UTF-8 text representation used by the hunk matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApplyPatchTextFile {
    /// File lines without trailing newline markers.
    pub(super) lines: Vec<String>,
    /// Whether the original/final file ends with a newline.
    pub(super) trailing_newline: bool,
}

impl ApplyPatchTextFile {
    /// Builds text-file state from regular file bytes.
    ///
    /// # Parameters
    /// - `path`: Path used only for diagnostic context.
    /// - `bytes`: Exact file bytes captured by the read phase.
    pub(super) fn from_bytes(path: &str, bytes: &[u8]) -> Result<Self> {
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| {
            MezError::invalid_args(format!("apply_patch: file is not valid UTF-8: {path}"))
        })?;
        Ok(Self {
            lines: text.lines().map(ToString::to_string).collect(),
            trailing_newline: text.ends_with('\n'),
        })
    }

    /// Builds text-file state from already parsed lines.
    ///
    /// # Parameters
    /// - `lines`: Content lines without newline markers.
    /// - `trailing_newline`: Whether serialized bytes should end with `\n`.
    pub(super) fn from_lines(lines: Vec<String>, trailing_newline: bool) -> Self {
        Self {
            lines,
            trailing_newline,
        }
    }

    /// Serializes text-file state back to bytes.
    pub(super) fn into_bytes(self) -> Vec<u8> {
        let mut text = self.lines.join("\n");
        if !self.lines.is_empty() && self.trailing_newline {
            text.push('\n');
        }
        text.into_bytes()
    }
}

/// Parses the marker-framed output produced by the apply-patch read phase.
///
/// # Parameters
/// - `output`: Decoded shell output from the read phase.
pub(super) fn parse_apply_patch_snapshot_output(
    output: &str,
) -> Result<BTreeMap<String, ApplyPatchSnapshot>> {
    let mut lines = output.replace("\r\n", "\n").replace('\r', "\n");
    if !lines.ends_with('\n') {
        lines.push('\n');
    }
    let lines = lines.lines().collect::<Vec<_>>();
    let begin = lines
        .iter()
        .position(|line| *line == APPLY_PATCH_READ_BEGIN_MARKER)
        .ok_or_else(|| {
            MezError::invalid_args("apply_patch: read phase did not emit a snapshot begin marker")
        })?;
    let end = lines
        .iter()
        .rposition(|line| *line == APPLY_PATCH_READ_END_MARKER)
        .ok_or_else(|| {
            MezError::invalid_args("apply_patch: read phase did not emit a snapshot end marker")
        })?;
    let mut index = begin + 1;
    let mut snapshots = BTreeMap::new();
    while index < end {
        if lines[index] != APPLY_PATCH_FILE_BEGIN_MARKER {
            return apply_patch_parse_error("malformed remote snapshot entry");
        }
        index += 1;
        let path = decode_apply_patch_snapshot_field(lines.get(index), "PATH_B64")?;
        index += 1;
        let resolved_path = decode_apply_patch_snapshot_field(lines.get(index), "RESOLVED_B64")?;
        index += 1;
        let status = lines
            .get(index)
            .and_then(|line| line.strip_prefix("STATUS "))
            .ok_or_else(|| {
                MezError::invalid_args("apply_patch: malformed remote snapshot status")
            })?;
        index += 1;
        let state = match status {
            "regular" => {
                if lines.get(index) != Some(&APPLY_PATCH_CONTENT_BEGIN_MARKER) {
                    return apply_patch_parse_error("regular remote snapshot omitted content");
                }
                index += 1;
                let mut encoded = String::new();
                while index < end && lines[index] != APPLY_PATCH_CONTENT_END_MARKER {
                    encoded.push_str(lines[index].trim());
                    index += 1;
                }
                if lines.get(index) != Some(&APPLY_PATCH_CONTENT_END_MARKER) {
                    return apply_patch_parse_error(
                        "regular remote snapshot content was unterminated",
                    );
                }
                index += 1;
                ApplyPatchSnapshotState::Regular(decode_base64_bytes(&encoded, "file content")?)
            }
            "missing" => ApplyPatchSnapshotState::Missing,
            "non_regular" => ApplyPatchSnapshotState::NonRegular,
            "outside_cwd" => ApplyPatchSnapshotState::OutsideCwd,
            _ => ApplyPatchSnapshotState::Error,
        };
        if lines.get(index) != Some(&APPLY_PATCH_FILE_END_MARKER) {
            return apply_patch_parse_error("remote snapshot entry was unterminated");
        }
        index += 1;
        snapshots.insert(
            path.clone(),
            ApplyPatchSnapshot {
                path,
                resolved_path,
                state,
            },
        );
    }
    Ok(snapshots)
}

/// Decodes one named base64 snapshot field.
///
/// # Parameters
/// - `line`: The optional raw snapshot line.
/// - `name`: The expected field name.
fn decode_apply_patch_snapshot_field(line: Option<&&str>, name: &str) -> Result<String> {
    let encoded = line
        .and_then(|line| line.strip_prefix(&format!("{name} ")))
        .ok_or_else(|| {
            MezError::invalid_args(format!("apply_patch: missing snapshot field {name}"))
        })?;
    let bytes = decode_base64_bytes(encoded, name)?;
    String::from_utf8(bytes).map_err(|_| {
        MezError::invalid_args(format!("apply_patch: snapshot field {name} is not UTF-8"))
    })
}

/// Decodes one base64 payload with apply-patch diagnostic context.
///
/// # Parameters
/// - `encoded`: The base64 text to decode.
/// - `label`: Human-readable label for error messages.
fn decode_base64_bytes(encoded: &str, label: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|error| {
            MezError::invalid_args(format!(
                "apply_patch: failed to decode {label} base64: {error}"
            ))
        })
}

/// Converts one raw snapshot into optional text-file state.
///
/// # Parameters
/// - `snapshot`: The remote snapshot to normalize.
pub(super) fn snapshot_text_state(
    snapshot: &ApplyPatchSnapshot,
) -> Result<Option<ApplyPatchTextFile>> {
    match &snapshot.state {
        ApplyPatchSnapshotState::Regular(bytes) => {
            Ok(Some(ApplyPatchTextFile::from_bytes(&snapshot.path, bytes)?))
        }
        ApplyPatchSnapshotState::Missing => Ok(None),
        ApplyPatchSnapshotState::NonRegular => Err(MezError::invalid_args(format!(
            "apply_patch: refusing to patch non-regular file: {}",
            snapshot.path
        ))),
        ApplyPatchSnapshotState::OutsideCwd => Err(MezError::invalid_args(format!(
            "apply_patch: resolved path is outside current working directory: {}",
            snapshot.path
        ))),
        ApplyPatchSnapshotState::Error => Err(MezError::invalid_args(format!(
            "apply_patch: failed to resolve path: {}",
            snapshot.path
        ))),
    }
}

/// Ensures the current file state is absent for add/move targets.
///
/// # Parameters
/// - `path`: The logical patch path for diagnostics.
/// - `state`: The current text state for the path.
pub(super) fn ensure_missing_state(
    path: &str,
    state: Option<&Option<ApplyPatchTextFile>>,
) -> Result<()> {
    match state {
        Some(None) => Ok(()),
        Some(Some(_)) => Err(MezError::invalid_args(format!(
            "apply_patch: refusing to add existing path: {path}"
        ))),
        None => Err(MezError::invalid_args(format!(
            "apply_patch: missing remote snapshot for path: {path}"
        ))),
    }
}

/// Ensures the current file state is regular text and returns it.
///
/// # Parameters
/// - `path`: The logical patch path for diagnostics.
/// - `state`: The current text state for the path.
pub(super) fn ensure_regular_state<'a>(
    path: &str,
    state: Option<&'a Option<ApplyPatchTextFile>>,
) -> Result<&'a ApplyPatchTextFile> {
    match state {
        Some(Some(file)) => Ok(file),
        Some(None) => Err(MezError::invalid_args(format!(
            "apply_patch: missing file: {path}"
        ))),
        None => Err(MezError::invalid_args(format!(
            "apply_patch: missing remote snapshot for path: {path}"
        ))),
    }
}
