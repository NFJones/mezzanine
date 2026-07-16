//! Control Json implementation.
//!
//! This module owns the control json boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{MezError, Result, SystemTime, UNIX_EPOCH, geteuid};

// Local JSON and time helpers.

/// Runs the json rpc success operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_rpc_success(id: &str, result: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#, id)
}

/// Runs the json rpc error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_rpc_error(id: &str, code: i32, message: &str, mezzanine_code: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":{},"message":"{}","data":{{"mezzanine_code":"{}"}}}}}}"#,
        id,
        code,
        json_escape(message),
        mezzanine_code
    )
}

/// Runs the error code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn error_code(kind: crate::error::MezErrorKind) -> i32 {
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

/// Runs the mezzanine error code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mezzanine_error_code(kind: crate::error::MezErrorKind) -> &'static str {
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

/// Runs the reject unknown json fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn reject_unknown_json_fields(
    body: &str,
    context: &str,
    allowed_fields: &[&str],
) -> Result<()> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MezError::invalid_args(format!("{context} must be a JSON object")))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{context} must be a JSON object")))?;
    for (field, value) in object {
        if field == "extensions" {
            if !value.is_object() {
                return Err(MezError::invalid_args(format!(
                    "{context} extensions must be an object"
                )));
            }
            continue;
        }
        if !allowed_fields.contains(&field.as_str()) {
            return Err(MezError::invalid_args(format!(
                "{context} contains unknown field `{field}`"
            )));
        }
    }
    Ok(())
}

/// Runs the json string field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_string_field(body: &str, field: &str) -> Option<String> {
    let value = field_value(body, field)?;
    let value = value.trim_start();
    if !value.starts_with('"') {
        return None;
    }
    Some(parse_json_string(value))
}

/// Runs the json bool field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_bool_field(body: &str, field: &str) -> Option<bool> {
    let value = field_value(body, field)?.trim_start();
    if value.starts_with("true") {
        Some(true)
    } else if value.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Runs the json null field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_null_field(body: &str, field: &str) -> bool {
    field_value(body, field)
        .map(|value| value.trim_start().starts_with("null"))
        .unwrap_or(false)
}

/// Runs the json raw field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_raw_field(body: &str, field: &str) -> Option<String> {
    let value = field_value(body, field)?.trim_start();
    if value.starts_with('{') {
        return Some(take_balanced(value, '{', '}'));
    }
    if value.starts_with('[') {
        return Some(take_balanced(value, '[', ']'));
    }
    if value.starts_with('"') {
        return take_json_string_literal(value).map(str::to_string);
    }
    let scalar = value
        .chars()
        .take_while(|ch| !matches!(ch, ',' | '}' | ']'))
        .collect::<String>();
    let scalar = scalar.trim();
    if scalar.is_empty() {
        None
    } else {
        Some(scalar.to_string())
    }
}

/// Runs the json object field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_object_field(body: &str, field: &str) -> Option<String> {
    let value = field_value(body, field)?.trim_start();
    if !value.starts_with('{') {
        return None;
    }
    Some(take_balanced(value, '{', '}'))
}

/// Runs the json array field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_array_field(body: &str, field: &str) -> Option<String> {
    let value = field_value(body, field)?.trim_start();
    if !value.starts_with('[') {
        return None;
    }
    Some(take_balanced(value, '[', ']'))
}

/// Runs the json string array field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_string_array_field(body: &str, field: &str) -> Result<Option<Vec<String>>> {
    if field_value(body, field).is_none() || json_null_field(body, field) {
        return Ok(None);
    }
    let array = json_array_field(body, field)
        .ok_or_else(|| MezError::invalid_args(format!("{field} must be an array or null")))?;
    let inner = array
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| MezError::invalid_args(format!("{field} must be an array")))?;
    let mut values = Vec::new();
    let mut rest = inner.trim_start();
    while !rest.is_empty() {
        if rest.starts_with(',') {
            rest = rest[1..].trim_start();
            continue;
        }
        if !rest.starts_with('"') {
            return Err(MezError::invalid_args(format!(
                "{field} must contain only strings"
            )));
        }
        let literal = take_json_string_literal(rest)
            .ok_or_else(|| MezError::invalid_args(format!("{field} contains an invalid string")))?;
        values.push(parse_json_string(literal));
        rest = rest[literal.len()..].trim_start();
        if rest.starts_with(',') {
            rest = rest[1..].trim_start();
        } else if !rest.is_empty() {
            return Err(MezError::invalid_args(format!("{field} array is invalid")));
        }
    }
    Ok(Some(values))
}

/// Runs the field value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn field_value<'a>(body: &'a str, field: &str) -> Option<&'a str> {
    let needle = format!(r#""{field}""#);
    let field_start = body.find(&needle)?;
    let after_field = &body[field_start + needle.len()..];
    let colon = after_field.find(':')?;
    Some(&after_field[colon + 1..])
}

/// Runs the parse json string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_json_string(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars();
    if chars.next() != Some('"') {
        return output;
    }
    let mut escaped = false;
    for ch in chars {
        if escaped {
            output.push(match ch {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => break,
            _ => output.push(ch),
        }
    }
    output
}

/// Runs the take json string literal operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn take_json_string_literal(value: &str) -> Option<&str> {
    if !value.starts_with('"') {
        return None;
    }
    let mut escaped = false;
    for (index, ch) in value.char_indices().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(&value[..=index]),
            _ => {}
        }
    }
    None
}

/// Runs the take balanced operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn take_balanced(value: &str, open: char, close: char) -> String {
    let mut output = String::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for ch in value.chars() {
        output.push(ch);
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            ch if ch == open => depth += 1,
            ch if ch == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    output
}

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the current rfc3339 seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn current_rfc3339_seconds() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    unix_seconds_to_rfc3339(seconds)
}

/// Runs the unix seconds to rfc3339 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn unix_seconds_to_rfc3339(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Runs the civil from days operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn civil_from_days(days_since_unix_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

/// Runs the effective uid operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn effective_uid() -> u32 {
    geteuid().as_raw()
}
