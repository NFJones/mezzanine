//! Permission and approval command helpers for runtime commands.
//!
//! This child module owns live permission policy commands, approval-policy
//! commands, command-rule parsing, bypass toggles, and permission audit
//! records. It keeps policy mutation and display rules isolated from unrelated
//! command-support helpers while preserving the runtime-visible command API.

use super::super::{
    ApprovalPolicy, ArgumentPolicy, AuditActor, AuditRecord, CommandInvocation, CommandRule,
    CommandRuleScope, DEFAULT_COMMAND_SHELL_CLASSIFICATION, MezError, PermissionAuthorityChange,
    PermissionPolicy, PermissionPreset, Result, RuleDecision, RuleMatch, RuntimeSessionService,
    compare_approval_policy_authority, compare_permission_preset_authority,
};
use super::mcp::runtime_apply_permission_live_override;
use super::{runtime_flag_value, runtime_positional_args};

/// Runs the runtime permissions command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_permissions_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let args = runtime_positional_args(invocation);
    if args.is_empty() || matches!(args.as_slice(), ["status"] | ["show"]) {
        return Ok(runtime_permission_policy_display(
            service.permission_policy(),
        ));
    }

    let body = match args.as_slice() {
        ["preset", requested] | ["set-preset", requested] => {
            let Ok(requested) = runtime_parse_permission_preset(requested) else {
                return Ok(format!(
                    "field=preset:requested={requested}:changed=false:reason=unsupported-permission-preset:source=runtime-policy"
                ));
            };
            let current = service.permission_policy().preset;
            let change = compare_permission_preset_authority(current, requested);
            runtime_apply_permission_live_override(
                service,
                None,
                "permissions.preset",
                runtime_permission_preset_name(requested),
                "terminal/command:permissions",
            )?;
            runtime_append_permission_audit(
                service,
                "permissions.preset",
                "permission_change",
                runtime_permission_preset_name(requested),
                "changed",
            )?;
            runtime_permission_change_display(
                "preset",
                runtime_permission_preset_name(current),
                runtime_permission_preset_name(requested),
                change,
                true,
            )
        }
        ["approval-policy", requested] | ["approval_policy", requested] => {
            let Ok(requested) = runtime_parse_approval_policy(requested) else {
                return Ok(format!(
                    "field=approval_policy:requested={requested}:changed=false:reason=unsupported-approval-policy:source=runtime-policy"
                ));
            };
            let current = service.permission_policy().approval_policy;
            let change = compare_approval_policy_authority(current, requested);
            runtime_apply_permission_live_override(
                service,
                None,
                "permissions.approval_policy",
                runtime_approval_policy_name(requested),
                "terminal/command:permissions",
            )?;
            runtime_append_permission_audit(
                service,
                "permissions.approval_policy",
                "permission_change",
                runtime_approval_policy_name(requested),
                "changed",
            )?;
            runtime_permission_change_display(
                "approval_policy",
                runtime_approval_policy_name(current),
                runtime_approval_policy_name(requested),
                change,
                true,
            )
        }
        [requested] => match runtime_parse_permission_preset(requested) {
            Ok(requested) => {
                let current = service.permission_policy().preset;
                let change = compare_permission_preset_authority(current, requested);
                runtime_apply_permission_live_override(
                    service,
                    None,
                    "permissions.preset",
                    runtime_permission_preset_name(requested),
                    "terminal/command:permissions",
                )?;
                runtime_append_permission_audit(
                    service,
                    "permissions.preset",
                    "permission_change",
                    runtime_permission_preset_name(requested),
                    "changed",
                )?;
                runtime_permission_change_display(
                    "preset",
                    runtime_permission_preset_name(current),
                    runtime_permission_preset_name(requested),
                    change,
                    true,
                )
            }
            Err(_) => {
                format!(
                    "requested={requested}:changed=false:reason=unsupported-permission-command:source=runtime-policy"
                )
            }
        },
        _ => {
            "changed=false:reason=unsupported-permission-command:source=runtime-policy".to_string()
        }
    };
    Ok(body)
}

/// Runs the runtime approval command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_approval_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let args = runtime_positional_args(invocation);
    if args.is_empty() || matches!(args.as_slice(), ["status"] | ["show"]) {
        return Ok(format!(
            "approval_policy={} source=runtime-policy",
            runtime_approval_policy_name(service.permission_policy().approval_policy)
        ));
    }
    let [requested] = args.as_slice() else {
        return Ok(
            "changed=false:reason=unsupported-approval-command:source=runtime-policy".to_string(),
        );
    };
    let Ok(requested) = runtime_parse_approval_policy(requested) else {
        return Ok(format!(
            "field=approval_policy:requested={requested}:changed=false:reason=unsupported-approval-policy:source=runtime-policy"
        ));
    };
    let current = service.permission_policy().approval_policy;
    let change = compare_approval_policy_authority(current, requested);
    runtime_apply_permission_live_override(
        service,
        None,
        "permissions.approval_policy",
        runtime_approval_policy_name(requested),
        "terminal/command:approval",
    )?;
    runtime_append_permission_audit(
        service,
        "permissions.approval_policy",
        "permission_change",
        runtime_approval_policy_name(requested),
        "changed",
    )?;
    Ok(runtime_permission_change_display(
        "approval_policy",
        runtime_approval_policy_name(current),
        runtime_approval_policy_name(requested),
        change,
        true,
    ))
}

/// Runs the runtime permission policy display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_permission_policy_display(policy: &PermissionPolicy) -> String {
    format!(
        "preset={} approval_policy={} bypass={} rules={} source=runtime-policy",
        runtime_permission_preset_name(policy.preset),
        runtime_approval_policy_name(policy.approval_policy),
        policy.approval_bypass(),
        policy.rules().len()
    )
}

/// Runs the runtime permission change display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_permission_change_display(
    field: &str,
    current: &str,
    requested: &str,
    change: PermissionAuthorityChange,
    changed: bool,
) -> String {
    let approval_required = matches!(change, PermissionAuthorityChange::Broadening);
    let approved_by = if approval_required {
        ":approved_by=primary-command"
    } else {
        ""
    };
    format!(
        "field={field}:current={current}:requested={requested}:authority_change={}:approval_required={approval_required}{approved_by}:changed={changed}:source=runtime-policy",
        runtime_permission_authority_change_name(change)
    )
}

/// Runs the runtime list command rules display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_list_command_rules_display(policy: &PermissionPolicy) -> String {
    if policy.rules().is_empty() {
        return "rules=0 source=runtime-policy".to_string();
    }
    policy
        .rules()
        .iter()
        .enumerate()
        .map(|(index, rule)| runtime_command_rule_display_line(index + 1, rule))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs the runtime command rule display line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_command_rule_display_line(index: usize, rule: &CommandRule) -> String {
    format!(
        "rule{}:scope={}:decision={}:match={}:pattern={}:argument_policy={}:source=runtime-policy",
        index,
        runtime_command_rule_scope_name(rule.scope),
        runtime_rule_decision_name(rule.decision),
        runtime_rule_match_name(&rule.rule_match),
        rule.pattern.join(" "),
        runtime_argument_policy_name(&rule.argument_policy)
    )
}

/// Runs the runtime add command rule operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_add_command_rule(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let rule = runtime_command_rule_from_invocation(invocation)?;
    let decision = runtime_rule_decision_name(rule.decision);
    let prefix = rule.pattern.join(" ");
    let scope = runtime_command_rule_scope_name(rule.scope);
    let rule_match = runtime_rule_match_name(&rule.rule_match);
    let approval_required = rule.decision == RuleDecision::Allow;
    service.permission_policy_mut().add_rule(rule);
    runtime_append_permission_audit(
        service,
        "permissions.command_rules",
        "command_rule",
        decision,
        "added",
    )?;
    Ok(format!(
        "decision={decision}:scope={scope}:match={rule_match}:prefix={prefix}:approval_required={approval_required}:approved_by=primary-command:changed=true:source=runtime-policy"
    ))
}

/// Runs the runtime remove command rule operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_remove_command_rule(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let rule_id = runtime_positional_args(invocation)
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("remove-command-rule requires a rule id"))?;
    let removed = service.permission_policy_mut().remove_rule(rule_id)?;
    runtime_append_permission_audit(
        service,
        "permissions.command_rules",
        "command_rule",
        runtime_rule_decision_name(removed.decision),
        "removed",
    )?;
    Ok(format!(
        "rule={rule_id}:removed=true:decision={}:scope={}:source=runtime-policy",
        runtime_rule_decision_name(removed.decision),
        runtime_command_rule_scope_name(removed.scope)
    ))
}

/// Runs the runtime command rule from invocation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_command_rule_from_invocation(
    invocation: &CommandInvocation,
) -> Result<CommandRule> {
    let decision = match invocation.name.as_str() {
        "allow-command" => RuleDecision::Allow,
        "deny-command" => RuleDecision::Forbid,
        "prompt-command" => RuleDecision::Prompt,
        _ => {
            return Err(MezError::invalid_args(format!(
                "command `{}` cannot create a command rule",
                invocation.name
            )));
        }
    };
    let scope = runtime_command_rule_scope(invocation)?;
    let rule_match = runtime_command_rule_match(invocation)?;
    let mut rule = match rule_match {
        RuleMatch::ExactSha256 { .. } => {
            let digest =
                runtime_flag_value(&invocation.args, "--exact-sha256").ok_or_else(|| {
                    MezError::invalid_args("exact_sha256 command rules require --exact-sha256")
                })?;
            CommandRule::from_exact_sha256_digest(
                digest,
                runtime_flag_value(&invocation.args, "--shell-classification")
                    .unwrap_or(DEFAULT_COMMAND_SHELL_CLASSIFICATION),
                decision,
            )?
        }
        RuleMatch::Prefix | RuleMatch::Exact => {
            let pattern = runtime_command_rule_pattern_args(invocation);
            if pattern.is_empty() {
                return Err(MezError::invalid_args(
                    "command rule requires a command prefix",
                ));
            }
            CommandRule::new(pattern, decision, rule_match)?
        }
    }
    .with_scope(scope);
    if let Some(justification) = runtime_flag_value(&invocation.args, "--justification") {
        rule = rule.with_justification(justification);
    }
    Ok(rule)
}

/// Runs the runtime command rule pattern args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_command_rule_pattern_args(invocation: &CommandInvocation) -> Vec<&str> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < invocation.args.len() {
        let arg = invocation.args[index].as_str();
        if arg == "--" {
            values.extend(invocation.args[index + 1..].iter().map(String::as_str));
            break;
        }
        if matches!(
            arg,
            "--scope" | "--match" | "--exact-sha256" | "--shell-classification" | "--justification"
        ) {
            index = index.saturating_add(2);
            continue;
        }
        values.push(arg);
        index += 1;
    }
    values
}

/// Runs the runtime command rule scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_command_rule_scope(
    invocation: &CommandInvocation,
) -> Result<CommandRuleScope> {
    let value = runtime_flag_value(&invocation.args, "--scope").unwrap_or("session");
    match value {
        "session" => Ok(CommandRuleScope::Session),
        "project" => Ok(CommandRuleScope::Project),
        "user" | "global" => Ok(CommandRuleScope::User),
        "managed" => Ok(CommandRuleScope::Managed),
        "built-in" => Err(MezError::invalid_args(
            "built-in command rules cannot be added through the live policy",
        )),
        _ => Err(MezError::invalid_args(
            "command rule scope must be session, project, user, global, or managed",
        )),
    }
}

/// Runs the runtime command rule match operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_command_rule_match(invocation: &CommandInvocation) -> Result<RuleMatch> {
    if let Some(digest) = runtime_flag_value(&invocation.args, "--exact-sha256") {
        return Ok(RuleMatch::ExactSha256 {
            digest_hex: digest.to_string(),
            shell_classification: runtime_flag_value(&invocation.args, "--shell-classification")
                .unwrap_or(DEFAULT_COMMAND_SHELL_CLASSIFICATION)
                .to_string(),
        });
    }
    match runtime_flag_value(&invocation.args, "--match").unwrap_or("prefix") {
        "prefix" => Ok(RuleMatch::Prefix),
        "exact" => Ok(RuleMatch::Exact),
        "exact_sha256" => Err(MezError::invalid_args(
            "exact_sha256 command rules require --exact-sha256",
        )),
        _ => Err(MezError::invalid_args(
            "command rule match must be prefix or exact",
        )),
    }
}

/// Runs the runtime bypass approvals command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_bypass_approvals_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let requested = runtime_bypass_action(invocation).unwrap_or("status");
    let current = service.permission_policy().approval_bypass();
    let body = match requested {
        "status" | "show" => format!("bypass={current}:source=runtime-policy"),
        "enable" | "on" | "true" => {
            if current {
                return Ok(
                    "requested=enable:bypass=true:changed=false:source=runtime-policy".to_string(),
                );
            }
            if !runtime_has_bypass_confirmation(invocation) {
                return Ok("requested=enable:bypass=false:changed=false:confirmation_required=true:reason=explicit-confirmation-required:source=runtime-policy".to_string());
            }
            service.set_live_approval_bypass_override(true);
            runtime_append_permission_audit(
                service,
                "permissions.bypass_mode",
                "approval_bypass",
                "enabled",
                "changed",
            )?;
            "requested=enable:bypass=true:changed=true:confirmed=true:source=runtime-policy"
                .to_string()
        }
        "disable" | "off" | "false" => {
            service.set_live_approval_bypass_override(false);
            if current {
                runtime_append_permission_audit(
                    service,
                    "permissions.bypass_mode",
                    "approval_bypass",
                    "disabled",
                    "changed",
                )?;
            }
            format!(
                "requested=disable:bypass=false:changed={}:source=runtime-policy",
                current
            )
        }
        _ => format!(
            "requested={requested}:bypass={current}:changed=false:reason=unsupported-bypass-command:source=runtime-policy"
        ),
    };
    Ok(body)
}

/// Runs the runtime append permission audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_append_permission_audit(
    service: &mut RuntimeSessionService,
    permission_id: &str,
    action_kind: &str,
    decision: &str,
    outcome: &str,
) -> Result<()> {
    let policy_mode =
        runtime_permission_preset_name(service.permission_policy().preset).to_string();
    let Some(audit_log) = service.persistence.audit_log_mut() else {
        return Ok(());
    };
    let record = AuditRecord::permission_decision(
        service.session.id.to_string(),
        AuditActor {
            kind: "client".to_string(),
            id: "primary-command".to_string(),
        },
        permission_id.to_string(),
        action_kind.to_string(),
        decision.to_string(),
        policy_mode,
        outcome.to_string(),
    );
    let _ = audit_log.append(record)?;
    Ok(())
}

/// Runs the runtime bypass action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_bypass_action(invocation: &CommandInvocation) -> Option<&str> {
    invocation
        .args
        .iter()
        .find(|arg| {
            !matches!(
                arg.as_str(),
                "--confirm" | "--yes" | "--dangerously-bypass-approvals"
            )
        })
        .map(String::as_str)
}

/// Runs the runtime has bypass confirmation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_has_bypass_confirmation(invocation: &CommandInvocation) -> bool {
    invocation.args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--confirm" | "--yes" | "--dangerously-bypass-approvals"
        )
    })
}

/// Runs the runtime parse permission preset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_parse_permission_preset(
    value: &str,
) -> std::result::Result<PermissionPreset, ()> {
    match value {
        "read-only" | "readonly" => Ok(PermissionPreset::ReadOnly),
        "auto" => Ok(PermissionPreset::Auto),
        _ => Err(()),
    }
}

/// Runs the runtime parse approval policy operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_parse_approval_policy(
    value: &str,
) -> std::result::Result<ApprovalPolicy, ()> {
    match value {
        "ask" => Ok(ApprovalPolicy::Ask),
        "auto-allow" | "auto_allow" => Ok(ApprovalPolicy::AutoAllow),
        "full-access" | "full_access" => Ok(ApprovalPolicy::FullAccess),
        _ => Err(()),
    }
}

/// Runs the runtime permission preset name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_permission_preset_name(preset: PermissionPreset) -> &'static str {
    match preset {
        PermissionPreset::ReadOnly => "read-only",
        PermissionPreset::Auto => "auto",
    }
}

/// Runs the runtime approval policy name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_approval_policy_name(policy: ApprovalPolicy) -> &'static str {
    match policy {
        ApprovalPolicy::Ask => "ask",
        ApprovalPolicy::AutoAllow => "auto-allow",
        ApprovalPolicy::FullAccess => "full-access",
    }
}

/// Runs the runtime permission authority change name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_permission_authority_change_name(
    change: PermissionAuthorityChange,
) -> &'static str {
    match change {
        PermissionAuthorityChange::Narrowing => "narrowing",
        PermissionAuthorityChange::NoChange => "no-change",
        PermissionAuthorityChange::Broadening => "broadening",
    }
}

/// Runs the runtime command rule scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_command_rule_scope_name(scope: CommandRuleScope) -> &'static str {
    match scope {
        CommandRuleScope::BuiltIn => "built-in",
        CommandRuleScope::Session => "session",
        CommandRuleScope::Project => "project",
        CommandRuleScope::User => "user",
        CommandRuleScope::Managed => "managed",
    }
}

/// Runs the runtime rule decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_rule_decision_name(decision: RuleDecision) -> &'static str {
    match decision {
        RuleDecision::Forbid => "deny",
        RuleDecision::Prompt => "prompt",
        RuleDecision::Allow => "allow",
    }
}

/// Runs the runtime rule match name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_rule_match_name(rule_match: &RuleMatch) -> &'static str {
    match rule_match {
        RuleMatch::Prefix => "prefix",
        RuleMatch::Exact => "exact",
        RuleMatch::ExactSha256 { .. } => "exact_sha256",
    }
}

/// Runs the runtime argument policy name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_argument_policy_name(argument_policy: &ArgumentPolicy) -> &'static str {
    match argument_policy {
        ArgumentPolicy::None => "none",
        ArgumentPolicy::ExecutableProbe { .. } => "executable_probe",
        ArgumentPolicy::UnameProbe => "uname_probe",
        ArgumentPolicy::LiteralOutput => "literal_output",
        ArgumentPolicy::ReadPaths { .. } => "read_paths",
        ArgumentPolicy::ScriptThenReadPaths { .. } => "script_then_read_paths",
        ArgumentPolicy::FindReadOnly => "find_read_only",
        ArgumentPolicy::GitReadOnly { .. } => "git_read_only",
    }
}
