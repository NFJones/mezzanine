//! Persistent memory store implementation.
//!
//! This module owns durable file I/O, private permissions, validation for
//! persisted records, and stable record ordering.

use super::{
    BTreeMap, MemoryRecord, MezError, OpenOptions, Path, PathBuf, PersistentMemoryStore, Read,
    Result, Write, fs, set_private_dir_permissions, set_private_file_permissions,
};

impl PersistentMemoryStore {
    /// Runs the under config root operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn under_config_root(config_root: impl Into<PathBuf>) -> Self {
        Self {
            path: config_root.into().join("memory.tsv"),
        }
    }

    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Runs the path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs the list operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn list(&self) -> Result<Vec<MemoryRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let mut data = String::new();
        fs::File::open(&self.path)?.read_to_string(&mut data)?;
        data.lines()
            .filter(|line| !line.trim().is_empty())
            .map(MemoryRecord::decode)
            .collect()
    }

    /// Runs the inspect operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn inspect(&self, id: &str) -> Result<MemoryRecord> {
        self.list()?
            .into_iter()
            .find(|record| record.id == id)
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "memory not found"))
    }

    /// Runs the upsert operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn upsert(&self, record: MemoryRecord) -> Result<()> {
        record.validate_for_persistence()?;
        let mut records = self
            .list()?
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect::<BTreeMap<_, _>>();
        records.insert(record.id.clone(), record);
        self.write_all(records.into_values().collect())
    }

    /// Runs the edit content operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn edit_content(
        &self,
        id: &str,
        content: impl Into<String>,
        updated_at_unix_seconds: u64,
        explicit_sensitive_consent: bool,
    ) -> Result<MemoryRecord> {
        let mut record = self.inspect(id)?;
        record.content = content.into();
        record.updated_at_unix_seconds = updated_at_unix_seconds;
        record.explicit_sensitive_consent = explicit_sensitive_consent;
        self.upsert(record.clone())?;
        Ok(record)
    }

    /// Runs the delete operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn delete(&self, id: &str) -> Result<bool> {
        if !self.path.exists() {
            return Ok(false);
        }
        let mut deleted = false;
        let records = self
            .list()?
            .into_iter()
            .filter(|record| {
                let keep = record.id != id;
                if !keep {
                    deleted = true;
                }
                keep
            })
            .collect::<Vec<_>>();
        if deleted {
            self.write_all(records)?;
        }
        Ok(deleted)
    }

    /// Runs the export tsv operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn export_tsv(&self) -> Result<String> {
        let mut output = String::new();
        for record in self.list()? {
            output.push_str(&record.encode()?);
            output.push('\n');
        }
        Ok(output)
    }

    /// Runs the write all operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_all(&self, records: Vec<MemoryRecord>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
            set_private_dir_permissions(parent)?;
        }
        let mut data = String::new();
        for record in records {
            data.push_str(&record.encode()?);
            data.push('\n');
        }
        let temporary = self.path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(data.as_bytes())?;
        file.sync_all()?;
        set_private_file_permissions(&temporary)?;
        fs::rename(&temporary, &self.path)?;
        set_private_file_permissions(&self.path)?;
        Ok(())
    }
}
