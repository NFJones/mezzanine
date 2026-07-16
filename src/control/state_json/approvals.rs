//! Approval and project-trust state, decisions, audit, and scope persistence.

use super::*;

/// Runs the approvals json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn approvals_json_for_params(
    session: &Session,
    queue: &BlockedApprovalQueue,
    params: Option<&str>,
) -> Result<String> {
    state_request_session_target_matches(session, params, "approval/list params")?;
    let state = approval_state_filter_from_params(params, "approval/list params")?;
    Ok(approvals_json_for_state(queue, state))
}

/// Runs the approvals json for state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn approvals_json_for_state(
    queue: &BlockedApprovalQueue,
    state: Option<ApprovalListStateFilter>,
) -> String {
    let approvals = queue
        .requests()
        .filter(|approval| match state {
            Some(ApprovalListStateFilter::Matches(state)) => approval.state == state,
            Some(ApprovalListStateFilter::AlwaysEmpty) => false,
            None => true,
        })
        .map(approval_json)
        .collect::<Vec<_>>();
    format!("[{}]", approvals.join(","))
}

/// Carries Approval List State Filter state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApprovalListStateFilter {
    /// Represents the Matches case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Matches(BlockedApprovalState),
    /// Represents the Always Empty case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    AlwaysEmpty,
}

/// Runs the approval state filter from params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn approval_state_filter_from_params(
    params: Option<&str>,
    label: &str,
) -> Result<Option<ApprovalListStateFilter>> {
    let Some(params) = params else {
        return Ok(None);
    };
    let value = parse_json_object_value(params, label)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{label} must be an object")))?;
    let Some(state) = object.get("state") else {
        return Ok(None);
    };
    if state.is_null() {
        return Ok(None);
    }
    let state = state
        .as_str()
        .ok_or_else(|| MezError::invalid_args("approval/list state must be a string or null"))?;
    match state {
        "pending" => Ok(Some(ApprovalListStateFilter::Matches(
            BlockedApprovalState::Pending,
        ))),
        "approved" => Ok(Some(ApprovalListStateFilter::Matches(
            BlockedApprovalState::Approved,
        ))),
        "disapproved" => Ok(Some(ApprovalListStateFilter::Matches(
            BlockedApprovalState::Disapproved,
        ))),
        "redirected" => Ok(Some(ApprovalListStateFilter::Matches(
            BlockedApprovalState::Redirected,
        ))),
        "cancelled" | "invalidated" => Ok(Some(ApprovalListStateFilter::AlwaysEmpty)),
        _ => Err(MezError::invalid_args(
            "approval/list state must be pending, approved, disapproved, redirected, cancelled, invalidated, or null",
        )),
    }
}

/// Runs the approval audit record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn approval_audit_record(
    session: &Session,
    primary_client_id: &ClientId,
    approval: &BlockedApprovalRequest,
    outcome: &str,
) -> AuditRecord {
    let decision = approval
        .decision
        .map(approval_decision_name)
        .unwrap_or("pending");
    let scope = if approval.read_scopes.is_empty() && approval.write_scopes.is_empty() {
        "none".to_string()
    } else {
        format!(
            "read=[{}];write=[{}]",
            approval.read_scopes.join(","),
            approval.write_scopes.join(",")
        )
    };
    AuditRecord::approval_decision(
        session.id.as_str(),
        control_audit_actor(primary_client_id),
        &approval.id,
        &approval.requesting_agent_id,
        decision,
        scope,
        outcome,
    )
}

/// Runs the control audit actor operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn control_audit_actor(client_id: &ClientId) -> AuditActor {
    AuditActor {
        kind: "client".to_string(),
        id: client_id.to_string(),
    }
}

/// Runs the approval json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn approval_json(request: &BlockedApprovalRequest) -> String {
    format!(
        r#"{{"id":"{}","version":1,"approval_id":"{}","state":"{}","requester":{{"agent_id":"{}","pane_id":"{}","parent_agent_chain":{}}},"requesting_agent_id":"{}","pane_id":"{}","action_type":"{}","action_kind":"{}","created_at":{},"decided_at":{},"decided_by_client_id":{},"summary":"{}","action_summary":"{}","effects":{},"scope":{},"instruction":{},"decision":{},"redirect_instruction":{},"declared_effects":{},"matched_rules":{},"read_scopes":{},"write_scopes":{},"cooperation_mode":{}}}"#,
        json_escape(&request.id),
        json_escape(&request.id),
        blocked_approval_state_name(request.state),
        json_escape(&request.requesting_agent_id),
        json_escape(&request.pane_id),
        string_array_json(&request.parent_agent_chain),
        json_escape(&request.requesting_agent_id),
        json_escape(&request.pane_id),
        json_escape(&request.action_kind),
        json_escape(&request.action_kind),
        optional_rfc3339_timestamp_json(request.created_at_unix_seconds),
        optional_rfc3339_timestamp_json(request.decided_at_unix_seconds),
        json_optional_string(request.decided_by_client_id.as_deref()),
        json_escape(&request.action_summary),
        json_escape(&request.action_summary),
        approval_effects_json(request),
        approval_scope_json(request),
        json_optional_string(request.redirect_instruction.as_deref()),
        request
            .decision
            .map(approval_decision_name)
            .map(|value| format!(r#""{}""#, value))
            .unwrap_or_else(|| "null".to_string()),
        json_optional_string(request.redirect_instruction.as_deref()),
        string_array_json(&request.declared_effects),
        string_array_json(&request.matched_rules),
        string_array_json(&request.read_scopes),
        string_array_json(&request.write_scopes),
        json_optional_string(request.cooperation_mode.as_deref())
    )
}

/// Runs the optional rfc3339 timestamp json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn optional_rfc3339_timestamp_json(timestamp: Option<u64>) -> String {
    timestamp
        .map(|seconds| format!(r#""{}""#, unix_seconds_to_rfc3339(seconds)))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the approval effects json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn approval_effects_json(request: &BlockedApprovalRequest) -> String {
    let unknown = request.declared_effects.is_empty()
        || request
            .declared_effects
            .iter()
            .any(|effect| effect == "unknown");
    let network = request
        .declared_effects
        .iter()
        .any(|effect| effect == "network");
    let credentials = request
        .declared_effects
        .iter()
        .any(|effect| effect == "credentials");
    let process_control = request
        .declared_effects
        .iter()
        .any(|effect| effect == "process_control");
    let destructive = request
        .declared_effects
        .iter()
        .any(|effect| effect == "destructive");
    let privilege_change = request
        .declared_effects
        .iter()
        .any(|effect| effect == "privilege_change");
    format!(
        r#"{{"reads":{},"writes":{},"creates":[],"deletes":[],"touches":[],"network":{},"credentials":{},"process_control":{},"destructive":{},"privilege_change":{},"unknown":{}}}"#,
        string_array_json(&request.read_scopes),
        string_array_json(&request.write_scopes),
        network,
        credentials,
        process_control,
        destructive,
        privilege_change,
        unknown
    )
}

/// Runs the approval scope json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn approval_scope_json(request: &BlockedApprovalRequest) -> String {
    format!(
        r#"{{"persistence":"project","read_scopes":{},"write_scopes":{},"matched_rules":{}}}"#,
        string_array_json(&request.read_scopes),
        string_array_json(&request.write_scopes),
        string_array_json(&request.matched_rules)
    )
}

/// Runs the parse approval decision operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn parse_approval_decision(value: &str) -> Result<ApprovalDecision> {
    match value {
        "approve" => Ok(ApprovalDecision::Approve),
        "disapprove" => Ok(ApprovalDecision::Disapprove),
        "redirect" => Ok(ApprovalDecision::Redirect),
        _ => Err(MezError::invalid_args(
            "approval decision must be approve, disapprove, or redirect",
        )),
    }
}

/// Carries Approval Decision Scope Persistence state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDecisionScopePersistence {
    /// Represents the Once case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Once,
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session,
    /// Represents the Project case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Project,
    /// Represents the Global case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Global,
}

/// Parse the optional `approval/decide` scope object.
///
/// The baseline protocol allows callers to choose whether an approval applies
/// once, for the current session, for the current project, or globally. The full
/// scope object carries optional narrowing fields that are not yet semantically
/// consumed by every approval path, but the request shape is validated here so
/// malformed scopes do not pass silently through specialized approval dispatch.
pub(crate) fn approval_decide_scope_persistence(
    params: &str,
) -> Result<Option<ApprovalDecisionScopePersistence>> {
    let Some(raw_scope) = json_raw_field(params, "scope") else {
        return Ok(None);
    };
    if raw_scope.trim() == "null" {
        return Ok(None);
    }
    let scope = serde_json::from_str::<serde_json::Value>(&raw_scope).map_err(|error| {
        MezError::invalid_args(format!("approval/decide scope is invalid JSON: {error}"))
    })?;
    let object = scope
        .as_object()
        .ok_or_else(|| MezError::invalid_args("approval/decide scope must be an object or null"))?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "persistence"
                | "command_prefix"
                | "exact_sha256"
                | "working_directory"
                | "project_root"
                | "external_integration"
        ) {
            return Err(MezError::invalid_args(format!(
                "approval/decide scope contains unknown field `{key}`"
            )));
        }
    }
    let persistence = object
        .get("persistence")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args("approval/decide scope requires persistence"))?;
    validate_approval_scope_fields(object)?;
    match persistence {
        "once" => Ok(Some(ApprovalDecisionScopePersistence::Once)),
        "session" => Ok(Some(ApprovalDecisionScopePersistence::Session)),
        "project" => Ok(Some(ApprovalDecisionScopePersistence::Project)),
        "global" => Ok(Some(ApprovalDecisionScopePersistence::Global)),
        _ => Err(MezError::invalid_args(
            "approval/decide scope persistence must be once, session, project, or global",
        )),
    }
}

/// Runs the validate approval scope fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_approval_scope_fields(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    if let Some(command_prefix) = object.get("command_prefix") {
        let tokens = command_prefix.as_array().ok_or_else(|| {
            MezError::invalid_args("approval/decide scope command_prefix must be an array")
        })?;
        if tokens.is_empty() {
            return Err(MezError::invalid_args(
                "approval/decide scope command_prefix must not be empty",
            ));
        }
        for token in tokens {
            if token.as_str().is_none_or(|token| token.trim().is_empty()) {
                return Err(MezError::invalid_args(
                    "approval/decide scope command_prefix entries must be non-empty strings",
                ));
            }
        }
    }

    if let Some(exact_sha256) = object.get("exact_sha256") {
        let digest = non_empty_scope_string(exact_sha256, "exact_sha256")?;
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(MezError::invalid_args(
                "approval/decide scope exact_sha256 must be a 64-character hexadecimal digest",
            ));
        }
    }

    for field in ["working_directory", "project_root", "external_integration"] {
        if let Some(value) = object.get(field) {
            non_empty_scope_string(value, field)?;
        }
    }

    Ok(())
}

/// Runs the non empty scope string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn non_empty_scope_string<'a>(
    value: &'a serde_json::Value,
    field: &str,
) -> Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            MezError::invalid_args(format!(
                "approval/decide scope {field} must be a non-empty string"
            ))
        })
}

/// Runs the blocked approval state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn blocked_approval_state_name(state: BlockedApprovalState) -> &'static str {
    match state {
        BlockedApprovalState::Pending => "pending",
        BlockedApprovalState::Approved => "approved",
        BlockedApprovalState::Disapproved => "disapproved",
        BlockedApprovalState::Redirected => "redirected",
    }
}

/// Runs the approval decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn approval_decision_name(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Approve => "approve",
        ApprovalDecision::Disapprove => "disapprove",
        ApprovalDecision::Redirect => "redirect",
    }
}

/// Runs the parse trust decision operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn parse_trust_decision(value: &str) -> Result<TrustDecision> {
    match value {
        "trust" | "trusted" => Ok(TrustDecision::Trusted),
        "reject" | "rejected" => Ok(TrustDecision::Rejected),
        _ => Err(MezError::invalid_args(
            "project trust decision must be trust or reject",
        )),
    }
}

/// Runs the project trust state filter from params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn project_trust_state_filter_from_params(
    params: Option<&str>,
    label: &str,
) -> Result<Option<TrustDecision>> {
    let Some(params) = params else {
        return Ok(None);
    };
    let value = parse_json_object_value(params, label)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{label} must be an object")))?;
    let Some(state) = object.get("state") else {
        return Ok(None);
    };
    if state.is_null() {
        return Ok(None);
    }
    let state = state.as_str().ok_or_else(|| {
        MezError::invalid_args("project/trust/list state must be a string or null")
    })?;
    match state {
        "pending" => Ok(Some(TrustDecision::Pending)),
        "trusted" => Ok(Some(TrustDecision::Trusted)),
        "rejected" => Ok(Some(TrustDecision::Rejected)),
        "revoked" => Ok(Some(TrustDecision::Revoked)),
        _ => Err(MezError::invalid_args(
            "project/trust/list state must be pending, trusted, rejected, revoked, or null",
        )),
    }
}

/// Runs the project trust json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn project_trust_json(record: &ProjectTrustRecord) -> String {
    let git_marker_path = record
        .git_marker_path
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());
    let decided_at = if record.trusted_at_unix_seconds == 0 {
        "null".to_string()
    } else {
        format!(
            r#""{}""#,
            unix_seconds_to_rfc3339(record.trusted_at_unix_seconds)
        )
    };
    format!(
        r#"{{"id":"{}","version":1,"project_root":"{}","state":"{}","git_marker_path":{},"trusted_at":{},"rejected_at":{},"revoked_at":{},"decided_by_client_id":{},"trust_policy_version":{},"configuration_schema_version":{},"overlay_files":[],"capability_expansion_summary":[],"diagnostics":[],"trusted_at_unix_seconds":{},"vcs_remote":{}}}"#,
        json_escape(&record.project_root.to_string_lossy()),
        json_escape(&record.project_root.to_string_lossy()),
        trust_decision_name(record.state),
        json_optional_string(git_marker_path.as_deref()),
        if matches!(record.state, TrustDecision::Trusted) {
            decided_at.as_str()
        } else {
            "null"
        },
        if matches!(record.state, TrustDecision::Rejected) {
            decided_at.as_str()
        } else {
            "null"
        },
        if matches!(record.state, TrustDecision::Revoked) {
            decided_at.as_str()
        } else {
            "null"
        },
        json_optional_string(record.decided_by_client_id.as_deref()),
        record.trust_policy_version,
        record.configuration_schema_version,
        record.trusted_at_unix_seconds,
        json_optional_string(record.vcs_remote.as_deref())
    )
}

/// Runs the trust decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn trust_decision_name(decision: TrustDecision) -> &'static str {
    match decision {
        TrustDecision::Pending => "pending",
        TrustDecision::Trusted => "trusted",
        TrustDecision::Rejected => "rejected",
        TrustDecision::Revoked => "revoked",
    }
}
