//! Project trust data types.
//!
//! These types model trust decisions, overlay prompts, and the in-memory trust
//! database without performing filesystem discovery or persistence.

use super::{BTreeMap, PathBuf};

/// Defines the OVERLAY FILENAMES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const OVERLAY_FILENAMES: &[&str] = &[
    ".mezzanine/config.toml",
    ".mezzanine/config.yaml",
    ".mezzanine/config.yml",
    ".mezzanine/config.json",
];

/// Carries Trust Decision state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// Represents the Pending case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pending,
    /// Represents the Trusted case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Trusted,
    /// Represents the Rejected case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Rejected,
    /// Represents the Revoked case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Revoked,
}

/// Carries Project Trust Record state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustRecord {
    /// Stores the project root value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub project_root: PathBuf,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: TrustDecision,
    /// Stores the git marker path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub git_marker_path: Option<PathBuf>,
    /// Stores the trusted at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub trusted_at_unix_seconds: u64,
    /// Stores the decided by client id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub decided_by_client_id: Option<String>,
    /// Stores the trust policy version value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub trust_policy_version: u32,
    /// Stores the configuration schema version value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub configuration_schema_version: u32,
    /// Stores the vcs remote value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub vcs_remote: Option<String>,
}

/// Carries Project Trust Prompt state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct ProjectTrustPrompt {
    /// Stores the project root value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub project_root: PathBuf,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: TrustDecision,
    /// Stores the overlay files value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub overlay_files: Vec<PathBuf>,
    /// Stores the capability expansion summary value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub capability_expansion_summary: Vec<String>,
    /// Stores the blocks until primary decision value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub blocks_until_primary_decision: bool,
}

/// Carries Project Trust Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
pub struct ProjectTrustStore {
    /// Stores the records value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) records: BTreeMap<PathBuf, ProjectTrustRecord>,
}
