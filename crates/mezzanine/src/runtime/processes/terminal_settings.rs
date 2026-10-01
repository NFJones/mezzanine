//! Terminal process configuration applied to the existing runtime state.
//!
//! History changes validate before settings publication. Width changes rebuild
//! process, agent, and transaction screens together; no parallel policy owner
//! or independently mutable presentation store is introduced.

use super::{PaneSpawnPolicy, Result, RuntimeProcessSettings, RuntimeSessionService};

impl RuntimeSessionService {
    /// Returns the active pane-screen history limit.
    pub(crate) fn terminal_history_limit(&self) -> usize {
        self.process.settings.terminal_history_limit
    }

    /// Returns the active pane-screen history rotation batch size.
    pub(crate) fn terminal_history_rotate_lines(&self) -> usize {
        self.process.settings.terminal_history_rotate_lines
    }

    /// Installs history policy on an isolated presentation projection service.
    pub(crate) fn configure_agent_projection_history_policy(
        &mut self,
        history_limit: usize,
        history_rotate_lines: usize,
    ) -> Result<()> {
        self.configure_pane_screen_history(history_limit, history_rotate_lines)?;
        self.process.settings.terminal_history_limit = history_limit;
        self.process.settings.terminal_history_rotate_lines = history_rotate_lines;
        Ok(())
    }

    /// Returns the TERM value exported to pane processes and clients.
    pub(crate) fn terminal_term(&self) -> &str {
        &self.process.settings.terminal_term
    }

    /// Returns the configured transient shell-output visual-row tail count.
    pub(crate) fn terminal_shell_output_preview_lines(&self) -> usize {
        self.process.settings.terminal_shell_output_preview_lines
    }

    /// Applies one parsed generation of terminal process settings.
    pub(crate) fn apply_process_terminal_settings(
        &mut self,
        history_limit: usize,
        history_rotate_lines: usize,
        terminal_term: String,
        pane_spawn_policy: PaneSpawnPolicy,
        terminal_emoji_width: mez_terminal::TerminalEmojiWidth,
        shell_output_preview_lines: usize,
    ) -> Result<()> {
        self.configure_pane_screen_history(history_limit, history_rotate_lines)?;
        self.presentation
            .clear_agent_presentation_replay_cache_entries();
        let emoji_width_changed =
            self.process.settings.terminal_emoji_width != terminal_emoji_width;
        self.process.settings = RuntimeProcessSettings {
            terminal_history_limit: history_limit,
            terminal_history_rotate_lines: history_rotate_lines,
            terminal_term,
            pane_spawn_policy,
            terminal_emoji_width,
            terminal_shell_output_preview_lines: shell_output_preview_lines,
        };
        mez_terminal::set_terminal_emoji_width(terminal_emoji_width);
        if emoji_width_changed {
            for screen in self.process.process_pane_screens.values_mut() {
                screen.rebuild_for_width_policy_change(terminal_emoji_width);
            }
            for agent_screen in self.process.agent_pane_screens.values_mut() {
                agent_screen
                    .screen_mut()
                    .rebuild_for_width_policy_change(terminal_emoji_width);
            }
            for screen in self.process.pane_transaction_osc_screens.values_mut() {
                screen.rebuild_for_width_policy_change(terminal_emoji_width);
            }
        }
        Ok(())
    }
}
