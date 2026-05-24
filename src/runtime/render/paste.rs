//! Runtime render paste helpers.
//!
//! This module owns paste-source metadata and readline paste framing used by
//! the runtime render/input layer. RuntimeSessionService remains responsible
//! for choosing the paste source and routing the resulting input bytes.

/// Source metadata for paste operations routed through runtime render input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimePasteSource {
    /// Human-readable paste source label.
    pub(super) label: String,
    /// Optional named paste buffer source.
    pub(super) buffer_name: Option<String>,
    /// Text content to paste.
    pub(super) content: String,
}

/// Wraps pasted text for the readline decoder as one bracketed-paste payload.
///
/// # Parameters
/// - `content`: Plain text paste content.
pub(super) fn runtime_readline_paste_bytes(content: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(content.len().saturating_add(12));
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(content.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}
