//! Locked atomic repository operations for durable leases.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
use serde::{Deserialize, Serialize};

use super::{
    LeaseCheckpointReference, LeaseGarbageCollectionPolicy, LeaseGarbageCollectionPreview,
    LeaseReservation, LeaseReservationRequest, MezError, RemoteSessionLease,
    RemoteSessionLeaseState, Result, validate_nonempty_identifier, validate_optional_text,
};
use crate::runtime::current_effective_uid;

const DATABASE_VERSION: u32 = 1;
const DATABASE_FILE_NAME: &str = "leases.json";
const LOCK_FILE_NAME: &str = "leases.lock";
const MAX_DATABASE_BYTES: u64 = 4 * 1024 * 1024;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LeaseDatabase {
    version: u32,
    boot_generation: u64,
    leases: Vec<RemoteSessionLease>,
    #[serde(default)]
    snapshot_cleanup_candidates: Vec<String>,
}

impl Default for LeaseDatabase {
    fn default() -> Self {
        Self {
            version: DATABASE_VERSION,
            boot_generation: 0,
            leases: Vec::new(),
            snapshot_cleanup_candidates: Vec::new(),
        }
    }
}

/// Private transactional repository for durable remote-session leases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteSessionLeaseRepository {
    directory: PathBuf,
}

impl RemoteSessionLeaseRepository {
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub(crate) fn list(&self) -> Result<Vec<RemoteSessionLease>> {
        self.with_locked_database(|database| {
            let mut leases = database.leases;
            leases.sort_by(|left, right| left.lease_id.cmp(&right.lease_id));
            Ok(leases)
        })
    }

    pub(crate) fn get(&self, lease_id: &str) -> Result<Option<RemoteSessionLease>> {
        validate_nonempty_identifier(lease_id, "id")?;
        self.with_locked_database(|database| {
            Ok(database
                .leases
                .into_iter()
                .find(|lease| lease.lease_id == lease_id))
        })
    }

    pub(crate) fn get_by_session(&self, session_id: &str) -> Result<Option<RemoteSessionLease>> {
        validate_nonempty_identifier(session_id, "session id")?;
        self.with_locked_database(|database| {
            Ok(database
                .leases
                .into_iter()
                .find(|lease| lease.session_id == session_id))
        })
    }

    pub(crate) fn boot_generation(&self) -> Result<u64> {
        self.with_locked_database(|database| Ok(database.boot_generation))
    }

    pub(crate) fn reserve_pending(
        &self,
        request: LeaseReservationRequest,
    ) -> Result<LeaseReservation> {
        self.reserve_pending_with_limits(request, usize::MAX, usize::MAX, usize::MAX, usize::MAX)
    }

    /// Reserves one pending lease while enforcing principal and global quotas
    /// in the same locked transaction as idempotency and uniqueness checks.
    pub(crate) fn reserve_pending_with_limits(
        &self,
        request: LeaseReservationRequest,
        max_leases_for_owner: usize,
        max_live_for_owner: usize,
        max_leases_global: usize,
        max_live_global: usize,
    ) -> Result<LeaseReservation> {
        validate_reservation_request(&request)?;
        self.mutate_database(|database| {
            if let Some(existing) = database.leases.iter().find(|lease| {
                lease.owner_principal_id == request.owner_principal_id
                    && lease.idempotency_key == request.idempotency_key
            }) {
                if existing.creation_fingerprint == request.creation_fingerprint {
                    return Ok(LeaseReservation::Replay(existing.clone()));
                }
                return Err(MezError::conflict(
                    "remote session lease idempotency key was reused with different creation inputs",
                ));
            }
            let active_global = database
                .leases
                .iter()
                .filter(|lease| !lease.state.is_garbage_collectable())
                .count();
            if active_global >= max_leases_global {
                return Err(MezError::conflict(
                    "global remote session lease limit has been reached",
                ));
            }
            let active_for_owner = database
                .leases
                .iter()
                .filter(|lease| {
                    lease.owner_principal_id == request.owner_principal_id
                        && !lease.state.is_garbage_collectable()
                })
                .count();
            if active_for_owner >= max_leases_for_owner {
                return Err(MezError::conflict(
                    "remote principal lease limit has been reached",
                ));
            }
            let live_global = database
                .leases
                .iter()
                .filter(|lease| {
                    matches!(
                        lease.state,
                        RemoteSessionLeaseState::Pending | RemoteSessionLeaseState::Active
                    )
                })
                .count();
            if live_global >= max_live_global {
                return Err(MezError::conflict(
                    "global remote live-session limit has been reached",
                ));
            }
            let live_for_owner = database
                .leases
                .iter()
                .filter(|lease| {
                    lease.owner_principal_id == request.owner_principal_id
                        && matches!(
                            lease.state,
                            RemoteSessionLeaseState::Pending | RemoteSessionLeaseState::Active
                        )
                })
                .count();
            if live_for_owner >= max_live_for_owner {
                return Err(MezError::conflict(
                    "remote principal live-session limit has been reached",
                ));
            }
            if request.name.as_ref().is_some_and(|name| {
                database.leases.iter().any(|lease| {
                    lease.name.as_ref() == Some(name) && !lease.state.is_garbage_collectable()
                })
            }) {
                return Err(MezError::conflict(
                    "remote session lease name is already reserved",
                ));
            }
            if database.leases.iter().any(|lease| lease.lease_id == request.lease_id) {
                return Err(MezError::conflict("remote session lease id already exists"));
            }
            if database.leases.iter().any(|lease| lease.session_id == request.session_id) {
                return Err(MezError::conflict(
                    "remote session lease session id already exists",
                ));
            }
            let lease = RemoteSessionLease {
                lease_id: request.lease_id,
                session_id: request.session_id,
                owner_principal_id: request.owner_principal_id,
                owner_live_session_limit: request.owner_live_session_limit,
                name: request.name,
                default_for_owner: request.default_for_owner,
                state: RemoteSessionLeaseState::Pending,
                created_at_unix_seconds: request.now_unix_seconds,
                updated_at_unix_seconds: request.now_unix_seconds,
                activated_at_unix_seconds: None,
                terminal_at_unix_seconds: None,
                idempotency_key: request.idempotency_key,
                creation_fingerprint: request.creation_fingerprint,
                checkpoint: None,
                failure: None,
                boot_generation: database.boot_generation,
                lease_generation: 1,
            };
            lease.validate()?;
            database.leases.push(lease.clone());
            Ok(LeaseReservation::Created(lease))
        })
    }

    pub(crate) fn activate(
        &self,
        lease_id: &str,
        expected_boot_generation: u64,
        expected_lease_generation: u64,
        now_unix_seconds: u64,
    ) -> Result<RemoteSessionLease> {
        self.transition(
            lease_id,
            expected_boot_generation,
            expected_lease_generation,
            now_unix_seconds,
            |lease| {
                require_state(
                    lease,
                    &[
                        RemoteSessionLeaseState::Pending,
                        RemoteSessionLeaseState::Recoverable,
                    ],
                    "activate",
                )?;
                lease.state = RemoteSessionLeaseState::Active;
                lease.activated_at_unix_seconds = Some(now_unix_seconds);
                lease.failure = None;
                Ok(())
            },
        )
    }

    pub(crate) fn mark_recoverable(
        &self,
        lease_id: &str,
        expected_boot_generation: u64,
        expected_lease_generation: u64,
        now_unix_seconds: u64,
    ) -> Result<RemoteSessionLease> {
        self.transition(
            lease_id,
            expected_boot_generation,
            expected_lease_generation,
            now_unix_seconds,
            |lease| {
                require_state(
                    lease,
                    &[RemoteSessionLeaseState::Active],
                    "mark recoverable",
                )?;
                lease.state = RemoteSessionLeaseState::Recoverable;
                Ok(())
            },
        )
    }

    /// Marks an active lease recoverable after its supervised runtime exits.
    pub(crate) fn mark_recoverable_after_runtime_exit(
        &self,
        lease_id: &str,
        expected_boot_generation: u64,
        expected_lease_generation: u64,
        now_unix_seconds: u64,
        diagnostic: String,
    ) -> Result<RemoteSessionLease> {
        validate_optional_text(Some(&diagnostic), "runtime exit diagnostic", 1024)?;
        self.transition(
            lease_id,
            expected_boot_generation,
            expected_lease_generation,
            now_unix_seconds,
            |lease| {
                require_state(
                    lease,
                    &[RemoteSessionLeaseState::Active],
                    "mark recoverable after runtime exit",
                )?;
                lease.state = RemoteSessionLeaseState::Recoverable;
                lease.failure = Some(diagnostic);
                Ok(())
            },
        )
    }

    /// Records a retryable recovery diagnostic without consuming recoverability.
    pub(crate) fn record_retryable_recovery_failure(
        &self,
        lease_id: &str,
        expected_boot_generation: u64,
        expected_lease_generation: u64,
        now_unix_seconds: u64,
        failure: String,
    ) -> Result<RemoteSessionLease> {
        validate_optional_text(Some(&failure), "recovery failure", 1024)?;
        self.transition(
            lease_id,
            expected_boot_generation,
            expected_lease_generation,
            now_unix_seconds,
            |lease| {
                require_state(
                    lease,
                    &[RemoteSessionLeaseState::Recoverable],
                    "record retryable recovery failure",
                )?;
                lease.failure = Some(failure);
                Ok(())
            },
        )
    }

    pub(crate) fn mark_failed(
        &self,
        lease_id: &str,
        expected_boot_generation: u64,
        expected_lease_generation: u64,
        now_unix_seconds: u64,
        failure: String,
    ) -> Result<RemoteSessionLease> {
        validate_optional_text(Some(&failure), "failure", 1024)?;
        self.transition(
            lease_id,
            expected_boot_generation,
            expected_lease_generation,
            now_unix_seconds,
            |lease| {
                require_state(
                    lease,
                    &[
                        RemoteSessionLeaseState::Pending,
                        RemoteSessionLeaseState::Active,
                        RemoteSessionLeaseState::Recoverable,
                    ],
                    "mark failed",
                )?;
                lease.state = RemoteSessionLeaseState::Failed;
                lease.failure = Some(failure);
                lease.terminal_at_unix_seconds = Some(now_unix_seconds);
                Ok(())
            },
        )
    }

    pub(crate) fn update_checkpoint(
        &self,
        lease_id: &str,
        expected_boot_generation: u64,
        expected_lease_generation: u64,
        checkpoint: LeaseCheckpointReference,
        now_unix_seconds: u64,
    ) -> Result<RemoteSessionLease> {
        validate_nonempty_identifier(lease_id, "id")?;
        self.mutate_database(|database| {
            if database.boot_generation != expected_boot_generation {
                return Err(MezError::conflict(
                    "remote session lease boot generation is stale",
                ));
            }
            let lease = database
                .leases
                .iter_mut()
                .find(|lease| lease.lease_id == lease_id)
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "remote session lease not found",
                    )
                })?;
            if lease.boot_generation != expected_boot_generation
                || lease.lease_generation != expected_lease_generation
            {
                return Err(MezError::conflict(
                    "remote session lease generation is stale",
                ));
            }
            if now_unix_seconds < lease.updated_at_unix_seconds {
                return Err(MezError::conflict(
                    "remote session lease update timestamp is stale",
                ));
            }
            require_state(
                lease,
                &[
                    RemoteSessionLeaseState::Active,
                    RemoteSessionLeaseState::Recoverable,
                ],
                "update checkpoint",
            )?;
            checkpoint.validate(&lease.session_id)?;
            if database
                .snapshot_cleanup_candidates
                .contains(&checkpoint.snapshot_id)
            {
                return Err(MezError::conflict(
                    "remote session checkpoint is already pending artifact cleanup",
                ));
            }
            let replaced_snapshot_id = lease
                .checkpoint
                .as_ref()
                .filter(|prior| prior.snapshot_id != checkpoint.snapshot_id)
                .map(|prior| prior.snapshot_id.clone());
            lease.checkpoint = Some(checkpoint);
            lease.updated_at_unix_seconds = now_unix_seconds;
            lease.lease_generation = lease.lease_generation.saturating_add(1);
            lease.validate()?;
            let updated = lease.clone();
            if let Some(snapshot_id) = replaced_snapshot_id {
                database.snapshot_cleanup_candidates.push(snapshot_id);
                database.snapshot_cleanup_candidates.sort();
                database.snapshot_cleanup_candidates.dedup();
            }
            Ok(updated)
        })
    }

    /// Lists snapshot identifiers whose last known owning reference was
    /// replaced or garbage-collected and whose artifact cleanup is pending.
    pub(crate) fn snapshot_cleanup_candidates(&self) -> Result<Vec<String>> {
        self.with_locked_database(|database| {
            let mut candidates = database.snapshot_cleanup_candidates;
            candidates.sort();
            Ok(candidates)
        })
    }

    /// Checks whether any current durable lease still owns this snapshot.
    pub(crate) fn snapshot_is_referenced(&self, snapshot_id: &str) -> Result<bool> {
        validate_nonempty_identifier(snapshot_id, "checkpoint snapshot id")?;
        self.with_locked_database(|database| {
            Ok(database.leases.iter().any(|lease| {
                lease
                    .checkpoint
                    .as_ref()
                    .is_some_and(|checkpoint| checkpoint.snapshot_id == snapshot_id)
            }))
        })
    }

    /// Removes one completed cleanup intent unless a current lease still
    /// references the snapshot identifier.
    pub(crate) fn acknowledge_snapshot_cleanup(&self, snapshot_id: &str) -> Result<bool> {
        validate_nonempty_identifier(snapshot_id, "checkpoint snapshot id")?;
        self.mutate_database(|database| {
            if database.leases.iter().any(|lease| {
                lease
                    .checkpoint
                    .as_ref()
                    .is_some_and(|checkpoint| checkpoint.snapshot_id == snapshot_id)
            }) {
                return Ok(false);
            }
            let prior_len = database.snapshot_cleanup_candidates.len();
            database
                .snapshot_cleanup_candidates
                .retain(|candidate| candidate != snapshot_id);
            Ok(database.snapshot_cleanup_candidates.len() != prior_len)
        })
    }

    pub(crate) fn release(
        &self,
        lease_id: &str,
        expected_boot_generation: u64,
        expected_lease_generation: u64,
        now_unix_seconds: u64,
    ) -> Result<RemoteSessionLease> {
        self.transition(
            lease_id,
            expected_boot_generation,
            expected_lease_generation,
            now_unix_seconds,
            |lease| {
                require_state(
                    lease,
                    &[
                        RemoteSessionLeaseState::Pending,
                        RemoteSessionLeaseState::Active,
                        RemoteSessionLeaseState::Recoverable,
                        RemoteSessionLeaseState::Failed,
                    ],
                    "release",
                )?;
                lease.state = RemoteSessionLeaseState::Released;
                lease.terminal_at_unix_seconds = Some(now_unix_seconds);
                Ok(())
            },
        )
    }

    pub(crate) fn revoke(
        &self,
        lease_id: &str,
        expected_boot_generation: u64,
        expected_lease_generation: u64,
        now_unix_seconds: u64,
        reason: Option<String>,
    ) -> Result<RemoteSessionLease> {
        validate_optional_text(reason.as_deref(), "revocation reason", 1024)?;
        self.transition(
            lease_id,
            expected_boot_generation,
            expected_lease_generation,
            now_unix_seconds,
            |lease| {
                require_state(
                    lease,
                    &[
                        RemoteSessionLeaseState::Pending,
                        RemoteSessionLeaseState::Active,
                        RemoteSessionLeaseState::Recoverable,
                        RemoteSessionLeaseState::Failed,
                    ],
                    "revoke",
                )?;
                lease.state = RemoteSessionLeaseState::Revoked;
                lease.failure = reason;
                lease.terminal_at_unix_seconds = Some(now_unix_seconds);
                Ok(())
            },
        )
    }

    pub(crate) fn advance_boot_generation(&self, now_unix_seconds: u64) -> Result<u64> {
        self.mutate_database(|database| {
            database.boot_generation = database.boot_generation.saturating_add(1);
            for lease in &mut database.leases {
                match lease.state {
                    RemoteSessionLeaseState::Pending => {
                        lease.state = RemoteSessionLeaseState::Failed;
                        lease.failure =
                            Some("lease creation was interrupted by host restart".to_string());
                        lease.terminal_at_unix_seconds = Some(now_unix_seconds);
                    }
                    RemoteSessionLeaseState::Active => {
                        lease.state = RemoteSessionLeaseState::Recoverable;
                    }
                    _ => {}
                }
                lease.updated_at_unix_seconds = now_unix_seconds;
                lease.boot_generation = database.boot_generation;
                lease.lease_generation = lease.lease_generation.saturating_add(1);
            }
            Ok(database.boot_generation)
        })
    }

    pub(crate) fn preview_gc(
        &self,
        policy: LeaseGarbageCollectionPolicy,
    ) -> Result<LeaseGarbageCollectionPreview> {
        self.with_locked_database(|database| Ok(gc_preview(&database, policy)))
    }

    pub(crate) fn apply_gc(
        &self,
        policy: LeaseGarbageCollectionPolicy,
    ) -> Result<LeaseGarbageCollectionPreview> {
        self.mutate_database(|database| {
            let preview = gc_preview(database, policy);
            let candidates = preview.lease_ids.iter().cloned().collect::<HashSet<_>>();
            database
                .snapshot_cleanup_candidates
                .extend(preview.checkpoint_snapshot_ids.iter().cloned());
            database.snapshot_cleanup_candidates.sort();
            database.snapshot_cleanup_candidates.dedup();
            database
                .leases
                .retain(|lease| !candidates.contains(&lease.lease_id));
            Ok(preview)
        })
    }

    fn transition(
        &self,
        lease_id: &str,
        expected_boot_generation: u64,
        expected_lease_generation: u64,
        now_unix_seconds: u64,
        operation: impl FnOnce(&mut RemoteSessionLease) -> Result<()>,
    ) -> Result<RemoteSessionLease> {
        validate_nonempty_identifier(lease_id, "id")?;
        self.mutate_database(|database| {
            if database.boot_generation != expected_boot_generation {
                return Err(MezError::conflict(
                    "remote session lease boot generation is stale",
                ));
            }
            let lease = database
                .leases
                .iter_mut()
                .find(|lease| lease.lease_id == lease_id)
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "remote session lease not found",
                    )
                })?;
            if lease.boot_generation != expected_boot_generation
                || lease.lease_generation != expected_lease_generation
            {
                return Err(MezError::conflict(
                    "remote session lease generation is stale",
                ));
            }
            if now_unix_seconds < lease.updated_at_unix_seconds {
                return Err(MezError::conflict(
                    "remote session lease update timestamp is stale",
                ));
            }
            operation(lease)?;
            lease.updated_at_unix_seconds = now_unix_seconds;
            lease.lease_generation = lease.lease_generation.saturating_add(1);
            lease.validate()?;
            Ok(lease.clone())
        })
    }

    fn mutate_database<T>(
        &self,
        operation: impl FnOnce(&mut LeaseDatabase) -> Result<T>,
    ) -> Result<T> {
        ensure_private_directory(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let mut database = self.load_database()?;
        let result = operation(&mut database)?;
        validate_database(&database)?;
        self.write_database(&database)?;
        Ok(result)
    }

    fn with_locked_database<T>(
        &self,
        operation: impl FnOnce(LeaseDatabase) -> Result<T>,
    ) -> Result<T> {
        ensure_private_directory(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockShared).map_err(std::io::Error::from)?;
        operation(self.load_database()?)
    }

    fn load_database(&self) -> Result<LeaseDatabase> {
        let path = self.directory.join(DATABASE_FILE_NAME);
        let file = match open_private_file_read(&path) {
            Ok(file) => file,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => {
                return Ok(LeaseDatabase::default());
            }
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > MAX_DATABASE_BYTES {
            return Err(MezError::invalid_state(
                "remote session lease database exceeds the protected size limit",
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_DATABASE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_DATABASE_BYTES {
            return Err(MezError::invalid_state(
                "remote session lease database exceeds the protected size limit",
            ));
        }
        let database: LeaseDatabase = serde_json::from_slice(&bytes).map_err(|error| {
            MezError::invalid_state(format!(
                "remote session lease database is malformed: {error}"
            ))
        })?;
        validate_database(&database)?;
        Ok(database)
    }

    fn write_database(&self, database: &LeaseDatabase) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(database).map_err(|error| {
            MezError::invalid_state(format!(
                "failed to encode remote session lease database: {error}"
            ))
        })?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_DATABASE_BYTES {
            return Err(MezError::invalid_state(
                "remote session lease database exceeds the protected size limit",
            ));
        }
        write_private_atomic(&self.directory.join(DATABASE_FILE_NAME), &bytes)
    }
}

fn validate_reservation_request(request: &LeaseReservationRequest) -> Result<()> {
    validate_nonempty_identifier(&request.lease_id, "id")?;
    validate_nonempty_identifier(&request.session_id, "session id")?;
    validate_nonempty_identifier(&request.owner_principal_id, "owner principal id")?;
    validate_nonempty_identifier(&request.idempotency_key, "idempotency key")?;
    validate_nonempty_identifier(&request.creation_fingerprint, "creation fingerprint")?;
    validate_optional_text(request.name.as_deref(), "name", 256)
}

fn require_state(
    lease: &RemoteSessionLease,
    allowed: &[RemoteSessionLeaseState],
    operation: &str,
) -> Result<()> {
    if allowed.contains(&lease.state) {
        Ok(())
    } else {
        Err(MezError::invalid_state(format!(
            "cannot {operation} remote session lease in state {:?}",
            lease.state
        )))
    }
}

fn gc_preview(
    database: &LeaseDatabase,
    policy: LeaseGarbageCollectionPolicy,
) -> LeaseGarbageCollectionPreview {
    let mut lease_ids = Vec::new();
    let mut checkpoint_snapshot_ids = Vec::new();
    for lease in &database.leases {
        let terminal_at = lease.terminal_at_unix_seconds.unwrap_or(u64::MAX);
        let eligible = match lease.state {
            RemoteSessionLeaseState::Released => terminal_at <= policy.released_before_unix_seconds,
            RemoteSessionLeaseState::Revoked => terminal_at <= policy.revoked_before_unix_seconds,
            RemoteSessionLeaseState::Failed => terminal_at <= policy.failed_before_unix_seconds,
            _ => false,
        };
        if eligible {
            lease_ids.push(lease.lease_id.clone());
            if let Some(checkpoint) = &lease.checkpoint {
                checkpoint_snapshot_ids.push(checkpoint.snapshot_id.clone());
            }
        }
    }
    lease_ids.sort();
    checkpoint_snapshot_ids.sort();
    checkpoint_snapshot_ids.dedup();
    LeaseGarbageCollectionPreview {
        lease_ids,
        checkpoint_snapshot_ids,
    }
}

fn validate_database(database: &LeaseDatabase) -> Result<()> {
    if database.version != DATABASE_VERSION {
        return Err(MezError::invalid_state(format!(
            "unsupported remote session lease database version {}",
            database.version
        )));
    }
    let mut lease_ids = HashSet::new();
    let mut session_ids = HashSet::new();
    let mut idempotency = HashSet::new();
    let mut cleanup_candidates = HashSet::new();
    for snapshot_id in &database.snapshot_cleanup_candidates {
        validate_nonempty_identifier(snapshot_id, "checkpoint snapshot id")?;
        if !cleanup_candidates.insert(snapshot_id) {
            return Err(MezError::invalid_state(
                "remote session lease database contains duplicate snapshot cleanup candidates",
            ));
        }
    }
    for lease in &database.leases {
        lease.validate()?;
        if !lease_ids.insert(lease.lease_id.clone()) {
            return Err(MezError::invalid_state(
                "remote session lease database contains duplicate lease ids",
            ));
        }
        if !session_ids.insert(lease.session_id.clone()) {
            return Err(MezError::invalid_state(
                "remote session lease database contains duplicate session ids",
            ));
        }
        if !idempotency.insert((
            lease.owner_principal_id.clone(),
            lease.idempotency_key.clone(),
        )) {
            return Err(MezError::invalid_state(
                "remote session lease database contains duplicate principal idempotency keys",
            ));
        }
        if lease.boot_generation > database.boot_generation {
            return Err(MezError::invalid_state(
                "remote session lease generation exceeds the database boot generation",
            ));
        }
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<()> {
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
            "remote session lease directory must be private and owned by the current user",
        ));
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

fn open_private_file_read(path: &Path) -> Result<fs::File> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
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
            "remote session lease path {} must be a private regular file owned by the current user",
            path.display()
        )));
    }
    Ok(())
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MezError::invalid_args("remote session lease path has no parent"))?;
    ensure_private_directory(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        validate_private_file(path, &metadata)?;
    }
    let temporary = parent.join(format!(
        ".{DATABASE_FILE_NAME}.{}.{}.tmp",
        std::process::id(),
        NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        let metadata = fs::symlink_metadata(path)?;
        validate_private_file(path, &metadata)?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
