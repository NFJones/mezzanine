//! Shell-output transport decoding for agent actions.
//!
//! Pane shell transactions can wrap command output in Mezzanine base64 markers
//! so model-facing action results receive clean UTF-8 text rather than shell
//! wrapper scaffolding. This module keeps that decoding separate from turn
//! execution and action-result construction.

use base64::Engine;

const OUTSIDE_FRAME_PREVIEW_LIMIT: usize = 160;

/// Marker that begins one base64-encoded shell-output transport block.
pub const SHELL_OUTPUT_BASE64_BEGIN_MARKER: &str = "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__";
/// Marker that ends one base64-encoded shell-output transport block.
pub const SHELL_OUTPUT_BASE64_END_MARKER: &str = "__MEZ_SHELL_OUTPUT_BASE64_END__";
/// Marker that reports raw bytes dropped before base64 output emission.
pub const SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER: &str =
    "__MEZ_SHELL_OUTPUT_BASE64_DROPPED_BYTES__";

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
    /// Number of user-visible output bytes dropped after hitting a capture cap.
    pub output_bytes_dropped: usize,
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
        self.missing_end_marker
            || self.partial_base64_bytes_dropped > 0
            || self.output_bytes_dropped > 0
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
            "output_bytes_dropped": self.output_bytes_dropped,
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
        if !in_block && let Some(dropped) = parse_dropped_bytes_marker(marker_candidate) {
            result.diagnostics.output_bytes_dropped = result
                .diagnostics
                .output_bytes_dropped
                .saturating_add(dropped);
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

/// Parses the optional dropped-byte marker emitted after a bounded transport frame.
fn parse_dropped_bytes_marker(line: &str) -> Option<usize> {
    let value = line
        .strip_prefix(SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER)?
        .trim();
    value.parse::<usize>().ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verifies partial base64 quartets are counted as structured diagnostics
    /// while preserving the complete retained prefix.
    fn shell_output_transport_counts_partial_base64_bytes() {
        let stdout = format!("{SHELL_OUTPUT_BASE64_BEGIN_MARKER}\nZm9vY\n");
        let decoded = decode_shell_output_transport_with_diagnostics(&stdout);

        assert_eq!(decoded.output, "foo");
        assert_eq!(decoded.diagnostics.partial_base64_bytes_dropped, 1);
        assert!(decoded.diagnostics.missing_end_marker);
    }

    #[test]
    /// Verifies truncated encoded shell-output observations keep diagnostics
    /// separate from decoded command output.
    fn shell_output_transport_decodes_complete_prefix_when_truncated() {
        let stdout = format!("{SHELL_OUTPUT_BASE64_BEGIN_MARKER}\nZm9v\n");
        let decoded = decode_shell_output_transport_with_diagnostics(&stdout);

        assert_eq!(decoded.output, "foo");
        assert!(decoded.diagnostics.missing_end_marker);
        assert!(decoded.diagnostics.output_truncated());
    }

    #[test]
    /// Verifies shell-output transport decoding discards Mezzanine wrapper echo
    /// around an encoded child-output block.
    ///
    /// Plain commands can print large text, and the model must see only command
    /// output rather than transaction scaffolding used to drive the pane shell.
    fn shell_output_transport_discards_wrapper_echo_around_encoded_output() {
        let stdout = format!(
            "stty -echo\n{SHELL_OUTPUT_BASE64_BEGIN_MARKER}\nQXBhY2hlIExpY2Vuc2UKVmVyc2lvbiAyLjAK\n{SHELL_OUTPUT_BASE64_END_MARKER}\n}}\n"
        );

        let decoded = decode_shell_output_transport(&stdout);

        assert_eq!(decoded, "Apache License\nVersion 2.0\n");
        assert!(!decoded.contains("stty"), "{decoded:?}");
    }

    #[test]
    /// Verifies dropped-byte markers become structured truncation diagnostics
    /// rather than command output or wrapper noise.
    fn shell_output_transport_records_dropped_byte_marker_as_truncation() {
        let stdout = format!(
            "{SHELL_OUTPUT_BASE64_BEGIN_MARKER}\nZm9v\n{SHELL_OUTPUT_BASE64_END_MARKER}\n{SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER} 17\n"
        );

        let decoded = decode_shell_output_transport_with_diagnostics(&stdout);

        assert_eq!(decoded.output, "foo");
        assert_eq!(decoded.diagnostics.output_bytes_dropped, 17);
        assert!(decoded.diagnostics.output_truncated());
        assert_eq!(decoded.diagnostics.outside_frame_bytes, 0);
    }

    #[test]
    /// Verifies raw text after an encoded block is reported as transport
    /// diagnostics rather than command output.
    fn shell_output_transport_records_non_wrapper_tail_after_encoded_output_as_diagnostics() {
        let stdout = format!(
            "{SHELL_OUTPUT_BASE64_BEGIN_MARKER}\nZmlyc3QgbGluZQo=\n{SHELL_OUTPUT_BASE64_END_MARKER}\n}}\nfinal output\n"
        );

        let decoded = decode_shell_output_transport_with_diagnostics(&stdout);

        assert_eq!(decoded.output, "first line\n");
        assert!(
            decoded.diagnostics.outside_frame_bytes >= "final output\n".len(),
            "{:?}",
            decoded.diagnostics
        );
        assert!(
            decoded
                .diagnostics
                .outside_frame_preview
                .as_deref()
                .unwrap_or_default()
                .contains("final output\\n"),
            "{:?}",
            decoded.diagnostics
        );
    }

    #[test]
    /// Verifies wrapper-only leakage produces empty output and structured
    /// missing-frame diagnostics so empty-output fallbacks remain available.
    fn shell_output_transport_records_wrapper_only_output_as_missing_frame() {
        let decoded = decode_shell_output_transport_with_diagnostics("else\nfi\nTE_STATUS=1; fi\n");

        assert_eq!(decoded.output, "");
        assert!(decoded.diagnostics.missing_frame);
        assert!(decoded.diagnostics.transport_incomplete());
        assert!(decoded.diagnostics.outside_frame_bytes > 0);
    }

    #[test]
    /// Verifies missing base64 end markers are reported as structured transport
    /// diagnostics rather than command-output text.
    fn shell_output_transport_reports_missing_end_marker_as_diagnostics() {
        let stdout = format!("{SHELL_OUTPUT_BASE64_BEGIN_MARKER}\nZm9v\n");

        let decoded = decode_shell_output_transport_with_diagnostics(&stdout);

        assert_eq!(decoded.output, "foo");
        assert!(decoded.diagnostics.missing_end_marker);
        assert!(decoded.diagnostics.transport_incomplete());
        assert!(decoded.diagnostics.output_truncated());
    }
}
