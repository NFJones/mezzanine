//! Runtime render attached-terminal step orchestration.
//!
//! This module owns primary-client pane input dispatch, attached terminal step
//! application, primary command prompt entry, attached action error reporting,
//! and redraw policy decisions. Keeping this orchestration outside the render
//! facade leaves  as the module root for types, imports, and
//! tests while preserving behavior through  methods.

use super::*;

impl RuntimeSessionService {
    /// Returns the compact approval label shown in the pane agent status area.
    pub(super) fn runtime_frame_policy_mode_name(
        policy: crate::permissions::ApprovalPolicy,
    ) -> &'static str {
        match policy {
            crate::permissions::ApprovalPolicy::Ask => "ask",
            crate::permissions::ApprovalPolicy::AutoAllow => "auto-allow",
            crate::permissions::ApprovalPolicy::FullAccess => "full-access",
        }
    }

    /// Runs the active agent shell visible operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn active_agent_shell_visible(&self) -> Result<bool> {
        let pane_id = self.active_pane_id()?;
        Ok(self
            .agent_shell_store
            .get(&pane_id)
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible))
    }

    /// Reports whether the focused pane is waiting for an agent turn to stop before exit.
    fn active_agent_shell_exit_pending(&self) -> Result<bool> {
        let pane_id = self.active_pane_id()?;
        Ok(self.agent_shell_store.get(&pane_id).is_some_and(|session| {
            session.visibility == AgentShellVisibility::HidePendingTaskCompletion
        }))
    }

    /// Runs the write input to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn write_input_to_pane(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        target: Option<&str>,
        input: &[u8],
    ) -> Result<PaneInputDispatch> {
        self.require_live()?;
        if input.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let descriptor = match target {
            Some(target) => self.find_pane_descriptor(target).ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
            })?,
            None => self.active_window_pane_descriptor(None)?,
        };
        self.write_input_to_pane_descriptor(primary_client_id, &descriptor, input)
    }

    /// Runs the write input to pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn write_input_to_pane_descriptor(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        descriptor: &PaneDescriptor,
        input: &[u8],
    ) -> Result<PaneInputDispatch> {
        self.require_live()?;
        if input.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let primary_pid = self
            .primary_pid_for_live_pane_process(descriptor.pane_id.as_str())
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane process not found",
                )
            })?;
        self.clear_shell_output_filters_for_foreground_input(descriptor.pane_id.as_str());
        self.active_copy_modes.remove(descriptor.pane_id.as_str());
        self.scrollback_copy_mode_panes
            .remove(descriptor.pane_id.as_str());
        self.write_runtime_pane_input(descriptor.pane_id.as_str(), input)?;
        Ok(PaneInputDispatch {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid,
            bytes_written: input.len(),
        })
    }

    /// Runs the apply attached terminal step plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn apply_attached_terminal_step_plan(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        step: &AttachedTerminalClientStepPlan,
    ) -> Result<AttachedClientStepApplication> {
        self.apply_attached_terminal_step_plan_inner(primary_client_id, step, false)
            .map(|(application, _)| application)
    }

    /// Runs the apply attached terminal step plan deferred pane io operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn apply_attached_terminal_step_plan_deferred_pane_io(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        step: &AttachedTerminalClientStepPlan,
    ) -> Result<(AttachedClientStepApplication, Vec<DeferredPaneInput>)> {
        self.apply_attached_terminal_step_plan_inner(primary_client_id, step, true)
    }

    /// Opens an actor-owned command prompt on the primary client.
    ///
    /// The prompt is rendered as part of the next primary client view. Input is
    /// captured by runtime state until the prompt is submitted, cancelled, or
    /// closed by EOF.
    pub fn enter_primary_command_prompt(&mut self, prefill: &str) -> Result<()> {
        self.enter_primary_prompt(ReadlinePromptKind::Command, prefill)
    }

    /// Runs the enter primary prompt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn enter_primary_prompt(&mut self, kind: ReadlinePromptKind, prefill: &str) -> Result<()> {
        self.require_live()?;
        if kind == ReadlinePromptKind::Command && self.primary_command_prompt_history.is_empty() {
            self.reload_primary_command_prompt_history()?;
        }
        let mut prompt_input = runtime_primary_prompt_input(kind, prefill);
        if kind == ReadlinePromptKind::Command {
            prompt_input
                .prompt
                .buffer
                .set_history(self.primary_command_prompt_history.clone());
            prompt_input
                .prompt
                .set_selector_extra_candidates(self.runtime_command_selector_extra_candidates());
        }
        self.primary_prompt_input = Some(prompt_input);
        Ok(())
    }

    /// Runs the apply attached terminal step plan inner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_attached_terminal_step_plan_inner(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        step: &AttachedTerminalClientStepPlan,
        defer_pane_io: bool,
    ) -> Result<(AttachedClientStepApplication, Vec<DeferredPaneInput>)> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let mut deferred_pane_inputs = Vec::new();
        let mut report = AttachedClientStepApplication {
            forwarded_bytes: 0,
            mux_actions_applied: 0,
            mouse_actions_reported: 0,
            unsupported_actions: Vec::new(),
            agent_prompt_inputs_applied: 0,
            view_refresh_required: false,
            full_redraw_required: false,
        };

        if !step.actions.is_empty()
            && let Some(message) = self.primary_error_status_overlay.take()
        {
            let consume_action = message.starts_with("mez error:") || message.starts_with("error:");
            report.view_refresh_required = true;
            if consume_action {
                report.full_redraw_required = true;
                return Ok((report, deferred_pane_inputs));
            }
        }

        for action in &step.actions {
            if !matches!(action, TerminalClientLoopAction::EnterPrefixKeyMode) {
                self.primary_prefix_key_pending = false;
            }
            let primary_display_overlay_requires_full_redraw =
                self.primary_display_overlay_action_requires_full_redraw(action);
            if self.primary_display_overlay.is_some()
                && self.apply_primary_display_overlay_terminal_action(primary_client_id, action)?
            {
                report.view_refresh_required = true;
                if primary_display_overlay_requires_full_redraw {
                    report.full_redraw_required = true;
                }
                continue;
            }
            if self.pane_agent_status_selector.is_some()
                && self
                    .apply_pane_agent_status_selector_terminal_action(primary_client_id, action)?
            {
                report.view_refresh_required = true;
                continue;
            }
            if self.pane_agent_status_selector.is_some()
                && !matches!(
                    action,
                    TerminalClientLoopAction::HandleMouse(
                        MouseAction::OpenPaneAgentStatusSelector { .. }
                            | MouseAction::HoverPaneAgentStatusSelector { .. }
                            | MouseAction::SelectPaneAgentStatusSelector { .. }
                            | MouseAction::ScrollPaneAgentStatusSelector { .. }
                            | MouseAction::ClosePaneAgentStatusSelector
                    )
                )
            {
                self.pane_agent_status_selector = None;
                report.view_refresh_required = true;
            }
            if self.primary_prompt_input.is_some()
                && matches!(
                    action,
                    TerminalClientLoopAction::ForwardToPane(_)
                        | TerminalClientLoopAction::ForwardMouseToPane { .. }
                )
            {
                if self.apply_primary_prompt_terminal_action(primary_client_id, action)? {
                    report.view_refresh_required = true;
                }
                continue;
            }
            match action {
                TerminalClientLoopAction::ForwardToPane(input) => {
                    if self.active_agent_shell_visible()? {
                        if self.apply_attached_agent_prompt_input(primary_client_id, input)? {
                            self.sync_tracked_pty_sizes()?;
                            report.agent_prompt_inputs_applied =
                                report.agent_prompt_inputs_applied.saturating_add(1);
                            report.view_refresh_required = true;
                            if !self.active_agent_shell_visible()? {
                                report.full_redraw_required = true;
                            }
                        }
                    } else if self.active_agent_shell_exit_pending()? {
                        let pane_id = self.active_pane_id()?;
                        self.append_agent_status_text_to_terminal_buffer(
                            &pane_id,
                            "agent: input blocked while agent shell is stopping",
                        )?;
                        report.agent_prompt_inputs_applied =
                            report.agent_prompt_inputs_applied.saturating_add(1);
                        report.view_refresh_required = true;
                        report.full_redraw_required = true;
                    } else {
                        if defer_pane_io {
                            let descriptors = self.active_window_input_descriptors()?;
                            for descriptor in descriptors {
                                self.clear_shell_output_filters_for_foreground_input(
                                    descriptor.pane_id.as_str(),
                                );
                                self.active_copy_modes.remove(descriptor.pane_id.as_str());
                                self.scrollback_copy_mode_panes
                                    .remove(descriptor.pane_id.as_str());
                                deferred_pane_inputs.push(DeferredPaneInput {
                                    pane_id: descriptor.pane_id.to_string(),
                                    bytes: input.clone(),
                                    priority: false,
                                });
                                report.forwarded_bytes =
                                    report.forwarded_bytes.saturating_add(input.len());
                            }
                        } else {
                            for descriptor in self.active_window_input_descriptors()? {
                                let dispatch = self.write_input_to_pane_descriptor(
                                    primary_client_id,
                                    &descriptor,
                                    input,
                                )?;
                                report.forwarded_bytes = report
                                    .forwarded_bytes
                                    .saturating_add(dispatch.bytes_written);
                            }
                        }
                    }
                }
                TerminalClientLoopAction::ForwardMouseToPane { pane_id, input } => {
                    let Some(descriptor) = self.find_pane_descriptor(pane_id) else {
                        continue;
                    };
                    if defer_pane_io {
                        self.clear_shell_output_filters_for_foreground_input(
                            descriptor.pane_id.as_str(),
                        );
                        self.active_copy_modes.remove(descriptor.pane_id.as_str());
                        self.scrollback_copy_mode_panes
                            .remove(descriptor.pane_id.as_str());
                        deferred_pane_inputs.push(DeferredPaneInput {
                            pane_id: descriptor.pane_id.to_string(),
                            bytes: input.clone(),
                            priority: false,
                        });
                        report.forwarded_bytes = report.forwarded_bytes.saturating_add(input.len());
                    } else {
                        let dispatch = self.write_input_to_pane_descriptor(
                            primary_client_id,
                            &descriptor,
                            input,
                        )?;
                        report.forwarded_bytes = report
                            .forwarded_bytes
                            .saturating_add(dispatch.bytes_written);
                    }
                }
                TerminalClientLoopAction::ExecuteMux(action) => {
                    if let Some(prefill) = mux_action_command_prompt_prefill(*action) {
                        match self.enter_primary_command_prompt(prefill) {
                            Ok(()) => {
                                report.view_refresh_required = true;
                            }
                            Err(error) => {
                                self.present_attached_action_error(&mut report, &error)?
                            }
                        }
                        continue;
                    }
                    let toggles_agent_shell = *action == MuxAction::ToggleAgentShell;
                    match self.apply_attached_mux_action(primary_client_id, *action) {
                        Ok(true) => {
                            report.mux_actions_applied =
                                report.mux_actions_applied.saturating_add(1);
                            report.view_refresh_required = true;
                            if toggles_agent_shell || Self::mux_action_requires_full_redraw(*action)
                            {
                                report.full_redraw_required = true;
                            }
                        }
                        Ok(false) => {
                            report
                                .unsupported_actions
                                .push(format!("mux:{}", mux_action_name(*action)));
                        }
                        Err(error) => self.present_attached_action_error(&mut report, &error)?,
                    }
                }
                TerminalClientLoopAction::ExecuteCommand(command) => {
                    match self.execute_terminal_command(primary_client_id, command) {
                        Ok(output) => {
                            self.append_lifecycle_event(
                                EventKind::Diagnostic,
                                format!(
                                    r#"{{"key_binding_command":"{}","output":"{}"}}"#,
                                    json_escape(command),
                                    json_escape(&output)
                                ),
                            )?;
                            report.mux_actions_applied =
                                report.mux_actions_applied.saturating_add(1);
                            report.view_refresh_required = true;
                            report.full_redraw_required = true;
                        }
                        Err(error) => self.present_attached_action_error(&mut report, &error)?,
                    }
                }
                TerminalClientLoopAction::HandleMouse(action) => {
                    let overlay_was_open = self.primary_display_overlay.is_some();
                    match self.apply_attached_mouse_action(primary_client_id, action.clone()) {
                        Ok(true) => {
                            report.mouse_actions_reported =
                                report.mouse_actions_reported.saturating_add(1);
                            report.view_refresh_required = true;
                            if Self::mouse_action_requires_full_redraw(action.clone())
                                || overlay_was_open != self.primary_display_overlay.is_some()
                            {
                                report.full_redraw_required = true;
                            }
                        }
                        Ok(false) => {
                            report.mouse_actions_reported =
                                report.mouse_actions_reported.saturating_add(1);
                            report
                                .unsupported_actions
                                .push(format!("mouse:{}", mouse_action_name(action.clone())));
                        }
                        Err(error) => self.present_attached_action_error(&mut report, &error)?,
                    }
                }
                TerminalClientLoopAction::HandleCopyMode(action) => {
                    match self.apply_attached_copy_mode_action(*action) {
                        Ok(true) => {
                            report.view_refresh_required = true;
                        }
                        Ok(false) => {
                            report
                                .unsupported_actions
                                .push(format!("copy-mode:{action:?}"));
                        }
                        Err(error) => self.present_attached_action_error(&mut report, &error)?,
                    }
                }
                TerminalClientLoopAction::EnterPrefixKeyMode => {
                    self.primary_prefix_key_pending = true;
                    report.view_refresh_required = true;
                }
                TerminalClientLoopAction::ReportUnboundPrefix(chord) => report
                    .unsupported_actions
                    .push(format!("prefix:unbound:{chord:?}")),
            }
        }

        self.persist_or_defer_registry_update()?;
        Ok((report, deferred_pane_inputs))
    }

    /// Returns true when a mux action can change pane/window geometry enough to
    /// require resetting the attached terminal frame before the next render.
    fn mux_action_requires_full_redraw(action: MuxAction) -> bool {
        matches!(
            action,
            MuxAction::NewWindow
                | MuxAction::NewGroup
                | MuxAction::SplitPaneVertical
                | MuxAction::SplitPaneHorizontal
                | MuxAction::TogglePaneZoom
                | MuxAction::CycleLayouts
                | MuxAction::KillPaneAfterConfirmation
                | MuxAction::BreakPaneToNewWindow
                | MuxAction::SwapPanePrevious
                | MuxAction::SwapPaneNext
        )
    }

    /// Records a recoverable foreground action error as a transient primary
    /// status notice instead of allowing it to abort the attached client.
    fn present_attached_action_error(
        &mut self,
        report: &mut AttachedClientStepApplication,
        error: &MezError,
    ) -> Result<()> {
        self.show_primary_error_overlay(vec![format!("mez error: {error}")])?;
        report.view_refresh_required = true;
        report.full_redraw_required = true;
        Ok(())
    }

    /// Returns true when a mouse action can change pane geometry and therefore
    /// needs a full attached-frame redraw after the action is applied.
    fn mouse_action_requires_full_redraw(action: MouseAction) -> bool {
        matches!(
            action,
            MouseAction::ResizePane { .. } | MouseAction::ReleaseWindowAction { .. }
        )
    }
}
