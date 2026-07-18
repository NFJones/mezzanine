//! Runtime agent configuration-change execution helpers.
//!
//! This module owns the MAAP `config_change` execution path after a model
//! action has cleared approval planning. It converts model-authored operations
//! into validated runtime control requests or batched persisted mutations while
//! the parent runtime agent module continues to own turn lifecycle state.

use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnExecution,
    AgentTurnRecord, AgentTurnState, CommandInvocation, ConfigFormat, ConfigLayer, ConfigMutation,
    ConfigMutationOperation, ConfigMutationValue, ConfigPaths, ConfigScope,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, MezError, PermissionPolicy, Result,
    RuntimeSessionService, exact_command_sha256, json_escape, runtime_agent_action_summary,
    runtime_agent_turn_state_from_action_results, runtime_apply_persisted_config_mutation_batch,
    runtime_mezzanine_error_code, runtime_set_theme_command,
};
#[cfg(test)]
use super::{
    outcome::RuntimeTerminalActionObservations, runtime_execution_ready_for_provider_continuation,
};
use crate::config::{compose_effective_config, contains_secret_material};
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

/// Builds the target identity shared by semantic duplicate detection and
/// model-facing result diagnostics.
fn runtime_config_change_persistent_target(path: &std::path::Path) -> String {
    format!("user:{}", path.to_string_lossy())
}

/// Canonicalizes one model-authored configuration mutation for turn-local
/// duplicate detection.
fn runtime_config_change_signature(
    action: &AgentAction,
    persist_path: &std::path::Path,
) -> Result<mez_agent::ConfigChangeMutationSignature> {
    let AgentActionPayload::ConfigChange {
        setting_path,
        operation,
        value,
    } = &action.payload
    else {
        return Err(MezError::invalid_args(
            "config_change signature requires a config_change action",
        ));
    };
    Ok(mez_agent::ConfigChangeMutationSignature::new(
        setting_path,
        operation,
        value.as_deref(),
        runtime_config_change_persistent_target(persist_path),
    )?)
}

/// Returns a canonical requested value safe for model-visible result context.
fn runtime_config_change_requested_value_json(
    signature: &mez_agent::ConfigChangeMutationSignature,
) -> serde_json::Value {
    let Some(value_json) = signature.value_json() else {
        return serde_json::Value::Null;
    };
    if contains_secret_material(signature.setting_path(), ConfigScope::Primary) {
        return serde_json::Value::String("[redacted]".to_string());
    }
    serde_json::from_str(value_json)
        .unwrap_or_else(|_| serde_json::Value::String(value_json.to_string()))
}

/// Returns a stable model-facing mutation identity without deriving it from a
/// secret value.
fn runtime_config_change_mutation_id(
    signature: &mez_agent::ConfigChangeMutationSignature,
) -> String {
    if !contains_secret_material(signature.setting_path(), ConfigScope::Primary) {
        return signature.mutation_id();
    }
    let material = format!(
        "redacted-config-change-signature-v1\noperation={}\npath={}\ntarget={}",
        signature.operation(),
        signature.setting_path(),
        signature.persistent_target(),
    );
    let digest = exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, &material);
    format!("config-mutation-redacted-v1-{digest}")
}

/// Returns the resulting effective value and source layer after mutation.
fn runtime_config_change_effective_result_json(
    service: &RuntimeSessionService,
    signature: &mez_agent::ConfigChangeMutationSignature,
) -> Result<serde_json::Value> {
    let effective = compose_effective_config(service.integration.config_layers())?;
    let Some(value) = effective.values().get(signature.setting_path()) else {
        return Ok(serde_json::json!({
            "value": serde_json::Value::Null,
            "source_layer": serde_json::Value::Null,
        }));
    };
    let redacted = contains_secret_material(signature.setting_path(), ConfigScope::Primary);
    let source_layer = value.source_layer.clone();
    let value = if redacted {
        serde_json::Value::String("[redacted]".to_string())
    } else {
        serde_json::from_str(&value.value)
            .unwrap_or_else(|_| serde_json::Value::String(value.value.clone()))
    };
    Ok(serde_json::json!({
        "value": value,
        "source_layer": source_layer,
    }))
}

/// Builds the authoritative structured success result for one control-backed
/// configuration mutation.
fn runtime_config_change_success_json(
    service: &RuntimeSessionService,
    action: &AgentAction,
    approval_state: &str,
    signature: &mez_agent::ConfigChangeMutationSignature,
    persistent_response: &str,
) -> Result<String> {
    let persistent_response_json = serde_json::from_str::<serde_json::Value>(persistent_response)
        .unwrap_or_else(|_| serde_json::Value::String(persistent_response.to_string()));
    let changed = persistent_response_json
        .pointer("/result/plan/changed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let persistence_state = if changed {
        if service.persistence.config_uses_adapter() {
            "queued"
        } else {
            "persisted"
        }
    } else {
        "already_satisfied"
    };
    let live_state = if changed {
        "applied"
    } else {
        "already_satisfied"
    };
    Ok(serde_json::json!({
        "approval": {
            "state": approval_state,
            "kind": "config_change",
            "action_id": action.id,
        },
        "config_change": {
            "mutation_id": runtime_config_change_mutation_id(signature),
            "operation": signature.operation(),
            "path": signature.setting_path(),
            "requested_value": runtime_config_change_requested_value_json(signature),
            "value_redacted": contains_secret_material(
                signature.setting_path(),
                ConfigScope::Primary,
            ),
            "persistent_target": signature.persistent_target(),
            "validation": { "state": "succeeded" },
            "changed": changed,
            "no_op": !changed,
            "persistence": { "state": persistence_state },
            "live_application": { "state": live_state },
            "effective": runtime_config_change_effective_result_json(service, signature)?,
        },
        "persistent_control_response": persistent_response_json,
    })
    .to_string())
}

/// Builds explicit persistence/live failure state for one rejected control
/// response without claiming that either effect succeeded.
fn runtime_config_change_failure_json(
    action: &AgentAction,
    approval_state: &str,
    signature: &mez_agent::ConfigChangeMutationSignature,
    persistent_response: &str,
) -> String {
    let persistent_response_json = serde_json::from_str::<serde_json::Value>(persistent_response)
        .unwrap_or_else(|_| serde_json::Value::String(persistent_response.to_string()));
    serde_json::json!({
        "approval": {
            "state": approval_state,
            "kind": "config_change",
            "action_id": action.id,
        },
        "config_change": {
            "mutation_id": runtime_config_change_mutation_id(signature),
            "operation": signature.operation(),
            "path": signature.setting_path(),
            "requested_value": runtime_config_change_requested_value_json(signature),
            "value_redacted": contains_secret_material(
                signature.setting_path(),
                ConfigScope::Primary,
            ),
            "persistent_target": signature.persistent_target(),
            "validation": { "state": "failed" },
            "changed": false,
            "no_op": false,
            "persistence": { "state": "failed" },
            "live_application": { "state": "not_applied" },
        },
        "persistent_control_response": persistent_response_json,
    })
    .to_string()
}

/// Reports whether one result is the terminal same-turn semantic duplicate
/// guard for a configuration mutation.
fn runtime_config_change_result_is_suppressed_duplicate(result: &ActionResult) -> bool {
    result.status == ActionStatus::Succeeded
        && result
            .structured_content_json
            .as_deref()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok())
            .and_then(|content| {
                content
                    .pointer("/guard/reason")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .as_deref()
            == Some("repeated_successful_config_mutation")
}

/// Reads one boolean field from the stable colon-delimited runtime command
/// response format.
fn runtime_config_change_command_field_bool(response: &str, field: &str) -> bool {
    response.split(':').any(|segment| {
        segment
            .split_once('=')
            .is_some_and(|(name, value)| name == field && value == "true")
    })
}

impl RuntimeSessionService {
    /// Returns a successful guard result when this turn already completed the
    /// same semantic configuration mutation.
    fn repeated_config_change_result(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        approval_state: &str,
        signature: &mez_agent::ConfigChangeMutationSignature,
    ) -> Option<ActionResult> {
        let original = self
            .agent
            .agent_turn_config_change_successes
            .get(&turn.turn_id)?
            .get(&signature.canonical_material())?;
        Some(ActionResult::succeeded(
            turn,
            action,
            vec![
                "duplicate configuration mutation skipped because the same semantic mutation already succeeded this turn"
                    .to_string(),
            ],
            Some(
                serde_json::json!({
                    "approval": {
                        "state": approval_state,
                        "kind": "config_change",
                        "action_id": action.id,
                    },
                    "config_change": {
                        "mutation_id": runtime_config_change_mutation_id(signature),
                        "operation": signature.operation(),
                        "path": signature.setting_path(),
                        "requested_value": runtime_config_change_requested_value_json(signature),
                        "value_redacted": contains_secret_material(
                            signature.setting_path(),
                            ConfigScope::Primary,
                        ),
                        "persistent_target": signature.persistent_target(),
                        "validation": { "state": "succeeded_previously" },
                        "changed": false,
                        "no_op": true,
                        "persistence": { "state": "skipped_duplicate" },
                        "live_application": { "state": "skipped_duplicate" },
                    },
                    "guard": {
                        "kind": "semantic_duplicate",
                        "reason": "repeated_successful_config_mutation",
                        "original_action_id": original.action_id,
                        "original_mutation_id": runtime_config_change_mutation_id(signature),
                        "continuation": "terminated",
                    },
                })
                .to_string(),
            ),
        ))
    }

    /// Records one successful semantic mutation for later continuations of the
    /// same logical turn.
    fn record_successful_config_change(
        &mut self,
        turn_id: &str,
        signature: &mez_agent::ConfigChangeMutationSignature,
        result: &ActionResult,
    ) {
        if result.status != ActionStatus::Succeeded {
            return;
        }
        self.agent
            .agent_turn_config_change_successes
            .entry(turn_id.to_string())
            .or_default()
            .entry(signature.canonical_material())
            .or_insert_with(|| result.clone());
    }

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
    pub(crate) fn execute_config_change_action_for_turn(
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
        let persist_path = self.ensure_agent_config_change_persist_path()?;
        let signature = match runtime_config_change_signature(action, &persist_path) {
            Ok(signature) => signature,
            Err(error) => {
                return Ok(ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mezzanine_error_code(error.kind()),
                    error.message().to_string(),
                )?);
            }
        };
        if let Some(result) =
            self.repeated_config_change_result(turn, action, approval_state, &signature)
        {
            return Ok(result);
        }
        if setting_path == "theme.active"
            && mez_agent::normalize_config_change_operation(operation)?.sets_value()
        {
            let result = self.execute_theme_config_change_action_for_turn(
                turn,
                action,
                approval_state,
                &signature,
            )?;
            self.record_successful_config_change(&turn.turn_id, &signature, &result);
            return Ok(result);
        }
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
                    let public_response =
                        if contains_secret_material(signature.setting_path(), ConfigScope::Primary)
                        {
                            "config_change control request failed for a redacted setting"
                                .to_string()
                        } else {
                            persistent_response.clone()
                        };
                    let mut result = ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        "config_change_failed",
                        public_response.clone(),
                    )?;
                    result.structured_content_json = Some(runtime_config_change_failure_json(
                        action,
                        approval_state,
                        &signature,
                        &public_response,
                    ));
                    Ok(result)
                } else {
                    let changed = serde_json::from_str::<serde_json::Value>(&persistent_response)
                        .ok()
                        .and_then(|value| {
                            value
                                .pointer("/result/plan/changed")
                                .and_then(serde_json::Value::as_bool)
                        })
                        .unwrap_or(false);
                    let result = ActionResult::succeeded(
                        turn,
                        action,
                        vec![format!(
                            "configuration change {}: {} {}",
                            if changed {
                                "persisted and applied"
                            } else {
                                "already satisfied"
                            },
                            signature.operation(),
                            signature.setting_path()
                        )],
                        Some(runtime_config_change_success_json(
                            self,
                            action,
                            approval_state,
                            &signature,
                            &persistent_response,
                        )?),
                    );
                    self.record_successful_config_change(&turn.turn_id, &signature, &result);
                    Ok(result)
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
        approval_state: &str,
        signature: &mez_agent::ConfigChangeMutationSignature,
    ) -> Result<ActionResult> {
        let AgentActionPayload::ConfigChange { value, .. } = &action.payload else {
            return Err(MezError::invalid_args(
                "theme config change requires a config_change action",
            ));
        };
        let theme =
            match mez_agent::config_change_string_value(signature.setting_path(), value.as_deref())
            {
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
            Ok(command_response) => {
                let live_changed =
                    runtime_config_change_command_field_bool(&command_response, "changed");
                let persisted_changed = runtime_config_change_command_field_bool(
                    &command_response,
                    "persisted_changed",
                );
                let changed = live_changed || persisted_changed;
                let persistence_state = if persisted_changed {
                    "persisted"
                } else {
                    "already_satisfied"
                };
                let live_state = if live_changed {
                    "applied"
                } else {
                    "already_satisfied"
                };
                Ok(ActionResult::succeeded(
                    turn,
                    action,
                    vec![format!(
                        "configuration change {}: {} {}",
                        if changed {
                            "persisted and applied"
                        } else {
                            "already satisfied"
                        },
                        signature.operation(),
                        signature.setting_path()
                    )],
                    Some(
                        serde_json::json!({
                            "approval": {
                                "state": approval_state,
                                "kind": "config_change",
                                "action_id": action.id,
                            },
                            "config_change": {
                                "mutation_id": runtime_config_change_mutation_id(signature),
                                "operation": signature.operation(),
                                "path": signature.setting_path(),
                                "requested_value": runtime_config_change_requested_value_json(signature),
                                "value_redacted": contains_secret_material(
                                    signature.setting_path(),
                                    ConfigScope::Primary,
                                ),
                                "persistent_target": signature.persistent_target(),
                                "validation": { "state": "succeeded" },
                                "changed": changed,
                                "no_op": !changed,
                                "persistence": { "state": persistence_state },
                                "live_application": { "state": live_state },
                                "effective": runtime_config_change_effective_result_json(
                                    self,
                                    signature,
                                )?,
                            },
                            "runtime_command": "set-theme",
                            "theme": theme,
                            "command_response": command_response,
                        })
                        .to_string(),
                    ),
                ))
            }
            Err(error) => {
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mezzanine_error_code(error.kind()),
                    error.message().to_string(),
                )?;
                result.structured_content_json = Some(runtime_config_change_failure_json(
                    action,
                    approval_state,
                    signature,
                    error.message(),
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
        let preparation = (|| -> Result<_> {
            let persist_path = self.ensure_agent_config_change_persist_path()?;
            let mut pending = Vec::new();
            let mut duplicates = Vec::new();
            for (index, action) in actions {
                let signature = runtime_config_change_signature(action, &persist_path)?;
                if let Some(result) =
                    self.repeated_config_change_result(turn, action, approval_state, &signature)
                {
                    duplicates.push((*index, result));
                } else {
                    pending.push((
                        *index,
                        action.clone(),
                        signature,
                        runtime_config_change_mutation_from_action(action)?,
                    ));
                }
            }
            Ok((persist_path, pending, duplicates))
        })();
        match preparation {
            Ok((persist_path, pending, duplicates)) => {
                for (index, result) in duplicates {
                    execution.action_results[index] = result;
                }
                if !pending.is_empty() {
                    let mutations = pending
                        .iter()
                        .map(|(_, _, _, mutation)| mutation.clone())
                        .collect::<Vec<_>>();
                    match runtime_apply_persisted_config_mutation_batch(
                        self,
                        persist_path,
                        &mutations,
                        "agent/config_change:theme-batch",
                    ) {
                        Ok(report) => {
                            for ((index, action, signature, _), changed) in pending
                                .into_iter()
                                .zip(report.mutation_changed.iter().copied())
                            {
                                let persistence_state = if changed {
                                    if report.deferred {
                                        "queued"
                                    } else {
                                        "persisted"
                                    }
                                } else {
                                    "already_satisfied"
                                };
                                let live_state = if changed {
                                    "applied"
                                } else {
                                    "already_satisfied"
                                };
                                let result = ActionResult::succeeded(
                                    turn,
                                    &action,
                                    vec![format!(
                                            "configuration change {} in batch: {} {}",
                                        if changed {
                                            "persisted and applied"
                                        } else {
                                            "already satisfied"
                                        },
                                        signature.operation(),
                                        signature.setting_path()
                                    )],
                                    Some(
                                        serde_json::json!({
                                            "approval": {
                                                "state": approval_state,
                                                "kind": "config_change",
                                                "action_id": action.id,
                                            },
                                            "config_change": {
                                                "mutation_id": runtime_config_change_mutation_id(&signature),
                                                "operation": signature.operation(),
                                                "path": signature.setting_path(),
                                                "requested_value": runtime_config_change_requested_value_json(&signature),
                                                "value_redacted": contains_secret_material(
                                                    signature.setting_path(),
                                                    ConfigScope::Primary,
                                                ),
                                                "persistent_target": signature.persistent_target(),
                                                "validation": { "state": "succeeded" },
                                                "changed": changed,
                                                "no_op": !changed,
                                                "persistence": { "state": persistence_state },
                                                "live_application": { "state": live_state },
                                                "effective": runtime_config_change_effective_result_json(
                                                    self,
                                                    &signature,
                                                )?,
                                            },
                                            "persistent_batch": {
                                                "path": report.path.to_string_lossy(),
                                                "changed": report.changed,
                                                "reload_required": report.reload_required,
                                                "mutation_count": report.mutation_count,
                                                "deferred": report.deferred,
                                            },
                                        })
                                        .to_string(),
                                    ),
                                );
                                self.record_successful_config_change(
                                    &turn.turn_id,
                                    &signature,
                                    &result,
                                );
                                execution.action_results[index] = result;
                            }
                        }
                        Err(error) => {
                            for (index, action, _, _) in pending {
                                execution.action_results[index] = ActionResult::failed(
                                    turn,
                                    &action,
                                    ActionStatus::Failed,
                                    runtime_mezzanine_error_code(error.kind()),
                                    error.message().to_string(),
                                )?;
                            }
                        }
                    }
                }
            }
            Err(error) => {
                for (index, action) in actions {
                    execution.action_results[*index] = ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?;
                }
            }
        }
        let executed = actions.len();
        let suppressed_duplicate = execution
            .action_results
            .iter()
            .any(runtime_config_change_result_is_suppressed_duplicate);
        if suppressed_duplicate {
            execution.final_turn = true;
            self.agent
                .pending_agent_provider_tasks
                .remove(&turn.turn_id);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
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
        let suppressed_duplicate = execution
            .action_results
            .iter()
            .any(runtime_config_change_result_is_suppressed_duplicate);
        if suppressed_duplicate {
            execution.final_turn = true;
            self.agent
                .pending_agent_provider_tasks
                .remove(&turn.turn_id);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        Ok(executed)
    }

    /// Retries retained configuration changes once a primary client reconnects.
    ///
    /// A configuration mutation is executed through the primary-owned runtime
    /// control path. While detached, the action therefore remains running in
    /// its retained execution instead of failing the agent turn. Reattachment
    /// supplies the primary identity required to finish that work.
    #[cfg(test)]
    pub(crate) fn resume_detached_config_change_actions(&mut self) -> Result<()> {
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
            let mut terminal_observations = RuntimeTerminalActionObservations::default();
            terminal_observations.observe(&execution);
            if self.execute_running_config_change_actions_for_turn(&turn, &mut execution)? > 0 {
                terminal_observations.observe(&execution);
                if !terminal_observations.results().is_empty() {
                    self.commit_settled_action_results_context(
                        &turn.turn_id,
                        terminal_observations.results(),
                    )?;
                }
                if execution.terminal_state == AgentTurnState::Running
                    && runtime_execution_ready_for_provider_continuation(&execution)
                {
                    self.agent
                        .pending_agent_provider_tasks
                        .insert(turn.turn_id.clone());
                }
                self.agent_turn_executions_mut()
                    .insert(turn.turn_id.clone(), execution);
            }
        }
        Ok(())
    }
}
