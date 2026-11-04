//! Bounded preservation proofs for unrelated canonical custody rows.
//!
//! Restart cleanup and native journal writes may change only their exact target
//! keys. Length-framed fingerprints cover every other checksum-validated row
//! before and after SQL effects in the same transaction, including trigger
//! effects. One at-most-256-KiB row is retained at a time. Native serialization
//! adds at most 240 KiB, fitting the lower owner's reserved worker headroom. This
//! is a row-preservation check, not full inventory or native restore authority.

use super::*;

/// Hashes every checksum-validated row except the exact rows being replaced.
/// Length framing makes key/value boundaries unambiguous, and one bounded row
/// is read at a time. Both passes execute inside the same write transaction.
pub(super) fn unaffected_rows_digest(
    transaction: &rusqlite::Transaction<'_>,
    excluded_keys: &[&str],
) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut statement = transaction
        .prepare("SELECT CASE WHEN length(CAST(key AS BLOB))<=256 THEN key ELSE NULL END FROM records ORDER BY key COLLATE BINARY")
        .map_err(database_error)?;
    let mut rows = statement.query([]).map_err(database_error)?;
    while let Some(row) = rows.next().map_err(database_error)? {
        let key: Option<String> = row.get(0).map_err(database_error)?;
        let key = key.ok_or_else(changed)?;
        if excluded_keys.contains(&key.as_str()) {
            continue;
        }
        let value = read_record(transaction, &key)?.ok_or_else(changed)?;
        hash.update(
            u64::try_from(key.len())
                .map_err(|_| changed())?
                .to_le_bytes(),
        );
        hash.update(key.as_bytes());
        hash.update(
            u64::try_from(value.len())
                .map_err(|_| changed())?
                .to_le_bytes(),
        );
        hash.update(Sha256::digest(&value));
    }
    Ok(hash.finalize().into())
}

/// Content-free diagnostic for an unavailable original preservation proof.
fn changed() -> MezError {
    MezError::invalid_state("MMP custody unrelated row preservation unavailable")
}
