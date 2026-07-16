//! Runtime agent configuration-change execution helpers.
//!
//! This module owns the MAAP `config_change` execution path after a model
//! action has cleared approval planning. It converts model-authored operations
//! into validated runtime control requests or batched persisted mutations while
//! the parent runtime agent module continues to own turn lifecycle state.

use super::*;
use crate::runtime::fs;

/// Runs the runtime config change control request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_config_change_control_request(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    setting_path: &str,
    operation: &str,
    value: Option<&str>,
    persist_target_json: &str,
    idempotency_suffix: &str,
) -> Result<String> {
    match mez_agent::normalize_config_change_operation(operation)? {
        mez_agent::ConfigChangeOperation::Set => {
            let value = mez_agent::parse_config_change_value(value)?.canonical_json();
            let idempotency_key = runtime_config_change_idempotency_key(
                turn,
                action,
                RuntimeConfigChangeIdempotency {
                    method: "config/set",
                    setting_path,
                    operation,
                    value_json: Some(value.as_str()),
                    persist_target_json,
                    suffix: idempotency_suffix,
                },
            );
            Ok(format!(
                r#"{{"jsonrpc":"2.0","id":"agent-config-change","method":"config/set","params":{{"path":"{}","value":{},"persist":{},"idempotency_key":"{}"}}}}"#,
                json_escape(setting_path),
                value,
                persist_target_json,
                json_escape(&idempotency_key)
            ))
        }
        mez_agent::ConfigChangeOperation::Unset => {
            let idempotency_key = runtime_config_change_idempotency_key(
                turn,
                action,
                RuntimeConfigChangeIdempotency {
                    method: "config/unset",
                    setting_path,
                    operation,
                    value_json: None,
                    persist_target_json,
                    suffix: idempotency_suffix,
                },
            );
            Ok(format!(
                r#"{{"jsonrpc":"2.0","id":"agent-config-change","method":"config/unset","params":{{"path":"{}","persist":{},"idempotency_key":"{}"}}}}"#,
                json_escape(setting_path),
                persist_target_json,
                json_escape(&idempotency_key)
            ))
        }
    }
}

/// Returns the setting path for one config-change action.
fn runtime_config_change_setting_path(action: &AgentAction) -> Option<&str> {
    match &action.payload {
        AgentActionPayload::ConfigChange { setting_path, .. } => Some(setting_path.as_str()),
        _ => None,
    }
}

/// Returns the operation name for one config-change action.
fn runtime_config_change_operation_name(action: &AgentAction) -> &str {
    match &action.payload {
        AgentActionPayload::ConfigChange { operation, .. } => operation.as_str(),
        _ => "unknown",
    }
}

/// Reports whether a config change can be folded into a theme scalar batch.
fn runtime_config_change_action_is_theme_scalar_batchable(action: &AgentAction) -> bool {
    let AgentActionPayload::ConfigChange {
        setting_path,
        operation,
        ..
    } = &action.payload
    else {
        return false;
    };
    mez_agent::normalize_config_change_operation(operation).is_ok()
        && (setting_path.starts_with("theme.aliases.") || setting_path.starts_with("theme.colors."))
}

/// Returns the model-facing approval state for an accepted config change.
fn runtime_config_change_approval_state(
    permission_policy: &PermissionPolicy,
    action: &AgentAction,
) -> &'static str {
    if permission_policy.approval_bypass() {
        "bypassed"
    } else if permission_policy.approval_policy == mez_agent::ApprovalPolicy::AutoAllow
        && mez_agent::action_supports_auto_allow(action, mez_agent::ActionPlanningInput::default())
    {
        "auto_allowed"
    } else {
        "full_access"
    }
}

/// Converts a model-authored config-change action into a validated mutation.
fn runtime_config_change_mutation_from_action(action: &AgentAction) -> Result<ConfigMutation> {
    let AgentActionPayload::ConfigChange {
        setting_path,
        operation,
        value,
    } = &action.payload
    else {
        return Err(MezError::invalid_args(
            "config_change batch requires config_change actions",
        ));
    };
    let operation = match mez_agent::normalize_config_change_operation(operation)? {
        mez_agent::ConfigChangeOperation::Set => {
            ConfigMutationOperation::Set(runtime_config_change_mutation_value(value.as_deref())?)
        }
        mez_agent::ConfigChangeOperation::Unset => ConfigMutationOperation::Unset,
    };
    Ok(ConfigMutation {
        path: setting_path.clone(),
        operation,
    })
}

/// Converts one model-authored config-change value into a scalar config value.
fn runtime_config_change_mutation_value(value: Option<&str>) -> Result<ConfigMutationValue> {
    match mez_agent::parse_config_change_value(value)? {
        mez_agent::ConfigChangeValue::String(value) => Ok(ConfigMutationValue::String(value)),
        mez_agent::ConfigChangeValue::Integer(value) => Ok(ConfigMutationValue::Integer(value)),
        mez_agent::ConfigChangeValue::Boolean(value) => Ok(ConfigMutationValue::Boolean(value)),
        mez_agent::ConfigChangeValue::StringArray(values) => {
            Ok(ConfigMutationValue::StringArray(values))
        }
    }
}

/// Holds the request material used to build one config-change idempotency key.
struct RuntimeConfigChangeIdempotency<'a> {
    /// The JSON-RPC control method being requested.
    method: &'a str,
    /// The config setting path being changed.
    setting_path: &'a str,
    /// The model-requested config operation.
    operation: &'a str,
    /// The canonical JSON value for set-like operations.
    value_json: Option<&'a str>,
    /// The canonical JSON persist target.
    persist_target_json: &'a str,
    /// The per-action sequencing suffix.
    suffix: &'a str,
}

/// Builds a payload-sensitive control idempotency key for one config change.
///
/// Model-authored action ids are synthesized by Mezzanine, but recovery and
/// compatibility paths can still produce duplicate local ids for separate
/// mutations. Include a stable request fingerprint so different settings do
/// not collide in the JSON-RPC control idempotency cache.
fn runtime_config_change_idempotency_key(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    request: RuntimeConfigChangeIdempotency<'_>,
) -> String {
    let material = format!(
        "method={}\npath={}\noperation={}\nvalue={}\npersist={}",
        request.method,
        request.setting_path,
        request.operation.trim().to_ascii_lowercase(),
        request.value_json.unwrap_or("null"),
        request.persist_target_json
    );
    let digest = exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, &material);
    format!(
        "agent-config-change-{}-{}-{}-{}",
        turn.turn_id,
        action.id,
        request.suffix,
        &digest[..24]
    )
}

impl RuntimeSessionService {
    /// Applies one MAAP `config_change` action through the live configuration
    /// control path and maps the control response back into an action result.
    ///
    /// # Parameters
    /// - `turn`: The agent turn that proposed the configuration change.
    /// - `action`: The `config_change` action to apply.
    /// - `caller_client_id`: The primary client identity used for control
    ///   authorization.
    /// - `approval_state`: The structured approval state to report, such as
    ///   `approved` for a routed approval or `full_access` for policy-accepted
    ///   execution.
    pub(in crate::runtime) fn execute_config_change_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        caller_client_id: &mez_core::ids::ClientId,
        approval_state: &str,
    ) -> Result<ActionResult> {
        let AgentActionPayload::ConfigChange {
            setting_path,
            operation,
            value,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "config_change execution requires a config_change action",
            ));
        };
        if setting_path == "theme.active"
            && mez_agent::normalize_config_change_operation(operation)?.sets_value()
        {
            return self.execute_theme_config_change_action_for_turn(
                turn,
                action,
                setting_path,
                operation,
                value.as_deref(),
                approval_state,
            );
        }
        let persist_path = self.ensure_agent_config_change_persist_path()?;
        let persistent_target_json = format!(
            r#"{{"scope":"user","path":"{}"}}"#,
            json_escape(&persist_path.to_string_lossy())
        );
        match runtime_config_change_control_request(
            turn,
            action,
            setting_path,
            operation,
            value.as_deref(),
            &persistent_target_json,
            "persist",
        ) {
            Ok(persistent_request) => {
                let persistent_response =
                    self.dispatch_runtime_control_body(&persistent_request, caller_client_id);
                if persistent_response.contains(r#""error""#) {
                    let mut result = ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        "config_change_failed",
                        persistent_response.clone(),
                    )?;
                    result.structured_content_json = Some(format!(
                        r#"{{"approval":{{"state":"{}","kind":"config_change","action_id":"{}"}},"persistent_control_response":{}}}"#,
                        json_escape(approval_state),
                        json_escape(&action.id),
                        persistent_response
                    ));
                    Ok(result)
                } else {
                    Ok(ActionResult::succeeded(
                        turn,
                        action,
                        vec![format!(
                            "configuration change persisted and applied: {} {}",
                            operation, setting_path
                        )],
                        Some(format!(
                            r#"{{"approval":{{"state":"{}","kind":"config_change","action_id":"{}"}},"persistent_control_response":{}}}"#,
                            json_escape(approval_state),
                            json_escape(&action.id),
                            persistent_response
                        )),
                    ))
                }
            }
            Err(error) => Ok(ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                runtime_mezzanine_error_code(error.kind()),
                error.message().to_string(),
            )?),
        }
    }

    /// Applies a `theme.active` config change through the same runtime command
    /// path as `:set-theme`.
    ///
    /// The dedicated command materializes the selected theme aliases and color
    /// slots before persistence. A generic `config/set theme.active` request
    /// would change only the selector and could leave stale materialized colors
    /// in place.
    fn execute_theme_config_change_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        setting_path: &str,
        operation: &str,
        value: Option<&str>,
        approval_state: &str,
    ) -> Result<ActionResult> {
        let theme = match mez_agent::config_change_string_value(setting_path, value) {
            Ok(theme) => theme,
            Err(error) => {
                let error = MezError::from(error);
                return Ok(ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mezzanine_error_code(error.kind()),
                    error.message().to_string(),
                )?);
            }
        };
        let invocation = CommandInvocation {
            name: "set-theme".to_string(),
            args: vec![theme.clone()],
        };
        match runtime_set_theme_command(self, &invocation) {
            Ok(command_response) => Ok(ActionResult::succeeded(
                turn,
                action,
                vec![format!(
                    "configuration change persisted and applied: {} {}",
                    operation, setting_path
                )],
                Some(format!(
                    r#"{{"approval":{{"state":"{}","kind":"config_change","action_id":"{}"}},"runtime_command":"set-theme","theme":"{}","command_response":"{}"}}"#,
                    json_escape(approval_state),
                    json_escape(&action.id),
                    json_escape(&theme),
                    json_escape(&command_response)
                )),
            )),
            Err(error) => {
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mezzanine_error_code(error.kind()),
                    error.message().to_string(),
                )?;
                result.structured_content_json = Some(format!(
                    r#"{{"approval":{{"state":"{}","kind":"config_change","action_id":"{}"}},"runtime_command":"set-theme","theme":"{}","error":"{}"}}"#,
                    json_escape(approval_state),
                    json_escape(&action.id),
                    json_escape(&theme),
                    json_escape(error.message())
                ));
                Ok(result)
            }
        }
    }

    /// Applies a batch of theme scalar `config_change` actions with one reload.
    ///
    /// # Parameters
    /// - `turn`: The agent turn that proposed the configuration changes.
    /// - `execution`: The running execution whose action results should be
    ///   replaced with terminal results.
    /// - `actions`: Running theme scalar config-change actions and their result
    ///   indexes.
    /// - `approval_state`: The policy approval state to include in each
    ///   action-result payload.
    fn execute_batched_theme_config_change_actions_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
        actions: &[(usize, AgentAction)],
        approval_state: &str,
    ) -> Result<usize> {
        for (_, action) in actions {
            if !self.append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, action)? {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(action)
                            .unwrap_or_else(|| "config change".to_string())
                    ),
                )?;
            }
        }
        let batch_result = (|| {
            let mutations = actions
                .iter()
                .map(|(_, action)| runtime_config_change_mutation_from_action(action))
                .collect::<Result<Vec<_>>>()?;
            let persist_path = self.ensure_agent_config_change_persist_path()?;
            runtime_apply_persisted_config_mutation_batch(
                self,
                persist_path,
                &mutations,
                "agent/config_change:theme-batch",
            )
        })();
        for (index, action) in actions {
            execution.action_results[*index] = match &batch_result {
                Ok(report) => ActionResult::succeeded(
                    turn,
                    action,
                    vec![format!(
                        "configuration change persisted and applied in batch: {} {}",
                        runtime_config_change_operation_name(action),
                        runtime_config_change_setting_path(action).unwrap_or("unknown")
                    )],
                    Some(format!(
                        r#"{{"approval":{{"state":"{}","kind":"config_change","action_id":"{}"}},"persistent_batch":{{"path":"{}","changed":{},"reload_required":{},"mutation_count":{},"deferred":{}}}}}"#,
                        json_escape(approval_state),
                        json_escape(&action.id),
                        json_escape(&report.path.to_string_lossy()),
                        report.changed,
                        report.reload_required,
                        report.mutation_count,
                        report.deferred
                    )),
                ),
                Err(error) => ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mezzanine_error_code(error.kind()),
                    error.message().to_string(),
                )?,
            };
        }
        let executed = actions.len();
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| result.action_type == "config_change")
            {
                self.agent_turn_contexts_mut()
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
            self.agent
                .pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed)
    }

    /// Ensures model-authored configuration changes have a persistent user
    /// config file target before they are applied live.
    ///
    /// The model-facing `config_change` action intentionally does not let the
    /// model choose arbitrary files. The runtime selects the active primary
    /// config layer when one exists, or creates the default private config file
    /// under the configured Mezzanine config root.
    fn ensure_agent_config_change_persist_path(&mut self) -> Result<std::path::PathBuf> {
        if let Some(path) = self
            .integration
            .config_layers()
            .iter()
            .find(|layer| layer.scope == ConfigScope::Primary && layer.path.is_some())
            .and_then(|layer| layer.path.clone())
        {
            return Ok(path);
        }
        let root = self
            .integration
            .config_root()
            .map(|path| path.to_path_buf())
            .ok_or_else(|| {
            MezError::config(
                "config_change persistence requires a configured config root or primary config file",
            )
        })?;
        let path = ConfigPaths::from_root(root).ensure_default_config()?;
        let format = ConfigFormat::from_path(&path)?;
        let text = fs::read_to_string(&path)?;
        self.integration.config_layers_mut().push(ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format,
            scope: ConfigScope::Primary,
            trusted: true,
            text,
        });
        let _ = self.apply_runtime_config_layers()?;
        Ok(path)
    }

    /// Executes provider-produced `config_change` actions that were accepted by
    /// the active approval policy instead of entering blocked approval routing.
    ///
    /// Full-access mode resolves the approval prompt at action-planning time,
    /// but live configuration mutation still has to pass through the normal
    /// runtime control path so validation, events, and idempotency remain
    /// identical to approved blocked config changes.
    pub(super) fn execute_running_config_change_actions_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let Some(controller) = self.session.primary_client_id().cloned() else {
            return Ok(0);
        };
        let pending_config_actions = execution
            .action_results
            .iter()
            .enumerate()
            .filter(|(_, result)| {
                result.status == ActionStatus::Running && result.action_type == "config_change"
            })
            .map(|(index, result)| {
                batch
                    .actions
                    .iter()
                    .find(|action| action.id == result.action_id)
                    .cloned()
                    .map(|action| (index, action))
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "running config_change result does not match an action",
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        if pending_config_actions.len() > 1
            && pending_config_actions
                .iter()
                .all(|(_, action)| runtime_config_change_action_is_theme_scalar_batchable(action))
        {
            let permission_policy = self.permission_policy_for_turn(turn);
            let approval_state = pending_config_actions
                .first()
                .map(|(_, action)| runtime_config_change_approval_state(&permission_policy, action))
                .unwrap_or("full_access");
            return self.execute_batched_theme_config_change_actions_for_turn(
                turn,
                execution,
                &pending_config_actions,
                approval_state,
            );
        }
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || execution.action_results[index].action_type != "config_change"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running config_change result does not match an action")
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "config change".to_string())
                    ),
                )?;
            }
            let permission_policy = self.permission_policy_for_turn(turn);
            let approval_state = runtime_config_change_approval_state(&permission_policy, &action);
            execution.action_results[index] = self.execute_config_change_action_for_turn(
                turn,
                &action,
                &controller,
                approval_state,
            )?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| result.action_type == "config_change")
            {
                self.agent_turn_contexts_mut()
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
            self.agent
                .pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed)
    }

    /// Retries retained configuration changes once a primary client reconnects.
    ///
    /// A configuration mutation is executed through the primary-owned runtime
    /// control path. While detached, the action therefore remains running in
    /// its retained execution instead of failing the agent turn. Reattachment
    /// supplies the primary identity required to finish that work.
    pub(in crate::runtime) fn resume_detached_config_change_actions(&mut self) -> Result<()> {
        let turns = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| turn.state == AgentTurnState::Running)
            .cloned()
            .collect::<Vec<_>>();
        for turn in turns {
            let Some(mut execution) = self.agent_turn_executions().get(&turn.turn_id).cloned()
            else {
                continue;
            };
            if !execution.action_results.iter().any(|result| {
                result.status == ActionStatus::Running && result.action_type == "config_change"
            }) {
                continue;
            }
            if self.execute_running_config_change_actions_for_turn(&turn, &mut execution)? > 0 {
                self.agent_turn_executions_mut()
                    .insert(turn.turn_id.clone(), execution);
            }
        }
        Ok(())
    }
}
