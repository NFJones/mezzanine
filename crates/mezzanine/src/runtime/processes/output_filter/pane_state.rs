//! Pane-output filtering, OSC transaction parsing, screen application, and cleanup timers.

use super::super::*;
use crate::runtime::{RuntimeTimerKey, RuntimeTimerKind};
use base64::Engine as _;

/// Maximum encoded line retained while incrementally rendering one output frame.
///
/// Generated Base64 lines are 76 bytes. The larger ceiling leaves room for a
/// complete marker and defensive compatibility without permitting an
/// unterminated line to grow with transaction output.
const RUNTIME_SHELL_OUTPUT_RENDER_LINE_LIMIT_BYTES: usize = 4 * 1024;

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
    /// Applies the shell-native empty-line repaint before hiding handoff traffic.
    ManagedEditorClear,
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
        let transaction_bytes =
            self.visible_pane_output_bytes(output.pane_id.as_str(), &output.bytes);
        let render_mode = self.pane_output_render_mode(output.pane_id.as_str());
        let protocol_bytes =
            self.decoded_pane_output_bytes(output.pane_id.as_str(), &transaction_bytes);
        let (mut osc_events, _, _, _) = self.terminal_protocol_observation_for_pane_bytes(
            output.pane_id.as_str(),
            descriptor_size,
            &transaction_bytes,
        )?;
        let mut restored_foreign_parent = None;
        for event in &osc_events {
            let TerminalOscEvent::ForeignShellLoaderExited { marker, exit_code } = event else {
                continue;
            };
            if self.observe_agent_shell_transaction_events(
                output.pane_id.as_str(),
                std::slice::from_ref(event),
            )? > 0
            {
                restored_foreign_parent = Some((marker.as_str(), *exit_code));
                break;
            }
        }
        let restored_prompt_offset = restored_foreign_parent.and_then(|(marker, exit_code)| {
            Self::foreign_shell_loader_exit_end_offset(&protocol_bytes, marker, exit_code)
        });
        let settled_render_mode = restored_prompt_offset
            .map(|_| self.pane_output_render_mode(output.pane_id.as_str()))
            .unwrap_or(render_mode);
        let render_bytes = if let Some(offset) = restored_prompt_offset {
            self.renderable_decoded_pane_output_bytes(
                output.pane_id.as_str(),
                settled_render_mode,
                &protocol_bytes[offset..],
            )
        } else {
            self.renderable_decoded_pane_output_bytes(
                output.pane_id.as_str(),
                render_mode,
                &protocol_bytes,
            )
        };
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
        let mut synchronized_output = mez_terminal::SynchronizedOutputFeedOutcome::default();
        if let Some(offset) = restored_prompt_offset {
            synchronized_output
                .merge(process_screen.feed_protocol_preserving_content(&protocol_bytes[..offset]));
            if matches!(
                settled_render_mode,
                PaneOutputRenderMode::Normal | PaneOutputRenderMode::ManagedEditorClear
            ) {
                // Hidden loader traffic preserves the retained parent cursor.
                // Some shells emit the restored prompt immediately after the
                // loader-exit record without a carriage return, so normalize
                // that repaint to the beginning of the retained prompt row.
                synchronized_output.merge(process_screen.feed(b"\r"));
                synchronized_output.merge(process_screen.feed(&protocol_bytes[offset..]));
            } else {
                synchronized_output.merge(
                    process_screen.feed_protocol_preserving_content(&protocol_bytes[offset..]),
                );
            }
        } else if matches!(
            render_mode,
            PaneOutputRenderMode::Normal | PaneOutputRenderMode::ManagedEditorClear
        ) {
            synchronized_output.merge(process_screen.feed(&protocol_bytes));
        } else {
            synchronized_output
                .merge(process_screen.feed_protocol_preserving_content(&protocol_bytes));
        }
        let defer_render =
            process_screen.synchronized_output_active() && !synchronized_output.released;
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
            | PaneOutputRenderMode::HiddenRetainedAgentShell
            | PaneOutputRenderMode::ManagedEditorClear => (0, 0),
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
        let progress_changed = osc_events
            .iter()
            .filter_map(|event| match event {
                TerminalOscEvent::Progress(progress) => Some(*progress),
                _ => None,
            })
            .fold(false, |changed, progress| {
                let pane_id = output.pane_id.as_str();
                let event_changed = match progress {
                    mez_terminal::TerminalProgressState::Clear => self
                        .process
                        .pane_terminal_progress
                        .remove(pane_id)
                        .is_some(),
                    progress => {
                        self.process.pane_terminal_progress.get(pane_id) != Some(&progress) && {
                            self.process
                                .pane_terminal_progress
                                .insert(pane_id.to_string(), progress);
                            true
                        }
                    }
                };
                changed || event_changed
            });
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
        if self.agent_subshell_input_clear_is_pending(output.pane_id.as_str())
            && transaction_bytes
                .iter()
                .any(|byte| matches!(*byte, b'\r' | b'\n'))
            && self.pane_foreground_certified_shell_state(output.pane_id.as_str()) == Some(true)
        {
            let _ = self.observe_passive_shell_prompt_candidate(
                output.pane_id.as_str(),
                "non-native-input-clear-output",
            )?;
        }
        let resumes_non_native_entry = self
            .agent_subshell_input_clear_is_pending(output.pane_id.as_str())
            && matches!(
                self.pane_readiness_state(output.pane_id.as_str()),
                PaneReadinessState::Ready | PaneReadinessState::PromptCandidate
            );
        if (self.pane_readiness_state(output.pane_id.as_str()) == PaneReadinessState::Ready
            && self
                .bash_receiver_token_for_pane(output.pane_id.as_str())
                .is_some()
            || resumes_non_native_entry)
            && self.agent_subshell_entry_is_deferred(output.pane_id.as_str())
            && !self.agent_subshell_is_active(output.pane_id.as_str())
            && self
                .agent_shell_store()
                .get(output.pane_id.as_str())
                .is_some_and(|session| {
                    session.visibility == mez_agent::AgentShellVisibility::Visible
                })
        {
            // Process every event in the PTY batch before sending another
            // private Bash trigger. Receiver completion and the restored
            // prompt can share one read, so waiting for a later batch can
            // strand re-entry while resuming inside the completion event can
            // race the `bind -x` callback teardown.
            let _ = self.enter_agent_subshell_if_needed(output.pane_id.as_str())?;
        }
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
            invalidate_output_frame: alternate_screen_switched
                || synchronized_output.full_redraw
                || progress_changed,
            defer_render,
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
        let exit_echo_visible = self.visible_agent_subshell_exit_echo_bytes(pane_id, bytes);
        if exit_echo_visible.is_empty() {
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
                && !mez_wrapper_filter_bytes_may_contain_boilerplate(&exit_echo_visible))
        {
            let mut visible = self
                .process
                .pane_mez_wrapper_filter_pending
                .remove(pane_id)
                .unwrap_or_default();
            visible.extend_from_slice(&exit_echo_visible);
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
        pending.extend_from_slice(&exit_echo_visible);
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
            let filtered_line = mez_wrapper_echo_line_visible_bytes(line, filter_commands.as_ref());
            if filtered_line.len() != line.len() {
                filtered_wrapper_echo = true;
            }
            visible.extend_from_slice(&filtered_line);
            line_start = line_end;
        }

        if line_start < pending.len() {
            let tail = &pending[line_start..];
            if tail.contains(&0x1b) {
                let filtered_tail =
                    mez_wrapper_echo_line_visible_bytes(tail, filter_commands.as_ref());
                if filtered_tail.len() != tail.len() {
                    filtered_wrapper_echo = true;
                }
                visible.extend_from_slice(&filtered_tail);
            } else if mez_wrapper_echo_line_is_hidden(tail, filter_commands.as_ref()) {
                filtered_wrapper_echo = true;
            } else if tail.len() > RUNTIME_SHELL_WRAPPER_FILTER_PENDING_LIMIT_BYTES
                || !mez_wrapper_echo_line_is_possible_prefix(tail, filter_commands.as_ref())
            {
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
        if self
            .process
            .pane_managed_shell_handoffs
            .get(pane_id)
            .is_some_and(ManagedShellHandoff::editor_clear_render_is_pending)
        {
            return PaneOutputRenderMode::ManagedEditorClear;
        }
        if self.agent_subshell_input_clear_is_pending(pane_id)
            || self.agent_subshell_input_clear_was_completed(pane_id)
        {
            return PaneOutputRenderMode::HiddenLiveAgentShell;
        }
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
                RunningShellTransactionKind::FocusedShellHook
                | RunningShellTransactionKind::ReadinessProbe
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
            && (self
                .process
                .pane_agent_subshell_parent_return_pending
                .contains(pane_id)
                || self
                    .process
                    .pane_hidden_shell_render_recent_polls
                    .contains_key(pane_id))
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

    /// Locates the first prompt byte after one generated foreign-loader exit record.
    ///
    /// The dependency-free loader owns every byte through its correlated OSC
    /// record. Bytes after the record belong to the restored uninstrumented
    /// parent and may be presented under the post-settlement render policy.
    fn foreign_shell_loader_exit_end_offset(
        bytes: &[u8],
        marker: &str,
        exit_code: i32,
    ) -> Option<usize> {
        let record = format!(
            "\u{1b}]133;R;mez_foreign_loader=exited;mez_marker={marker};mez_status={exit_code}\u{1b}\\"
        );
        find_byte_subsequence(bytes, record.as_bytes())
            .map(|offset| offset.saturating_add(record.len()))
    }

    /// Decodes private shell-output frames independently of display visibility.
    fn decoded_pane_output_bytes(&mut self, pane_id: &str, transaction_bytes: &[u8]) -> Vec<u8> {
        let encoded_output_owned = self
            .process
            .shell_transaction_encoded_output_markers
            .iter()
            .any(|marker| {
                self.process
                    .running_shell_transactions
                    .get(marker)
                    .is_some_and(|transaction| transaction.pane_id == pane_id)
            });
        if !encoded_output_owned {
            self.process
                .pane_shell_output_render_pending
                .remove(pane_id);
            return transaction_bytes.to_vec();
        }
        let begin = SHELL_OUTPUT_BASE64_BEGIN_MARKER.as_bytes();
        let end = SHELL_OUTPUT_BASE64_END_MARKER.as_bytes();
        let mut state = self
            .process
            .pane_shell_output_render_pending
            .remove(pane_id)
            .unwrap_or_default();
        let mut input = std::mem::take(&mut state.marker_prefix);
        input.extend_from_slice(transaction_bytes);
        let mut decoded = Vec::new();
        let mut offset = 0usize;

        while offset < input.len() {
            if !state.in_frame {
                let remaining = &input[offset..];
                if let Some(begin_index) = find_byte_subsequence(remaining, begin) {
                    decoded.extend(renderable_shell_transaction_bytes(
                        &remaining[..begin_index],
                    ));
                    offset = offset
                        .saturating_add(begin_index)
                        .saturating_add(begin.len());
                    state.in_frame = true;
                    state.discard_frame = false;
                    state.encoded_line.clear();
                    continue;
                }
                let partial_len = (1..begin.len().min(remaining.len() + 1))
                    .rev()
                    .find(|length| remaining.ends_with(&begin[..*length]))
                    .unwrap_or(0);
                let render_end = remaining.len().saturating_sub(partial_len);
                decoded.extend(renderable_shell_transaction_bytes(&remaining[..render_end]));
                state
                    .marker_prefix
                    .extend_from_slice(&remaining[render_end..]);
                offset = input.len();
                continue;
            }

            let remaining = &input[offset..];
            let Some(newline) = remaining.iter().position(|byte| *byte == b'\n') else {
                if state.encoded_line.len().saturating_add(remaining.len())
                    > RUNTIME_SHELL_OUTPUT_RENDER_LINE_LIMIT_BYTES
                {
                    state.discard_frame = true;
                    state.encoded_line.clear();
                } else if !state.discard_frame {
                    state.encoded_line.extend_from_slice(remaining);
                }
                offset = input.len();
                continue;
            };
            if state.encoded_line.len().saturating_add(newline)
                <= RUNTIME_SHELL_OUTPUT_RENDER_LINE_LIMIT_BYTES
            {
                state.encoded_line.extend_from_slice(&remaining[..newline]);
            } else {
                state.discard_frame = true;
                state.encoded_line.clear();
            }
            offset = offset.saturating_add(newline).saturating_add(1);
            let line = state
                .encoded_line
                .strip_suffix(b"\r")
                .unwrap_or(&state.encoded_line);
            if line == end {
                state.in_frame = false;
                state.discard_frame = false;
                state.encoded_line.clear();
                continue;
            }
            if !state.discard_frame && !line.is_empty() {
                match base64::engine::general_purpose::STANDARD.decode(line) {
                    Ok(bytes) => decoded.extend(bytes),
                    Err(_) => state.discard_frame = true,
                }
            }
            state.encoded_line.clear();
        }

        if state.in_frame || !state.marker_prefix.is_empty() {
            self.process
                .pane_shell_output_render_pending
                .insert(pane_id.to_string(), state);
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
            PaneOutputRenderMode::ManagedEditorClear => Vec::new(),
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

    /// Retains the parent-owned boundary emitted after one agent subshell exits.
    pub(crate) fn remember_agent_subshell_exit_marker(&mut self, pane_id: &str, marker: Vec<u8>) {
        self.process
            .pane_agent_subshell_parent_return_pending
            .remove(pane_id);
        self.process
            .pane_agent_subshell_exit_markers
            .insert(pane_id.to_string(), marker);
    }

    /// Arms child-shell teardown suppression and retains the pre-entry process
    /// presentation until foreground input returns ownership to the parent.
    pub(crate) fn remember_agent_subshell_exit_echo(&mut self, pane_id: &str) {
        self.process
            .pane_agent_subshell_exit_echo_pending
            .insert(pane_id.to_string(), Vec::new());
        self.process
            .pane_agent_subshell_parent_return_pending
            .insert(pane_id.to_string());
    }

    /// Filters all child-owned teardown before general wrapper filtering.
    ///
    /// The parent wrapper emits an opaque marker after the child exits and
    /// cleanup completes. PTY bytes before that marker are never pane content;
    /// bytes after it belong to the restored parent shell. Exit handling keeps
    /// those initial parent bytes in protocol-preserving hidden rendering so
    /// prompt repaint and delayed Readline cleanup cannot replace the retained
    /// pre-entry prompt cursor.
    fn visible_agent_subshell_exit_echo_bytes(&mut self, pane_id: &str, bytes: &[u8]) -> Vec<u8> {
        let Some(mut pending) = self
            .process
            .pane_agent_subshell_exit_echo_pending
            .remove(pane_id)
        else {
            return bytes.to_vec();
        };
        pending.extend_from_slice(bytes);
        let Some(marker) = self
            .process
            .pane_agent_subshell_exit_markers
            .get(pane_id)
            .cloned()
        else {
            return pending;
        };
        if let Some(start) = find_byte_subsequence(&pending, &marker) {
            self.process
                .pane_agent_subshell_exit_markers
                .remove(pane_id);
            let _ = self.observe_managed_shell_child_exit_boundary(pane_id);
            let parent_bytes = &pending[start + marker.len()..];
            if self.agent_subshell_input_clear_was_completed(pane_id) {
                if let Some(parent_bytes) = parent_bytes.strip_prefix(b"^C\r\n") {
                    return parent_bytes.to_vec();
                }
                if let Some(parent_bytes) = parent_bytes.strip_prefix(b"^C\n") {
                    return parent_bytes.to_vec();
                }
            }
            return parent_bytes.to_vec();
        }
        let suffix_length = (1..marker.len().min(pending.len() + 1))
            .rev()
            .find(|length| pending.ends_with(&marker[..*length]))
            .unwrap_or(0);
        self.process.pane_agent_subshell_exit_echo_pending.insert(
            pane_id.to_string(),
            pending[pending.len().saturating_sub(suffix_length)..].to_vec(),
        );
        Vec::new()
    }

    /// Returns the registered parent exit boundary for focused regressions.
    #[cfg(test)]
    pub(crate) fn agent_subshell_exit_marker_for_tests(&self, pane_id: &str) -> Option<&[u8]> {
        self.process
            .pane_agent_subshell_exit_markers
            .get(pane_id)
            .map(Vec::as_slice)
    }

    /// Clears retained shell-output filters for explicit foreground input.
    ///
    /// Hidden-shell and wrapper-echo retention suppress delayed implementation
    /// prompt repaint bytes after agent-owned shell work. Once foreground
    /// control returns to the pane, following PTY output belongs to the user's
    /// interaction and must not be swallowed or reduced to cursor-control
    /// remnants by the previous agent turn's cleanup window.
    pub(crate) fn clear_shell_output_filters_for_foreground_input(&mut self, pane_id: &str) {
        self.clear_completed_agent_subshell_input_clear(pane_id);
        self.process
            .pane_hidden_shell_render_recent_polls
            .remove(pane_id);
        self.process
            .pane_agent_subshell_parent_return_pending
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

    /// Requests fresh parent-process proof after the restoration event deadline.
    ///
    /// A deadline alone never proves whether the parent editor, private
    /// receiver, or child owns the PTY. Recovery therefore retains queued
    /// foreground bytes and restoration ownership until the exact pane worker
    /// proves that the original parent process group is foreground again.
    fn recover_expired_managed_shell_parent_restorations_at(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        let expired = self
            .process
            .pane_managed_shell_handoffs
            .iter()
            .filter(|(_, handoff)| {
                handoff.recovery_observation().map_or_else(
                    || {
                        handoff.started_at_unix_ms().is_some_and(|started_at| {
                            now_unix_ms.saturating_sub(started_at)
                                >= RUNTIME_MANAGED_SHELL_PARENT_RESTORATION_TIMEOUT_MS
                        })
                    },
                    |pending| {
                        now_unix_ms.saturating_sub(pending.started_at_unix_ms)
                            >= RUNTIME_MANAGED_SHELL_PARENT_RESTORATION_TIMEOUT_MS
                    },
                )
            })
            .map(|(pane_id, _)| pane_id.clone())
            .collect::<Vec<_>>();
        let mut requested = 0usize;
        for pane_id in &expired {
            let Some(handoff) = self
                .process
                .pane_managed_shell_handoffs
                .get(pane_id)
                .cloned()
            else {
                continue;
            };
            self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
            let Some(instance) = self.adapter_owned_pane_process_instance(pane_id) else {
                if let Some(current) = self.process.pane_managed_shell_handoffs.get_mut(pane_id) {
                    let _ = reduce_managed_shell_handoff(
                        current,
                        ManagedShellHandoffEvent::RecoveryProofUnavailable,
                    );
                }
                self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","managed_shell_handoff":"proof_unavailable","marker":"{}"}}"#,
                        json_escape(pane_id),
                        json_escape(&handoff.identity().marker)
                    ),
                )?;
                continue;
            };
            let Some(primary_process_id) = handoff.identity().primary_process_id else {
                continue;
            };
            self.process.next_shell_dispatch_recovery_observation = self
                .process
                .next_shell_dispatch_recovery_observation
                .saturating_add(1);
            let observation_id = format!(
                "managed-shell-parent-restoration:{}:{}:{}",
                instance.generation,
                handoff.identity().marker,
                self.process.next_shell_dispatch_recovery_observation
            );
            let Some(current) = self.process.pane_managed_shell_handoffs.get_mut(pane_id) else {
                continue;
            };
            let transition = reduce_managed_shell_handoff(
                current,
                ManagedShellHandoffEvent::RecoveryProofRequested {
                    observation: ManagedShellRecoveryObservation {
                        instance: instance.clone(),
                        observation_id: observation_id.clone(),
                        started_at_unix_ms: now_unix_ms,
                    },
                },
            );
            if !transition
                .effects
                .contains(&ManagedShellHandoffEffect::RequestParentProof)
            {
                continue;
            }
            self.persistence
                .queue_pane_observation(RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id: observation_id.clone(),
                        expected_process_group_id: Some(primary_process_id),
                    },
                });
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","managed_shell_handoff":"proof_requested","marker":"{}","observation_id":"{}"}}"#,
                    json_escape(pane_id),
                    json_escape(&handoff.identity().marker),
                    json_escape(&observation_id)
                ),
            )?;
            requested = requested.saturating_add(1);
        }
        Ok(requested)
    }

    /// Fails closed when managed Bash never publishes protocol availability.
    fn recover_expired_managed_bash_admissions_at(&mut self, now_unix_ms: u64) -> Result<usize> {
        let expired = self
            .process
            .pane_bash_admissions
            .iter()
            .filter_map(|(pane_id, admission)| match admission {
                crate::runtime::processes::RuntimeManagedBashAdmission::Pending {
                    started_at_unix_ms: Some(started_at_unix_ms),
                    ..
                } if now_unix_ms.saturating_sub(*started_at_unix_ms)
                    >= crate::runtime::processes::RUNTIME_MANAGED_BASH_ADMISSION_TIMEOUT_MS =>
                {
                    Some(pane_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for pane_id in &expired {
            self.process.pane_bash_admissions.insert(
                pane_id.clone(),
                crate::runtime::processes::RuntimeManagedBashAdmission::Unavailable {
                    reason: "startup-admission-timeout".to_string(),
                },
            );
            self.clear_deferred_agent_subshell_entry(pane_id);
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: managed Bash integration unavailable (startup-admission-timeout)",
            )?;
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","managed_bash_admission":"timed_out"}}"#,
                    json_escape(pane_id)
                ),
            )?;
        }
        Ok(expired.len())
    }

    /// Fails closed when managed zsh never publishes startup admission.
    fn recover_expired_managed_zsh_admissions_at(&mut self, now_unix_ms: u64) -> Result<usize> {
        let expired = self
            .process
            .pane_zsh_admissions
            .iter()
            .filter_map(|(pane_id, admission)| match admission {
                crate::runtime::processes::RuntimeManagedZshAdmission::Pending {
                    started_at_unix_ms: Some(started_at_unix_ms),
                    ..
                } if now_unix_ms.saturating_sub(*started_at_unix_ms)
                    >= crate::runtime::processes::RUNTIME_MANAGED_ZSH_ADMISSION_TIMEOUT_MS =>
                {
                    Some(pane_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for pane_id in &expired {
            self.process.pane_zsh_admissions.insert(
                pane_id.clone(),
                crate::runtime::processes::RuntimeManagedZshAdmission::Unavailable {
                    reason: "startup-admission-timeout".to_string(),
                },
            );
            self.clear_deferred_agent_subshell_entry(pane_id);
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: managed zsh integration unavailable (startup-admission-timeout)",
            )?;
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","managed_zsh_admission":"timed_out"}}"#,
                    json_escape(pane_id)
                ),
            )?;
        }
        Ok(expired.len())
    }

    #[cfg(test)]
    /// Expires managed-shell parent restoration at a supplied time for deterministic tests.
    pub(crate) fn recover_expired_managed_shell_parent_restorations_for_tests(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        self.recover_expired_managed_shell_parent_restorations_at(now_unix_ms)
    }

    #[cfg(test)]
    /// Expires managed Bash startup admission at a supplied time for deterministic tests.
    pub(crate) fn recover_expired_managed_bash_admissions_for_tests(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        self.recover_expired_managed_bash_admissions_at(now_unix_ms)
    }

    #[cfg(test)]
    /// Expires managed zsh startup admission at a supplied time for deterministic tests.
    pub(crate) fn recover_expired_managed_zsh_admissions_for_tests(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        self.recover_expired_managed_zsh_admissions_at(now_unix_ms)
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
                let managed_shell_parent_restorations_recovered = self
                    .recover_expired_managed_shell_parent_restorations_at(current_unix_millis())?;
                let bash_admissions_recovered =
                    self.recover_expired_managed_bash_admissions_at(current_unix_millis())?;
                let zsh_admissions_recovered =
                    self.recover_expired_managed_zsh_admissions_at(current_unix_millis())?;
                let reconciled = self.reconcile_agent_runtime_progress_paths_with_actor_progress(
                    actor_progress_turn_ids,
                )?;
                Ok(hidden_shell_retention_aged
                    .saturating_add(managed_shell_parent_restorations_recovered)
                    .saturating_add(bash_admissions_recovered)
                    .saturating_add(zsh_admissions_recovered)
                    .saturating_add(reconciled))
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
            || self.managed_bash_admission_timer_needed()
            || self.managed_zsh_admission_timer_needed()
            || self
                .process
                .pane_managed_shell_handoffs
                .values()
                .any(|handoff| handoff.started_at_unix_ms().is_some())
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
    pub(crate) fn remember_mez_wrapper_filter_command(
        &mut self,
        pane_id: &str,
        command: &str,
    ) -> std::sync::Arc<[String]> {
        let descriptor: std::sync::Arc<[String]> = command
            .lines()
            .map(str::trim)
            .filter(|line| {
                !line.is_empty()
                    && line.len() <= RUNTIME_SHELL_WRAPPER_FILTER_COMMAND_LINE_LIMIT_BYTES
            })
            .fold(Vec::<String>::new(), |mut lines, line| {
                if let Some(index) = lines.iter().position(|existing| existing == line) {
                    lines.remove(index);
                }
                lines.push(line.to_string());
                let extra = lines
                    .len()
                    .saturating_sub(RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT);
                if extra > 0 {
                    lines.drain(0..extra);
                }
                lines
            })
            .into();
        self.remember_mez_wrapper_filter_descriptor(pane_id, descriptor.as_ref());
        descriptor
    }

    /// Merges one precomputed transaction descriptor into the pane-level
    /// immutable filter reused for every output batch.
    fn remember_mez_wrapper_filter_descriptor(&mut self, pane_id: &str, commands: &[String]) {
        let mut retained = self
            .process
            .pane_mez_wrapper_filter_recent_commands
            .get(pane_id)
            .map(|commands| commands.to_vec())
            .unwrap_or_default();
        for line in commands {
            if !retained.iter().any(|existing| existing == line) {
                retained.push(line.clone());
            }
        }
        let extra = retained
            .len()
            .saturating_sub(RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT);
        if extra > 0 {
            retained.drain(0..extra);
        }
        self.process
            .pane_mez_wrapper_filter_recent_commands
            .insert(pane_id.to_string(), retained.into());
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
    fn mez_wrapper_filter_commands_for_pane(&mut self, pane_id: &str) -> std::sync::Arc<[String]> {
        #[cfg(test)]
        self.synchronize_direct_test_wrapper_filter_commands(pane_id);
        self.process
            .pane_mez_wrapper_filter_recent_commands
            .get(pane_id)
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::from(Vec::<String>::new()))
    }

    /// Synchronizes direct map mutations used by process fixtures.
    ///
    /// Production transactions always install their descriptor through
    /// `register_running_shell_transaction`; this fallback keeps legacy tests
    /// representative without putting full-command scans back in production.
    #[cfg(test)]
    fn synchronize_direct_test_wrapper_filter_commands(&mut self, pane_id: &str) {
        let missing = self
            .process
            .running_shell_transactions
            .iter()
            .filter(|(marker, transaction)| {
                transaction.pane_id == pane_id
                    && !self
                        .process
                        .shell_transaction_wrapper_filter_commands
                        .contains_key(*marker)
            })
            .map(|(marker, transaction)| {
                let commands = if transaction.pending_input_payload.is_none() {
                    transaction.command.clone()
                } else {
                    String::new()
                };
                (marker.clone(), commands)
            })
            .collect::<Vec<_>>();
        for (marker, command) in missing {
            let descriptor = if command.is_empty() {
                std::sync::Arc::from(Vec::<String>::new())
            } else {
                self.remember_mez_wrapper_filter_command(pane_id, &command)
            };
            self.process
                .shell_transaction_wrapper_filter_commands
                .insert(marker, descriptor);
        }
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
