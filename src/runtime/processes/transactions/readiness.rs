//! Passive shell readiness and readiness-probe transitions.

use super::{
    AgentTurnRecord, AgentTurnState, EventKind, MezError, PaneReadinessState,
    RUNTIME_READINESS_PROBE_TIMEOUT_MS, ReadinessOverrideRevocation, Result,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeSessionService,
    ShellTransaction, TerminalOscEvent, current_unix_millis, json_escape,
    readiness_probe_command_for_classification, runtime_execution_ready_for_provider_continuation,
    runtime_marker_for_action, runtime_pane_readiness_state_name,
    terminal_clipboard_policy_accepts_osc52,
};

impl RuntimeSessionService {
    /// Runs the apply terminal osc events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn apply_terminal_osc_events(
        &mut self,
        events: &[TerminalOscEvent],
    ) -> Result<usize> {
        let mut applied = 0usize;
        for event in events {
            match event {
                TerminalOscEvent::ClipboardSet { selection, content }
                    if terminal_clipboard_policy_accepts_osc52(self.terminal_clipboard()) =>
                {
                    self.copy_text_to_buffer_and_host_clipboard(
                        "osc52",
                        content.clone(),
                        format!("terminal-osc52:{selection}"),
                    )?;
                    applied = applied.saturating_add(1);
                }
                TerminalOscEvent::TitleChanged { .. }
                | TerminalOscEvent::ClipboardSet { .. }
                | TerminalOscEvent::ShellIntegration { .. }
                | TerminalOscEvent::ShellPromptStart
                | TerminalOscEvent::ShellPromptEnd
                | TerminalOscEvent::ShellCommandOutputStart
                | TerminalOscEvent::ShellCommandFinished { .. }
                | TerminalOscEvent::ShellTransactionStart { .. }
                | TerminalOscEvent::ShellTransactionEnd { .. } => {}
            }
        }
        Ok(applied)
    }

    /// Runs the observe passive shell prompt candidate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn observe_passive_shell_prompt_candidate(
        &mut self,
        pane_id: &str,
        source: &str,
    ) -> Result<usize> {
        let previous = self.pane_readiness_state(pane_id);
        let foreground_primary_shell = self.pane_foreground_primary_shell_state(pane_id);
        let may_recover_interactive_block = matches!(
            previous,
            PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked
        ) && foreground_primary_shell == Some(true);
        let may_recover_degraded =
            previous == PaneReadinessState::Degraded && foreground_primary_shell != Some(false);
        if !matches!(
            previous,
            PaneReadinessState::Unknown | PaneReadinessState::Busy
        ) && !may_recover_interactive_block
            && !may_recover_degraded
        {
            return Ok(0);
        }
        self.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","readiness_event":"prompt_candidate","source":"{}","previous_state":"{}","state":"prompt-candidate"}}"#,
                json_escape(pane_id),
                json_escape(source),
                runtime_pane_readiness_state_name(previous)
            ),
        )?;
        let queued =
            self.queue_waiting_agent_turn_for_passive_readiness(pane_id, "prompt_candidate")?;
        Ok(1usize.saturating_add(queued))
    }

    /// Runs the observe passive shell busy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn observe_passive_shell_busy(
        &mut self,
        pane_id: &str,
        source: &str,
    ) -> Result<usize> {
        let previous = self.pane_readiness_state(pane_id);
        let waiting_turn_id = if source == "osc133-command-start" {
            self.pane_agent_turn_waiting_for_provider_or_shell_dispatch(pane_id)
        } else {
            None
        };
        if matches!(
            previous,
            PaneReadinessState::Probing
                | PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked
        ) {
            if let Some(turn_id) = waiting_turn_id {
                self.append_agent_trace_turn_event(
                    pane_id,
                    &turn_id,
                    "passive command-start ignored reason=agent_turn_waiting",
                )?;
            }
            return Ok(0);
        }
        let revoked = self
            .process
            .pane_readiness_overrides
            .revoke(pane_id, ReadinessOverrideRevocation::CommandStartMetadata)
            .is_some();
        if previous == PaneReadinessState::Busy && !revoked {
            if let Some(turn_id) = waiting_turn_id {
                self.append_agent_trace_turn_event(
                    pane_id,
                    &turn_id,
                    "passive command-start ignored reason=agent_turn_waiting",
                )?;
            }
            return Ok(0);
        }
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","readiness_event":"busy","source":"{}","previous_state":"{}","state":"busy","override_revoked":{}}}"#,
                json_escape(pane_id),
                json_escape(source),
                runtime_pane_readiness_state_name(previous),
                revoked
            ),
        )?;
        if let Some(turn_id) = waiting_turn_id {
            self.append_agent_trace_turn_event(
                pane_id,
                &turn_id,
                "passive command-start ignored reason=agent_turn_waiting",
            )?;
        }
        Ok(1)
    }

    /// Runs the record running shell transaction output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn dispatch_readiness_probe_to_pane(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        if self
            .process
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == turn.pane_id)
        {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "readiness_probe dispatch skipped reason=shell_transaction_running",
            )?;
            return Ok(());
        }
        if self
            .process
            .running_shell_transactions
            .values()
            .any(|transaction| {
                transaction.turn_id == turn.turn_id
                    && transaction.kind == RunningShellTransactionKind::ReadinessProbe
            })
        {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "readiness_probe dispatch skipped reason=already_running",
            )?;
            return Ok(());
        }
        let previous_readiness = self.pane_readiness_state(&turn.pane_id);
        let marker = runtime_marker_for_action(turn, "readiness-probe")?;
        let marker_id = marker.as_str().to_string();
        let classification = self.shell_classification_for_pane(&turn.pane_id);
        let probe_command = readiness_probe_command_for_classification(classification);
        let transaction = ShellTransaction::new(
            marker,
            &turn.turn_id,
            &turn.agent_id,
            &turn.pane_id,
            self.session.shell.path(),
            probe_command,
        )?;
        let transaction_input = transaction.render_for_classification_input(classification);
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        self.remember_mez_wrapper_filter_command(&turn.pane_id, probe_command);
        self.write_runtime_pane_input(&turn.pane_id, wrapper.as_bytes())?;
        self.process
            .pane_readiness_overrides
            .record_pending_probe(&turn.pane_id, &marker_id)?;
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Probing);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "pane_readiness {} -> probing reason=readiness_probe_sent marker={}",
                runtime_pane_readiness_state_name(previous_readiness),
                marker_id
            ),
        )?;
        self.process.running_shell_transactions.insert(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn.turn_id.clone(),
                kind: RunningShellTransactionKind::ReadinessProbe,
                pane_id: turn.pane_id.clone(),
                command: probe_command.to_string(),
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(RUNTIME_READINESS_PROBE_TIMEOUT_MS),
                pending_input_payload: (!transaction_input.payload.is_empty())
                    .then(|| transaction_input.payload.into_bytes()),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
        );
        self.process
            .shell_transaction_require_start_markers
            .insert(marker_id.clone());
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"probing","readiness_probe":"sent","marker":"{}"}}"#,
                json_escape(&turn.pane_id),
                json_escape(&turn.turn_id),
                json_escape(&marker_id)
            ),
        )?;
        Ok(())
    }

    /// Runs the observe readiness probe transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn observe_readiness_probe_transaction_end(
        &mut self,
        marker: &str,
        turn_id: &str,
        agent_id: &str,
        pane_id: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.agent_id != agent_id || turn.pane_id != pane_id {
            return Err(MezError::invalid_state(
                "readiness probe marker identity does not match agent turn",
            ));
        }
        if !self
            .process
            .pane_readiness_overrides
            .clear_pending_probe_if_matches(pane_id, marker)
        {
            self.append_agent_trace_turn_event(
                pane_id,
                turn_id,
                &format!("readiness_probe ignored reason=stale_marker marker={marker}"),
            )?;
            return Ok(0);
        }
        if exit_code == 0 {
            let previous_readiness = self.pane_readiness_state(pane_id);
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            self.append_agent_trace_turn_event(
                pane_id,
                turn_id,
                &format!(
                    "pane_readiness {} -> ready reason=readiness_probe_completed marker={}",
                    runtime_pane_readiness_state_name(previous_readiness),
                    marker
                ),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"ready","readiness_probe":"completed","marker":"{}","exit_code":0}}"#,
                    json_escape(pane_id),
                    json_escape(turn_id),
                    json_escape(marker)
                ),
            )?;
            let should_dispatch_stored_shell = self
                .agent_turn_executions()
                .get(turn_id)
                .is_some_and(|execution| {
                    self.execution_has_pending_shell_dispatch(turn_id, execution)
                });
            if should_dispatch_stored_shell {
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    "pending_shell_dispatch available reason=readiness_probe_completed",
                )?;
                let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
            } else if self
                .agent_turn_ledger()
                .turns()
                .iter()
                .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
                && self
                    .agent_turn_executions()
                    .get(turn_id)
                    .is_some_and(runtime_execution_ready_for_provider_continuation)
            {
                self.queue_agent_provider_task(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    "provider_task queued reason=readiness_probe_completed",
                )?;
            }
        } else {
            self.process
                .pane_readiness_overrides
                .revoke(pane_id, ReadinessOverrideRevocation::ReadinessProbeFailed);
            let previous_readiness = self.pane_readiness_state(pane_id);
            self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
            self.append_agent_trace_turn_event(
                pane_id,
                turn_id,
                &format!(
                    "pane_readiness {} -> degraded reason=readiness_probe_failed marker={} exit_code={}",
                    runtime_pane_readiness_state_name(previous_readiness),
                    marker,
                    exit_code
                ),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"degraded","readiness_probe":"failed","marker":"{}","exit_code":{}}}"#,
                    json_escape(pane_id),
                    json_escape(turn_id),
                    json_escape(marker),
                    exit_code
                ),
            )?;
        }
        Ok(1)
    }
}
