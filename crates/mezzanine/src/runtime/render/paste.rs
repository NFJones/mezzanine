//! Runtime render paste helpers.
//!
//! This module owns readline paste framing and executes mux-selected paste
//! sources through product prompt or pane-input adapters. Host clipboard reads
//! remain product I/O while deterministic source precedence belongs to mux.

use super::{
    AgentShellVisibility, ClipboardPasteSource, ClipboardPasteSourceKind, EventKind,
    PaneDescriptor, Result, RuntimeSessionService, json_escape, runtime_paste_bytes,
    select_clipboard_paste_source,
};

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

impl RuntimeSessionService {
    /// Runs the paste most recent buffer to active pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn paste_most_recent_buffer_to_active_pane(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
    ) -> Result<bool> {
        let Some(source) = self.most_recent_paste_buffer_source() else {
            return Ok(false);
        };
        let descriptor = self.active_window_pane_descriptor(None)?;
        self.paste_source_to_pane(primary_client_id, &descriptor, source)
    }

    /// Runs the paste clipboard or most recent buffer to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn paste_clipboard_or_most_recent_buffer_to_pane(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        descriptor: &PaneDescriptor,
    ) -> Result<bool> {
        let Some(source) = self.clipboard_or_most_recent_paste_source() else {
            return Ok(false);
        };
        self.paste_source_to_pane(primary_client_id, descriptor, source)
    }

    /// Pastes clipboard or paste-buffer content into active prompt text when
    /// one is visible, otherwise into the clicked pane.
    pub(super) fn paste_clipboard_or_most_recent_buffer_to_text_entry_or_pane(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        descriptor: &PaneDescriptor,
        queue_for_adapter: bool,
    ) -> Result<bool> {
        let Some(source) = self.clipboard_or_most_recent_paste_source() else {
            return Ok(false);
        };
        self.paste_source_to_text_entry_or_pane(
            primary_client_id,
            descriptor,
            source,
            queue_for_adapter,
        )
    }

    /// Routes one paste source to a prompt text entry or a pane PTY.
    pub(super) fn paste_source_to_text_entry_or_pane(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        descriptor: &PaneDescriptor,
        source: ClipboardPasteSource,
        queue_for_adapter: bool,
    ) -> Result<bool> {
        let paste_bytes = runtime_readline_paste_bytes(source.content());
        if self.presentation.primary_prompt_input.is_some() {
            return self.apply_primary_prompt_input(
                primary_client_id,
                &paste_bytes,
                queue_for_adapter,
            );
        }
        if self
            .agent_shell_store()
            .get(descriptor.pane_id.as_str())
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible)
        {
            return self.apply_attached_agent_prompt_input_for_pane(
                primary_client_id,
                descriptor.pane_id.as_str(),
                &paste_bytes,
            );
        }
        self.paste_source_to_pane(primary_client_id, descriptor, source)
    }

    /// Runs the clipboard or most recent paste source operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn clipboard_or_most_recent_paste_source(&self) -> Option<ClipboardPasteSource> {
        select_clipboard_paste_source(
            self.presentation.copy.host_clipboard.read(),
            self.most_recent_paste_buffer_candidate(),
        )
    }

    /// Runs the most recent paste buffer source operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn most_recent_paste_buffer_source(&self) -> Option<ClipboardPasteSource> {
        select_clipboard_paste_source(None, self.most_recent_paste_buffer_candidate())
    }

    /// Reads the most recent mux paste-buffer value for pure source selection.
    fn most_recent_paste_buffer_candidate(&self) -> Option<(String, String)> {
        let buffer_name = self
            .presentation
            .copy
            .paste_buffers
            .most_recent_name()?
            .to_string();
        let content = self
            .presentation
            .copy
            .paste_buffers
            .get(&buffer_name)?
            .to_string();
        Some((buffer_name, content))
    }

    /// Runs the paste source to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn paste_source_to_pane(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        descriptor: &PaneDescriptor,
        source: ClipboardPasteSource,
    ) -> Result<bool> {
        let paste_bytes = runtime_paste_bytes(
            self.pane_screen(descriptor.pane_id.as_str()),
            source.content(),
        );
        let dispatch = self.write_input_to_pane(
            primary_client_id,
            Some(descriptor.pane_id.as_str()),
            &paste_bytes,
        )?;
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","paste_source":"{}","paste_buffer":{},"input_bytes":{}}}"#,
                json_escape(&dispatch.pane_id),
                match source.kind() {
                    ClipboardPasteSourceKind::Host => "host-clipboard",
                    ClipboardPasteSourceKind::PasteBuffer { .. } => "paste-buffer",
                },
                match source.kind() {
                    ClipboardPasteSourceKind::Host => "null".to_string(),
                    ClipboardPasteSourceKind::PasteBuffer { name } => {
                        format!(r#""{}""#, json_escape(name))
                    }
                },
                dispatch.bytes_written
            ),
        )?;
        Ok(true)
    }
}
