//! Pane bootstrap dispatch and completion.

use super::*;

impl RuntimeSessionService {
    /// Runs the dispatch bootstrap to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn dispatch_bootstrap_to_pane(&mut self, pane_id: &str) -> Result<()> {
        if self
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == pane_id)
        {
            return Ok(());
        }
        let agent_id = format!("agent-{pane_id}");
        let turn_id = format!("bootstrap-{pane_id}-{}", current_unix_seconds());
        let marker = runtime_random_marker_token(&format!("bootstrap\0{pane_id}\0{turn_id}"))?;
        let marker_id = marker.as_str().to_string();
        let classification = self.shell_classification_for_pane(pane_id);
        let bootstrap_script = bootstrap_script_for_classification(classification);
        let transaction = ShellTransaction::new(
            marker,
            &turn_id,
            &agent_id,
            pane_id,
            self.session.shell.path(),
            bootstrap_script.clone(),
        )?;
        let transaction_input = transaction.render_for_classification_input(classification);
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        self.remember_mez_wrapper_filter_command(pane_id, &bootstrap_script);
        self.write_runtime_pane_input(pane_id, wrapper.as_bytes())?;
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.running_shell_transactions.insert(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn_id.clone(),
                kind: RunningShellTransactionKind::Bootstrap,
                pane_id: pane_id.to_string(),
                command: bootstrap_script,
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(DEFAULT_BOOTSTRAP_TIMEOUT_MS),
                pending_input_payload: (!transaction_input.payload.is_empty())
                    .then(|| transaction_input.payload.into_bytes()),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
        );
        self.shell_transaction_require_start_markers
            .insert(marker_id.clone());
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","bootstrap":"sent","marker":"{}"}}"#,
                json_escape(pane_id),
                json_escape(&marker_id)
            ),
        )?;
        Ok(())
    }

    /// Runs the observe bootstrap transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn observe_bootstrap_transaction_end(
        &mut self,
        marker: &str,
        pane_id: &str,
        exit_code: i32,
        observed_output_preview: &str,
        observed_output_truncated: bool,
    ) -> Result<usize> {
        self.process.pane_bootstrap_pending.remove(pane_id);
        let mut bootstrap_parsed = false;
        if exit_code == 0 {
            let all_output = if observed_output_preview.trim().is_empty() {
                let screen = self.pane_screens.get(pane_id).ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "pane terminal screen not found",
                    )
                })?;
                screen.normal_content_lines().join("\n")
            } else {
                observed_output_preview.to_string()
            };

            let (signature, inventory, instruction_files) =
                parse_bootstrap_env_output(&all_output, self.session.shell.path());

            if let Some(sig) = signature.clone() {
                bootstrap_parsed = true;
                self.process
                    .pane_environment_signatures
                    .insert(pane_id.to_string(), sig.clone());
                if let Some(inv) = inventory.clone() {
                    self.tool_discovery_cache.record(sig, inv);
                }
                if !instruction_files.is_empty() {
                    self.pane_instruction_files
                        .insert(pane_id.to_string(), instruction_files);
                }
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"completed","marker":"{}","exit_code":0,"output_truncated":{}}}"#,
                        json_escape(pane_id),
                        json_escape(marker),
                        observed_output_truncated
                    ),
                )?;
            } else {
                self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"unparsed","marker":"{}","exit_code":0,"output_truncated":{},"message":"bootstrap completed but no environment signature was parsed; continuing with degraded context"}}"#,
                        json_escape(pane_id),
                        json_escape(marker),
                        observed_output_truncated
                    ),
                )?;
            }
        } else {
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","bootstrap":"failed","marker":"{}","exit_code":{}}}"#,
                    json_escape(pane_id),
                    json_escape(marker),
                    exit_code
                ),
            )?;
        }
        if bootstrap_parsed || exit_code == 0 {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
        } else if self.pane_readiness_state(pane_id) == PaneReadinessState::Busy {
            self.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);
        }
        let pending_shell_turns = self
            .agent_turn_executions
            .iter()
            .filter(|(turn_id, execution)| {
                self.execution_has_pending_shell_dispatch(turn_id, execution)
                    && self.agent_turn_ledger.turns().iter().any(|turn| {
                        turn.turn_id == **turn_id
                            && turn.pane_id == pane_id
                            && turn.state == AgentTurnState::Running
                    })
            })
            .map(|(turn_id, _)| turn_id.clone())
            .collect::<Vec<_>>();
        for turn_id in pending_shell_turns {
            let _ = self.dispatch_stored_running_shell_actions(&turn_id)?;
        }
        let _ = self.recover_stranded_agent_shell_dispatches()?;
        Ok(1)
    }

    /// Dispatches hidden bootstrap wrappers for pending panes that have reached
    /// prompt-like readiness.
    pub(crate) fn maybe_bootstrap_ready_panes(&mut self) -> Result<usize> {
        let ready_panes: Vec<String> = self
            .process
            .pane_readiness_states
            .iter()
            .filter(|(k, v)| {
                self.process.pane_bootstrap_pending.contains(k.as_str())
                    && !self
                        .running_shell_transactions
                        .values()
                        .any(|transaction| transaction.pane_id == k.as_str())
                    && matches!(
                        v,
                        PaneReadinessState::Ready | PaneReadinessState::PromptCandidate
                    )
            })
            .map(|(k, _)| k.clone())
            .collect();
        let dispatches = ready_panes.len();
        for pane_id in ready_panes {
            self.dispatch_bootstrap_to_pane(&pane_id)?;
        }
        Ok(dispatches)
    }
}
