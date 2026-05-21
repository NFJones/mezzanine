//! Parser for escaped instruction discovery records.
//!
//! The shell command emits tab-separated key-value records with explicit escapes
//! for tabs, newlines, carriage returns, and backslashes. This parser preserves
//! that minimal format without coupling discovery to local filesystem access.

use crate::error::{MezError, Result};

use super::types::DiscoveredInstructionFile;

/// Parses escaped discovery command output into instruction files.
///
/// Empty lines are ignored. Records are sorted from parent scopes to deeper
/// child scopes so callers can apply broad guidance before narrow guidance.
pub fn parse_instruction_discovery_output(output: &str) -> Result<Vec<DiscoveredInstructionFile>> {
    let mut files = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_instruction_line)
        .collect::<Result<Vec<DiscoveredInstructionFile>>>()?;
    files.sort_by_key(|file| instruction_scope_depth(&file.scope_root));
    Ok(files)
}

/// Runs the parse instruction line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_instruction_line(line: &str) -> Result<DiscoveredInstructionFile> {
    let fields = split_fields(line)?;
    let mut path = None;
    let mut scope = None;
    let mut bytes = None;
    let mut truncated = None;
    let mut content = None;
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            return Err(MezError::invalid_args(
                "instruction record field is malformed",
            ));
        };
        match key {
            "path" => path = Some(value.to_string()),
            "scope" => scope = Some(value.to_string()),
            "bytes" => {
                bytes = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| MezError::invalid_args("invalid instruction byte count"))?,
                );
            }
            "truncated" => {
                truncated =
                    Some(value.parse::<bool>().map_err(|_| {
                        MezError::invalid_args("invalid instruction truncated flag")
                    })?);
            }
            "content" => content = Some(value.to_string()),
            _ => {}
        }
    }
    Ok(DiscoveredInstructionFile {
        path: path.ok_or_else(|| MezError::invalid_args("instruction path missing"))?,
        scope_root: scope.ok_or_else(|| MezError::invalid_args("instruction scope missing"))?,
        bytes: bytes.ok_or_else(|| MezError::invalid_args("instruction bytes missing"))?,
        truncated: truncated
            .ok_or_else(|| MezError::invalid_args("instruction truncated flag missing"))?,
        content: content.ok_or_else(|| MezError::invalid_args("instruction content missing"))?,
    })
}

/// Runs the split fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn split_fields(line: &str) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\t' => {
                fields.push(field);
                field = String::new();
            }
            '\\' => {
                let escaped = chars
                    .next()
                    .ok_or_else(|| MezError::invalid_args("trailing instruction escape"))?;
                field.push(match escaped {
                    '\\' => '\\',
                    't' => '\t',
                    'n' => '\n',
                    'r' => '\r',
                    _ => return Err(MezError::invalid_args("unsupported instruction escape")),
                });
            }
            _ => field.push(ch),
        }
    }
    fields.push(field);
    Ok(fields)
}

/// Runs the instruction scope depth operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn instruction_scope_depth(scope: &str) -> usize {
    if scope == "." {
        return 0;
    }
    scope
        .trim_start_matches("./")
        .split('/')
        .filter(|part| !part.is_empty())
        .count()
}
