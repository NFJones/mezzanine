//! Shell transaction event observation and foreground-shell state.

use super::super::{
    EnvironmentSignature, RuntimeBootstrapShellCertificationEvidence, RuntimeCertifiedShellSource,
    RuntimePaneCertifiedShellIdentity, RuntimePaneShellHandoff,
};
use super::{
    AgentTurnState, Result, RuntimeSessionService, TerminalOscEvent,
    runtime_execution_ready_for_provider_continuation,
};

/// Best-effort foreground-process information attached to a readiness failure.
///
/// The runtime uses this diagnostic to explain why it refused to send shell
/// input without exposing process command lines, arguments, or environment
/// values. Process metadata is inherently transient, so absent fields mean the
/// host could not provide that observation at dispatch time.
#[derive(Debug, Clone)]
pub(crate) struct RuntimePaneForegroundDiagnostic {
    /// Whether both the pane primary process and foreground process group were available.
    metadata_available: bool,
    /// Source of the foreground process-group observation.
    foreground_process_group_source: &'static str,
    /// Best-effort display name for the foreground process-group leader.
    foreground_process_name: Option<String>,
    /// Foreground process group currently reported for the pane PTY.
    foreground_process_group_id: Option<u32>,
    /// Primary pane-shell process id.
    primary_process_id: Option<u32>,
    /// Primary pane-shell process group id.
    primary_process_group_id: Option<u32>,
    /// Whether the observed foreground group belongs to the primary shell.
    primary_shell_is_foreground: Option<bool>,
    /// Non-primary process group certified for the current shell epoch.
    certified_shell_process_group_id: Option<u32>,
    /// Runtime-owned provenance for the non-primary certification.
    certified_shell_source: Option<&'static str>,
    /// Whether the observed group is an accepted primary or certified shell.
    certified_shell_is_foreground: Option<bool>,
    /// Current shell-interaction generation for trace correlation.
    shell_interaction_generation: Option<u64>,
}

impl RuntimePaneForegroundDiagnostic {
    /// Renders the diagnostic as structured action-result content.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "metadata_available": self.metadata_available,
            "foreground_process_group_source": self.foreground_process_group_source,
            "foreground_process_name": self.foreground_process_name,
            "foreground_process_group_id": self.foreground_process_group_id,
            "primary_process_id": self.primary_process_id,
            "primary_process_group_id": self.primary_process_group_id,
            "primary_shell_is_foreground": self.primary_shell_is_foreground,
            "certified_shell_process_group_id": self.certified_shell_process_group_id,
            "certified_shell_source": self.certified_shell_source,
            "certified_shell_is_foreground": self.certified_shell_is_foreground,
            "shell_interaction_generation": self.shell_interaction_generation,
        })
    }

    /// Renders concise safe text for the terminal error buffer and trace.
    pub(crate) fn summary(&self) -> String {
        match self.foreground_process_group_id {
            Some(process_group_id) => format!(
                "foreground_process={} foreground_process_group={} primary_process={} primary_process_group={} primary_shell_is_foreground={} certified_shell_process_group={} certified_shell_is_foreground={} certification_source={} shell_interaction_generation={} source={}",
                self.foreground_process_name
                    .as_deref()
                    .unwrap_or("unavailable"),
                process_group_id,
                self.primary_process_id
                    .map(|process_id| process_id.to_string())
                    .as_deref()
                    .unwrap_or("unavailable"),
                self.primary_process_group_id
                    .map(|process_group_id| process_group_id.to_string())
                    .as_deref()
                    .unwrap_or("unavailable"),
                self.primary_shell_is_foreground
                    .map(|is_foreground| is_foreground.to_string())
                    .as_deref()
                    .unwrap_or("unavailable"),
                self.certified_shell_process_group_id
                    .map(|process_group_id| process_group_id.to_string())
                    .as_deref()
                    .unwrap_or("unavailable"),
                self.certified_shell_is_foreground
                    .map(|is_foreground| is_foreground.to_string())
                    .as_deref()
                    .unwrap_or("unavailable"),
                self.certified_shell_source.unwrap_or("primary-shell"),
                self.shell_interaction_generation
                    .map(|generation| generation.to_string())
                    .as_deref()
                    .unwrap_or("unavailable"),
                self.foreground_process_group_source,
            ),
            None => "foreground_process_metadata=unavailable".to_string(),
        }
    }
}

impl RuntimeSessionService {
    /// Returns the best foreground process-group observation and its source.
    fn pane_foreground_process_group_observation(
        &self,
        pane_id: &str,
    ) -> (Option<u32>, &'static str) {
        match self
            .process
            .pane_processes
            .foreground_process_group_id(pane_id)
        {
            Some(process_group_id) => (Some(process_group_id), "pty"),
            None => match self
                .process
                .pane_foreground_process_groups
                .get(pane_id)
                .copied()
            {
                Some(process_group_id) => (Some(process_group_id), "worker-cache"),
                None => (None, "unavailable"),
            },
        }
    }

    /// Returns the original pane-shell process group for one live pane.
    fn pane_primary_process_group_id(&self, pane_id: &str, primary_pid: u32) -> u32 {
        self.process
            .pane_processes
            .process_group_leader(pane_id)
            .and_then(|leader| u32::try_from(leader).ok())
            .unwrap_or(primary_pid)
    }

    /// Reports whether the pane foreground group is the primary shell or the
    /// non-primary shell certified for the current process and interaction epoch.
    pub(crate) fn pane_foreground_certified_shell_state(&self, pane_id: &str) -> Option<bool> {
        let primary_pid = self.primary_pid_for_live_pane_process(pane_id)?;
        let foreground_group = self.pane_foreground_process_group_observation(pane_id).0?;
        let primary_process_group = self.pane_primary_process_group_id(pane_id, primary_pid);
        if foreground_group == primary_pid || foreground_group == primary_process_group {
            return Some(true);
        }
        let certified = self.process.pane_certified_shell_identities.get(pane_id);
        Some(certified.is_some_and(|identity| {
            identity.primary_process_id == primary_pid
                && self
                    .process
                    .pane_shell_interaction_generations
                    .get(pane_id)
                    .copied()
                    == Some(identity.interaction_generation)
                && self.process.pane_environment_signatures.get(pane_id)
                    == Some(&identity.environment_signature)
                && identity.process_group_id == foreground_group
        }))
    }

    /// Starts a new runtime-owned agent-subshell handoff and invalidates state
    /// derived from the previous pane environment before bootstrap dispatch.
    pub(crate) fn begin_agent_subshell_shell_handoff(&mut self, pane_id: &str) -> Result<()> {
        let primary_process_id =
            self.primary_pid_for_live_pane_process(pane_id)
                .ok_or_else(|| {
                    crate::error::MezError::invalid_state("pane shell process is unavailable")
                })?;
        self.process.next_shell_interaction_generation = self
            .process
            .next_shell_interaction_generation
            .saturating_add(1);
        let interaction_generation = self.process.next_shell_interaction_generation;
        self.process
            .pane_shell_interaction_generations
            .insert(pane_id.to_string(), interaction_generation);
        self.process.pane_certified_shell_identities.remove(pane_id);
        self.process
            .bootstrap_shell_certification_evidence
            .retain(|_, evidence| evidence.pane_id != pane_id);
        self.process.pane_shell_handoffs.insert(
            pane_id.to_string(),
            RuntimePaneShellHandoff {
                primary_process_id,
                interaction_generation,
                bootstrap_marker: None,
            },
        );
        self.process.pane_environment_signatures.remove(pane_id);
        self.process
            .pane_path_scopes
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_path_scope_failures
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_bubblewrap_capabilities
            .retain(|key, _| key.pane_id != pane_id);
        self.clear_pane_agent_instruction_files(pane_id);
        self.process
            .pane_bootstrap_pending
            .insert(pane_id.to_string());
        Ok(())
    }

    /// Binds the exact registered bootstrap marker to a pending subshell handoff.
    pub(crate) fn bind_agent_subshell_bootstrap_marker(&mut self, pane_id: &str, marker: &str) {
        if let Some(handoff) = self.process.pane_shell_handoffs.get_mut(pane_id)
            && handoff.bootstrap_marker.is_none()
        {
            handoff.bootstrap_marker = Some(marker.to_string());
        }
    }

    /// Captures persistent-receiver evidence before releasing bootstrap payload.
    ///
    /// The wrapper is blocked in its payload read loop at this boundary, so the
    /// foreground group belongs to the persistent agent subshell rather than an
    /// isolated child launched later by the transaction body.
    pub(crate) fn observe_agent_subshell_bootstrap_start(&mut self, pane_id: &str, marker: &str) {
        let Some(handoff) = self.process.pane_shell_handoffs.get(pane_id) else {
            return;
        };
        if handoff.bootstrap_marker.as_deref() != Some(marker) {
            return;
        }
        let Some(process_group_id) = self.pane_foreground_process_group_observation(pane_id).0
        else {
            return;
        };
        self.process.bootstrap_shell_certification_evidence.insert(
            marker.to_string(),
            RuntimeBootstrapShellCertificationEvidence {
                pane_id: pane_id.to_string(),
                primary_process_id: handoff.primary_process_id,
                process_group_id,
                interaction_generation: handoff.interaction_generation,
            },
        );
    }

    /// Settles a handoff bootstrap and promotes only consistent, complete proof.
    pub(crate) fn settle_agent_subshell_bootstrap_certification(
        &mut self,
        pane_id: &str,
        marker: &str,
        exit_code: i32,
        observed_output_truncated: bool,
        environment_signature: Option<&EnvironmentSignature>,
    ) -> Option<bool> {
        let handoff = self.process.pane_shell_handoffs.get(pane_id)?.clone();
        if handoff.bootstrap_marker.as_deref() != Some(marker) {
            return None;
        }
        self.process.pane_shell_handoffs.remove(pane_id);
        let evidence = self
            .process
            .bootstrap_shell_certification_evidence
            .remove(marker);
        let current_primary_process_id = self.primary_pid_for_live_pane_process(pane_id);
        let current_foreground_group = self.pane_foreground_process_group_observation(pane_id).0;
        let current_interaction_generation = self
            .process
            .pane_shell_interaction_generations
            .get(pane_id)
            .copied();
        let valid = evidence.as_ref().is_some_and(|evidence| {
            evidence.pane_id == pane_id
                && evidence.primary_process_id == handoff.primary_process_id
                && evidence.interaction_generation == handoff.interaction_generation
                && current_primary_process_id == Some(handoff.primary_process_id)
                && current_interaction_generation == Some(handoff.interaction_generation)
                && current_foreground_group == Some(evidence.process_group_id)
        }) && exit_code == 0
            && !observed_output_truncated
            && environment_signature.is_some();
        if let (true, Some(evidence), Some(environment_signature)) =
            (valid, evidence, environment_signature)
        {
            self.process.pane_certified_shell_identities.insert(
                pane_id.to_string(),
                RuntimePaneCertifiedShellIdentity {
                    primary_process_id: evidence.primary_process_id,
                    process_group_id: evidence.process_group_id,
                    interaction_generation: evidence.interaction_generation,
                    environment_signature: environment_signature.clone(),
                    source: RuntimeCertifiedShellSource::AgentSubshellBootstrap,
                },
            );
        } else {
            self.process.pane_certified_shell_identities.remove(pane_id);
            self.process.pane_environment_signatures.remove(pane_id);
            self.process
                .pane_path_scopes
                .retain(|key, _| key.pane_id != pane_id);
            self.process
                .pane_path_scope_failures
                .retain(|key, _| key.pane_id != pane_id);
            self.process
                .pane_bubblewrap_capabilities
                .retain(|key, _| key.pane_id != pane_id);
            self.clear_pane_agent_instruction_files(pane_id);
        }
        Some(valid)
    }

    /// Invalidates every non-primary shell proof associated with one pane.
    pub(crate) fn clear_agent_subshell_shell_identity(&mut self, pane_id: &str) {
        self.process.pane_certified_shell_identities.remove(pane_id);
        self.process.pane_shell_handoffs.remove(pane_id);
        self.process
            .bootstrap_shell_certification_evidence
            .retain(|_, evidence| evidence.pane_id != pane_id);
        if self
            .process
            .pane_shell_interaction_generations
            .contains_key(pane_id)
        {
            self.process.next_shell_interaction_generation = self
                .process
                .next_shell_interaction_generation
                .saturating_add(1);
            self.process.pane_shell_interaction_generations.insert(
                pane_id.to_string(),
                self.process.next_shell_interaction_generation,
            );
        }
    }

    /// Cancels an unstarted agent-subshell bootstrap and returns its payload.
    ///
    /// The caller must deliver the returned payload before its exit input so
    /// the already-delivered wrapper can finish its receiver without treating
    /// end-of-file as a truncated command. Once start observation has released
    /// the payload, the transaction remains registered and exit waits for its
    /// normal completion instead.
    pub(crate) fn cancel_agent_subshell_bootstrap_for_exit(
        &mut self,
        pane_id: &str,
    ) -> Option<Vec<u8>> {
        let marker = self
            .process
            .pane_shell_handoffs
            .get(pane_id)
            .and_then(|handoff| handoff.bootstrap_marker.clone());
        let marker = marker?;
        let removable = self
            .process
            .running_shell_transactions
            .get(&marker)
            .is_some_and(|transaction| {
                transaction.pane_id == pane_id
                    && transaction.kind == super::RunningShellTransactionKind::Bootstrap
                    && transaction.pending_input_payload.is_some()
            });
        if !removable {
            return None;
        }
        let payload = self
            .process
            .running_shell_transactions
            .remove(&marker)
            .and_then(|transaction| transaction.pending_input_payload);
        self.clear_shell_transaction_protocol_state(&marker);
        self.process
            .bootstrap_shell_certification_evidence
            .remove(&marker);
        self.process.pane_bootstrap_pending.remove(pane_id);
        payload
    }

    /// Invalidates child-environment evidence and schedules discovery after
    /// control returns to the original pane shell.
    pub(crate) fn schedule_parent_shell_rebootstrap_after_agent_subshell(&mut self, pane_id: &str) {
        self.process.pane_environment_signatures.remove(pane_id);
        self.process
            .pane_path_scopes
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_path_scope_failures
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_bubblewrap_capabilities
            .retain(|key, _| key.pane_id != pane_id);
        self.clear_pane_agent_instruction_files(pane_id);
        self.process
            .pane_bootstrap_pending
            .insert(pane_id.to_string());
        self.set_pane_readiness(pane_id, super::PaneReadinessState::Unknown);
    }

    /// Returns the foreground-process observation available for a pane readiness failure.
    pub(crate) fn pane_foreground_process_diagnostic(
        &self,
        pane_id: &str,
    ) -> RuntimePaneForegroundDiagnostic {
        let primary_process_id = self.primary_pid_for_live_pane_process(pane_id);
        let primary_process_group_id = primary_process_id.map(|primary_process_id| {
            self.pane_primary_process_group_id(pane_id, primary_process_id)
        });
        let (foreground_process_group_id, foreground_process_group_source) =
            self.pane_foreground_process_group_observation(pane_id);
        let primary_shell_is_foreground =
            foreground_process_group_id.and_then(|process_group_id| {
                primary_process_id.map(|primary_process_id| {
                    Some(process_group_id) == primary_process_group_id
                        || process_group_id == primary_process_id
                })
            });
        let certified_identity = self.process.pane_certified_shell_identities.get(pane_id);
        let certified_shell_is_foreground = foreground_process_group_id
            .and_then(|_| self.pane_foreground_certified_shell_state(pane_id));

        RuntimePaneForegroundDiagnostic {
            metadata_available: primary_process_id.is_some()
                && foreground_process_group_id.is_some(),
            foreground_process_group_source,
            foreground_process_name: foreground_process_group_id
                .and_then(|_| self.process.pane_processes.foreground_process_name(pane_id)),
            foreground_process_group_id,
            primary_process_id,
            primary_process_group_id,
            primary_shell_is_foreground,
            certified_shell_process_group_id: certified_identity
                .map(|identity| identity.process_group_id),
            certified_shell_source: certified_identity.map(|identity| identity.source.as_str()),
            certified_shell_is_foreground,
            shell_interaction_generation: self
                .process
                .pane_shell_interaction_generations
                .get(pane_id)
                .copied(),
        }
    }

    /// Runs the observe agent shell transaction events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn observe_agent_shell_transaction_events(
        &mut self,
        output_pane_id: &str,
        events: &[TerminalOscEvent],
    ) -> Result<usize> {
        let mut observed = 0usize;
        let mut observed_harness_transaction_end = false;
        for event in events {
            let decoded_event;
            let event = if let TerminalOscEvent::ShellIntegration { payload } = event {
                let encoded = format!("133;{payload}");
                decoded_event = crate::host::terminal::parse_mez_shell_transaction_osc(&encoded);
                let Some(event) = decoded_event.as_ref() else {
                    continue;
                };
                event
            } else {
                event
            };
            match event {
                TerminalOscEvent::ShellIntegration { .. } => {}
                TerminalOscEvent::TitleChanged { .. } | TerminalOscEvent::Clipboard(_) => {}
                TerminalOscEvent::ShellPromptStart => {}
                TerminalOscEvent::ShellPromptEnd => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-prompt-end",
                            )?);
                    }
                }
                TerminalOscEvent::ShellCommandFinished { .. } => {}
                TerminalOscEvent::ShellCommandOutputStart => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_busy(
                                output_pane_id,
                                "osc133-command-start",
                            )?);
                    }
                }
                TerminalOscEvent::ShellTransactionStart {
                    marker,
                    turn_id,
                    agent_id,
                    pane_id,
                } => {
                    observed =
                        observed.saturating_add(self.observe_agent_shell_transaction_start(
                            output_pane_id,
                            marker,
                            turn_id,
                            agent_id,
                            pane_id,
                        )?);
                }
                TerminalOscEvent::ShellTransactionEnd {
                    marker,
                    turn_id,
                    agent_id,
                    pane_id,
                    exit_code,
                } => {
                    let agent_observed = self.observe_agent_shell_transaction_end(
                        output_pane_id,
                        marker,
                        turn_id,
                        agent_id,
                        pane_id,
                        *exit_code,
                    )?;
                    if agent_observed == 0 {
                        observed = observed.saturating_add(
                            self.observe_focused_shell_hook_transaction_end(
                                output_pane_id,
                                marker,
                                pane_id,
                                *exit_code,
                            )?,
                        );
                    } else {
                        observed = observed.saturating_add(agent_observed);
                        observed_harness_transaction_end = true;
                    }
                }
            }
        }
        Ok(observed)
    }

    /// Runs the pane agent turn waiting for provider or shell dispatch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn pane_agent_turn_waiting_for_provider_or_shell_dispatch(
        &self,
        pane_id: &str,
    ) -> Option<String> {
        let turn_id = self
            .agent_shell_store()
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())?;
        let turn_is_running = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running);
        if !turn_is_running {
            return None;
        }
        if self.agent_provider_task_is_pending(turn_id) {
            return Some(turn_id.to_string());
        }
        if self.agent_provider_task_is_claimed(turn_id) {
            return None;
        }
        let execution = self.agent_turn_executions().get(turn_id)?;
        if runtime_execution_ready_for_provider_continuation(execution)
            || self.execution_has_pending_shell_dispatch(turn_id, execution)
        {
            Some(turn_id.to_string())
        } else {
            None
        }
    }

    /// Runs the queue waiting agent turn for passive readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_waiting_agent_turn_for_passive_readiness(
        &mut self,
        pane_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let Some(turn_id) = self.pane_agent_turn_waiting_for_provider_or_shell_dispatch(pane_id)
        else {
            return Ok(0);
        };
        if !self.queue_agent_provider_task(turn_id.clone()) {
            return Ok(0);
        }
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            &format!("provider_task queued reason={reason}"),
        )?;
        Ok(1)
    }
}
