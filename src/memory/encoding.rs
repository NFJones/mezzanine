//! Line-oriented encoding helpers for memory records.
//!
//! Session and persistent stores share these helpers so escaping, source names,
//! scopes, and primitive parsing stay consistent.

use super::{MemoryScope, MemorySource, MezError, Result};

/// Runs the encode scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn encode_scope(scope: &MemoryScope) -> String {
    match scope {
        MemoryScope::Global => "global".to_string(),
        MemoryScope::Project { root } => format!("project:{}", escape_component(root)),
        MemoryScope::Session { session_id } => format!("session:{}", escape_component(session_id)),
        MemoryScope::Window {
            session_id,
            window_id,
        } => format!(
            "window:{}:{}",
            escape_component(session_id),
            escape_component(window_id)
        ),
        MemoryScope::Pane {
            session_id,
            pane_id,
        } => format!(
            "pane:{}:{}",
            escape_component(session_id),
            escape_component(pane_id)
        ),
        MemoryScope::Agent {
            session_id,
            agent_id,
        } => format!(
            "agent:{}:{}",
            escape_component(session_id),
            escape_component(agent_id)
        ),
    }
}

/// Runs the decode scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn decode_scope(encoded: &str) -> Result<MemoryScope> {
    let parts = split_components(encoded)?;
    match parts.as_slice() {
        [kind] if kind == "global" => Ok(MemoryScope::Global),
        [kind, root] if kind == "project" => Ok(MemoryScope::Project { root: root.clone() }),
        [kind, session_id] if kind == "session" => Ok(MemoryScope::Session {
            session_id: session_id.clone(),
        }),
        [kind, session_id, window_id] if kind == "window" => Ok(MemoryScope::Window {
            session_id: session_id.clone(),
            window_id: window_id.clone(),
        }),
        [kind, session_id, pane_id] if kind == "pane" => Ok(MemoryScope::Pane {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
        }),
        [kind, session_id, agent_id] if kind == "agent" => Ok(MemoryScope::Agent {
            session_id: session_id.clone(),
            agent_id: agent_id.clone(),
        }),
        _ => Err(MezError::invalid_args("unknown memory scope")),
    }
}

/// Runs the source name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn source_name(source: MemorySource) -> &'static str {
    match source {
        MemorySource::User => "user",
        MemorySource::Agent => "agent",
        MemorySource::Imported => "imported",
        MemorySource::Configuration => "configuration",
    }
}

/// Runs the parse source operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_source(value: &str) -> Result<MemorySource> {
    match value {
        "user" => Ok(MemorySource::User),
        "agent" => Ok(MemorySource::Agent),
        "imported" => Ok(MemorySource::Imported),
        "configuration" => Ok(MemorySource::Configuration),
        _ => Err(MezError::invalid_args("unknown memory source")),
    }
}

/// Runs the parse u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_u64(value: &str, label: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| MezError::invalid_args(format!("invalid {label}")))
}

/// Runs the parse bool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_bool(value: &str) -> Result<bool> {
    value
        .parse::<bool>()
        .map_err(|_| MezError::invalid_args("invalid memory boolean"))
}

/// Runs the escape field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn escape_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Runs the split fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_fields(line: &str) -> Result<Vec<String>> {
    split_escaped(line, '\t')
}

/// Runs the escape component operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn escape_component(value: &str) -> String {
    value.replace('\\', "\\\\").replace(':', "\\:")
}

/// Runs the split components operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_components(value: &str) -> Result<Vec<String>> {
    split_escaped(value, ':')
}

/// Runs the split escaped operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_escaped(value: &str, separator: char) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == separator {
            fields.push(field);
            field = String::new();
            continue;
        }
        if ch != '\\' {
            field.push(ch);
            continue;
        }
        let escaped = chars
            .next()
            .ok_or_else(|| MezError::invalid_args("trailing memory escape"))?;
        match escaped {
            '\\' => field.push('\\'),
            ':' if separator == ':' => field.push(':'),
            't' if separator == '\t' => field.push('\t'),
            'n' if separator == '\t' => field.push('\n'),
            'r' if separator == '\t' => field.push('\r'),
            _ => return Err(MezError::invalid_args("unsupported memory escape")),
        }
    }
    fields.push(field);
    Ok(fields)
}
