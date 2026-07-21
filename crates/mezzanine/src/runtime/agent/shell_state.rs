//! Runtime agent shell dispatch and readiness helpers.
//!
//! This module owns pane readiness state, shell action transaction dispatch,
//! scoped path/permission helpers, and provider-continuation wakeups tied to
//! shell readiness. It is shared by action execution, process observation, and
//! command/control paths.

use super::{
    AgentAction, AgentActionPayload, AgentTurnRecord, AgentTurnState, ApplyPatchTransactionPhase,
    MezError, PaneReadinessState, PathScopes, PermissionPolicy, ReadinessOverrideRevocation,
    Result, RunningShellTransactionKind, RunningShellTransactionRef, RuntimeSessionService,
    ShellTransaction, ShellTransactionOutputTransport, SubagentScopeDeclaration,
    apply_patch_transaction_phase, current_unix_millis, runtime_agent_shell_status,
    runtime_agent_terminal_preview, runtime_execution_ready_for_provider_continuation,
    runtime_marker_for_action, runtime_pane_readiness_state_name,
};
use crate::runtime::SandboxConfig;
use mez_agent::permissions::{EffectCompleteness, PermissionEvaluation};
use mez_agent::{ShellChildArgument, ShellChildLaunch};

/// Builds the exact resolver request needed for complete per-action filesystem
/// effects and protected descendants of deterministic user-home authority.
fn bubblewrap_action_path_resolution_request(
    maximum: &PathScopes,
    evaluation: &PermissionEvaluation,
) -> Result<Option<mez_agent::shell::PanePathResolutionRequest>> {
    let mut additional_paths =
        crate::security::sandbox::bubblewrap_protected_path_resolution_candidates(maximum)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
    if evaluation.completeness == EffectCompleteness::Complete {
        additional_paths.extend(
            evaluation
                .effects
                .reads
                .iter()
                .chain(&evaluation.effects.writes)
                .chain(&evaluation.effects.creates)
                .chain(&evaluation.effects.deletes)
                .chain(&evaluation.effects.touches)
                .cloned(),
        );
    }
    if additional_paths.is_empty() {
        return Ok(None);
    }
    mez_agent::shell::PanePathResolutionRequest::new(
        maximum.read_scopes.clone(),
        maximum.write_scopes.clone(),
        additional_paths.into_iter().collect(),
    )
    .map(Some)
    .map_err(|error| MezError::invalid_args(error.message()))
}

/// Per-action inputs required to render and track one pane shell transaction.
pub(super) struct ShellActionDispatch<'a> {
    /// Original command retained for execution, preview, and audit identity.
    pub(super) command: &'a str,
    /// Whether the command intentionally mutates the persistent pane shell.
    pub(super) stateful: bool,
    /// Whether the command requires interactive terminal behavior.
    pub(super) interactive: bool,
    /// Optional action-specific execution timeout.
    pub(super) timeout_ms: Option<u64>,
    /// Structured authorization result retained for sandbox compilation.
    pub(super) permission_evaluation: Option<&'a PermissionEvaluation>,
}

impl RuntimeSessionService {
    /// Runs the dispatch shell action to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_shell_action_to_pane(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        dispatch: ShellActionDispatch<'_>,
    ) -> Result<()> {
        let ShellActionDispatch {
            command,
            stateful,
            interactive,
            timeout_ms,
            permission_evaluation,
        } = dispatch;
        self.require_pane_ready_for_agent_command(&turn.pane_id)?;
        let previous_readiness = self.pane_readiness_state(&turn.pane_id);
        let marker = runtime_marker_for_action(turn, &action.id)?;
        let marker_id = marker.as_str().to_string();
        let mut transaction = ShellTransaction::new(
            marker,
            &turn.turn_id,
            &turn.agent_id,
            &turn.pane_id,
            self.session.shell.path(),
            command,
        )?;
        if let SandboxConfig::Bubblewrap(config) = self.configured_permissions().sandbox.clone() {
            let evaluation = permission_evaluation.ok_or_else(|| {
                MezError::invalid_state(
                    "Bubblewrap dispatch requires the retained structured permission evaluation",
                )
            })?;
            let signature = self
                .pane_environment_signature(&turn.pane_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "pane environment is unavailable for Bubblewrap dispatch",
                    )
                })?;
            let probe_plan = crate::security::sandbox::bubblewrap_capability_probe_plan(
                &config,
                &signature.shell_path,
            )
            .map_err(|error| MezError::invalid_state(error.message()))?;
            let cache_key = crate::security::sandbox::bubblewrap_capability_cache_key(
                &signature.stable_hash(),
                &probe_plan,
            )
            .map_err(|error| MezError::invalid_state(error.message()))?;
            let capability = self.bubblewrap_capability(&cache_key).ok_or_else(|| {
                MezError::invalid_state(
                    "Bubblewrap capability is unavailable for the active pane environment",
                )
            })?;
            let maximum_authority = self.bubblewrap_path_scopes_for_turn(turn, evaluation)?;
            let launch_plan = crate::security::sandbox::compile_bubblewrap_launch_plan(
                crate::security::sandbox::BubblewrapCompileRequest {
                    config: &config,
                    capability,
                    pane_environment_signature: &cache_key.pane_environment_signature,
                    network_policy: self.configured_permissions().resources.network_policy,
                    maximum_authority: &maximum_authority,
                    permission_evaluation: evaluation,
                    child_shell_path: &signature.shell_path,
                    command_file_host_path:
                        crate::security::sandbox::BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER,
                    stateful,
                    interactive,
                },
            )
            .map_err(|error| MezError::invalid_state(error.message()))?;
            let arguments = launch_plan
                .arguments
                .into_iter()
                .map(|argument| {
                    if argument
                        == crate::security::sandbox::BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER
                    {
                        ShellChildArgument::MaterializedCommandFile
                    } else {
                        ShellChildArgument::Literal(argument)
                    }
                })
                .collect();
            let child_launch = ShellChildLaunch::new(launch_plan.executable, arguments)?;
            transaction = transaction.with_child_launch(child_launch);
        }
        let transaction = transaction.with_output_transport(if stateful {
            ShellTransactionOutputTransport::Raw
        } else {
            ShellTransactionOutputTransport::Base64
        });
        let classification = self.shell_classification_for_pane(&turn.pane_id);
        let transaction_input = if stateful {
            None
        } else {
            Some(transaction.render_for_classification_input(classification))
        };
        let mut wrapper = if stateful {
            transaction.render_stateful_for_classification(classification)
        } else {
            transaction_input
                .as_ref()
                .expect("non-stateful transactions render streamed input")
                .wrapper
                .clone()
        };
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        let payload_len = transaction_input
            .as_ref()
            .map(|input| input.payload.len())
            .unwrap_or_default();
        let is_internal_apply_patch_write_phase =
            matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
                && apply_patch_transaction_phase(command)
                    == Some(ApplyPatchTransactionPhase::Write);
        let emitted_action_log = if is_internal_apply_patch_write_phase {
            false
        } else {
            self.append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, action)?
        };
        let is_model_shell_command =
            matches!(action.payload, AgentActionPayload::ShellCommand { .. });
        let should_emit_fallback_action_status = (self.agent_verbose_enabled(&turn.pane_id)
            || !is_model_shell_command)
            && !is_internal_apply_patch_write_phase
            && !emitted_action_log;
        if should_emit_fallback_action_status {
            let emitted_thinking =
                self.append_agent_action_model_thinking_to_terminal_buffer(&turn.pane_id, action)?;
            if !emitted_thinking {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &runtime_agent_shell_status(action, "shell command"),
                )?;
            }
        }
        if is_model_shell_command
            || (!is_internal_apply_patch_write_phase && !emitted_action_log)
            || self.agent_verbose_enabled(&turn.pane_id)
        {
            self.append_agent_command_preview_to_terminal_buffer(&turn.pane_id, command)?;
        }
        self.remember_mez_wrapper_filter_command(&turn.pane_id, command);
        let wrapper_bytes = wrapper.len().saturating_add(payload_len);
        self.write_runtime_pane_input(&turn.pane_id, wrapper.as_bytes())?;
        self.revoke_pane_readiness_override(
            &turn.pane_id,
            ReadinessOverrideRevocation::HarnessOwnedCommand,
        );
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Busy);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "pane_readiness {} -> busy reason=shell_dispatch action={} marker={}",
                runtime_pane_readiness_state_name(previous_readiness),
                action.id,
                marker_id
            ),
        )?;
        self.append_agent_shell_command_audit(
            turn,
            action,
            command,
            permission_evaluation,
            "sent",
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "pane_input accepted bytes={} action={} marker={}",
                wrapper_bytes, action.id, marker_id
            ),
        )?;
        self.register_running_shell_transaction(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn.turn_id.clone(),
                kind: RunningShellTransactionKind::AgentAction {
                    action_id: action.id.clone(),
                },
                pane_id: turn.pane_id.clone(),
                command: command.to_string(),
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(mez_agent::agent_shell_timeout_ms(
                    turn.started_at_unix_seconds,
                    current_unix_millis(),
                    timeout_ms,
                )),
                pending_input_payload: transaction_input.and_then(|input| {
                    (!input.payload.is_empty()).then(|| input.payload.into_bytes())
                }),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
            true,
        );
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "shell_transaction inserted marker={} action={} command={}",
                marker_id,
                action.id,
                runtime_agent_terminal_preview(command)
            ),
        )?;
        Ok(())
    }

    /// Ensures complete filesystem effects have exact pane-shell path evidence
    /// before Bubblewrap capability probing or workload compilation begins.
    pub(crate) fn ensure_bubblewrap_path_resolution_for_action(
        &mut self,
        turn: &AgentTurnRecord,
        action_id: &str,
        evaluation: Option<&PermissionEvaluation>,
    ) -> Result<bool> {
        if !matches!(
            self.configured_permissions().sandbox,
            SandboxConfig::Bubblewrap(_)
        ) {
            return Ok(true);
        }
        let evaluation = evaluation.ok_or_else(|| {
            MezError::invalid_state(
                "Bubblewrap path resolution requires the retained permission evaluation",
            )
        })?;
        let maximum = self.bubblewrap_maximum_path_scopes_for_turn(turn)?;
        let Some(request) = bubblewrap_action_path_resolution_request(&maximum, evaluation)? else {
            return Ok(true);
        };
        if self
            .path_scopes_for_pane_request(&turn.pane_id, &request)?
            .is_some()
        {
            return Ok(true);
        }
        let _ = self.dispatch_action_path_resolution_to_pane(turn, action_id, request)?;
        Ok(false)
    }

    /// Returns the exact pane-resolved authority and effect evidence used to
    /// compile one Bubblewrap action.
    fn bubblewrap_path_scopes_for_turn(
        &self,
        turn: &AgentTurnRecord,
        evaluation: &PermissionEvaluation,
    ) -> Result<PathScopes> {
        let maximum = self.bubblewrap_maximum_path_scopes_for_turn(turn)?;
        let Some(request) = bubblewrap_action_path_resolution_request(&maximum, evaluation)? else {
            return Ok(maximum);
        };
        self.path_scopes_for_pane_request(&turn.pane_id, &request)?
            .ok_or_else(|| {
                MezError::invalid_state(
                    "Bubblewrap dispatch requires resolved action filesystem effects",
                )
            })
    }

    /// Returns the exact pane-resolved maximum authority for one turn.
    fn bubblewrap_maximum_path_scopes_for_turn(
        &self,
        turn: &AgentTurnRecord,
    ) -> Result<PathScopes> {
        let primary = self.path_scopes_for_pane(&turn.pane_id).ok_or_else(|| {
            MezError::invalid_state("Bubblewrap dispatch requires resolved primary path authority")
        })?;
        let Some(scope) = self.subagent_scope_declaration_for_turn(turn) else {
            return Ok(primary);
        };
        if scope.read_scopes.is_empty() && scope.write_scopes.is_empty() {
            return Ok(primary);
        }
        let request = mez_agent::shell::PanePathResolutionRequest::new(
            scope.read_scopes,
            scope.write_scopes,
            Vec::new(),
        )
        .map_err(|error| MezError::invalid_args(error.message()))?;
        let child = self
            .path_scopes_for_pane_request(&turn.pane_id, &request)?
            .ok_or_else(|| {
                MezError::invalid_state(
                    "Bubblewrap dispatch requires resolved subagent path authority",
                )
            })?;
        primary
            .intersection(&child)
            .map_err(|error| MezError::invalid_state(error.message()))
    }

    /// Runs the require pane ready for agent command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn require_pane_ready_for_agent_command(&self, pane_id: &str) -> Result<()> {
        match self.pane_readiness_state(pane_id) {
            PaneReadinessState::Ready => Ok(()),
            state => Err(MezError::conflict(format!(
                "pane {pane_id} is not ready for agent shell input: {}",
                runtime_pane_readiness_state_name(state)
            ))),
        }
    }

    /// Builds the best-available `PathScopes` for a pane.
    ///
    /// Configured primary authority is returned only after the exact request was
    /// resolved in the pane environment. Configurations without primary scopes
    /// retain an explicit unresolved state rather than inferring authority from
    /// the working directory.
    pub(crate) fn path_scopes_for_pane(&self, pane_id: &str) -> Option<PathScopes> {
        let resources = &self.configured_permissions().resources;
        if !resources.read_scopes.is_empty() || !resources.write_scopes.is_empty() {
            let request = mez_agent::shell::PanePathResolutionRequest::new(
                resources.read_scopes.clone(),
                resources.write_scopes.clone(),
                Vec::new(),
            )
            .ok()?;
            return self
                .path_scopes_for_pane_request(pane_id, &request)
                .ok()
                .flatten();
        }
        let signature = self.pane_environment_signature(pane_id)?;
        Some(PathScopes::unresolved(
            signature.working_directory.clone(),
            Vec::new(),
            Vec::new(),
        ))
    }

    /// Reports whether a running shell transaction should display a transient
    /// latest-output line in the pane while its output is otherwise hidden.
    pub(crate) fn agent_shell_transaction_action_shows_live_output(
        &self,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        self.agent_turn_executions()
            .get(turn_id)
            .and_then(|execution| execution.response.action_batch.as_ref())
            .and_then(|batch| batch.actions.iter().find(|action| action.id == action_id))
            .is_some_and(|action| matches!(action.payload, AgentActionPayload::ShellCommand { .. }))
    }

    /// Runs the subagent scope declaration for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn subagent_scope_declaration_for_turn(
        &self,
        turn: &AgentTurnRecord,
    ) -> Option<SubagentScopeDeclaration> {
        let mut declaration = self.subagent_scope_declaration(&turn.agent_id)?;
        if let Some(signature) = self.pane_environment_signature(&turn.pane_id) {
            declaration.current_directory = signature.working_directory.clone();
        }
        Some(declaration)
    }

    /// Runs the permission policy for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn permission_policy_for_turn(&self, turn: &AgentTurnRecord) -> PermissionPolicy {
        let mut policy = self.permission_policy().clone();
        if let Some(preset) = self
            .subagent_scope_declaration_for_turn(turn)
            .and_then(|declaration| declaration.permission_preset)
        {
            policy.preset = preset;
        }
        policy
    }

    /// Queues provider continuation for the running turn in a pane when its
    /// stored execution has no running or blocked action results left.
    ///
    /// Readiness probes already call this continuation path when the probe
    /// completes. Manual readiness overrides use this helper so an operator
    /// can unblock a turn waiting for readiness without waiting for a pending
    /// probe marker to finish.
    pub(crate) fn queue_ready_provider_continuation_for_pane(&mut self, pane_id: &str) -> usize {
        if self.pane_readiness_state(pane_id) != PaneReadinessState::Ready
            || self.pane_readiness_override_has_pending_probe(pane_id)
        {
            return 0;
        }
        let Some(turn_id) = self
            .agent_shell_store()
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
        else {
            return 0;
        };
        if self.agent.pending_agent_provider_tasks.contains(turn_id)
            || self
                .agent
                .claimed_agent_provider_tasks
                .contains_key(turn_id)
        {
            return 0;
        }
        let turn_is_running = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running);
        if !turn_is_running {
            return 0;
        }
        let Some(execution) = self.agent_turn_executions().get(turn_id) else {
            return 0;
        };
        if !runtime_execution_ready_for_provider_continuation(execution)
            && !self.execution_has_pending_shell_dispatch(turn_id, execution)
        {
            return 0;
        }
        self.agent
            .pending_agent_provider_tasks
            .insert(turn_id.to_string());
        1
    }
}
