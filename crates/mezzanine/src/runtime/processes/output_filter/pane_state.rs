//! Pane-output filtering, OSC transaction parsing, screen application, and cleanup timers.

use super::super::*;
use crate::runtime::{RuntimeTimerKey, RuntimeTimerKind};

/// Carries Pane Output Render Mode state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneOutputRenderMode {
    /// Represents the Normal case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Normal,
    /// Represents the Hidden Live Agent Shell case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HiddenLiveAgentShell,
    /// Represents the Hidden Retained Agent Shell case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HiddenRetainedAgentShell,
    /// Represents the Verbose Agent Action case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    VerboseAgentAction,
    /// Represents the Trace case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Trace,
}

impl RuntimeSessionService {
    /// Runs the apply pane process output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn apply_pane_process_output(
        &mut self,
        output: PaneProcessOutput,
        terminal_title_panes: &mut BTreeSet<String>,
    ) -> Result<PaneOutputUpdate> {
        let descriptor = self.find_pane_descriptor(&output.pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane output has no matching pane",
            )
        })?;
        let descriptor_size = descriptor.size;
        let process_presentation_size = self
            .session
            .windows()
            .iter()
            .find(|window| window.id == descriptor.window_id)
            .and_then(|window| self.pane_presentation_size_for(window, descriptor.pane_id.as_str()))
            .unwrap_or(descriptor_size);
        let descriptor_window_id = descriptor.window_id.to_string();
        let background = self
            .session
            .active_window()
            .is_none_or(|window| window.active_pane().id.as_str() != descriptor.pane_id.as_str());
        let render_mode = self.pane_output_render_mode(output.pane_id.as_str());
        let transaction_bytes =
            self.visible_pane_output_bytes(output.pane_id.as_str(), &output.bytes);
        let protocol_bytes =
            self.decoded_pane_output_bytes(output.pane_id.as_str(), &transaction_bytes);
        let render_bytes = self.renderable_decoded_pane_output_bytes(
            output.pane_id.as_str(),
            render_mode,
            &protocol_bytes,
        );
        let (mut osc_events, _, _, _) = self.terminal_protocol_observation_for_pane_bytes(
            output.pane_id.as_str(),
            descriptor_size,
            &transaction_bytes,
        )?;
        let previous_process_alternate_active = self
            .process
            .process_pane_screens
            .get(output.pane_id.as_str())
            .is_some_and(TerminalScreen::alternate_screen_active);
        let previous_process_alternate_generation = self
            .process
            .process_pane_screens
            .get(output.pane_id.as_str())
            .map_or(0, TerminalScreen::alternate_screen_generation);
        let previous_process_activity_events = self
            .process
            .process_pane_screens
            .get(output.pane_id.as_str())
            .map_or(0, TerminalScreen::activity_events);
        let previous_process_bell_events = self
            .process
            .process_pane_screens
            .get(output.pane_id.as_str())
            .map_or(0, TerminalScreen::bell_events);
        let process_screen = self
            .process
            .process_pane_screens
            .entry(output.pane_id.clone())
            .or_insert(TerminalScreen::new_with_history_config(
                process_presentation_size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?);
        process_screen.resize(process_presentation_size);
        if render_mode == PaneOutputRenderMode::Normal {
            process_screen.feed(&protocol_bytes);
        } else {
            process_screen.feed_protocol_preserving_content(&protocol_bytes);
        }
        let terminal_response_bytes = process_screen.drain_terminal_response_bytes();
        osc_events.extend(
            process_screen
                .drain_osc_events()
                .into_iter()
                .filter(|event| !matches!(event, TerminalOscEvent::ShellIntegration { .. })),
        );
        let process_alternate_active = process_screen.alternate_screen_active();
        let process_screen_switched =
            process_screen.alternate_screen_generation() != previous_process_alternate_generation;
        let (activity_events, bell_events) = match render_mode {
            PaneOutputRenderMode::Normal => (
                process_screen
                    .activity_events()
                    .saturating_sub(previous_process_activity_events),
                process_screen
                    .bell_events()
                    .saturating_sub(previous_process_bell_events),
            ),
            PaneOutputRenderMode::VerboseAgentAction | PaneOutputRenderMode::Trace => {
                let (previous_activity_events, previous_bell_events) = self
                    .agent_pane_screen(output.pane_id.as_str())
                    .map(|screen| (screen.activity_events(), screen.bell_events()))
                    .unwrap_or_default();
                self.append_agent_pty_diagnostic_bytes_to_terminal_buffer(
                    output.pane_id.as_str(),
                    &render_bytes,
                )?;
                let screen = self
                    .agent_pane_screen(output.pane_id.as_str())
                    .ok_or_else(|| {
                        MezError::invalid_state("agent PTY presentation target screen not found")
                    })?;
                (
                    screen
                        .activity_events()
                        .saturating_sub(previous_activity_events),
                    screen.bell_events().saturating_sub(previous_bell_events),
                )
            }
            PaneOutputRenderMode::HiddenLiveAgentShell
            | PaneOutputRenderMode::HiddenRetainedAgentShell => (0, 0),
        };
        if !terminal_response_bytes.is_empty() {
            self.write_runtime_pane_input_priority(
                output.pane_id.as_str(),
                &terminal_response_bytes,
            )?;
        }
        let previous_alternate_active = previous_process_alternate_active;
        let alternate_active = process_alternate_active;
        let alternate_screen_exited = previous_alternate_active && !alternate_active;
        let alternate_screen_switched = process_screen_switched;
        let terminal_title = osc_events.iter().rev().find_map(|event| match event {
            TerminalOscEvent::TitleChanged { title } => Some(title.clone()),
            _ => None,
        });
        if terminal_title.is_some() {
            terminal_title_panes.insert(output.pane_id.clone());
        }
        self.apply_terminal_osc_events(&osc_events)?;
        if alternate_active {
            self.process.pane_readiness_overrides.revoke(
                output.pane_id.as_str(),
                ReadinessOverrideRevocation::AlternateScreenEntry,
            );
            self.set_pane_readiness(
                output.pane_id.as_str(),
                PaneReadinessState::InteractiveBlocked,
            );
        } else if alternate_screen_exited {
            let _ = self.observe_passive_shell_prompt_candidate(
                output.pane_id.as_str(),
                "alternate-screen-exit",
            )?;
        }
        self.record_running_shell_transaction_output(output.pane_id.as_str(), &transaction_bytes);
        self.observe_agent_shell_transaction_events(output.pane_id.as_str(), &osc_events)?;
        self.write_active_pane_pipe(output.pane_id.as_str(), &render_bytes)?;
        let title_changed = if let Some(title) = terminal_title {
            let foreground_group = self
                .process
                .pane_foreground_process_groups
                .get(output.pane_id.as_str())
                .copied()
                .or_else(|| {
                    self.process
                        .pane_processes
                        .foreground_process_group_id(output.pane_id.as_str())
                })
                .unwrap_or(output.primary_pid);
            self.set_pane_title_from_program_output(
                output.pane_id.as_str(),
                title,
                foreground_group,
            )?
        } else {
            false
        };

        let update = PaneOutputUpdate {
            session_id: self.session.id.to_string(),
            window_id: descriptor_window_id,
            pane_id: output.pane_id,
            primary_pid: output.primary_pid,
            bytes_read: output.bytes.len(),
            activity_events,
            bell_events,
            background,
            invalidate_output_frame: alternate_screen_switched,
        };
        self.append_pane_output_event(&update)?;
        if title_changed {
            self.append_pane_title_event(&update)?;
        }
        Ok(update)
    }

    /// Returns pane bytes that should be retained for active Mezzanine-owned
    /// shell transactions after filtering wrapper echo that is irrelevant to the
    /// model and the runtime state machine.
    ///
    /// Interactive shells echo the wrapper lines that Mezzanine writes around
    /// agent actions, readiness probes, and bootstrap probes. Those lines are
    /// implementation traffic, not user commands, so normal transaction
    /// observation hides them while preserving command output and the OSC
    /// transaction markers that drive the runtime state machine. Trace logging
    /// disables this filter for diagnosis.
    pub(crate) fn visible_pane_output_bytes(&mut self, pane_id: &str, bytes: &[u8]) -> Vec<u8> {
        if bytes.is_empty() {
            return Vec::new();
        }
        let active_transaction = self
            .process
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == pane_id);
        let filter_commands = self.mez_wrapper_filter_commands_for_pane(pane_id);
        if self.agent_trace_enabled(pane_id)
            || (filter_commands.is_empty()
                && !mez_wrapper_filter_bytes_may_contain_boilerplate(bytes))
        {
            let mut visible = self
                .process
                .pane_mez_wrapper_filter_pending
                .remove(pane_id)
                .unwrap_or_default();
            visible.extend_from_slice(bytes);
            if !active_transaction {
                self.tick_mez_wrapper_filter_retention(pane_id);
            }
            return visible;
        }

        let mut pending = self
            .process
            .pane_mez_wrapper_filter_pending
            .remove(pane_id)
            .unwrap_or_default();
        pending.extend_from_slice(bytes);
        let mut visible = Vec::with_capacity(pending.len());
        let mut filtered_wrapper_echo = false;
        let mut line_start = 0usize;
        while let Some(relative_terminator) = pending[line_start..]
            .iter()
            .position(|byte| *byte == b'\n' || *byte == b'\r')
        {
            let terminator = line_start + relative_terminator;
            let line_end = if pending[terminator] == b'\r'
                && pending
                    .get(terminator + 1)
                    .is_some_and(|byte| *byte == b'\n')
            {
                terminator + 2
            } else {
                terminator + 1
            };
            let line = &pending[line_start..line_end];
            let filtered_line = mez_wrapper_echo_line_visible_bytes(line, &filter_commands);
            if filtered_line.len() != line.len() {
                filtered_wrapper_echo = true;
            }
            visible.extend_from_slice(&filtered_line);
            line_start = line_end;
        }

        if line_start < pending.len() {
            let tail = &pending[line_start..];
            if tail.contains(&0x1b) {
                let filtered_tail = mez_wrapper_echo_line_visible_bytes(tail, &filter_commands);
                if filtered_tail.len() != tail.len() {
                    filtered_wrapper_echo = true;
                }
                visible.extend_from_slice(&filtered_tail);
            } else if mez_wrapper_echo_line_is_hidden(tail, &filter_commands) {
                filtered_wrapper_echo = true;
            } else if !mez_wrapper_echo_line_is_possible_prefix(tail, &filter_commands) {
                visible.extend_from_slice(tail);
            } else {
                filtered_wrapper_echo = true;
                self.process
                    .pane_mez_wrapper_filter_pending
                    .insert(pane_id.to_string(), tail.to_vec());
            }
        }
        if !active_transaction {
            if filtered_wrapper_echo {
                self.process.pane_mez_wrapper_filter_recent_polls.insert(
                    pane_id.to_string(),
                    RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS,
                );
            } else {
                self.tick_mez_wrapper_filter_retention(pane_id);
            }
        }
        visible
    }

    /// Runs the pane output render mode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_output_render_mode(&self, pane_id: &str) -> PaneOutputRenderMode {
        let shell_view_enabled = self.agent_shell_view_enabled(pane_id);
        let mut has_agent_action = false;
        for transaction in self
            .process
            .running_shell_transactions
            .values()
            .filter(|transaction| transaction.pane_id == pane_id)
        {
            match &transaction.kind {
                RunningShellTransactionKind::AgentAction { .. } => {
                    has_agent_action = true;
                }
                RunningShellTransactionKind::ReadinessProbe
                | RunningShellTransactionKind::Bootstrap
                | RunningShellTransactionKind::ShellIdentityProbe { .. }
                | RunningShellTransactionKind::PathResolution { .. }
                | RunningShellTransactionKind::EnvironmentEvidence { .. }
                | RunningShellTransactionKind::BubblewrapCapabilityProbe { .. } => {
                    return PaneOutputRenderMode::HiddenLiveAgentShell;
                }
            }
        }
        if has_agent_action {
            if self.agent_trace_enabled(pane_id)
                && self
                    .agent_shell_store()
                    .get(pane_id)
                    .is_some_and(|session| {
                        session.visibility != crate::runtime::AgentShellVisibility::Hidden
                    })
            {
                PaneOutputRenderMode::Trace
            } else if shell_view_enabled {
                PaneOutputRenderMode::VerboseAgentAction
            } else {
                PaneOutputRenderMode::HiddenLiveAgentShell
            }
        } else if !shell_view_enabled
            && (self.pane_has_running_agent_turn(pane_id)
                || self.pane_agent_subshell_active(pane_id))
        {
            PaneOutputRenderMode::HiddenLiveAgentShell
        } else if !shell_view_enabled
            && self
                .process
                .pane_hidden_shell_render_recent_polls
                .contains_key(pane_id)
        {
            PaneOutputRenderMode::HiddenRetainedAgentShell
        } else {
            PaneOutputRenderMode::Normal
        }
    }

    /// Runs the renderable pane output bytes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub(crate) fn renderable_pane_output_bytes(
        &mut self,
        pane_id: &str,
        transaction_bytes: &[u8],
    ) -> Vec<u8> {
        let render_mode = self.pane_output_render_mode(pane_id);
        let decoded = self.decoded_pane_output_bytes(pane_id, transaction_bytes);
        self.renderable_decoded_pane_output_bytes(pane_id, render_mode, &decoded)
    }

    /// Decodes private shell-output frames independently of display visibility.
    fn decoded_pane_output_bytes(&mut self, pane_id: &str, transaction_bytes: &[u8]) -> Vec<u8> {
        let begin = SHELL_OUTPUT_BASE64_BEGIN_MARKER.as_bytes();
        let end = SHELL_OUTPUT_BASE64_END_MARKER.as_bytes();
        let mut pending = self
            .process
            .pane_shell_output_render_pending
            .remove(pane_id)
            .unwrap_or_default();
        pending.extend_from_slice(transaction_bytes);

        if let Some(begin_index) = find_byte_subsequence(&pending, begin) {
            let before = renderable_shell_transaction_bytes(&pending[..begin_index]);
            if find_byte_subsequence(&pending[begin_index + begin.len()..], end).is_some() {
                let mut decoded = before;
                decoded.extend(renderable_shell_transaction_bytes(&pending[begin_index..]));
                return decoded;
            }
            if pending.len() <= SHELL_OUTPUT_BASE64_MAX_RAW_BYTES.saturating_mul(2) {
                self.process
                    .pane_shell_output_render_pending
                    .insert(pane_id.to_string(), pending[begin_index..].to_vec());
            }
            return before;
        }

        let partial_len = (1..begin.len().min(pending.len() + 1))
            .rev()
            .find(|length| pending.ends_with(&begin[..*length]))
            .unwrap_or(0);
        let decode_end = pending.len().saturating_sub(partial_len);
        let decoded = renderable_shell_transaction_bytes(&pending[..decode_end]);
        if partial_len > 0 {
            self.process
                .pane_shell_output_render_pending
                .insert(pane_id.to_string(), pending[decode_end..].to_vec());
        }
        decoded
    }

    /// Applies display visibility policy to already-decoded process output.
    fn renderable_decoded_pane_output_bytes(
        &mut self,
        pane_id: &str,
        render_mode: PaneOutputRenderMode,
        decoded: &[u8],
    ) -> Vec<u8> {
        match render_mode {
            PaneOutputRenderMode::Normal
            | PaneOutputRenderMode::VerboseAgentAction
            | PaneOutputRenderMode::Trace => decoded.to_vec(),
            PaneOutputRenderMode::HiddenLiveAgentShell => {
                if !decoded.is_empty() {
                    self.remember_hidden_shell_render_suppression(pane_id);
                }
                Vec::new()
            }
            PaneOutputRenderMode::HiddenRetainedAgentShell => Vec::new(),
        }
    }

    /// Reports whether the pane has a runtime agent turn currently occupying
    /// the pane's agent shell session.
    fn pane_has_running_agent_turn(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
    }

    /// Reports whether a pane currently owns a child shell for agent mode.
    ///
    /// The child shell's prompt and setup repaint are implementation traffic
    /// unless shell-view diagnostics are enabled.
    fn pane_agent_subshell_active(&self, pane_id: &str) -> bool {
        self.agent_subshell_is_active(pane_id)
    }

    /// Retains short-lived shell-output suppression after a hidden agent shell
    /// transaction so delayed prompt repaint bytes do not leak into the pane.
    pub(crate) fn remember_hidden_shell_render_suppression(&mut self, pane_id: &str) {
        self.process.pane_hidden_shell_render_recent_polls.insert(
            pane_id.to_string(),
            RUNTIME_HIDDEN_SHELL_RENDER_RETENTION_POLLS,
        );
    }

    /// Clears retained shell-output filters for explicit foreground input.
    ///
    /// Hidden-shell and wrapper-echo retention suppress delayed implementation
    /// prompt repaint bytes after agent-owned shell work. Once foreground
    /// control returns to the pane, following PTY output belongs to the user's
    /// interaction and must not be swallowed or reduced to cursor-control
    /// remnants by the previous agent turn's cleanup window.
    pub(crate) fn clear_shell_output_filters_for_foreground_input(&mut self, pane_id: &str) {
        self.process
            .pane_hidden_shell_render_recent_polls
            .remove(pane_id);
        self.process.pane_mez_wrapper_filter_pending.remove(pane_id);
        self.process
            .pane_mez_wrapper_filter_recent_commands
            .remove(pane_id);
        self.process
            .pane_mez_wrapper_filter_recent_polls
            .remove(pane_id);
    }

    /// Ages out retained shell-output suppression for panes whose agent turn and
    /// Mezzanine-owned shell transaction have both settled.
    pub(crate) fn tick_hidden_shell_render_retention(&mut self) -> usize {
        let mut aged = 0usize;
        let retained = self
            .process
            .pane_hidden_shell_render_recent_polls
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for pane_id in retained {
            if self.pane_has_running_agent_turn(&pane_id)
                || self
                    .process
                    .running_shell_transactions
                    .values()
                    .any(|transaction| transaction.pane_id == pane_id)
            {
                continue;
            }
            let Some(remaining) = self
                .process
                .pane_hidden_shell_render_recent_polls
                .get_mut(&pane_id)
            else {
                continue;
            };
            *remaining = remaining.saturating_sub(1);
            aged = aged.saturating_add(1);
            if *remaining == 0 {
                self.process
                    .pane_hidden_shell_render_recent_polls
                    .remove(&pane_id);
            }
        }
        aged
    }

    /// Applies runtime idle-cleanup timer work while honoring actor-owned
    /// progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn apply_idle_cleanup_timer_event_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        match self.session.lifecycle_state() {
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed => Ok(0),
            RuntimeLifecycleState::Running
            | RuntimeLifecycleState::Detached
            | RuntimeLifecycleState::Stopping => {
                let hidden_shell_retention_aged = self.tick_hidden_shell_render_retention();
                let reconciled = self.reconcile_agent_runtime_progress_paths_with_actor_progress(
                    actor_progress_turn_ids,
                )?;
                Ok(hidden_shell_retention_aged.saturating_add(reconciled))
            }
        }
    }

    /// Reconciles running agent turns while honoring actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Turns waiting on progress owned by the
    ///   async actor, such as provider retry timers that are not represented in
    ///   service-owned queues.
    pub fn reconcile_agent_runtime_progress_paths_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        if matches!(
            self.session.lifecycle_state(),
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed
        ) {
            return Ok(0);
        }
        let missing_pane_failures = self.fail_agent_turns_for_missing_panes()?;
        let stranded_shell_recoveries = self
            .recover_stranded_agent_shell_dispatches_with_actor_progress(actor_progress_turn_ids)?;
        let unreachable_turn_failures =
            self.fail_unreachable_running_agent_turns_with_actor_progress(actor_progress_turn_ids)?;
        Ok(missing_pane_failures
            .saturating_add(stranded_shell_recoveries)
            .saturating_add(unreachable_turn_failures))
    }

    /// Reports whether actor-owned idle cleanup should remain scheduled while
    /// honoring actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn idle_cleanup_timer_needed_with_actor_progress(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        self.missing_pane_agent_turn_cleanup_needed()
            || self.hidden_shell_render_retention_timer_needed()
            || self.stranded_agent_shell_dispatch_recovery_timer_needed()
            || self.unreachable_running_agent_turn_timer_needed_with_actor_progress(
                actor_progress_turn_ids,
            )
    }

    /// Builds the desired idle-cleanup timer transition for an external timer adapter.
    pub(crate) fn idle_cleanup_timer_transition_with_actor_progress(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
        timer_active: bool,
        generation: u64,
        retention_delay_ms: u64,
        recovery_delay_ms: u64,
    ) -> RuntimeTransition {
        if timer_active
            || !self.idle_cleanup_timer_needed_with_actor_progress(actor_progress_turn_ids)
        {
            return RuntimeTransition::default();
        }
        let delay_ms = if self.hidden_shell_render_retention_timer_needed() {
            retention_delay_ms
        } else {
            recovery_delay_ms
        };
        RuntimeTransition {
            applied: false,
            side_effects: vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(RuntimeTimerKind::IdleCleanup, "session", generation),
                delay_ms,
            }],
        }
    }

    /// Reports whether hidden shell-render suppression still needs to age out.
    pub fn hidden_shell_render_retention_timer_needed(&self) -> bool {
        !self
            .process
            .pane_hidden_shell_render_recent_polls
            .is_empty()
    }

    /// Reports whether any pending agent shell dispatch may need recovery.
    pub fn stranded_agent_shell_dispatch_recovery_timer_needed(&self) -> bool {
        !self
            .stranded_agent_shell_dispatch_recovery_candidates()
            .is_empty()
    }

    /// Reports whether any running turn has no remaining runtime progress path
    /// after accounting for async actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn unreachable_running_agent_turn_timer_needed_with_actor_progress(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        !self
            .unreachable_running_agent_turn_candidates(actor_progress_turn_ids)
            .is_empty()
    }

    /// Runs the terminal osc events for pane bytes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub(crate) fn terminal_osc_events_for_pane_bytes(
        &mut self,
        pane_id: &str,
        size: Size,
        bytes: &[u8],
    ) -> Result<(Vec<TerminalOscEvent>, bool, bool)> {
        let (events, alternate_active, screen_switched, _) =
            self.terminal_protocol_observation_for_pane_bytes(pane_id, size, bytes)?;
        Ok((events, alternate_active, screen_switched))
    }

    /// Observes process-owned terminal protocol state without choosing a display surface.
    fn terminal_protocol_observation_for_pane_bytes(
        &mut self,
        pane_id: &str,
        size: Size,
        bytes: &[u8],
    ) -> Result<(Vec<TerminalOscEvent>, bool, bool, Vec<u8>)> {
        if bytes.is_empty() {
            return Ok((Vec::new(), false, false, Vec::new()));
        }
        let screen =
            if let Some(screen) = self.process.pane_transaction_osc_screens.get_mut(pane_id) {
                screen.resize(size);
                screen
            } else {
                self.process.pane_transaction_osc_screens.insert(
                    pane_id.to_string(),
                    TerminalScreen::new_with_history_config(
                        size,
                        self.process.settings.terminal_history_limit,
                        self.process.settings.terminal_history_rotate_lines,
                    )?,
                );
                self.process
                    .pane_transaction_osc_screens
                    .get_mut(pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("transaction OSC parser was not retained for pane")
                    })?
            };
        let previous_alternate_generation = screen.alternate_screen_generation();
        screen.feed(bytes);
        let alternate_screen_switched =
            screen.alternate_screen_generation() != previous_alternate_generation;
        let terminal_response_bytes = screen.drain_terminal_response_bytes();
        let events = screen
            .drain_osc_events()
            .into_iter()
            .filter_map(|event| match event {
                TerminalOscEvent::ShellIntegration { payload } => {
                    parse_mez_shell_transaction_osc(&format!("133;{payload}"))
                }
                _ => None,
            })
            .collect();
        Ok((
            events,
            screen.alternate_screen_active(),
            alternate_screen_switched,
            terminal_response_bytes,
        ))
    }

    /// Runs the remember mez wrapper filter command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn remember_mez_wrapper_filter_command(&mut self, pane_id: &str, command: &str) {
        let retained = self
            .process
            .pane_mez_wrapper_filter_recent_commands
            .entry(pane_id.to_string())
            .or_default();
        for line in command
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if !retained.iter().any(|existing| existing == line) {
                retained.push(line.to_string());
            }
        }
        let extra = retained
            .len()
            .saturating_sub(RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT);
        if extra > 0 {
            retained.drain(0..extra);
        }
        self.process.pane_mez_wrapper_filter_recent_polls.insert(
            pane_id.to_string(),
            RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS,
        );
    }

    /// Runs the mez wrapper filter commands for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mez_wrapper_filter_commands_for_pane(&self, pane_id: &str) -> Vec<String> {
        let mut commands = self
            .process
            .running_shell_transactions
            .values()
            .filter(|transaction| transaction.pane_id == pane_id)
            .flat_map(|transaction| {
                transaction
                    .command
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        if let Some(retained) = self
            .process
            .pane_mez_wrapper_filter_recent_commands
            .get(pane_id)
        {
            for command in retained {
                if !commands.iter().any(|existing| existing == command) {
                    commands.push(command.clone());
                }
            }
        }
        commands
    }

    /// Runs the tick mez wrapper filter retention operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn tick_mez_wrapper_filter_retention(&mut self, pane_id: &str) {
        let Some(remaining) = self
            .process
            .pane_mez_wrapper_filter_recent_polls
            .get_mut(pane_id)
        else {
            return;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            self.process
                .pane_mez_wrapper_filter_recent_polls
                .remove(pane_id);
            self.process
                .pane_mez_wrapper_filter_recent_commands
                .remove(pane_id);
            self.process.pane_mez_wrapper_filter_pending.remove(pane_id);
        }
    }
}
