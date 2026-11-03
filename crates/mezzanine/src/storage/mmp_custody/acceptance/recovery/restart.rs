//! Restart validation and declaration of interrupted acceptance cleanup.
//!
//! The descriptor and the latest batch identify one actual cleanup step, while
//! every surviving chunk is checked as a bounded dense prefix. The original
//! write is no longer available here, so these facts remain quarantined storage
//! evidence. A separate declaration step requires the live bound owner and a
//! complete inventory in the same immediate transaction as the stage update.
//! Neither operation restores a sender or recipient or publishes nonacceptance.

use super::*;
use crate::storage::mmp_custody::{inventory::PendingCustodyRef, ownership::OwnedCustodyStore};
use mez_agent::messaging::MessageAcceptanceDecodeLimits;

impl OwnedCustodyStore<'_> {
    /// Declares one verified interrupted stage under its actual live root owner.
    ///
    /// Complete inventory, the absent marker, the original last batch and the
    /// exact stage preimage share the immediate write transaction. No chunk is
    /// deleted and no sender refusal or native delivery authority is returned.
    /// Any error after commit is uncertain and must be reconciled from the store.
    /// Returns the declared sequence, or `None` when no unfinished stage exists.
    pub(crate) fn declare_restart_cleanup(
        &mut self,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<Option<u64>> {
        self.revalidate()?;
        let result = {
            let store = self.store_mut();
            let transaction = rusqlite::Transaction::new_unchecked(
                &store.connection,
                rusqlite::TransactionBehavior::Immediate,
            )
            .map_err(database_error)?;
            let inventory = store.restart_inventory_in_snapshot(&transaction, limits)?;
            let Some(PendingCustodyRef::Unfinished {
                sequence,
                encoded_bytes,
                present_chunks,
                ..
            }) = inventory.pending
            else {
                transaction.commit().map_err(database_error)?;
                self.revalidate()?;
                return Ok(None);
            };
            let generation = inventory.inventory.generation;
            let stage_key = format!("a1:{sequence:016x}:stage");
            let stage = read_record(&transaction, &stage_key)?.ok_or_else(changed)?;
            let unaffected = unaffected_rows_digest(&transaction, &stage_key)?;
            let initial_generation = generation
                .checked_sub(u64::try_from(present_chunks).map_err(|_| changed())?)
                .and_then(|value| value.checked_sub(1))
                .ok_or_else(changed)?;
            let declared_generation = generation.checked_add(1).ok_or_else(changed)?;
            let completed_generation = declared_generation
                .checked_add(u64::try_from(present_chunks).map_err(|_| changed())?)
                .ok_or_else(changed)?;
            if stage.len() < ABORT_BYTES || completed_generation > i64::MAX as u64 {
                return Err(changed());
            }
            let header_digest: [u8; 32] = Sha256::digest(&stage).into();
            let abort = Abort {
                sequence,
                initial_generation,
                declared_generation,
                chunks: u16::try_from(present_chunks).map_err(|_| changed())?,
                remaining: u16::try_from(present_chunks).map_err(|_| changed())?,
                encoded_bytes: u32::try_from(encoded_bytes).map_err(|_| changed())?,
                header_digest,
                previous_stage_digest: header_digest,
                last_deleted_digest: [0; 32],
            };
            let replacement = abort.padded(stage.len());
            store.commit_in_transaction(
                &transaction,
                generation,
                &[CustodyWrite {
                    key: &stage_key,
                    previous: Some(&header_digest),
                    value: Some(&replacement),
                }],
            )?;
            let after = store.restart_inventory_in_snapshot(&transaction, limits)?;
            let expected = if present_chunks == 0 {
                after.pending.is_none() && after.inventory.aborted_sequences.contains(&sequence)
            } else {
                matches!(after.pending, Some(PendingCustodyRef::Aborting {
                sequence: current, present_chunks: count, ..
            }) if current == sequence && count == present_chunks)
            };
            if !expected || after.inventory.generation != declared_generation {
                return Err(changed());
            }
            if unaffected_rows_digest(&transaction, &stage_key)? != unaffected {
                return Err(changed());
            }
            verify_abort_batch(
                &format!("a1:{sequence:016x}:"),
                &replacement,
                &abort,
                &store.checked_state(&transaction)?.batch,
            )?;
            transaction.commit().map_err(database_error)?;
            Some(sequence)
        };
        self.revalidate()?;
        Ok(result)
    }
}

/// Hashes every checksum-validated row except the exact stage being replaced.
/// Length framing makes key/value boundaries unambiguous, and one bounded row
/// is read at a time. Both passes execute inside the same write transaction.
fn unaffected_rows_digest(
    transaction: &rusqlite::Transaction<'_>,
    excluded_key: &str,
) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut statement = transaction
        .prepare("SELECT CASE WHEN length(CAST(key AS BLOB))<=256 THEN key ELSE NULL END FROM records ORDER BY key COLLATE BINARY")
        .map_err(database_error)?;
    let mut rows = statement.query([]).map_err(database_error)?;
    while let Some(row) = rows.next().map_err(database_error)? {
        let key: Option<String> = row.get(0).map_err(database_error)?;
        let key = key.ok_or_else(changed)?;
        if key == excluded_key {
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
    let state = store.checked_state(transaction)?;
    if u64::try_from(state.generation).map_err(|_| changed())? != generation || state.batch != batch
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::mmp_custody::{
        acceptance::test_fixture::{Fixture, original},
        inventory::PendingCustodyRef,
        ownership::RootOwnership,
    };
    use mez_agent::messaging::MessageAcceptanceWriteOutcome;

    /// Uses the destination's existing finite decode policy for the fixture.
    fn limits() -> MessageAcceptanceDecodeLimits {
        MessageAcceptanceDecodeLimits {
            payload_bytes: 1024 * 1024,
            auxiliary_bytes: 16 * 1024 * 1024,
            recipient_count: 4096,
            recipient_bytes: 1024 * 1024,
        }
    }

    /// Interrupts a real original before its marker, with zero or more chunks.
    fn interrupt(fixture: &Fixture, before_chunks: bool) {
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let (_service, ticket, write) = original("restart-source".repeat(15_000));
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
        let trigger = if before_chunks {
            "CREATE TRIGGER interrupt BEFORE INSERT ON records WHEN NEW.key LIKE '%:chunk:%' BEGIN SELECT RAISE(ABORT,'interrupt chunk'); END;"
        } else {
            "CREATE TRIGGER interrupt BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'interrupt marker'); END;"
        };
        store.connection.execute_batch(trigger).unwrap();
        assert!(matches!(
            write.persist(&mut store.acceptance_port(&plan)),
            MessageAcceptanceWriteOutcome::Unknown(_)
        ));
        store
            .connection
            .execute_batch("DROP TRIGGER interrupt")
            .unwrap();
    }

    /// After reopening without the original write, the actual owner may declare
    /// only a fully inventoried marker-free stage. All chunks remain unchanged,
    /// and repeating the step never advances generation or publishes a receipt.
    #[test]
    fn bound_restart_declaration_preserves_uncommitted_chunks() {
        for before_chunks in [true, false] {
            let fixture = Fixture::new();
            interrupt(&fixture, before_chunks);
            let owner = RootOwnership::acquire(&fixture.0).unwrap();
            let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
            let before = store.inspect_restart_inventory(limits()).unwrap();
            let (sequence, chunks) = match before.pending.unwrap() {
                PendingCustodyRef::Unfinished {
                    sequence,
                    present_chunks,
                    ..
                } => (sequence, present_chunks),
                _ => panic!("expected interrupted stage"),
            };
            let prefix = format!("a1:{sequence:016x}:");
            let source = (0..chunks)
                .map(|index| store.read(&chunk_key(&prefix, index)).unwrap().unwrap())
                .collect::<Vec<_>>();
            let original_generation = before.inventory.generation;
            let mut owned = owner.bind_store(&mut store).unwrap();
            if chunks > 1 {
                assert_ne!(source[0], source[1], "trigger must change the chunk");
                owned.store_mut().connection.execute_batch("CREATE TRIGGER change_chunk AFTER UPDATE OF value ON records WHEN NEW.key='a1:0000000000000001:stage' BEGIN UPDATE records SET value=(SELECT value FROM records WHERE key='a1:0000000000000001:chunk:00000000'),digest=(SELECT digest FROM records WHERE key='a1:0000000000000001:chunk:00000000') WHERE key='a1:0000000000000001:chunk:00000001'; END;").unwrap();
                assert!(owned.declare_restart_cleanup(limits()).is_err());
                assert_eq!(
                    owned
                        .store_mut()
                        .read(&chunk_key(&prefix, 1))
                        .unwrap()
                        .as_ref(),
                    Some(&source[1])
                );
                owned
                    .store_mut()
                    .connection
                    .execute_batch("DROP TRIGGER change_chunk")
                    .unwrap();
            }
            assert_eq!(
                owned.declare_restart_cleanup(limits()).unwrap(),
                Some(sequence)
            );
            let after = owned
                .store_mut()
                .inspect_restart_inventory(limits())
                .unwrap();
            assert_eq!(after.inventory.generation, original_generation + 1);
            if chunks == 0 {
                assert!(after.pending.is_none());
                assert_eq!(after.inventory.aborted_sequences, vec![sequence]);
            } else {
                assert!(matches!(after.pending, Some(PendingCustodyRef::Aborting {
                    sequence: actual, present_chunks, ..
                }) if actual == sequence && present_chunks == chunks));
            }
            for (index, value) in source.iter().enumerate() {
                assert_eq!(
                    owned
                        .store_mut()
                        .read(&chunk_key(&prefix, index))
                        .unwrap()
                        .as_ref(),
                    Some(value)
                );
            }
            assert!(
                owned
                    .store_mut()
                    .read(&format!("{prefix}commit"))
                    .unwrap()
                    .is_none()
            );
            assert_eq!(owned.declare_restart_cleanup(limits()).unwrap(), None);
            assert_eq!(
                owned
                    .store_mut()
                    .inspect_restart_inventory(limits())
                    .unwrap()
                    .inventory
                    .generation,
                original_generation + 1
            );
        }
    }

    /// A trigger inserting a marker during declaration must roll the whole
    /// transaction back. A changed namespace must also refuse the operation;
    /// neither fault can turn the original stage into a qualified abort.
    #[test]
    fn declaration_rolls_back_on_triggered_marker_or_unknown_rows() {
        let fixture = Fixture::new();
        interrupt(&fixture, false);
        let owner = RootOwnership::acquire(&fixture.0).unwrap();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let before = store.inspect_restart_inventory(limits()).unwrap();
        let stage = store.read("a1:0000000000000001:stage").unwrap().unwrap();
        store.connection.execute_batch("CREATE TRIGGER add_marker AFTER UPDATE OF value ON records WHEN NEW.key='a1:0000000000000001:stage' BEGIN INSERT INTO records(key,value,digest) VALUES('a1:0000000000000001:commit',NEW.value,NEW.digest); END;").unwrap();
        let mut owned = owner.bind_store(&mut store).unwrap();
        assert!(owned.declare_restart_cleanup(limits()).is_err());
        assert_eq!(
            owned.store_mut().read("a1:0000000000000001:stage").unwrap(),
            Some(stage.clone())
        );
        assert!(
            owned
                .store_mut()
                .read("a1:0000000000000001:commit")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            owned
                .store_mut()
                .inspect_restart_inventory(limits())
                .unwrap()
                .inventory
                .generation,
            before.inventory.generation
        );
        owned
            .store_mut()
            .connection
            .execute_batch("DROP TRIGGER add_marker")
            .unwrap();
        owned
            .store_mut()
            .commit(
                before.inventory.generation,
                &[CustodyWrite {
                    key: "unexpected",
                    previous: None,
                    value: Some(b"foreign"),
                }],
            )
            .unwrap();
        assert!(owned.declare_restart_cleanup(limits()).is_err());
        assert_eq!(
            owned.store_mut().read("a1:0000000000000001:stage").unwrap(),
            Some(stage)
        );
    }

    /// Replacing the locked pathname cannot authorize restart mutation, even
    /// when a replacement has the same owner label. Restoring the original
    /// directory permits the same bound worker to make the declaration.
    #[test]
    fn declaration_requires_original_locked_directory() {
        let fixture = Fixture::new();
        let moved = Fixture::new();
        let replacement = Fixture::new();
        interrupt(&fixture, false);
        let owner = RootOwnership::acquire(&fixture.0).unwrap();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let before = store
            .inspect_restart_inventory(limits())
            .unwrap()
            .inventory
            .generation;
        let mut owned = owner.bind_store(&mut store).unwrap();
        std::fs::rename(&fixture.0, &moved.0).unwrap();
        drop(MmpCustodyStore::open(&fixture.0, "session").unwrap());
        assert!(owned.declare_restart_cleanup(limits()).is_err());
        std::fs::rename(&fixture.0, &replacement.0).unwrap();
        std::fs::rename(&moved.0, &fixture.0).unwrap();
        assert_eq!(
            owned
                .store_mut()
                .inspect_restart_inventory(limits())
                .unwrap()
                .inventory
                .generation,
            before
        );
        assert_eq!(owned.declare_restart_cleanup(limits()).unwrap(), Some(1));
    }
}
