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
use crate::runtime::{RUNTIME_APPLY_PATCH_SNAPSHOT_OBSERVATION_LIMIT_BYTES, SandboxConfig};
use crate::security::project::TrustDecision;
use mez_agent::permissions::{EffectCompleteness, PermissionEvaluation};
use mez_agent::{SHELL_OUTPUT_BASE64_MAX_RAW_BYTES, ShellChildArgument, ShellChildLaunch};
use std::path::PathBuf;

/// Effective primary filesystem authority and its user-visible provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePrimaryPathScopeStatus {
    /// Effective read authority before pane-shell canonicalization.
    pub(crate) read_scopes: Vec<String>,
    /// Effective write authority before pane-shell canonicalization.
    pub(crate) write_scopes: Vec<String>,
    /// Stable provenance name: `explicit`, `trusted-project`, or `none`.
    pub(crate) provenance: &'static str,
    /// Selected trusted root when project trust supplied the authority.
    pub(crate) trusted_project_root: Option<String>,
}

/// Returns the bounded raw-output ceiling for one generated shell transaction.
///
/// Apply-patch read phases carry complete file snapshots required by Rust-side
/// planning. Ordinary actions retain the smaller model-visible output bound.
pub(crate) fn shell_transaction_output_max_raw_bytes(command: &str) -> usize {
    if apply_patch_transaction_phase(command) == Some(ApplyPatchTransactionPhase::Read) {
        RUNTIME_APPLY_PATCH_SNAPSHOT_OBSERVATION_LIMIT_BYTES
    } else {
        SHELL_OUTPUT_BASE64_MAX_RAW_BYTES
    }
}

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

/// Result of preparing and dispatching one shell-backed action.
pub(super) enum ShellActionDispatchOutcome {
    /// The pane accepted a concrete shell transaction.
    Dispatched,
    /// Bubblewrap could not represent an otherwise prompt-eligible action, so
    /// the caller may offer one exact approval-gated unsandboxed retry.
    SandboxFallbackEligible {
        /// Fresh transaction identity retained for audit and approval facts.
        marker: String,
        /// Typed, redacted preparation evidence.
        proof: String,
    },
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
    ) -> Result<ShellActionDispatchOutcome> {
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
        let mut sandbox_audit_summary = None;
        let mut managed_home_activity_lock = None;
        let permission_policy = self.permission_policy_for_turn(turn);
        let bubblewrap_applies = crate::runtime::config::bubblewrap_applies_to_policy(
            &self.configured_permissions().sandbox,
            &permission_policy,
        );
        let sandbox_bypassed = bubblewrap_applies
            && self.activate_sandbox_bypass_after_approval(&turn.turn_id, &action.id);
        if let SandboxConfig::Bubblewrap(config) = self.configured_permissions().sandbox.clone()
            && bubblewrap_applies
            && !sandbox_bypassed
        {
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
                &turn.pane_id,
                &signature.stable_hash(),
                self.session.config_generation,
                &probe_plan,
            )
            .map_err(|error| MezError::invalid_state(error.message()))?;
            let capability = self.bubblewrap_capability(&cache_key).ok_or_else(|| {
                MezError::invalid_state(
                    "Bubblewrap capability is unavailable for the active pane environment",
                )
            })?;
            let maximum_authority = self.bubblewrap_path_scopes_for_turn(turn, evaluation)?;
            let managed_home = match (
                self.integration.config_root(),
                self.trusted_project_root_for_pane(&turn.pane_id),
            ) {
                (Some(config_root), Some(project_root)) => {
                    let (home, activity_lock) =
                        crate::security::sandbox::prepare_bubblewrap_managed_home_for_workload(
                            config_root,
                            &project_root,
                        )
                        .map_err(|error| MezError::invalid_state(error.message()))?;
                    managed_home_activity_lock = Some(activity_lock);
                    Some(home)
                }
                _ => None,
            };
            let rust_toolchain = crate::security::sandbox::bubblewrap_rust_toolchain_roots(
                &config,
                &signature.environment_managers,
            )
            .map_err(|error| MezError::invalid_state(error.message()))?;
            let launch_plan = match crate::security::sandbox::compile_bubblewrap_launch_plan(
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
                    managed_home_host_path: managed_home
                        .as_ref()
                        .map(|home| home.host_path.as_path()),
                    rust_toolchain: rust_toolchain.as_ref(),
                    stateful,
                    interactive,
                },
            ) {
                Ok(launch_plan) => launch_plan,
                Err(error)
                    if evaluation.decision == mez_agent::permissions::RuleDecision::Prompt
                        && error.kind().approval_fallback_eligible() =>
                {
                    return Ok(ShellActionDispatchOutcome::SandboxFallbackEligible {
                        marker: marker_id,
                        proof: format!("{}: {}", error.kind().as_str(), error.message()),
                    });
                }
                Err(error) => return Err(MezError::invalid_state(error.message())),
            };
            sandbox_audit_summary = Some(launch_plan.audit_summary.clone());
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
            let child_launch = ShellChildLaunch::new(launch_plan.executable, arguments)?
                .with_status_fd(crate::security::sandbox::BUBBLEWRAP_STATUS_FD)?;
            transaction = transaction.with_child_launch(child_launch);
        }
        let transaction = transaction.with_output_transport(if stateful {
            ShellTransactionOutputTransport::Raw
        } else {
            ShellTransactionOutputTransport::Base64
        });
        let transaction =
            transaction.with_output_max_raw_bytes(shell_transaction_output_max_raw_bytes(command));
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
        let apply_patch_read_path =
            if matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
                && apply_patch_transaction_phase(command) == Some(ApplyPatchTransactionPhase::Read)
            {
                self.agent
                    .apply_patch_batch_states
                    .get(&Self::apply_patch_batch_state_key(
                        &turn.turn_id,
                        &action.id,
                    ))
                    .and_then(|state| state.current_path.clone())
            } else {
                None
            };
        let emitted_action_log = if is_internal_apply_patch_write_phase {
            false
        } else if let Some(path) = apply_patch_read_path {
            self.append_agent_action_execution_header_to_terminal_buffer(
                &turn.pane_id,
                action,
                &format!("apply patch: {path}"),
            )?;
            true
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
        if sandbox_audit_summary.is_some() {
            self.register_sandboxed_shell_transaction_marker(&marker_id);
        }
        if let Some(activity_lock) = managed_home_activity_lock {
            self.register_managed_home_activity_lock(&marker_id, activity_lock);
        }
        if let Err(error) = self.write_runtime_pane_input(&turn.pane_id, wrapper.as_bytes()) {
            self.remove_running_shell_transaction(&marker_id);
            self.clear_shell_transaction_protocol_state(&marker_id);
            self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Degraded);
            return Err(error);
        }
        self.append_agent_shell_command_audit(
            turn,
            action,
            command,
            permission_evaluation,
            sandbox_audit_summary.as_ref(),
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
        Ok(ShellActionDispatchOutcome::Dispatched)
    }

    /// Dispatches one ordinary shell action for focused sandbox-boundary tests.
    #[cfg(test)]
    pub(crate) fn dispatch_shell_action_to_pane_for_tests(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        command: &str,
        permission_evaluation: Option<&PermissionEvaluation>,
    ) -> Result<bool> {
        self.dispatch_shell_action_to_pane(
            turn,
            action,
            ShellActionDispatch {
                command,
                stateful: false,
                interactive: false,
                timeout_ms: None,
                permission_evaluation,
            },
        )
        .map(|outcome| matches!(outcome, ShellActionDispatchOutcome::Dispatched))
    }

    /// Ensures complete filesystem effects have exact pane-shell path evidence
    /// before Bubblewrap capability probing or workload compilation begins.
    pub(crate) fn ensure_bubblewrap_path_resolution_for_action(
        &mut self,
        turn: &AgentTurnRecord,
        action_id: &str,
        evaluation: Option<&PermissionEvaluation>,
    ) -> Result<bool> {
        let permission_policy = self.permission_policy_for_turn(turn);
        if !crate::runtime::config::bubblewrap_applies_to_policy(
            &self.configured_permissions().sandbox,
            &permission_policy,
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
            return PathScopes::try_shell_resolved(
                scope.current_directory,
                Vec::new(),
                Vec::new(),
                Default::default(),
            )
            .map_err(|error| MezError::invalid_state(error.message()));
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

    /// Returns the maximum primary authority requested for one pane.
    ///
    /// Explicit configured scopes take precedence. When no scopes are
    /// configured, a pane inside one or more trusted projects receives
    /// read-write authority for the deepest matching project root only.
    pub(crate) fn primary_path_scope_paths(&self, pane_id: &str) -> (Vec<String>, Vec<String>) {
        let status = self.primary_path_scope_status(pane_id);
        (status.read_scopes, status.write_scopes)
    }

    /// Returns effective primary authority together with its stable provenance.
    pub(crate) fn primary_path_scope_status(&self, pane_id: &str) -> RuntimePrimaryPathScopeStatus {
        let resources = &self.configured_permissions().resources;
        if !resources.read_scopes.is_empty() || !resources.write_scopes.is_empty() {
            return RuntimePrimaryPathScopeStatus {
                read_scopes: resources.read_scopes.clone(),
                write_scopes: resources.write_scopes.clone(),
                provenance: "explicit",
                trusted_project_root: None,
            };
        }
        let Some(project_root) = self.trusted_project_root_for_pane(pane_id) else {
            return RuntimePrimaryPathScopeStatus {
                read_scopes: Vec::new(),
                write_scopes: Vec::new(),
                provenance: "none",
                trusted_project_root: None,
            };
        };
        let project_root = project_root.to_string_lossy().into_owned();
        RuntimePrimaryPathScopeStatus {
            read_scopes: vec![project_root.clone()],
            write_scopes: vec![project_root.clone()],
            provenance: "trusted-project",
            trusted_project_root: Some(project_root),
        }
    }

    /// Returns the deepest trusted project containing the pane directory.
    pub(crate) fn trusted_project_root_for_pane(&self, pane_id: &str) -> Option<PathBuf> {
        let working_directory = self.pane_current_working_directory(pane_id)?;
        self.integration.project_trust_store().and_then(|store| {
            store
                .records()
                .filter(|record| record.state == TrustDecision::Trusted)
                .filter(|record| {
                    crate::runtime::runtime_path_under_project_root(
                        &working_directory,
                        &record.project_root,
                    )
                })
                .max_by_key(|record| record.project_root.components().count())
                .map(|record| record.project_root.clone())
        })
    }

    /// Builds the best-available `PathScopes` for a pane.
    ///
    /// Configured primary authority is returned only after the exact request was
    /// resolved in the pane environment. When no configured authority exists,
    /// a trusted project root is used as the bounded primary authority.
    pub(crate) fn path_scopes_for_pane(&self, pane_id: &str) -> Option<PathScopes> {
        let (read_scopes, write_scopes) = self.primary_path_scope_paths(pane_id);
        if !read_scopes.is_empty() || !write_scopes.is_empty() {
            let request = mez_agent::shell::PanePathResolutionRequest::new(
                read_scopes,
                write_scopes,
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
