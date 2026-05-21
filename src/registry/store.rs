//! Filesystem-backed session registry operations.
//!
//! This module owns registry loading, persistence, lifecycle mutation, and
//! private runtime-directory handling.

use super::{
    BTreeMap, OpenOptions, Path, PathBuf, REGISTRY_FILE_NAME, Read, Result, SessionRecord,
    SessionRegistry, Write, decode_records, ensure_private_socket_directory, fs,
    set_private_file_permissions,
};
use rustix::fs::{FlockOperation, flock};
use tokio::fs as tokio_fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        let path = self.registry_file();
        if !path.exists() {
            return Ok(Vec::new());
        }

        let mut data = String::new();
        fs::File::open(path)?.read_to_string(&mut data)?;
        let mut records = decode_records(&data)?;
        records.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        Ok(records)
    }

    /// Runs the list async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn list_async(&self) -> Result<Vec<SessionRecord>> {
        let path = self.registry_file();
        let mut data = String::new();
        match tokio_fs::File::open(path).await {
            Ok(mut file) => {
                file.read_to_string(&mut data).await?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.into()),
        }
        let mut records = decode_records(&data)?;
        records.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        Ok(records)
    }

    /// Runs the get operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn get(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        Ok(self
            .list()?
            .into_iter()
            .find(|record| record.session_id == session_id))
    }

    /// Runs the prune stale operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn prune_stale(&self) -> Result<usize> {
        if !self.registry_file().exists() {
            return Ok(0);
        }

        let _lock = self.acquire_exclusive_lock()?;
        let records = self.list()?;
        let original_len = records.len();
        let live_records = records
            .into_iter()
            .filter(|record| record.socket_path.exists())
            .collect::<Vec<_>>();
        let pruned = original_len.saturating_sub(live_records.len());
        if pruned > 0 {
            self.write_records(live_records)?;
        }
        Ok(pruned)
    }

    /// Runs the upsert operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn upsert(&self, record: SessionRecord) -> Result<()> {
        record.validate()?;
        let _lock = self.acquire_exclusive_lock()?;
        ensure_private_socket_directory(&self.root, self.owner_uid)?;

        let mut by_id = self
            .list()?
            .into_iter()
            .map(|record| (record.session_id.clone(), record))
            .collect::<BTreeMap<_, _>>();
        by_id.insert(record.session_id.clone(), record);
        self.write_records(by_id.into_values().collect())
    }

    /// Runs the upsert async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn upsert_async(&self, record: SessionRecord) -> Result<()> {
        record.validate()?;
        let _lock = self.acquire_exclusive_lock()?;
        ensure_private_socket_directory(&self.root, self.owner_uid)?;

        let mut by_id = self
            .list_async()
            .await?
            .into_iter()
            .map(|record| (record.session_id.clone(), record))
            .collect::<BTreeMap<_, _>>();
        by_id.insert(record.session_id.clone(), record);
        self.write_records_async(by_id.into_values().collect())
            .await
    }

    /// Runs the remove operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn remove(&self, session_id: &str) -> Result<bool> {
        if !self.registry_file().exists() {
            return Ok(false);
        }

        let _lock = self.acquire_exclusive_lock()?;
        let mut removed = false;
        let records = self
            .list()?
            .into_iter()
            .filter(|record| {
                let keep = record.session_id != session_id;
                if !keep {
                    removed = true;
                }
                keep
            })
            .collect::<Vec<_>>();

        if removed {
            self.write_records(records)?;
        }
        Ok(removed)
    }

    /// Runs the remove async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn remove_async(&self, session_id: &str) -> Result<bool> {
        match tokio_fs::metadata(self.registry_file()).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }

        let _lock = self.acquire_exclusive_lock()?;
        let mut removed = false;
        let records = self
            .list_async()
            .await?
            .into_iter()
            .filter(|record| {
                let keep = record.session_id != session_id;
                if !keep {
                    removed = true;
                }
                keep
            })
            .collect::<Vec<_>>();

        if removed {
            self.write_records_async(records).await?;
        }
        Ok(removed)
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

    /// Runs the write records operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_records(&self, records: Vec<SessionRecord>) -> Result<()> {
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

    /// Runs the write records async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn write_records_async(&self, records: Vec<SessionRecord>) -> Result<()> {
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

        let mut file = tokio_fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .await?;
        file.write_all(data.as_bytes()).await?;
        file.sync_all().await?;
        set_private_file_permissions(&temporary)?;
        tokio_fs::rename(&temporary, &path).await?;
        set_private_file_permissions(&path)?;
        Ok(())
    }
}
