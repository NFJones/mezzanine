use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
use serde::{Deserialize, Serialize};

use super::{
    LocalAssignmentCheckpoint, LocalAssignmentReservationRequest, LocalSessionAssignment,
    LocalSessionAssignmentState, MezError, Result, validate_identifier, validate_optional_text,
};
use crate::runtime::current_effective_uid;

/// In-memory document version retained for the legacy JSON import.
const DATABASE_VERSION: u32 = 1;

/// Exclusive lock retained for the one-time legacy import and for mutual
/// exclusion with a build that still writes the legacy JSON document.
const LOCK_FILE_NAME: &str = "assignments.lock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LocalAssignmentDatabase {
    pub(super) version: u32,
    pub(super) boot_generation: u64,
    pub(super) assignments: Vec<LocalSessionAssignment>,
}

impl Default for LocalAssignmentDatabase {
    fn default() -> Self {
        Self {
            version: DATABASE_VERSION,
            boot_generation: 0,
            assignments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalSessionAssignmentRepository {
    directory: PathBuf,
}

impl LocalSessionAssignmentRepository {
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub(crate) fn list(&self) -> Result<Vec<LocalSessionAssignment>> {
        self.with_locked_database(|database| {
            let mut assignments = database.assignments;
            assignments.sort_by(|left, right| left.session_id.cmp(&right.session_id));
            Ok(assignments)
        })
    }

    /// Renders this store in its inspection TSV shape without creating it or
    /// taking the repository lock, so an export cannot mutate or block the
    /// daemon's store.
    pub(crate) fn export_tsv_read_only(&self) -> Result<Option<String>> {
        super::sqlite::export_tsv_read_only(&self.directory)
    }

    pub(crate) fn get(&self, session_id: &str) -> Result<Option<LocalSessionAssignment>> {
        validate_identifier(session_id, "session id")?;
        self.with_locked_database(|database| {
            Ok(database
                .assignments
                .into_iter()
                .find(|assignment| assignment.session_id == session_id))
        })
    }

    pub(crate) fn reserve_pending(
        &self,
        request: LocalAssignmentReservationRequest,
    ) -> Result<LocalSessionAssignment> {
        validate_identifier(&request.session_id, "session id")?;
        validate_optional_text(Some(&request.name), "name", 256)?;
        self.mutate_database(|database| {
            if database
                .assignments
                .iter()
                .any(|assignment| assignment.session_id == request.session_id)
            {
                return Err(MezError::conflict(
                    "local session identity is already assigned",
                ));
            }
            if request.default_for_host {
                for assignment in &mut database.assignments {
                    assignment.default_for_host = false;
                }
            }
            let assignment = LocalSessionAssignment {
                session_id: request.session_id,
                name: request.name,
                default_for_host: request.default_for_host,
                state: LocalSessionAssignmentState::Pending,
                created_at_unix_seconds: request.now_unix_seconds,
                updated_at_unix_seconds: request.now_unix_seconds,
                checkpoint: None,
                failure: None,
                boot_generation: database.boot_generation,
                assignment_generation: 1,
            };
            assignment.validate()?;
            database.assignments.push(assignment.clone());
            Ok(assignment)
        })
    }

    pub(crate) fn activate(
        &self,
        session_id: &str,
        expected_boot_generation: u64,
        expected_assignment_generation: u64,
        now_unix_seconds: u64,
    ) -> Result<LocalSessionAssignment> {
        self.transition(
            session_id,
            expected_boot_generation,
            expected_assignment_generation,
            now_unix_seconds,
            |assignment| {
                if !matches!(
                    assignment.state,
                    LocalSessionAssignmentState::Pending | LocalSessionAssignmentState::Recoverable
                ) {
                    return Err(MezError::invalid_state(
                        "local session assignment is not activatable",
                    ));
                }
                assignment.state = LocalSessionAssignmentState::Active;
                assignment.failure = None;
                Ok(())
            },
        )
    }

    pub(crate) fn update_checkpoint(
        &self,
        session_id: &str,
        expected_boot_generation: u64,
        expected_assignment_generation: u64,
        checkpoint: LocalAssignmentCheckpoint,
        now_unix_seconds: u64,
    ) -> Result<LocalSessionAssignment> {
        checkpoint.validate(session_id)?;
        self.transition(
            session_id,
            expected_boot_generation,
            expected_assignment_generation,
            now_unix_seconds,
            |assignment| {
                if assignment.state != LocalSessionAssignmentState::Active {
                    return Err(MezError::invalid_state(
                        "only an active local assignment can be checkpointed",
                    ));
                }
                assignment.checkpoint = Some(checkpoint);
                Ok(())
            },
        )
    }

    pub(crate) fn mark_recoverable_after_runtime_exit(
        &self,
        session_id: &str,
        expected_boot_generation: u64,
        expected_assignment_generation: u64,
        now_unix_seconds: u64,
        diagnostic: String,
    ) -> Result<LocalSessionAssignment> {
        validate_optional_text(Some(&diagnostic), "runtime exit diagnostic", 1024)?;
        self.transition(
            session_id,
            expected_boot_generation,
            expected_assignment_generation,
            now_unix_seconds,
            |assignment| {
                if assignment.state != LocalSessionAssignmentState::Active
                    || assignment.checkpoint.is_none()
                {
                    return Err(MezError::invalid_state(
                        "local session assignment is not recoverable",
                    ));
                }
                assignment.state = LocalSessionAssignmentState::Recoverable;
                assignment.failure = Some(diagnostic);
                Ok(())
            },
        )
    }

    pub(crate) fn record_retryable_recovery_failure(
        &self,
        session_id: &str,
        expected_boot_generation: u64,
        expected_assignment_generation: u64,
        now_unix_seconds: u64,
        failure: String,
    ) -> Result<LocalSessionAssignment> {
        validate_optional_text(Some(&failure), "recovery failure", 1024)?;
        self.transition(
            session_id,
            expected_boot_generation,
            expected_assignment_generation,
            now_unix_seconds,
            |assignment| {
                if assignment.state != LocalSessionAssignmentState::Recoverable {
                    return Err(MezError::invalid_state(
                        "local session assignment is not recoverable",
                    ));
                }
                assignment.failure = Some(failure);
                Ok(())
            },
        )
    }

    pub(crate) fn mark_failed(
        &self,
        session_id: &str,
        expected_boot_generation: u64,
        expected_assignment_generation: u64,
        now_unix_seconds: u64,
        failure: String,
    ) -> Result<LocalSessionAssignment> {
        validate_optional_text(Some(&failure), "failure", 1024)?;
        self.transition(
            session_id,
            expected_boot_generation,
            expected_assignment_generation,
            now_unix_seconds,
            |assignment| {
                assignment.state = LocalSessionAssignmentState::Failed;
                assignment.failure = Some(failure);
                Ok(())
            },
        )
    }

    pub(crate) fn advance_boot_generation(&self, now_unix_seconds: u64) -> Result<u64> {
        self.mutate_database(|database| {
            database.boot_generation = database.boot_generation.saturating_add(1);
            for assignment in &mut database.assignments {
                match assignment.state {
                    LocalSessionAssignmentState::Pending => {
                        assignment.state = LocalSessionAssignmentState::Failed;
                        assignment.failure = Some(
                            "local session creation was interrupted by host restart".to_string(),
                        );
                    }
                    LocalSessionAssignmentState::Active if assignment.checkpoint.is_some() => {
                        assignment.state = LocalSessionAssignmentState::Recoverable;
                        assignment.failure = Some(
                            "local session runtime ended with the previous host process"
                                .to_string(),
                        );
                    }
                    LocalSessionAssignmentState::Active => {
                        assignment.state = LocalSessionAssignmentState::Failed;
                        assignment.failure = Some(
                            "local session runtime ended without a recoverable checkpoint"
                                .to_string(),
                        );
                    }
                    _ => {}
                }
                assignment.updated_at_unix_seconds = now_unix_seconds;
                assignment.boot_generation = database.boot_generation;
                assignment.assignment_generation =
                    assignment.assignment_generation.saturating_add(1);
            }
            Ok(database.boot_generation)
        })
    }

    fn transition(
        &self,
        session_id: &str,
        expected_boot_generation: u64,
        expected_assignment_generation: u64,
        now_unix_seconds: u64,
        operation: impl FnOnce(&mut LocalSessionAssignment) -> Result<()>,
    ) -> Result<LocalSessionAssignment> {
        validate_identifier(session_id, "session id")?;
        self.mutate_database(|database| {
            if database.boot_generation != expected_boot_generation {
                return Err(MezError::conflict(
                    "local session assignment boot generation is stale",
                ));
            }
            let assignment = database
                .assignments
                .iter_mut()
                .find(|assignment| assignment.session_id == session_id)
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "local session assignment not found",
                    )
                })?;
            if assignment.boot_generation != expected_boot_generation
                || assignment.assignment_generation != expected_assignment_generation
            {
                return Err(MezError::conflict(
                    "local session assignment generation is stale",
                ));
            }
            if now_unix_seconds < assignment.updated_at_unix_seconds {
                return Err(MezError::conflict(
                    "local session assignment update timestamp is stale",
                ));
            }
            operation(assignment)?;
            assignment.updated_at_unix_seconds = now_unix_seconds;
            assignment.assignment_generation = assignment.assignment_generation.saturating_add(1);
            assignment.validate()?;
            Ok(assignment.clone())
        })
    }

    fn mutate_database<T>(
        &self,
        operation: impl FnOnce(&mut LocalAssignmentDatabase) -> Result<T>,
    ) -> Result<T> {
        ensure_private_directory(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let before = self.load_database()?;
        let mut after = before.clone();
        let result = operation(&mut after)?;
        validate_database(&after)?;
        self.write_database(&before, &after)?;
        Ok(result)
    }

    fn with_locked_database<T>(
        &self,
        operation: impl FnOnce(LocalAssignmentDatabase) -> Result<T>,
    ) -> Result<T> {
        ensure_private_directory(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockShared).map_err(std::io::Error::from)?;
        operation(self.load_database()?)
    }

    fn load_database(&self) -> Result<LocalAssignmentDatabase> {
        super::sqlite::load_database(&self.directory)
    }

    fn write_database(
        &self,
        before: &LocalAssignmentDatabase,
        after: &LocalAssignmentDatabase,
    ) -> Result<()> {
        super::sqlite::write_database(&self.directory, before, after)
    }
}

pub(super) fn validate_database(database: &LocalAssignmentDatabase) -> Result<()> {
    if database.version != DATABASE_VERSION {
        return Err(MezError::invalid_state(format!(
            "unsupported local session assignment database version {}",
            database.version
        )));
    }
    let mut session_ids = HashSet::new();
    let mut defaults = 0usize;
    for assignment in &database.assignments {
        assignment.validate()?;
        if !session_ids.insert(assignment.session_id.clone()) {
            return Err(MezError::invalid_state(
                "local session assignment database contains duplicate session ids",
            ));
        }
        if assignment.default_for_host && assignment.state != LocalSessionAssignmentState::Failed {
            defaults = defaults.saturating_add(1);
        }
        if assignment.boot_generation > database.boot_generation {
            return Err(MezError::invalid_state(
                "local session assignment generation exceeds the database boot generation",
            ));
        }
    }
    if defaults > 1 {
        return Err(MezError::invalid_state(
            "local session assignment database contains multiple defaults",
        ));
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    let existed = path.exists();
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != current_effective_uid()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(MezError::forbidden(
            "local session assignment directory must be private and owned by the current user",
        ));
    }
    if !existed && let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn open_private_lock(path: &Path) -> Result<fs::File> {
    let descriptor = open(
        path,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(std::io::Error::from)?;
    let file = fs::File::from(descriptor);
    validate_private_file(path, &file.metadata()?)?;
    Ok(file)
}

fn validate_private_file(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != current_effective_uid()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(MezError::forbidden(format!(
            "local session assignment path {} must be a private regular file owned by the current user",
            path.display()
        )));
    }
    Ok(())
}
