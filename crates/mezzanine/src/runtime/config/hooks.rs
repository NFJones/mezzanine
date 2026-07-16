//! Runtime hook configuration and payload helpers.
//!
//! This module owns hook payload serialization, marker-token creation, and
//! runtime hook definition parsing for live config application. Keeping these
//! helpers out of the config root separates hook-specific contracts from the
//! general JSON and permission parsing utilities.

use serde_json::Value;
use std::fs;

use crate::error::{MezError, Result};
use crate::integrations::hooks::{
    HookDefinition, HookEvent, HookInvocation, HookMatcherGroup, HookMatcherOperator,
    HookMatcherPredicate,
};
use mez_agent::permissions::{DEFAULT_COMMAND_SHELL_CLASSIFICATION, exact_command_sha256};
use mez_agent::{ActionResult, AgentAction, AgentTurnRecord, MarkerToken, ModelProfile};

use super::{
    json_escape, runtime_json_bool, runtime_json_object, runtime_json_scalar_string,
    runtime_json_string, runtime_json_string_array, runtime_json_u64,
};

/// Runs the runtime marker for action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_marker_for_action(
    turn: &AgentTurnRecord,
    action_id: &str,
) -> Result<MarkerToken> {
    let material = format!(
        "{}\0{}\0{}\0{}",
        turn.turn_id, turn.agent_id, turn.pane_id, action_id
    );
    runtime_random_marker_token(&material)
}

/// Runs the runtime random marker token operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_random_marker_token(material: &str) -> Result<MarkerToken> {
    let mut random = [0u8; 32];
    {
        use std::io::Read as _;
        let mut source = fs::File::open("/dev/urandom").map_err(|error| {
            MezError::invalid_state(format!("failed to open marker entropy source: {error}"))
        })?;
        source.read_exact(&mut random).map_err(|error| {
            MezError::invalid_state(format!("failed to read marker entropy: {error}"))
        })?;
    }
    let mut token = String::with_capacity(96);
    for byte in random {
        let _ = std::fmt::Write::write_fmt(&mut token, format_args!("{byte:02x}"));
    }
    token.push_str(&exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, material)[..32]);
    Ok(MarkerToken::new(token)?)
}

/// Runs the runtime pre shell hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_pre_shell_hook_payload(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    command: &str,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","action_id":"{}","action_type":"shell_command","command":"{}","command_sha256":"{}"}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&action.id),
        json_escape(command),
        exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, command)
    )
}

/// Runs the runtime post shell hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_post_shell_hook_payload(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    result: &ActionResult,
    exit_code: i32,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","action_id":"{}","action_type":"shell_command","status":"{:?}","is_error":{},"exit_code":{}}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&action.id),
        result.status,
        result.is_error,
        exit_code
    )
}

/// Runs the runtime hook target pane id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_hook_target_pane_id(event_payload_json: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(event_payload_json).ok()?;
    value
        .get("pane_id")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/turn/pane_id").and_then(Value::as_str))
        .or_else(|| value.pointer("/pane/id").and_then(Value::as_str))
        .filter(|pane_id| !pane_id.is_empty())
        .map(ToOwned::to_owned)
}

/// Runs the runtime user prompt hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_user_prompt_hook_payload(pane_id: &str, prompt: &str) -> String {
    format!(
        r#"{{"pane_id":"{}","prompt_bytes":{},"prompt_sha256":"{}"}}"#,
        json_escape(pane_id),
        prompt.len(),
        exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, prompt)
    )
}

/// Runs the runtime agent turn start hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_agent_turn_start_hook_payload(
    turn: &AgentTurnRecord,
    model_profile: &ModelProfile,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","model_provider":"{}","model":"{}"}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&model_profile.provider),
        json_escape(&model_profile.model)
    )
}

/// Runs the runtime pre mcp hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_pre_mcp_hook_payload(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    server: &str,
    tool: &str,
    arguments_json: &str,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","action_id":"{}","action_type":"mcp_call","server":"{}","tool":"{}","arguments_json":"{}","arguments_bytes":{}}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&action.id),
        json_escape(server),
        json_escape(tool),
        json_escape(arguments_json),
        arguments_json.len()
    )
}

/// Runs the runtime post mcp hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_post_mcp_hook_payload(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    result: &ActionResult,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","action_id":"{}","action_type":"mcp_call","status":"{:?}","is_error":{}}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&action.id),
        result.status,
        result.is_error
    )
}

/// Runs the runtime permission request hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_permission_request_hook_payload(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    result: &ActionResult,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","action_id":"{}","action_type":"{}","approval":{}}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&action.id),
        action.action_type(),
        result
            .structured_content_json
            .as_deref()
            .unwrap_or(r#"{"approval":{"state":"pending"}}"#)
    )
}

/// Runs the runtime permission decision hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_permission_decision_hook_payload(
    approval_id: &str,
    decision: &str,
) -> String {
    format!(
        r#"{{"approval_id":"{}","decision":"{}"}}"#,
        json_escape(approval_id),
        json_escape(decision)
    )
}

/// Runs the runtime mcp error code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_mcp_error_code(error: &MezError) -> &'static str {
    match error.kind() {
        crate::error::MezErrorKind::InvalidState
            if error.message().contains("MCP protocol error")
                || error.message().contains("JSON-RPC")
                || error.message().contains("response") =>
        {
            "mcp_protocol_error"
        }
        crate::error::MezErrorKind::InvalidState => "transport_error",
        crate::error::MezErrorKind::Forbidden => "permission_denied",
        _ => "transport_error",
    }
}

/// Runs the runtime hook definitions from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_hook_definitions_from_config(root: &Value) -> Result<Vec<HookDefinition>> {
    let mut definitions = Vec::new();
    let Some(hooks) = runtime_json_object(root, "hooks") else {
        return Ok(definitions);
    };

    for (hook_id, value) in hooks {
        let Some(object) = value.as_object() else {
            return Err(MezError::config(format!(
                "hooks.{hook_id} must be an object"
            )));
        };
        let events = runtime_hook_events_from_config(hook_id, object)?;
        if events.is_empty() {
            continue;
        }
        let invocation = runtime_hook_invocation_from_config(hook_id, object)?;
        let enabled = runtime_json_bool(object.get("enabled")).unwrap_or(true);
        let required = runtime_json_bool(object.get("required")).unwrap_or(false);
        let agent_hook = runtime_json_bool(object.get("agent_hook")).unwrap_or(false);
        let matcher_groups = runtime_hook_matcher_groups_from_config(hook_id, object)?;
        let timeout_ms = runtime_json_u64(object.get("timeout_ms")).or_else(|| {
            runtime_json_u64(object.get("timeout_sec")).map(|seconds| seconds.saturating_mul(1000))
        });
        let on_failure = runtime_json_string(object.get("on_failure"))
            .map(runtime_hook_on_failure_from_config)
            .transpose()?;

        for event in events {
            let definition = HookDefinition {
                id: hook_id.clone(),
                event,
                invocation: invocation.clone(),
                enabled,
                required,
                agent_hook,
                matcher_groups: matcher_groups.clone(),
                timeout_ms,
                on_failure,
            };
            definition.validate()?;
            definitions.push(definition);
        }
    }

    Ok(definitions)
}

/// Runs the runtime hook matcher groups from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_matcher_groups_from_config(
    hook_id: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<HookMatcherGroup>> {
    let mut groups = Vec::new();
    if let Some(group) = object.get("match") {
        groups.push(runtime_hook_matcher_group_from_config(
            &format!("hooks.{hook_id}.match"),
            group,
        )?);
    }
    if let Some(matches) = object.get("matches") {
        let array = matches.as_array().ok_or_else(|| {
            MezError::config(format!(
                "hooks.{hook_id}.matches must be an array of matcher groups"
            ))
        })?;
        for (index, group) in array.iter().enumerate() {
            groups.push(runtime_hook_matcher_group_from_config(
                &format!("hooks.{hook_id}.matches[{index}]"),
                group,
            )?);
        }
    }
    Ok(groups)
}

/// Runs the runtime hook matcher group from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_hook_matcher_group_from_config(path: &str, value: &Value) -> Result<HookMatcherGroup> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a matcher object")))?;
    let mut predicates = Vec::new();
    if runtime_hook_matcher_is_single_predicate(object) {
        let predicate_path = runtime_json_string(object.get("path")).ok_or_else(|| {
            MezError::config(format!("{path}.path is required for matcher predicates"))
        })?;
        predicates.push(runtime_hook_matcher_predicate_from_config(
            path,
            predicate_path,
            value,
        )?);
    } else {
        for (predicate_path, predicate_value) in object {
            predicates.push(runtime_hook_matcher_predicate_from_config(
                &format!("{path}.{predicate_path}"),
                predicate_path,
                predicate_value,
            )?);
        }
    }
    if predicates.is_empty() {
        return Err(MezError::config(format!(
            "{path} must contain at least one matcher predicate"
        )));
    }
    Ok(HookMatcherGroup { predicates })
}

/// Runs the runtime hook matcher is single predicate operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_hook_matcher_is_single_predicate(object: &serde_json::Map<String, Value>) -> bool {
    object.contains_key("path")
        && object.keys().any(|key| {
            matches!(
                key.as_str(),
                "equals" | "prefix" | "suffix" | "contains" | "exists"
            )
        })
}

/// Runs the runtime hook matcher predicate from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_hook_matcher_predicate_from_config(
    diagnostic_path: &str,
    predicate_path: &str,
    value: &Value,
) -> Result<HookMatcherPredicate> {
    if predicate_path.trim().is_empty() {
        return Err(MezError::config(format!(
            "{diagnostic_path} matcher path must not be empty"
        )));
    }
    Ok(HookMatcherPredicate {
        path: predicate_path.to_string(),
        operator: runtime_hook_matcher_operator_from_config(diagnostic_path, value)?,
    })
}

/// Runs the runtime hook matcher operator from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_hook_matcher_operator_from_config(
    diagnostic_path: &str,
    value: &Value,
) -> Result<HookMatcherOperator> {
    let Some(object) = value.as_object() else {
        return Ok(HookMatcherOperator::Equals(runtime_json_scalar_string(
            diagnostic_path,
            value,
        )?));
    };
    if let Some(value) = object.get("equals") {
        return Ok(HookMatcherOperator::Equals(runtime_json_scalar_string(
            &format!("{diagnostic_path}.equals"),
            value,
        )?));
    }
    if let Some(value) = object.get("prefix") {
        return Ok(HookMatcherOperator::Prefix(runtime_json_scalar_string(
            &format!("{diagnostic_path}.prefix"),
            value,
        )?));
    }
    if let Some(value) = object.get("suffix") {
        return Ok(HookMatcherOperator::Suffix(runtime_json_scalar_string(
            &format!("{diagnostic_path}.suffix"),
            value,
        )?));
    }
    if let Some(value) = object.get("contains") {
        return Ok(HookMatcherOperator::Contains(runtime_json_scalar_string(
            &format!("{diagnostic_path}.contains"),
            value,
        )?));
    }
    if let Some(value) = object.get("exists") {
        let exists = value.as_bool().ok_or_else(|| {
            MezError::config(format!("{diagnostic_path}.exists must be a boolean"))
        })?;
        return Ok(HookMatcherOperator::Exists(exists));
    }
    Err(MezError::config(format!(
        "{diagnostic_path} must use equals, prefix, suffix, contains, or exists"
    )))
}

/// Runs the runtime hook events from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_events_from_config(
    hook_id: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<HookEvent>> {
    if let Some(event) = runtime_json_string(object.get("event")) {
        return Ok(vec![runtime_hook_event_from_config(event)?]);
    }
    if let Some(events) = runtime_json_string_array(object.get("events"))? {
        let mut parsed = Vec::with_capacity(events.len());
        for event in events {
            parsed.push(runtime_hook_event_from_config(&event)?);
        }
        return Ok(parsed);
    }
    if object.is_empty() {
        return Ok(Vec::new());
    }
    Err(MezError::config(format!(
        "hooks.{hook_id}.event or hooks.{hook_id}.events is required"
    )))
}

/// Runs the runtime hook invocation from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_invocation_from_config(
    hook_id: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<HookInvocation> {
    let args = runtime_json_string_array(object.get("args"))?.unwrap_or_default();
    if let Some(program) = runtime_json_string(object.get("program")) {
        return Ok(HookInvocation::Program {
            command: program.to_string(),
            args,
        });
    }
    let Some(command) = runtime_json_string(object.get("command")) else {
        return Err(MezError::config(format!(
            "hooks.{hook_id}.program or hooks.{hook_id}.command is required"
        )));
    };
    match runtime_json_string(object.get("kind")) {
        Some("program") => Ok(HookInvocation::Program {
            command: command.to_string(),
            args,
        }),
        Some("shell" | "focused_shell" | "focused-shell") | None => {
            Ok(HookInvocation::FocusedShell {
                command: command.to_string(),
            })
        }
        Some(kind) => Err(MezError::config(format!(
            "hooks.{hook_id}.kind must be program, shell, or focused_shell; got {kind}"
        ))),
    }
}

/// Runs the runtime hook event from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_event_from_config(value: &str) -> Result<HookEvent> {
    match value {
        "session_start" | "SessionStart" => Ok(HookEvent::SessionStart),
        "session_stop" | "SessionStop" => Ok(HookEvent::SessionStop),
        "client_attach" | "ClientAttach" => Ok(HookEvent::ClientAttach),
        "client_detach" | "ClientDetach" => Ok(HookEvent::ClientDetach),
        "window_create" | "WindowCreate" => Ok(HookEvent::WindowCreate),
        "window_close" | "WindowClose" => Ok(HookEvent::WindowClose),
        "session_detach" | "SessionDetach" => Ok(HookEvent::SessionDetach),
        "pane_create" | "pane_created" | "PaneCreate" | "PaneCreated" => Ok(HookEvent::PaneCreate),
        "pane_close" | "pane_closed" | "PaneClose" | "PaneClosed" => Ok(HookEvent::PaneClose),
        "user_prompt_submit" | "UserPromptSubmit" => Ok(HookEvent::UserPromptSubmit),
        "agent_turn_start" | "AgentTurnStart" => Ok(HookEvent::AgentTurnStart),
        "agent_turn_stop" | "agent_turn_end" | "AgentTurnStop" | "AgentTurnEnd" => {
            Ok(HookEvent::AgentTurnStop)
        }
        "pre_shell_command" | "PreShellCommand" => Ok(HookEvent::PreShellCommand),
        "post_shell_command" | "PostShellCommand" => Ok(HookEvent::PostShellCommand),
        "permission_request" | "PermissionRequest" => Ok(HookEvent::PermissionRequest),
        "permission_decision" | "PermissionDecision" => Ok(HookEvent::PermissionDecision),
        "pre_mcp_tool_use" | "PreMcpToolUse" => Ok(HookEvent::PreMcpToolUse),
        "post_mcp_tool_use" | "PostMcpToolUse" => Ok(HookEvent::PostMcpToolUse),
        "layout_save" | "LayoutSave" => Ok(HookEvent::LayoutSave),
        "layout_load" | "LayoutLoad" => Ok(HookEvent::LayoutLoad),
        _ => Err(MezError::config(format!(
            "unsupported hook event `{value}`"
        ))),
    }
}

/// Runs the runtime hook on failure from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_on_failure_from_config(
    value: &str,
) -> Result<crate::integrations::hooks::HookOnFailure> {
    match value {
        "block" => Ok(crate::integrations::hooks::HookOnFailure::Block),
        "warn" => Ok(crate::integrations::hooks::HookOnFailure::Warn),
        "ignore" => Ok(crate::integrations::hooks::HookOnFailure::Ignore),
        _ => Err(MezError::config(format!(
            "hook on_failure must be block, warn, or ignore; got {value}"
        ))),
    }
}
