//! Cli Json implementation.
//!
//! This module owns the cli json boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{MezError, Result, Serialize, SystemTime, UNIX_EPOCH, Write};

// Shared CLI JSON and user-facing output helpers.

/// Carries Cli Output Format state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CliOutputFormat {
    /// Represents the Plain case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Plain,
    /// Represents the Json case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Json,
}

impl CliOutputFormat {
    /// Runs the from json flag operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn from_json_flag(json: bool) -> Self {
        if json { Self::Json } else { Self::Plain }
    }

    /// Runs the is json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn is_json(self) -> bool {
        self == Self::Json
    }
}

/// Runs the current unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn current_unix_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| MezError::invalid_state("system clock is before Unix epoch"))?
        .as_secs())
}

/// Runs the json optional operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_optional(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

/// Carries Diagnostic Json state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Serialize)]
pub(super) struct DiagnosticJson<'a> {
    /// Stores the path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) path: &'a str,
    /// Stores the message value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) message: &'a str,
}

/// Runs the diagnostics json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn diagnostics_json(diagnostics: &[crate::config::ConfigDiagnostic]) -> Result<String> {
    let diagnostics = diagnostics
        .iter()
        .map(|diagnostic| DiagnosticJson {
            path: &diagnostic.path,
            message: &diagnostic.message,
        })
        .collect::<Vec<_>>();
    serialize_json(&diagnostics)
}

/// Runs the serialize json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn serialize_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|error| MezError::invalid_state(format!("failed to serialize JSON: {error}")))
}

/// Runs the write json or plain operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn write_json_or_plain<W: Write>(
    stdout: &mut W,
    output_format: CliOutputFormat,
    json: &str,
) -> Result<()> {
    if output_format.is_json() {
        writeln!(stdout, "{json}")?;
        return Ok(());
    }
    write!(stdout, "{}", json_to_plain_text(json)?)?;
    Ok(())
}

/// Runs the write control response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn write_control_response<W: Write>(
    stdout: &mut W,
    output_format: CliOutputFormat,
    body: &str,
) -> Result<()> {
    if output_format.is_json() {
        writeln!(stdout, "{body}")?;
        return Ok(());
    }
    write!(stdout, "{}", control_response_to_plain_text(body)?)?;
    Ok(())
}

/// Runs the control response to plain text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn control_response_to_plain_text(body: &str) -> Result<String> {
    let value = parse_json_value(body)?;
    if let Some(error) = value.get("error") {
        return json_value_to_plain_text(error);
    }
    if let Some(result) = value.get("result") {
        return json_value_to_plain_text(result);
    }
    json_value_to_plain_text(&value)
}

/// Runs the json to plain text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn json_to_plain_text(json: &str) -> Result<String> {
    json_value_to_plain_text(&parse_json_value(json)?)
}

/// Runs the parse json value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_json_value(json: &str) -> Result<serde_json::Value> {
    serde_json::from_str(json).map_err(|error| {
        MezError::invalid_state(format!("failed to parse CLI JSON output: {error}"))
    })
}

/// Runs the json value to plain text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn json_value_to_plain_text(value: &serde_json::Value) -> Result<String> {
    let mut output = String::new();
    write_plain_json_value(&mut output, None, value, 0);
    if !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

/// Runs the write plain json value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn write_plain_json_value(
    output: &mut String,
    label: Option<&str>,
    value: &serde_json::Value,
    indent: usize,
) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(label) = label {
                push_indent(output, indent);
                output.push_str(label);
                output.push_str(":\n");
            }
            if object.is_empty() {
                push_indent(output, indent + label_indent(label));
                output.push_str("(none)\n");
                return;
            }
            let child_indent = indent + label_indent(label);
            for (key, value) in object {
                write_plain_json_value(output, Some(key), value, child_indent);
            }
        }
        serde_json::Value::Array(values) => {
            if let Some(label) = label {
                push_indent(output, indent);
                output.push_str(label);
                output.push_str(":\n");
            }
            let child_indent = indent + label_indent(label);
            if values.is_empty() {
                push_indent(output, child_indent);
                output.push_str("(none)\n");
                return;
            }
            for value in values {
                match value {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        push_indent(output, child_indent);
                        output.push_str("-\n");
                        write_plain_json_value(output, None, value, child_indent + 2);
                    }
                    _ => {
                        push_indent(output, child_indent);
                        output.push_str("- ");
                        output.push_str(&plain_scalar(value));
                        output.push('\n');
                    }
                }
            }
        }
        _ => {
            push_indent(output, indent);
            if let Some(label) = label {
                output.push_str(label);
                output.push_str(": ");
            }
            output.push_str(&plain_scalar(value));
            output.push('\n');
        }
    }
}

/// Runs the label indent operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn label_indent(label: Option<&str>) -> usize {
    if label.is_some() { 2 } else { 0 }
}

/// Runs the push indent operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn push_indent(output: &mut String, indent: usize) {
    output.extend(std::iter::repeat_n(' ', indent));
}

/// Runs the plain scalar operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn plain_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "none".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => value.to_string(),
    }
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
