//! Read-only validation of a partial v2 abort within a store snapshot.
//!
//! The descriptor and the latest batch identify one actual cleanup step, while
//! every surviving chunk is checked as a bounded dense prefix. The original
//! write is no longer available here, so these facts remain quarantined storage
//! evidence. They do not establish nonacceptance, authorize deletion, or
//! replace complete inventory and the actual exclusive worker owner.

use super::*;

/// Validates the supplied stage and its surviving prefix in the same snapshot.
/// Returns the original encoded size and declared cleanup chunk count only.
pub(super) fn inspect_partial_abort(
    store: &MmpCustodyStore,
    transaction: &rusqlite::Transaction<'_>,
    sequence: u64,
    generation: u64,
    batch: &[u8],
    present_chunks: usize,
    stage: &[u8],
) -> Result<(usize, usize)> {
    if sequence == 0 || !std::ptr::eq(&store.connection, &**transaction) {
        return Err(changed());
    }
    let (stored_generation, stored_batch, stored_id): (i64, Option<Vec<u8>>, Option<Vec<u8>>) = transaction.query_row(
        "SELECT generation,CASE WHEN length(batch)=32 THEN batch ELSE NULL END,CASE WHEN length(store_id)=16 THEN store_id ELSE NULL END FROM state WHERE singleton=1",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).map_err(database_error)?;
    if u64::try_from(stored_generation).map_err(|_| changed())? != generation
        || stored_batch.as_deref() != Some(batch)
        || stored_id.as_deref() != Some(store.store_id.as_slice())
    {
        return Err(changed());
    }
    let prefix = format!("a1:{sequence:016x}:");
    if read_record(transaction, &format!("{prefix}stage"))?.as_deref() != Some(stage)
        || read_record(transaction, &format!("{prefix}commit"))?.is_some()
    {
        return Err(changed());
    }
    let abort = Abort::decode(stage)?;
    let encoded_bytes = abort.encoded_bytes as usize;
    let total_chunks = encoded_bytes.div_ceil(CHUNK_BYTES);
    let declared = abort
        .initial_generation
        .checked_add(u64::from(abort.chunks))
        .and_then(|value| value.checked_add(2))
        .ok_or_else(changed)?;
    let completed = abort
        .declared_generation
        .checked_add(u64::from(abort.chunks))
        .ok_or_else(changed)?;
    if abort.sequence != sequence
        || encoded_bytes == 0
        || encoded_bytes > store.limits.logical_bytes
        || abort.remaining == 0
        || abort.remaining > abort.chunks
        || usize::from(abort.remaining) != present_chunks
        || usize::from(abort.chunks) > total_chunks
        || abort.declared_generation != declared
        || completed > i64::MAX as u64
        || generation
            != abort
                .declared_generation
                .checked_add(u64::from(abort.chunks - abort.remaining))
                .ok_or_else(changed)?
    {
        return Err(changed());
    }
    let header = |digest| Header {
        record_version: RECORD_VERSION,
        sequence,
        encoded_bytes,
        chunks: total_chunks,
        digest,
    };
    let shortest = serde_json::to_vec(&header([0; 32]))
        .map_err(encoding_error)?
        .len();
    let longest = serde_json::to_vec(&header([255; 32]))
        .map_err(encoding_error)?
        .len();
    if !(shortest..=longest).contains(&stage.len()) {
        return Err(changed());
    }
    verify_abort_batch(&prefix, stage, &abort, batch)?;
    for index in 0..present_chunks {
        let value = read_record(transaction, &chunk_key(&prefix, index))?.ok_or_else(changed)?;
        let offset = index.checked_mul(CHUNK_BYTES).ok_or_else(changed)?;
        let expected = encoded_bytes
            .checked_sub(offset)
            .ok_or_else(changed)?
            .min(CHUNK_BYTES);
        if value.len() != expected {
            return Err(changed());
        }
    }
    Ok((encoded_bytes, usize::from(abort.chunks)))
}
