//! Filesystem-backed session registry operations.
//!
//! This module owns registry loading, persistence, lifecycle mutation, and
//! private runtime-directory handling.

#[cfg(test)]
use super::Path;
use super::{
    MezError, OpenOptions, PathBuf, REGISTRY_FILE_NAME, Result, SessionRecord, SessionRegistry,
    ensure_private_socket_directory, fs, set_private_file_permissions,
};
use rustix::fs::{FlockOperation, flock};
#[cfg(test)]
use std::io::Write;

/// Defines the REGISTRY LOCK FILE NAME const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const REGISTRY_LOCK_FILE_NAME: &str = ".sessions.tsv.lock";

/// Carries Session Registry Lock state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub(super) struct SessionRegistryLock {
    /// Stores the file value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    _file: fs::File,
}

/// Runs one blocking registry mutation on Tokio's blocking thread pool.
///
/// Registry writes use process-wide advisory file locks. Running the complete
/// synchronous read-modify-write operation in a blocking task prevents a
/// current-thread async runtime from parking its only reactor thread while
/// waiting for another registry lock holder.
async fn run_blocking_registry_mutation<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            MezError::invalid_state(format!("registry persistence task failed: {error}"))
        })?
}

impl SessionRegistry {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(root: PathBuf, owner_uid: u32) -> Self {
        Self { root, owner_uid }
    }

    /// Runs the root operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs the registry file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn registry_file(&self) -> PathBuf {
        self.root.join(REGISTRY_FILE_NAME)
    }

    /// Runs the registry lock file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn registry_lock_file(&self) -> PathBuf {
        self.root.join(REGISTRY_LOCK_FILE_NAME)
    }

    /// Runs the list operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn list(&self) -> Result<Vec<SessionRecord>> {
        super::sqlite::list(self)
    }

    /// Runs the list async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn list_async(&self) -> Result<Vec<SessionRecord>> {
        let registry = self.clone();
        run_blocking_registry_mutation(move || super::sqlite::list(&registry)).await
    }

    /// Runs the get operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn get(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        super::sqlite::get(self, session_id)
    }

    /// Runs the prune stale operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn prune_stale(&self) -> Result<usize> {
        super::sqlite::prune_stale(self)
    }

    /// Runs the upsert operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn upsert(&self, record: SessionRecord) -> Result<()> {
        super::sqlite::upsert(self, record)
    }

    /// Runs the upsert async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn upsert_async(&self, record: SessionRecord) -> Result<()> {
        let registry = self.clone();
        run_blocking_registry_mutation(move || registry.upsert(record)).await
    }

    /// Runs the remove operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn remove(&self, session_id: &str) -> Result<bool> {
        super::sqlite::remove(self, session_id)
    }

    /// Runs the remove async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn remove_async(&self, session_id: &str) -> Result<bool> {
        let registry = self.clone();
        let session_id = session_id.to_string();
        run_blocking_registry_mutation(move || registry.remove(&session_id)).await
    }

    /// Runs the acquire exclusive lock operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn acquire_exclusive_lock(&self) -> Result<SessionRegistryLock> {
        ensure_private_socket_directory(&self.root, self.owner_uid)?;
        let lock_path = self.registry_lock_file();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        set_private_file_permissions(&lock_path)?;
        flock(&file, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        Ok(SessionRegistryLock { _file: file })
    }

    /// Writes the legacy flat registry for focused migration tests.
    #[cfg(test)]
    pub(super) fn write_legacy_records_for_tests(&self, records: Vec<SessionRecord>) -> Result<()> {
        ensure_private_socket_directory(&self.root, self.owner_uid)?;
        let path = self.registry_file();
        let temporary = self.root.join(format!(
            ".{}.{}.tmp",
            REGISTRY_FILE_NAME,
            std::process::id()
        ));

        let mut data = String::new();
        for record in records {
            record.validate()?;
            data.push_str(&record.encode()?);
            data.push('\n');
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(data.as_bytes())?;
        file.sync_all()?;
        set_private_file_permissions(&temporary)?;
        fs::rename(&temporary, &path)?;
        set_private_file_permissions(&path)?;
        Ok(())
    }
}
