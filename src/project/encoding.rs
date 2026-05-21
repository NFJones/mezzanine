//! Project trust record encoding and filesystem permission helpers.
//!
//! The store persists a small line-oriented format; this module owns parsing,
//! escaping, canonicalization, timestamps, and private file permissions.

use super::{MezError, Path, PathBuf, ProjectTrustRecord, Result, TrustDecision, fs};
use std::time::{SystemTime, UNIX_EPOCH};

impl TrustDecision {
    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Trusted => "trusted",
            Self::Rejected => "rejected",
            Self::Revoked => "revoked",
        }
    }

    /// Runs the parse operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "trusted" => Ok(Self::Trusted),
            "rejected" => Ok(Self::Rejected),
            "revoked" => Ok(Self::Revoked),
            _ => Err(MezError::config("unknown project trust decision")),
        }
    }
}

impl ProjectTrustRecord {
    /// Runs the to line operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn to_line(&self) -> String {
        [
            escape_field(&self.project_root.to_string_lossy()),
            self.state.as_str().to_string(),
            escape_field(
                &self
                    .git_marker_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_default(),
            ),
            self.trusted_at_unix_seconds.to_string(),
            escape_field(self.decided_by_client_id.as_deref().unwrap_or_default()),
            self.trust_policy_version.to_string(),
            self.configuration_schema_version.to_string(),
            escape_field(self.vcs_remote.as_deref().unwrap_or_default()),
        ]
        .join("\t")
    }

    /// Runs the from fields operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn from_fields(fields: &[String]) -> Result<Self> {
        if fields.len() != 7 && fields.len() != 8 {
            return Err(MezError::config(
                "project trust record has an unsupported field count",
            ));
        }
        let has_decision_client = fields.len() == 8;
        let decided_by_client_id = if has_decision_client && !fields[4].is_empty() {
            Some(fields[4].clone())
        } else {
            None
        };
        let trust_policy_version_index = if has_decision_client { 5 } else { 4 };
        let configuration_schema_version_index = if has_decision_client { 6 } else { 5 };
        let vcs_remote_index = if has_decision_client { 7 } else { 6 };
        let project_root = PathBuf::from(&fields[0]);
        let git_marker_path = if fields[2].is_empty() {
            None
        } else {
            Some(PathBuf::from(&fields[2]))
        };
        let vcs_remote = if fields[vcs_remote_index].is_empty() {
            None
        } else {
            Some(fields[vcs_remote_index].clone())
        };

        Ok(Self {
            project_root: project_root.clone(),
            state: TrustDecision::parse(&fields[1])?,
            git_marker_path,
            trusted_at_unix_seconds: parse_u64(&fields[3])?,
            decided_by_client_id,
            trust_policy_version: parse_u32(&fields[trust_policy_version_index])?,
            configuration_schema_version: parse_u32(&fields[configuration_schema_version_index])?,
            vcs_remote,
        })
    }
}

/// Runs the canonicalize existing or original operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn canonicalize_existing_or_original(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

/// Runs the unix now seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn unix_now_seconds() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(_) => 0,
    }
}

/// Runs the parse u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_u64(value: &str) -> Result<u64> {
    value
        .parse()
        .map_err(|_| MezError::config("invalid integer in project trust record"))
}

/// Runs the parse u32 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_u32(value: &str) -> Result<u32> {
    value
        .parse()
        .map_err(|_| MezError::config("invalid integer in project trust record"))
}

/// Runs the escape field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn escape_field(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the parse record line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_record_line(line: &str) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            match ch {
                't' => current.push('\t'),
                'n' => current.push('\n'),
                '\\' => current.push('\\'),
                _ => return Err(MezError::config("invalid escape in project trust record")),
            }
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '\t' => fields.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    if escaped {
        return Err(MezError::config(
            "unterminated escape in project trust record",
        ));
    }
    fields.push(current);
    Ok(fields)
}

/// Runs the set private file permissions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}
