//! Config Mutation implementation.
//!
//! This module owns the config mutation boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    ConfigFormat, ConfigMutationOperation, ConfigMutationValue, MezError, Result,
    clean_key_segment, extract_config_values, extract_json_paths, extract_toml_paths,
    extract_yaml_paths, line_indent,
};
use crate::protocol::identifiers::is_ascii_identifier_segment;

const MUTABLE_MCP_EXTERNAL_CAPABILITY_KEYS: &[&str] = &[
    "purpose",
    "usage_instructions",
    "mutates_filesystem_outside_shell",
    "executes_processes_outside_shell",
    "accesses_credentials_outside_shell",
];

// TOML, YAML, and JSON mutation logic.

/// Runs the parse mutation path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_mutation_path(path: &str) -> Result<Vec<String>> {
    let segments = path.split('.').collect::<Vec<_>>();
    if segments.is_empty() || segments.iter().any(|segment| segment.trim().is_empty()) {
        return Err(MezError::config(
            "configuration mutation path must not be empty",
        ));
    }
    if segments
        .iter()
        .any(|segment| !is_ascii_identifier_segment(segment))
    {
        return Err(MezError::config(
            "configuration mutation path segments must be ASCII [A-Za-z0-9_-]",
        ));
    }
    Ok(segments.into_iter().map(str::to_string).collect())
}

/// Runs the reject unsupported mutation path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn reject_unsupported_mutation_path(segments: &[String]) -> Result<()> {
    let allow_nested_mcp_external_capability = segments.len() == 4
        && segments.first().map(String::as_str) == Some("mcp_servers")
        && segments.get(2).map(String::as_str) == Some("external_capability")
        && segments.get(3).is_some_and(|segment| {
            MUTABLE_MCP_EXTERNAL_CAPABILITY_KEYS.contains(&segment.as_str())
        });
    if segments.len() > 3 && !allow_nested_mcp_external_capability {
        return Err(MezError::config(
            "configuration mutation supports only scalar paths up to three segments except supported mcp_servers.<name>.external_capability scalar keys",
        ));
    }
    if segments.first().map(String::as_str) == Some("permissions")
        && segments.get(1).is_some_and(|segment| {
            matches!(
                segment.as_str(),
                "command_rules" | "session_command_rules" | "global_command_rules"
            )
        })
    {
        return Err(MezError::config(
            "configuration mutation cannot safely address entries in command rule arrays",
        ));
    }
    Ok(())
}

/// Runs the reject container target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn reject_container_target(
    format: ConfigFormat,
    text: &str,
    segments: &[String],
    operation: &ConfigMutationOperation,
) -> Result<()> {
    if matches!(operation, ConfigMutationOperation::Unset) {
        return Ok(());
    }
    let path = segments.join(".");
    let paths = match format {
        ConfigFormat::Toml => extract_toml_paths(text),
        ConfigFormat::Yaml => extract_yaml_paths(text),
        ConfigFormat::Json => extract_json_paths(text),
    };
    let values = extract_config_values(format, text);
    if paths.iter().any(|existing| existing == &path) && !values.contains_key(&path) {
        return Err(MezError::config(format!(
            "configuration mutation target `{path}` is a nested container, not a scalar"
        )));
    }
    Ok(())
}

/// Runs the mutate toml text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mutate_toml_text(
    text: &str,
    segments: &[String],
    operation: &ConfigMutationOperation,
) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    let leaf = segments.last().expect("path has at least one segment");
    let parent_segments = &segments[..segments.len().saturating_sub(1)];

    match operation {
        ConfigMutationOperation::Set(value) => {
            let parent = toml_parent_table_mut(document.as_table_mut(), parent_segments, true)?
                .expect("create=true returns a parent table");
            reject_toml_array_table_item(parent.get(leaf))?;
            parent.insert(leaf, toml_item_from_mutation_value(value));
        }
        ConfigMutationOperation::Unset => {
            if let Some(parent) =
                toml_parent_table_mut(document.as_table_mut(), parent_segments, false)?
            {
                reject_toml_array_table_item(parent.get(leaf))?;
                parent.remove(leaf);
            }
        }
    }

    Ok(document.to_string())
}

/// Runs the toml parent table mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn toml_parent_table_mut<'a>(
    table: &'a mut toml_edit::Table,
    segments: &[String],
    create: bool,
) -> Result<Option<&'a mut toml_edit::Table>> {
    let Some((segment, rest)) = segments.split_first() else {
        return Ok(Some(table));
    };

    if table.get(segment).is_none() {
        if !create {
            return Ok(None);
        }
        let mut child = toml_edit::Table::new();
        child.set_implicit(true);
        table.insert(segment, toml_edit::Item::Table(child));
    }

    let item = table
        .get_mut(segment)
        .ok_or_else(|| MezError::config("configuration mutation parent could not be created"))?;
    match item {
        toml_edit::Item::Table(child) => toml_parent_table_mut(child, rest, create),
        toml_edit::Item::ArrayOfTables(_) => Err(MezError::config(
            "configuration mutation cannot safely edit an array table entry",
        )),
        _ => Err(MezError::config(format!(
            "configuration mutation target `{}` is nested below a scalar",
            segments.join(".")
        ))),
    }
}

/// Runs the reject toml array table item operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn reject_toml_array_table_item(item: Option<&toml_edit::Item>) -> Result<()> {
    if matches!(item, Some(toml_edit::Item::ArrayOfTables(_))) {
        return Err(MezError::config(
            "configuration mutation cannot safely edit an array table entry",
        ));
    }
    Ok(())
}

/// Runs the toml item from mutation value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn toml_item_from_mutation_value(value: &ConfigMutationValue) -> toml_edit::Item {
    match value {
        ConfigMutationValue::String(value) => toml_edit::value(value.as_str()),
        ConfigMutationValue::Integer(value) => toml_edit::value(*value),
        ConfigMutationValue::Boolean(value) => toml_edit::value(*value),
        ConfigMutationValue::StringArray(values) => {
            let mut array = toml_edit::Array::default();
            for value in values {
                array.push(value.as_str());
            }
            toml_edit::value(array)
        }
    }
}

/// Runs the mutate yaml text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mutate_yaml_text(
    text: &str,
    segments: &[String],
    operation: &ConfigMutationOperation,
) -> Result<String> {
    let line_ending = detect_line_ending(text);
    let mut lines = split_lines_lossless(text);
    let records = yaml_records(&lines);
    let target = segments.join(".");
    let parent = segments[..segments.len().saturating_sub(1)].join(".");
    let leaf = segments.last().expect("path has at least one segment");

    if let Some(record) = records.iter().find(|record| record.path == target) {
        if !record.has_value {
            match operation {
                ConfigMutationOperation::Set(_) => {
                    return Err(MezError::config(format!(
                        "configuration mutation target `{target}` is a nested container, not a scalar"
                    )));
                }
                ConfigMutationOperation::Unset => {
                    let end = yaml_container_record_end(&lines, record.line);
                    lines.drain(record.line..end);
                    return Ok(join_lines(&lines, line_ending, text.ends_with('\n')));
                }
            }
        }
        match operation {
            ConfigMutationOperation::Set(value) => {
                let indent = line_indent(&lines[record.line]);
                lines[record.line] = format!("{indent}{leaf}: {}", yaml_scalar(value));
            }
            ConfigMutationOperation::Unset => {
                lines.remove(record.line);
            }
        }
        return Ok(join_lines(&lines, line_ending, text.ends_with('\n')));
    }

    if matches!(operation, ConfigMutationOperation::Unset) {
        return Ok(text.to_string());
    }

    let ConfigMutationOperation::Set(value) = operation else {
        unreachable!("unset returned above");
    };
    if parent.is_empty() {
        lines.push(format!("{leaf}: {}", yaml_scalar(value)));
        return Ok(join_lines(&lines, line_ending, text.ends_with('\n')));
    }
    let Some(parent_record) = records.iter().find(|record| record.path == parent) else {
        return Err(MezError::config(format!(
            "configuration mutation cannot create missing nested parent `{parent}`"
        )));
    };
    if parent_record.has_value {
        return Err(MezError::config(format!(
            "configuration mutation parent `{parent}` is a scalar, not a mapping"
        )));
    }

    let insert_at = yaml_block_end(&records, parent_record);
    let indent = " ".repeat(parent_record.indent + 2);
    lines.insert(insert_at, format!("{indent}{leaf}: {}", yaml_scalar(value)));
    Ok(join_lines(&lines, line_ending, text.ends_with('\n')))
}

/// Runs the yaml container record end operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn yaml_container_record_end(lines: &[String], start: usize) -> usize {
    let base_indent = line_indent(&lines[start]).len();
    let mut end = start + 1;
    while end < lines.len() {
        let line = &lines[end];
        if line.trim().is_empty() {
            end += 1;
            continue;
        }
        if line_indent(line).len() <= base_indent {
            break;
        }
        end += 1;
    }
    end
}

/// Carries Yaml Record state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct YamlRecord {
    /// Stores the path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) path: String,
    /// Stores the line value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) line: usize,
    /// Stores the indent value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) indent: usize,
    /// Stores the has value value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) has_value: bool,
}

/// Runs the yaml records operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn yaml_records(lines: &[String]) -> Vec<YamlRecord> {
    let mut records = Vec::new();
    let mut stack = Vec::<(usize, String)>::new();

    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.chars().take_while(|ch| ch.is_whitespace()).count();
        let trimmed = line.trim_start();
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        if key.starts_with('-') {
            continue;
        }

        while stack
            .last()
            .is_some_and(|(existing, _)| *existing >= indent)
        {
            stack.pop();
        }
        stack.push((indent, clean_key_segment(key)));
        records.push(YamlRecord {
            path: stack
                .iter()
                .map(|(_, key)| key.as_str())
                .collect::<Vec<_>>()
                .join("."),
            line: index,
            indent,
            has_value: !value.trim().is_empty(),
        });
    }

    records
}

/// Runs the yaml block end operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn yaml_block_end(records: &[YamlRecord], parent: &YamlRecord) -> usize {
    records
        .iter()
        .filter(|record| record.line > parent.line && record.indent <= parent.indent)
        .map(|record| record.line)
        .next()
        .unwrap_or_else(|| records.last().map(|record| record.line + 1).unwrap_or(0))
}

/// Runs the mutate json text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mutate_json_text(
    text: &str,
    segments: &[String],
    operation: &ConfigMutationOperation,
) -> Result<String> {
    let mut root: serde_json::Value = serde_json::from_str(text).map_err(|error| {
        MezError::config(format!("JSON configuration mutation parse failed: {error}"))
    })?;
    if !root.is_object() {
        return Err(MezError::config(
            "JSON configuration mutation requires an object root",
        ));
    }
    json_mutate_value(&mut root, segments, operation)?;
    serde_json::to_string_pretty(&root)
        .map(|rendered| format!("{rendered}\n"))
        .map_err(|error| {
            MezError::config(format!(
                "JSON configuration mutation render failed: {error}"
            ))
        })
}

/// Runs the json mutate value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_mutate_value(
    root: &mut serde_json::Value,
    segments: &[String],
    operation: &ConfigMutationOperation,
) -> Result<()> {
    let parent_segments = &segments[..segments.len().saturating_sub(1)];
    let leaf = segments.last().expect("path has at least one segment");
    let mut node = root;
    for segment in parent_segments {
        let Some(values) = node.as_object_mut() else {
            return Err(MezError::config(format!(
                "configuration mutation parent `{segment}` is a scalar, not an object"
            )));
        };
        let Some(next) = values.get_mut(segment) else {
            return Err(MezError::config(format!(
                "configuration mutation cannot create missing nested parent `{segment}`"
            )));
        };
        node = next;
    }

    let Some(values) = node.as_object_mut() else {
        return Err(MezError::config(
            "configuration mutation parent is a scalar, not an object",
        ));
    };
    match operation {
        ConfigMutationOperation::Set(value) => {
            if values.get(leaf).is_some_and(serde_json::Value::is_object) {
                return Err(MezError::config(format!(
                    "configuration mutation target `{}` is a nested container, not a scalar",
                    segments.join(".")
                )));
            }
            values.insert(leaf.clone(), json_value_from_mutation_value(value));
        }
        ConfigMutationOperation::Unset => {
            values.remove(leaf);
        }
    }
    Ok(())
}

/// Runs the json value from mutation value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_value_from_mutation_value(value: &ConfigMutationValue) -> serde_json::Value {
    match value {
        ConfigMutationValue::String(value) => serde_json::Value::String(value.clone()),
        ConfigMutationValue::Integer(value) => serde_json::Value::Number((*value).into()),
        ConfigMutationValue::Boolean(value) => serde_json::Value::Bool(*value),
        ConfigMutationValue::StringArray(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| serde_json::Value::String(value.clone()))
                .collect(),
        ),
    }
}

/// Runs the yaml scalar operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn yaml_scalar(value: &ConfigMutationValue) -> String {
    match value {
        ConfigMutationValue::String(value) => format!("\"{}\"", json_escape(value)),
        ConfigMutationValue::Integer(value) => value.to_string(),
        ConfigMutationValue::Boolean(value) => value.to_string(),
        ConfigMutationValue::StringArray(values) => {
            let rendered = values
                .iter()
                .map(|value| format!("\"{}\"", json_escape(value)))
                .collect::<Vec<_>>();
            format!("[{}]", rendered.join(", "))
        }
    }
}

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect()
}

/// Runs the detect line ending operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn detect_line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Runs the split lines lossless operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_lines_lossless(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

/// Runs the join lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn join_lines(lines: &[String], line_ending: &str, trailing_newline: bool) -> String {
    let mut text = lines.join(line_ending);
    if trailing_newline || !lines.is_empty() {
        text.push_str(line_ending);
    }
    text
}
