//! Process and conversation-bound terminal screen ownership.
//!
//! Screen state remains in RuntimeSessionService. Conversation identity and
//! lineage fence delayed agent projections; synchronized-output release and
//! history pruning retain their existing render and copy invalidation ordering.

use super::*;

impl RuntimeSessionService {
    /// Returns all modeled pane screens for whole-layout presentation.
    pub(crate) fn process_pane_screens(
        &self,
    ) -> &std::collections::BTreeMap<String, TerminalScreen> {
        &self.process.process_pane_screens
    }

    /// Returns the authoritative process terminal screen for one pane.
    pub(crate) fn process_pane_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        self.process.process_pane_screens.get(pane_id)
    }

    /// Returns mutable authoritative process terminal state for one pane.
    pub(crate) fn process_pane_screen_mut(&mut self, pane_id: &str) -> Option<&mut TerminalScreen> {
        self.process.process_pane_screens.get_mut(pane_id)
    }

    /// Force-releases one matching synchronized terminal transaction after its recovery timeout.
    pub(crate) fn apply_synchronized_output_timeout_transition(
        &mut self,
        pane_id: &str,
        begin_epoch: u64,
    ) -> RuntimeTransition {
        let released = self.process_pane_screen_mut(pane_id).is_some_and(|screen| {
            screen.synchronized_output_begin_epoch() == Some(begin_epoch)
                && screen.force_release_synchronized_output()
        });
        self.runtime_pane_transition_with_render(
            pane_id,
            released,
            Some(RenderInvalidationReason::FullRedraw),
        )
    }

    /// Releases a pane synchronized-output transaction before a lifecycle mutation.
    pub(crate) fn force_release_pane_synchronized_output(&mut self, pane_id: &str) -> bool {
        self.process_pane_screen_mut(pane_id)
            .is_some_and(TerminalScreen::force_release_synchronized_output)
    }

    /// Replaces the authoritative process terminal screen for one pane.
    #[allow(
        dead_code,
        reason = "explicit process-screen fixture API used by test targets"
    )]
    pub(crate) fn set_process_pane_screen(
        &mut self,
        pane_id: impl Into<String>,
        screen: TerminalScreen,
    ) {
        self.process
            .process_pane_screens
            .insert(pane_id.into(), screen);
    }

    /// Returns the conversation-bound agent screen state for one pane.
    #[allow(dead_code)]
    pub(crate) fn agent_pane_screen_state(&self, pane_id: &str) -> Option<&AgentPaneScreen> {
        self.process.agent_pane_screens.get(pane_id)
    }

    /// Returns the retained agent terminal screen for one pane.
    #[allow(dead_code)]
    pub(crate) fn agent_pane_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        self.agent_pane_screen_state(pane_id)
            .map(AgentPaneScreen::screen)
    }

    /// Returns the lineage when one pane still belongs to the requested conversation.
    pub(crate) fn agent_pane_screen_lineage(
        &self,
        pane_id: &str,
        conversation_id: &str,
    ) -> Option<u64> {
        self.agent_pane_screen_state(pane_id)
            .filter(|state| state.conversation_id() == conversation_id)
            .map(AgentPaneScreen::lineage)
    }

    /// Returns mutable retained agent terminal state for one pane.
    #[allow(dead_code)]
    pub(crate) fn agent_pane_screen_mut(&mut self, pane_id: &str) -> Option<&mut TerminalScreen> {
        self.process.next_agent_pane_screen_lineage = self
            .process
            .next_agent_pane_screen_lineage
            .saturating_add(1)
            .max(1);
        let lineage = self.process.next_agent_pane_screen_lineage;
        self.process
            .agent_pane_screens
            .get_mut(pane_id)
            .map(|state| {
                state.lineage = lineage;
                state.screen_mut()
            })
    }

    /// Replaces one pane's retained agent screen with a conversation-bound value.
    pub(crate) fn set_agent_pane_screen(
        &mut self,
        pane_id: impl Into<String>,
        conversation_id: impl Into<String>,
        screen: TerminalScreen,
    ) {
        let pane_id = pane_id.into();
        self.reset_steering_presentation_surface(&pane_id);
        self.process.next_agent_pane_screen_lineage = self
            .process
            .next_agent_pane_screen_lineage
            .saturating_add(1)
            .max(1);
        self.clear_interaction_state_for_surface(&pane_id, PaneSurfaceKind::Agent);
        self.process.agent_pane_screens.insert(
            pane_id,
            AgentPaneScreen {
                conversation_id: conversation_id.into(),
                lineage: self.process.next_agent_pane_screen_lineage,
                screen,
            },
        );
    }

    /// Replaces a current conversation's agent screen without interrupting its interaction state.
    ///
    /// Returns the newly installed lineage, or `None` when the pane has no
    /// retained agent screen for the requested conversation. Delayed projections
    /// can retain this token without comparing complete terminal histories.
    /// Related live snapshots preserve the furthest presented viewport origin;
    /// reconstructed screens carry a distinct coordinate epoch and are exempt.
    pub(crate) fn update_agent_pane_screen_preserving_interaction(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        mut screen: TerminalScreen,
    ) -> Option<u64> {
        self.agent_pane_screen_lineage(pane_id, conversation_id)?;
        let (composite, pending_rows) = self
            .compose_pending_steering_suffix(pane_id, conversation_id, screen)
            .ok()?;
        screen = composite;
        let current = self.process.agent_pane_screens.get_mut(pane_id)?;
        if current.conversation_id != conversation_id {
            return None;
        }
        screen.preserve_normal_viewport_origin(&current.screen);
        self.process.next_agent_pane_screen_lineage = self
            .process
            .next_agent_pane_screen_lineage
            .saturating_add(1)
            .max(1);
        current.lineage = self.process.next_agent_pane_screen_lineage;
        current.screen = screen;
        self.presentation.pending_steering_suffixes.insert(
            pane_id.to_string(),
            (conversation_id.to_string(), current.lineage, pending_rows),
        );
        Some(current.lineage)
    }

    /// Removes one pane's retained agent screen during replacement rollback.
    pub(crate) fn remove_agent_pane_screen(&mut self, pane_id: &str) {
        self.reset_steering_presentation_surface(pane_id);
        self.clear_interaction_state_for_surface(pane_id, PaneSurfaceKind::Agent);
        self.process.agent_pane_screens.remove(pane_id);
    }

    /// Ensures one pane has an agent screen bound to the requested conversation.
    ///
    /// A conversation change replaces the prior screen instead of allowing
    /// delayed presentation from one session to contaminate another session.
    #[allow(dead_code)]
    pub(crate) fn ensure_agent_pane_screen(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        size: Size,
    ) -> Result<&mut TerminalScreen> {
        let replace = self
            .process
            .agent_pane_screens
            .get(pane_id)
            .is_none_or(|screen| screen.conversation_id() != conversation_id);
        if replace {
            let screen = TerminalScreen::new_with_history_config(
                size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?;
            self.process.next_agent_pane_screen_lineage = self
                .process
                .next_agent_pane_screen_lineage
                .saturating_add(1)
                .max(1);
            self.clear_interaction_state_for_surface(pane_id, PaneSurfaceKind::Agent);
            self.process.agent_pane_screens.insert(
                pane_id.to_string(),
                AgentPaneScreen {
                    conversation_id: conversation_id.to_string(),
                    lineage: self.process.next_agent_pane_screen_lineage,
                    screen,
                },
            );
        }
        self.agent_pane_screen_mut(pane_id)
            .ok_or_else(|| MezError::invalid_state("agent pane screen was not initialized"))
    }

    /// Returns the presentation surface selected by pane-local agent visibility.
    #[allow(dead_code)]
    pub(crate) fn presented_pane_surface(&self, pane_id: &str) -> PaneSurfaceKind {
        if self.external_editor_session_is_active(pane_id) {
            PaneSurfaceKind::Process
        } else if self
            .agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.visibility != super::super::AgentShellVisibility::Hidden)
        {
            PaneSurfaceKind::Agent
        } else {
            PaneSurfaceKind::Process
        }
    }

    /// Returns the retained screen selected for presentation in one pane.
    #[allow(dead_code)]
    pub(crate) fn presented_pane_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        match self.presented_pane_surface(pane_id) {
            PaneSurfaceKind::Process => self.process_pane_screen(pane_id),
            PaneSurfaceKind::Agent => {
                let session = self.agent_shell_store().get(pane_id)?;
                let screen = self.agent_pane_screen_state(pane_id)?;
                (screen.conversation_id() == session.session_id).then(|| screen.screen())
            }
        }
    }

    /// Returns mutable terminal state for the surface selected for presentation.
    pub(crate) fn presented_pane_screen_mut(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut TerminalScreen> {
        match self.presented_pane_surface(pane_id) {
            PaneSurfaceKind::Process => self.process_pane_screen_mut(pane_id),
            PaneSurfaceKind::Agent => {
                let conversation_id = self.agent_shell_store().get(pane_id)?.session_id.clone();
                let screen = self.process.agent_pane_screens.get_mut(pane_id)?;
                if screen.conversation_id() != conversation_id {
                    return None;
                }
                Some(screen.screen_mut())
            }
        }
    }

    /// Returns the displayed screen through the temporary compatibility surface.
    ///
    /// Process protocol callers must use `process_pane_screen`; dependent
    /// refactor slices migrate remaining interaction callers to explicit
    /// presented-surface accessors before this compatibility API is removed.
    #[cfg(test)]
    pub(crate) fn pane_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        self.presented_pane_screen(pane_id)
    }

    /// Returns mutable displayed state through the temporary compatibility API.
    #[cfg(test)]
    pub(crate) fn pane_screen_mut(&mut self, pane_id: &str) -> Option<&mut TerminalScreen> {
        self.presented_pane_screen_mut(pane_id)
    }

    /// Replaces process state through the temporary compatibility API.
    #[allow(
        dead_code,
        reason = "compatibility fixture API retained during screen migration"
    )]
    pub(crate) fn set_pane_screen(&mut self, pane_id: impl Into<String>, screen: TerminalScreen) {
        self.set_process_pane_screen(pane_id, screen);
    }

    /// Clears modeled terminal state when the live session is replaced.
    pub(crate) fn clear_pane_screens(&mut self) {
        self.process.process_pane_screens.clear();
        self.process.agent_pane_screens.clear();
    }

    /// Applies new history retention policy to every modeled pane screen.
    pub(crate) fn configure_pane_screen_history(
        &mut self,
        history_limit: usize,
        rotate_lines: usize,
    ) -> Result<()> {
        let mut process_copy_invalidations = Vec::new();
        for (pane_id, screen) in &mut self.process.process_pane_screens {
            let previous_history_len = screen.history().len();
            screen.set_history_limit(history_limit)?;
            screen.set_history_rotate_lines(rotate_lines)?;
            if screen.history().len() != previous_history_len {
                process_copy_invalidations.push(pane_id.clone());
            }
        }
        let mut agent_copy_invalidations = Vec::new();
        for (pane_id, agent_screen) in &mut self.process.agent_pane_screens {
            let previous_history_len = agent_screen.screen().history().len();
            agent_screen.screen_mut().set_history_limit(history_limit)?;
            agent_screen
                .screen_mut()
                .set_history_rotate_lines(rotate_lines)?;
            if agent_screen.screen().history().len() != previous_history_len {
                agent_copy_invalidations.push(pane_id.clone());
            }
        }
        for pane_id in process_copy_invalidations {
            self.clear_copy_state_for_surface(&pane_id, PaneSurfaceKind::Process);
        }
        for pane_id in agent_copy_invalidations {
            self.clear_copy_state_for_surface(&pane_id, PaneSurfaceKind::Agent);
        }
        Ok(())
    }
}
