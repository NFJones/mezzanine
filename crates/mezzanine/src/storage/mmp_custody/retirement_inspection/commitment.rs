//! Fixed-size commitments to already validated journal rows in one snapshot.
//!
//! Inventory validates each row's bytes, checksum and typed source before its
//! visitor runs. Only then may this helper read the same snapshot's checksum.
//! It retains neither journal keys nor payloads. A digest is descriptive preimage
//! evidence, never a cleanup grant or proof that canonical history may be erased.

use super::*;

/// Version-one deterministic commitment to a sequence of validated journal rows.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct JournalCommitment {
    pub records: u64,
    pub sha256: [u8; 32],
}

pub(super) struct JournalHasher {
    records: u64,
    hash: Sha256,
}

impl JournalHasher {
    pub(super) fn new(domain: &[u8], sequence: u64, source_sha256: &[u8; 32]) -> Self {
        let mut hash = Sha256::new();
        hash.update(domain);
        hash.update(sequence.to_le_bytes());
        hash.update(source_sha256);
        Self { records: 0, hash }
    }

    /// Called once per selected row after strict inventory validation, in its
    /// binary key order. The enclosing transaction prevents checksum substitution
    /// between validation and this bounded read. Unknown/invalid rows never enter.
    pub(super) fn observe(
        &mut self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
    ) -> Result<()> {
        if key.len() > 256 {
            return Err(unavailable());
        }
        let digest: Option<Vec<u8>> = transaction.query_row(
            "SELECT CASE WHEN length(digest)=32 THEN digest ELSE NULL END FROM records WHERE key=?1",
            [key], |row| row.get(0),
        ).map_err(database_error)?;
        let digest = digest.ok_or_else(unavailable)?;
        self.records = self.records.checked_add(1).ok_or_else(unavailable)?;
        self.hash.update((key.len() as u64).to_le_bytes());
        self.hash.update(key.as_bytes());
        self.hash.update(digest);
        Ok(())
    }

    pub(super) fn finish(mut self) -> JournalCommitment {
        self.hash.update(self.records.to_le_bytes());
        JournalCommitment {
            records: self.records,
            sha256: self.hash.finalize().into(),
        }
    }
}
