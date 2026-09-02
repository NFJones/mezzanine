//! Runtime render attached-terminal step orchestration.
//!
//! This module owns primary-client pane input dispatch, attached terminal step
//! application, primary command prompt entry, attached action error reporting,
//! and redraw policy decisions. Keeping this orchestration outside the render
//! facade leaves  as the module root for types, imports, and
//! tests while preserving behavior through  methods.

use super::{
    AgentShellVisibility, AttachedClientStepApplication, AttachedTerminalClientStepPlan,
    ClientViewRole, EventKind, MezError, MouseAction, MuxAction, PaneDescriptor, PaneInputDispatch,
    ReadlinePromptKind, Result, RuntimeSessionService, Size, TerminalClientLoopAction,
    TerminalClientLoopConfig, json_escape, mouse_action_name, mux_action_command_prompt_prefill,
    mux_action_name, runtime_primary_prompt_input,
};
use crate::host::terminal::{
    AttachedTerminalFdReadiness, AttachedTerminalFdRole, TerminalFdInterest,
    plan_attached_terminal_client_step,
};
use crate::runtime::{
    AttachedClientClipboardWrite, PaneProcessIoEffect, RenderInvalidationReason,
    RuntimeAgentPromptProviderInfoRefresh, RuntimeSideEffect, RuntimeTransition,
};

impl RuntimeSessionService {
    /// Returns the compact approval label shown in the pane agent status area.
    pub(super) fn runtime_frame_policy_mode_name(
        policy: mez_agent::ApprovalPolicy,
    ) -> &'static str {
        match policy {
            mez_agent::ApprovalPolicy::Ask => "ask",
            mez_agent::ApprovalPolicy::AutoAllow => "auto-allow",
            mez_agent::ApprovalPolicy::FullAccess => "full-access",
            mez_agent::ApprovalPolicy::HostAccess => "host-access",
        }
    }

    /// Runs the active agent shell visible operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn active_agent_shell_visible(&self) -> Result<bool> {
        let pane_id = self.active_pane_id()?;
        if self.external_editor_session_is_active(&pane_id) {
            return Ok(false);
        }
        Ok(self
            .agent_shell_store()
            .get(&pane_id)
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible))
    }

    /// Builds deferred pane input for the owner currently responsible for the PTY.
    ///
    /// Adapter-owned processes require their exact generation so an older worker
    /// cannot consume input intended for a replacement process with the same pane id.
    fn deferred_pane_input_effect(&self, pane_id: String, bytes: Vec<u8>) -> RuntimeSideEffect {
        if let Some(instance) = self.adapter_owned_pane_process_instance(&pane_id) {
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: PaneProcessIoEffect::WriteInput { bytes },
            }
        } else {
            RuntimeSideEffect::WritePaneInput { pane_id, bytes }
        }
    }

    /// Reports whether foreground input may be delivered to a pane process.
    fn pane_process_input_is_allowed(&self, pane_id: &str) -> bool {
        self.presented_pane_surface(pane_id) == crate::runtime::PaneSurfaceKind::Process
    }

    /// Reports whether the focused pane is waiting for an agent turn to stop before exit.
    fn active_agent_shell_exit_pending(&self) -> Result<bool> {
        let pane_id = self.active_pane_id()?;
        Ok(self
            .agent_shell_store()
            .get(&pane_id)
            .is_some_and(|session| {
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
        primary_client_id: &mez_core::ids::ClientId,
        target: Option<&str>,
        input: &[u8],
    ) -> Result<PaneInputDispatch> {
        self.require_live()?;
        if input.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
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
    pub(crate) fn write_input_to_pane_descriptor(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        descriptor: &PaneDescriptor,
        input: &[u8],
    ) -> Result<PaneInputDispatch> {
        self.require_live()?;
        if input.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
        }
        if !self.pane_process_input_is_allowed(descriptor.pane_id.as_str()) {
            return Err(MezError::forbidden(
                "pane process input requires the process surface to be presented",
            ));
        }
        let primary_pid = self
            .primary_pid_for_live_pane_process(descriptor.pane_id.as_str())
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane process not found",
                )
            })?;
        if self.queue_managed_shell_parent_input(descriptor.pane_id.as_str(), input)? {
            self.clear_copy_state_for_surface(
                descriptor.pane_id.as_str(),
                crate::runtime::PaneSurfaceKind::Process,
            );
            self.record_user_process_input(descriptor.pane_id.as_str(), input);
            return Ok(PaneInputDispatch {
                session_id: self.session.id.to_string(),
                window_id: descriptor.window_id.to_string(),
                pane_id: descriptor.pane_id.to_string(),
                primary_pid,
                bytes_written: input.len(),
            });
        }
        self.clear_shell_output_filters_for_foreground_input(descriptor.pane_id.as_str());
        self.clear_copy_state_for_surface(
            descriptor.pane_id.as_str(),
            crate::runtime::PaneSurfaceKind::Process,
        );
        self.record_user_process_input(descriptor.pane_id.as_str(), input);
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
        primary_client_id: &mez_core::ids::ClientId,
        step: &AttachedTerminalClientStepPlan,
    ) -> Result<AttachedClientStepApplication> {
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
        }
        self.presentation.activate_client_state(primary_client_id);
        self.session.activate_client_navigation(primary_client_id)?;
        let result = self
            .apply_attached_terminal_step_plan_inner(primary_client_id, step, false, false, false)
            .map(|(application, _)| application);
        self.presentation.capture_projected_client_state();
        result
    }

    /// Applies one planned client step and returns its ordered adapter effects.
    pub(crate) fn apply_attached_terminal_step_transition(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        step: &AttachedTerminalClientStepPlan,
    ) -> Result<(AttachedClientStepApplication, RuntimeTransition)> {
        self.apply_attached_terminal_step_transition_with_clipboard_policy(
            primary_client_id,
            step,
            false,
        )
    }

    /// Applies one planned client step with an explicit host-clipboard policy.
    ///
    /// When the acting primary owns a client transport clipboard route, the
    /// copied text is delivered to the client adapter instead of the server
    /// host, so host-side clipboard commands are suppressed for that step.
    pub(crate) fn apply_attached_terminal_step_transition_with_clipboard_policy(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        step: &AttachedTerminalClientStepPlan,
        suppress_host_clipboard_copy: bool,
    ) -> Result<(AttachedClientStepApplication, RuntimeTransition)> {
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
        }
        self.presentation.activate_client_state(primary_client_id);
        self.session.activate_client_navigation(primary_client_id)?;
        let pre_mutation_window_id = self
            .session
            .active_window_for(primary_client_id)
            .ok()
            .map(|window| window.id.to_string());
        let pre_mutation_primary_client_ids =
            pre_mutation_window_id
                .as_ref()
                .map_or_else(Vec::new, |window_id| {
                    self.session
                        .clients()
                        .iter()
                        .filter(|client| {
                            client.role == mez_mux::session::ClientRole::Primary
                                && client.state == mez_mux::session::ClientState::Attached
                        })
                        .filter_map(|client| {
                            self.session
                                .active_window_for(&client.id)
                                .ok()
                                .filter(|window| window.id.as_str() == window_id)
                                .map(|_| client.id.clone())
                        })
                        .collect()
                });
        let result = self.apply_attached_terminal_step_plan_inner(
            primary_client_id,
            step,
            true,
            true,
            suppress_host_clipboard_copy,
        );
        self.presentation.capture_projected_client_state();
        let (application, mut side_effects) = result?;
        let structural_mutation = step.actions.iter().any(|action| {
            matches!(
                action,
                TerminalClientLoopAction::ExecuteMux(
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
            )
        });
        let divider_resize_only = step.actions.iter().all(|action| {
            matches!(
                action,
                TerminalClientLoopAction::HandleMouse(
                    MouseAction::ResizePane { .. } | MouseAction::FinishResizePane
                )
            )
        });
        if structural_mutation || (application.full_redraw_required && !divider_resize_only) {
            self.presentation.clear_mouse_resize_drag_state();
            self.presentation
                .redispatch_pending_agent_presentation_resizes();
        }
        let render_reason = self.attached_terminal_step_render_reason(&application, step);
        side_effects.extend(render_reason.map_or_else(Vec::new, |reason| {
            if structural_mutation {
                let mut window_ids = pre_mutation_window_id.into_iter().collect::<Vec<_>>();
                if let Ok(window) = self.session.active_window_for(primary_client_id)
                    && !window_ids
                        .iter()
                        .any(|window_id| window_id == window.id.as_str())
                {
                    window_ids.push(window.id.to_string());
                }
                let mut effects =
                    self.render_effects_for_clients_projecting_windows(&window_ids, reason);
                let prior_effects = self.render_effects_for_primary_projections(
                    &pre_mutation_primary_client_ids,
                    reason,
                );
                effects.retain(|effect| !prior_effects.contains(effect));
                effects.extend(prior_effects);
                effects
            } else {
                self.render_effects_for_primary_projection(primary_client_id, reason)
            }
        }));
        let applied = application.forwarded_bytes > 0
            || application.mux_actions_applied > 0
            || application.mouse_actions_reported > 0
            || !application.unsupported_actions.is_empty()
            || application.agent_prompt_inputs_applied > 0
            || application.view_refresh_required
            || application.full_redraw_required;
        if application.registry_persistence_required {
            side_effects.extend(self.registry_persistence_transition().side_effects);
        }
        Ok((
            application,
            RuntimeTransition {
                applied,
                side_effects,
            },
        ))
    }

    /// Classifies the strongest exact-client render invalidation for one step.
    ///
    /// Both actor-delivered steps and synchronous framed control steps use this
    /// classifier so a mutation that is not represented by an inline view can
    /// wake the authoritative pushed-render stream with identical semantics.
    pub(crate) fn attached_terminal_step_render_reason(
        &self,
        application: &AttachedClientStepApplication,
        step: &AttachedTerminalClientStepPlan,
    ) -> Option<RenderInvalidationReason> {
        let deferred_resize_drag = step.actions.iter().any(|action| {
            matches!(
                action,
                TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { .. })
            )
        }) && self.presentation.mouse_resize_drag_active();
        let deferred_resize_release =
            step.actions.iter().any(|action| {
                matches!(
                    action,
                    TerminalClientLoopAction::HandleMouse(MouseAction::FinishResizePane)
                )
            }) && self.presentation.pending_divider_layout_commit_active();
        if deferred_resize_drag || deferred_resize_release {
            None
        } else if application.full_redraw_required {
            Some(RenderInvalidationReason::FullRedraw)
        } else if application.agent_prompt_inputs_applied > 0 {
            Some(RenderInvalidationReason::AgentPrompt)
        } else if application.view_refresh_required
            || application.mux_actions_applied > 0
            || application.mouse_actions_reported > 0
        {
            Some(RenderInvalidationReason::Overlay)
        } else {
            None
        }
    }

    /// Drains interactive provider refreshes queued by prompt submission.
    pub(crate) fn take_pending_agent_prompt_provider_info_refreshes(
        &mut self,
    ) -> Vec<RuntimeAgentPromptProviderInfoRefresh> {
        std::mem::take(
            &mut self
                .presentation
                .pending_agent_prompt_provider_info_refreshes,
        )
    }

    /// Plans and applies raw primary-client input as a runtime transition.
    pub(crate) fn apply_client_input_transition(
        &mut self,
        client_id: &mez_core::ids::ClientId,
        bytes: &[u8],
    ) -> Result<RuntimeTransition> {
        if bytes.is_empty() || !self.session.is_attached_primary(client_id) {
            return Ok(RuntimeTransition::default());
        }
        let Some(terminal) = self
            .session
            .clients()
            .iter()
            .find(|client| {
                client.id == *client_id && client.state == mez_mux::session::ClientState::Attached
            })
            .and_then(|client| client.terminal.clone())
        else {
            return Ok(RuntimeTransition::default());
        };
        self.presentation.activate_client_state(client_id);
        self.session.activate_client_navigation(client_id)?;
        let pane_id = self.active_pane_id()?;
        if self.external_editor_session_is_active(&pane_id) {
            if !self.external_editor_session_owned_by(&pane_id, client_id) {
                self.presentation.capture_projected_client_state();
                return Ok(RuntimeTransition::default());
            }
            self.clear_copy_state_for_surface(&pane_id, crate::runtime::PaneSurfaceKind::Process);
            let transition = RuntimeTransition {
                applied: true,
                side_effects: vec![
                    self.deferred_external_editor_input_effect(&pane_id, bytes.to_vec())?,
                ],
            };
            self.presentation.capture_projected_client_state();
            return Ok(transition);
        }
        let size = Size::new(terminal.columns, terminal.rows)?;
        let config = self.terminal_client_loop_config(TerminalClientLoopConfig::default())?;
        let view =
            self.render_client_view_with_resolved_config(ClientViewRole::Primary, size, &config)?;
        let readiness = [AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }];
        let step = plan_attached_terminal_client_step(
            &readiness,
            Some(bytes),
            view.as_ref(),
            None,
            &config,
        )?;
        if step.actions.is_empty() {
            self.presentation.capture_projected_client_state();
            return Ok(RuntimeTransition::default());
        }
        self.apply_attached_terminal_step_transition(client_id, &step)
            .map(|(_, transition)| transition)
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
        if kind == ReadlinePromptKind::Command
            && self.presentation.primary_command_prompt_history.is_empty()
        {
            self.reload_primary_command_prompt_history()?;
        }
        let mut prompt_input = runtime_primary_prompt_input(kind, prefill);
        if kind == ReadlinePromptKind::Command {
            prompt_input
                .prompt
                .buffer
                .set_structured_history(self.presentation.primary_command_prompt_history.clone());
            prompt_input
                .prompt
                .set_selector_extra_candidates(self.runtime_command_selector_extra_candidates());
        }
        self.presentation.primary_prompt_input = Some(prompt_input);
        Ok(())
    }

    /// Runs the apply attached terminal step plan inner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_attached_terminal_step_plan_inner(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        step: &AttachedTerminalClientStepPlan,
        defer_pane_io: bool,
        queue_external_effects: bool,
        suppress_host_clipboard_copy: bool,
    ) -> Result<(AttachedClientStepApplication, Vec<RuntimeSideEffect>)> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
        }
        let mut pane_input_effects = Vec::new();
        let mut report = AttachedClientStepApplication {
            forwarded_bytes: 0,
            mux_actions_applied: 0,
            mouse_actions_reported: 0,
            unsupported_actions: Vec::new(),
            agent_prompt_inputs_applied: 0,
            view_refresh_required: false,
            full_redraw_required: false,
            registry_persistence_required: false,
            client_clipboard_write: None,
        };

        let editor_pane_id = self
            .active_pane_id()
            .ok()
            .filter(|pane_id| self.external_editor_session_is_active(pane_id));
        if let Some(pane_id) = editor_pane_id {
            if !self.external_editor_session_owned_by(&pane_id, primary_client_id) {
                return Ok((report, pane_input_effects));
            }
            for action in &step.actions {
                let TerminalClientLoopAction::ForwardToPane(input) = action else {
                    continue;
                };
                self.clear_copy_state_for_surface(
                    &pane_id,
                    crate::runtime::PaneSurfaceKind::Process,
                );
                if defer_pane_io {
                    pane_input_effects
                        .push(self.deferred_external_editor_input_effect(&pane_id, input.clone())?);
                } else {
                    self.write_external_editor_input(&pane_id, input)?;
                }
                report.forwarded_bytes = report.forwarded_bytes.saturating_add(input.len());
            }
            return Ok((report, pane_input_effects));
        }

        if !step.actions.is_empty()
            && let Some(message) = self.presentation.primary_error_status_overlay.take()
        {
            let consume_action = message.starts_with("mez error:") || message.starts_with("error:");
            report.view_refresh_required = true;
            if consume_action {
                report.full_redraw_required = true;
                return Ok((report, pane_input_effects));
            }
        }

        for action in &step.actions {
            if !matches!(action, TerminalClientLoopAction::EnterPrefixKeyMode) {
                self.presentation.primary_prefix_key_pending = false;
            }
            let primary_display_overlay_requires_full_redraw =
                self.primary_display_overlay_action_requires_full_redraw(action);
            if self.presentation.primary_display_overlay.is_some() {
                if let (true, client_clipboard_write) = self
                    .apply_primary_display_overlay_terminal_action(
                        primary_client_id,
                        action,
                        suppress_host_clipboard_copy,
                    )?
                {
                    if report.client_clipboard_write.is_none() {
                        report.client_clipboard_write =
                            client_clipboard_write.map(AttachedClientClipboardWrite::new);
                    }
                    report.view_refresh_required = true;
                    if primary_display_overlay_requires_full_redraw {
                        report.full_redraw_required = true;
                    }
                    continue;
                }
                if matches!(
                    action,
                    TerminalClientLoopAction::ForwardToPane(_)
                        | TerminalClientLoopAction::ForwardMouseToPane { .. }
                ) {
                    continue;
                }
            }
            if self.presentation.pane_agent_status_selector.is_some()
                && self
                    .apply_pane_agent_status_selector_terminal_action(primary_client_id, action)?
            {
                report.view_refresh_required = true;
                continue;
            }
            if self.presentation.pane_agent_status_selector.is_some()
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
                self.presentation.pane_agent_status_selector = None;
                report.view_refresh_required = true;
            }
            if self.presentation.primary_prompt_input.is_some()
                && matches!(
                    action,
                    TerminalClientLoopAction::ForwardToPane(_)
                        | TerminalClientLoopAction::ForwardMouseToPane { .. }
                )
            {
                let overlay_was_open = self.presentation.primary_display_overlay.is_some();
                if self.apply_primary_prompt_terminal_action(
                    primary_client_id,
                    action,
                    queue_external_effects,
                )? {
                    report.view_refresh_required = true;
                    if overlay_was_open != self.presentation.primary_display_overlay.is_some() {
                        report.full_redraw_required = true;
                    }
                }
                continue;
            }
            match action {
                TerminalClientLoopAction::ForwardToPane(input) => {
                    if self.active_agent_shell_visible()? {
                        let pane_id = self.active_pane_id()?;
                        let resize_pane_id = self.session.active_window().and_then(|window| {
                            window
                                .panes()
                                .iter()
                                .find(|pane| pane.id.as_str() == pane_id)
                                .map(|pane| pane.id.clone())
                        });
                        let previous_process_size = self
                            .session
                            .active_window()
                            .and_then(|window| self.pane_process_size_for(window, &pane_id));
                        let overlay_was_open = self.presentation.primary_display_overlay.is_some();
                        if self.apply_attached_agent_prompt_input(primary_client_id, input)? {
                            let current_process_size = self
                                .session
                                .active_window()
                                .and_then(|window| self.pane_process_size_for(window, &pane_id));
                            if current_process_size != previous_process_size
                                && let Some(size) = current_process_size
                                && let Some(resize_pane_id) = resize_pane_id
                            {
                                self.sync_pane_resize_effects(&[
                                    mez_mux::session::PaneResizeEffect {
                                        pane_id: resize_pane_id,
                                        size,
                                    },
                                ])?;
                            }
                            report.agent_prompt_inputs_applied =
                                report.agent_prompt_inputs_applied.saturating_add(1);
                            report.view_refresh_required = true;
                            if !self.active_agent_shell_visible()?
                                || overlay_was_open
                                    != self.presentation.primary_display_overlay.is_some()
                            {
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
                                if !self.pane_process_input_is_allowed(descriptor.pane_id.as_str())
                                {
                                    continue;
                                }
                                if self.queue_managed_shell_parent_input(
                                    descriptor.pane_id.as_str(),
                                    input,
                                )? {
                                    self.clear_copy_state_for_surface(
                                        descriptor.pane_id.as_str(),
                                        crate::runtime::PaneSurfaceKind::Process,
                                    );
                                    report.forwarded_bytes =
                                        report.forwarded_bytes.saturating_add(input.len());
                                    continue;
                                }
                                self.clear_shell_output_filters_for_foreground_input(
                                    descriptor.pane_id.as_str(),
                                );
                                self.clear_copy_state_for_surface(
                                    descriptor.pane_id.as_str(),
                                    crate::runtime::PaneSurfaceKind::Process,
                                );
                                pane_input_effects.push(self.deferred_pane_input_effect(
                                    descriptor.pane_id.to_string(),
                                    input.clone(),
                                ));
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
                    if self.presented_pane_surface(pane_id.as_str())
                        != crate::runtime::PaneSurfaceKind::Process
                    {
                        continue;
                    }
                    let Some(descriptor) = self.find_pane_descriptor(pane_id) else {
                        continue;
                    };
                    if defer_pane_io {
                        if self
                            .queue_managed_shell_parent_input(descriptor.pane_id.as_str(), input)?
                        {
                            self.clear_copy_state_for_surface(
                                descriptor.pane_id.as_str(),
                                crate::runtime::PaneSurfaceKind::Process,
                            );
                            report.forwarded_bytes =
                                report.forwarded_bytes.saturating_add(input.len());
                            continue;
                        }
                        self.clear_shell_output_filters_for_foreground_input(
                            descriptor.pane_id.as_str(),
                        );
                        self.clear_copy_state_for_surface(
                            descriptor.pane_id.as_str(),
                            crate::runtime::PaneSurfaceKind::Process,
                        );
                        pane_input_effects.push(self.deferred_pane_input_effect(
                            descriptor.pane_id.to_string(),
                            input.clone(),
                        ));
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
                            report.registry_persistence_required |=
                                Self::mux_action_requires_registry_persistence(*action);
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
                            report.registry_persistence_required = true;
                            self.append_primary_client_event(
                                primary_client_id,
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
                    let overlay_was_open = self.presentation.primary_display_overlay.is_some();
                    match self.apply_attached_mouse_action(
                        primary_client_id,
                        action.clone(),
                        queue_external_effects,
                        suppress_host_clipboard_copy,
                    ) {
                        Ok((true, client_clipboard_write)) => {
                            report.mouse_actions_reported =
                                report.mouse_actions_reported.saturating_add(1);
                            report.registry_persistence_required |=
                                Self::mouse_action_requires_registry_persistence(action);
                            report.client_clipboard_write =
                                client_clipboard_write.map(AttachedClientClipboardWrite::new);
                            report.view_refresh_required = true;
                            if Self::mouse_action_requires_full_redraw(action.clone())
                                || overlay_was_open
                                    != self.presentation.primary_display_overlay.is_some()
                            {
                                report.full_redraw_required = true;
                            }
                        }
                        Ok((false, _)) => {
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
                    match self
                        .apply_attached_copy_mode_action(*action, suppress_host_clipboard_copy)
                    {
                        Ok((true, client_clipboard_write)) => {
                            report.view_refresh_required = true;
                            report.client_clipboard_write =
                                client_clipboard_write.map(AttachedClientClipboardWrite::new);
                        }
                        Ok((false, _)) => {
                            report
                                .unsupported_actions
                                .push(format!("copy-mode:{action:?}"));
                        }
                        Err(error) => self.present_attached_action_error(&mut report, &error)?,
                    }
                }
                TerminalClientLoopAction::EnterPrefixKeyMode => {
                    self.presentation.primary_prefix_key_pending = true;
                    report.view_refresh_required = true;
                }
                TerminalClientLoopAction::ReportUnboundPrefix(chord) => report
                    .unsupported_actions
                    .push(format!("prefix:unbound:{chord:?}")),
            }
        }

        if report.registry_persistence_required && !defer_pane_io {
            self.persist_or_defer_registry_update()?;
        }
        Ok((report, pane_input_effects))
    }

    /// Returns whether a successful mux action changes registry-visible state.
    fn mux_action_requires_registry_persistence(action: MuxAction) -> bool {
        matches!(
            action,
            MuxAction::NewWindow
                | MuxAction::NewGroup
                | MuxAction::BreakPaneToNewWindow
                | MuxAction::DetachPrimaryClient
        )
    }

    /// Returns whether a successful mouse action can run a registry-visible command.
    fn mouse_action_requires_registry_persistence(action: &MouseAction) -> bool {
        matches!(action, MouseAction::ReleaseWindowAction { .. })
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
                | MuxAction::EditAgentPrompt
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
