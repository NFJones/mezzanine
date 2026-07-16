//! Append-only audit log writing and optional hash-chain maintenance.
//!
//! The writer is responsible for private filesystem permissions and monotonically
//! increasing event identifiers. Callers provide already-classified records.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::error::{MezError, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

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

        let mut write = AuditWrite {
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
        if self.config.hash_chain {
            let state = audit_hash_state_from_file(&self.config.path, self.next_event_id)?;
            self.previous_hash = state.last_hash;
            write.hash = state.event_hash;
        }

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
pub(super) fn chained_hash(previous_hash: Option<&str>, line: &str) -> String {
    let mut hasher = Sha256::new();
    if let Some(previous_hash) = previous_hash {
        hasher.update(previous_hash.as_bytes());
    }
    hasher.update(b"\0");
    hasher.update(line.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Hash state recovered from a retained audit JSONL file.
struct AuditHashFileState {
    /// Last retained hash in file order.
    last_hash: Option<String>,
    /// Retained hash for the event that triggered the write, if still present.
    event_hash: Option<String>,
}

/// Reads retained audit hash state after synchronous retention enforcement.
fn audit_hash_state_from_file(path: &Path, event_id: u64) -> Result<AuditHashFileState> {
    let data = fs::read_to_string(path)?;
    let mut state = AuditHashFileState {
        last_hash: None,
        event_hash: None,
    };
    for line in data.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(hash) = value.get("hash").and_then(Value::as_str) else {
            continue;
        };
        state.last_hash = Some(hash.to_string());
        if value.get("event_id").and_then(Value::as_u64) == Some(event_id) {
            state.event_hash = Some(hash.to_string());
        }
    }
    Ok(state)
}
