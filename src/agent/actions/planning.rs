//! Agent action planning and approval metadata.
//!
//! This module owns conversion from validated MAAP actions into initial
//! `ActionResult` records. It keeps permission prompts, auto-allow metadata,
//! local/network action acceptance, MCP approval checks, and subagent/config
//! planning separate from turn execution and recovery handling.

use super::super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnRecord, MezError,
    PermissionPolicy, Result, RuleDecision, json_escape, local_action_plan, local_action_summary,
    network_action_plan, network_action_structured_content_json, network_action_summary,
    string_array_json,
};
use super::{AgentTurnRunner, say_structured_content_json, shell_command_structured_content_json};

impl<'a, P> AgentTurnRunner<'a, P> {
    /// Executes the `plan_action_result` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub(super) fn plan_action_result(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let local_plan = local_action_plan(action)?;
        let network_plan = network_action_plan(action)?;
        match &action.payload {
            AgentActionPayload::Say {
                status,
                text,
                content_type,
            } => Ok(ActionResult::succeeded(
                turn,
                action,
                vec![text.clone()],
                Some(say_structured_content_json(*status, content_type, text)),
            )),
            AgentActionPayload::RequestCapability { .. } => Err(MezError::invalid_state(
                "request_capability reached executable action planning",
            )),
            AgentActionPayload::RequestSkills => Ok(ActionResult::running(
                turn,
                action,
                vec!["skill catalog accepted for runtime lookup".to_string()],
                Some(r#"{"state":"pending_runtime_skill_lookup"}"#.to_string()),
            )),
            AgentActionPayload::CallSkill { name, .. } => Ok(ActionResult::running(
                turn,
                action,
                vec![format!("skill {name} accepted for runtime loading")],
                Some(format!(
                    r#"{{"state":"pending_runtime_skill_load","name":"{}"}}"#,
                    json_escape(name)
                )),
            )),
            _ if local_plan.is_some() => {
                let Some(plan) = local_plan.as_ref() else {
                    return Err(MezError::invalid_state(
                        "local action plan was unavailable after local action match",
                    ));
                };
                if let Some(scope) = self.subagent_scope
                    && let Some(message) =
                        subagent_scope_violation(scope, action, &plan.policy_command)?
                {
                    return ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Denied,
                        "subagent_scope_violation",
                        message,
                    );
                }
                match self
                    .permissions
                    .evaluate_shell_command_with_approvals_scoped(
                        &plan.policy_command,
                        self.approvals,
                        self.path_scopes,
                    ) {
                    RuleDecision::Allow => Ok(ActionResult::running(
                        turn,
                        action,
                        vec!["local action accepted for local dispatch".to_string()],
                        Some(shell_command_structured_content_json(
                            action,
                            Some("pending_local_dispatch"),
                            false,
                            serde_json::Value::Null,
                            &[],
                            serde_json::json!({"state":"pending_dispatch"}),
                        )?),
                    )),
                    RuleDecision::Prompt
                        if self.permissions.approval_policy
                            == mez_agent::ApprovalPolicy::AutoAllow
                            && action_supports_auto_allow(action) =>
                    {
                        let reason = action_auto_allow_reason(action);
                        Ok(ActionResult::running(
                            turn,
                            action,
                            vec![
                                "local action auto-allowed by model assessment".to_string(),
                                reason,
                            ],
                            Some(shell_command_structured_content_json(
                                action,
                                Some("pending_local_dispatch"),
                                false,
                                auto_allow_approval_json(action, action.action_type()),
                                &[],
                                serde_json::json!({"state":"pending_dispatch"}),
                            )?),
                        ))
                    }
                    RuleDecision::Prompt => Ok(ActionResult::blocked(
                        turn,
                        action,
                        vec!["approval required before executing local action".to_string()],
                        shell_command_structured_content_json(
                            action,
                            Some("pending_local_dispatch"),
                            false,
                            serde_json::json!({
                                "state": "pending",
                                "kind": action.action_type(),
                                "action_id": action.id.as_str(),
                                "command": plan.policy_command.as_str()
                            }),
                            &[],
                            serde_json::json!({"state":"pending_approval"}),
                        )?,
                    )),
                    RuleDecision::Forbid => ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Denied,
                        "policy_forbidden",
                        "local action denied by permission policy",
                    ),
                }
            }
            _ if network_plan.is_some() => {
                let Some(plan) = network_plan.as_ref() else {
                    return Err(MezError::invalid_state(
                        "network action plan was unavailable after network action match",
                    ));
                };
                match self
                    .permissions
                    .evaluate_shell_command_with_approvals_scoped(
                        &plan.policy_command,
                        self.approvals,
                        self.path_scopes,
                    ) {
                    RuleDecision::Allow => Ok(ActionResult::running(
                        turn,
                        action,
                        vec!["network action accepted for runtime execution".to_string()],
                        Some(network_action_structured_content_json(
                            action,
                            serde_json::Value::Null,
                            serde_json::json!({"state":"pending_runtime_network"}),
                        )?),
                    )),
                    RuleDecision::Prompt
                        if self.permissions.approval_policy
                            == mez_agent::ApprovalPolicy::AutoAllow
                            && action_supports_auto_allow(action) =>
                    {
                        let reason = action_auto_allow_reason(action);
                        Ok(ActionResult::running(
                            turn,
                            action,
                            vec![
                                "network action auto-allowed by model assessment".to_string(),
                                reason,
                            ],
                            Some(network_action_structured_content_json(
                                action,
                                auto_allow_approval_json(action, action.action_type()),
                                serde_json::json!({"state":"pending_runtime_network"}),
                            )?),
                        ))
                    }
                    RuleDecision::Prompt => Ok(ActionResult::blocked(
                        turn,
                        action,
                        vec!["approval required before executing network action".to_string()],
                        network_action_structured_content_json(
                            action,
                            serde_json::json!({
                                "state": "pending",
                                "kind": action.action_type(),
                                "action_id": action.id.as_str(),
                                "policy_command": plan.policy_command.as_str()
                            }),
                            serde_json::json!({"state":"pending_approval"}),
                        )?,
                    )),
                    RuleDecision::Forbid => ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Denied,
                        "policy_forbidden",
                        "network action denied by permission policy",
                    ),
                }
            }
            AgentActionPayload::SendMessage {
                recipient,
                content_type,
                payload,
            } => Ok(ActionResult::running(
                turn,
                action,
                vec!["message accepted for local delivery".to_string()],
                Some(format!(
                    r#"{{"recipient":"{}","content_type":"{}","bytes":{},"message_id":null,"delivery_status":"pending_runtime_delivery","protocol_error":null}}"#,
                    json_escape(recipient),
                    json_escape(content_type),
                    payload.len()
                )),
            )),
            AgentActionPayload::SpawnAgent {
                role,
                placement,
                cooperation_mode,
                read_scopes,
                write_scopes,
                task_prompt,
            } => Ok(ActionResult::running(
                turn,
                action,
                vec!["subagent spawn accepted for control endpoint placement".to_string()],
                Some(format!(
                    r#"{{"role":"{}","placement":"{}","cooperation_mode":"{}","read_scopes":{},"write_scopes":{},"prompt_bytes":{}}}"#,
                    json_escape(role),
                    json_escape(placement),
                    json_escape(cooperation_mode),
                    string_array_json(read_scopes),
                    string_array_json(write_scopes),
                    task_prompt.len()
                )),
            )),
            AgentActionPayload::MemorySearch { .. } | AgentActionPayload::MemoryStore { .. } => {
                Ok(ActionResult::running(
                    turn,
                    action,
                    vec!["memory action accepted for runtime execution".to_string()],
                    Some(r#"{"state":"pending_runtime_memory"}"#.to_string()),
                ))
            }
            AgentActionPayload::IssueAdd { .. }
            | AgentActionPayload::IssueUpdate { .. }
            | AgentActionPayload::IssueQuery { .. }
            | AgentActionPayload::IssueDelete { .. } => Ok(ActionResult::running(
                turn,
                action,
                vec!["issue action accepted for runtime execution".to_string()],
                Some(r#"{"state":"pending_runtime_issue"}"#.to_string()),
            )),
            AgentActionPayload::ConfigChange {
                setting_path,
                operation,
                ..
            } => {
                let policy_allowed = action_prompt_gate_satisfied_by_policy(self.permissions);
                let auto_allowed = !policy_allowed
                    && self.permissions.approval_policy == mez_agent::ApprovalPolicy::AutoAllow
                    && action_supports_auto_allow(action);
                if !policy_allowed && !auto_allowed {
                    return Ok(ActionResult::blocked(
                        turn,
                        action,
                        vec!["approval required before applying configuration change".to_string()],
                        format!(
                            r#"{{"approval":{{"state":"pending","kind":"config_change","path":"{}","operation":"{}","required_command":"/approve"}},"setting_path":"{}","operation":"{}","validation":{{"status":"pending_primary_approval"}},"applied_layer":null,"persistence":{{"requested":true,"completed":false,"scope":"user"}}}}"#,
                            json_escape(setting_path),
                            json_escape(operation),
                            json_escape(setting_path),
                            json_escape(operation)
                        ),
                    ));
                }
                let approval = if auto_allowed {
                    auto_allow_approval_json(action, "config_change")
                } else {
                    action_policy_approval_json(action, "config_change", self.permissions)
                };
                Ok(ActionResult::running(
                    turn,
                    action,
                    vec!["configuration change accepted for runtime application".to_string()],
                    Some(
                        serde_json::json!({
                            "approval": approval,
                            "setting_path": setting_path,
                            "operation": operation,
                            "validation": {"status": "pending_runtime_config_change"},
                            "applied_layer": null,
                            "persistence": {
                                "requested": true,
                                "completed": false,
                                "scope": "user"
                            }
                        })
                        .to_string(),
                    ),
                ))
            }
            AgentActionPayload::McpCall {
                server,
                tool,
                arguments_json,
            } => {
                let approval_required = self.mcp_tool_requires_approval(server, tool);
                let policy_allowed =
                    approval_required && action_prompt_gate_satisfied_by_policy(self.permissions);
                let auto_allowed = approval_required
                    && !policy_allowed
                    && self.permissions.approval_policy == mez_agent::ApprovalPolicy::AutoAllow
                    && action_supports_auto_allow(action);
                if approval_required && !policy_allowed && !auto_allowed {
                    return Ok(ActionResult::blocked(
                        turn,
                        action,
                        vec!["approval required before executing MCP tool call".to_string()],
                        format!(
                            r#"{{"approval":{{"state":"pending","kind":"mcp_call","action_id":"{}","server":"{}","tool":"{}"}}}}"#,
                            json_escape(&action.id),
                            json_escape(server),
                            json_escape(tool)
                        ),
                    ));
                }
                let auto_allow_reason = action_auto_allow_reason(action);
                Ok(ActionResult::running(
                    turn,
                    action,
                    if auto_allowed {
                        vec![
                            "mcp call auto-allowed by model assessment".to_string(),
                            auto_allow_reason,
                        ]
                    } else if approval_required {
                        vec!["mcp call accepted by approval policy".to_string()]
                    } else {
                        vec!["mcp call accepted for external-integration execution".to_string()]
                    },
                    Some(format!(
                        r#"{{"server":"{}","tool":"{}","arguments":{},"approval":{}}}"#,
                        json_escape(server),
                        json_escape(tool),
                        arguments_json,
                        if auto_allowed {
                            auto_allow_approval_json(action, "mcp_call").to_string()
                        } else if approval_required {
                            action_policy_approval_json(action, "mcp_call", self.permissions)
                                .to_string()
                        } else {
                            "null".to_string()
                        }
                    )),
                ))
            }
            AgentActionPayload::Complete => Ok(ActionResult::succeeded(
                turn,
                action,
                vec!["turn complete".to_string()],
                Some(r#"{"complete":true}"#.to_string()),
            )),
            AgentActionPayload::Abort { reason } => ActionResult::failed(
                turn,
                action,
                ActionStatus::Cancelled,
                "agent_aborted",
                reason,
            ),
            _ => Err(MezError::invalid_state(
                "shell-backed action was not planned before action-result planning",
            )),
        }
    }

    /// Executes the `mcp_tool_requires_approval` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub(super) fn mcp_tool_requires_approval(&self, server: &str, tool: &str) -> bool {
        self.available_mcp_tools
            .iter()
            .find(|available| available.server_id == server && available.tool_name == tool)
            .map(|available| available.approval_required)
            .unwrap_or(true)
    }
}

/// Returns a delegated subagent scope violation for one local action.
fn subagent_scope_violation(
    scope: &mez_agent::SubagentScopeDeclaration,
    action: &AgentAction,
    policy_command: &str,
) -> Result<Option<String>> {
    match &action.payload {
        AgentActionPayload::ApplyPatch { patch, .. } => {
            crate::subagent::SubagentScopeEnforcement::apply_patch_violation(scope, patch)
        }
        _ => crate::subagent::SubagentScopeEnforcement::shell_command_violation(
            scope,
            policy_command,
        ),
    }
}

/// Executes the `action_supports_auto_allow` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn action_supports_auto_allow(action: &AgentAction) -> bool {
    !action_auto_allow_reason(action).trim().is_empty()
}

/// Returns the most concise model-authored reason available for auto-allow
/// decisions after compact MAAP omitted the formerly mandatory rationale.
fn action_auto_allow_reason(action: &AgentAction) -> String {
    if !action.rationale.trim().is_empty() {
        return action.rationale.clone();
    }
    if let Ok(Some(summary)) = local_action_summary(action)
        && !summary.trim().is_empty()
    {
        return summary;
    }
    if let Ok(Some(summary)) = network_action_summary(action)
        && !summary.trim().is_empty()
    {
        return summary;
    }
    match &action.payload {
        AgentActionPayload::Say { text, .. } => text.clone(),
        AgentActionPayload::Abort { reason } => reason.clone(),
        AgentActionPayload::CallSkill { name, .. } => format!("load skill {name}"),
        AgentActionPayload::RequestSkills => "request available skills".to_string(),
        _ => String::new(),
    }
}

/// Returns true when the active runtime policy resolves a fresh approval
/// prompt without user interaction.
fn action_prompt_gate_satisfied_by_policy(permissions: &PermissionPolicy) -> bool {
    permissions.approval_bypass()
        || permissions.approval_policy == mez_agent::ApprovalPolicy::FullAccess
}

/// Builds structured approval metadata for actions accepted by policy rather
/// than by an explicit blocked-approval decision.
fn action_policy_approval_json(
    action: &AgentAction,
    kind: &str,
    permissions: &PermissionPolicy,
) -> serde_json::Value {
    let state = if permissions.approval_bypass() {
        "bypassed"
    } else {
        "full_access"
    };
    serde_json::json!({
        "state": state,
        "kind": kind,
        "action_id": action.id.as_str()
    })
}

/// Executes the `auto_allow_approval_json` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn auto_allow_approval_json(
    action: &AgentAction,
    action_kind: &str,
) -> serde_json::Value {
    serde_json::json!({
        "state": "auto_allowed",
        "kind": action_kind,
        "action_id": action.id.as_str(),
        "reason": action_auto_allow_reason(action)
    })
}
