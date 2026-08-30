//! Agent-prompt integration for runtime-owned external-editor sessions.
//!
//! This module snapshots one exact client's pane-local prompt, launches the
//! generic foreground editor target, and restores or replaces the prompt only
//! after the matching editor completion. Editor exit never submits a turn or
//! appends prompt history.

use super::{
    AgentShellVisibility, MezError, RenderInvalidationReason, Result, RuntimeAgentPromptInput,
    RuntimeSessionService,
};
use crate::runtime::{ExternalEditTarget, ExternalEditorCompletion};
use crate::ui::readline::ReadlineInputDecoder;

/// Maximum draft bytes accepted when returning external text to a prompt.
const EXTERNAL_AGENT_PROMPT_MAX_BYTES: u64 =
    mez_mux::readline::MAX_READLINE_HISTORY_ENTRY_BYTES as u64;

/// Exact prompt state retained while one editor owns the pane PTY.
#[derive(Debug, Clone)]
pub(super) struct RuntimeAgentPromptEditSnapshot {
    pub(super) client_id: mez_core::ids::ClientId,
    pub(super) session_id: String,
    pub(super) completion_nonce: String,
    pub(super) original_content: String,
    pub(super) prompt_input: RuntimeAgentPromptInput,
    pub(super) apply_on_success: bool,
}

/// Target-specific result used by lifecycle cleanup and retention policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime) enum ExternalPromptEditSettlement {
    /// The completion belonged to another target or prompt generation.
    Unhandled,
    /// The exact prompt snapshot was restored without applying editor content.
    Restored,
    /// Valid changed content was applied to the prompt.
    Applied,
}

impl RuntimeSessionService {
    /// Launches external editing for the focused visible agent prompt.
    pub(super) fn start_agent_prompt_external_edit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
    ) -> Result<bool> {
        let pane_id = self.active_pane_id()?;
        self.start_agent_prompt_external_edit_for_pane(primary_client_id, &pane_id, None, true)
    }

    /// Reopens retained text in a fresh editor session without applying it first.
    pub(in crate::runtime) fn reopen_agent_prompt_external_edit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        retained_content: String,
    ) -> Result<bool> {
        self.start_agent_prompt_external_edit_for_pane(
            primary_client_id,
            pane_id,
            Some(retained_content),
            false,
        )
    }

    pub(in crate::runtime) fn start_agent_prompt_external_edit_for_pane(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        initial_draft_content: Option<String>,
        apply_on_success: bool,
    ) -> Result<bool> {
        let visible = self
            .agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible);
        if !visible {
            return Err(MezError::conflict("agent prompt is not active"));
        }
        if self.presentation.primary_prompt_input.is_some()
            || self.presentation.primary_display_overlay.is_some()
            || self.presentation.pane_agent_status_selector.is_some()
        {
            return Err(MezError::conflict(
                "another prompt or selector currently owns input",
            ));
        }
        if self.agent_shell_pane_has_active_turn(pane_id) {
            return Err(MezError::conflict(
                "external prompt editing is unavailable while an agent turn is active",
            ));
        }
        let prompt_input = self
            .agent_prompt_input_for_client(primary_client_id, pane_id)
            .unwrap_or_else(super::default_runtime_agent_prompt_input);
        let draft = prompt_input.prompt.buffer.draft_snapshot();
        let original_content = mez_mux::readline::ReadlineBuffer::expanded_draft(&draft);
        if original_content.len() as u64 > EXTERNAL_AGENT_PROMPT_MAX_BYTES {
            return Err(MezError::invalid_args(
                "agent prompt is too large for external editing",
            ));
        }
        let started = self.start_external_editor_session(
            primary_client_id,
            pane_id,
            ExternalEditTarget::AgentPrompt,
            original_content.clone(),
            initial_draft_content.unwrap_or_else(|| original_content.clone()),
            apply_on_success,
        )?;
        self.presentation.external_agent_prompt_edits.insert(
            pane_id.to_string(),
            RuntimeAgentPromptEditSnapshot {
                client_id: primary_client_id.clone(),
                session_id: started.session_id,
                completion_nonce: started.completion_nonce,
                original_content,
                prompt_input,
                apply_on_success,
            },
        );
        self.sync_tracked_pty_sizes()?;
        Ok(true)
    }

    /// Restores one exact prompt snapshot after its editor lease aborts.
    ///
    /// Draft content is never applied on this path. Detached clients lose
    /// their obsolete snapshot, while an attached owner regains the complete
    /// prompt state captured before editor launch.
    pub(in crate::runtime) fn settle_agent_prompt_external_edit_abort(
        &mut self,
        completion: &ExternalEditorCompletion,
    ) -> Result<bool> {
        if !matches!(completion.target, ExternalEditTarget::AgentPrompt) {
            return Ok(false);
        }
        let matches = self
            .presentation
            .external_agent_prompt_edits
            .get(&completion.pane_id)
            .is_some_and(|snapshot| {
                snapshot.session_id == completion.session_id
                    && snapshot.completion_nonce == completion.completion_nonce
            });
        if !matches {
            return Ok(false);
        }
        let snapshot = self
            .presentation
            .external_agent_prompt_edits
            .remove(&completion.pane_id)
            .expect("matching prompt edit snapshot was checked above");
        if self.session.is_attached_primary(&snapshot.client_id) {
            self.set_agent_prompt_input_for_client(
                &snapshot.client_id,
                &completion.pane_id,
                snapshot.prompt_input,
            );
        }
        Ok(true)
    }

    /// Applies one matching prompt completion or restores the exact snapshot.
    pub(in crate::runtime) fn settle_agent_prompt_external_edit(
        &mut self,
        completion: &ExternalEditorCompletion,
    ) -> Result<ExternalPromptEditSettlement> {
        if !matches!(completion.target, ExternalEditTarget::AgentPrompt) {
            return Ok(ExternalPromptEditSettlement::Unhandled);
        }
        let matches = self
            .presentation
            .external_agent_prompt_edits
            .get(&completion.pane_id)
            .is_some_and(|snapshot| {
                snapshot.session_id == completion.session_id
                    && snapshot.completion_nonce == completion.completion_nonce
            });
        if !matches {
            return Ok(ExternalPromptEditSettlement::Unhandled);
        }
        let snapshot = self
            .presentation
            .external_agent_prompt_edits
            .remove(&completion.pane_id)
            .expect("matching prompt edit snapshot was checked above");
        let mut restored = snapshot.prompt_input;
        let mut settlement = ExternalPromptEditSettlement::Restored;
        if snapshot.apply_on_success
            && completion.exit_code == 0
            && let Some(edited) = completion.validated_content.clone()
        {
            let edited = normalize_external_agent_prompt(edited);
            if edited != snapshot.original_content {
                restored.prompt.clear_transient_editing_state();
                restored.prompt.buffer.set_line(edited);
                restored.decoder = ReadlineInputDecoder::new();
                restored.pending_ctrl_c_exit_at_unix_ms = None;
                settlement = ExternalPromptEditSettlement::Applied;
            }
        }
        self.set_agent_prompt_input_for_client(&snapshot.client_id, &completion.pane_id, restored);
        self.sync_tracked_pty_sizes()?;
        let render_effects = self.render_effects_for_primary_projection(
            &snapshot.client_id,
            RenderInvalidationReason::FullRedraw,
        );
        self.presentation.defer_render_effects(render_effects);
        Ok(settlement)
    }
}

/// Removes one conventional editor-added final newline and preserves all others.
pub(in crate::runtime) fn normalize_external_agent_prompt(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies one final editor newline is removed without collapsing an
    /// intentional additional trailing blank line.
    #[test]
    fn prompt_editor_newline_normalization_removes_exactly_one_line_ending() {
        assert_eq!(
            normalize_external_agent_prompt("text\n".to_string()),
            "text"
        );
        assert_eq!(
            normalize_external_agent_prompt("text\r\n".to_string()),
            "text"
        );
        assert_eq!(
            normalize_external_agent_prompt("text\n\n".to_string()),
            "text\n"
        );
        assert_eq!(normalize_external_agent_prompt(String::new()), "");
    }
}
