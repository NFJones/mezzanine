//! Mezzanine patch planning, parsing, matching, and shell transaction generation.
//!
//! This module owns the deterministic patch pipeline behind the semantic
//! facade: validating model-authored Mezzanine patches, reading remote file
//! snapshots, matching hunks, producing diagnostics, and generating the shell
//! write transaction that applies verified bytes.

use super::LocalActionPlan;
use crate::agent::maap::{is_mez_patch_payload, validate_apply_patch_payload};
use crate::agent::shell::shell_quote;
use crate::error::{MezError, Result};
use base64::Engine;
use std::collections::{BTreeMap, BTreeSet};

mod parser;

use parser::{
    MezPatch, MezPatchHunk, MezPatchHunkLine, MezPatchOperation, MezPatchRangeHint, parse_mez_patch,
};

/// Timeout for patch application actions.
///
/// Patch actions should either apply quickly or fail with a diagnostic that the
/// model can repair. Keeping them below the turn-wide shell-action timeout avoids
/// making a malformed patch look like an indefinite stalled turn.
pub(in crate::agent) const APPLY_PATCH_TIMEOUT_MS: u64 = 30 * 1000;
/// Marker that identifies the shell-backed read phase for `apply_patch`.
pub(super) const APPLY_PATCH_READ_PHASE_MARKER: &str = "__MEZ_APPLY_PATCH_READ_PHASE__";
/// Marker that identifies the shell-backed write phase for `apply_patch`.
pub(super) const APPLY_PATCH_WRITE_PHASE_MARKER: &str = "__MEZ_APPLY_PATCH_WRITE_PHASE__";
/// Marker that starts one `apply_patch` remote snapshot stream.
const APPLY_PATCH_READ_BEGIN_MARKER: &str = "__MEZ_APPLY_PATCH_READ_BEGIN__";
/// Marker that ends one `apply_patch` remote snapshot stream.
const APPLY_PATCH_READ_END_MARKER: &str = "__MEZ_APPLY_PATCH_READ_END__";
/// Marker that starts one path entry in an `apply_patch` snapshot stream.
const APPLY_PATCH_FILE_BEGIN_MARKER: &str = "__MEZ_APPLY_PATCH_FILE_BEGIN__";
/// Marker that ends one path entry in an `apply_patch` snapshot stream.
const APPLY_PATCH_FILE_END_MARKER: &str = "__MEZ_APPLY_PATCH_FILE_END__";
/// Marker that starts base64 file content in an `apply_patch` snapshot stream.
const APPLY_PATCH_CONTENT_BEGIN_MARKER: &str = "__MEZ_APPLY_PATCH_CONTENT_BEGIN__";
/// Marker that ends base64 file content in an `apply_patch` snapshot stream.
const APPLY_PATCH_CONTENT_END_MARKER: &str = "__MEZ_APPLY_PATCH_CONTENT_END__";
/// Maximum base64 payload bytes emitted on one generated shell-source line.
///
/// File mutations cross the pane PTY as shell input. Keeping individual lines
/// well below common canonical-line limits prevents large content writes from
/// filling the line discipline before the newline is accepted.
const FILE_CONTENT_BASE64_SHELL_LINE_BYTES: usize = 768;

/// One shell-backed phase used to complete an `apply_patch` action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyPatchTransactionPhase {
    /// The action is reading remote file snapshots.
    Read,
    /// The action is verifying and writing patched bytes.
    Write,
}

fn shell_print_line(line: &str) -> String {
    format!("printf '%s\\n' {}", shell_quote(line))
}

fn unified_diff_lines(
    title: &str,
    old_label: &str,
    new_label: &str,
    old_path: &str,
    new_path: &str,
) -> Vec<String> {
    vec![
        shell_print_line(&format!("diff -- {title}")),
        format!(
            "diff -u --label {old_label} --label {new_label} -- {old_path} {new_path}",
            old_label = shell_quote(old_label),
            new_label = shell_quote(new_label)
        ),
        "MEZ_DIFF_STATUS=$?".to_string(),
        "case \"$MEZ_DIFF_STATUS\" in 0|1) :;; *) exit \"$MEZ_DIFF_STATUS\";; esac".to_string(),
    ]
}

/// Builds shell lines that write exact content bytes without embedding the raw
/// payload in the generated shell source.
fn write_content_lines(content: &str, target: &str, append: bool) -> Vec<String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let redirect = if append { ">>" } else { ">" };
    let mut lines = vec![
        "command -v base64 >/dev/null || { printf '%s\\n' 'base64 is required for semantic file content actions' >&2; exit 127; }"
            .to_string(),
        "MEZ_CONTENT_B64=$(mktemp) || exit 1".to_string(),
        "{".to_string(),
    ];
    if encoded.is_empty() {
        lines.push("  :".to_string());
    } else {
        for chunk in encoded
            .as_bytes()
            .chunks(FILE_CONTENT_BASE64_SHELL_LINE_BYTES)
        {
            let chunk = std::str::from_utf8(chunk)
                .expect("standard base64 output should always be valid UTF-8");
            lines.push(format!("  printf '%s' {}", shell_quote(chunk)));
        }
    }
    lines.extend([
        "} > \"$MEZ_CONTENT_B64\"".to_string(),
        format!(
            "if base64 -d < \"$MEZ_CONTENT_B64\" {redirect} {target} 2>/dev/null; then MEZ_CONTENT_STATUS=0; else base64 -D < \"$MEZ_CONTENT_B64\" {redirect} {target}; MEZ_CONTENT_STATUS=$?; fi"
        ),
        "rm -f -- \"$MEZ_CONTENT_B64\"".to_string(),
        "if [ \"$MEZ_CONTENT_STATUS\" != 0 ]; then exit \"$MEZ_CONTENT_STATUS\"; fi".to_string(),
    ]);
    lines
}

/// Returns the shell transaction phase represented by a generated apply-patch
/// command.
///
/// # Parameters
/// - `command`: The generated shell command being inspected.
pub fn apply_patch_transaction_phase(command: &str) -> Option<ApplyPatchTransactionPhase> {
    if command.contains(APPLY_PATCH_READ_PHASE_MARKER) {
        Some(ApplyPatchTransactionPhase::Read)
    } else if command.contains(APPLY_PATCH_WRITE_PHASE_MARKER) {
        Some(ApplyPatchTransactionPhase::Write)
    } else {
        None
    }
}

/// Builds the write phase for an `apply_patch` action from a remote snapshot.
///
/// # Parameters
/// - `patch`: The model-authored Mezzanine patch block.
/// - `read_output`: The decoded shell output from the read phase.
pub fn apply_patch_write_plan_from_read_output(
    patch: &str,
    read_output: &str,
) -> Result<LocalActionPlan> {
    let patch = parse_mez_patch(patch)?;
    let snapshots = parse_apply_patch_snapshot_output(read_output)?;
    let changes = apply_mez_patch_to_snapshots(&patch, &snapshots)?;
    mez_apply_patch_write_plan(changes)
}

/// Returns the sorted relative paths touched by one Mezzanine patch.
///
/// # Parameters
/// - `patch`: The model-authored Mezzanine patch block to inspect.
pub fn apply_patch_touched_paths(patch: &str) -> Result<Vec<String>> {
    Ok(parse_mez_patch(patch)?
        .touched_paths()
        .into_iter()
        .collect())
}

/// Builds a write-phase shell action that reports one deterministic
/// `apply_patch` error.
///
/// # Parameters
/// - `message`: The diagnostic to show to the model and user.
pub fn apply_patch_error_plan(message: &str) -> LocalActionPlan {
    let message = message.strip_prefix("apply_patch: ").unwrap_or(message);
    LocalActionPlan {
        summary: "I’ll apply a patch.".to_string(),
        command: format!(
            "# {APPLY_PATCH_WRITE_PHASE_MARKER}\nprintf '%s\\n' {} >&2\nexit 1",
            shell_quote(&format!("apply_patch: {message}"))
        ),
        policy_command: "apply_patch".to_string(),
        interactive: false,
        stateful: false,
        timeout_ms: Some(APPLY_PATCH_TIMEOUT_MS),
        display_output_after_completion: true,
    }
}

pub(super) fn apply_patch_plan(patch: &str, strip: Option<u64>) -> Result<LocalActionPlan> {
    validate_apply_patch_payload(patch)?;
    debug_assert!(is_mez_patch_payload(patch));
    mez_apply_patch_read_plan(patch, strip)
}

fn mez_apply_patch_read_plan(patch: &str, strip: Option<u64>) -> Result<LocalActionPlan> {
    if strip.is_some() {
        return Err(MezError::invalid_args(
            "apply_patch strip is unsupported for Mezzanine patch blocks",
        ));
    }
    let patch = parse_mez_patch(patch)?;
    let command = mez_apply_patch_read_command(&patch.touched_paths());
    Ok(LocalActionPlan {
        summary: "I’ll apply a patch.".to_string(),
        command,
        policy_command: "apply_patch".to_string(),
        interactive: false,
        stateful: false,
        timeout_ms: Some(APPLY_PATCH_TIMEOUT_MS),
        display_output_after_completion: true,
    })
}

fn mez_apply_patch_write_plan(changes: Vec<ApplyPatchFileChange>) -> Result<LocalActionPlan> {
    let mut command = String::from("# ");
    command.push_str(APPLY_PATCH_WRITE_PHASE_MARKER);
    command.push('\n');
    command.push_str(&apply_patch_write_command_prelude());
    for (index, change) in changes.iter().enumerate() {
        command.push_str(&apply_patch_write_change_command(index, change));
    }
    Ok(LocalActionPlan {
        summary: "I’ll apply a patch.".to_string(),
        command,
        policy_command: "apply_patch".to_string(),
        interactive: false,
        stateful: false,
        timeout_ms: Some(APPLY_PATCH_TIMEOUT_MS),
        display_output_after_completion: true,
    })
}

fn mez_apply_patch_read_command(paths: &BTreeSet<String>) -> String {
    let mut lines = vec![
        format!("# {APPLY_PATCH_READ_PHASE_MARKER}"),
        "command -v base64 >/dev/null || { printf '%s\\n' 'apply_patch: base64 is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v realpath >/dev/null || { printf '%s\\n' 'apply_patch: coreutils realpath is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v tr >/dev/null || { printf '%s\\n' 'apply_patch: tr is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "if ! realpath -m -- . >/dev/null 2>&1; then printf '%s\\n' 'apply_patch: coreutils realpath -m is required for apply_patch actions' >&2; exit 127; fi".to_string(),
        "MEZ_APPLY_CWD=$(pwd -P) || exit 1".to_string(),
        "MEZ_APPLY_CWD_PREFIX=${MEZ_APPLY_CWD%/}".to_string(),
        "if [ -z \"$MEZ_APPLY_CWD_PREFIX\" ]; then MEZ_APPLY_CWD_PREFIX=/; fi".to_string(),
        "mez_apply_patch_b64() { printf '%s' \"$1\" | base64 | tr -d '\\n'; }".to_string(),
        "mez_apply_patch_emit_path() {".to_string(),
        "MEZ_APPLY_PATH=$1".to_string(),
        "MEZ_APPLY_RESOLVED=$(realpath -m -- \"$MEZ_APPLY_PATH\" 2>/dev/null) || MEZ_APPLY_RESOLVED=".to_string(),
        "MEZ_APPLY_STATUS=error".to_string(),
        "if [ -n \"$MEZ_APPLY_RESOLVED\" ]; then".to_string(),
        "  case \"$MEZ_APPLY_RESOLVED\" in \"$MEZ_APPLY_CWD\"|\"$MEZ_APPLY_CWD_PREFIX\"/*)".to_string(),
        "    if [ -e \"$MEZ_APPLY_PATH\" ] || [ -L \"$MEZ_APPLY_PATH\" ]; then".to_string(),
        "      if [ -f \"$MEZ_APPLY_RESOLVED\" ]; then MEZ_APPLY_STATUS=regular; else MEZ_APPLY_STATUS=non_regular; fi".to_string(),
        "    else".to_string(),
        "      MEZ_APPLY_STATUS=missing".to_string(),
        "    fi".to_string(),
        "    ;;".to_string(),
        "  *) MEZ_APPLY_STATUS=outside_cwd ;;".to_string(),
        "  esac".to_string(),
        "fi".to_string(),
        format!("printf '%s\\n' {}", shell_quote(APPLY_PATCH_FILE_BEGIN_MARKER)),
        "printf 'PATH_B64 %s\\n' \"$(mez_apply_patch_b64 \"$MEZ_APPLY_PATH\")\"".to_string(),
        "printf 'RESOLVED_B64 %s\\n' \"$(mez_apply_patch_b64 \"$MEZ_APPLY_RESOLVED\")\"".to_string(),
        "printf 'STATUS %s\\n' \"$MEZ_APPLY_STATUS\"".to_string(),
        "if [ \"$MEZ_APPLY_STATUS\" = regular ]; then".to_string(),
        format!("  printf '%s\\n' {}", shell_quote(APPLY_PATCH_CONTENT_BEGIN_MARKER)),
        "  base64 < \"$MEZ_APPLY_RESOLVED\"".to_string(),
        format!("  printf '%s\\n' {}", shell_quote(APPLY_PATCH_CONTENT_END_MARKER)),
        "fi".to_string(),
        format!("printf '%s\\n' {}", shell_quote(APPLY_PATCH_FILE_END_MARKER)),
        "}".to_string(),
        format!("printf '%s\\n' {}", shell_quote(APPLY_PATCH_READ_BEGIN_MARKER)),
    ];
    for path in paths {
        lines.push(format!("mez_apply_patch_emit_path {}", shell_quote(path)));
    }
    lines.extend([
        format!("printf '%s\\n' {}", shell_quote(APPLY_PATCH_READ_END_MARKER)),
        "unset -f mez_apply_patch_emit_path mez_apply_patch_b64 2>/dev/null || :".to_string(),
        "unset MEZ_APPLY_CWD MEZ_APPLY_CWD_PREFIX MEZ_APPLY_PATH MEZ_APPLY_RESOLVED MEZ_APPLY_STATUS".to_string(),
    ]);
    lines.join("\n")
}

fn apply_patch_write_command_prelude() -> String {
    [
        "command -v base64 >/dev/null || { printf '%s\\n' 'apply_patch: base64 is required for apply_patch actions' >&2; exit 127; }",
        "command -v realpath >/dev/null || { printf '%s\\n' 'apply_patch: coreutils realpath is required for apply_patch actions' >&2; exit 127; }",
        "command -v cmp >/dev/null || { printf '%s\\n' 'apply_patch: cmp is required for apply_patch actions' >&2; exit 127; }",
        "command -v dirname >/dev/null || { printf '%s\\n' 'apply_patch: dirname is required for apply_patch actions' >&2; exit 127; }",
        "if ! realpath -m -- . >/dev/null 2>&1; then printf '%s\\n' 'apply_patch: coreutils realpath -m is required for apply_patch actions' >&2; exit 127; fi",
        "MEZ_APPLY_CWD=$(pwd -P) || exit 1",
        "MEZ_APPLY_CWD_PREFIX=${MEZ_APPLY_CWD%/}",
        "if [ -z \"$MEZ_APPLY_CWD_PREFIX\" ]; then MEZ_APPLY_CWD_PREFIX=/; fi",
        "mez_apply_patch_resolve_checked() {",
        "MEZ_APPLY_PATH=$1",
        "MEZ_APPLY_EXPECTED_RESOLVED=$2",
        "MEZ_APPLY_RESOLVED=$(realpath -m -- \"$MEZ_APPLY_PATH\" 2>/dev/null) || { printf '%s\\n' \"apply_patch: failed to resolve path: $MEZ_APPLY_PATH\" >&2; exit 1; }",
        "case \"$MEZ_APPLY_RESOLVED\" in \"$MEZ_APPLY_CWD\"|\"$MEZ_APPLY_CWD_PREFIX\"/*) ;; *) printf '%s\\n' \"apply_patch: resolved path is outside current working directory: $MEZ_APPLY_PATH\" >&2; exit 1;; esac",
        "if [ \"$MEZ_APPLY_RESOLVED\" != \"$MEZ_APPLY_EXPECTED_RESOLVED\" ]; then printf '%s\\n' \"apply_patch: resolved path changed before apply: $MEZ_APPLY_PATH\" >&2; exit 1; fi",
        "}",
        "",
    ]
    .join("\n")
}

fn apply_patch_write_change_command(index: usize, change: &ApplyPatchFileChange) -> String {
    let expected_var = format!("MEZ_APPLY_EXPECTED_{index}");
    let new_var = format!("MEZ_APPLY_NEW_{index}");
    let original_is_regular = matches!(&change.original, ApplyPatchOriginalState::Regular(_));
    let mut lines = vec![format!(
        "mez_apply_patch_resolve_checked {} {}",
        shell_quote(&change.path),
        shell_quote(&change.resolved_path)
    )];
    match &change.original {
        ApplyPatchOriginalState::Regular(bytes) => {
            lines.push(format!("{expected_var}=$(mktemp) || exit 1"));
            lines.extend(write_content_lines(
                &String::from_utf8_lossy(bytes),
                &format!("\"${expected_var}\""),
                false,
            ));
            lines.push(format!(
                "if [ ! -f \"$MEZ_APPLY_RESOLVED\" ]; then printf '%s\\n' {} >&2; rm -f -- \"${expected_var}\"; exit 1; fi",
                shell_quote(&format!(
                    "apply_patch: refusing to patch non-regular file: {}",
                    change.path
                ))
            ));
            lines.push(format!(
                "if ! cmp -s -- \"${expected_var}\" \"$MEZ_APPLY_RESOLVED\"; then printf '%s\\n' {} >&2; rm -f -- \"${expected_var}\"; exit 1; fi",
                shell_quote(&format!("apply_patch: file changed before apply: {}", change.path))
            ));
        }
        ApplyPatchOriginalState::Missing => {
            lines.push(format!(
                "if [ -e {} ] || [ -L {} ] || [ -e \"$MEZ_APPLY_RESOLVED\" ] || [ -L \"$MEZ_APPLY_RESOLVED\" ]; then printf '%s\\n' {} >&2; exit 1; fi",
                shell_quote(&change.path),
                shell_quote(&change.path),
                shell_quote(&format!("apply_patch: refusing to add existing path: {}", change.path))
            ));
        }
    }
    if let Some(bytes) = &change.final_bytes {
        lines.push(format!("{new_var}=$(mktemp) || exit 1"));
        lines.extend(write_content_lines(
            &String::from_utf8_lossy(bytes),
            &format!("\"${new_var}\""),
            false,
        ));
        let old_label = if original_is_regular {
            format!("a/{}", change.path)
        } else {
            "/dev/null".to_string()
        };
        let old_path = if original_is_regular {
            format!("\"${expected_var}\"")
        } else {
            shell_quote("/dev/null")
        };
        lines.extend(unified_diff_lines(
            "apply patch",
            &old_label,
            &format!("b/{}", change.path),
            &old_path,
            &format!("\"${new_var}\""),
        ));
        lines.push("mkdir -p -- \"$(dirname -- \"$MEZ_APPLY_RESOLVED\")\"".to_string());
        lines.push(format!("mv -f -- \"${new_var}\" \"$MEZ_APPLY_RESOLVED\""));
    } else {
        lines.extend(unified_diff_lines(
            "apply patch",
            &format!("a/{}", change.path),
            "/dev/null",
            &format!("\"${expected_var}\""),
            &shell_quote("/dev/null"),
        ));
        lines.push("rm -f -- \"$MEZ_APPLY_RESOLVED\"".to_string());
    }
    if original_is_regular {
        lines.push(format!("rm -f -- \"${expected_var}\""));
    }
    lines.join("\n") + "\n"
}

impl MezPatchHunk {
    fn replacement_lines(
        &self,
        file: &ApplyPatchTextFile,
        hunk_match: &ApplyPatchHunkMatch,
    ) -> Result<Vec<String>> {
        let mut old_index = 0usize;
        let mut next_source_offset = 0usize;
        let mut lines = Vec::new();
        let gap_policies = self.old_line_gap_policies();
        for line in &self.lines {
            match line {
                MezPatchHunkLine::Context(_) => {
                    let gap_policy = *gap_policies
                        .get(old_index)
                        .unwrap_or(&ApplyPatchBlankGapPolicy::Disallow);
                    let offset = *hunk_match.old_line_offsets.get(old_index).ok_or_else(|| {
                        MezError::invalid_args(
                            "apply_patch: internal hunk replacement range was invalid",
                        )
                    })?;
                    append_skipped_blank_context_lines(
                        &mut lines,
                        file,
                        hunk_match.position,
                        next_source_offset,
                        offset,
                        gap_policy,
                    )?;
                    let source = file
                        .lines
                        .get(hunk_match.position + offset)
                        .ok_or_else(|| {
                            MezError::invalid_args(
                                "apply_patch: internal hunk replacement range was invalid",
                            )
                        })?;
                    lines.push(source.clone());
                    old_index += 1;
                    next_source_offset = offset.saturating_add(1);
                }
                MezPatchHunkLine::Remove(_) => {
                    let gap_policy = *gap_policies
                        .get(old_index)
                        .unwrap_or(&ApplyPatchBlankGapPolicy::Disallow);
                    let offset = *hunk_match.old_line_offsets.get(old_index).ok_or_else(|| {
                        MezError::invalid_args(
                            "apply_patch: internal hunk replacement range was invalid",
                        )
                    })?;
                    append_skipped_blank_context_lines(
                        &mut lines,
                        file,
                        hunk_match.position,
                        next_source_offset,
                        offset,
                        gap_policy,
                    )?;
                    next_source_offset = offset.saturating_add(1);
                    old_index += 1;
                }
                MezPatchHunkLine::Add(text) => lines.push(text.clone()),
            }
        }
        Ok(lines)
    }

    fn old_line_gap_policies(&self) -> Vec<ApplyPatchBlankGapPolicy> {
        let mut policies = Vec::with_capacity(self.old.len());
        let mut previous_old_kind = None;
        for line in &self.lines {
            match line {
                MezPatchHunkLine::Context(_) => {
                    let policy = match previous_old_kind {
                        Some(MezPatchOldLineKind::Context | MezPatchOldLineKind::Remove) => {
                            ApplyPatchBlankGapPolicy::Preserve
                        }
                        _ => ApplyPatchBlankGapPolicy::Disallow,
                    };
                    policies.push(policy);
                    previous_old_kind = Some(MezPatchOldLineKind::Context);
                }
                MezPatchHunkLine::Remove(_) => {
                    let policy = match previous_old_kind {
                        Some(MezPatchOldLineKind::Context | MezPatchOldLineKind::Remove) => {
                            ApplyPatchBlankGapPolicy::Delete
                        }
                        _ => ApplyPatchBlankGapPolicy::Disallow,
                    };
                    policies.push(policy);
                    previous_old_kind = Some(MezPatchOldLineKind::Remove);
                }
                MezPatchHunkLine::Add(_) => {}
            }
        }
        policies
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MezPatchOldLineKind {
    Context,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPatchBlankGapPolicy {
    Disallow,
    Preserve,
    Delete,
}

fn append_skipped_blank_context_lines(
    output: &mut Vec<String>,
    file: &ApplyPatchTextFile,
    position: usize,
    start_offset: usize,
    end_offset: usize,
    policy: ApplyPatchBlankGapPolicy,
) -> Result<()> {
    if end_offset <= start_offset {
        return Ok(());
    }
    if policy == ApplyPatchBlankGapPolicy::Disallow {
        return Err(MezError::invalid_args(
            "apply_patch: internal hunk replacement range was invalid",
        ));
    }
    for offset in start_offset..end_offset {
        let source = file.lines.get(position + offset).ok_or_else(|| {
            MezError::invalid_args("apply_patch: internal hunk replacement range was invalid")
        })?;
        if !source.trim().is_empty() {
            return Err(MezError::invalid_args(
                "apply_patch: internal hunk replacement range was invalid",
            ));
        }
        if policy == ApplyPatchBlankGapPolicy::Preserve {
            output.push(source.clone());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchSnapshot {
    path: String,
    resolved_path: String,
    state: ApplyPatchSnapshotState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ApplyPatchSnapshotState {
    Regular(Vec<u8>),
    Missing,
    NonRegular,
    OutsideCwd,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchFileChange {
    path: String,
    resolved_path: String,
    original: ApplyPatchOriginalState,
    final_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ApplyPatchOriginalState {
    Regular(Vec<u8>),
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchTextFile {
    lines: Vec<String>,
    trailing_newline: bool,
}

impl ApplyPatchTextFile {
    fn from_bytes(path: &str, bytes: &[u8]) -> Result<Self> {
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| {
            MezError::invalid_args(format!("apply_patch: file is not valid UTF-8: {path}"))
        })?;
        Ok(Self {
            lines: text.lines().map(ToString::to_string).collect(),
            trailing_newline: text.ends_with('\n'),
        })
    }

    fn from_lines(lines: Vec<String>, trailing_newline: bool) -> Self {
        Self {
            lines,
            trailing_newline,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        let mut text = self.lines.join("\n");
        if !self.lines.is_empty() && self.trailing_newline {
            text.push('\n');
        }
        text.into_bytes()
    }
}

fn apply_patch_parse_error<T>(message: &str) -> Result<T> {
    Err(MezError::invalid_args(format!("apply_patch: {message}")))
}

fn parse_apply_patch_snapshot_output(output: &str) -> Result<BTreeMap<String, ApplyPatchSnapshot>> {
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

fn decode_base64_bytes(encoded: &str, label: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|error| {
            MezError::invalid_args(format!(
                "apply_patch: failed to decode {label} base64: {error}"
            ))
        })
}

fn apply_mez_patch_to_snapshots(
    patch: &MezPatch,
    snapshots: &BTreeMap<String, ApplyPatchSnapshot>,
) -> Result<Vec<ApplyPatchFileChange>> {
    let mut current = BTreeMap::new();
    let mut original = BTreeMap::new();
    for (path, snapshot) in snapshots {
        let state = snapshot_text_state(snapshot)?;
        original.insert(path.clone(), state.clone());
        current.insert(path.clone(), state);
    }
    for operation in &patch.operations {
        match operation {
            MezPatchOperation::Add { path, content } => {
                ensure_missing_state(path, current.get(path))?;
                current.insert(
                    path.clone(),
                    Some(ApplyPatchTextFile::from_lines(content.clone(), true)),
                );
            }
            MezPatchOperation::Delete { path } => {
                ensure_regular_state(path, current.get(path))?;
                current.insert(path.clone(), None);
            }
            MezPatchOperation::Update {
                path,
                move_to,
                hunks,
                trailing_newline,
            } => {
                let mut file = ensure_regular_state(path, current.get(path))?.clone();
                if let Some(value) = trailing_newline {
                    file.trailing_newline = *value;
                }
                let mut cursor = 0usize;
                for hunk in hunks {
                    let hunk_match =
                        find_hunk_position(&file, hunk, cursor).map_err(|problem| {
                            apply_patch_hunk_mismatch_error(path, &file, hunk, problem)
                        })?;
                    let replacement = hunk.replacement_lines(&file, &hunk_match)?;
                    let replacement_len = replacement.len();
                    file.lines.splice(
                        hunk_match.position..hunk_match.position + hunk_match.span_len(),
                        replacement,
                    );
                    cursor = hunk_match.position + replacement_len;
                }
                if let Some(target) = move_to {
                    ensure_missing_state(target, current.get(target))?;
                    current.insert(path.clone(), None);
                    current.insert(target.clone(), Some(file));
                } else {
                    current.insert(path.clone(), Some(file));
                }
            }
        }
    }
    let mut changes = Vec::new();
    for path in patch.touched_paths() {
        let snapshot = snapshots.get(&path).ok_or_else(|| {
            MezError::invalid_args(format!(
                "apply_patch: missing remote snapshot for path: {path}"
            ))
        })?;
        let original_state = match original.get(&path).cloned().flatten() {
            Some(file) => ApplyPatchOriginalState::Regular(file.into_bytes()),
            None => ApplyPatchOriginalState::Missing,
        };
        let final_bytes = current
            .get(&path)
            .cloned()
            .flatten()
            .map(|file| file.into_bytes());
        let original_empty = matches!(original_state, ApplyPatchOriginalState::Missing);
        if original_empty && final_bytes.is_none() {
            continue;
        }
        changes.push(ApplyPatchFileChange {
            path,
            resolved_path: snapshot.resolved_path.clone(),
            original: original_state,
            final_bytes,
        });
    }
    Ok(changes)
}

fn apply_patch_hunk_mismatch_error(
    path: &str,
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    problem: ApplyPatchHunkMatchProblem,
) -> MezError {
    let (
        failure_code,
        reason,
        candidate_spans,
        context_center,
        missing_anchor,
        mode,
        attempts,
        scope,
        range_rejection,
    ) = match problem {
        ApplyPatchHunkMatchProblem::Missing {
            context_center,
            missing_anchor,
            attempts,
            scope,
            range_rejection,
        } => (
            "HUNK_CONTEXT_MISMATCH",
            "hunk context was not found in the current file",
            Vec::new(),
            context_center,
            missing_anchor,
            None,
            attempts,
            scope,
            range_rejection,
        ),
        ApplyPatchHunkMatchProblem::Ambiguous {
            candidate_spans,
            mode,
            attempts,
            scope,
            range_rejection,
        } => {
            let reason = match mode {
                Some(ApplyPatchMatchMode::Exact) | None => {
                    "exact hunk context is ambiguous in the current file"
                }
                Some(ApplyPatchMatchMode::TrimEnd) => {
                    "trim_end hunk context is ambiguous in the current file"
                }
                Some(ApplyPatchMatchMode::Trim) => {
                    "trim hunk context is ambiguous in the current file"
                }
                Some(ApplyPatchMatchMode::Normalized) => {
                    "normalized hunk context is ambiguous in the current file"
                }
            };
            (
                "HUNK_CONTEXT_AMBIGUOUS",
                reason,
                candidate_spans,
                None,
                None,
                mode,
                attempts,
                scope,
                range_rejection,
            )
        }
    };
    let candidate_lines = candidate_spans
        .iter()
        .map(|span| span.start_line)
        .collect::<Vec<_>>();
    let mut message = format!(
        "apply_patch: hunk did not match: {path}\n\
         apply_patch: {reason}\n\
         apply_patch: failure_code={failure_code}\n\
         apply_patch: affected_path={path}\n\
         apply_patch: failed old-context line count: {}",
        hunk.old.len()
    );
    if !attempts.is_empty() {
        message.push_str(&format!(
            "\napply_patch: matching_attempts={}",
            apply_patch_match_attempts_summary(&attempts)
        ));
    }
    if let Some(mode) = mode {
        message.push_str(&format!(
            "\napply_patch: ambiguous_matching_mode={}",
            mode.as_str()
        ));
    }
    message.push_str(&format!("\napply_patch: matching_scope={}", scope.as_str()));
    if !hunk.anchors.is_empty() {
        message.push_str(&format!(
            "\napply_patch: hunk header anchor(s): {}",
            hunk.anchors.join(" -> ")
        ));
    }
    if let Some(range_hint) = hunk.range_hint {
        message.push_str(&format!(
            "\napply_patch: hunk header old-line hint: {}",
            range_hint.old_start
        ));
    }
    if let Some(anchor) = missing_anchor {
        message.push_str(&format!(
            "\napply_patch: hunk header anchor was not found in order: {}",
            apply_patch_mismatch_excerpt(&anchor)
        ));
    }
    if !candidate_lines.is_empty() {
        message.push_str(&format!(
            "\napply_patch: candidate match line(s): {}",
            candidate_lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !candidate_spans.is_empty() {
        message.push_str(&format!(
            "\napply_patch: candidate match span(s): {}",
            candidate_spans
                .iter()
                .map(|span| {
                    if span.start_line == span.end_line {
                        span.start_line.to_string()
                    } else {
                        format!("{}-{}", span.start_line, span.end_line)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(rejection) = range_rejection {
        message.push_str(&format!(
            "\napply_patch: range_hint_disambiguation=rejected reason={} hint_line={}",
            rejection.reason.as_str(),
            rejection.hint_line
        ));
        if let Some(distance) = rejection.nearest_distance {
            message.push_str(&format!(" nearest_distance={distance}"));
        }
        if let Some(distance) = rejection.next_distance {
            message.push_str(&format!(" next_distance={distance}"));
        }
    }
    if let Some(hint) = apply_patch_replacement_presence_hint(file, hunk, scope) {
        message.push_str(&format!(
            "\napply_patch: replacement_hint={} span(s): {}",
            hint.kind.as_str(),
            hint.spans
                .iter()
                .map(|span| {
                    if span.start_line == span.end_line {
                        span.start_line.to_string()
                    } else {
                        format!("{}-{}", span.start_line, span.end_line)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
        message.push_str(
            "\napply_patch: replacement_hint_next_step=reconcile_current_file_before_retry",
        );
    }
    if let Some(first_line) = hunk.old.first() {
        let anchor_lines = apply_patch_anchor_line_numbers(&file.lines, first_line);
        if anchor_lines.is_empty() {
            message.push_str("\napply_patch: first old-context line was not found anywhere");
        } else {
            message.push_str(&format!(
                "\napply_patch: first old-context line appears at current line(s): {}",
                anchor_lines
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let context_center = context_center
        .or_else(|| {
            candidate_lines
                .first()
                .and_then(|line| line.checked_sub(1))
                .or_else(|| {
                    hunk.old
                        .first()
                        .and_then(|line| {
                            apply_patch_anchor_line_numbers(&file.lines, line)
                                .first()
                                .copied()
                        })
                        .and_then(|line| line.checked_sub(1))
                })
        })
        .or_else(|| (!file.lines.is_empty()).then_some(0));
    if let Some(center) = context_center
        && let Some((start, end)) = apply_patch_current_context_range(file, center)
    {
        message.push_str(
            "\napply_patch: suggested_next_step=reread_region\
             \napply_patch: retry_without_reread=false",
        );
        message.push_str(&format!(
            "\napply_patch: suggested_read_range={path}:{start}-{end}"
        ));
    } else {
        message.push_str(
            "\napply_patch: suggested_next_step=reread_target_file\
             \napply_patch: retry_without_reread=false",
        );
    }
    if let Some(center) = context_center {
        message.push_str(&apply_patch_current_context_message(file, center));
    }
    message.push_str("\napply_patch: failed old context follows:");
    for line in hunk.old.iter().take(APPLY_PATCH_MISMATCH_CONTEXT_LINES) {
        message.push_str("\napply_patch:   ");
        message.push_str(&apply_patch_mismatch_excerpt(line));
    }
    if hunk.old.len() > APPLY_PATCH_MISMATCH_CONTEXT_LINES {
        message.push_str(&format!(
            "\napply_patch:   ... ({} more old-context lines omitted)",
            hunk.old.len() - APPLY_PATCH_MISMATCH_CONTEXT_LINES
        ));
    }
    message.push_str(&format!(
        "\napply_patch: next step: read {path} around the reported line(s), then retry with a smaller fresh Mezzanine patch using a distinctive @@ header anchor"
    ));
    message.push_str(
        "\napply_patch: do not retry substantially the same patch without fresh target context",
    );
    MezError::invalid_args(message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPatchReplacementPresenceKind {
    FullReplacementBlockPresent,
    DistinctiveAddedLinesPresent,
}

impl ApplyPatchReplacementPresenceKind {
    fn as_str(self) -> &'static str {
        match self {
            ApplyPatchReplacementPresenceKind::FullReplacementBlockPresent => {
                "full_replacement_block_present"
            }
            ApplyPatchReplacementPresenceKind::DistinctiveAddedLinesPresent => {
                "distinctive_added_lines_present"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchReplacementPresenceHint {
    kind: ApplyPatchReplacementPresenceKind,
    spans: Vec<ApplyPatchCandidateSpan>,
}

fn apply_patch_replacement_presence_hint(
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    scope: ApplyPatchSearchScope,
) -> Option<ApplyPatchReplacementPresenceHint> {
    let ranges = apply_patch_replacement_search_ranges(file, hunk, scope);
    if !hunk.new.is_empty() && hunk.new != hunk.old {
        let spans = apply_patch_exact_sequence_spans(&file.lines, &hunk.new, &ranges);
        if !spans.is_empty() {
            return Some(ApplyPatchReplacementPresenceHint {
                kind: ApplyPatchReplacementPresenceKind::FullReplacementBlockPresent,
                spans,
            });
        }
    }

    let distinctive_added_lines = hunk
        .lines
        .iter()
        .filter_map(|line| match line {
            MezPatchHunkLine::Add(text) => Some(text),
            MezPatchHunkLine::Context(_) | MezPatchHunkLine::Remove(_) => None,
        })
        .filter(|line| apply_patch_added_line_is_distinctive(line, &hunk.old))
        .fold(Vec::<&String>::new(), |mut lines, line| {
            if !lines.contains(&line) {
                lines.push(line);
            }
            lines
        });
    if distinctive_added_lines.is_empty() {
        return None;
    }

    let mut spans = Vec::new();
    for line in distinctive_added_lines {
        let line_spans =
            apply_patch_exact_sequence_spans(&file.lines, std::slice::from_ref(line), &ranges);
        if line_spans.is_empty() {
            return None;
        }
        spans.extend(line_spans);
        if spans.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
            spans.truncate(APPLY_PATCH_MATCH_CANDIDATE_LIMIT);
            break;
        }
    }
    Some(ApplyPatchReplacementPresenceHint {
        kind: ApplyPatchReplacementPresenceKind::DistinctiveAddedLinesPresent,
        spans,
    })
}

fn apply_patch_replacement_search_ranges(
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    scope: ApplyPatchSearchScope,
) -> Vec<(usize, usize)> {
    if !hunk.anchors.is_empty() {
        let chains = ordered_anchor_chains(&file.lines, &hunk.anchors, 0);
        if !chains.is_empty() {
            if scope == ApplyPatchSearchScope::StructuralAnchorScope {
                let structural_ranges = structural_anchor_ranges(&file.lines, &chains);
                if !structural_ranges.is_empty() {
                    return structural_ranges;
                }
            }
            let ordered_ranges = ordered_anchor_search_ranges(&file.lines, &chains, 0);
            if !ordered_ranges.is_empty() {
                return ordered_ranges;
            }
        }
    }
    vec![(0, file.lines.len())]
}

fn apply_patch_exact_sequence_spans(
    lines: &[String],
    needle: &[String],
    ranges: &[(usize, usize)],
) -> Vec<ApplyPatchCandidateSpan> {
    let mut spans = Vec::new();
    for (start, end) in ranges {
        let matches =
            find_line_sequence_matches(lines, needle, *start, *end, ApplyPatchMatchMode::Exact);
        spans.extend(matches.lines.into_iter().map(|position| {
            ApplyPatchCandidateSpan::from_position_and_len(position, needle.len())
        }));
        if spans.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
            spans.truncate(APPLY_PATCH_MATCH_CANDIDATE_LIMIT);
            break;
        }
    }
    spans
}

fn apply_patch_added_line_is_distinctive(line: &str, old: &[String]) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && !matches!(trimmed, "{" | "}" | ");" | "," | ".")
        && trimmed
            .chars()
            .any(|character| character.is_ascii_alphanumeric() || character == '_')
        && !old.iter().any(|old_line| old_line == line)
}

fn apply_patch_match_attempts_summary(attempts: &[ApplyPatchMatchAttempt]) -> String {
    attempts
        .iter()
        .map(|attempt| {
            let count = if attempt.capped {
                format!(">={}", attempt.candidate_count)
            } else {
                attempt.candidate_count.to_string()
            };
            format!("{}:{count}", attempt.mode.as_str())
        })
        .collect::<Vec<_>>()
        .join(",")
}

const APPLY_PATCH_MISMATCH_CONTEXT_LINES: usize = 8;
const APPLY_PATCH_MISMATCH_LINE_CHARS: usize = 160;
const APPLY_PATCH_MISMATCH_ANCHOR_LIMIT: usize = 5;
const APPLY_PATCH_MATCH_CANDIDATE_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPatchMatchMode {
    Exact,
    TrimEnd,
    Trim,
    Normalized,
}

impl ApplyPatchMatchMode {
    fn as_str(self) -> &'static str {
        match self {
            ApplyPatchMatchMode::Exact => "exact",
            ApplyPatchMatchMode::TrimEnd => "trim_end",
            ApplyPatchMatchMode::Trim => "trim",
            ApplyPatchMatchMode::Normalized => "normalized",
        }
    }
}

const APPLY_PATCH_MATCH_MODES: &[ApplyPatchMatchMode] = &[
    ApplyPatchMatchMode::Exact,
    ApplyPatchMatchMode::TrimEnd,
    ApplyPatchMatchMode::Trim,
    ApplyPatchMatchMode::Normalized,
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchHunkMatch {
    position: usize,
    mode: ApplyPatchMatchMode,
    old_line_offsets: Vec<usize>,
}

impl ApplyPatchHunkMatch {
    fn span_len(&self) -> usize {
        self.old_line_offsets
            .last()
            .map(|offset| offset.saturating_add(1))
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchLineSequenceMatch {
    position: usize,
    old_line_offsets: Vec<usize>,
}

impl ApplyPatchLineSequenceMatch {
    fn exact(position: usize, old_line_count: usize) -> Self {
        Self {
            position,
            old_line_offsets: (0..old_line_count).collect(),
        }
    }

    fn into_hunk_match(self, mode: ApplyPatchMatchMode) -> ApplyPatchHunkMatch {
        ApplyPatchHunkMatch {
            position: self.position,
            mode,
            old_line_offsets: self.old_line_offsets,
        }
    }

    fn span_len(&self) -> usize {
        self.old_line_offsets
            .last()
            .map(|offset| offset.saturating_add(1))
            .unwrap_or(0)
    }

    fn span(&self) -> ApplyPatchCandidateSpan {
        ApplyPatchCandidateSpan::from_position_and_len(self.position, self.span_len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchMatchAttempt {
    mode: ApplyPatchMatchMode,
    candidate_count: usize,
    capped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchCandidateMatches {
    lines: Vec<usize>,
    capped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchLineSequenceMatches {
    lines: Vec<ApplyPatchLineSequenceMatch>,
    capped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPatchSearchScope {
    FullFile,
    OrderedAnchorRange,
    StructuralAnchorScope,
}

impl ApplyPatchSearchScope {
    fn as_str(self) -> &'static str {
        match self {
            ApplyPatchSearchScope::FullFile => "full_file",
            ApplyPatchSearchScope::OrderedAnchorRange => "ordered_anchor_range",
            ApplyPatchSearchScope::StructuralAnchorScope => "structural_anchor_scope",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApplyPatchCandidateSpan {
    start_line: usize,
    end_line: usize,
}

impl ApplyPatchCandidateSpan {
    fn from_position_and_len(position: usize, len: usize) -> Self {
        let start_line = position + 1;
        let end_line = position + len.max(1);
        Self {
            start_line,
            end_line,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPatchRangeHintRejectionReason {
    CandidateListCapped,
    Tie,
    NearTie,
    Distant,
}

impl ApplyPatchRangeHintRejectionReason {
    fn as_str(self) -> &'static str {
        match self {
            ApplyPatchRangeHintRejectionReason::CandidateListCapped => "candidate_list_capped",
            ApplyPatchRangeHintRejectionReason::Tie => "tie",
            ApplyPatchRangeHintRejectionReason::NearTie => "near_tie",
            ApplyPatchRangeHintRejectionReason::Distant => "distant",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApplyPatchRangeHintRejection {
    hint_line: usize,
    nearest_distance: Option<usize>,
    next_distance: Option<usize>,
    reason: ApplyPatchRangeHintRejectionReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ApplyPatchHunkMatchProblem {
    Missing {
        context_center: Option<usize>,
        missing_anchor: Option<String>,
        attempts: Vec<ApplyPatchMatchAttempt>,
        scope: ApplyPatchSearchScope,
        range_rejection: Option<ApplyPatchRangeHintRejection>,
    },
    Ambiguous {
        candidate_spans: Vec<ApplyPatchCandidateSpan>,
        mode: Option<ApplyPatchMatchMode>,
        attempts: Vec<ApplyPatchMatchAttempt>,
        scope: ApplyPatchSearchScope,
        range_rejection: Option<ApplyPatchRangeHintRejection>,
    },
}

fn find_hunk_position(
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    cursor: usize,
) -> std::result::Result<ApplyPatchHunkMatch, ApplyPatchHunkMatchProblem> {
    if hunk.anchors.is_empty() {
        return find_unanchored_hunk_position(
            &file.lines,
            &hunk.old,
            &hunk.old_line_gap_policies(),
            cursor,
            hunk.range_hint,
        );
    }

    let chains = ordered_anchor_chains(&file.lines, &hunk.anchors, cursor);
    if chains.is_empty() {
        return Err(ApplyPatchHunkMatchProblem::Missing {
            context_center: None,
            missing_anchor: first_missing_ordered_anchor(&file.lines, &hunk.anchors, cursor),
            attempts: Vec::new(),
            scope: ApplyPatchSearchScope::OrderedAnchorRange,
            range_rejection: None,
        });
    }

    if hunk.old.is_empty() {
        if chains.len() == 1 {
            return Ok(ApplyPatchHunkMatch {
                position: chains[0]
                    .last()
                    .copied()
                    .map(|line| (line + 1).min(file.lines.len()))
                    .unwrap_or_else(|| cursor.min(file.lines.len())),
                mode: ApplyPatchMatchMode::Exact,
                old_line_offsets: Vec::new(),
            });
        }
        return Err(ApplyPatchHunkMatchProblem::Ambiguous {
            candidate_spans: chains
                .iter()
                .filter_map(|chain| chain.last())
                .map(|line| ApplyPatchCandidateSpan::from_position_and_len(line + 1, 0))
                .take(APPLY_PATCH_MATCH_CANDIDATE_LIMIT)
                .collect(),
            mode: None,
            attempts: Vec::new(),
            scope: ApplyPatchSearchScope::OrderedAnchorRange,
            range_rejection: None,
        });
    }

    let structural_ranges = structural_anchor_ranges(&file.lines, &chains);
    if !structural_ranges.is_empty() {
        match find_hunk_position_in_ranges(
            &file.lines,
            &hunk.old,
            &hunk.old_line_gap_policies(),
            &structural_ranges,
            hunk.range_hint,
            ApplyPatchSearchScope::StructuralAnchorScope,
        ) {
            Ok(hunk_match) => return Ok(hunk_match),
            Err(ApplyPatchLineSequenceFailure::Ambiguous {
                candidate_spans,
                mode,
                attempts,
                scope,
                range_rejection,
            }) => {
                return Err(ApplyPatchHunkMatchProblem::Ambiguous {
                    candidate_spans,
                    mode: Some(mode),
                    attempts,
                    scope,
                    range_rejection,
                });
            }
            Err(ApplyPatchLineSequenceFailure::Missing { .. }) => {}
        }
    }

    let ranges = ordered_anchor_search_ranges(&file.lines, &chains, cursor);
    find_hunk_position_in_ranges(
        &file.lines,
        &hunk.old,
        &hunk.old_line_gap_policies(),
        &ranges,
        hunk.range_hint,
        ApplyPatchSearchScope::OrderedAnchorRange,
    )
    .map_err(|failure| match failure {
        ApplyPatchLineSequenceFailure::Missing {
            attempts,
            range_rejection,
            ..
        } => ApplyPatchHunkMatchProblem::Missing {
            context_center: chains.first().and_then(|chain| chain.last()).copied(),
            missing_anchor: None,
            attempts,
            scope: ApplyPatchSearchScope::OrderedAnchorRange,
            range_rejection,
        },
        ApplyPatchLineSequenceFailure::Ambiguous {
            candidate_spans,
            mode,
            attempts,
            scope,
            range_rejection,
        } => ApplyPatchHunkMatchProblem::Ambiguous {
            candidate_spans,
            mode: Some(mode),
            attempts,
            scope,
            range_rejection,
        },
    })
}

fn ordered_anchor_search_ranges(
    lines: &[String],
    chains: &[Vec<usize>],
    cursor: usize,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for (chain_index, chain) in chains.iter().enumerate() {
        let search_start = chain.first().copied().unwrap_or(cursor);
        let search_end = chains
            .get(chain_index + 1)
            .and_then(|next| next.first().copied())
            .unwrap_or(lines.len());
        ranges.push((search_start, search_end));
    }
    ranges
}

fn find_unanchored_hunk_position(
    lines: &[String],
    old: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    cursor: usize,
    range_hint: Option<MezPatchRangeHint>,
) -> std::result::Result<ApplyPatchHunkMatch, ApplyPatchHunkMatchProblem> {
    if old.is_empty() {
        return Ok(ApplyPatchHunkMatch {
            position: apply_patch_preferred_position(range_hint, lines.len())
                .unwrap_or(lines.len()),
            mode: ApplyPatchMatchMode::Exact,
            old_line_offsets: Vec::new(),
        });
    }
    find_unanchored_hunk_position_layered(lines, old, blank_gap_policies, cursor, range_hint)
        .map_err(|failure| match failure {
            ApplyPatchLineSequenceFailure::Missing {
                attempts,
                range_rejection,
                ..
            } => ApplyPatchHunkMatchProblem::Missing {
                context_center: old.first().and_then(|line| {
                    apply_patch_anchor_line_numbers(lines, line)
                        .first()
                        .and_then(|line| line.checked_sub(1))
                }),
                missing_anchor: None,
                attempts,
                scope: ApplyPatchSearchScope::FullFile,
                range_rejection,
            },
            ApplyPatchLineSequenceFailure::Ambiguous {
                candidate_spans,
                mode,
                attempts,
                scope,
                range_rejection,
            } => ApplyPatchHunkMatchProblem::Ambiguous {
                candidate_spans,
                mode: Some(mode),
                attempts,
                scope,
                range_rejection,
            },
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ApplyPatchLineSequenceFailure {
    Missing {
        attempts: Vec<ApplyPatchMatchAttempt>,
        scope: ApplyPatchSearchScope,
        range_rejection: Option<ApplyPatchRangeHintRejection>,
    },
    Ambiguous {
        candidate_spans: Vec<ApplyPatchCandidateSpan>,
        mode: ApplyPatchMatchMode,
        attempts: Vec<ApplyPatchMatchAttempt>,
        scope: ApplyPatchSearchScope,
        range_rejection: Option<ApplyPatchRangeHintRejection>,
    },
}

fn find_hunk_position_in_ranges(
    lines: &[String],
    old: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    ranges: &[(usize, usize)],
    range_hint: Option<MezPatchRangeHint>,
    scope: ApplyPatchSearchScope,
) -> std::result::Result<ApplyPatchHunkMatch, ApplyPatchLineSequenceFailure> {
    let mut attempts = Vec::new();
    for mode in APPLY_PATCH_MATCH_MODES {
        let mut candidates = Vec::new();
        let mut capped = false;
        for (start, end) in ranges {
            let matches = find_line_sequence_matches(lines, old, *start, *end, *mode);
            capped |= matches.capped;
            candidates.extend(
                matches
                    .lines
                    .into_iter()
                    .map(|line| ApplyPatchLineSequenceMatch::exact(line, old.len())),
            );
            if candidates.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
                candidates.truncate(APPLY_PATCH_MATCH_CANDIDATE_LIMIT);
                capped = true;
                break;
            }
        }
        attempts.push(ApplyPatchMatchAttempt {
            mode: *mode,
            candidate_count: candidates.len(),
            capped,
        });
        if let Some(hunk_match) =
            resolve_hunk_candidates(candidates, capped, *mode, range_hint, &attempts, scope)?
        {
            return Ok(hunk_match);
        }
        let tolerant_matches = find_line_sequence_matches_omitting_blank_context(
            lines,
            old,
            blank_gap_policies,
            ranges,
            *mode,
        );
        if !tolerant_matches.lines.is_empty() {
            attempts.push(ApplyPatchMatchAttempt {
                mode: *mode,
                candidate_count: tolerant_matches.lines.len(),
                capped: tolerant_matches.capped,
            });
        }
        if let Some(hunk_match) = resolve_hunk_candidates(
            tolerant_matches.lines,
            tolerant_matches.capped,
            *mode,
            range_hint,
            &attempts,
            scope,
        )? {
            return Ok(hunk_match);
        }
    }
    Err(ApplyPatchLineSequenceFailure::Missing {
        attempts,
        scope,
        range_rejection: None,
    })
}

fn find_unanchored_hunk_position_layered(
    lines: &[String],
    old: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    cursor: usize,
    range_hint: Option<MezPatchRangeHint>,
) -> std::result::Result<ApplyPatchHunkMatch, ApplyPatchLineSequenceFailure> {
    let mut attempts = Vec::new();
    let scope = ApplyPatchSearchScope::FullFile;
    for mode in APPLY_PATCH_MATCH_MODES {
        let mut matches = find_line_sequence_matches(lines, old, cursor, lines.len(), *mode);
        if matches.lines.is_empty() && cursor > 0 {
            matches = find_line_sequence_matches(lines, old, 0, lines.len(), *mode);
        }
        let candidates = matches
            .lines
            .into_iter()
            .map(|line| ApplyPatchLineSequenceMatch::exact(line, old.len()))
            .collect::<Vec<_>>();
        attempts.push(ApplyPatchMatchAttempt {
            mode: *mode,
            candidate_count: candidates.len(),
            capped: matches.capped,
        });
        if let Some(hunk_match) = resolve_hunk_candidates(
            candidates,
            matches.capped,
            *mode,
            range_hint,
            &attempts,
            scope,
        )? {
            return Ok(hunk_match);
        }
        let tolerant_matches = find_line_sequence_matches_omitting_blank_context(
            lines,
            old,
            blank_gap_policies,
            &[(cursor.min(lines.len()), lines.len())],
            *mode,
        );
        if !tolerant_matches.lines.is_empty() {
            attempts.push(ApplyPatchMatchAttempt {
                mode: *mode,
                candidate_count: tolerant_matches.lines.len(),
                capped: tolerant_matches.capped,
            });
        }
        if let Some(hunk_match) = resolve_hunk_candidates(
            tolerant_matches.lines,
            tolerant_matches.capped,
            *mode,
            range_hint,
            &attempts,
            scope,
        )? {
            return Ok(hunk_match);
        }
    }
    Err(ApplyPatchLineSequenceFailure::Missing {
        attempts,
        scope,
        range_rejection: None,
    })
}

const APPLY_PATCH_RANGE_HINT_MAX_DISTANCE: usize = 20;
const APPLY_PATCH_RANGE_HINT_MIN_DISTANCE_GAP: usize = 3;

fn resolve_hunk_candidates(
    candidates: Vec<ApplyPatchLineSequenceMatch>,
    capped: bool,
    mode: ApplyPatchMatchMode,
    range_hint: Option<MezPatchRangeHint>,
    attempts: &[ApplyPatchMatchAttempt],
    scope: ApplyPatchSearchScope,
) -> std::result::Result<Option<ApplyPatchHunkMatch>, ApplyPatchLineSequenceFailure> {
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates
            .into_iter()
            .next()
            .map(|candidate| candidate.into_hunk_match(mode))),
        _ => match range_hint_candidate(
            &candidates,
            capped,
            if scope == ApplyPatchSearchScope::FullFile {
                range_hint
            } else {
                None
            },
        ) {
            ApplyPatchRangeHintSelection::Selected(index) => Ok(Some(
                candidates
                    .into_iter()
                    .nth(index)
                    .expect("selected candidate index should be valid")
                    .into_hunk_match(mode),
            )),
            ApplyPatchRangeHintSelection::Unavailable { rejection } => {
                Err(ApplyPatchLineSequenceFailure::Ambiguous {
                    candidate_spans: candidates
                        .iter()
                        .map(ApplyPatchLineSequenceMatch::span)
                        .collect(),
                    mode,
                    attempts: attempts.to_vec(),
                    scope,
                    range_rejection: rejection,
                })
            }
        },
    }
}

enum ApplyPatchRangeHintSelection {
    Selected(usize),
    Unavailable {
        rejection: Option<ApplyPatchRangeHintRejection>,
    },
}

fn range_hint_candidate(
    candidates: &[ApplyPatchLineSequenceMatch],
    capped: bool,
    range_hint: Option<MezPatchRangeHint>,
) -> ApplyPatchRangeHintSelection {
    let Some(range_hint) = range_hint else {
        return ApplyPatchRangeHintSelection::Unavailable { rejection: None };
    };
    if capped {
        return ApplyPatchRangeHintSelection::Unavailable {
            rejection: Some(ApplyPatchRangeHintRejection {
                hint_line: range_hint.old_start,
                nearest_distance: None,
                next_distance: None,
                reason: ApplyPatchRangeHintRejectionReason::CandidateListCapped,
            }),
        };
    }
    let hint_position = range_hint.old_start.saturating_sub(1);
    let mut distances = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            (
                index,
                range_hint_distance_to_candidate(hint_position, candidate),
            )
        })
        .collect::<Vec<_>>();
    distances.sort_by_key(|(_, distance)| *distance);
    let Some((nearest_index, nearest_distance)) = distances.first().copied() else {
        return ApplyPatchRangeHintSelection::Unavailable { rejection: None };
    };
    let next_distance = distances.get(1).map(|(_, distance)| *distance);
    if nearest_distance > APPLY_PATCH_RANGE_HINT_MAX_DISTANCE {
        return ApplyPatchRangeHintSelection::Unavailable {
            rejection: Some(ApplyPatchRangeHintRejection {
                hint_line: range_hint.old_start,
                nearest_distance: Some(nearest_distance),
                next_distance,
                reason: ApplyPatchRangeHintRejectionReason::Distant,
            }),
        };
    }
    if next_distance == Some(nearest_distance) {
        return ApplyPatchRangeHintSelection::Unavailable {
            rejection: Some(ApplyPatchRangeHintRejection {
                hint_line: range_hint.old_start,
                nearest_distance: Some(nearest_distance),
                next_distance,
                reason: ApplyPatchRangeHintRejectionReason::Tie,
            }),
        };
    }
    if let Some(next_distance) = next_distance
        && next_distance.saturating_sub(nearest_distance) < APPLY_PATCH_RANGE_HINT_MIN_DISTANCE_GAP
    {
        return ApplyPatchRangeHintSelection::Unavailable {
            rejection: Some(ApplyPatchRangeHintRejection {
                hint_line: range_hint.old_start,
                nearest_distance: Some(nearest_distance),
                next_distance: Some(next_distance),
                reason: ApplyPatchRangeHintRejectionReason::NearTie,
            }),
        };
    }
    ApplyPatchRangeHintSelection::Selected(nearest_index)
}

fn range_hint_distance_to_candidate(
    hint_position: usize,
    candidate: &ApplyPatchLineSequenceMatch,
) -> usize {
    let start = candidate.position;
    let end = candidate
        .span_len()
        .saturating_sub(1)
        .saturating_add(candidate.position);
    if hint_position < start {
        start - hint_position
    } else if hint_position > end {
        hint_position.saturating_sub(end)
    } else {
        0
    }
}

fn apply_patch_preferred_position(
    range_hint: Option<MezPatchRangeHint>,
    line_count: usize,
) -> Option<usize> {
    range_hint.map(|hint| hint.old_start.saturating_sub(1).min(line_count))
}

fn structural_anchor_ranges(lines: &[String], chains: &[Vec<usize>]) -> Vec<(usize, usize)> {
    chains
        .iter()
        .filter_map(|chain| {
            let anchor_line = chain.last().copied()?;
            rust_like_block_scope(lines, anchor_line)
        })
        .collect()
}

fn rust_like_block_scope(lines: &[String], anchor_line: usize) -> Option<(usize, usize)> {
    let anchor_text = lines.get(anchor_line)?;
    if !looks_like_rust_structural_anchor(anchor_text) {
        return None;
    }
    let mut depth = 0isize;
    let mut saw_open = false;
    let mut in_block_comment = false;
    for (line_index, line) in lines
        .iter()
        .enumerate()
        .skip(anchor_line)
        .take(APPLY_PATCH_STRUCTURAL_ANCHOR_SCAN_LINES)
    {
        let (opens, closes) = rust_like_brace_counts(line, &mut in_block_comment)?;
        if opens > 0 {
            saw_open = true;
        }
        depth += opens as isize;
        depth -= closes as isize;
        if depth < 0 {
            return None;
        }
        if saw_open && depth == 0 {
            return Some((anchor_line, line_index + 1));
        }
    }
    None
}

const APPLY_PATCH_STRUCTURAL_ANCHOR_SCAN_LINES: usize = 400;

fn looks_like_rust_structural_anchor(line: &str) -> bool {
    let line = line.trim_start();
    RUST_STRUCTURAL_ANCHOR_PREFIXES
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

const RUST_STRUCTURAL_ANCHOR_PREFIXES: &[&str] = &[
    "fn ",
    "pub fn ",
    "pub(crate) fn ",
    "pub(super) fn ",
    "async fn ",
    "pub async fn ",
    "const fn ",
    "pub const fn ",
    "impl ",
    "trait ",
    "pub trait ",
    "struct ",
    "pub struct ",
    "enum ",
    "pub enum ",
    "mod ",
    "pub mod ",
];

fn rust_like_brace_counts(line: &str, in_block_comment: &mut bool) -> Option<(usize, usize)> {
    if line.contains("r#\"") || line.contains("r\"") {
        return None;
    }
    let mut opens = 0usize;
    let mut closes = 0usize;
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    let mut in_char = false;
    let mut escaped = false;
    while let Some(character) = chars.next() {
        if *in_block_comment {
            if character == '*' && chars.peek() == Some(&'/') {
                chars.next();
                *in_block_comment = false;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if in_char {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '\'' {
                in_char = false;
            }
            continue;
        }
        match character {
            '/' if chars.peek() == Some(&'/') => break,
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                *in_block_comment = true;
            }
            '"' => in_string = true,
            '\'' => in_char = true,
            '{' => opens += 1,
            '}' => closes += 1,
            _ => {}
        }
    }
    Some((opens, closes))
}

fn ordered_anchor_chains(lines: &[String], anchors: &[String], cursor: usize) -> Vec<Vec<usize>> {
    if anchors.is_empty() {
        return vec![Vec::new()];
    }

    let mut chains = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(cursor.min(lines.len())) {
        if !line.contains(&anchors[0]) {
            continue;
        }
        let mut chain = vec![index];
        let mut next_start = index + 1;
        let mut complete = true;
        for anchor in &anchors[1..] {
            if let Some((next_index, _)) = lines
                .iter()
                .enumerate()
                .skip(next_start)
                .find(|(_, line)| line.contains(anchor))
            {
                chain.push(next_index);
                next_start = next_index + 1;
            } else {
                complete = false;
                break;
            }
        }
        if complete {
            chains.push(chain);
            if chains.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
                break;
            }
        }
    }
    chains
}

fn first_missing_ordered_anchor(
    lines: &[String],
    anchors: &[String],
    cursor: usize,
) -> Option<String> {
    let mut next_start = cursor.min(lines.len());
    for anchor in anchors {
        if let Some((next_index, _)) = lines
            .iter()
            .enumerate()
            .skip(next_start)
            .find(|(_, line)| line.contains(anchor))
        {
            next_start = next_index + 1;
        } else {
            return Some(anchor.clone());
        }
    }
    None
}

fn find_line_sequence_matches(
    lines: &[String],
    needle: &[String],
    start: usize,
    end: usize,
    mode: ApplyPatchMatchMode,
) -> ApplyPatchCandidateMatches {
    if needle.is_empty() {
        return ApplyPatchCandidateMatches {
            lines: vec![start.min(lines.len())],
            capped: false,
        };
    }
    let start = start.min(lines.len());
    let end = end.min(lines.len());
    if end < start || end.saturating_sub(start) < needle.len() {
        return ApplyPatchCandidateMatches {
            lines: Vec::new(),
            capped: false,
        };
    }
    let last_start = end - needle.len();
    let mut matches = Vec::new();
    let mut capped = false;
    for index in start..=last_start {
        if line_sequence_matches(&lines[index..index + needle.len()], needle, mode) {
            matches.push(index);
            if matches.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
                capped = true;
                break;
            }
        }
    }
    ApplyPatchCandidateMatches {
        lines: matches,
        capped,
    }
}

fn find_line_sequence_matches_omitting_blank_context(
    lines: &[String],
    needle: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    ranges: &[(usize, usize)],
    mode: ApplyPatchMatchMode,
) -> ApplyPatchLineSequenceMatches {
    if !blank_gap_policies
        .iter()
        .any(|policy| *policy != ApplyPatchBlankGapPolicy::Disallow)
        || blank_gap_policies.len() != needle.len()
    {
        return ApplyPatchLineSequenceMatches {
            lines: Vec::new(),
            capped: false,
        };
    }
    let mut matches = Vec::new();
    let mut capped = false;
    for (start, end) in ranges {
        let start = (*start).min(lines.len());
        let end = (*end).min(lines.len());
        if end < start {
            continue;
        }
        for index in start..end {
            if let Some(match_result) = line_sequence_match_omitting_blank_context_at(
                lines,
                needle,
                blank_gap_policies,
                index,
                end,
                mode,
            ) {
                matches.push(match_result);
                if matches.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
                    capped = true;
                    return ApplyPatchLineSequenceMatches {
                        lines: matches,
                        capped,
                    };
                }
            }
        }
    }
    ApplyPatchLineSequenceMatches {
        lines: matches,
        capped,
    }
}

fn line_sequence_match_omitting_blank_context_at(
    lines: &[String],
    needle: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    position: usize,
    end: usize,
    mode: ApplyPatchMatchMode,
) -> Option<ApplyPatchLineSequenceMatch> {
    if needle.is_empty()
        || blank_gap_policies.len() != needle.len()
        || !blank_gap_policies
            .iter()
            .any(|policy| *policy != ApplyPatchBlankGapPolicy::Disallow)
    {
        return None;
    }
    let end = end.min(lines.len());
    if position >= end {
        return None;
    }
    let mut actual_index = position;
    let mut old_line_offsets = Vec::with_capacity(needle.len());
    let mut skipped_blank = false;
    for (needle_index, expected) in needle.iter().enumerate() {
        if actual_index >= end {
            return None;
        }
        if patch_line_matches(&lines[actual_index], expected, mode) {
            old_line_offsets.push(actual_index.saturating_sub(position));
            actual_index += 1;
            continue;
        }
        if blank_gap_policies[needle_index] == ApplyPatchBlankGapPolicy::Disallow {
            return None;
        }
        let blank_start = actual_index;
        while actual_index < end && lines[actual_index].trim().is_empty() {
            actual_index += 1;
        }
        if actual_index == blank_start || actual_index >= end {
            return None;
        }
        if !patch_line_matches(&lines[actual_index], expected, mode) {
            return None;
        }
        skipped_blank = true;
        old_line_offsets.push(actual_index.saturating_sub(position));
        actual_index += 1;
    }
    skipped_blank.then_some(ApplyPatchLineSequenceMatch {
        position,
        old_line_offsets,
    })
}

fn line_sequence_matches(
    actual: &[String],
    expected: &[String],
    mode: ApplyPatchMatchMode,
) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| patch_line_matches(actual, expected, mode))
}

fn patch_line_matches(actual: &str, expected: &str, mode: ApplyPatchMatchMode) -> bool {
    match mode {
        ApplyPatchMatchMode::Exact => actual == expected,
        ApplyPatchMatchMode::TrimEnd => actual.trim_end() == expected.trim_end(),
        ApplyPatchMatchMode::Trim => actual.trim() == expected.trim(),
        ApplyPatchMatchMode::Normalized => {
            normalized_patch_line(actual) == normalized_patch_line(expected)
        }
    }
}

fn normalized_patch_line(line: &str) -> String {
    line.trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

fn apply_patch_anchor_line_numbers(lines: &[String], anchor: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (line == anchor).then_some(index + 1))
        .take(APPLY_PATCH_MISMATCH_ANCHOR_LIMIT)
        .collect()
}

fn apply_patch_mismatch_excerpt(line: &str) -> String {
    let mut excerpt: String = line.chars().take(APPLY_PATCH_MISMATCH_LINE_CHARS).collect();
    if line.chars().count() > APPLY_PATCH_MISMATCH_LINE_CHARS {
        excerpt.push_str("...");
    }
    excerpt
}

fn apply_patch_current_context_message(file: &ApplyPatchTextFile, center: usize) -> String {
    if file.lines.is_empty() {
        return "\napply_patch: current file is empty".to_string();
    }
    let Some((start_line, end_line)) = apply_patch_current_context_range(file, center) else {
        return "\napply_patch: current file is empty".to_string();
    };
    let mut message = format!(
        "\napply_patch: current file context near line {} follows:",
        center + 1
    );
    for index in start_line.saturating_sub(1)..end_line {
        message.push_str(&format!(
            "\napply_patch:   {:>4}: {}",
            index + 1,
            apply_patch_mismatch_excerpt(&file.lines[index])
        ));
    }
    message
}

fn apply_patch_current_context_range(
    file: &ApplyPatchTextFile,
    center: usize,
) -> Option<(usize, usize)> {
    if file.lines.is_empty() {
        return None;
    }
    let start = center.saturating_sub(APPLY_PATCH_MISMATCH_CONTEXT_LINES / 2);
    let end = (center + (APPLY_PATCH_MISMATCH_CONTEXT_LINES / 2) + 1).min(file.lines.len());
    Some((start + 1, end))
}

fn snapshot_text_state(snapshot: &ApplyPatchSnapshot) -> Result<Option<ApplyPatchTextFile>> {
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

fn ensure_missing_state(path: &str, state: Option<&Option<ApplyPatchTextFile>>) -> Result<()> {
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

fn ensure_regular_state<'a>(
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
