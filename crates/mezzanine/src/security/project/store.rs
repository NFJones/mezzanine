//! Persistent project trust database operations.
//!
//! The store layer loads, saves, and mutates trust records while delegating
//! parsing and canonical path handling to the encoding module.

use super::{
    MezError, Path, PathBuf, ProjectTrustRecord, ProjectTrustSnapshot, ProjectTrustStore, Result,
    TrustDecision, canonicalize_existing_or_original, unix_now_seconds,
};
use crate::config::CURRENT_CONFIG_SCHEMA_VERSION;

/// Current project trust record policy version.
const PROJECT_TRUST_POLICY_VERSION: u32 = 1;

/// Returns whether one stored record matches the trust policy and configuration
/// schema versions implicit-authority callers accept.
///
/// A record written under an older trust policy or configuration schema is
/// treated as no decision at all, so a stale trusted record can neither grant
/// nor withhold implicit project authority that the stricter lookup already
/// refuses to recognize.
pub(super) fn record_matches_current_trust_versions(record: &ProjectTrustRecord) -> bool {
    record.trust_policy_version == PROJECT_TRUST_POLICY_VERSION
        && record.configuration_schema_version == CURRENT_CONFIG_SCHEMA_VERSION as u32
}

impl ProjectTrustStore {
    /// Runs the decide operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn decide(
        &mut self,
        project_root: PathBuf,
        decision: TrustDecision,
        git_marker_path: Option<PathBuf>,
    ) -> Result<()> {
        self.decide_at(project_root, decision, git_marker_path, unix_now_seconds())
    }

    /// Runs the decide at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn decide_at(
        &mut self,
        project_root: PathBuf,
        decision: TrustDecision,
        git_marker_path: Option<PathBuf>,
        trusted_at_unix_seconds: u64,
    ) -> Result<()> {
        self.decide_at_with_client(
            project_root,
            decision,
            git_marker_path,
            trusted_at_unix_seconds,
            None,
        )
    }

    /// Runs the decide with client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn decide_with_client(
        &mut self,
        project_root: PathBuf,
        decision: TrustDecision,
        git_marker_path: Option<PathBuf>,
        decided_by_client_id: Option<String>,
    ) -> Result<()> {
        self.decide_at_with_client(
            project_root,
            decision,
            git_marker_path,
            unix_now_seconds(),
            decided_by_client_id,
        )
    }

    /// Runs the decide at with client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn decide_at_with_client(
        &mut self,
        project_root: PathBuf,
        decision: TrustDecision,
        git_marker_path: Option<PathBuf>,
        trusted_at_unix_seconds: u64,
        decided_by_client_id: Option<String>,
    ) -> Result<()> {
        if !matches!(
            decision,
            TrustDecision::Trusted | TrustDecision::Rejected | TrustDecision::Revoked
        ) {
            return Err(MezError::invalid_args(
                "project trust decision must be trust, reject, or revoke",
            ));
        }
        let project_root = canonicalize_existing_or_original(project_root);
        let git_marker_path = git_marker_path.map(canonicalize_existing_or_original);
        self.records.insert(
            project_root.clone(),
            ProjectTrustRecord {
                project_root,
                state: decision,
                git_marker_path,
                trusted_at_unix_seconds,
                decided_by_client_id,
                trust_policy_version: PROJECT_TRUST_POLICY_VERSION,
                configuration_schema_version: CURRENT_CONFIG_SCHEMA_VERSION as u32,
                vcs_remote: None,
            },
        );
        Ok(())
    }

    /// Runs the get operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn get(&self, project_root: &Path) -> Option<&ProjectTrustRecord> {
        let canonical = canonicalize_existing_or_original(project_root.to_path_buf());
        self.records.get(&canonical)
    }

    /// Returns a trust record only when it matches the current project identity.
    pub fn get_for_project(
        &self,
        project_root: &Path,
        git_marker_path: Option<&Path>,
    ) -> Option<&ProjectTrustRecord> {
        let record = self.get(project_root)?;
        if !record_matches_current_trust_versions(record) {
            return None;
        }
        let git_marker_path =
            git_marker_path.map(|path| canonicalize_existing_or_original(path.to_path_buf()));
        if record.git_marker_path != git_marker_path {
            return None;
        }
        Some(record)
    }

    /// Returns stored records that match the current trust policy and
    /// configuration schema versions.
    ///
    /// Deepest-decision resolution uses this stricter view rather than
    /// [`Self::records`] so resolution and [`Self::get_for_project`] agree on
    /// which records count as a stored decision.
    pub(super) fn records_matching_current_versions(
        &self,
    ) -> impl Iterator<Item = &ProjectTrustRecord> {
        self.records()
            .filter(|record| record_matches_current_trust_versions(record))
    }

    /// Runs the records operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn records(&self) -> impl Iterator<Item = &ProjectTrustRecord> {
        self.records.values()
    }

    /// Runs the load from file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn load_from_file(path: &Path) -> Result<Self> {
        Ok(Self::load_snapshot_from_file(path)?.store)
    }

    /// Loads trust records together with the revision of their persisted
    /// contents.
    ///
    /// Callers that retain trust state use the revision to detect external
    /// decisions without relying on timestamp resolution or file length.
    pub fn load_snapshot_from_file(path: &Path) -> Result<ProjectTrustSnapshot> {
        super::sqlite::load_snapshot(path)
    }

    /// Runs the save to file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[allow(
        dead_code,
        reason = "complete-store persistence remains available for import and focused tests"
    )]
    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        super::sqlite::save(path, self)
    }

    /// Serializes one read-modify-write trust update under the database lock.
    ///
    /// The callback receives the latest persisted store. Its successful
    /// mutation is atomically committed before the resulting snapshot is
    /// returned, preventing independent CLI and daemon writers from dropping
    /// each other's records.
    pub fn update_file<F>(path: &Path, update: F) -> Result<ProjectTrustSnapshot>
    where
        F: FnOnce(&mut Self) -> Result<()>,
    {
        super::sqlite::update(path, update)
    }

    /// Renders the stored trust records in the legacy TSV shape without
    /// creating or migrating the database.
    pub fn export_database_tsv_read_only(path: &Path) -> Result<Option<String>> {
        super::sqlite::export_tsv_read_only(path)
    }

    /// Inserts one record directly for tests that need a state the public
    /// decisions do not produce, such as a pending nested decision.
    #[cfg(test)]
    pub(crate) fn insert_record_for_tests(&mut self, record: ProjectTrustRecord) {
        self.records.insert(record.project_root.clone(), record);
    }
}
