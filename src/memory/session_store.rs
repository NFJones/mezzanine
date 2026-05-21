//! In-memory session-scoped memory store.
//!
//! Session memory enforces session ownership and delegates record validation to
//! shared type and validation helpers.

use super::{MemoryRecord, Result, SessionMemoryStore, scope_belongs_to_session};

impl SessionMemoryStore {
    /// Runs the upsert operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn upsert(&mut self, record: MemoryRecord) -> Result<()> {
        record.validate_for_session()?;
        self.records.insert(record.id.clone(), record);
        Ok(())
    }

    /// Runs the inspect operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn inspect(&self, id: &str) -> Option<&MemoryRecord> {
        self.records.get(id)
    }

    /// Runs the delete operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn delete(&mut self, id: &str) -> bool {
        self.records.remove(id).is_some()
    }

    /// Runs the clear session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn clear_session(&mut self, session_id: &str) -> usize {
        let before = self.records.len();
        self.records
            .retain(|_, record| !scope_belongs_to_session(&record.scope, session_id));
        before - self.records.len()
    }

    /// Runs the clear all operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn clear_all(&mut self) -> usize {
        let before = self.records.len();
        self.records.clear();
        before
    }

    /// Runs the export operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn export(&self) -> Vec<MemoryRecord> {
        self.records.values().cloned().collect()
    }
}
