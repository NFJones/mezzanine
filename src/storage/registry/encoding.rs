//! Registry encoding and JSON reporting helpers.
//!
//! The session registry persists tab-separated records and exposes deterministic
//! JSON summaries without coupling encoding details to store operations.

use super::{
    MezError, Path, PathBuf, RegistrySessionState, Result, Session, SessionRecord, SessionState,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;

/// One session registry record paired with its derived creation-order alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecordIndexAlias<'a> {
    /// Registry record addressed by the alias.
    pub record: &'a SessionRecord,
    /// Stable display alias derived from creation order within the current
    /// registry snapshot.
    pub index_alias: String,
}

impl SessionRecord {
    /// Runs the from session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_session(
        session: &Session,
        socket_path: PathBuf,
        created_at_unix_seconds: u64,
        last_attach_at_unix_seconds: Option<u64>,
    ) -> Self {
        Self {
            session_id: session.id.to_string(),
            name: session.name.clone(),
            state: RegistrySessionState::from_session_state(session.state),
            socket_path,
            created_at_unix_seconds,
            last_attach_at_unix_seconds,
            window_count: session.windows().len(),
            client_count: session.clients().len(),
            primary_available: session.primary_client_id().is_none(),
            authoritative_columns: session.authoritative_size.columns,
            authoritative_rows: session.authoritative_size.rows,
        }
    }

    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate(&self) -> Result<()> {
        validate_non_empty("session_id", &self.session_id)?;
        validate_non_empty("name", &self.name)?;
        if !self.socket_path.is_absolute() {
            return Err(MezError::invalid_args(
                "session registry socket path must be absolute",
            ));
        }
        if self.socket_path.to_str().is_none() {
            return Err(MezError::invalid_args(
                "session registry socket path must be valid UTF-8",
            ));
        }
        if self.authoritative_columns == 0 || self.authoritative_rows == 0 {
            return Err(MezError::invalid_args(
                "session registry terminal dimensions must be non-zero",
            ));
        }
        Ok(())
    }

    /// Runs the to json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn to_json(&self) -> String {
        self.to_json_with_index_alias(None)
    }

    /// Serializes this record with an optional creation-order index alias.
    ///
    /// The alias is derived from the current registry listing and is not
    /// persisted with the record, so old registry files remain compatible.
    ///
    /// # Parameters
    /// - `index_alias`: Optional `$N` alias assigned to this record for display
    ///   and attach target resolution.
    fn to_json_with_index_alias(&self, index_alias: Option<&str>) -> String {
        let last_attach = self
            .last_attach_at_unix_seconds
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string());
        let index_alias = index_alias
            .map(|alias| format!(r#""{}""#, json_escape(alias)))
            .unwrap_or_else(|| "null".to_string());
        format!(
            "{{\"session_id\":\"{}\",\"index_alias\":{},\"name\":\"{}\",\"state\":\"{}\",\"socket_path\":\"{}\",\"created_at_unix_seconds\":{},\"last_attach_at_unix_seconds\":{},\"windows\":{},\"clients\":{},\"primary_available\":{},\"columns\":{},\"rows\":{}}}",
            json_escape(&self.session_id),
            index_alias,
            json_escape(&self.name),
            self.state.as_str(),
            json_escape(&self.socket_path.to_string_lossy()),
            self.created_at_unix_seconds,
            last_attach,
            self.window_count,
            self.client_count,
            self.primary_available,
            self.authoritative_columns,
            self.authoritative_rows
        )
    }

    /// Runs the encode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn encode(&self) -> Result<String> {
        self.validate()?;
        let socket_path = self
            .socket_path
            .to_str()
            .ok_or_else(|| MezError::invalid_args("socket path must be valid UTF-8"))?;
        let fields = [
            self.session_id.as_str().to_string(),
            self.name.as_str().to_string(),
            self.state.as_str().to_string(),
            socket_path.to_string(),
            self.created_at_unix_seconds.to_string(),
            self.last_attach_at_unix_seconds
                .map(|value| value.to_string())
                .unwrap_or_default(),
            self.window_count.to_string(),
            self.client_count.to_string(),
            self.primary_available.to_string(),
            self.authoritative_columns.to_string(),
            self.authoritative_rows.to_string(),
        ];
        Ok(fields
            .into_iter()
            .map(|field| encode_field(&field))
            .collect::<Vec<_>>()
            .join("\t"))
    }

    /// Runs the decode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn decode(line: &str) -> Result<Self> {
        let fields = split_escaped_fields(line)?;
        if fields.len() != 11 {
            return Err(MezError::invalid_args(
                "session registry record has the wrong field count",
            ));
        }

        let record = Self {
            session_id: fields[0].clone(),
            name: fields[1].clone(),
            state: RegistrySessionState::parse(&fields[2])?,
            socket_path: PathBuf::from(&fields[3]),
            created_at_unix_seconds: parse_u64(&fields[4], "created_at_unix_seconds")?,
            last_attach_at_unix_seconds: parse_optional_u64(
                &fields[5],
                "last_attach_at_unix_seconds",
            )?,
            window_count: parse_usize(&fields[6], "window_count")?,
            client_count: parse_usize(&fields[7], "client_count")?,
            primary_available: parse_bool(&fields[8], "primary_available")?,
            authoritative_columns: parse_u16(&fields[9], "authoritative_columns")?,
            authoritative_rows: parse_u16(&fields[10], "authoritative_rows")?,
        };
        record.validate()?;
        Ok(record)
    }
}

impl RegistrySessionState {
    /// Runs the from session state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_session_state(state: SessionState) -> Self {
        match state {
            SessionState::Running => Self::Running,
            SessionState::Detached => Self::Detached,
            SessionState::Empty => Self::Empty,
            SessionState::Stopping => Self::Stopping,
            SessionState::Failed => Self::Failed,
        }
    }

    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Detached => "detached",
            Self::Empty => "empty",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        }
    }

    /// Runs the parse operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "detached" => Ok(Self::Detached),
            "empty" => Ok(Self::Empty),
            "stopping" => Ok(Self::Stopping),
            "failed" => Ok(Self::Failed),
            _ => Err(MezError::invalid_args("unknown session registry state")),
        }
    }
}

/// Runs the records to json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn records_to_json(records: &[SessionRecord]) -> String {
    let aliases = session_record_index_aliases(records);
    let body = aliases
        .iter()
        .map(|alias| {
            alias
                .record
                .to_json_with_index_alias(Some(&alias.index_alias))
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{body}]")
}

/// Returns records paired with creation-order `$N` aliases.
///
/// Aliases are computed from the supplied registry snapshot rather than stored
/// in the registry file. Older sessions therefore gain aliases immediately
/// after upgrade, and deleting a session naturally compacts the next displayed
/// index order.
///
/// # Parameters
/// - `records`: Registry records to alias.
pub fn session_record_index_aliases(records: &[SessionRecord]) -> Vec<SessionRecordIndexAlias<'_>> {
    let mut aliases = records.iter().collect::<Vec<_>>();
    aliases.sort_by(|left, right| {
        left.created_at_unix_seconds
            .cmp(&right.created_at_unix_seconds)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    aliases
        .into_iter()
        .enumerate()
        .map(|(index, record)| SessionRecordIndexAlias {
            record,
            index_alias: format!("${}", index.saturating_add(1)),
        })
        .collect()
}

/// Resolves a user-provided session target against full ids and index aliases.
///
/// Exact session ids take precedence over aliases so a canonical id can never
/// be shadowed by a derived display alias. Bare decimal aliases are accepted as
/// a convenience for `mez attach 1`; displayed `$N` aliases are accepted as
/// written.
///
/// # Parameters
/// - `records`: Registry records to inspect.
/// - `target`: User-provided full session id, `$N` alias, or bare decimal
///   alias.
pub fn resolve_session_record_target<'a>(
    records: &'a [SessionRecord],
    target: &str,
) -> Option<&'a SessionRecord> {
    if let Some(record) = records.iter().find(|record| record.session_id == target) {
        return Some(record);
    }
    let alias_target = normalized_session_index_alias_target(target)?;
    session_record_index_aliases(records)
        .into_iter()
        .find(|alias| alias.index_alias == alias_target)
        .map(|alias| alias.record)
}

/// Normalizes `$N` and bare decimal session aliases into `$N` form.
fn normalized_session_index_alias_target(target: &str) -> Option<String> {
    let bare = target.strip_prefix('$').unwrap_or(target);
    (!bare.is_empty() && bare.chars().all(|character| character.is_ascii_digit()))
        .then(|| format!("${bare}"))
}

/// Runs the decode records operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn decode_records(data: &str) -> Result<Vec<SessionRecord>> {
    let mut records = Vec::new();
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        records.push(SessionRecord::decode(line)?);
    }
    Ok(records)
}

/// Runs the validate non empty operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        Err(MezError::invalid_args(format!(
            "session registry field `{field}` must not be empty"
        )))
    } else {
        Ok(())
    }
}

/// Runs the encode field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn encode_field(value: &str) -> String {
    let mut encoded = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            _ => encoded.push(ch),
        }
    }
    encoded
}

/// Runs the split escaped fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn split_escaped_fields(line: &str) -> Result<Vec<String>> {
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
                    .ok_or_else(|| MezError::invalid_args("trailing escape in registry field"))?;
                match escaped {
                    '\\' => field.push('\\'),
                    'n' => field.push('\n'),
                    'r' => field.push('\r'),
                    't' => field.push('\t'),
                    _ => {
                        return Err(MezError::invalid_args(
                            "unsupported escape in registry field",
                        ));
                    }
                }
            }
            _ => field.push(ch),
        }
    }

    fields.push(field);
    Ok(fields)
}

/// Runs the parse u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_u64(value: &str, field: &str) -> Result<u64> {
    value.parse::<u64>().map_err(|_| {
        MezError::invalid_args(format!(
            "session registry field `{field}` must be an integer"
        ))
    })
}

/// Runs the parse optional u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_optional_u64(value: &str, field: &str) -> Result<Option<u64>> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_u64(value, field).map(Some)
    }
}

/// Runs the parse usize operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_usize(value: &str, field: &str) -> Result<usize> {
    value.parse::<usize>().map_err(|_| {
        MezError::invalid_args(format!(
            "session registry field `{field}` must be an integer"
        ))
    })
}

/// Runs the parse u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_u16(value: &str, field: &str) -> Result<u16> {
    value.parse::<u16>().map_err(|_| {
        MezError::invalid_args(format!(
            "session registry field `{field}` must be an integer"
        ))
    })
}

/// Runs the parse bool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_bool(value: &str, field: &str) -> Result<bool> {
    value.parse::<bool>().map_err(|_| {
        MezError::invalid_args(format!(
            "session registry field `{field}` must be a boolean"
        ))
    })
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

/// Runs the set private file permissions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_private_file_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
