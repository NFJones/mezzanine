//! Runtime render multiplexer action methods.
//!
//! This module owns attached multiplexer action dispatch, command-display
//! overlay execution, pane swapping, observer approval cutoffs, and active pane
//! lookup. Keeping these methods outside the render facade separates mux-level
//! terminal actions from frame rendering and prompt/input handling.

use super::*;

impl RuntimeSessionService {
    /// Runs the apply attached mux action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn apply_attached_mux_action(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        action: MuxAction,
    ) -> Result<bool> {
        match action {
            MuxAction::SendPrefixToPane => {
                let input = key_chord_input_bytes(self.presentation.settings.key_bindings.escape)
                    .ok_or_else(|| {
                    MezError::invalid_state("configured prefix key cannot be sent to pane")
                })?;
                self.write_input_to_pane(primary_client_id, None, &input)?;
            }
            MuxAction::ListKeyBindings => {
                self.execute_attached_display_command(primary_client_id, "list-keys")?;
            }
            MuxAction::NewWindow => {
                self.create_window_with_pane_process(primary_client_id, "shell", true, None)?;
            }
            MuxAction::NewGroup => {
                self.create_group_with_pane_process(primary_client_id, "shell", true, None, None)?;
            }
            MuxAction::SplitPaneVertical => {
                self.split_pane_with_process(primary_client_id, SplitDirection::Vertical, None)?;
            }
            MuxAction::SplitPaneHorizontal => {
                self.split_pane_with_process(primary_client_id, SplitDirection::Horizontal, None)?;
            }
            MuxAction::FocusPane(direction) => {
                self.session.select_adjacent_pane(
                    primary_client_id,
                    pane_navigation_direction(direction),
                )?;
            }
            MuxAction::FocusLastPane => {
                self.session.select_last_pane(primary_client_id)?;
            }
            MuxAction::EnterCopyMode => {
                let pane_id = self.active_pane_id()?;
                self.ensure_active_copy_mode(pane_id.as_str())?;
            }
            MuxAction::EnterCopyModeAndPageUp => {
                let pane_id = self.active_pane_id()?;
                let copy_mode = self.ensure_active_copy_mode(pane_id.as_str())?;
                copy_mode.page_up();
            }
            MuxAction::FocusWindow(WindowFocusTarget::Next) => {
                self.session.next_window(primary_client_id)?;
            }
            MuxAction::FocusWindow(WindowFocusTarget::Previous) => {
                self.session.previous_window(primary_client_id)?;
            }
            MuxAction::FocusWindow(WindowFocusTarget::LastActive) => {
                self.session.last_window(primary_client_id)?;
            }
            MuxAction::FocusWindow(WindowFocusTarget::Index(index)) => {
                self.session
                    .select_window(primary_client_id, &index.to_string())?;
            }
            MuxAction::FocusWindow(WindowFocusTarget::ChooseInteractively) => {
                self.execute_attached_display_command(primary_client_id, "choose-window")?;
            }
            MuxAction::FocusGroup(GroupFocusTarget::Next) => {
                let effects = self.session.next_group_transition(primary_client_id)?;
                self.sync_pane_resize_effects(&effects)?;
            }
            MuxAction::FocusGroup(GroupFocusTarget::Previous) => {
                let effects = self.session.previous_group_transition(primary_client_id)?;
                self.sync_pane_resize_effects(&effects)?;
            }
            MuxAction::FocusGroup(GroupFocusTarget::LastActive) => {
                let effects = self.session.last_group_transition(primary_client_id)?;
                self.sync_pane_resize_effects(&effects)?;
            }
            MuxAction::FocusGroup(GroupFocusTarget::ChooseInteractively) => {
                self.execute_attached_display_command(primary_client_id, "choose-group")?;
            }
            MuxAction::CyclePane => {
                self.session
                    .select_adjacent_pane(primary_client_id, PaneNavigationDirection::Right)?;
            }
            MuxAction::ShowPaneIndexes => {
                self.execute_attached_display_command(primary_client_id, "display-panes")?;
            }
            MuxAction::TogglePaneZoom => {
                let (_, effects) = self
                    .session
                    .toggle_active_pane_zoom_transition(primary_client_id)?;
                self.sync_pane_resize_effects(&effects)?;
            }
            MuxAction::CycleLayouts => {
                let (_, effects) = self.session.cycle_layout_transition(primary_client_id)?;
                self.sync_pane_resize_effects(&effects)?;
            }
            MuxAction::BreakPaneToNewWindow => {
                self.break_pane_and_sync_pty_sizes(
                    primary_client_id,
                    None,
                    Some("shell".to_string()),
                    true,
                )?;
            }
            MuxAction::SwapPanePrevious | MuxAction::SwapPaneNext => {
                if !self.swap_active_pane_with_neighbor(primary_client_id, action)? {
                    return Ok(false);
                }
            }
            MuxAction::DetachPrimaryClient => {
                self.detach_primary(primary_client_id, self.session.authoritative_size)?;
            }
            MuxAction::ChooseClientOrObserverToDetach => {
                self.execute_attached_display_command(primary_client_id, "choose-client")?;
            }
            MuxAction::PasteBuffer(PasteBufferTarget::MostRecent) => {
                if !self.paste_most_recent_buffer_to_active_pane(primary_client_id)? {
                    return Ok(false);
                }
            }
            MuxAction::PasteBuffer(PasteBufferTarget::ChooseInteractively) => {
                self.execute_attached_display_command(primary_client_id, "choose-buffer")?;
            }
            MuxAction::ListPasteBuffers => {
                self.execute_attached_display_command(primary_client_id, "list-buffers")?;
            }
            MuxAction::DeleteMostRecentPasteBuffer => {
                let Some(name) = self
                    .presentation
                    .copy
                    .paste_buffers
                    .most_recent_name()
                    .map(ToOwned::to_owned)
                else {
                    return Ok(false);
                };
                self.execute_attached_display_command(
                    primary_client_id,
                    &format!("delete-buffer {name}"),
                )?;
            }
            MuxAction::ChoosePendingObservers => {
                self.execute_attached_display_command(primary_client_id, "choose-observer")?;
            }
            MuxAction::ToggleAgentShell => {
                self.toggle_active_agent_shell()?;
            }
            MuxAction::ShowMessages => {
                self.execute_attached_display_command(primary_client_id, "show-messages")?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Runs the execute attached display command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn execute_attached_display_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        command: &str,
    ) -> Result<()> {
        let output = self.execute_terminal_command(primary_client_id, command)?;
        let output_excerpt = output.chars().take(384).collect::<String>();
        let truncated = output_excerpt.len() < output.len();
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"attached_display_command":"{}","output":"{}","truncated":{}}}"#,
                json_escape(command),
                json_escape(&output_excerpt),
                truncated
            ),
        )?;
        let content =
            runtime_command_display_overlay_content(&output, &self.presentation.settings.ui_theme)?;
        self.present_runtime_command_display_content(content)
    }

    /// Runs the swap active pane with neighbor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn swap_active_pane_with_neighbor(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        action: MuxAction,
    ) -> Result<bool> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        if window.panes().len() < 2 {
            return Ok(false);
        }
        let active = window.active_pane_index();
        let target = match action {
            MuxAction::SwapPanePrevious => {
                if active == 0 {
                    window.panes().len() - 1
                } else {
                    active - 1
                }
            }
            MuxAction::SwapPaneNext => (active + 1) % window.panes().len(),
            _ => return Ok(false),
        };
        self.swap_panes_and_sync_pty_sizes(primary_client_id, None, &target.to_string())?;
        Ok(true)
    }

    /// Runs the approve observer with runtime cutoff operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn approve_observer_with_runtime_cutoff(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        observer_id: &str,
    ) -> Result<()> {
        if let Some(visible_from_event_id) = self
            .control
            .event_log()
            .map(|event_log| event_log.latest_event_id().saturating_add(1))
        {
            Ok(self
                .session
                .approve_observer_target_with_visible_from_event_id(
                    primary_client_id,
                    observer_id,
                    visible_from_event_id,
                )?)
        } else {
            Ok(self
                .session
                .approve_observer_target(primary_client_id, observer_id)?)
        }
    }

    /// Runs the active pane id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn active_pane_id(&self) -> Result<String> {
        self.session
            .active_window()
            .map(|window| window.active_pane().id.to_string())
            .ok_or_else(|| MezError::invalid_state("session has no active pane"))
    }
}
