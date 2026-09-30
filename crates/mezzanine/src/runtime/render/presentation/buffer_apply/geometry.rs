//! Shared pane geometry for transcript framing, prompt editing, and persistence.
//!
//! Layout authority remains with the runtime compositor. These methods consume
//! the current pane plan and configured column cap without caching another width
//! owner; missing panes fail before presentation can create a destination.

use super::*;

impl RuntimeSessionService {
    /// Returns the display cells available after the agent transcript gutter.
    pub(in crate::runtime::render::presentation) fn agent_terminal_markdown_frame_width(
        &self,
        pane_id: &str,
    ) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        Ok(bounded_agent_terminal_presentation_columns(
            columns,
            self.presentation.settings.terminal_agent_wrap_column_cap,
        )
        .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
        .max(1))
    }

    /// Returns bounded display cells available after the agent transcript gutter.
    pub(super) fn agent_terminal_markdown_terminal_width(&self, pane_id: &str) -> Result<usize> {
        self.agent_terminal_markdown_frame_width(pane_id)
    }

    /// Returns display cells available for editable pane-local prompt text.
    ///
    /// This width mirrors the terminal renderer, which draws the editable text
    /// after both the agent transcript gutter and the editable `❱ ` marker.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current presentation width bounds the prompt.
    pub(crate) fn agent_prompt_editable_body_width(&self, pane_id: &str) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        let prompt_prefix_width = UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX)
            .saturating_add(UnicodeWidthStr::width(AGENT_PROMPT_TEXT_PREFIX));
        Ok(columns.saturating_sub(prompt_prefix_width).max(1))
    }

    /// Returns the current pane presentation width in terminal display cells.
    pub(super) fn agent_terminal_presentation_columns(&self, pane_id: &str) -> Result<usize> {
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if let Some(columns) = self.agent_terminal_render_region_columns(pane_id) {
            return Ok(columns);
        }
        let columns = self
            .agent_pane_screen(pane_id)
            .map(|screen| screen.size().columns)
            .unwrap_or(descriptor.size.columns);
        Ok(usize::from(columns))
    }

    /// Returns the pane-local render width used by the terminal compositor.
    fn agent_terminal_render_region_columns(&self, pane_id: &str) -> Option<usize> {
        let window = self.session.active_window()?;
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let plan = self.window_presentation_plan(window)?;
        Some(usize::from(plan.pane(pane.index)?.content_size.columns))
    }

    /// Returns the pane width to persist with one agent presentation entry.
    pub(super) fn agent_presentation_terminal_width(&self, pane_id: &str) -> Option<u16> {
        self.agent_pane_screen(pane_id)
            .map(|screen| screen.size().columns)
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| descriptor.size.columns)
            })
    }
}
