//! Conversation-local startup refusal without discarding accepted append source.
//!
//! Canonical filename routing establishes the affected conversation before its
//! receipt is read. Unroutable inventory remains fatal. Every healthy inventory
//! is preflighted before any replay; failed conversations retain all their files
//! and are excluded from automatic hydration by the runtime. Strict recovery and
//! qualified native restoration continue to propagate failures.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

impl AgentTranscriptStore {
    /// Recovers independent ordinary append owners, returning explicit local
    /// refusals. A refusal grants neither append settlement nor usable history.
    /// Namespace/routing failures still fail the complete startup operation.
    pub(crate) fn recover_append_receipts_for_startup(&self) -> Result<BTreeMap<String, MezError>> {
        let mut subjects = BTreeSet::new();
        for path in self.append_receipt_paths()? {
            if let Some(id) = receipt_conversation_id(&path)? {
                subjects.insert(id.to_owned());
            }
        }
        let mut failures = BTreeMap::new();
        let mut ready = Vec::new();
        for id in subjects {
            let lease = match self.acquire_conversation_lock(&id) {
                Ok(lease) => lease,
                Err(error) => {
                    failures.insert(id, error);
                    continue;
                }
            };
            // Shared namespace failures are never attributed to this subject.
            // Revalidate after locking so a newly appeared unknown name cannot
            // hide inside a local source-read refusal.
            let paths = self.append_receipt_paths()?;
            for path in &paths {
                receipt_conversation_id(path)?;
            }
            let read = self.read_locked_append_receipts_at(&[&id], paths);
            drop(lease);
            let receipts = match read {
                Ok(receipts) => receipts
                    .into_iter()
                    .filter(|(_, receipt, _)| receipt.custody_plan().is_none())
                    .collect::<Vec<_>>(),
                Err(error) => {
                    failures.insert(id, error);
                    continue;
                }
            };
            match self.preflight_append_recovery(&receipts) {
                Ok(()) => ready.push((id, receipts)),
                Err(error) => {
                    failures.insert(id, error);
                }
            }
        }
        for (id, receipts) in ready {
            if let Err(error) = self.replay_append_receipts(receipts) {
                failures.insert(id, error);
            }
        }
        Ok(failures)
    }
}

#[cfg(test)]
mod tests;

/// Refused source and dependent children outlive their active pane bindings.
/// This sticky hold grants no source/restore authority; only explicit deletion
/// may discard it. Retention checks presence under the conversation lease.
const REFUSAL_DIRECTORY: &str = ".startup-refusals-v1";

impl AgentTranscriptStore {
    /// Durably protects isolated history from automatic age/count retention.
    /// Existing holds remain conservative even if the receipt was later repaired.
    pub(crate) fn retain_startup_refusal(&self, conversation: &str) -> Result<()> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        validate_conversation_id(conversation)?;
        let _lease = self.acquire_conversation_lock_without_recovery(conversation)?;
        self.ensure_store_dir()?;
        let directory = self.root.join(REFUSAL_DIRECTORY);
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(MezError::invalid_state(
                "startup refusal namespace must be an owned private directory",
            ));
        }
        let path = directory.join(conversation);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file.sync_all()?,
            // An existing entry already protects retention; it supplies no
            // readable evidence or permission to follow a substituted path.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        fs::File::open(&directory)?.sync_all()?;
        fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    /// Presence is a conservative retention veto, never a restore capability.
    pub(in crate::storage::transcript) fn startup_refusal_is_retained(
        &self,
        conversation: &str,
    ) -> Result<bool> {
        validate_conversation_id(conversation)?;
        match fs::symlink_metadata(self.root.join(REFUSAL_DIRECTORY).join(conversation)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// A retained refusal is an explicit model-history adoption veto. Clearing
    /// it after repair supplies no authority and never skips source validation.
    pub(crate) fn require_no_startup_refusal(&self, conversation: &str) -> Result<()> {
        if self.startup_refusal_is_retained(conversation)? {
            return Err(MezError::conflict(
                "conversation startup recovery was refused; repair the saved source and clear its startup refusal before resuming",
            ));
        }
        Ok(())
    }
}
