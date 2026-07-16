//! Control-method parameter schema and pane-size validation.

use super::entry::client_terminal_descriptor_from_control;
use super::{
    ControlParamsSchema, JsonRpcRequest, MezError, PaneSizeSpec, RequestedRole, ResizeAxis,
    ResizeDirection, Result, Session, client_descriptor_from_json, client_json,
    control_method_spec, ensure_client_descriptor_role_matches, json_null_field, json_object_field,
    json_raw_field, json_string_field, parse_json_object_value, reject_unknown_json_fields,
    require_idempotency_key, validate_config_control_params_schema,
};
/// Returns the wall-clock timestamp supplied to lower approval state changes.
pub(super) fn control_current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Runs the validate control method params schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn validate_control_method_params_schema(request: &JsonRpcRequest) -> Result<()> {
    let params = request.params.as_deref().unwrap_or("{}");
    let Some(method_spec) = control_method_spec(&request.method) else {
        return Ok(());
    };
    match method_spec.params_schema {
        ControlParamsSchema::Unchecked => Ok(()),
        ControlParamsSchema::Allowed(allowed_fields) => reject_unknown_json_fields(
            params,
            &format!("{} params", request.method),
            allowed_fields,
        ),
        ControlParamsSchema::Config => validate_config_control_params_schema(request),
    }
}

/// Runs the reject runtime required creation fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn reject_runtime_required_creation_fields(
    method: &str,
    params: &str,
    fields: &[&str],
) -> Result<()> {
    for field in fields {
        if json_raw_field(params, field).is_some() && !json_null_field(params, field) {
            return Err(MezError::invalid_state(format!(
                "{method} requires an attached terminal runtime for `{field}`"
            )));
        }
    }
    Ok(())
}

/// Runs the dispatch session attach parsed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_session_attach_parsed(
    request: &JsonRpcRequest,
    session: &mut Session,
) -> Result<String> {
    let params = request
        .params
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("session/attach requires a params object"))?;
    require_idempotency_key(params)?;
    let role = json_string_field(params, "role").unwrap_or_else(|| "primary".to_string());
    let client_descriptor = json_object_field(params, "client")
        .as_deref()
        .map(client_descriptor_from_json)
        .transpose()?
        .ok_or_else(|| MezError::invalid_args("session/attach requires client"))?;
    match role.as_str() {
        "primary" => {
            ensure_client_descriptor_role_matches(
                &client_descriptor,
                RequestedRole::Primary,
                "session/attach client descriptor",
            )?;
            let client_id = session.attach_primary_with_terminal(
                &client_descriptor.name,
                client_descriptor.interactive,
                client_terminal_descriptor_from_control(client_descriptor.terminal.as_ref()),
            )?;
            let client = session
                .clients()
                .iter()
                .find(|client| client.id == client_id)
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "client not found")
                })?;
            Ok(format!(
                r#"{{"client":{},"approval_pending":false}}"#,
                client_json(session, client)
            ))
        }
        "observer" => {
            ensure_client_descriptor_role_matches(
                &client_descriptor,
                RequestedRole::Observer,
                "session/attach client descriptor",
            )?;
            let (client_id, _observer_id) = session.request_observer_with_terminal(
                &client_descriptor.name,
                client_terminal_descriptor_from_control(client_descriptor.terminal.as_ref()),
            );
            let client = session
                .clients()
                .iter()
                .find(|client| client.id == client_id)
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "client not found")
                })?;
            Ok(format!(
                r#"{{"client":{},"approval_pending":true}}"#,
                client_json(session, client)
            ))
        }
        _ => Err(MezError::invalid_args(
            "session/attach role must be primary or observer",
        )),
    }
}

/// Runs the control pane size spec operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn control_pane_size_spec(params: &str, context: &'static str) -> Result<PaneSizeSpec> {
    let value = parse_json_object_value(params, "pane/resize params")?;
    let size = value
        .get("size")
        .ok_or_else(|| MezError::invalid_args("pane/resize requires size"))?;
    control_pane_size_spec_value(size, context)
}

/// Runs the control pane size spec value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn control_pane_size_spec_value(
    value: &serde_json::Value,
    context: &'static str,
) -> Result<PaneSizeSpec> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{context} must be an object")))?;
    let mode = object
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args(format!("{context} requires mode")))?;
    match mode {
        "cells" => {
            let columns = optional_size_u16(object.get("columns"), "columns")?;
            let rows = optional_size_u16(object.get("rows"), "rows")?;
            if columns.is_none() && rows.is_none() {
                return Err(MezError::invalid_args(
                    "cells size requires columns or rows",
                ));
            }
            Ok(PaneSizeSpec::Cells { columns, rows })
        }
        "percent" => {
            let percent = required_size_u16(object.get("percent"), "percent")?;
            let axis = match object
                .get("axis")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("both")
            {
                "columns" | "horizontal" => ResizeAxis::Columns,
                "rows" | "vertical" => ResizeAxis::Rows,
                "both" => ResizeAxis::Both,
                _ => return Err(MezError::invalid_args("percent size axis is invalid")),
            };
            Ok(PaneSizeSpec::Percent { percent, axis })
        }
        "delta" => {
            let direction = object
                .get("direction")
                .and_then(serde_json::Value::as_str)
                .and_then(ResizeDirection::from_name)
                .ok_or_else(|| MezError::invalid_args("delta size direction is invalid"))?;
            let amount = required_size_u16(object.get("amount"), "amount")?;
            Ok(PaneSizeSpec::Delta { direction, amount })
        }
        "edge" => {
            let edge = object
                .get("edge")
                .and_then(serde_json::Value::as_str)
                .and_then(ResizeDirection::from_name)
                .ok_or_else(|| MezError::invalid_args("edge size edge is invalid"))?;
            let amount = required_size_u16(object.get("amount"), "amount")?;
            Ok(PaneSizeSpec::Edge { edge, amount })
        }
        _ => Err(MezError::invalid_args("size mode is invalid")),
    }
}

/// Runs the optional size u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn optional_size_u16(
    value: Option<&serde_json::Value>,
    field: &'static str,
) -> Result<Option<u16>> {
    value
        .map(|value| required_size_u16(Some(value), field))
        .transpose()
}

/// Runs the required size u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn required_size_u16(
    value: Option<&serde_json::Value>,
    field: &'static str,
) -> Result<u16> {
    let value = value
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args(format!("size {field} must be a number")))?;
    u16::try_from(value)
        .map_err(|_| MezError::invalid_args(format!("size {field} is out of range")))
}
