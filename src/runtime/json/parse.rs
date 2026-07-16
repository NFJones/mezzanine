//! JSON-RPC errors and typed request-body parsing for runtime control operations.

use super::*;

/// Runs the runtime json rpc error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_rpc_error(
    id: &str,
    kind: crate::error::MezErrorKind,
    message: &str,
) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":{},"message":"{}","data":{{"mezzanine_code":"{}"}}}}}}"#,
        id,
        runtime_error_code(kind),
        json_escape(message),
        runtime_mezzanine_error_code(kind)
    )
}

/// Runs the runtime error code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_error_code(kind: crate::error::MezErrorKind) -> i32 {
    match kind {
        crate::error::MezErrorKind::InvalidArgs => -32602,
        crate::error::MezErrorKind::InvalidState => -32004,
        crate::error::MezErrorKind::Conflict => -32006,
        crate::error::MezErrorKind::NotFound => -32005,
        crate::error::MezErrorKind::Forbidden => -32002,
        crate::error::MezErrorKind::NotImplemented => -32601,
        crate::error::MezErrorKind::Config | crate::error::MezErrorKind::Io => -32000,
    }
}

/// Runs the runtime mezzanine error code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_mezzanine_error_code(
    kind: crate::error::MezErrorKind,
) -> &'static str {
    match kind {
        crate::error::MezErrorKind::InvalidArgs => "invalid_params",
        crate::error::MezErrorKind::InvalidState => "invalid_state",
        crate::error::MezErrorKind::Conflict => "conflict",
        crate::error::MezErrorKind::NotFound => "not_found",
        crate::error::MezErrorKind::Forbidden => "forbidden",
        crate::error::MezErrorKind::NotImplemented => "method_not_found",
        crate::error::MezErrorKind::Config | crate::error::MezErrorKind::Io => "internal_error",
    }
}

/// Runs the runtime json string field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_string_field(body: &str, field: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .as_object()?
        .get(field)?
        .as_str()
        .map(ToOwned::to_owned)
}

/// Runs the runtime json bool field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_bool_field(body: &str, field: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .as_object()?
        .get(field)?
        .as_bool()
}

/// Runs the runtime json creation command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_creation_command(body: &str) -> Result<Option<String>> {
    let value = runtime_json_value(body)?;
    let Some(command) = value
        .as_object()
        .and_then(|object| object.get("shell_command"))
    else {
        return Ok(None);
    };
    match command {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(command) => Ok(Some(command.clone())),
        serde_json::Value::Array(values) => {
            let mut argv = Vec::with_capacity(values.len());
            for value in values {
                let argument = value.as_str().ok_or_else(|| {
                    MezError::invalid_args("shell_command array must contain only strings")
                })?;
                argv.push(argument.to_string());
            }
            Ok(shell_command_from_argv(&argv).map(Some)?)
        }
        _ => Err(MezError::invalid_args(
            "shell_command must be a string, string array, or null",
        )),
    }
}

/// Runs the runtime json start directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_start_directory(body: &str) -> Result<Option<PathBuf>> {
    let value = runtime_json_value(body)?;
    let Some(start_directory) = value
        .as_object()
        .and_then(|object| object.get("start_directory"))
    else {
        return Ok(None);
    };
    match start_directory {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(path) if path.is_empty() => {
            Err(MezError::invalid_args("start_directory must not be empty"))
        }
        serde_json::Value::String(path) => Ok(Some(PathBuf::from(path))),
        _ => Err(MezError::invalid_args(
            "start_directory must be a string or null",
        )),
    }
}

/// Runs the runtime json optional size field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_optional_size_field(
    body: &str,
    field: &str,
) -> Result<Option<PaneSizeSpec>> {
    let value = runtime_json_value(body)?;
    let Some(size) = value.as_object().and_then(|object| object.get(field)) else {
        return Ok(None);
    };
    if size.is_null() {
        return Ok(None);
    }
    parse_runtime_size_spec(size, "pane size").map(Some)
}

/// Runs the runtime initialize terminal size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_initialize_terminal_size(
    request: &crate::control::JsonRpcRequest,
) -> Option<Size> {
    let params = request.params.as_ref()?;
    let value = serde_json::from_str::<serde_json::Value>(params).ok()?;
    let terminal = value
        .as_object()?
        .get("client")?
        .as_object()?
        .get("terminal")?
        .as_object()?;
    let columns = terminal.get("columns")?.as_u64()?;
    let rows = terminal.get("rows")?.as_u64()?;
    Size::new(u16::try_from(columns).ok()?, u16::try_from(rows).ok()?).ok()
}

/// Runs the runtime initialize requested primary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_initialize_requested_primary(
    request: &crate::control::JsonRpcRequest,
) -> bool {
    request
        .params
        .as_ref()
        .and_then(|params| serde_json::from_str::<serde_json::Value>(params).ok())
        .and_then(|value| {
            value
                .as_object()?
                .get("requested_role")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .as_deref()
        == Some("primary")
}

/// Returns whether one initialize request is asking for a pending observer role.
pub(in crate::runtime) fn runtime_initialize_requested_observer(
    request: &crate::control::JsonRpcRequest,
) -> bool {
    request
        .params
        .as_ref()
        .and_then(|params| serde_json::from_str::<serde_json::Value>(params).ok())
        .and_then(|value| {
            value
                .as_object()?
                .get("requested_role")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .as_deref()
        == Some("observer")
}

/// Runs the current unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Formats an elapsed duration for human-facing agent status lines.
pub(in crate::runtime) fn runtime_agent_turn_duration_display(elapsed_seconds: u64) -> String {
    let hours = elapsed_seconds / 3600;
    let minutes = (elapsed_seconds % 3600) / 60;
    let seconds = elapsed_seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// Returns the current Unix timestamp in milliseconds, saturating when the
/// host clock cannot fit the millisecond count into the runtime representation.
pub(in crate::runtime) fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Runs the runtime json size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_size(body: &str) -> Result<PaneSizeSpec> {
    let value = runtime_json_value(body)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("runtime control params must be an object"))?;
    let size = object
        .get("size")
        .ok_or_else(|| MezError::invalid_args("pane/resize requires size"))?;
    parse_runtime_size_spec(size, "pane size")
}

/// Runs the runtime json optional client size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_optional_client_size(body: &str) -> Result<Option<Size>> {
    let value = runtime_json_value(body)?;
    let Some(size) = value
        .as_object()
        .and_then(|object| object.get("client_size").or_else(|| object.get("size")))
    else {
        return Ok(None);
    };
    let size = size
        .as_object()
        .ok_or_else(|| MezError::invalid_args("terminal/step client_size must be an object"))?;
    let columns = size
        .get("columns")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args("terminal/step client_size requires columns"))?;
    let rows = size
        .get("rows")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args("terminal/step client_size requires rows"))?;
    let columns = u16::try_from(columns)
        .map_err(|_| MezError::invalid_args("terminal/step client_size columns is out of range"))?;
    let rows = u16::try_from(rows)
        .map_err(|_| MezError::invalid_args("terminal/step client_size rows is out of range"))?;
    Ok(Some(Size::new(columns, rows)?))
}

/// Runs the runtime json optional view offset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_optional_view_offset(
    body: &str,
) -> Result<Option<(usize, usize)>> {
    let value = runtime_json_value(body)?;
    let Some(offset) = value
        .as_object()
        .and_then(|object| object.get("view_offset").or_else(|| object.get("viewport")))
    else {
        return Ok(None);
    };
    let offset = offset
        .as_object()
        .ok_or_else(|| MezError::invalid_args("terminal/view view_offset must be an object"))?;
    let row = offset
        .get("row")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let column = offset
        .get("column")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let row = usize::try_from(row)
        .map_err(|_| MezError::invalid_args("terminal/view view_offset row is out of range"))?;
    let column = usize::try_from(column)
        .map_err(|_| MezError::invalid_args("terminal/view view_offset column is out of range"))?;
    Ok(Some((row, column)))
}

/// Runs the runtime json input bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_input_bytes(body: &str) -> Result<Vec<u8>> {
    let value = runtime_json_value(body)?;
    let Some(input) = value
        .as_object()
        .and_then(|object| object.get("input_bytes"))
    else {
        return Ok(Vec::new());
    };
    let input = input
        .as_array()
        .ok_or_else(|| MezError::invalid_args("terminal/step input_bytes must be an array"))?;
    input
        .iter()
        .map(|value| {
            let byte = value.as_u64().ok_or_else(|| {
                MezError::invalid_args("terminal/step input_bytes entries must be integers")
            })?;
            u8::try_from(byte).map_err(|_| {
                MezError::invalid_args("terminal/step input_bytes entries must be bytes")
            })
        })
        .collect()
}

/// Runs the runtime json value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_json_value(body: &str) -> Result<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(body).map_err(|error| {
        MezError::invalid_args(format!("runtime control params are invalid JSON: {error}"))
    })
}

/// Runs the parse runtime size spec operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_runtime_size_spec(value: &serde_json::Value, context: &str) -> Result<PaneSizeSpec> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{context} must be an object")))?;
    let mode = object
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args(format!("{context} requires mode")))?;
    match mode {
        "cells" => {
            let columns = optional_runtime_size_u16(object.get("columns"), "columns")?;
            let rows = optional_runtime_size_u16(object.get("rows"), "rows")?;
            if columns.is_none() && rows.is_none() {
                return Err(MezError::invalid_args(
                    "cells size requires columns or rows",
                ));
            }
            Ok(PaneSizeSpec::Cells { columns, rows })
        }
        "percent" => {
            let percent = required_runtime_size_u16(object.get("percent"), "percent")?;
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
            let amount = required_runtime_size_u16(object.get("amount"), "amount")?;
            Ok(PaneSizeSpec::Delta { direction, amount })
        }
        "edge" => {
            let edge = object
                .get("edge")
                .and_then(serde_json::Value::as_str)
                .and_then(ResizeDirection::from_name)
                .ok_or_else(|| MezError::invalid_args("edge size edge is invalid"))?;
            let amount = required_runtime_size_u16(object.get("amount"), "amount")?;
            Ok(PaneSizeSpec::Edge { edge, amount })
        }
        _ => Err(MezError::invalid_args("size mode is invalid")),
    }
}

/// Runs the optional runtime size u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_runtime_size_u16(
    value: Option<&serde_json::Value>,
    field: &'static str,
) -> Result<Option<u16>> {
    value
        .map(|value| required_runtime_size_u16(Some(value), field))
        .transpose()
}

/// Runs the required runtime size u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_runtime_size_u16(
    value: Option<&serde_json::Value>,
    field: &'static str,
) -> Result<u16> {
    let value = value
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args(format!("size {field} must be a number")))?;
    u16::try_from(value)
        .map_err(|_| MezError::invalid_args(format!("size {field} is out of range")))
}
