//! Pane bootstrap dispatch and completion.

use mez_agent::AgentShellVisibility;

use super::super::{RuntimeAgentSubshellCertificationOutcome, RuntimePendingBootstrapEnvironment};
use super::{
    AgentTurnState, DEFAULT_BOOTSTRAP_TIMEOUT_MS, EventKind, MezError, PaneReadinessState, Result,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeSessionService,
    ShellTransaction, bootstrap_script_for_classification, current_unix_millis,
    current_unix_seconds, json_escape, parse_bootstrap_env_output, runtime_random_marker_token,
};

impl RuntimeSessionService {
    /// Registers one pane bootstrap and returns the exact wrapper that must be
    /// delivered after any preceding shell-handoff input.
    ///
    /// The encoded command payload remains on the registered transaction until
    /// the runtime observes the wrapper's start marker and releases it through
    /// the priority input path.
    pub(crate) fn prepare_bootstrap_to_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<Option<(String, String)>> {
        if self
            .process
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == pane_id)
        {
            return Ok(None);
        }
        let agent_id = format!("agent-{pane_id}");
        let turn_id = format!("bootstrap-{pane_id}-{}", current_unix_seconds());
        let marker = runtime_random_marker_token(&format!("bootstrap\0{pane_id}\0{turn_id}"))?;
        let marker_id = marker.as_str().to_string();
        let classification = self.shell_classification_for_pane(pane_id);
        let bootstrap_script = bootstrap_script_for_classification(classification);
        let transaction = self.configure_shell_transaction_for_pane(
            pane_id,
            ShellTransaction::new(
                marker,
                &turn_id,
                &agent_id,
                pane_id,
                self.session.shell.path(),
                bootstrap_script.clone(),
            )?,
        );
        let transaction_input = transaction.render_for_classification_input(classification);
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        self.remember_mez_wrapper_filter_command(pane_id, &bootstrap_script);
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.register_running_shell_transaction(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id,
                kind: RunningShellTransactionKind::Bootstrap,
                pane_id: pane_id.to_string(),
                command: bootstrap_script,
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(DEFAULT_BOOTSTRAP_TIMEOUT_MS),
                pending_input_payload: (!transaction_input.payload.is_empty()).then(|| {
                    mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                        transaction_input.payload.into_bytes(),
                        marker_id.clone(),
                        transaction_input.payload_receiver_acknowledgements,
                    )
                }),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
            true,
        );
        Ok(Some((marker_id, wrapper)))
    }

    /// Records successful delivery of a previously registered bootstrap.
    pub(crate) fn record_bootstrap_sent(&mut self, pane_id: &str, marker: &str) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","bootstrap":"sent","marker":"{}"}}"#,
                json_escape(pane_id),
                json_escape(marker)
            ),
        )?;
        Ok(())
    }

    /// Runs the dispatch bootstrap to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn dispatch_bootstrap_to_pane(&mut self, pane_id: &str) -> Result<()> {
        let Some((marker_id, wrapper)) = self.prepare_bootstrap_to_pane(pane_id)? else {
            return Ok(());
        };
        self.bind_agent_subshell_bootstrap_marker(pane_id, &marker_id);
        if let Err(error) = self.write_runtime_pane_shell_input(pane_id, wrapper.as_bytes()) {
            self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
            return Err(error);
        }
        self.record_bootstrap_sent(pane_id, &marker_id)?;
        Ok(())
    }

    /// Runs the observe bootstrap transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn observe_bootstrap_transaction_end(
        &mut self,
        marker: &str,
        pane_id: &str,
        exit_code: i32,
        observed_output_preview: &str,
        observed_output_truncated: bool,
    ) -> Result<usize> {
        let mut bootstrap_parsed = false;
        let mut bootstrap_environment = None;
        if exit_code == 0 {
            let all_output = if observed_output_preview.trim().is_empty() {
                let screen = self
                    .process
                    .process_pane_screens
                    .get(pane_id)
                    .ok_or_else(|| {
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

            if let Some(sig) = signature {
                bootstrap_parsed = true;
                bootstrap_environment = Some(RuntimePendingBootstrapEnvironment {
                    signature: sig,
                    tool_inventory: inventory,
                    instruction_files,
                });
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
        let certification = self.settle_agent_subshell_bootstrap_certification(
            pane_id,
            marker,
            exit_code,
            observed_output_truncated,
            bootstrap_environment.clone(),
        );
        match certification {
            RuntimeAgentSubshellCertificationOutcome::Pending => {
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"certification_pending","marker":"{}","observation":"fresh_worker"}}"#,
                        json_escape(pane_id),
                        json_escape(marker)
                    ),
                )?;
                return Ok(1);
            }
            RuntimeAgentSubshellCertificationOutcome::Rejected(reason) => {
                self.process.pane_bootstrap_pending.remove(pane_id);
                self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"certification_failed","marker":"{}","reason":"{}"}}"#,
                        json_escape(pane_id),
                        json_escape(marker),
                        reason.as_str()
                    ),
                )?;
                self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
            }
            RuntimeAgentSubshellCertificationOutcome::NotApplicable => {
                self.process.pane_bootstrap_pending.remove(pane_id);
                if let Some(environment) = bootstrap_environment {
                    self.publish_bootstrap_environment(pane_id, environment);
                }
                if bootstrap_parsed || exit_code == 0 {
                    self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
                } else if self.pane_readiness_state(pane_id) == PaneReadinessState::Busy {
                    self.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);
                }
            }
            RuntimeAgentSubshellCertificationOutcome::Certified => {
                self.process.pane_bootstrap_pending.remove(pane_id);
                self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            }
        }
        self.resume_after_bootstrap_settlement(pane_id)?;
        Ok(1)
    }

    /// Resumes deferred agent work only after bootstrap authority is settled.
    pub(crate) fn resume_after_bootstrap_settlement(&mut self, pane_id: &str) -> Result<()> {
        let pending_shell_turns = self
            .agent_turn_executions()
            .iter()
            .filter(|(turn_id, execution)| {
                self.execution_has_pending_shell_dispatch(turn_id, execution)
                    && self.agent_turn_ledger().turns().iter().any(|turn| {
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
        if self.agent_subshell_is_active(pane_id)
            && self
                .agent_shell_store()
                .get(pane_id)
                .is_some_and(|session| session.visibility == AgentShellVisibility::Hidden)
        {
            let _ = self.exit_agent_subshell_if_active(pane_id)?;
        }
        Ok(())
    }

    /// Dispatches hidden bootstrap wrappers for pending panes that have reached
    /// prompt-like readiness.
    pub(crate) fn maybe_bootstrap_ready_panes(&mut self) -> Result<usize> {
        let ready_panes: Vec<String> = self
            .process
            .pane_readiness_states
            .iter()
            .filter(|(k, v)| {
                let has_deferred_wrapper = self
                    .process
                    .pane_shell_handoffs
                    .get(k.as_str())
                    .is_some_and(|handoff| handoff.deferred_bootstrap_wrapper.is_some());
                self.process.pane_bootstrap_pending.contains(k.as_str())
                    && !self.pane_agent_subshell_certification_is_pending(k.as_str())
                    && (has_deferred_wrapper
                        || !self
                            .process
                            .running_shell_transactions
                            .values()
                            .any(|transaction| transaction.pane_id == k.as_str()))
                    && matches!(
                        v,
                        PaneReadinessState::Ready | PaneReadinessState::PromptCandidate
                    )
            })
            .map(|(k, _)| k.clone())
            .collect();
        let dispatches = ready_panes.len();
        for pane_id in ready_panes {
            let deferred = self
                .process
                .pane_shell_handoffs
                .get_mut(&pane_id)
                .and_then(|handoff| {
                    let marker = handoff.bootstrap_marker.clone()?;
                    let wrapper = handoff.deferred_bootstrap_wrapper.take()?;
                    Some((marker, wrapper))
                });
            if let Some((marker, wrapper)) = deferred {
                if let Err(error) =
                    self.write_runtime_pane_shell_input(&pane_id, wrapper.as_bytes())
                {
                    self.fail_shell_transactions_for_pane_write_failure(&pane_id, error.message())?;
                    return Err(error);
                }
                self.record_bootstrap_sent(&pane_id, &marker)?;
            } else {
                self.dispatch_bootstrap_to_pane(&pane_id)?;
            }
        }
        Ok(dispatches)
    }
}
