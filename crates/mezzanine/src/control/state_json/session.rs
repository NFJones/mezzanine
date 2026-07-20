//! Session summaries, target filtering, and state request projection.

use super::approvals::optional_rfc3339_timestamp_json;
use super::clients::{clients_json, observers_json_for_state};
use super::snapshots::observer_state_filter_from_params;
use super::{
    ClientState, GrantedRole, MezError, Result, Session, builtin_rules, json_escape,
    json_optional_string, observers_json, pane_state_json, parse_json_object_value,
    require_session_target_matches_value, resolve_window_target_value, session_state_name,
    window_by_id, window_state_json,
};
/// Runs the granted role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn granted_role_name(role: GrantedRole) -> &'static str {
    match role {
        GrantedRole::Primary => "primary",
        GrantedRole::PendingObserver => "pending_observer",
        GrantedRole::Observer => "observer",
        GrantedRole::Agent => "agent",
        GrantedRole::Automation => "automation",
    }
}

/// Runs the session summary json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn session_summary_json(session: &Session) -> String {
    let active_window_id = session.active_window().map(|window| window.id.to_string());
    let primary_client_id = session
        .primary_client_id()
        .map(|client_id| client_id.to_string());
    let attached_client_count = session
        .clients()
        .iter()
        .filter(|client| client.state == ClientState::Attached)
        .count();
    format!(
        r#"{{"id":"{}","version":1,"name":"{}","state":"{}","created_at":{},"last_attached_at":{},"window_count":{},"attached_client_count":{},"has_primary":{},"primary_client_id":{},"active_window_id":{}}}"#,
        json_escape(&session.id.to_string()),
        json_escape(&session.name),
        session_state_name(session.state),
        optional_rfc3339_timestamp_json(Some(session.created_at_unix_seconds)),
        optional_rfc3339_timestamp_json(session.last_attached_at_unix_seconds),
        session.windows().len(),
        attached_client_count,
        session.primary_client_id().is_some(),
        json_optional_string(primary_client_id.as_deref()),
        json_optional_string(active_window_id.as_deref())
    )
}

/// Runs the session state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn session_state_json(session: &Session) -> String {
    let primary_client_id = session
        .primary_client_id()
        .map(|client_id| client_id.to_string());
    let active_window_id = session.active_window().map(|window| window.id.to_string());
    format!(
        r#"{{"id":"{}","version":1,"session_id":"{}","name":"{}","state":"{}","created_at":{},"updated_at":{},"primary_client_id":{},"authoritative_size":{{"columns":{},"rows":{}}},"active_window_id":{},"windows":{},"window_count":{},"clients":{},"observers":{},"config_generation":{},"permission_summary":{}}}"#,
        json_escape(&session.id.to_string()),
        json_escape(&session.id.to_string()),
        json_escape(&session.name),
        session_state_name(session.state),
        optional_rfc3339_timestamp_json(Some(session.created_at_unix_seconds)),
        optional_rfc3339_timestamp_json(Some(session.updated_at_unix_seconds)),
        json_optional_string(primary_client_id.as_deref()),
        session.authoritative_size.columns,
        session.authoritative_size.rows,
        json_optional_string(active_window_id.as_deref()),
        windows_json(session),
        session.windows().len(),
        clients_json(session),
        observers_json(session),
        session.config_generation,
        permission_summary_json()
    )
}

/// Runs the session state json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn session_state_json_for_params(
    session: &Session,
    params: Option<&str>,
) -> Result<String> {
    state_request_session_target_matches(session, params, "session/get params")?;
    Ok(session_state_json(session))
}

/// Runs the permission summary json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn permission_summary_json() -> String {
    format!(
        r#"{{"preset":"read-only","approval_policy":"ask","bypass_active":false,"sandbox":"policy-only","network_policy":"prompt","trusted_project":false,"trusted_directories":[],"read_scopes":[],"write_scopes":[],"command_rule_generation":{}}}"#,
        builtin_rules().len()
    )
}

/// Runs the windows json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn windows_json(session: &Session) -> String {
    let windows = session
        .windows()
        .iter()
        .map(|window| window_state_json(session, window))
        .collect::<Vec<_>>();
    format!("[{}]", windows.join(","))
}

/// Runs the windows json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn windows_json_for_params(
    session: &Session,
    params: Option<&str>,
) -> Result<String> {
    state_request_session_target_matches(session, params, "window/list params")?;
    Ok(windows_json(session))
}

/// Runs the panes json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn panes_json(session: &Session) -> String {
    let panes = session
        .active_window()
        .map(|window| {
            window
                .panes()
                .iter()
                .map(|pane| pane_state_json(session, window, pane))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    format!("[{}]", panes.join(","))
}

/// Runs the panes json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn panes_json_for_params(
    session: &Session,
    params: Option<&str>,
) -> Result<String> {
    let Some(window_ids) = state_request_pane_list_window_ids(session, params, "pane/list params")?
    else {
        return Ok(panes_json(session));
    };
    let panes = window_ids
        .iter()
        .map(|window_id| window_by_id(session, window_id))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flat_map(|window| {
            window
                .panes()
                .iter()
                .map(|pane| pane_state_json(session, window, pane))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    Ok(format!("[{}]", panes.join(",")))
}

/// Runs the clients json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn clients_json_for_params(
    session: &Session,
    params: Option<&str>,
) -> Result<String> {
    state_request_session_target_matches(session, params, "client/list params")?;
    Ok(clients_json(session))
}

/// Runs the observers json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn observers_json_for_params(
    session: &Session,
    params: Option<&str>,
) -> Result<String> {
    state_request_session_target_matches(session, params, "observer/list params")?;
    let state = observer_state_filter_from_params(params, "observer/list params")?;
    Ok(observers_json_for_state(session, state))
}

/// Validate a read-only state request `target` as a SessionTarget when present.
pub(crate) fn state_request_session_target_matches(
    session: &Session,
    params: Option<&str>,
    label: &str,
) -> Result<()> {
    let Some(target) = state_request_target_value(params, label)? else {
        return Ok(());
    };
    require_session_target_matches_value(session, &target)
}

/// Validate a nullable read-only `target` as a SessionTarget when present.
pub(crate) fn nullable_state_request_session_target_matches(
    session: &Session,
    params: Option<&str>,
    label: &str,
) -> Result<()> {
    let Some(params) = params else {
        return Ok(());
    };
    let value = parse_json_object_value(params, label)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{label} must be an object")))?;
    let Some(target) = object.get("target") else {
        return Ok(());
    };
    if target.is_null() {
        return Ok(());
    }
    require_session_target_matches_value(session, target)
}

/// Resolve the optional `pane/list` target into the windows whose panes should
/// be serialized. A missing target preserves legacy active-window behavior,
/// while a SessionTarget expands to every window in the current session.
pub(crate) fn state_request_pane_list_window_ids(
    session: &Session,
    params: Option<&str>,
    label: &str,
) -> Result<Option<Vec<String>>> {
    let Some(target) = state_request_target_value(params, label)? else {
        return Ok(None);
    };
    if state_target_has_window_shape(session, &target) {
        let window_id = resolve_window_target_value(session, &target)?
            .ok_or_else(|| MezError::invalid_args("pane/list WindowTarget requires a selector"))?;
        return Ok(Some(vec![window_id]));
    }
    require_session_target_matches_value(session, &target)?;
    Ok(Some(
        session
            .windows()
            .iter()
            .map(|window| window.id.to_string())
            .collect(),
    ))
}

/// Runs the state request target value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn state_request_target_value(
    params: Option<&str>,
    label: &str,
) -> Result<Option<serde_json::Value>> {
    let Some(params) = params else {
        return Ok(None);
    };
    let value = parse_json_object_value(params, label)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{label} must be an object")))?;
    Ok(object.get("target").cloned())
}

/// Runs the state target has window shape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn state_target_has_window_shape(session: &Session, value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.contains_key("window_id")
        || object.contains_key("window_index")
        || object.contains_key("window_name")
        || object.contains_key("index")
        || object.contains_key("active")
        || object.contains_key("session")
        || object.contains_key("default_session")
        || object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| name != session.name)
}
