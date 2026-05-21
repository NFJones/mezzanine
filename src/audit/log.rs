//! Append-only audit log writing and optional hash-chain maintenance.
//!
//! The writer is responsible for private filesystem permissions and monotonically
//! increasing event identifiers. Callers provide already-classified records.

use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::error::{MezError, Result};

use super::json::{insert_hash_field, record_json};
use super::time::current_timestamp;
use super::types::{
    AuditConfig, AuditDeferredWrite, AuditLog, AuditRecord, AuditRetentionPolicy, AuditWrite,
};

impl AuditLog {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(config: AuditConfig) -> Self {
        Self {
            config,
            retention: AuditRetentionPolicy::disabled(),
            next_event_id: 1,
            previous_hash: None,
            defer_writes: false,
            deferred_writes: Vec::new(),
        }
    }

    /// Runs the with retention operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_retention(mut self, retention: AuditRetentionPolicy) -> Self {
        self.retention = retention;
        self
    }

    /// Runs the append operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn append(&mut self, mut record: AuditRecord) -> Result<Option<AuditWrite>> {
        if !self.config.enabled {
            if self.config.required {
                return Err(MezError::forbidden(
                    "audit logging is required but disabled",
                ));
            }
            return Ok(None);
        }

        record.event_id = self.next_event_id;
        if record.timestamp.is_empty() {
            record.timestamp = current_timestamp();
        }
        let record = record.sanitized();
        let mut line = record_json(&record);
        let hash = if self.config.hash_chain {
            let hash = chained_hash(self.previous_hash.as_deref(), &line);
            line = insert_hash_field(line, &hash);
            self.previous_hash = Some(hash.clone());
            Some(hash)
        } else {
            None
        };
        line.push('\n');

        let write = AuditWrite {
            event_id: self.next_event_id,
            hash,
        };

        if self.defer_writes {
            self.next_event_id += 1;
            self.deferred_writes.push(AuditDeferredWrite {
                path: self.config.path.clone(),
                bytes: line.into_bytes(),
                retention: self.retention.clone(),
            });
            return Ok(Some(write));
        }

        if let Some(parent) = self.config.path.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.path)?;
        file.write_all(line.as_bytes())?;
        file.sync_all()?;
        fs::set_permissions(&self.config.path, fs::Permissions::from_mode(0o600))?;
        self.retention.enforce_jsonl(&self.config.path)?;

        self.next_event_id += 1;
        Ok(Some(write))
    }

    /// Enables or disables deferred audit JSONL writes.
    pub fn set_defer_writes(&mut self, defer: bool) {
        self.defer_writes = defer;
    }

    /// Drains audit JSONL records queued for asynchronous persistence.
    pub fn drain_deferred_writes(&mut self) -> Vec<AuditDeferredWrite> {
        std::mem::take(&mut self.deferred_writes)
    }

    /// Runs the path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn path(&self) -> &Path {
        &self.config.path
    }
}

/// Runs the chained hash operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn chained_hash(previous_hash: Option<&str>, line: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    previous_hash.hash(&mut hasher);
    line.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
