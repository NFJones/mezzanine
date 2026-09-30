//! Installation of provider-only screens with independently owned shell previews.
//!
//! Composition uses the current conversation and exact installed-screen lineage.
//! Provider and preview metadata are updated only after one atomic screen install;
//! stale preview ownership is discarded without erasing intervening durable rows.

use super::*;

impl RuntimeSessionService {
    /// Installs one provider-only screen with active shell previews recomposed.
    ///
    /// The provider and shell-preview states record the same exact composite
    /// generation so delayed updates from either owner are fenced atomically.
    pub(super) fn update_agent_streaming_screen(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        provider_screen: TerminalScreen,
    ) -> Result<u64> {
        let current_lineage = self
            .agent_pane_screen_lineage(pane_id, conversation_id)
            .ok_or_else(|| {
                MezError::invalid_state("streaming presentation lineage was not initialized")
            })?;
        let mut preview_presentation = self
            .presentation
            .agent_shell_output_previews
            .get(pane_id)
            .cloned()
            .filter(|preview| {
                preview.conversation_id == conversation_id
                    && preview.installed_lineage == current_lineage
            });
        if let Some(preview) = preview_presentation.as_mut() {
            Self::retire_settled_agent_shell_previews(preview);
            if preview.previews.is_empty() {
                preview_presentation = None;
            }
        }
        let mut composite_screen = provider_screen.clone();
        let mut transient_rows = 0;
        if let Some(preview) = preview_presentation.as_ref() {
            let ui_theme = self.presentation.settings.ui_theme.clone();
            let max_preview_rows = self.terminal_shell_output_preview_lines();
            transient_rows = Self::append_agent_shell_previews_to_screen(
                &mut composite_screen,
                &preview.previews,
                &ui_theme,
                max_preview_rows,
                self.presentation.settings.terminal_agent_wrap_column_cap,
            )?;
        }
        let installed_lineage = self
            .update_agent_pane_screen_preserving_interaction(
                pane_id,
                conversation_id,
                composite_screen,
            )
            .ok_or_else(|| {
                MezError::invalid_state("streaming presentation screen conversation changed")
            })?;
        self.presentation
            .agent_shell_output_previews
            .remove(pane_id);
        if let Some(mut preview) = preview_presentation {
            preview.installed_lineage = installed_lineage;
            preview.baseline_screen = std::sync::Arc::new(provider_screen.clone());
            preview.transient_rows = transient_rows;
            self.presentation
                .agent_shell_output_previews
                .insert(pane_id.to_string(), preview);
        }
        if let Some(presentation) = self
            .presentation
            .agent_streaming_say_presentations
            .get_mut(pane_id)
            .filter(|presentation| presentation.conversation_id == conversation_id)
        {
            presentation.provider_screen = std::sync::Arc::new(provider_screen);
            presentation.installed_lineage = installed_lineage;
            if presentation.projected_lineage.is_some() {
                presentation.projected_lineage = Some(installed_lineage);
            }
        }
        Ok(installed_lineage)
    }
}
