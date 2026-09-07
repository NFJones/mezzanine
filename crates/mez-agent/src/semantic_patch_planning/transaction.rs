//! Shell transaction generation for semantic apply-patch actions.
//!
//! The semantic patch pipeline verifies desired file mutations before shell
//! execution. This module owns only the generated shell source used to read
//! remote file snapshots, write verified content bytes, and present unified
//! diffs after the write phase succeeds.

use super::{
    APPLY_PATCH_CONTENT_BEGIN_MARKER, APPLY_PATCH_CONTENT_END_MARKER, APPLY_PATCH_DIFF_MARKER,
    APPLY_PATCH_FILE_BEGIN_MARKER, APPLY_PATCH_FILE_END_MARKER,
    APPLY_PATCH_PROGRESS_MAX_RETAINED_BYTES, APPLY_PATCH_READ_BEGIN_MARKER,
    APPLY_PATCH_READ_END_MARKER, APPLY_PATCH_READ_PHASE_MARKER, APPLY_PATCH_RESULT_MARKER,
    ApplyPatchFileChange, ApplyPatchFileOutcome, ApplyPatchOriginalState, ApplyPatchPathBoundary,
    apply_patch_path_resolution_lines,
};
use crate::semantic_patch::{SemanticPatchPlanningError, SemanticPatchPlanningResult};
use crate::shell_quote;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Maximum Base64 payload bytes emitted in one semantic-write sidecar record.
///
/// Sidecar records cross the pane PTY as bounded data. Keeping individual
/// records below common canonical-line limits preserves portable receiver
/// behavior while avoiding recursive encoding of generated shell source.
pub(super) const FILE_CONTENT_BASE64_SHELL_LINE_BYTES: usize = 768;

/// One shell-backed phase used to complete an `apply_patch` action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyPatchTransactionPhase {
    /// The action is reading remote file snapshots.
    Read,
    /// The action is verifying and writing patched bytes.
    Write,
}

/// One per-file diff whose matching write result authoritatively confirmed the
/// filesystem mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPatchConfirmedSection {
    /// Zero-based write-order identity emitted by the generated transaction.
    pub ordinal: usize,
    /// UTF-8 patch target path associated with the section.
    pub path: String,
    /// Exact UTF-8 unified diff emitted for the confirmed mutation.
    pub diff: String,
}

/// Newly decoded semantic-patch progress from one arbitrary byte chunk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ApplyPatchProgress {
    /// Diff sections newly confirmed while consuming the chunk.
    pub confirmed_sections: Vec<ApplyPatchConfirmedSection>,
    /// Per-file outcomes newly completed while consuming the chunk.
    pub outcomes: Vec<ApplyPatchFileOutcome>,
}

impl ApplyPatchProgress {
    /// Appends later progress while preserving transaction order.
    pub fn extend(&mut self, later: Self) {
        self.confirmed_sections.extend(later.confirmed_sections);
        self.outcomes.extend(later.outcomes);
    }

    /// Reports whether no section or outcome became available.
    pub fn is_empty(&self) -> bool {
        self.confirmed_sections.is_empty() && self.outcomes.is_empty()
    }
}

#[derive(Debug)]
struct PendingApplyPatchDiff {
    ordinal: usize,
    path: String,
    expected_len: usize,
    bytes: Vec<u8>,
}

/// Incrementally decodes length-delimited write output without exposing a diff
/// until the immediately following `APPLIED` record matches its ordinal, path,
/// and byte length.
///
/// Input may be split at any byte boundary. Unrelated complete or partial lines
/// are discarded incrementally, while candidate framing and one proposed diff
/// are retained privately up to [`APPLY_PATCH_PROGRESS_MAX_RETAINED_BYTES`].
/// Any malformed, stale, duplicate, mismatched, or truncated framing poisons
/// the decoder and prevents later bytes from releasing buffered content.
#[derive(Debug)]
pub struct ApplyPatchProgressDecoder {
    next_ordinal: usize,
    pending: Option<PendingApplyPatchDiff>,
    remaining_diff_bytes: Option<usize>,
    line: Vec<u8>,
    discarding_line: bool,
    cumulative_bytes: usize,
    poisoned: bool,
}

impl Default for ApplyPatchProgressDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ApplyPatchProgressDecoder {
    /// Creates a decoder expecting per-file ordinal zero.
    pub fn new() -> Self {
        Self {
            next_ordinal: 0,
            pending: None,
            remaining_diff_bytes: None,
            line: Vec::new(),
            discarding_line: false,
            cumulative_bytes: 0,
            poisoned: false,
        }
    }

    /// Consumes one arbitrary byte chunk and returns only newly confirmed data.
    ///
    /// The method returns an error and permanently poisons the decoder when a
    /// runtime-owned framing line is malformed or inconsistent.
    pub fn push(&mut self, chunk: &[u8]) -> SemanticPatchPlanningResult<ApplyPatchProgress> {
        if self.poisoned {
            return Err(apply_patch_progress_error(
                "confirmed progress decoder is already invalid",
            ));
        }
        match self.push_inner(chunk) {
            Ok(progress) => Ok(progress),
            Err(error) => {
                self.poison();
                Err(error)
            }
        }
    }

    /// Consumes one append-only cumulative observation without rescanning bytes
    /// already accepted by an earlier call.
    ///
    /// A shorter observation is stale or belongs to another attempt and
    /// permanently poisons the decoder. Attempt identity itself remains the
    /// responsibility of the runtime's outer transaction fence.
    pub fn push_cumulative(
        &mut self,
        observation: &[u8],
    ) -> SemanticPatchPlanningResult<ApplyPatchProgress> {
        if observation.len() < self.cumulative_bytes {
            self.poison();
            return Err(apply_patch_progress_error(
                "cumulative observation moved backwards",
            ));
        }
        let start = self.cumulative_bytes;
        let progress = self.push(&observation[start..])?;
        self.cumulative_bytes = observation.len();
        Ok(progress)
    }

    /// Finalizes the current observation and rejects incomplete private state.
    pub fn finish(&mut self) -> SemanticPatchPlanningResult<ApplyPatchProgress> {
        if self.poisoned {
            return Err(apply_patch_progress_error(
                "confirmed progress decoder is already invalid",
            ));
        }
        if self.pending.is_some()
            || self.remaining_diff_bytes.is_some()
            || (!self.line.is_empty() && apply_patch_line_could_be_framing(&self.line))
        {
            self.poison();
            return Err(apply_patch_progress_error(
                "truncated confirmed progress framing",
            ));
        }
        self.line.clear();
        self.discarding_line = false;
        Ok(ApplyPatchProgress::default())
    }

    fn push_inner(&mut self, chunk: &[u8]) -> SemanticPatchPlanningResult<ApplyPatchProgress> {
        let mut progress = ApplyPatchProgress::default();
        let mut cursor = 0;
        while cursor < chunk.len() {
            if let Some(remaining) = self.remaining_diff_bytes {
                let take = remaining.min(chunk.len() - cursor);
                let pending = self.pending.as_mut().ok_or_else(|| {
                    apply_patch_progress_error("diff bytes have no pending section")
                })?;
                pending
                    .bytes
                    .extend_from_slice(&chunk[cursor..cursor + take]);
                cursor += take;
                let remaining = remaining - take;
                self.remaining_diff_bytes = (remaining != 0).then_some(remaining);
                continue;
            }

            let byte = chunk[cursor];
            cursor += 1;
            if self.discarding_line {
                if byte == b'\n' {
                    self.discarding_line = false;
                }
                continue;
            }
            self.line.push(byte);
            if byte == b'\n' {
                let line = std::mem::take(&mut self.line);
                self.process_framing_line(&line, &mut progress)?;
                continue;
            }
            if !apply_patch_line_could_be_framing(&self.line) {
                self.line.clear();
                self.discarding_line = true;
                continue;
            }
            if self.line.len() > APPLY_PATCH_PROGRESS_MAX_RETAINED_BYTES {
                return Err(apply_patch_progress_error(
                    "confirmed progress retained-byte limit exceeded",
                ));
            }
        }
        Ok(progress)
    }

    fn process_framing_line(
        &mut self,
        line: &[u8],
        progress: &mut ApplyPatchProgress,
    ) -> SemanticPatchPlanningResult<()> {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let text = std::str::from_utf8(line)
            .map_err(|_| apply_patch_progress_error("framing line is not UTF-8"))?;
        let fields = text.split_whitespace().collect::<Vec<_>>();
        match fields.as_slice() {
            [marker, ordinal, path, length] if *marker == APPLY_PATCH_DIFF_MARKER => {
                self.start_diff(ordinal, path, length)
            }
            [marker, "APPLIED", ordinal, path, length] if *marker == APPLY_PATCH_RESULT_MARKER => {
                self.confirm_diff(ordinal, path, length, progress)
            }
            [marker, "FAILED", ordinal, path, diagnostic]
                if *marker == APPLY_PATCH_RESULT_MARKER =>
            {
                self.record_failure(ordinal, path, diagnostic, progress)
            }
            [marker, ..]
                if *marker == APPLY_PATCH_DIFF_MARKER || *marker == APPLY_PATCH_RESULT_MARKER =>
            {
                Err(apply_patch_progress_error(
                    "malformed confirmed progress record",
                ))
            }
            _ => Ok(()),
        }
    }

    fn start_diff(
        &mut self,
        ordinal: &str,
        path: &str,
        length: &str,
    ) -> SemanticPatchPlanningResult<()> {
        if self.pending.is_some() {
            return Err(apply_patch_progress_error(
                "received a new diff before confirming the prior section",
            ));
        }
        let ordinal = parse_apply_patch_ordinal(ordinal)?;
        self.require_next_ordinal(ordinal)?;
        let path = decode_apply_patch_utf8(path, "path")?;
        let expected_len = parse_apply_patch_length(length)?;
        if expected_len > APPLY_PATCH_PROGRESS_MAX_RETAINED_BYTES {
            return Err(apply_patch_progress_error(
                "confirmed progress retained-byte limit exceeded",
            ));
        }
        self.pending = Some(PendingApplyPatchDiff {
            ordinal,
            path,
            expected_len,
            bytes: Vec::with_capacity(expected_len.min(16 * 1024)),
        });
        self.remaining_diff_bytes = (expected_len != 0).then_some(expected_len);
        Ok(())
    }

    fn confirm_diff(
        &mut self,
        ordinal: &str,
        path: &str,
        length: &str,
        progress: &mut ApplyPatchProgress,
    ) -> SemanticPatchPlanningResult<()> {
        if self.remaining_diff_bytes.is_some() {
            return Err(apply_patch_progress_error(
                "applied record arrived before the complete diff section",
            ));
        }
        let ordinal = parse_apply_patch_ordinal(ordinal)?;
        self.require_next_ordinal(ordinal)?;
        let path = decode_apply_patch_utf8(path, "path")?;
        let length = parse_apply_patch_length(length)?;
        let pending = self.pending.take().ok_or_else(|| {
            apply_patch_progress_error("applied record has no proposed diff section")
        })?;
        if pending.ordinal != ordinal || pending.path != path || pending.expected_len != length {
            return Err(apply_patch_progress_error(
                "applied record does not match diff ordinal, path, and length",
            ));
        }
        let diff = String::from_utf8(pending.bytes)
            .map_err(|_| apply_patch_progress_error("confirmed diff is not UTF-8"))?;
        progress
            .confirmed_sections
            .push(ApplyPatchConfirmedSection {
                ordinal,
                path: path.clone(),
                diff,
            });
        progress
            .outcomes
            .push(ApplyPatchFileOutcome::Applied { path });
        self.advance_ordinal()?;
        Ok(())
    }

    fn record_failure(
        &mut self,
        ordinal: &str,
        path: &str,
        diagnostic: &str,
        progress: &mut ApplyPatchProgress,
    ) -> SemanticPatchPlanningResult<()> {
        if self.pending.is_some() || self.remaining_diff_bytes.is_some() {
            return Err(apply_patch_progress_error(
                "failed record interrupted a proposed diff section",
            ));
        }
        let ordinal = parse_apply_patch_ordinal(ordinal)?;
        self.require_next_ordinal(ordinal)?;
        let path = decode_apply_patch_utf8(path, "path")?;
        let diagnostic = decode_apply_patch_utf8(diagnostic, "diagnostic")?;
        progress
            .outcomes
            .push(ApplyPatchFileOutcome::Failed { path, diagnostic });
        self.advance_ordinal()?;
        Ok(())
    }

    fn require_next_ordinal(&self, ordinal: usize) -> SemanticPatchPlanningResult<()> {
        if ordinal != self.next_ordinal {
            return Err(apply_patch_progress_error(
                "duplicate, stale, or out-of-order per-file ordinal",
            ));
        }
        Ok(())
    }

    fn advance_ordinal(&mut self) -> SemanticPatchPlanningResult<()> {
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or_else(|| apply_patch_progress_error("per-file ordinal overflow"))?;
        Ok(())
    }

    fn poison(&mut self) {
        self.poisoned = true;
        self.pending = None;
        self.remaining_diff_bytes = None;
        self.line.clear();
        self.discarding_line = false;
    }
}

fn apply_patch_line_could_be_framing(line: &[u8]) -> bool {
    [APPLY_PATCH_DIFF_MARKER, APPLY_PATCH_RESULT_MARKER]
        .iter()
        .map(|marker| marker.as_bytes())
        .any(|marker| marker.starts_with(line) || line.starts_with(marker))
}

fn parse_apply_patch_ordinal(value: &str) -> SemanticPatchPlanningResult<usize> {
    value
        .parse()
        .map_err(|_| apply_patch_progress_error("malformed per-file ordinal"))
}

fn parse_apply_patch_length(value: &str) -> SemanticPatchPlanningResult<usize> {
    value
        .parse()
        .map_err(|_| apply_patch_progress_error("malformed diff byte length"))
}

fn decode_apply_patch_utf8(encoded: &str, field: &str) -> SemanticPatchPlanningResult<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| apply_patch_progress_error(&format!("malformed per-file {field}")))?;
    String::from_utf8(bytes)
        .map_err(|_| apply_patch_progress_error(&format!("per-file {field} is not UTF-8")))
}

fn apply_patch_progress_error(message: &str) -> SemanticPatchPlanningError {
    SemanticPatchPlanningError::invalid_args(format!("apply_patch: {message}"))
}

pub(super) fn shell_print_line(line: &str) -> String {
    format!("printf '%s\\n' {}", shell_quote(line))
}

pub(super) fn unified_diff_lines(
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

/// Returns a lowercase SHA-256 digest for exact semantic-write preconditions.
fn content_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Renders the single-encoded final-byte records consumed by write commands.
pub(super) fn apply_patch_write_sidecar(changes: &[ApplyPatchFileChange]) -> Option<String> {
    let mut sidecar = String::new();
    for (index, change) in changes.iter().enumerate() {
        let Some(final_bytes) = &change.final_bytes else {
            continue;
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(final_bytes);
        if encoded.is_empty() {
            sidecar.push_str(&format!("{index} \n"));
            continue;
        }
        for chunk in encoded
            .as_bytes()
            .chunks(FILE_CONTENT_BASE64_SHELL_LINE_BYTES)
        {
            let chunk = std::str::from_utf8(chunk)
                .expect("standard base64 output should always be valid UTF-8");
            sidecar.push_str(&format!("{index} {chunk}\n"));
        }
    }
    (!sidecar.is_empty()).then_some(sidecar)
}

fn apply_patch_boundary_case_lines(
    boundary: &ApplyPatchPathBoundary,
    failure_command: &str,
) -> Vec<String> {
    match boundary {
        ApplyPatchPathBoundary::CurrentDirectoryOnly => vec![format!(
            "case \"$MEZ_APPLY_PATH\" in /*) {failure_command} ;; *) case \"$MEZ_APPLY_RESOLVED\" in \"$MEZ_APPLY_CWD\"|\"$MEZ_APPLY_CWD_PREFIX\"/*) ;; *) {failure_command} ;; esac ;; esac"
        )],
        ApplyPatchPathBoundary::SandboxWriteScopes(scopes) => {
            let patterns = scopes
                .iter()
                .flat_map(|scope| {
                    let quoted = shell_quote(scope);
                    [quoted.clone(), format!("{quoted}/*")]
                })
                .collect::<Vec<_>>()
                .join("|");
            if patterns.is_empty() {
                vec![failure_command.to_string()]
            } else {
                vec![format!(
                    "case \"$MEZ_APPLY_RESOLVED\" in {patterns}) ;; *) {failure_command} ;; esac"
                )]
            }
        }
    }
}

pub(super) fn mez_apply_patch_read_command(
    paths: &BTreeSet<String>,
    boundary: &ApplyPatchPathBoundary,
) -> String {
    let mut lines = vec![
        format!("# {APPLY_PATCH_READ_PHASE_MARKER}"),
        "command -v base64 >/dev/null || { printf '%s\\n' 'apply_patch: base64 is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v tr >/dev/null || { printf '%s\\n' 'apply_patch: tr is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "MEZ_APPLY_CWD=$(pwd -P) || exit 1".to_string(),
        "MEZ_APPLY_CWD_PREFIX=${MEZ_APPLY_CWD%/}".to_string(),
        "if [ -z \"$MEZ_APPLY_CWD_PREFIX\" ]; then MEZ_APPLY_CWD_PREFIX=/; fi".to_string(),
    ];
    lines.extend(apply_patch_path_resolution_lines());
    lines.extend([
        "mez_apply_patch_b64() { printf '%s' \"$1\" | base64 | tr -d '\\n'; }".to_string(),
        "mez_apply_patch_emit_path() {".to_string(),
        "MEZ_APPLY_PATH=$1".to_string(),
        "MEZ_APPLY_RESOLVED=$(mez_apply_patch_resolve \"$MEZ_APPLY_PATH\" 2>/dev/null) || MEZ_APPLY_RESOLVED=".to_string(),
        "MEZ_APPLY_STATUS=error".to_string(),
        "if [ -n \"$MEZ_APPLY_RESOLVED\" ]; then".to_string(),
    ]);
    let outside_status = match boundary {
        ApplyPatchPathBoundary::CurrentDirectoryOnly => "outside_cwd",
        ApplyPatchPathBoundary::SandboxWriteScopes(_) => "outside_write_scopes",
    };
    lines.extend(apply_patch_boundary_case_lines(
        boundary,
        &format!("MEZ_APPLY_STATUS={outside_status}"),
    ));
    lines.extend([
        "  if [ \"$MEZ_APPLY_STATUS\" = error ]; then".to_string(),
        "    if [ -e \"$MEZ_APPLY_PATH\" ] || [ -L \"$MEZ_APPLY_PATH\" ]; then".to_string(),
        "      if [ -f \"$MEZ_APPLY_RESOLVED\" ]; then MEZ_APPLY_STATUS=regular; else MEZ_APPLY_STATUS=non_regular; fi".to_string(),
        "    else".to_string(),
        "      MEZ_APPLY_STATUS=missing".to_string(),
        "    fi".to_string(),
        "  fi".to_string(),
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
    ]);
    for path in paths {
        lines.push(format!("mez_apply_patch_emit_path {}", shell_quote(path)));
    }
    lines.extend([
        format!("printf '%s\\n' {}", shell_quote(APPLY_PATCH_READ_END_MARKER)),
        "unset -f mez_apply_patch_emit_path mez_apply_patch_b64 mez_apply_patch_resolve 2>/dev/null || :".to_string(),
        "unset MEZ_APPLY_CWD MEZ_APPLY_CWD_PREFIX MEZ_APPLY_PATH MEZ_APPLY_RESOLVED MEZ_APPLY_STATUS MEZ_APPLY_USE_REALPATH_M MEZ_APPLY_READLINK".to_string(),
    ]);
    lines.join("\n")
}

pub(super) fn apply_patch_write_command_prelude(boundary: &ApplyPatchPathBoundary) -> String {
    let mut lines = vec![
        "command -v base64 >/dev/null || { printf '%s\\n' 'apply_patch: base64 is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v dirname >/dev/null || { printf '%s\\n' 'apply_patch: dirname is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v sed >/dev/null || { printf '%s\\n' 'apply_patch: sed is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v tr >/dev/null || { printf '%s\\n' 'apply_patch: tr is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "if command -v sha256sum >/dev/null 2>&1; then MEZ_APPLY_SHA256=sha256sum; elif command -v shasum >/dev/null 2>&1; then MEZ_APPLY_SHA256=shasum; else printf '%s\\n' 'apply_patch: sha256sum or shasum is required for apply_patch actions' >&2; exit 127; fi".to_string(),
        "MEZ_APPLY_SIDECAR_FILE=${MEZ_APPLY_SIDECAR_FILE:-$0}".to_string(),
        "MEZ_APPLY_CWD=$(pwd -P) || exit 1".to_string(),
        "MEZ_APPLY_CWD_PREFIX=${MEZ_APPLY_CWD%/}".to_string(),
        "if [ -z \"$MEZ_APPLY_CWD_PREFIX\" ]; then MEZ_APPLY_CWD_PREFIX=/; fi".to_string(),
    ];
    lines.extend(apply_patch_path_resolution_lines());
    lines.extend([
        "mez_apply_patch_resolve_checked() {".to_string(),
        "MEZ_APPLY_PATH=$1".to_string(),
        "MEZ_APPLY_EXPECTED_RESOLVED=$2".to_string(),
        "MEZ_APPLY_RESOLVED=".to_string(),
        "MEZ_APPLY_RESOLVED=$(mez_apply_patch_resolve \"$MEZ_APPLY_PATH\" 2>/dev/null) || { printf '%s\\n' \"apply_patch: failed to resolve path: $MEZ_APPLY_PATH\" >&2; return 1; }".to_string(),
    ]);
    let diagnostic = match boundary {
        ApplyPatchPathBoundary::CurrentDirectoryOnly => {
            "apply_patch: resolved path is outside current working directory: $MEZ_APPLY_PATH"
        }
        ApplyPatchPathBoundary::SandboxWriteScopes(_) => {
            "apply_patch: resolved path is outside configured sandbox write scopes: $MEZ_APPLY_PATH"
        }
    };
    lines.extend(apply_patch_boundary_case_lines(
        boundary,
        &format!("printf '%s\\n' {} >&2; return 1", shell_quote(diagnostic)),
    ));
    lines.extend([
        "if [ \"$MEZ_APPLY_RESOLVED\" != \"$MEZ_APPLY_EXPECTED_RESOLVED\" ]; then printf '%s\\n' \"apply_patch: resolved path changed before apply: $MEZ_APPLY_PATH\" >&2; return 1; fi".to_string(),
        "}".to_string(),
        "mez_apply_patch_sha256() { if [ \"$MEZ_APPLY_SHA256\" = sha256sum ]; then sha256sum -- \"$1\"; else shasum -a 256 -- \"$1\"; fi | sed 's/[[:space:]].*$//'; }".to_string(),
        "mez_apply_patch_verify_regular() { MEZ_APPLY_VERIFY_PATH=$1; MEZ_APPLY_VERIFY_COUNT=$2; MEZ_APPLY_VERIFY_DIGEST=$3; MEZ_APPLY_VERIFY_LABEL=$4; if [ ! -f \"$MEZ_APPLY_RESOLVED\" ]; then printf '%s\\n' \"apply_patch: refusing to patch non-regular file: $MEZ_APPLY_VERIFY_LABEL\" >&2; return 1; fi; MEZ_APPLY_ACTUAL_COUNT=$(wc -c < \"$MEZ_APPLY_RESOLVED\" | tr -d '[:space:]') || return 1; MEZ_APPLY_ACTUAL_DIGEST=$(mez_apply_patch_sha256 \"$MEZ_APPLY_RESOLVED\") || return 1; if [ \"$MEZ_APPLY_ACTUAL_COUNT\" != \"$MEZ_APPLY_VERIFY_COUNT\" ] || [ \"$MEZ_APPLY_ACTUAL_DIGEST\" != \"$MEZ_APPLY_VERIFY_DIGEST\" ]; then printf '%s\\n' \"apply_patch: file changed before apply: $MEZ_APPLY_VERIFY_LABEL\" >&2; return 1; fi; }".to_string(),
        String::new(),
    ]);
    lines.join("\n")
}

pub(super) fn apply_patch_write_change_command(
    index: usize,
    change: &ApplyPatchFileChange,
) -> String {
    let new_var = format!("MEZ_APPLY_NEW_{index}");
    let encoded_var = format!("MEZ_APPLY_ENCODED_{index}");
    let original_is_regular = matches!(&change.original, ApplyPatchOriginalState::Regular(_));
    let function_name = format!("mez_apply_patch_change_{index}");
    let error_var = format!("MEZ_APPLY_ERROR_{index}");
    let output_var = format!("MEZ_APPLY_OUTPUT_{index}");
    let mut lines = vec![
        format!("{function_name}() {{"),
        format!(
            "mez_apply_patch_resolve_checked {} {} || return 1",
            shell_quote(&change.path),
            shell_quote(&change.resolved_path)
        ),
    ];
    match &change.original {
        ApplyPatchOriginalState::Regular(bytes) => {
            lines.push(format!(
                "mez_apply_patch_verify_regular {} {} {} {} || return 1",
                shell_quote(&change.resolved_path),
                bytes.len(),
                shell_quote(&content_sha256(bytes)),
                shell_quote(&change.path),
            ));
        }
        ApplyPatchOriginalState::Missing => {
            lines.push(format!(
                "if [ -e {} ] || [ -L {} ] || [ -e \"$MEZ_APPLY_RESOLVED\" ] || [ -L \"$MEZ_APPLY_RESOLVED\" ]; then printf '%s\\n' {} >&2; return 1; fi",
                shell_quote(&change.path),
                shell_quote(&change.path),
                shell_quote(&format!("apply_patch: refusing to add existing path: {}", change.path))
            ));
        }
    }
    if let Some(bytes) = &change.final_bytes {
        lines.push("mkdir -p -- \"$(dirname -- \"$MEZ_APPLY_RESOLVED\")\" || return 1".to_string());
        lines.push(format!(
            "{new_var}=$(mktemp \"$(dirname -- \"$MEZ_APPLY_RESOLVED\")/.mez-apply-patch.XXXXXX\") || return 1"
        ));
        lines.push(format!(
            "{encoded_var}=$(mktemp) || {{ rm -f -- \"${new_var}\"; return 1; }}"
        ));
        lines.push(format!(
            "sed -n {} \"${{MEZ_APPLY_SIDECAR_FILE:-$0}}\" > \"${encoded_var}\" || {{ rm -f -- \"${new_var}\" \"${encoded_var}\"; return 1; }}",
            shell_quote(&format!(
                "s/^# __MEZ_INPUT_SIDECAR_V1__ {index} //p"
            ))
        ));
        lines.push(format!(
            "if [ ! -s \"${encoded_var}\" ]; then printf '%s\\n' {} >&2; rm -f -- \"${new_var}\" \"${encoded_var}\"; return 1; fi",
            shell_quote(&format!("apply_patch: missing final content sidecar: {}", change.path)),
        ));
        lines.push(format!(
            "if base64 -d < \"${encoded_var}\" > \"${new_var}\" 2>/dev/null; then MEZ_CONTENT_STATUS=0; else base64 -D < \"${encoded_var}\" > \"${new_var}\"; MEZ_CONTENT_STATUS=$?; fi; rm -f -- \"${encoded_var}\"; if [ \"$MEZ_CONTENT_STATUS\" != 0 ]; then rm -f -- \"${new_var}\"; return \"$MEZ_CONTENT_STATUS\"; fi"
        ));
        lines.push(format!(
            "MEZ_APPLY_FINAL_COUNT=$(wc -c < \"${new_var}\" | tr -d '[:space:]') || {{ rm -f -- \"${new_var}\"; return 1; }}; MEZ_APPLY_FINAL_DIGEST=$(mez_apply_patch_sha256 \"${new_var}\") || {{ rm -f -- \"${new_var}\"; return 1; }}; if [ \"$MEZ_APPLY_FINAL_COUNT\" != {} ] || [ \"$MEZ_APPLY_FINAL_DIGEST\" != {} ]; then printf '%s\\n' {} >&2; rm -f -- \"${new_var}\"; return 1; fi",
            bytes.len(),
            shell_quote(&content_sha256(bytes)),
            shell_quote(&format!("apply_patch: final content digest mismatch: {}", change.path)),
        ));
        let old_label = if original_is_regular {
            format!("a/{}", change.path)
        } else {
            "/dev/null".to_string()
        };
        let old_path = if original_is_regular {
            "\"$MEZ_APPLY_RESOLVED\"".to_string()
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
        lines.push(format!(
            "mez_apply_patch_resolve_checked {} {} || return 1",
            shell_quote(&change.path),
            shell_quote(&change.resolved_path)
        ));
        match &change.original {
            ApplyPatchOriginalState::Regular(bytes) => lines.push(format!(
                "mez_apply_patch_verify_regular {} {} {} {} || {{ rm -f -- \"${new_var}\"; return 1; }}",
                shell_quote(&change.resolved_path),
                bytes.len(),
                shell_quote(&content_sha256(bytes)),
                shell_quote(&change.path),
            )),
            ApplyPatchOriginalState::Missing => lines.push(format!(
                "if [ -e {} ] || [ -L {} ] || [ -e \"$MEZ_APPLY_RESOLVED\" ] || [ -L \"$MEZ_APPLY_RESOLVED\" ]; then printf '%s\\n' {} >&2; rm -f -- \"${new_var}\"; return 1; fi",
                shell_quote(&change.path),
                shell_quote(&change.path),
                shell_quote(&format!("apply_patch: refusing to add existing path: {}", change.path)),
            )),
        }
        lines.push(format!(
            "mez_apply_patch_resolve_checked {} {} || {{ rm -f -- \"${new_var}\"; return 1; }}",
            shell_quote(&change.path),
            shell_quote(&change.resolved_path)
        ));
        lines.push(format!(
            "mv -f -- \"${new_var}\" \"$MEZ_APPLY_RESOLVED\" || {{ rm -f -- \"${new_var}\"; return 1; }}"
        ));
    } else {
        lines.extend(unified_diff_lines(
            "apply patch",
            &format!("a/{}", change.path),
            "/dev/null",
            "\"$MEZ_APPLY_RESOLVED\"",
            &shell_quote("/dev/null"),
        ));
        lines.push(format!(
            "mez_apply_patch_resolve_checked {} {} || return 1",
            shell_quote(&change.path),
            shell_quote(&change.resolved_path)
        ));
        if let ApplyPatchOriginalState::Regular(bytes) = &change.original {
            lines.push(format!(
                "mez_apply_patch_verify_regular {} {} {} {} || return 1",
                shell_quote(&change.resolved_path),
                bytes.len(),
                shell_quote(&content_sha256(bytes)),
                shell_quote(&change.path),
            ));
        }
        lines.push(format!(
            "mez_apply_patch_resolve_checked {} {} || return 1",
            shell_quote(&change.path),
            shell_quote(&change.resolved_path)
        ));
        lines.push("rm -f -- \"$MEZ_APPLY_RESOLVED\" || return 1".to_string());
    }
    lines.push("}".to_string());
    lines.push(format!("{error_var}=$(mktemp) || exit 1"));
    lines.push(format!("{output_var}=$(mktemp) || exit 1"));
    lines.push(format!(
        "if {function_name} >\"${output_var}\" 2>\"${error_var}\"; then MEZ_APPLY_DIFF_COUNT=$(wc -c < \"${output_var}\" | tr -d '[:space:]') || exit 1; printf '%s %s %s %s\\n' {} {index} {} \"$MEZ_APPLY_DIFF_COUNT\"; cat \"${output_var}\"; printf '%s %s %s %s %s\\n' {} APPLIED {index} {} \"$MEZ_APPLY_DIFF_COUNT\"; else MEZ_APPLY_FAILED=1; cat \"${error_var}\" >&2; printf '%s %s %s %s %s\\n' {} FAILED {index} {} \"$(base64 <\"${error_var}\" | tr -d '\\n')\"; fi",
        shell_quote(APPLY_PATCH_DIFF_MARKER),
        shell_quote(&base64::engine::general_purpose::STANDARD.encode(change.path.as_bytes())),
        shell_quote(APPLY_PATCH_RESULT_MARKER),
        shell_quote(&base64::engine::general_purpose::STANDARD.encode(change.path.as_bytes())),
        shell_quote(APPLY_PATCH_RESULT_MARKER),
        shell_quote(&base64::engine::general_purpose::STANDARD.encode(change.path.as_bytes())),
    ));
    lines.push(format!("rm -f -- \"${error_var}\" \"${output_var}\""));
    lines.push(format!("unset -f {function_name} 2>/dev/null || :"));
    lines.join("\n") + "\n"
}
