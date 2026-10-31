//! Runtime-local audit writer identity and exclusive staged append ownership.
//!
//! Actual attachment/removal allocates a new identity even for equal paths and
//! policies. Appending records or committing an exclusively staged continuation
//! preserves the original writer. Identity proves registration only, never disk
//! persistence. Staged callers cannot extract or replace the cloned writer and
//! hold the persistence component exclusively until commit or error.

use super::RuntimePersistenceComponent;
use crate::error::{MezError, Result};
use crate::security::audit::{AuditLog, AuditRecord, AuditWrite};
use std::sync::Arc;

/// Opaque allocation identity for one attached writer, independent of labels.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeAuditWriterIdentity(Arc<()>);

/// Exclusive append-only staged continuation; the concrete clone never escapes.
pub(crate) struct RuntimeStagedAuditWriter<'a> {
    /// Exclusive original owner retained through success, error or unwinding.
    source: &'a mut RuntimePersistenceComponent,
    /// Only successful installation acknowledges the complete staged operation.
    committed: bool,
    /// Original writer state cloned under the exclusive persistence borrow.
    writer: Option<AuditLog>,
    /// Entered append may have performed I/O even if its acknowledgement failed.
    append_attempted: bool,
}

impl RuntimeStagedAuditWriter<'_> {
    /// Appends through the existing writer semantics, or records no obligation
    /// when no writer was attached. Append errors leave the stage uncommitted;
    /// this does not claim rollback of already performed physical writes.
    pub(crate) fn append(&mut self, record: AuditRecord) -> Result<Option<AuditWrite>> {
        match self.writer.as_mut() {
            Some(writer) => {
                self.append_attempted = true;
                writer.append(record)
            }
            None => Ok(None),
        }
    }

    /// Installs the exact staged continuation and acknowledges its ownership.
    /// No fallible operation follows installation, and Drop becomes inert.
    fn commit(mut self) {
        self.source.audit_log = self.writer.take();
        self.committed = true;
    }
}

impl Drop for RuntimeStagedAuditWriter<'_> {
    /// Retains entered append custody on both returned errors and unwinding.
    /// This destructor performs no I/O and never certifies a healthy stream.
    fn drop(&mut self) {
        if !self.committed && self.append_attempted {
            self.source.audit_log = self.writer.take();
            self.source.audit_writer_unproven = true;
        }
    }
}

impl RuntimePersistenceComponent {
    /// Captures an attached writer registration without cloning event/hash state.
    pub(crate) fn audit_writer_identity(&self) -> Option<RuntimeAuditWriterIdentity> {
        self.audit_log
            .as_ref()
            .map(|_| RuntimeAuditWriterIdentity(Arc::clone(&self.audit_writer_identity)))
    }

    /// Compares actual registration allocations, never equal paths or settings.
    pub(crate) fn audit_writer_is_current(&self, identity: &RuntimeAuditWriterIdentity) -> bool {
        self.audit_log.is_some() && Arc::ptr_eq(&self.audit_writer_identity, &identity.0)
    }

    /// Reports whether failed staged append custody remains unresolved. This
    /// does not infer a disk failure from a missing callback or retry a record.
    pub(crate) fn audit_writer_is_unproven(&self) -> bool {
        self.audit_writer_unproven
    }

    /// Runs a synchronous staged append transaction under exclusive ownership.
    /// The caller receives only append access and cannot install another writer
    /// or interleave appends to the live original. Successful installation keeps
    /// the original identity. A failed stage with an entered append preserves
    /// its state/deferred bytes and marks the stream unproven for native use.
    /// The same custody survives unwinding. Without an entered append, errors
    /// preserve the live writer. Existing I/O
    /// cannot be rolled back and no retry or healthy-stream claim is implied.
    pub(crate) fn with_staged_audit_log<T>(
        &mut self,
        operation: impl FnOnce(&mut RuntimeStagedAuditWriter<'_>) -> Result<T>,
    ) -> Result<T> {
        if self.audit_writer_unproven {
            return Err(MezError::invalid_state(
                "audit writer has an unproven staged append",
            ));
        }
        let writer = self.audit_log.clone();
        let mut staged = RuntimeStagedAuditWriter {
            source: self,
            committed: false,
            writer,
            append_attempted: false,
        };
        let result = operation(&mut staged)?;
        staged.commit();
        Ok(result)
    }
}
