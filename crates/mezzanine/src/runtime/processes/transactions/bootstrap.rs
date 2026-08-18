//! Pane bootstrap dispatch and completion.

use mez_agent::{
    AgentShellVisibility, ShellClassification,
    agent_subshell_enter_command_with_shell_compatibility_and_exit_marker,
    agent_subshell_exit_marker_bytes, bash_private_source_input, parse_shell_identity_probe_output,
    shell_identity_probe_command,
};

use super::super::{
    RuntimeAgentSubshellCertificationOutcome, RuntimeForeignShellBootstrapPhase,
    RuntimePaneProbedShellIdentity, RuntimePaneShellExecutionIdentity, RuntimePaneShellHandoff,
    RuntimePendingBootstrapEnvironment,
};
use super::{
    AgentTurnState, DEFAULT_BOOTSTRAP_TIMEOUT_MS, EventKind, MezError, PaneReadinessState, Result,
    RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimePaneEnvironmentAuthorityUnavailableReason, RuntimeSessionService, ShellTransaction,
    bootstrap_script_for_classification, current_unix_millis, current_unix_seconds, json_escape,
    parse_bootstrap_env_output, runtime_random_marker_token,
};
use std::path::PathBuf;

impl RuntimeSessionService {
    /// Registers one syntax-neutral probe for the current pane interaction epoch.
    ///
    /// The outer command is accepted by POSIX-family and Fish parsers because
    /// all shell-specific work executes inside an explicit `/bin/sh` child.
    fn prepare_shell_identity_probe_to_pane(
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
        let Some(interaction_generation) = self
            .process
            .pane_shell_interaction_generations
            .get(pane_id)
            .copied()
        else {
            return Ok(None);
        };
        let primary_process_id = self
            .primary_pid_for_live_pane_process(pane_id)
            .ok_or_else(|| MezError::invalid_state("pane shell process is unavailable"))?;
        let agent_id = format!("agent-{pane_id}");
        let turn_id = format!("shell-identity-{pane_id}-{}", current_unix_seconds());
        let marker = runtime_random_marker_token(&format!(
            "shell-identity\0{pane_id}\0{turn_id}\0{interaction_generation}"
        ))?;
        let marker_id = marker.as_str().to_string();
        let command = shell_identity_probe_command(&marker_id, &turn_id, &agent_id, pane_id)?;
        let dependency_free_foreign_probe = self
            .process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .is_some_and(|boundary| {
                boundary.phase == RuntimeForeignShellBootstrapPhase::IdentityProbing
            });
        let classification = self.shell_classification_for_pane(pane_id);
        let mut input = command.clone();
        let mut staged_inputs = std::collections::VecDeque::new();
        let staged_shell = None;
        let mut completion_required = false;
        if classification == ShellClassification::Bash && !dependency_free_foreign_probe {
            let token = self.bash_receiver_token_for_pane(pane_id).ok_or_else(|| {
                MezError::invalid_state(
                    "managed Bash receiver is unavailable for shell identity probe",
                )
            })?;
            let private = bash_private_source_input(&command, token, &marker_id);
            input = private.wrapper;
            staged_inputs.push_back(mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                private.receiver_payload.into_bytes(),
                marker_id.clone(),
                true,
            ));
            completion_required = true;
        }
        if !input.ends_with('\n') {
            input.push('\n');
        }
        self.remember_mez_wrapper_filter_command(pane_id, &command);
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.register_running_shell_transaction(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id,
                kind: RunningShellTransactionKind::ShellIdentityProbe {
                    primary_process_id,
                    interaction_generation,
                },
                pane_id: pane_id.to_string(),
                command,
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(DEFAULT_BOOTSTRAP_TIMEOUT_MS),
                pending_input_payload: None,
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
            true,
        );
        if !staged_inputs.is_empty() {
            self.process
                .shell_receiver_pending_payloads
                .insert(marker_id.clone(), staged_inputs);
        }
        if let Some(shell) = staged_shell {
            self.register_managed_shell_handoff(&marker_id, shell, None);
        }
        if completion_required {
            self.process
                .shell_receiver_completion_required
                .insert(marker_id.clone());
        }
        if let Some(boundary) = self.process.pane_foreign_shell_boundaries.get_mut(pane_id) {
            boundary.identity_marker = Some(marker_id.clone());
        }
        Ok(Some((marker_id, input)))
    }

    /// Starts identity discovery directly at a user-authorized foreign prompt.
    ///
    /// Agent entry is the prompt-authority assertion. No remote Mezzanine
    /// binary or process-local adapter is required before this bounded probe.
    pub(crate) fn begin_dependency_free_foreign_shell_bootstrap(
        &mut self,
        pane_id: &str,
    ) -> Result<()> {
        let boundary = self
            .process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("foreign shell boundary is unavailable"))?;
        if boundary.phase != RuntimeForeignShellBootstrapPhase::AwaitingPrompt
            || self.primary_pid_for_live_pane_process(pane_id) != Some(boundary.primary_process_id)
            || self.pane_foreground_process_group_observation(pane_id).0
                != Some(boundary.process_group_id)
            || self
                .process
                .pane_shell_interaction_generations
                .get(pane_id)
                .copied()
                != Some(boundary.interaction_generation)
        {
            return Err(MezError::invalid_state(
                "foreign shell changed before dependency-free bootstrap",
            ));
        }
        if let Some(current) = self.process.pane_foreign_shell_boundaries.get_mut(pane_id) {
            current.phase = RuntimeForeignShellBootstrapPhase::IdentityProbing;
            current.phase_started_at_unix_ms = current_unix_millis();
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","foreign_bootstrap":"identity_probing","transport":"dependency-free","generation":{},"process_group_id":{}}}"#,
                json_escape(pane_id),
                boundary.interaction_generation,
                boundary.process_group_id
            ),
        )?;
        let Some((identity_marker, mut input)) =
            self.prepare_shell_identity_probe_to_pane(pane_id)?
        else {
            return Ok(());
        };
        let loader_token = runtime_random_marker_token(&format!(
            "dependency-free-foreign-loader\0{pane_id}\0{identity_marker}"
        ))?;
        let loader_marker = loader_token
            .as_str()
            .get(..32)
            .ok_or_else(|| MezError::invalid_state("foreign loader nonce is too short"))?;
        input.push_str(
            &mez_agent::dependency_free_foreign_shell_loader_command(loader_marker)
                .map_err(|error| MezError::invalid_state(error.to_string()))?,
        );
        if let Some(current) = self.process.pane_foreign_shell_boundaries.get_mut(pane_id) {
            current.loader_marker = Some(loader_marker.to_string());
            current.loader_ready = false;
        }
        if let Err(error) = self.write_runtime_pane_shell_input(pane_id, input.as_bytes()) {
            self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
            return Err(error);
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","shell_identity_probe":"sent","marker":"{}","foreign_loader":"queued","loader_marker":"{}"}}"#,
                json_escape(pane_id),
                json_escape(&identity_marker),
                json_escape(loader_marker)
            ),
        )?;
        Ok(())
    }

    /// Sends one registered syntax-neutral identity probe to the pane.
    pub(super) fn dispatch_shell_identity_probe_to_pane(&mut self, pane_id: &str) -> Result<()> {
        let Some((marker, input)) = self.prepare_shell_identity_probe_to_pane(pane_id)? else {
            return Ok(());
        };
        if let Err(error) = self.write_runtime_pane_shell_input(pane_id, input.as_bytes()) {
            self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
            return Err(error);
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","shell_identity_probe":"sent","marker":"{}"}}"#,
                json_escape(pane_id),
                json_escape(&marker)
            ),
        )?;
        Ok(())
    }

    /// Registers one pane bootstrap and returns the exact wrapper that must be
    /// delivered after any preceding shell-handoff input.
    ///
    /// The encoded command payload remains on the registered transaction until
    /// the runtime observes the wrapper's release boundary. Fish additionally
    /// requires its correlated receiver-ready event after transaction start.
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
        let shell_identity = self
            .process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .filter(|boundary| {
                boundary.phase == RuntimeForeignShellBootstrapPhase::BootstrappingChild
            })
            .and_then(|boundary| {
                self.process
                    .pane_probed_shell_identities
                    .get(pane_id)
                    .filter(|identity| {
                        identity.primary_process_id == boundary.primary_process_id
                            && identity.interaction_generation == boundary.interaction_generation
                    })
                    .map(|identity| identity.execution_identity.clone())
            })
            .map(Ok)
            .unwrap_or_else(|| self.shell_execution_identity_for_pane(pane_id))?;
        let classification = shell_identity.classification();
        let bootstrap_script = bootstrap_script_for_classification(classification);
        self.clear_pane_environment_authority_failure(pane_id);
        let transaction = self.configure_shell_transaction_for_pane(
            pane_id,
            ShellTransaction::new(
                marker,
                &turn_id,
                &agent_id,
                pane_id,
                shell_identity.shell_path(),
                bootstrap_script.clone(),
            )?,
        );
        let transaction_input = transaction.render_for_classification_input(classification);
        self.require_generated_shell_input(&transaction_input)?;
        let receiver_payload = (!transaction_input.receiver_payload.is_empty()).then(|| {
            mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                transaction_input.receiver_payload.clone().into_bytes(),
                marker_id.clone(),
                true,
            )
        });
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        let requires_payload_receiver_ready =
            classification == ShellClassification::Fish && !transaction_input.payload.is_empty();
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
        if requires_payload_receiver_ready {
            self.require_shell_transaction_payload_receiver_ready(&marker_id);
        }
        if let Some(receiver_payload) = receiver_payload {
            self.register_shell_receiver_payload(&marker_id, receiver_payload);
        }
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
        if self.process.pane_fish_compatibility.contains_key(pane_id)
            && !self.managed_fish_adapter_is_ready_for_pane(pane_id)
        {
            return Ok(());
        }
        if self.shell_execution_identity_for_pane(pane_id).is_err()
            && self
                .process
                .pane_shell_interaction_generations
                .contains_key(pane_id)
        {
            self.process.pane_certified_shell_identities.remove(pane_id);
            self.process.pane_probed_shell_identities.remove(pane_id);
            return self.dispatch_shell_identity_probe_to_pane(pane_id);
        }
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

    /// Settles a syntax-neutral identity probe and starts dialect-specific bootstrap.
    pub(crate) fn observe_shell_identity_probe_transaction_end(
        &mut self,
        marker: &str,
        exit_code: i32,
        transaction: &RunningShellTransactionRef,
    ) -> Result<usize> {
        let (primary_process_id, interaction_generation) = match &transaction.kind {
            RunningShellTransactionKind::ShellIdentityProbe {
                primary_process_id,
                interaction_generation,
            } => (*primary_process_id, *interaction_generation),
            _ => {
                return Err(MezError::invalid_state(
                    "shell identity probe completion received another transaction kind",
                ));
            }
        };
        let pane_id = transaction.pane_id.as_str();
        let observed_output_preview = transaction.observed_output_preview.as_str();
        let observed_output_truncated = transaction.observed_output_truncated;
        let current_process_id = self.primary_pid_for_live_pane_process(pane_id);
        let current_generation = self
            .process
            .pane_shell_interaction_generations
            .get(pane_id)
            .copied();
        if current_process_id != Some(primary_process_id)
            || current_generation != Some(interaction_generation)
        {
            self.process.pane_certified_shell_identities.remove(pane_id);
            self.process.pane_probed_shell_identities.remove(pane_id);
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","shell_identity_probe":"stale","marker":"{}"}}"#,
                    json_escape(pane_id),
                    json_escape(marker)
                ),
            )?;
            self.dispatch_bootstrap_to_pane(pane_id)?;
            return Ok(1);
        }

        let probe = if exit_code == 0 && !observed_output_truncated {
            parse_shell_identity_probe_output(observed_output_preview, marker)?
        } else {
            None
        };
        let Some(probe) = probe else {
            self.process.pane_probed_shell_identities.remove(pane_id);
            self.process.pane_bootstrap_pending.remove(pane_id);
            self.clear_agent_subshell_shell_identity(pane_id);
            self.mark_pane_environment_authority_unavailable(
                pane_id,
                RuntimePaneEnvironmentAuthorityUnavailableReason::ShellIdentityProbeFailed,
            );
            self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","shell_identity_probe":"failed","marker":"{}","exit_code":{},"output_truncated":{}}}"#,
                    json_escape(pane_id),
                    json_escape(marker),
                    exit_code,
                    observed_output_truncated
                ),
            )?;
            return Ok(1);
        };

        let execution_identity = RuntimePaneShellExecutionIdentity {
            shell_path: PathBuf::from(probe.shell_path),
            classification: probe.shell_classification,
            version_probe: probe.shell_version,
            primary_process_id: Some(primary_process_id),
            interaction_generation: Some(interaction_generation),
        };
        self.process.pane_probed_shell_identities.insert(
            pane_id.to_string(),
            RuntimePaneProbedShellIdentity {
                primary_process_id,
                interaction_generation,
                execution_identity,
            },
        );
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","shell_identity_probe":"completed","marker":"{}"}}"#,
                json_escape(pane_id),
                json_escape(marker)
            ),
        )?;
        if self
            .process
            .pane_foreign_shell_boundaries
            .contains_key(pane_id)
        {
            self.begin_dependency_free_foreign_child_bootstrap(pane_id)?;
            return Ok(1);
        }
        self.dispatch_bootstrap_to_pane(pane_id)?;
        Ok(1)
    }

    /// Launches an ephemeral managed child through a dependency-free `/bin/sh` loader.
    fn begin_dependency_free_foreign_child_bootstrap(&mut self, pane_id: &str) -> Result<()> {
        let boundary = self
            .process
            .pane_foreign_shell_boundaries
            .get(pane_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("foreign shell boundary is unavailable"))?;
        if boundary.phase != RuntimeForeignShellBootstrapPhase::IdentityProbing {
            return Err(MezError::invalid_state(
                "dependency-free child launch does not own foreign identity discovery",
            ));
        }
        let execution_identity = self
            .process
            .pane_probed_shell_identities
            .get(pane_id)
            .filter(|identity| {
                identity.primary_process_id == boundary.primary_process_id
                    && identity.interaction_generation == boundary.interaction_generation
            })
            .map(|identity| identity.execution_identity.clone())
            .ok_or_else(|| MezError::invalid_state("foreign shell identity is unavailable"))?;
        let child_shell = match execution_identity.classification() {
            ShellClassification::Bash => Some(mez_terminal::ManagedShellAdapter::Bash),
            ShellClassification::Fish => Some(mez_terminal::ManagedShellAdapter::Fish),
            ShellClassification::Zsh => Some(mez_terminal::ManagedShellAdapter::Zsh),
            ShellClassification::PosixSh | ShellClassification::UnknownUnix => None,
        };
        let child_token = runtime_random_marker_token(&format!(
            "dependency-free-foreign-child\0{pane_id}\0{}",
            boundary.interaction_generation
        ))?;
        if let Some(current) = self.process.pane_foreign_shell_boundaries.get_mut(pane_id) {
            current.phase = RuntimeForeignShellBootstrapPhase::BootstrappingChild;
            current.phase_started_at_unix_ms = current_unix_millis();
            current.child_token = Some(child_token.as_str().to_string());
            current.child_shell = child_shell;
        }
        self.process.pane_shell_handoffs.insert(
            pane_id.to_string(),
            RuntimePaneShellHandoff {
                primary_process_id: boundary.primary_process_id,
                interaction_generation: boundary.interaction_generation,
                bootstrap_marker: None,
                deferred_bootstrap_wrapper: None,
            },
        );
        let (marker, wrapper) = self
            .prepare_bootstrap_to_pane(pane_id)?
            .ok_or_else(|| MezError::invalid_state("foreign bootstrap is already running"))?;
        self.bind_agent_subshell_bootstrap_marker(pane_id, &marker);
        self.defer_agent_subshell_bootstrap_wrapper(pane_id, &marker, wrapper);
        let exit_marker = runtime_random_marker_token(&format!(
            "dependency-free-foreign-child-exit\0{pane_id}\0{marker}"
        ))?;
        let staging_source = match execution_identity.classification() {
            ShellClassification::Bash => {
                super::super::bash_compat::managed_dependency_free_foreign_bash_child_staging_source(
                    execution_identity.shell_path(),
                    &marker,
                    &child_token,
                    &exit_marker,
                )
            }
            ShellClassification::Zsh => {
                super::super::zsh_compat::managed_foreign_zsh_child_staging_source(
                    execution_identity.shell_path(),
                    &marker,
                    &child_token,
                    mez_agent::ManagedZshTrigger::EscapeM,
                    &exit_marker,
                )?
            }
            classification => {
                agent_subshell_enter_command_with_shell_compatibility_and_exit_marker(
                    execution_identity.shell_path(),
                    classification,
                    None,
                    None,
                    None,
                    None,
                    (classification == ShellClassification::Fish)
                        .then_some((&child_token, marker.as_str())),
                    None,
                    Some(&exit_marker),
                )
                .map_err(|error| MezError::invalid_state(error.to_string()))?
            }
        };
        if let Some(shell) = child_shell {
            let managed_shell = match shell {
                mez_terminal::ManagedShellAdapter::Bash => super::super::ManagedShellKind::Bash,
                mez_terminal::ManagedShellAdapter::Fish => super::super::ManagedShellKind::Fish,
                mez_terminal::ManagedShellAdapter::Zsh => super::super::ManagedShellKind::Zsh,
            };
            self.register_managed_shell_handoff(&marker, managed_shell, None);
        }
        let queued_loader_marker = boundary.loader_marker.clone();
        let loader_marker = if let Some(loader_marker) = queued_loader_marker.as_deref() {
            loader_marker.to_string()
        } else {
            runtime_random_marker_token(&format!(
                "dependency-free-foreign-loader\0{pane_id}\0{marker}"
            ))?
            .as_str()
            .get(..32)
            .ok_or_else(|| MezError::invalid_state("foreign loader nonce is too short"))?
            .to_string()
        };
        let loader_input = mez_agent::dependency_free_foreign_shell_loader_input(
            &staging_source,
            execution_identity.shell_path(),
            execution_identity.classification(),
            child_shell.map(|_| &child_token),
            &loader_marker,
        )
        .map_err(|error| MezError::invalid_state(error.to_string()))?;
        if let Some(current) = self.process.pane_foreign_shell_boundaries.get_mut(pane_id) {
            current.child_staging_source = Some(staging_source);
            current.loader_marker = Some(loader_marker.clone());
            current.loader_payload =
                Some(mez_mux::process::ShellInputDelivery::loader_acknowledged(
                    loader_input.payload.into_bytes(),
                    marker.clone(),
                ));
            current.loader_ready = false;
        }
        self.remember_agent_subshell_exit_marker(
            pane_id,
            agent_subshell_exit_marker_bytes(&exit_marker),
        );
        if queued_loader_marker.is_none()
            && let Err(error) =
                self.write_runtime_pane_shell_input(pane_id, loader_input.command.as_bytes())
        {
            self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
            return Err(error);
        }
        if child_shell.is_none() {
            self.enter_agent_subshell(pane_id);
            self.take_agent_subshell_command_exit(pane_id);
            self.remember_hidden_shell_render_suppression(pane_id);
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","foreign_bootstrap":"loading_child","transport":"dependency-free","marker":"{}"}}"#,
                json_escape(pane_id),
                json_escape(&marker)
            ),
        )?;
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

            let resolved_shell_path = self
                .process
                .pane_foreign_shell_boundaries
                .get(pane_id)
                .filter(|boundary| {
                    boundary.phase == RuntimeForeignShellBootstrapPhase::BootstrappingChild
                })
                .and_then(|boundary| {
                    self.process
                        .pane_probed_shell_identities
                        .get(pane_id)
                        .filter(|identity| {
                            identity.primary_process_id == boundary.primary_process_id
                                && identity.interaction_generation
                                    == boundary.interaction_generation
                        })
                        .map(|identity| identity.execution_identity.shell_path().to_path_buf())
                })
                .map(Ok)
                .unwrap_or_else(|| {
                    self.shell_execution_identity_for_pane(pane_id)
                        .map(|identity| identity.shell_path().to_path_buf())
                })?;
            let (signature, inventory, instruction_files) =
                parse_bootstrap_env_output(&all_output, &resolved_shell_path);

            if let Some(sig) = signature {
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
                    self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
                } else {
                    let reason = if observed_output_truncated {
                        RuntimePaneEnvironmentAuthorityUnavailableReason::BootstrapOutputTruncated
                    } else if exit_code == 0 {
                        RuntimePaneEnvironmentAuthorityUnavailableReason::EnvironmentSignatureMissing
                    } else {
                        RuntimePaneEnvironmentAuthorityUnavailableReason::BootstrapTransactionFailed
                    };
                    self.mark_pane_environment_authority_unavailable(pane_id, reason);
                    self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
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
        if self.agent_subshell_entry_is_deferred(pane_id) {
            if !self.agent_subshell_is_active(pane_id)
                && self.pane_readiness_state(pane_id) == PaneReadinessState::Ready
                && self
                    .agent_shell_store()
                    .get(pane_id)
                    .is_some_and(|session| session.visibility == AgentShellVisibility::Visible)
            {
                // A managed-Bash bootstrap settles from inside its private
                // `bind -x` callback. Sending another receiver trigger from
                // that same completion stack can place the control byte in
                // Readline's callback teardown instead of its next input
                // cycle. Keep the entry deferred until the restored parent
                // publishes the prompt that follows callback completion.
                if self.bash_receiver_token_for_pane(pane_id).is_none() {
                    let _ = self.enter_agent_subshell_if_needed(pane_id)?;
                }
            } else if !self.pane_bootstrap_is_pending(pane_id) {
                self.clear_deferred_agent_subshell_entry(pane_id);
            }
        }
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
                let awaits_managed_receiver = has_deferred_wrapper
                    && matches!(
                        self.shell_classification_for_pane(k.as_str()),
                        mez_agent::ShellClassification::Bash
                            | mez_agent::ShellClassification::Fish
                            | mez_agent::ShellClassification::Zsh
                    );
                let awaits_dependency_free_posix_child = self
                    .process
                    .pane_foreign_shell_boundaries
                    .get(k.as_str())
                    .is_some_and(|boundary| {
                        boundary.phase == RuntimeForeignShellBootstrapPhase::BootstrappingChild
                            && boundary.child_shell.is_none()
                    });
                let managed_fish_is_ready = !self
                    .process
                    .pane_fish_compatibility
                    .contains_key(k.as_str())
                    || self.managed_fish_adapter_is_ready_for_pane(k.as_str());
                self.process.pane_bootstrap_pending.contains(k.as_str())
                    && (!self.pane_has_uncertified_foreign_shell_boundary(k.as_str())
                        || awaits_dependency_free_posix_child)
                    && !self.pane_agent_subshell_certification_is_pending(k.as_str())
                    && !awaits_managed_receiver
                    && managed_fish_is_ready
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
