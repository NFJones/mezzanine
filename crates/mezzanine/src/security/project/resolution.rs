//! Deepest project-trust decision resolution.
//!
//! Project-trust records are keyed by canonical project root and may hold an
//! explicit trust, rejection, revocation, or an externally persisted pending
//! decision. Repository-marker discovery finds the nearest repository but never
//! grants or withholds authority by itself. Implicit authority (the
//! trusted-project filesystem default, project skills, project macros, and
//! project-overlay application) therefore derives from the deepest stored
//! decision that governs a directory, so a nested rejection or revocation can
//! never be shadowed by a broader trusted ancestor.
//!
//! Only records written under the current trust policy and configuration schema
//! count as a stored decision, matching the stricter record lookup so a stale
//! schema cannot grant or withhold implicit authority.

use super::{
    Path, PathBuf, ProjectTrustRecord, ProjectTrustStore, TrustDecision,
    canonicalize_existing_or_original,
};

/// Typed provenance for the deepest stored trust decision governing one path.
///
/// Callers grant implicit authority only for [`Self::TrustedRoot`]. The other
/// variants exist so a withheld decision is reported distinctly from a path
/// that simply has no stored decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectTrustProvenance {
    /// The deepest stored decision explicitly trusts this canonical root.
    TrustedRoot(PathBuf),
    /// The deepest stored decision rejects or revokes this canonical root.
    NegativeDecision {
        /// Canonical root of the withholding decision.
        root: PathBuf,
        /// Explicit rejection or revocation state.
        state: TrustDecision,
    },
    /// The deepest stored decision is an explicit pending decision.
    PendingDecision {
        /// Canonical root whose decision is still pending.
        root: PathBuf,
    },
    /// No stored decision governs the path.
    NoDecision,
}

impl ProjectTrustProvenance {
    /// Returns the canonical trusted root when implicit authority is granted.
    pub fn trusted_root(&self) -> Option<&Path> {
        match self {
            Self::TrustedRoot(root) => Some(root.as_path()),
            _ => None,
        }
    }

    /// Returns the canonical root and explicit state for a withholding decision.
    pub fn negative_decision(&self) -> Option<(&Path, TrustDecision)> {
        match self {
            Self::NegativeDecision { root, state } => Some((root.as_path(), *state)),
            _ => None,
        }
    }

    /// Returns the stable provenance name for a withheld decision.
    ///
    /// The names distinguish an explicit rejection or revocation from a pending
    /// decision and from the absence of any decision, which returns `None`.
    pub fn withheld_provenance(&self) -> Option<&'static str> {
        match self {
            Self::NegativeDecision {
                state: TrustDecision::Revoked,
                ..
            } => Some("project-trust-revoked"),
            Self::NegativeDecision { .. } => Some("project-trust-rejected"),
            Self::PendingDecision { .. } => Some("project-trust-pending"),
            Self::TrustedRoot(_) | Self::NoDecision => None,
        }
    }

    /// Returns the canonical root that governs the decision, when one exists.
    pub fn governing_root(&self) -> Option<&Path> {
        match self {
            Self::TrustedRoot(root)
            | Self::NegativeDecision { root, .. }
            | Self::PendingDecision { root } => Some(root.as_path()),
            Self::NoDecision => None,
        }
    }
}

/// Resolves the deepest stored project-trust decision governing one directory.
///
/// The resolver compares every stored record against the canonical working
/// directory, filters by the canonical root, and keeps the deepest canonical
/// match, so a nested decision always wins over a broader ancestor even when a
/// record stores a non-canonical root. Symlinked working directories and stored
/// roots are therefore filtered and ranked on the same canonical paths, while
/// the returned root stays the original stored record identity. Records written
/// under another trust policy or configuration schema are ignored, and a
/// directory that merely contains a nested repository marker but no deeper
/// stored decision keeps its recursive parent trust because no record is
/// invented here.
pub fn resolve_project_trust_provenance(
    store: &ProjectTrustStore,
    working_directory: &Path,
) -> ProjectTrustProvenance {
    let canonical_working_directory =
        canonicalize_existing_or_original(working_directory.to_path_buf());
    let mut deepest: Option<(PathBuf, &ProjectTrustRecord)> = None;
    for record in store.records_matching_current_versions() {
        let canonical_root = canonicalize_existing_or_original(record.project_root.clone());
        if !canonical_working_directory.starts_with(&canonical_root) {
            continue;
        }
        let deeper = deepest.as_ref().is_none_or(|(deepest_root, _)| {
            (depth(&canonical_root), &canonical_root) > (depth(deepest_root), deepest_root)
        });
        if deeper {
            deepest = Some((canonical_root, record));
        }
    }
    let Some((_, record)) = deepest else {
        return ProjectTrustProvenance::NoDecision;
    };
    match record.state {
        TrustDecision::Trusted => ProjectTrustProvenance::TrustedRoot(record.project_root.clone()),
        TrustDecision::Rejected | TrustDecision::Revoked => {
            ProjectTrustProvenance::NegativeDecision {
                root: record.project_root.clone(),
                state: record.state,
            }
        }
        TrustDecision::Pending => ProjectTrustProvenance::PendingDecision {
            root: record.project_root.clone(),
        },
    }
}

/// Counts the canonical path components used to rank candidate roots by depth.
fn depth(root: &Path) -> usize {
    root.components().count()
}
