//! Persistent project trust database operations.
//!
//! The store layer loads, saves, and mutates trust records while delegating
//! parsing and canonical path handling to the encoding module.

use super::{
    MezError, OpenOptions, Path, PathBuf, ProjectTrustRecord, ProjectTrustRevision,
    ProjectTrustSnapshot, ProjectTrustStore, Result, TrustDecision, Write,
    canonicalize_existing_or_original, fs, parse_record_line, set_private_file_permissions,
    unix_now_seconds,
};
use crate::config::CURRENT_CONFIG_SCHEMA_VERSION;
use rustix::fs::{FlockOperation, flock};
use sha2::{Digest, Sha256};

/// Current project trust record policy version.
const PROJECT_TRUST_POLICY_VERSION: u32 = 1;

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
        if record.trust_policy_version != PROJECT_TRUST_POLICY_VERSION {
            return None;
        }
        if record.configuration_schema_version != CURRENT_CONFIG_SCHEMA_VERSION as u32 {
            return None;
        }
        let git_marker_path =
            git_marker_path.map(|path| canonicalize_existing_or_original(path.to_path_buf()));
        if record.git_marker_path != git_marker_path {
            return None;
        }
        Some(record)
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

    /// Loads trust records together with the digest of their persisted bytes.
    ///
    /// Callers that retain trust state use the revision to detect external
    /// decisions without relying on timestamp resolution or file length.
    pub fn load_snapshot_from_file(path: &Path) -> Result<ProjectTrustSnapshot> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ProjectTrustSnapshot {
                    store: Self::default(),
                    revision: ProjectTrustRevision::Missing,
                });
            }
            Err(error) => return Err(error.into()),
        };
        let revision = ProjectTrustRevision::Sha256(
            Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        );
        let text = String::from_utf8(bytes)
            .map_err(|_| MezError::config("project trust database is not valid UTF-8"))?;
        let mut store = Self::default();

        for line in text.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let fields = parse_record_line(line)?;
            let record = ProjectTrustRecord::from_fields(&fields)?;
            store.records.insert(record.project_root.clone(), record);
        }

        Ok(ProjectTrustSnapshot { store, revision })
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
        let _lock = project_trust_database_lock(path)?;
        self.save_to_file_locked(path)
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
        let _lock = project_trust_database_lock(path)?;
        let mut store = Self::load_from_file(path)?;
        update(&mut store)?;
        store.save_to_file_locked(path)?;
        Self::load_snapshot_from_file(path)
    }

    /// Persists the complete store while the caller holds the database lock.
    fn save_to_file_locked(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project-trust.tsv");
        let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        let write_result = (|| -> Result<()> {
            file.write_all(b"# Mezzanine project trust database v1\n")?;
            for record in self.records.values() {
                file.write_all(record.to_line().as_bytes())?;
                file.write_all(b"\n")?;
            }
            file.flush()?;
            file.sync_all()?;
            set_private_file_permissions(&temporary)?;
            fs::rename(&temporary, path)?;
            set_private_file_permissions(path)?;
            if let Some(parent) = path.parent()
                && let Ok(directory) = OpenOptions::new().read(true).open(parent)
            {
                let _ = directory.sync_all();
            }
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result
    }
}

/// Holds an exclusive advisory lock for one project-trust database update.
#[derive(Debug)]
struct ProjectTrustDatabaseLock {
    _file: fs::File,
}

/// Acquires the sibling lock shared by every project-trust writer.
fn project_trust_database_lock(path: &Path) -> Result<ProjectTrustDatabaseLock> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project-trust.tsv");
    let lock_path = path.with_file_name(format!(".{file_name}.lock"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    set_private_file_permissions(&lock_path)?;
    flock(&file, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
    Ok(ProjectTrustDatabaseLock { _file: file })
}
