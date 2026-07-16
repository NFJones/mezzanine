//! Runtime render copy-mode state transitions.
//!
//! This module owns keyboard copy-mode actions, copy-buffer writes, copy-mode
//! viewport sizing, and active copy-mode initialization. Keeping these state
//! transitions outside the render facade separates copy-mode behavior from
//! attached terminal step orchestration while preserving runtime-visible helper
//! methods used by commands, processes, and tests.

use super::*;

impl RuntimeSessionService {
    /// Runs the apply attached copy mode action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_attached_copy_mode_action(
        &mut self,
        action: CopyModeKeyAction,
    ) -> Result<bool> {
        let pane_id = self.active_pane_id()?;
        if self
            .presentation
            .copy
            .scrollback_copy_mode_panes
            .remove(pane_id.as_str())
        {
            self.presentation
                .copy
                .active_copy_modes
                .remove(pane_id.as_str());
            return Ok(true);
        }
        let mut should_exit = false;
        let mut copied = None;
        {
            let copy_mode = self.ensure_active_copy_mode(pane_id.as_str())?;
            match copy_mode.apply_key_action(action) {
                mez_mux::copy::CopyModeActionOutcome::SelectionReady => {
                    copied = Some(copy_mode.copy_selection()?);
                    copy_mode.clear_selection();
                }
                mez_mux::copy::CopyModeActionOutcome::Exit => should_exit = true,
                mez_mux::copy::CopyModeActionOutcome::Updated
                | mez_mux::copy::CopyModeActionOutcome::Ignored => {}
            }
        }
        if let Some(copied) = copied {
            let buffer_name = self
                .presentation
                .copy
                .active_paste_buffer
                .clone()
                .unwrap_or_else(|| "clipboard".to_string());
            self.copy_text_to_buffer_and_host_clipboard(
                buffer_name.as_str(),
                copied,
                format!("pane:{pane_id}:copy-mode"),
            )?;
        }
        if should_exit {
            self.presentation
                .copy
                .active_copy_modes
                .remove(pane_id.as_str());
            self.presentation
                .copy
                .scrollback_copy_mode_panes
                .remove(pane_id.as_str());
        }
        Ok(true)
    }

    /// Runs the copy text to buffer and host clipboard operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn copy_text_to_buffer_and_host_clipboard(
        &mut self,
        name: &str,
        content: String,
        origin: String,
    ) -> Result<()> {
        self.presentation.copy.paste_buffers.set_with_origin(
            name,
            content.as_str(),
            Some(origin),
        )?;
        let _ = self.presentation.copy.host_clipboard.copy(content.as_str());
        Ok(())
    }

    /// Runs the copy mode viewport rows for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn copy_mode_viewport_rows_for_pane(&self, pane_id: &str) -> usize {
        self.session
            .active_window()
            .and_then(|window| {
                window
                    .panes()
                    .iter()
                    .find(|pane| pane.id.as_str() == pane_id)
                    .and_then(|pane| self.copy_mode_overlay_region(window, pane.index))
            })
            .map(|(_, _, size)| usize::from(size.rows))
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| usize::from(descriptor.size.rows))
            })
            .unwrap_or_else(|| usize::from(self.session.authoritative_size.rows))
            .max(1)
    }

    /// Runs the ensure active copy mode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn ensure_active_copy_mode(
        &mut self,
        pane_id: &str,
    ) -> Result<&mut CopyMode> {
        if !self
            .presentation
            .copy
            .active_copy_modes
            .contains_key(pane_id)
        {
            let viewport_rows = self.copy_mode_viewport_rows_for_pane(pane_id);
            let screen = self.pane_screens.get(pane_id).ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane screen not found",
                )
            })?;
            let copy_mode = CopyMode::from_screen(screen, viewport_rows)?;
            self.presentation
                .copy
                .active_copy_modes
                .insert(pane_id.to_string(), copy_mode);
        }
        self.presentation
            .copy
            .active_copy_modes
            .get_mut(pane_id)
            .ok_or_else(|| MezError::invalid_state("active copy mode was not retained"))
    }
}
