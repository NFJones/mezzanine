//! Shell-output transport decoding for agent actions.
//!
//! Pane shell transactions can wrap command output in Mezzanine base64 markers
//! so model-facing action results receive clean UTF-8 text rather than shell
//! wrapper scaffolding. This module keeps that decoding separate from turn
//! execution and action-result construction.

use super::super::shell::{SHELL_OUTPUT_BASE64_BEGIN_MARKER, SHELL_OUTPUT_BASE64_END_MARKER};
use base64::Engine;

const OUTSIDE_FRAME_PREVIEW_LIMIT: usize = 160;

/// Decoded shell-output transport payload plus integrity diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellTransportDecodeResult {
    /// UTF-8 command output decoded from base64 transport frames.
    pub output: String,
    /// Structured transport state kept separate from command output.
    pub diagnostics: ShellTransportDiagnostics,
}

/// Structured integrity state for one shell-output transport decode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellTransportDiagnostics {
    /// Bytes of non-whitespace text observed outside transport frames.
    pub outside_frame_bytes: usize,
    /// Bounded sanitized preview of non-whitespace text observed outside frames.
    pub outside_frame_preview: Option<String>,
    /// Whether a transport begin marker was observed.
    pub saw_begin_marker: bool,
    /// Whether a transport end marker was observed.
    pub saw_end_marker: bool,
    /// Whether non-empty output was observed without any begin marker.
    pub missing_frame: bool,
    /// Whether a begin marker was observed without a matching end marker.
    pub missing_end_marker: bool,
    /// Number of base64 blocks that failed to decode.
    pub invalid_base64_blocks: usize,
    /// Number of decoded blocks that contained non-UTF-8 bytes.
    pub non_utf8_blocks: usize,
    /// Number of trailing base64 bytes dropped from partial frames.
    pub partial_base64_bytes_dropped: usize,
}

impl ShellTransportDiagnostics {
    /// Reports whether the transport framing made the observation incomplete.
    pub fn transport_incomplete(&self) -> bool {
        self.missing_frame
            || self.missing_end_marker
            || self.invalid_base64_blocks > 0
            || self.partial_base64_bytes_dropped > 0
    }

    /// Reports whether user-visible command output may have been truncated.
    pub fn output_truncated(&self) -> bool {
        self.missing_end_marker || self.partial_base64_bytes_dropped > 0
    }

    /// Converts diagnostics into structured action-result metadata.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "outside_frame_bytes": self.outside_frame_bytes,
            "outside_frame_preview": self.outside_frame_preview,
            "saw_begin_marker": self.saw_begin_marker,
            "saw_end_marker": self.saw_end_marker,
            "missing_frame": self.missing_frame,
            "missing_end_marker": self.missing_end_marker,
            "invalid_base64_blocks": self.invalid_base64_blocks,
            "non_utf8_blocks": self.non_utf8_blocks,
            "partial_base64_bytes_dropped": self.partial_base64_bytes_dropped,
        })
    }
}

/// Decodes shell-output transport blocks emitted by non-stateful transactions.
///
/// # Parameters
/// - `stdout`: Bounded raw PTY observation for one shell transaction.
pub fn decode_shell_output_transport(stdout: &str) -> String {
    decode_shell_output_transport_with_diagnostics(stdout).output
}

/// Decodes shell-output transport blocks and returns separated diagnostics.
///
/// Only marker-bounded base64 payload becomes command output. Text outside
/// frames is counted as transport noise so wrapper/control leakage cannot be
/// mistaken for stdout or stderr.
///
/// # Parameters
/// - `stdout`: Bounded raw PTY observation for one shell transaction.
pub fn decode_shell_output_transport_with_diagnostics(stdout: &str) -> ShellTransportDecodeResult {
    decode_base64_transport_output(
        stdout,
        SHELL_OUTPUT_BASE64_BEGIN_MARKER,
        SHELL_OUTPUT_BASE64_END_MARKER,
    )
}

/// Decodes one marker-bounded base64 transport stream.
fn decode_base64_transport_output(
    stdout: &str,
    begin: &str,
    end: &str,
) -> ShellTransportDecodeResult {
    let normalized = stdout.replace("\r\n", "\n").replace('\r', "\n");
    let mut result = ShellTransportDecodeResult {
        output: String::new(),
        diagnostics: ShellTransportDiagnostics::default(),
    };
    let mut block = String::new();
    let mut in_block = false;
    for line in normalized.split_inclusive('\n') {
        let marker_candidate = line.trim_end_matches('\n');
        if marker_candidate == begin {
            result.diagnostics.saw_begin_marker = true;
            in_block = true;
            block.clear();
            continue;
        }
        if marker_candidate == end {
            if in_block {
                result.diagnostics.saw_end_marker = true;
                result.output.push_str(&decode_base64_transport_block(
                    &block,
                    false,
                    &mut result.diagnostics,
                ));
                in_block = false;
                block.clear();
            } else {
                record_outside_frame_text(&mut result.diagnostics, line);
            }
            continue;
        }
        if in_block {
            block.push_str(marker_candidate.trim());
        } else {
            record_outside_frame_text(&mut result.diagnostics, line);
        }
    }
    if in_block {
        result.diagnostics.missing_end_marker = true;
        result.output.push_str(&decode_base64_transport_block(
            &block,
            true,
            &mut result.diagnostics,
        ));
    }
    if !result.diagnostics.saw_begin_marker && !normalized.trim().is_empty() {
        result.diagnostics.missing_frame = true;
    }
    result
}

/// Records non-whitespace text observed outside a base64 transport frame.
fn record_outside_frame_text(diagnostics: &mut ShellTransportDiagnostics, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    diagnostics.outside_frame_bytes = diagnostics.outside_frame_bytes.saturating_add(text.len());
    append_outside_frame_preview(diagnostics, text);
}

/// Appends bounded diagnostic preview text without changing command output.
fn append_outside_frame_preview(diagnostics: &mut ShellTransportDiagnostics, text: &str) {
    let preview = diagnostics
        .outside_frame_preview
        .get_or_insert_with(String::new);
    let remaining = OUTSIDE_FRAME_PREVIEW_LIMIT.saturating_sub(preview.len());
    if remaining == 0 {
        return;
    }
    for character in text.chars() {
        let escaped = character.escape_default().to_string();
        if escaped.len() > OUTSIDE_FRAME_PREVIEW_LIMIT.saturating_sub(preview.len()) {
            break;
        }
        preview.push_str(&escaped);
    }
}

/// Decodes one base64 transport block into UTF-8 model-facing text.
fn decode_base64_transport_block(
    block: &str,
    partial: bool,
    diagnostics: &mut ShellTransportDiagnostics,
) -> String {
    let mut cleaned = block
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    if partial {
        let full_quartets = cleaned.len() - (cleaned.len() % 4);
        diagnostics.partial_base64_bytes_dropped = diagnostics
            .partial_base64_bytes_dropped
            .saturating_add(cleaned.len().saturating_sub(full_quartets));
        cleaned.truncate(full_quartets);
    }
    if cleaned.is_empty() {
        return String::new();
    }
    match base64::engine::general_purpose::STANDARD.decode(cleaned.as_bytes()) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                diagnostics.non_utf8_blocks = diagnostics.non_utf8_blocks.saturating_add(1);
                let bytes = error.into_bytes();
                String::from_utf8_lossy(&bytes).into_owned()
            }
        },
        Err(_) => {
            diagnostics.invalid_base64_blocks = diagnostics.invalid_base64_blocks.saturating_add(1);
            String::new()
        }
    }
}
