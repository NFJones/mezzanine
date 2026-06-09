//! Runtime permission, approval, and recipient config helpers.
//!
//! This module owns live permission-policy materialization, command-rule
//! parsing, approval decision naming, blocked-approval request summaries, and
//! message recipient parsing for runtime config consumers. Keeping these helpers
//! outside the config root separates permission-specific contracts from shared
//! JSON and scalar parsing utilities.

use super::*;

/// Runs the runtime approval decision name to kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_approval_decision_name_to_kind(
    value: &str,
) -> Option<ApprovalDecision> {
    match value {
        "approve" | "allow" => Some(ApprovalDecision::Approve),
        "disapprove" | "deny" | "reject" => Some(ApprovalDecision::Disapprove),
        "redirect" => Some(ApprovalDecision::Redirect),
        _ => None,
    }
}

/// Runs the runtime message recipient operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_message_recipient(value: &str) -> Result<Recipient> {
    if value == "session" || value == "group:session" {
        return Ok(Recipient::Session);
    }
    if let Some(agent) = value.strip_prefix("agent:") {
        return AgentId::opaque(agent.to_string())
            .map(Recipient::Agent)
            .ok_or_else(|| MezError::invalid_args("send_message recipient agent id is invalid"));
    }
    if value.starts_with("agent-") {
        return AgentId::opaque(value.to_string())
            .map(Recipient::Agent)
            .ok_or_else(|| MezError::invalid_args("send_message recipient agent id is invalid"));
    }
    if let Some(agent) = AgentId::parse('a', value.to_string()) {
        return Ok(Recipient::Agent(agent));
    }
    if let Some(pane) = value.strip_prefix("pane:") {
        return PaneId::parse('%', pane.to_string())
            .map(Recipient::Pane)
            .ok_or_else(|| MezError::invalid_args("send_message recipient pane id is invalid"));
    }
    if let Some(pane) = PaneId::parse('%', value.to_string()) {
        return Ok(Recipient::Pane(pane));
    }
    if let Some(window) = value.strip_prefix("window:") {
        return WindowId::parse('@', window.to_string())
            .map(Recipient::Window)
            .ok_or_else(|| MezError::invalid_args("send_message recipient window id is invalid"));
    }
    if let Some(window) = WindowId::parse('@', value.to_string()) {
        return Ok(Recipient::Window(window));
    }
    if let Some(role) = value.strip_prefix("role:") {
        if role.is_empty() {
            return Err(MezError::invalid_args(
                "send_message recipient role is invalid",
            ));
        }
        return Ok(Recipient::Role(role.to_string()));
    }
    if let Some(capability) = value.strip_prefix("capability:") {
        if capability.is_empty() {
            return Err(MezError::invalid_args(
                "send_message recipient capability is invalid",
            ));
        }
        return Ok(Recipient::Capability(capability.to_string()));
    }
    if let Some(group) = value.strip_prefix("group:") {
        if group.is_empty() {
            return Err(MezError::invalid_args(
                "send_message recipient group is invalid",
            ));
        }
        return Ok(Recipient::Group(group.to_string()));
    }
    Err(MezError::invalid_args(
        "send_message recipient must be session, agent:<id>, pane:<id>, window:<id>, role:<name>, capability:<name>, or group:<name>",
    ))
}

/// Runs the runtime blocked approval request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_blocked_approval_request(
    turn: &AgentTurnRecord,
    result: &ActionResult,
    scope: Option<&SubagentScopeDeclaration>,
) -> BlockedApprovalRequest {
    let approval = result
        .structured_content_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| value.get("approval").cloned());
    let action_kind = approval
        .as_ref()
        .and_then(|value| value.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or(result.action_type)
        .to_string();
    let action_summary = runtime_blocked_approval_summary(result, approval.as_ref());
    BlockedApprovalRequest {
        id: String::new(),
        requesting_agent_id: turn.agent_id.clone(),
        pane_id: turn.pane_id.clone(),
        parent_agent_chain: vec![turn.agent_id.clone()],
        action_kind,
        action_summary,
        declared_effects: result.content_texts(),
        matched_rules: vec!["runtime.agent_action_blocked".to_string()],
        read_scopes: scope
            .map(|scope| scope.read_scopes.clone())
            .unwrap_or_default(),
        write_scopes: scope
            .map(|scope| scope.write_scopes.clone())
            .unwrap_or_default(),
        cooperation_mode: scope
            .map(|scope| runtime_cooperation_mode_name(scope.cooperation_mode).to_string())
            .or_else(|| turn.cooperation_mode.clone()),
        created_at_unix_seconds: None,
        decided_at_unix_seconds: None,
        decided_by_client_id: None,
        state: crate::permissions::BlockedApprovalState::Pending,
        decision: None,
        redirect_instruction: None,
    }
}

/// Runs the runtime blocked approval summary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_blocked_approval_summary(
    result: &ActionResult,
    approval: Option<&Value>,
) -> String {
    if let Some(approval) = approval {
        if let Some(command) = approval.get("command").and_then(Value::as_str) {
            return command.to_string();
        }
        if let Some(command) = approval.get("policy_command").and_then(Value::as_str) {
            return command.to_string();
        }
        if let (Some(server), Some(tool)) = (
            approval.get("server").and_then(Value::as_str),
            approval.get("tool").and_then(Value::as_str),
        ) {
            return format!("{server}/{tool}");
        }
        if let Some(path) = approval.get("path").and_then(Value::as_str) {
            let operation = approval
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or("change");
            return format!("{operation} {path}");
        }
        if let Some(prompt) = approval.get("prompt").and_then(Value::as_str) {
            return prompt.to_string();
        }
    }
    if result.content.is_empty() {
        result.action_type.to_string()
    } else {
        result.content_text().replace('\n', " ")
    }
}

/// Runs the runtime permission policy from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_permission_policy_from_config(
    root: &Value,
) -> Result<PermissionPolicy> {
    let mut policy = PermissionPolicy::default();
    let Some(permissions) = runtime_json_object(root, "permissions") else {
        return Ok(policy);
    };
    if let Some(preset) = runtime_json_string(permissions.get("preset")) {
        policy.preset = runtime_config_permission_preset(preset)?;
    }
    if let Some(approval_policy) = runtime_json_string(permissions.get("approval_policy")) {
        policy.approval_policy = runtime_config_approval_policy(approval_policy)?;
    }
    if let Some(bypass) = runtime_json_bool(permissions.get("bypass_mode")) {
        if bypass {
            return Err(MezError::config(
                "permissions.bypass_mode cannot be enabled from configuration; use explicit approval bypass activation",
            ));
        }
        policy.set_approval_bypass(false);
    }

    for (table, default_scope) in [
        ("command_rules", CommandRuleScope::Managed),
        ("session_command_rules", CommandRuleScope::Session),
        ("global_command_rules", CommandRuleScope::User),
    ] {
        let Some(rules) = permissions.get(table).and_then(Value::as_array) else {
            continue;
        };
        for rule_value in rules {
            policy.add_rule(runtime_command_rule_from_config(rule_value, default_scope)?);
        }
    }
    if let Some(dirs) = permissions
        .get("trusted_directories")
        .and_then(Value::as_array)
    {
        policy.trusted_directories = dirs
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
    }
    if let Some(projects) = permissions
        .get("trusted_projects")
        .and_then(Value::as_array)
    {
        for project in projects.iter().filter_map(Value::as_str) {
            policy.trusted_directories.push(project.to_string());
        }
    }
    Ok(policy)
}

/// Runs the runtime provider registry from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
/// Runs the runtime command rule from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_rule_from_config(
    value: &Value,
    default_scope: CommandRuleScope,
) -> Result<CommandRule> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config("permission command rule must be an object"))?;
    let decision = runtime_config_rule_decision(
        runtime_json_string(object.get("decision"))
            .ok_or_else(|| MezError::config("permission command rule requires decision"))?,
    )?;
    let scope = match runtime_json_string(object.get("scope")) {
        Some(scope) => runtime_config_command_rule_scope(scope)?,
        None => default_scope,
    };
    if scope == CommandRuleScope::BuiltIn {
        return Err(MezError::config(
            "configuration command rules cannot use built-in scope",
        ));
    }
    let match_kind = runtime_json_string(object.get("match")).unwrap_or("prefix");
    let mut rule = if match_kind == "exact_sha256" {
        let digest = runtime_json_string(object.get("exact_sha256")).ok_or_else(|| {
            MezError::config("exact_sha256 command rule requires exact_sha256 digest")
        })?;
        CommandRule::from_exact_sha256_digest(
            digest,
            runtime_json_string(object.get("shell_classification"))
                .unwrap_or(DEFAULT_COMMAND_SHELL_CLASSIFICATION),
            decision,
        )?
    } else {
        let pattern = runtime_json_rule_pattern(object.get("pattern"))?;
        let rule_match = match match_kind {
            "prefix" => RuleMatch::Prefix,
            "exact" => RuleMatch::Exact,
            _ => {
                return Err(MezError::config(
                    "permission command rule match must be prefix, exact, or exact_sha256",
                ));
            }
        };
        CommandRule::new(pattern, decision, rule_match)?
    }
    .with_scope(scope);
    if let Some(justification) = runtime_json_string(object.get("justification")) {
        rule = rule.with_justification(justification);
    }
    Ok(rule)
}

/// Runs the runtime json rule pattern operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_rule_pattern(value: Option<&Value>) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Err(MezError::config("permission command rule requires pattern"));
    };
    if let Some(pattern) = value.as_str() {
        return Ok(vec![pattern.to_string()]);
    }
    runtime_json_string_array(Some(value))?
        .filter(|pattern| !pattern.is_empty())
        .ok_or_else(|| MezError::config("permission command rule pattern must not be empty"))
}

/// Runs the runtime config permission preset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_config_permission_preset(
    value: &str,
) -> Result<PermissionPreset> {
    match value {
        "read-only" | "readonly" => Ok(PermissionPreset::ReadOnly),
        "auto" => Ok(PermissionPreset::Auto),
        _ => Err(MezError::config("unsupported permission preset")),
    }
}

/// Runs the runtime config approval policy operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_approval_policy(value: &str) -> Result<ApprovalPolicy> {
    match value {
        "ask" => Ok(ApprovalPolicy::Ask),
        "auto-allow" | "auto_allow" => Ok(ApprovalPolicy::AutoAllow),
        "full-access" | "full_access" => Ok(ApprovalPolicy::FullAccess),
        _ => Err(MezError::config(
            "unsupported approval policy; use ask, auto-allow, or full-access",
        )),
    }
}

/// Runs the runtime config rule decision operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_rule_decision(value: &str) -> Result<RuleDecision> {
    match value {
        "allow" => Ok(RuleDecision::Allow),
        "prompt" => Ok(RuleDecision::Prompt),
        "forbid" | "deny" => Ok(RuleDecision::Forbid),
        _ => Err(MezError::config("unsupported command rule decision")),
    }
}

/// Runs the runtime config command rule scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_command_rule_scope(value: &str) -> Result<CommandRuleScope> {
    match value {
        "session" => Ok(CommandRuleScope::Session),
        "project" => Ok(CommandRuleScope::Project),
        "user" | "global" => Ok(CommandRuleScope::User),
        "managed" => Ok(CommandRuleScope::Managed),
        _ => Err(MezError::config("unsupported command rule scope")),
    }
}
