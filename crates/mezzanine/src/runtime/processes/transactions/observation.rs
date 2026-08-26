//! Shell transaction event observation and foreground-shell state.

use super::super::{
    ManagedShellHandoffEffect, ManagedShellHandoffEvent, ManagedShellHandoffIdentity,
    ManagedShellSettlementRenderPolicy, PaneForegroundProcessObservation, PaneProcessInstance,
    PaneProcessIoEffect, RuntimeAgentSubshellCertificationOutcome,
    RuntimeAgentSubshellCertificationRejection, RuntimeBootstrapShellCertificationEvidence,
    RuntimeCertifiedShellSource, RuntimeForeignShellBootstrapPhase, RuntimeForeignShellBoundary,
    RuntimePaneCertifiedShellIdentity, RuntimePaneEnvironmentAuthorityUnavailableReason,
    RuntimePaneProbedShellIdentity, RuntimePaneShellHandoff,
    RuntimePendingAgentSubshellCertification, RuntimePendingAgentSubshellStartObservation,
    RuntimePendingBootstrapEnvironment, RuntimePendingShellDispatchRecoveryObservation,
    RuntimeSideEffect, RuntimeTransition, reduce_managed_shell_handoff,
};
use super::{
    AgentTurnState, EventKind, MezError, PaneReadinessState,
    RUNTIME_AGENT_SUBSHELL_CERTIFICATION_TIMEOUT_MS,
    RUNTIME_SHELL_DISPATCH_RECOVERY_OBSERVATION_TIMEOUT_MS, RenderInvalidationReason, Result,
    RuntimeSessionService, TerminalOscEvent, current_unix_millis, json_escape,
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
    /// Latest stable agent-subshell certification rejection for this pane.
    agent_subshell_certification_rejection: Option<&'static str>,
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
            "agent_subshell_certification_rejection": self.agent_subshell_certification_rejection,
            "shell_interaction_generation": self.shell_interaction_generation,
        })
    }

    /// Renders concise safe text for the terminal error buffer and trace.
    pub(crate) fn summary(&self) -> String {
        match self.foreground_process_group_id {
            Some(process_group_id) => format!(
                "foreground_process={} foreground_process_group={} primary_process={} primary_process_group={} primary_shell_is_foreground={} certified_shell_process_group={} certified_shell_is_foreground={} certification_source={} agent_subshell_certification_rejection={} shell_interaction_generation={} source={}",
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
                self.agent_subshell_certification_rejection
                    .unwrap_or("unavailable"),
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
    /// Releases dependency-free staging records after the correlated loader is ready.
    fn observe_foreign_shell_loader_ready(&mut self, pane_id: &str, marker: &str) -> Result<usize> {
        let Some(boundary) = self
            .process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .cloned()
        else {
            return Ok(0);
        };
        if boundary.phase != RuntimeForeignShellBootstrapPhase::BootstrappingChild
            || boundary.loader_marker.as_deref() != Some(marker)
            || boundary.loader_ready
            || self.primary_pid_for_live_pane_process(pane_id) != Some(boundary.primary_process_id)
            || self
                .process
                .pane_shell_interaction_generations
                .get(pane_id)
                .copied()
                != Some(boundary.interaction_generation)
        {
            return Ok(0);
        }
        let now_unix_ms = current_unix_millis();
        let phase_elapsed_ms = now_unix_ms.saturating_sub(boundary.phase_started_at_unix_ms);
        let lifecycle_elapsed_ms =
            now_unix_ms.saturating_sub(boundary.lifecycle_started_at_unix_ms);
        let payload = self
            .process
            .pane_foreign_shell_boundaries
            .get_mut(pane_id)
            .and_then(|current| {
                current.loader_ready = true;
                current.phase_started_at_unix_ms = now_unix_ms;
                current.loader_payload.take()
            });
        let Some(payload) = payload else {
            return Ok(0);
        };
        let payload_len = payload.bytes.len();
        let delivery_id = payload.delivery_id.clone();
        if let Err(error) = self.write_runtime_pane_shell_delivery(pane_id, payload) {
            self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
            return Ok(0);
        }
        if let Some(delivery_id) = delivery_id.as_deref() {
            self.mark_managed_shell_payload_released(pane_id, delivery_id);
        }
        let prebuffered_bootstrap = boundary
            .child_shell
            .is_none()
            .then(|| {
                self.process
                    .pane_shell_handoffs
                    .get_mut(pane_id)
                    .and_then(|handoff| {
                        let bootstrap_marker = handoff.bootstrap_marker.clone()?;
                        let wrapper = handoff.deferred_bootstrap_wrapper.take()?;
                        Some((bootstrap_marker, wrapper))
                    })
            })
            .flatten();
        if let Some((bootstrap_marker, wrapper)) = prebuffered_bootstrap {
            if let Err(error) = self.write_runtime_pane_shell_input(pane_id, wrapper.as_bytes()) {
                self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
                return Ok(0);
            }
            self.record_bootstrap_sent(pane_id, &bootstrap_marker)?;
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","foreign_bootstrap":"loader_ready","marker":"{}","payload_bytes":{},"bootstrap_prebuffered":{},"phase_elapsed_ms":{},"lifecycle_elapsed_ms":{}}}"#,
                json_escape(pane_id),
                json_escape(marker),
                payload_len,
                self.process
                    .pane_shell_handoffs
                    .get(pane_id)
                    .is_some_and(|handoff| handoff.deferred_bootstrap_wrapper.is_none()),
                phase_elapsed_ms,
                lifecycle_elapsed_ms
            ),
        )?;
        Ok(1)
    }

    /// Settles the correlated dependency-free loader after its child returns.
    fn observe_foreign_shell_loader_exited(
        &mut self,
        pane_id: &str,
        marker: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let Some(boundary) = self
            .process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .cloned()
        else {
            return Ok(0);
        };
        if boundary.loader_marker.as_deref() != Some(marker) {
            return Ok(0);
        }
        let stable_identity_matches = self.primary_pid_for_live_pane_process(pane_id)
            == Some(boundary.primary_process_id)
            && self
                .process
                .pane_shell_interaction_generations
                .get(pane_id)
                .copied()
                == Some(boundary.interaction_generation);
        if !stable_identity_matches {
            return Ok(0);
        }
        let bootstrap_marker = self
            .process
            .pane_managed_shell_handoffs
            .get(pane_id)
            .map(|handoff| handoff.identity().marker.clone())
            .or_else(|| {
                self.process
                    .running_shell_transactions
                    .iter()
                    .find(|(_, transaction)| {
                        transaction.pane_id == pane_id
                            && transaction.kind == super::RunningShellTransactionKind::Bootstrap
                    })
                    .map(|(marker, _)| marker.clone())
            });
        if let Some(current) = self.process.pane_foreign_shell_boundaries.get_mut(pane_id) {
            current.loader_marker = None;
            current.loader_payload = None;
            current.loader_ready = false;
        }
        if boundary.phase == RuntimeForeignShellBootstrapPhase::Certified {
            let pending_input = if let Some(identity) = self
                .process
                .pane_managed_shell_handoffs
                .get(pane_id)
                .map(|handoff| handoff.identity().clone())
            {
                let transition = self
                    .process
                    .pane_managed_shell_handoffs
                    .get_mut(pane_id)
                    .map(|handoff| {
                        reduce_managed_shell_handoff(
                            handoff,
                            ManagedShellHandoffEvent::ParentReady { identity },
                        )
                    });
                transition.and_then(|transition| {
                    transition
                        .effects
                        .into_iter()
                        .find_map(|effect| match effect {
                            ManagedShellHandoffEffect::Settle { pending_input, .. } => {
                                Some(pending_input)
                            }
                            _ => None,
                        })
                })
            } else {
                Some(Vec::new())
            };
            let Some(pending_input) = pending_input else {
                return Ok(0);
            };
            self.clear_uncertified_foreign_shell_boundary(pane_id);
            self.leave_agent_subshell(pane_id);
            self.invalidate_agent_subshell_environment_after_exit(pane_id);
            self.settle_managed_shell_runtime_ownership(
                pane_id,
                pending_input,
                ManagedShellSettlementRenderPolicy::ReleaseForeignParent,
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","foreign_bootstrap":"loader_exited","marker":"{}","exit_code":{}}}"#,
                    json_escape(pane_id),
                    json_escape(marker),
                    exit_code
                ),
            )?;
            return Ok(1);
        }
        if boundary.phase != RuntimeForeignShellBootstrapPhase::BootstrappingChild {
            return Ok(0);
        }
        if let Some(current) = self.process.pane_foreign_shell_boundaries.get_mut(pane_id) {
            current.phase = RuntimeForeignShellBootstrapPhase::Failed;
            current.phase_started_at_unix_ms = current_unix_millis();
            current.child_token = None;
            current.child_shell = None;
            current.child_staging_source = None;
            current.identity_marker = None;
        }
        if let Some(bootstrap_marker) = bootstrap_marker.as_deref() {
            self.cancel_runtime_pane_shell_delivery(pane_id, bootstrap_marker);
            self.process
                .bootstrap_shell_certification_evidence
                .remove(bootstrap_marker);
            self.remove_running_shell_transaction(bootstrap_marker);
            self.clear_shell_transaction_protocol_state(bootstrap_marker);
        }
        self.process.pane_managed_shell_handoffs.remove(pane_id);
        self.process.pane_shell_handoffs.remove(pane_id);
        self.process
            .pending_agent_subshell_start_observations
            .remove(pane_id);
        self.process
            .pending_agent_subshell_certifications
            .remove(pane_id);
        self.process.pane_bootstrap_pending.remove(pane_id);
        self.clear_agent_subshell_shell_identity(pane_id);
        self.mark_pane_environment_authority_unavailable(
            pane_id,
            RuntimePaneEnvironmentAuthorityUnavailableReason::BootstrapTransactionFailed,
        );
        self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
        let message = format!(
            "dependency-free foreign shell loader exited before child certification (status {exit_code})"
        );
        self.append_agent_error_text_to_terminal_buffer(pane_id, &format!("agent: {message}"))?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","foreign_bootstrap":"failed","phase":"bootstrapping-child","transport":"dependency-free","exit_code":{},"state":"degraded"}}"#,
                json_escape(pane_id),
                exit_code
            ),
        )?;
        let pending_turn_ids = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| {
                turn.pane_id == pane_id
                    && turn.state == AgentTurnState::Running
                    && self.agent_provider_task_is_pending(&turn.turn_id)
            })
            .map(|turn| turn.turn_id.clone())
            .collect::<Vec<_>>();
        let error = MezError::invalid_state(message);
        for turn_id in pending_turn_ids {
            self.fail_configured_agent_provider_task(&turn_id, &error)?;
        }
        Ok(1)
    }

    /// Returns the best foreground process-group observation and its source.
    pub(crate) fn pane_foreground_process_group_observation(
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
    pub(crate) fn pane_primary_process_group_id(&self, pane_id: &str, primary_pid: u32) -> u32 {
        self.process
            .pane_processes
            .process_group_leader(pane_id)
            .and_then(|leader| u32::try_from(leader).ok())
            .unwrap_or(primary_pid)
    }

    /// Reports whether one foreground process group is the primary shell or a
    /// non-primary shell certified for the current process and interaction epoch.
    pub(crate) fn pane_process_group_is_certified_shell(
        &self,
        pane_id: &str,
        foreground_group: u32,
    ) -> Option<bool> {
        let primary_pid = self.primary_pid_for_live_pane_process(pane_id)?;
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

    /// Reports whether the pane foreground group is the primary shell or the
    /// non-primary shell certified for the current process and interaction epoch.
    pub(crate) fn pane_foreground_certified_shell_state(&self, pane_id: &str) -> Option<bool> {
        let foreground_group = self.pane_foreground_process_group_observation(pane_id).0?;
        self.pane_process_group_is_certified_shell(pane_id, foreground_group)
    }

    /// Reports whether an uncertified non-primary process group owns the pane.
    pub(crate) fn pane_has_uncertified_foreign_shell_boundary(&self, pane_id: &str) -> bool {
        self.process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .is_some_and(|boundary| boundary.phase != RuntimeForeignShellBootstrapPhase::Certified)
    }

    /// Returns the active dependency-free phase used by agent-exit admission.
    pub(crate) fn foreign_shell_bootstrap_phase_for_exit(
        &self,
        pane_id: &str,
    ) -> Option<&'static str> {
        self.process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .map(|boundary| boundary.phase.as_str())
    }

    /// Resumes explicit agent-entry discovery after fresh foreground proof.
    ///
    /// Agent entry can precede the pane worker's first process-group event.
    /// Once that exact event proves the selected shell still owns the PTY, the
    /// awaiting boundary may issue its first identity probe without requiring
    /// shell startup integration or another user action.
    pub(crate) fn resume_agent_entry_discovery_for_foreground(
        &mut self,
        pane_id: &str,
        process_group_id: u32,
    ) -> Result<bool> {
        let resumable = self
            .process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .is_some_and(|boundary| {
                boundary.phase == RuntimeForeignShellBootstrapPhase::AwaitingPrompt
                    && boundary.process_group_id == process_group_id
                    && self.primary_pid_for_live_pane_process(pane_id)
                        == Some(boundary.primary_process_id)
            })
            && matches!(
                self.pane_readiness_state(pane_id),
                PaneReadinessState::Ready | PaneReadinessState::PromptCandidate
            )
            && self
                .agent_shell_store()
                .get(pane_id)
                .is_some_and(|session| {
                    session.visibility == mez_agent::AgentShellVisibility::Visible
                })
            && self.effective_agent_shell_mode_for_pane(pane_id)
                != crate::runtime::config::ShellMode::Native;
        if !resumable {
            return Ok(false);
        }
        self.begin_dependency_free_foreign_shell_bootstrap(pane_id)?;
        Ok(true)
    }

    /// Starts a foreign boundary from the current worker or PTY foreground observation.
    #[allow(dead_code)]
    pub(crate) fn begin_uncertified_foreign_shell_boundary_for_current_foreground(
        &mut self,
        pane_id: &str,
    ) -> bool {
        let Some(primary_process_id) = self.primary_pid_for_live_pane_process(pane_id) else {
            return false;
        };
        let Some(process_group_id) = self.pane_foreground_process_group_observation(pane_id).0
        else {
            return false;
        };
        if self.pane_process_group_is_certified_shell(pane_id, process_group_id) != Some(false) {
            return false;
        }
        self.begin_uncertified_foreign_shell_boundary(pane_id, primary_process_id, process_group_id)
    }

    /// Starts explicit agent-entry discovery for the current pane foreground.
    ///
    /// Ordinary pane startup deliberately installs no managed shell adapter.
    /// Once the user shows the agent shell, both the original primary shell and
    /// a foreign foreground shell therefore use the dependency-free loader.
    /// The live foreground observation fences discovery against writing into an
    /// application that no longer owns the prompt selected by the user.
    pub(crate) fn begin_agent_entry_shell_boundary_for_current_foreground(
        &mut self,
        pane_id: &str,
    ) -> bool {
        let Some(primary_process_id) = self.primary_pid_for_live_pane_process(pane_id) else {
            return false;
        };
        let observed_process_group = self.pane_foreground_process_group_observation(pane_id).0;
        let process_group_id = observed_process_group
            .unwrap_or_else(|| self.pane_primary_process_group_id(pane_id, primary_process_id));
        if self
            .pane_process_group_is_certified_shell(pane_id, process_group_id)
            .is_none()
        {
            return false;
        }
        self.begin_uncertified_shell_boundary(
            pane_id,
            primary_process_id,
            process_group_id,
            observed_process_group.is_some(),
        )
    }

    /// Starts a generation-fenced discovery boundary for foreign foreground ownership.
    ///
    /// The transition invalidates all authority derived from the previous pane
    /// environment but intentionally retains host-managed adapter artifacts so
    /// the original primary shell can recover after the foreign process exits.
    /// Access to those artifacts remains blocked while this boundary is live.
    pub(crate) fn begin_uncertified_foreign_shell_boundary(
        &mut self,
        pane_id: &str,
        primary_process_id: u32,
        process_group_id: u32,
    ) -> bool {
        self.begin_uncertified_shell_boundary(pane_id, primary_process_id, process_group_id, true)
    }

    /// Starts a generation-fenced discovery boundary with explicit ownership evidence.
    fn begin_uncertified_shell_boundary(
        &mut self,
        pane_id: &str,
        primary_process_id: u32,
        process_group_id: u32,
        process_group_observed: bool,
    ) -> bool {
        if self
            .process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .is_some_and(|boundary| {
                boundary.primary_process_id == primary_process_id
                    && boundary.process_group_id == process_group_id
            })
        {
            return false;
        }

        self.process.next_shell_interaction_generation = self
            .process
            .next_shell_interaction_generation
            .saturating_add(1);
        let interaction_generation = self.process.next_shell_interaction_generation;
        self.process
            .pane_shell_interaction_generations
            .insert(pane_id.to_string(), interaction_generation);
        self.process.pane_certified_shell_identities.remove(pane_id);
        self.process.pane_probed_shell_identities.remove(pane_id);
        self.process.pane_shell_handoffs.remove(pane_id);
        self.process
            .pending_agent_subshell_start_observations
            .remove(pane_id);
        self.process
            .pending_agent_subshell_certifications
            .remove(pane_id);
        self.process
            .pending_shell_dispatch_recovery_observations
            .remove(pane_id);
        self.process
            .bootstrap_shell_certification_evidence
            .retain(|_, evidence| evidence.pane_id != pane_id);
        self.process.pane_environment_signatures.remove(pane_id);
        self.process
            .pane_path_scopes
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_path_scope_failures
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_environment_evidence
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_bubblewrap_capabilities
            .retain(|key, _| key.pane_id != pane_id);
        self.clear_pane_agent_instruction_files(pane_id);
        self.clear_pane_environment_authority_failure(pane_id);
        self.process.pane_bootstrap_pending.remove(pane_id);
        let started_at_unix_ms = current_unix_millis();
        self.process.pane_foreign_shell_boundaries.insert(
            pane_id.to_string(),
            RuntimeForeignShellBoundary {
                primary_process_id,
                process_group_id,
                process_group_observed,
                interaction_generation,
                phase: RuntimeForeignShellBootstrapPhase::AwaitingPrompt,
                lifecycle_started_at_unix_ms: started_at_unix_ms,
                phase_started_at_unix_ms: started_at_unix_ms,
                child_token: None,
                child_shell: None,
                loader_marker: None,
                loader_payload: None,
                loader_ready: false,
                child_staging_source: None,
                identity_marker: None,
            },
        );
        self.process
            .pane_bootstrap_pending
            .insert(pane_id.to_string());
        self.set_pane_readiness(pane_id, super::PaneReadinessState::Unknown);
        true
    }

    /// Clears a foreign boundary after the certified primary shell regains the PTY.
    pub(crate) fn clear_uncertified_foreign_shell_boundary(&mut self, pane_id: &str) -> bool {
        let cleared = self
            .process
            .pane_foreign_shell_boundaries
            .remove(pane_id)
            .is_some();
        if cleared {
            self.process.pane_bootstrap_pending.remove(pane_id);
        }
        cleared
    }

    /// Reports whether a certified dependency-free loader owns parent restoration.
    ///
    /// SSH and similar transports expose only their local outer process group, so the
    /// managed remote child and its unmanaged parent can share one host-side identity.
    /// Runtime-owned shell actions can temporarily replace the cached foreground group.
    /// After child certification, the live loader marker and stable process-generation
    /// fence remain authoritative until the correlated exit proves parent restoration.
    pub(crate) fn dependency_free_foreign_loader_owns_parent_restoration(
        &self,
        pane_id: &str,
    ) -> bool {
        self.process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .is_some_and(|boundary| {
                boundary.phase == RuntimeForeignShellBootstrapPhase::Certified
                    && boundary.loader_marker.is_some()
                    && boundary.loader_ready
                    && self.primary_pid_for_live_pane_process(pane_id)
                        == Some(boundary.primary_process_id)
                    && self
                        .process
                        .pane_shell_interaction_generations
                        .get(pane_id)
                        .copied()
                        == Some(boundary.interaction_generation)
            })
    }

    /// Reports whether a dependency-free loader owns one observed process group.
    pub(crate) fn dependency_free_foreign_loader_owns_process_group(
        &self,
        pane_id: &str,
        process_group_id: u32,
    ) -> bool {
        self.process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .is_some_and(|boundary| {
                boundary.phase != RuntimeForeignShellBootstrapPhase::Failed
                    && boundary.process_group_id == process_group_id
                    && self.primary_pid_for_live_pane_process(pane_id)
                        == Some(boundary.primary_process_id)
                    && self
                        .process
                        .pane_shell_interaction_generations
                        .get(pane_id)
                        .copied()
                        == Some(boundary.interaction_generation)
            })
    }

    /// Returns generation-fenced loader ownership usable as remote bootstrap proof.
    ///
    /// An SSH worker can observe only the outer SSH process group, so another
    /// foreground query cannot distinguish the managed remote child from its
    /// parent. A ready loader, matching bootstrap handoff, and authenticated
    /// managed-child installation provide the stronger remote ownership proof.
    fn dependency_free_foreign_bootstrap_process_group(
        &self,
        pane_id: &str,
        marker: &str,
    ) -> Option<u32> {
        let boundary = self.process.pane_foreign_shell_boundaries.get(pane_id)?;
        let primary_process_group =
            self.pane_primary_process_group_id(pane_id, boundary.primary_process_id);
        if boundary.process_group_id == boundary.primary_process_id
            || boundary.process_group_id == primary_process_group
        {
            return None;
        }
        let handoff_matches =
            self.process
                .pane_shell_handoffs
                .get(pane_id)
                .is_some_and(|handoff| {
                    handoff.bootstrap_marker.as_deref() == Some(marker)
                        && handoff.primary_process_id == boundary.primary_process_id
                        && handoff.interaction_generation == boundary.interaction_generation
                });
        let managed_child_owns_input = boundary.child_shell.is_none()
            || self
                .process
                .pane_managed_shell_handoffs
                .get(pane_id)
                .is_some_and(|handoff| {
                    handoff.identity().marker == marker && handoff.child_is_installed()
                });
        (boundary.phase == RuntimeForeignShellBootstrapPhase::BootstrappingChild
            && boundary.loader_marker.is_some()
            && boundary.loader_ready
            && handoff_matches
            && managed_child_owns_input
            && self.primary_pid_for_live_pane_process(pane_id) == Some(boundary.primary_process_id)
            && self
                .process
                .pane_shell_interaction_generations
                .get(pane_id)
                .copied()
                == Some(boundary.interaction_generation))
        .then_some(boundary.process_group_id)
    }

    /// Starts a new runtime-owned agent-subshell handoff and invalidates state
    /// derived from the previous pane environment before bootstrap dispatch.
    pub(crate) fn begin_agent_subshell_shell_handoff(&mut self, pane_id: &str) -> Result<()> {
        let primary_process_id =
            self.primary_pid_for_live_pane_process(pane_id)
                .ok_or_else(|| {
                    crate::error::MezError::invalid_state("pane shell process is unavailable")
                })?;
        let mut execution_identity = self.shell_execution_identity_for_pane(pane_id)?;
        self.clear_pane_environment_authority_failure(pane_id);
        self.process.next_shell_interaction_generation = self
            .process
            .next_shell_interaction_generation
            .saturating_add(1);
        let interaction_generation = self.process.next_shell_interaction_generation;
        self.process
            .pane_shell_interaction_generations
            .insert(pane_id.to_string(), interaction_generation);
        self.process
            .pane_agent_subshell_certification_rejections
            .remove(pane_id);
        self.process.pane_certified_shell_identities.remove(pane_id);
        self.process.pane_probed_shell_identities.remove(pane_id);
        execution_identity.primary_process_id = Some(primary_process_id);
        execution_identity.interaction_generation = Some(interaction_generation);
        self.process.pane_probed_shell_identities.insert(
            pane_id.to_string(),
            RuntimePaneProbedShellIdentity {
                primary_process_id,
                interaction_generation,
                execution_identity,
            },
        );
        self.process
            .pending_agent_subshell_certifications
            .remove(pane_id);
        self.process
            .pending_agent_subshell_start_observations
            .remove(pane_id);
        self.process
            .bootstrap_shell_certification_evidence
            .retain(|_, evidence| evidence.pane_id != pane_id);
        self.process.pane_shell_handoffs.insert(
            pane_id.to_string(),
            RuntimePaneShellHandoff {
                primary_process_id,
                interaction_generation,
                bootstrap_marker: None,
                deferred_bootstrap_wrapper: None,
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
            .pane_environment_evidence
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

    /// Holds a registered bootstrap wrapper until the new child shell reports
    /// its authenticated receiver installation or, for unmanaged shells,
    /// prompt readiness.
    pub(crate) fn defer_agent_subshell_bootstrap_wrapper(
        &mut self,
        pane_id: &str,
        marker: &str,
        wrapper: String,
    ) {
        if let Some(handoff) = self.process.pane_shell_handoffs.get_mut(pane_id)
            && handoff.bootstrap_marker.as_deref() == Some(marker)
        {
            handoff.deferred_bootstrap_wrapper = Some(wrapper);
        }
    }

    /// Captures or requests persistent-receiver evidence before payload release.
    ///
    /// The wrapper is blocked in its payload read loop at this boundary, so the
    /// foreground group belongs to the persistent agent subshell rather than an
    /// isolated child launched later by the transaction body.
    ///
    /// Returns `true` when an adapter-owned observation is pending and payload
    /// release must wait for its correlated result.
    pub(crate) fn observe_agent_subshell_bootstrap_start(
        &mut self,
        pane_id: &str,
        marker: &str,
    ) -> bool {
        let Some(handoff) = self.process.pane_shell_handoffs.get(pane_id) else {
            return false;
        };
        if handoff.bootstrap_marker.as_deref() != Some(marker) {
            return false;
        }
        if let Some(process_group_id) =
            self.dependency_free_foreign_bootstrap_process_group(pane_id, marker)
        {
            self.record_agent_subshell_bootstrap_start_observation(
                pane_id,
                marker,
                Some(process_group_id),
            );
            return false;
        }
        if let Some(instance) = self.adapter_owned_pane_process_instance(pane_id) {
            let observation_id = format!("{marker}:foreground-start:{}", instance.generation);
            self.process
                .pending_agent_subshell_start_observations
                .insert(
                    pane_id.to_string(),
                    RuntimePendingAgentSubshellStartObservation {
                        instance: instance.clone(),
                        observation_id: observation_id.clone(),
                        marker: marker.to_string(),
                    },
                );
            self.persistence
                .queue_pane_observation(RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id,
                        expected_process_group_id: None,
                    },
                });
            return true;
        }
        let process_group_id = self.pane_foreground_process_group_observation(pane_id).0;
        self.record_agent_subshell_bootstrap_start_observation(pane_id, marker, process_group_id);
        false
    }

    /// Records one fresh persistent-receiver process-group observation.
    fn record_agent_subshell_bootstrap_start_observation(
        &mut self,
        pane_id: &str,
        marker: &str,
        process_group_id: Option<u32>,
    ) {
        let Some(handoff) = self.process.pane_shell_handoffs.get(pane_id) else {
            return;
        };
        if handoff.bootstrap_marker.as_deref() != Some(marker) {
            return;
        }
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

    /// Settles synchronous proof or requests a fresh adapter-owned observation.
    pub(crate) fn settle_agent_subshell_bootstrap_certification(
        &mut self,
        pane_id: &str,
        marker: &str,
        exit_code: i32,
        observed_output_truncated: bool,
        environment: Option<RuntimePendingBootstrapEnvironment>,
    ) -> RuntimeAgentSubshellCertificationOutcome {
        let Some(handoff) = self.process.pane_shell_handoffs.get(pane_id).cloned() else {
            return RuntimeAgentSubshellCertificationOutcome::NotApplicable;
        };
        if handoff.bootstrap_marker.as_deref() != Some(marker) {
            return RuntimeAgentSubshellCertificationOutcome::NotApplicable;
        }
        let evidence = self
            .process
            .bootstrap_shell_certification_evidence
            .get(marker)
            .cloned();
        let current_primary_process_id = self.primary_pid_for_live_pane_process(pane_id);
        let current_interaction_generation = self
            .process
            .pane_shell_interaction_generations
            .get(pane_id)
            .copied();
        let rejection = match evidence.as_ref() {
            None => Some(RuntimeAgentSubshellCertificationRejection::MissingStartEvidence),
            Some(evidence)
                if evidence.pane_id != pane_id
                    || evidence.primary_process_id != handoff.primary_process_id
                    || current_primary_process_id != Some(handoff.primary_process_id) =>
            {
                Some(RuntimeAgentSubshellCertificationRejection::PrimaryProcessChanged)
            }
            Some(evidence)
                if evidence.interaction_generation != handoff.interaction_generation
                    || current_interaction_generation != Some(handoff.interaction_generation) =>
            {
                Some(RuntimeAgentSubshellCertificationRejection::InteractionGenerationChanged)
            }
            Some(evidence) if evidence.process_group_id.is_none() => {
                Some(RuntimeAgentSubshellCertificationRejection::ForegroundProcessUnavailable)
            }
            Some(_) if exit_code != 0 => {
                Some(RuntimeAgentSubshellCertificationRejection::TransactionFailed)
            }
            Some(_) if observed_output_truncated => {
                Some(RuntimeAgentSubshellCertificationRejection::OutputTruncated)
            }
            Some(_) if environment.is_none() => {
                Some(RuntimeAgentSubshellCertificationRejection::EnvironmentSignatureMissing)
            }
            Some(_) => None,
        };
        if let Some(rejection) = rejection {
            self.remove_agent_subshell_bootstrap_proof(pane_id, marker);
            self.reject_agent_subshell_certification(pane_id, rejection);
            return RuntimeAgentSubshellCertificationOutcome::Rejected(rejection);
        }

        let (Some(evidence), Some(environment)) = (evidence, environment) else {
            let rejection = RuntimeAgentSubshellCertificationRejection::MissingStartEvidence;
            self.remove_agent_subshell_bootstrap_proof(pane_id, marker);
            self.reject_agent_subshell_certification(pane_id, rejection);
            return RuntimeAgentSubshellCertificationOutcome::Rejected(rejection);
        };
        if self
            .dependency_free_foreign_bootstrap_process_group(pane_id, marker)
            .is_some_and(|process_group_id| evidence.process_group_id == Some(process_group_id))
        {
            let Some(process_group_id) = evidence.process_group_id else {
                return RuntimeAgentSubshellCertificationOutcome::Rejected(
                    RuntimeAgentSubshellCertificationRejection::ForegroundProcessUnavailable,
                );
            };
            self.remove_agent_subshell_bootstrap_proof(pane_id, marker);
            self.promote_agent_subshell_certification(
                pane_id,
                evidence,
                environment,
                process_group_id,
            );
            return RuntimeAgentSubshellCertificationOutcome::Certified;
        }
        if let Some(instance) = self.adapter_owned_pane_process_instance(pane_id) {
            let Some(expected_process_group_id) = evidence.process_group_id else {
                let rejection =
                    RuntimeAgentSubshellCertificationRejection::ForegroundProcessUnavailable;
                self.remove_agent_subshell_bootstrap_proof(pane_id, marker);
                self.reject_agent_subshell_certification(pane_id, rejection);
                return RuntimeAgentSubshellCertificationOutcome::Rejected(rejection);
            };
            let observation_id = format!("{marker}:foreground:{}", instance.generation);
            self.remove_agent_subshell_bootstrap_proof(pane_id, marker);
            self.process.pending_agent_subshell_certifications.insert(
                pane_id.to_string(),
                RuntimePendingAgentSubshellCertification {
                    marker: marker.to_string(),
                    instance: instance.clone(),
                    observation_id: observation_id.clone(),
                    evidence,
                    environment,
                    started_at_unix_ms: current_unix_millis(),
                    timeout_ms: RUNTIME_AGENT_SUBSHELL_CERTIFICATION_TIMEOUT_MS,
                },
            );
            self.persistence
                .queue_pane_observation(RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id,
                        expected_process_group_id: Some(expected_process_group_id),
                    },
                });
            return RuntimeAgentSubshellCertificationOutcome::Pending;
        }

        let current_foreground_group = self.pane_foreground_process_group_observation(pane_id).0;
        let rejection = match current_foreground_group {
            None => Some(RuntimeAgentSubshellCertificationRejection::ForegroundProcessUnavailable),
            Some(process_group_id) if Some(process_group_id) != evidence.process_group_id => {
                Some(RuntimeAgentSubshellCertificationRejection::ForegroundProcessGroupChanged)
            }
            Some(_) => None,
        };
        self.remove_agent_subshell_bootstrap_proof(pane_id, marker);
        if let Some(rejection) = rejection {
            self.reject_agent_subshell_certification(pane_id, rejection);
            RuntimeAgentSubshellCertificationOutcome::Rejected(rejection)
        } else if let Some(process_group_id) = current_foreground_group {
            self.promote_agent_subshell_certification(
                pane_id,
                evidence,
                environment,
                process_group_id,
            );
            RuntimeAgentSubshellCertificationOutcome::Certified
        } else {
            let rejection =
                RuntimeAgentSubshellCertificationRejection::ForegroundProcessUnavailable;
            self.reject_agent_subshell_certification(pane_id, rejection);
            RuntimeAgentSubshellCertificationOutcome::Rejected(rejection)
        }
    }

    /// Applies the exact pane-worker observation requested by pending certification.
    pub(crate) fn apply_pane_foreground_process_observation_transition(
        &mut self,
        instance: PaneProcessInstance,
        observation: PaneForegroundProcessObservation,
    ) -> Result<RuntimeTransition> {
        if let Some(handoff) = self
            .process
            .pane_managed_shell_handoffs
            .get(&instance.pane_id)
            .cloned()
            && handoff.recovery_observation().is_some_and(|pending| {
                pending.instance == instance && pending.observation_id == observation.observation_id
            })
        {
            let current_primary_process_id =
                self.primary_pid_for_live_pane_process(&instance.pane_id);
            let current_interaction_generation = self
                .process
                .pane_shell_interaction_generations
                .get(&instance.pane_id)
                .copied();
            let parent_foreground = handoff.identity().primary_process_id.is_some()
                && observation.error.is_none()
                && observation.process_group_id == handoff.identity().primary_process_id
                && current_primary_process_id == handoff.identity().primary_process_id
                && current_interaction_generation == handoff.identity().interaction_generation;
            if !parent_foreground {
                if let Some(current) = self
                    .process
                    .pane_managed_shell_handoffs
                    .get_mut(&instance.pane_id)
                {
                    let _ = reduce_managed_shell_handoff(
                        current,
                        ManagedShellHandoffEvent::RecoveryProofRejected {
                            now_unix_ms: current_unix_millis(),
                        },
                    );
                }
                self.set_pane_readiness(&instance.pane_id, PaneReadinessState::Degraded);
                self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","managed_shell_handoff":"proof_rejected","marker":"{}","observation_id":"{}"}}"#,
                        json_escape(&instance.pane_id),
                        json_escape(&handoff.identity().marker),
                        json_escape(&observation.observation_id)
                    ),
                )?;
                return Ok(self.runtime_transition_with_render(
                    true,
                    Some(RenderInvalidationReason::PaneOutput),
                ));
            }

            let current_identity = ManagedShellHandoffIdentity {
                marker: handoff.identity().marker.clone(),
                process_instance: self.adapter_owned_pane_process_instance(&instance.pane_id),
                primary_process_id: current_primary_process_id,
                interaction_generation: current_interaction_generation,
                parent_proof: handoff.identity().parent_proof.clone(),
            };
            let transition = {
                let Some(current) = self
                    .process
                    .pane_managed_shell_handoffs
                    .get_mut(&instance.pane_id)
                else {
                    return Ok(RuntimeTransition::default());
                };
                reduce_managed_shell_handoff(
                    current,
                    ManagedShellHandoffEvent::RecoveryProofAccepted {
                        identity: current_identity,
                        instance: instance.clone(),
                        observation_id: observation.observation_id.clone(),
                    },
                )
            };
            let Some(pending_input) =
                transition
                    .effects
                    .into_iter()
                    .find_map(|effect| match effect {
                        ManagedShellHandoffEffect::Settle { pending_input, .. } => {
                            Some(pending_input)
                        }
                        _ => None,
                    })
            else {
                return Ok(RuntimeTransition::default());
            };
            if self
                .process
                .pane_environment_authority_failures
                .contains_key(&instance.pane_id)
            {
                self.set_pane_readiness(&instance.pane_id, PaneReadinessState::Degraded);
            } else {
                self.set_pane_readiness(&instance.pane_id, PaneReadinessState::PromptCandidate);
            }
            self.settle_managed_shell_runtime_ownership(
                &instance.pane_id,
                pending_input,
                ManagedShellSettlementRenderPolicy::RetainManagedBashRepaintSuppression,
            )?;
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","managed_shell_handoff":"proof_accepted","marker":"{}","observation_id":"{}"}}"#,
                    json_escape(&instance.pane_id),
                    json_escape(&handoff.identity().marker),
                    json_escape(&observation.observation_id)
                ),
            )?;
            return Ok(self
                .runtime_transition_with_render(true, Some(RenderInvalidationReason::FullRedraw)));
        }
        if let Some(pending) = self
            .process
            .pending_shell_dispatch_recovery_observations
            .get(&instance.pane_id)
            .cloned()
            && pending.instance == instance
            && pending.observation_id == observation.observation_id
        {
            self.process
                .pending_shell_dispatch_recovery_observations
                .remove(&instance.pane_id);
            let current_primary_process_id =
                self.primary_pid_for_live_pane_process(&instance.pane_id);
            let current_interaction_generation = self
                .process
                .pane_shell_interaction_generations
                .get(&instance.pane_id)
                .copied();
            if current_primary_process_id != Some(pending.primary_process_id)
                || current_interaction_generation.unwrap_or_default()
                    != pending.interaction_generation
                || self
                    .pending_shell_action_id_for_turn(&pending.turn_id)
                    .as_deref()
                    != Some(pending.action_id.as_str())
            {
                return Ok(RuntimeTransition::default());
            }
            if observation.error.is_some() || observation.process_group_id.is_none() {
                self.set_pane_readiness(&instance.pane_id, PaneReadinessState::Degraded);
                let _ = self.queue_agent_provider_task(pending.turn_id.clone());
                self.append_agent_trace_turn_event(
                    &instance.pane_id,
                    &pending.turn_id,
                    &format!(
                        "action {} waiting reason=foreground_process_observation_unavailable",
                        pending.action_id
                    ),
                )?;
                return Ok(self.runtime_transition_with_render(
                    true,
                    Some(RenderInvalidationReason::PaneOutput),
                ));
            }
            self.apply_correlated_pane_foreground_observation(&instance.pane_id, &observation)?;
            let Some(process_group_id) = observation.process_group_id else {
                return Ok(RuntimeTransition::default());
            };
            if self.pane_foreground_certified_shell_state(&instance.pane_id) == Some(true) {
                self.clear_pending_shell_dispatch_blocked_recovery_attempt(
                    &pending.turn_id,
                    &pending.action_id,
                );
                self.set_pane_readiness(&instance.pane_id, PaneReadinessState::PromptCandidate);
                let _ = self.queue_agent_provider_task(pending.turn_id.clone());
                self.append_agent_trace_turn_event(
                    &instance.pane_id,
                    &pending.turn_id,
                    &format!(
                        "action {} recovered reason=fresh_certified_foreground_process",
                        pending.action_id
                    ),
                )?;
                return Ok(self.runtime_transition_with_render(
                    true,
                    Some(RenderInvalidationReason::PaneOutput),
                ));
            }
            let confirmations = self.record_pending_shell_dispatch_blocked_recovery_observation(
                &pending.turn_id,
                &pending.action_id,
                pending.primary_process_id,
                pending.interaction_generation,
                process_group_id,
            );
            let deadline_exhausted = self
                .pending_shell_dispatch_blocked_recovery_deadline_exhausted(
                    &pending.turn_id,
                    &pending.action_id,
                );
            if confirmations >= 3 || deadline_exhausted {
                let _ = self.queue_agent_provider_task(pending.turn_id.clone());
            }
            self.append_agent_trace_turn_event(
                &instance.pane_id,
                &pending.turn_id,
                &format!(
                    "action {} waiting reason=fresh_foreground_process_blocked confirmations={} deadline_exhausted={} foreground_process_group={}",
                    pending.action_id, confirmations, deadline_exhausted, process_group_id
                ),
            )?;
            return Ok(self
                .runtime_transition_with_render(true, Some(RenderInvalidationReason::PaneOutput)));
        }
        if let Some(pending) = self
            .process
            .pending_agent_subshell_start_observations
            .get(&instance.pane_id)
            .cloned()
            && pending.instance == instance
            && pending.observation_id == observation.observation_id
        {
            self.process
                .pending_agent_subshell_start_observations
                .remove(&instance.pane_id);
            if observation.error.is_none() {
                self.apply_correlated_pane_foreground_observation(&instance.pane_id, &observation)?;
            }
            let process_group_id = if observation.error.is_none() {
                observation.process_group_id
            } else {
                None
            };
            self.record_agent_subshell_bootstrap_start_observation(
                &instance.pane_id,
                &pending.marker,
                process_group_id,
            );
            self.release_agent_shell_transaction_payload_after_start(
                &pending.marker,
                &instance.pane_id,
            )?;
            return Ok(self
                .runtime_transition_with_render(true, Some(RenderInvalidationReason::PaneOutput)));
        }
        let Some(pending) = self
            .process
            .pending_agent_subshell_certifications
            .get(&instance.pane_id)
            .cloned()
        else {
            return Ok(RuntimeTransition::default());
        };
        if pending.instance != instance || pending.observation_id != observation.observation_id {
            return Ok(RuntimeTransition::default());
        }
        self.process
            .pending_agent_subshell_certifications
            .remove(&instance.pane_id);
        if observation.error.is_none() {
            self.apply_correlated_pane_foreground_observation(&instance.pane_id, &observation)?;
        }

        let current_primary_process_id = self.primary_pid_for_live_pane_process(&instance.pane_id);
        let current_interaction_generation = self
            .process
            .pane_shell_interaction_generations
            .get(&instance.pane_id)
            .copied();
        let rejection = if current_primary_process_id != Some(pending.evidence.primary_process_id) {
            Some(RuntimeAgentSubshellCertificationRejection::PrimaryProcessChanged)
        } else if current_interaction_generation != Some(pending.evidence.interaction_generation) {
            Some(RuntimeAgentSubshellCertificationRejection::InteractionGenerationChanged)
        } else if observation.error.is_some() || observation.process_group_id.is_none() {
            Some(RuntimeAgentSubshellCertificationRejection::ForegroundProcessUnavailable)
        } else if observation.process_group_id != pending.evidence.process_group_id {
            Some(RuntimeAgentSubshellCertificationRejection::ForegroundProcessGroupChanged)
        } else {
            None
        };

        let outcome = if let Some(rejection) = rejection {
            self.reject_agent_subshell_certification(&instance.pane_id, rejection);
            RuntimeAgentSubshellCertificationOutcome::Rejected(rejection)
        } else if let Some(process_group_id) = observation.process_group_id {
            self.promote_agent_subshell_certification(
                &instance.pane_id,
                pending.evidence,
                pending.environment,
                process_group_id,
            );
            RuntimeAgentSubshellCertificationOutcome::Certified
        } else {
            let rejection =
                RuntimeAgentSubshellCertificationRejection::ForegroundProcessUnavailable;
            self.reject_agent_subshell_certification(&instance.pane_id, rejection);
            RuntimeAgentSubshellCertificationOutcome::Rejected(rejection)
        };
        self.process
            .pane_bootstrap_pending
            .remove(&instance.pane_id);
        match outcome {
            RuntimeAgentSubshellCertificationOutcome::Certified => {
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"certified","marker":"{}","observation":"fresh_worker"}}"#,
                        json_escape(&instance.pane_id),
                        json_escape(&pending.marker)
                    ),
                )?;
                self.set_pane_readiness(&instance.pane_id, PaneReadinessState::Ready);
            }
            RuntimeAgentSubshellCertificationOutcome::Rejected(reason) => {
                self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"certification_failed","marker":"{}","reason":"{}"}}"#,
                        json_escape(&instance.pane_id),
                        json_escape(&pending.marker),
                        reason.as_str()
                    ),
                )?;
                self.set_pane_readiness(&instance.pane_id, PaneReadinessState::Degraded);
            }
            RuntimeAgentSubshellCertificationOutcome::NotApplicable
            | RuntimeAgentSubshellCertificationOutcome::Pending => {}
        }
        self.resume_after_bootstrap_settlement(&instance.pane_id)?;
        Ok(self.runtime_transition_with_render(true, Some(RenderInvalidationReason::FullRedraw)))
    }

    /// Requests one fresh, instance-correlated foreground observation for a
    /// blocked shell action. Existing recovery ownership suppresses duplicate
    /// requests until its exact worker result arrives.
    pub(crate) fn request_shell_dispatch_recovery_observation(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        if self
            .process
            .pending_shell_dispatch_recovery_observations
            .contains_key(pane_id)
        {
            return false;
        }
        let (Some(instance), Some(primary_process_id)) = (
            self.adapter_owned_pane_process_instance(pane_id),
            self.primary_pid_for_live_pane_process(pane_id),
        ) else {
            return false;
        };
        let interaction_generation = self
            .process
            .pane_shell_interaction_generations
            .get(pane_id)
            .copied()
            .unwrap_or_default();
        self.process.next_shell_dispatch_recovery_observation = self
            .process
            .next_shell_dispatch_recovery_observation
            .saturating_add(1);
        let observation_id = format!(
            "foreground-recovery:{}:{}:{}",
            instance.generation, turn_id, self.process.next_shell_dispatch_recovery_observation
        );
        self.process
            .pending_shell_dispatch_recovery_observations
            .insert(
                pane_id.to_string(),
                RuntimePendingShellDispatchRecoveryObservation {
                    instance: instance.clone(),
                    observation_id: observation_id.clone(),
                    turn_id: turn_id.to_string(),
                    action_id: action_id.to_string(),
                    primary_process_id,
                    interaction_generation,
                    started_at_unix_ms: current_unix_millis(),
                },
            );
        self.persistence
            .queue_pane_observation(RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: PaneProcessIoEffect::ObserveForegroundProcess {
                    observation_id,
                    expected_process_group_id: None,
                },
            });
        true
    }

    /// Expires adapter-owned completion certifications whose exact correlated
    /// worker event did not settle before the runtime-owned deadline.
    pub(super) fn expire_timed_out_agent_subshell_certifications(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        let expired = self
            .process
            .pending_agent_subshell_certifications
            .iter()
            .filter_map(|(pane_id, pending)| {
                let elapsed_ms = now_unix_ms.saturating_sub(pending.started_at_unix_ms);
                (elapsed_ms >= pending.timeout_ms).then(|| {
                    (
                        pane_id.clone(),
                        pending.marker.clone(),
                        pending.observation_id.clone(),
                        pending.timeout_ms,
                        elapsed_ms,
                    )
                })
            })
            .collect::<Vec<_>>();
        let mut expired_count = 0usize;
        for (pane_id, marker, observation_id, timeout_ms, elapsed_ms) in expired {
            let still_pending = self
                .process
                .pending_agent_subshell_certifications
                .get(&pane_id)
                .is_some_and(|pending| {
                    pending.marker == marker && pending.observation_id == observation_id
                });
            if !still_pending {
                continue;
            }
            self.process
                .pending_agent_subshell_certifications
                .remove(&pane_id);
            self.reject_agent_subshell_certification(
                &pane_id,
                RuntimeAgentSubshellCertificationRejection::ForegroundObservationTimedOut,
            );
            self.process.pane_bootstrap_pending.remove(&pane_id);
            self.set_pane_readiness(&pane_id, PaneReadinessState::Degraded);
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","bootstrap":"certification_failed","marker":"{}","reason":"{}","observation_id":"{}","timeout_ms":{},"elapsed_ms":{}}}"#,
                    json_escape(&pane_id),
                    json_escape(&marker),
                    RuntimeAgentSubshellCertificationRejection::ForegroundObservationTimedOut
                        .as_str(),
                    json_escape(&observation_id),
                    timeout_ms,
                    elapsed_ms
                ),
            )?;
            self.resume_after_bootstrap_settlement(&pane_id)?;
            expired_count = expired_count.saturating_add(1);
        }
        Ok(expired_count)
    }

    /// Releases a blocked dispatch when its exact recovery-owned foreground
    /// observation was lost or stale. Missing metadata never contributes a
    /// foreign-process confirmation, so the action returns to ordinary
    /// degraded-readiness handling rather than being denied from a timer tick.
    pub(super) fn expire_timed_out_shell_dispatch_recovery_observations(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        let expired = self
            .process
            .pending_shell_dispatch_recovery_observations
            .iter()
            .filter_map(|(pane_id, pending)| {
                let elapsed_ms = now_unix_ms.saturating_sub(pending.started_at_unix_ms);
                (elapsed_ms >= RUNTIME_SHELL_DISPATCH_RECOVERY_OBSERVATION_TIMEOUT_MS).then(|| {
                    (
                        pane_id.clone(),
                        pending.observation_id.clone(),
                        pending.turn_id.clone(),
                        pending.action_id.clone(),
                        elapsed_ms,
                    )
                })
            })
            .collect::<Vec<_>>();
        let mut expired_count = 0usize;
        for (pane_id, observation_id, turn_id, action_id, elapsed_ms) in expired {
            let still_pending = self
                .process
                .pending_shell_dispatch_recovery_observations
                .get(&pane_id)
                .is_some_and(|pending| {
                    pending.observation_id == observation_id
                        && pending.turn_id == turn_id
                        && pending.action_id == action_id
                });
            if !still_pending {
                continue;
            }
            self.process
                .pending_shell_dispatch_recovery_observations
                .remove(&pane_id);
            if self.pending_shell_action_id_for_turn(&turn_id).as_deref()
                != Some(action_id.as_str())
            {
                continue;
            }
            self.set_pane_readiness(&pane_id, PaneReadinessState::Degraded);
            let _ = self.queue_agent_provider_task(turn_id.clone());
            self.append_agent_trace_turn_event(
                &pane_id,
                &turn_id,
                &format!(
                    "action {} waiting reason=foreground_process_observation_timed_out elapsed_ms={}",
                    action_id, elapsed_ms
                ),
            )?;
            expired_count = expired_count.saturating_add(1);
        }
        Ok(expired_count)
    }

    /// Refreshes pane metadata from one accepted correlated worker observation.
    fn apply_correlated_pane_foreground_observation(
        &mut self,
        pane_id: &str,
        observation: &PaneForegroundProcessObservation,
    ) -> Result<()> {
        let (Some(process_name), Some(process_group_id)) = (
            observation.process_name.clone(),
            observation.process_group_id,
        ) else {
            return Ok(());
        };
        let _ = self.apply_pane_foreground_process_event(
            pane_id,
            process_name,
            process_group_id,
            observation.current_working_directory.clone(),
        )?;
        Ok(())
    }

    /// Publishes environment-derived context after certification succeeds.
    pub(crate) fn publish_bootstrap_environment(
        &mut self,
        pane_id: &str,
        environment: RuntimePendingBootstrapEnvironment,
    ) {
        let RuntimePendingBootstrapEnvironment {
            signature,
            tool_inventory,
            instruction_files,
        } = environment;
        self.clear_pane_environment_authority_failure(pane_id);
        self.process
            .pane_path_scopes
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_path_scope_failures
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_environment_evidence
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_environment_signatures
            .insert(pane_id.to_string(), signature.clone());
        if let Some(inventory) = tool_inventory {
            self.record_agent_tool_inventory(signature, inventory);
        }
        if !instruction_files.is_empty() {
            self.set_pane_agent_instruction_files(pane_id, instruction_files);
        }
    }

    /// Removes marker-bound handoff state after certification leaves phase one.
    fn remove_agent_subshell_bootstrap_proof(&mut self, pane_id: &str, marker: &str) {
        self.process.pane_shell_handoffs.remove(pane_id);
        self.process
            .bootstrap_shell_certification_evidence
            .remove(marker);
    }

    /// Publishes context and records the certified persistent receiver.
    fn promote_agent_subshell_certification(
        &mut self,
        pane_id: &str,
        evidence: RuntimeBootstrapShellCertificationEvidence,
        environment: RuntimePendingBootstrapEnvironment,
        process_group_id: u32,
    ) {
        let environment_signature = environment.signature.clone();
        self.publish_bootstrap_environment(pane_id, environment);
        self.process.pane_certified_shell_identities.insert(
            pane_id.to_string(),
            RuntimePaneCertifiedShellIdentity {
                primary_process_id: evidence.primary_process_id,
                process_group_id,
                interaction_generation: evidence.interaction_generation,
                environment_signature,
                source: RuntimeCertifiedShellSource::AgentSubshellBootstrap,
            },
        );
        self.process
            .pane_agent_subshell_certification_rejections
            .remove(pane_id);
        if let Some(boundary) = self.process.pane_foreign_shell_boundaries.get_mut(pane_id)
            && boundary.phase == RuntimeForeignShellBootstrapPhase::BootstrappingChild
            && boundary.primary_process_id == evidence.primary_process_id
            && boundary.interaction_generation == evidence.interaction_generation
        {
            boundary.phase = RuntimeForeignShellBootstrapPhase::Certified;
            boundary.phase_started_at_unix_ms = current_unix_millis();
            boundary.child_staging_source = None;
        }
    }

    /// Removes untrusted context and records one stable certification rejection.
    fn reject_agent_subshell_certification(
        &mut self,
        pane_id: &str,
        rejection: RuntimeAgentSubshellCertificationRejection,
    ) {
        self.process.pane_certified_shell_identities.remove(pane_id);
        self.process.pane_environment_signatures.remove(pane_id);
        self.process
            .pane_path_scopes
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_path_scope_failures
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_environment_evidence
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_bubblewrap_capabilities
            .retain(|key, _| key.pane_id != pane_id);
        self.clear_pane_agent_instruction_files(pane_id);
        self.process
            .pane_agent_subshell_certification_rejections
            .insert(pane_id.to_string(), rejection);
    }

    /// Invalidates every non-primary shell proof associated with one pane.
    pub(crate) fn clear_agent_subshell_shell_identity(&mut self, pane_id: &str) {
        self.clear_deferred_agent_subshell_entry(pane_id);
        self.process
            .pane_agent_subshell_certification_rejections
            .remove(pane_id);
        self.process.pane_certified_shell_identities.remove(pane_id);
        self.process.pane_probed_shell_identities.remove(pane_id);
        self.process.pane_shell_handoffs.remove(pane_id);
        self.process
            .pending_agent_subshell_start_observations
            .remove(pane_id);
        self.process
            .pending_agent_subshell_certifications
            .remove(pane_id);
        self.process
            .pending_shell_dispatch_recovery_observations
            .remove(pane_id);
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
        let wrapper_was_deferred = self
            .process
            .pane_shell_handoffs
            .get_mut(pane_id)
            .and_then(|handoff| handoff.deferred_bootstrap_wrapper.take())
            .is_some();
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
            .remove_running_shell_transaction(&marker)
            .and_then(|transaction| transaction.pending_input_payload);
        self.clear_shell_transaction_protocol_state(&marker);
        self.process
            .bootstrap_shell_certification_evidence
            .remove(&marker);
        self.process
            .pending_agent_subshell_start_observations
            .remove(pane_id);
        self.process.pane_bootstrap_pending.remove(pane_id);
        if wrapper_was_deferred {
            Some(Vec::new())
        } else {
            payload.map(|delivery| delivery.bytes)
        }
    }

    /// Invalidates child-environment evidence when control returns to the
    /// original pane shell without scheduling any hidden shell interaction.
    pub(crate) fn invalidate_agent_subshell_environment_after_exit(&mut self, pane_id: &str) {
        self.process
            .pane_agent_subshell_certification_rejections
            .remove(pane_id);
        self.clear_pane_environment_authority_failure(pane_id);
        self.process.pane_environment_signatures.remove(pane_id);
        self.process
            .pane_path_scopes
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_path_scope_failures
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_environment_evidence
            .retain(|key, _| key.pane_id != pane_id);
        self.process
            .pane_bubblewrap_capabilities
            .retain(|key, _| key.pane_id != pane_id);
        self.clear_pane_agent_instruction_files(pane_id);
        self.process.pane_bootstrap_pending.remove(pane_id);
        self.set_pane_readiness(pane_id, super::PaneReadinessState::Unknown);
    }

    /// Arms parent-shell discovery only after agent mode is visible again.
    ///
    /// The restored user shell remains fully user-owned while agent mode is
    /// hidden. Re-entry still fails closed by waiting for a fresh prompt and
    /// identity probe before a new agent child shell can start.
    pub(crate) fn schedule_parent_shell_discovery_for_agent_entry(
        &mut self,
        pane_id: &str,
    ) -> bool {
        if self.effective_agent_shell_mode_for_pane(pane_id)
            == crate::runtime::config::ShellMode::Native
            || self.pane_has_uncertified_foreign_shell_boundary(pane_id)
            || !self
                .process
                .pane_shell_interaction_generations
                .contains_key(pane_id)
            || self
                .process
                .pane_certified_shell_identities
                .contains_key(pane_id)
            || self
                .process
                .pane_probed_shell_identities
                .contains_key(pane_id)
        {
            return false;
        }
        self.clear_pane_environment_authority_failure(pane_id);
        self.process
            .pane_bootstrap_pending
            .insert(pane_id.to_string())
    }

    /// Returns the latest actionable agent-subshell certification rejection.
    pub(crate) fn pane_agent_subshell_certification_rejection(
        &self,
        pane_id: &str,
    ) -> Option<&'static str> {
        self.process
            .pane_agent_subshell_certification_rejections
            .get(pane_id)
            .copied()
            .map(RuntimeAgentSubshellCertificationRejection::as_str)
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
            agent_subshell_certification_rejection: self
                .pane_agent_subshell_certification_rejection(pane_id),
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
                TerminalOscEvent::ManagedShell {
                    version,
                    shell,
                    token,
                    event,
                } => {
                    observed = observed.saturating_add(self.observe_managed_shell_protocol_event(
                        output_pane_id,
                        *version,
                        *shell,
                        token,
                        event,
                    )?);
                }
                TerminalOscEvent::TitleChanged { .. }
                | TerminalOscEvent::Clipboard(_)
                | TerminalOscEvent::Progress(_) => {}
                TerminalOscEvent::ShellPromptStart => {}
                TerminalOscEvent::ShellPromptEnd => {
                    let current_primary_process_id =
                        self.primary_pid_for_live_pane_process(output_pane_id);
                    let fish_prompt_admission = self
                        .process
                        .pane_fish_admissions
                        .get(output_pane_id)
                        .and_then(|admission| {
                            match admission {
                            crate::runtime::processes::RuntimeManagedFishAdmission::AwaitingPrompt {
                                primary_process_id,
                                version,
                            } if Some(*primary_process_id) == current_primary_process_id => {
                                Some((*primary_process_id, *version))
                            }
                            _ => None,
                        }
                        });
                    if let Some((primary_process_id, version)) = fish_prompt_admission {
                        self.process.pane_fish_admissions.insert(
                            output_pane_id.to_string(),
                            crate::runtime::processes::RuntimeManagedFishAdmission::Ready {
                                primary_process_id,
                                version,
                            },
                        );
                        observed = observed.saturating_add(1);
                        if self.begin_managed_agent_surface_bootstrap(output_pane_id)? {
                            observed = observed.saturating_add(1);
                        } else {
                            observed = observed.saturating_add(self.maybe_bootstrap_ready_panes()?);
                        }
                    }
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-prompt-end",
                            )?);
                    }
                }
                TerminalOscEvent::ShellCommandFinished { .. } => {
                    if self
                        .process
                        .pane_terminal_progress
                        .remove(output_pane_id)
                        .is_some()
                    {
                        observed = observed.saturating_add(1);
                    }
                }
                TerminalOscEvent::ShellCommandOutputStart => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_busy(
                                output_pane_id,
                                "osc133-command-start",
                            )?);
                    }
                }
                TerminalOscEvent::ShellParentRestored {
                    token,
                    marker,
                    exit_code,
                } => {
                    observed = observed.saturating_add(self.observe_shell_parent_restored(
                        output_pane_id,
                        token,
                        marker,
                        *exit_code,
                    )?);
                }
                TerminalOscEvent::ShellReceiverReady { token, marker } => {
                    observed = observed.saturating_add(self.observe_shell_receiver_ready(
                        output_pane_id,
                        token,
                        marker,
                    )?);
                }
                TerminalOscEvent::ShellReceiverInstalled { token, marker } => {
                    observed = observed.saturating_add(self.observe_shell_receiver_installed(
                        output_pane_id,
                        token,
                        marker,
                    )?);
                }
                TerminalOscEvent::ShellReceiverComplete {
                    token,
                    marker,
                    exit_code,
                } => {
                    observed = observed.saturating_add(self.observe_shell_receiver_complete(
                        output_pane_id,
                        token,
                        marker,
                        *exit_code,
                    )?);
                }
                TerminalOscEvent::ForeignShellLoaderReady { marker } => {
                    observed = observed.saturating_add(
                        self.observe_foreign_shell_loader_ready(output_pane_id, marker)?,
                    );
                }
                TerminalOscEvent::ForeignShellLoaderExited { marker, exit_code } => {
                    observed = observed.saturating_add(self.observe_foreign_shell_loader_exited(
                        output_pane_id,
                        marker,
                        *exit_code,
                    )?);
                }
                TerminalOscEvent::ShellTransactionPayloadReceiverReady {
                    marker,
                    turn_id,
                    agent_id,
                    pane_id,
                } => {
                    observed = observed.saturating_add(
                        self.observe_shell_transaction_payload_receiver_ready(
                            output_pane_id,
                            marker,
                            turn_id,
                            agent_id,
                            pane_id,
                        )?,
                    );
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
