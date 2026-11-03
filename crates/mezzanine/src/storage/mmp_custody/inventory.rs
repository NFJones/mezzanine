//! Bounded descriptive inventory of the private MMP custody projection.
//!
//! A single SQLite snapshot covers the entire key scan and every acceptance or
//! ACK source read. Results contain only verified storage references, not live
//! registrations, native commits, cursors or a restored mailbox. Unknown rows,
//! corrupt originals and unfinished stages fail closed; a caller must still
//! reconcile unfinished work, fold all decisions, prove original native ownership
//! and enforce runtime/physical budgets before publishing recovered state.

use mez_agent::messaging::MessageAcceptanceDecodeLimits;

use super::*;

/// Ceiling on retained inventory metadata, independent of streamed payloads.
const MAX_INVENTORY_METADATA: usize = 8 * 1024 * 1024;
/// Per-reference charges include conservative vector capacity and index overhead.
const OCCURRENCE_CHARGE: usize = 256;
const ACK_CHARGE: usize = 256;
const ABORT_CHARGE: usize = 64;

/// One committed original identified by verified storage bytes, not live rights.
pub(crate) struct StoredOccurrenceRef {
    /// Original canonical sequence; gaps can be valid cancelled reservations.
    pub sequence: u64,
    /// Full committed source digest, not a process or registration capability.
    pub digest: [u8; 32],
    /// Exact encoded source size; payload remains cold on disk.
    pub encoded_bytes: usize,
    /// Original captured recipient count, without resolving current membership.
    pub recipient_count: usize,
}

/// Complete descriptive snapshot; its allocation identity is not authority.
pub(crate) struct CustodyInventory {
    /// Store allocation observed inside the snapshot.
    pub store_id: Vec<u8>,
    /// Store generation observed with all inventoried records.
    pub generation: u64,
    /// Fully verified original acceptance references in sequence order.
    pub occurrences: Vec<StoredOccurrenceRef>,
    /// Content-addressed ACK keys verified against their exact sources.
    pub acknowledgement_keys: Vec<String>,
    /// Fully cleaned abort sequences, never accepted messages.
    pub aborted_sequences: Vec<u64>,
}

/// One canonical occurrence namespace under the ordered store-key scan.
struct Group {
    sequence: u64,
    chunks: usize,
    stage: bool,
    commit: bool,
}

/// Recognized private row keys; no text from a row chooses a live recipient.
#[derive(Clone, Copy)]
enum Key<'a> {
    Chunk(u64, usize),
    Stage(u64),
    Commit(u64),
    Acknowledgement(&'a str),
}

impl MmpCustodyStore {
    /// Validates a bounded complete row inventory in one consistent snapshot.
    ///
    /// `limits` must cover the original admission policy. An uncommitted or
    /// partially cleaned stage is explicitly unavailable, not accepted mail or
    /// permission to delete it. The returned references remain descriptive:
    /// full journal folding, trusted original ownership, quotas and native input
    /// recovery must complete separately before any delivery is published.
    pub(crate) fn inventory_custody(
        &self,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<CustodyInventory> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let inventory = self.inventory_in_snapshot(&transaction, limits)?;
        transaction.commit().map_err(database_error)?;
        Ok(inventory)
    }

    /// Scans and verifies the caller's exact same-store transaction snapshot.
    /// Its private result must not escape before that transaction commits.
    fn inventory_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<CustodyInventory> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid("snapshot belongs to another store"));
        }
        let (generation, batch, store_id): (i64, Option<Vec<u8>>, Option<Vec<u8>>) = transaction.query_row(
            "SELECT generation,CASE WHEN length(batch) IN (0,32) THEN batch ELSE NULL END,CASE WHEN length(store_id)=16 THEN store_id ELSE NULL END FROM state WHERE singleton=1", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).map_err(database_error)?;
        let generation = u64::try_from(generation).map_err(|_| invalid("invalid generation"))?;
        let batch = batch.ok_or_else(|| invalid("invalid batch framing"))?;
        let store_id = store_id.ok_or_else(|| invalid("invalid store identity"))?;
        if store_id != self.store_id
            || (generation == 0 && !batch.is_empty())
            || (generation > 0 && batch.len() != 32)
        {
            return Err(invalid("store identity or generation framing changed"));
        }
        // Even the generic store can add at most MAX_BATCH_RECORDS in one
        // generation. Updates and deletions cannot increase this lower bound.
        let row_count: i64 = transaction
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .map_err(database_error)?;
        let row_count = usize::try_from(row_count).map_err(|_| invalid("invalid row count"))?;
        let minimum_generation = u64::try_from(row_count.div_ceil(MAX_BATCH_RECORDS))
            .map_err(|_| invalid("row count overflow"))?;
        if row_count > self.limits.records || generation < minimum_generation {
            return Err(invalid("generation cannot account for stored rows"));
        }
        let mut inventory = CustodyInventory {
            store_id,
            generation,
            occurrences: Vec::new(),
            acknowledgement_keys: Vec::new(),
            aborted_sequences: Vec::new(),
        };
        let mut metadata = 0usize;
        let mut group: Option<Group> = None;
        let mut seen_rows = 0usize;
        {
            let mut statement = transaction.prepare(
                "SELECT CASE WHEN length(CAST(key AS BLOB))<=256 THEN key ELSE NULL END FROM records ORDER BY key COLLATE BINARY"
            ).map_err(database_error)?;
            let mut rows = statement.query([]).map_err(database_error)?;
            while let Some(row) = rows.next().map_err(database_error)? {
                seen_rows = seen_rows
                    .checked_add(1)
                    .ok_or_else(|| invalid("row count overflow"))?;
                if seen_rows > self.limits.records {
                    return Err(invalid("row count exceeds policy"));
                }
                let key: Option<String> = row.get(0).map_err(database_error)?;
                let key = key.ok_or_else(|| invalid("row key exceeds policy"))?;
                match parse_key(&key)? {
                    Key::Acknowledgement(key) => {
                        if let Some(previous) = group.take() {
                            self.finish_inventory_group(
                                transaction,
                                previous,
                                limits,
                                &mut metadata,
                                &mut inventory,
                            )?;
                        }
                        // Validate every ACK and all exact source/binding evidence
                        // before retaining a compact key, not the ACK's payloads.
                        let _ =
                            self.read_stored_acknowledgement_in_snapshot(transaction, key, limits)?;
                        charge(&mut metadata, ACK_CHARGE)?;
                        inventory
                            .acknowledgement_keys
                            .try_reserve(1)
                            .map_err(|_| invalid("inventory allocation failed"))?;
                        let mut owned = String::new();
                        owned
                            .try_reserve_exact(key.len())
                            .map_err(|_| invalid("inventory allocation failed"))?;
                        owned.push_str(key);
                        inventory.acknowledgement_keys.push(owned);
                    }
                    item => {
                        let sequence = match item {
                            Key::Chunk(sequence, _)
                            | Key::Stage(sequence)
                            | Key::Commit(sequence) => sequence,
                            Key::Acknowledgement(_) => {
                                return Err(invalid("unexpected acknowledgment key"));
                            }
                        };
                        if group
                            .as_ref()
                            .is_some_and(|current| current.sequence != sequence)
                        {
                            let previous = group
                                .take()
                                .ok_or_else(|| invalid("occurrence group disappeared"))?;
                            self.finish_inventory_group(
                                transaction,
                                previous,
                                limits,
                                &mut metadata,
                                &mut inventory,
                            )?;
                        }
                        let current = group.get_or_insert(Group {
                            sequence,
                            chunks: 0,
                            stage: false,
                            commit: false,
                        });
                        match item {
                            Key::Chunk(_, index)
                                if !current.stage && !current.commit && index == current.chunks =>
                            {
                                current.chunks = current
                                    .chunks
                                    .checked_add(1)
                                    .ok_or_else(|| invalid("chunk count overflow"))?
                            }
                            Key::Commit(_) if !current.commit && !current.stage => {
                                current.commit = true
                            }
                            Key::Stage(_) if !current.stage => current.stage = true,
                            _ => return Err(invalid("occurrence row ordering is invalid")),
                        }
                    }
                }
            }
        }
        if seen_rows != row_count {
            return Err(invalid("row count changed inside snapshot"));
        }
        if let Some(previous) = group {
            self.finish_inventory_group(
                transaction,
                previous,
                limits,
                &mut metadata,
                &mut inventory,
            )?;
        }
        Ok(inventory)
    }

    /// Checks one complete occurrence or a fully cleaned abort in the snapshot.
    fn finish_inventory_group(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        group: Group,
        limits: MessageAcceptanceDecodeLimits,
        metadata: &mut usize,
        inventory: &mut CustodyInventory,
    ) -> Result<()> {
        if !group.stage {
            return Err(invalid("occurrence stage is missing"));
        }
        if group.commit {
            let (source, digest, encoded_bytes) =
                self.read_accepted_source_in_snapshot(transaction, group.sequence, limits)?;
            if group.chunks != encoded_bytes.div_ceil(64 * 1024) {
                return Err(invalid("occurrence chunks are incomplete or extra"));
            }
            charge(metadata, OCCURRENCE_CHARGE)?;
            inventory
                .occurrences
                .try_reserve(1)
                .map_err(|_| invalid("inventory allocation failed"))?;
            inventory.occurrences.push(StoredOccurrenceRef {
                sequence: group.sequence,
                digest,
                encoded_bytes,
                recipient_count: source.recipient_bindings().len(),
            });
        } else {
            let stage_key = format!("a1:{:016x}:stage", group.sequence);
            let stage = self
                .read(&stage_key)?
                .ok_or_else(|| invalid("occurrence stage is missing"))?;
            if group.chunks != 0 || !stage.starts_with(b"MMPABT02") {
                return Err(invalid("uncommitted occurrence requires reconciliation"));
            }
            super::acceptance::validate_finalized_abort(
                &stage,
                group.sequence,
                inventory.generation,
                self.limits.logical_bytes,
            )?;
            charge(metadata, ABORT_CHARGE)?;
            inventory
                .aborted_sequences
                .try_reserve(1)
                .map_err(|_| invalid("inventory allocation failed"))?;
            inventory.aborted_sequences.push(group.sequence);
        }
        Ok(())
    }
}

/// Parses only exact lowercase canonical internal keys and bounded indexes.
fn parse_key(key: &str) -> Result<Key<'_>> {
    if let Some(rest) = key.strip_prefix("a1:") {
        let (sequence, part) = rest
            .split_once(':')
            .ok_or_else(|| invalid("occurrence key is invalid"))?;
        let sequence = hexadecimal(sequence, 16)?;
        if sequence == 0 {
            return Err(invalid("occurrence sequence is zero"));
        }
        return match part {
            "stage" => Ok(Key::Stage(sequence)),
            "commit" => Ok(Key::Commit(sequence)),
            _ => {
                let index = part
                    .strip_prefix("chunk:")
                    .ok_or_else(|| invalid("occurrence key is unknown"))?;
                let index = usize::try_from(hexadecimal(index, 8)?)
                    .map_err(|_| invalid("chunk index is invalid"))?;
                Ok(Key::Chunk(sequence, index))
            }
        };
    }
    if let Some(digest) = key.strip_prefix("k1:")
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Ok(Key::Acknowledgement(key));
    }
    Err(invalid("unknown record key"))
}

/// Parses fixed-width canonical lowercase hexadecimal without intermediate data.
fn hexadecimal(value: &str, length: usize) -> Result<u64> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("hexadecimal key is invalid"));
    }
    u64::from_str_radix(value, 16).map_err(|_| invalid("hexadecimal key overflow"))
}

/// Charges retained references before growing any inventory container.
fn charge(total: &mut usize, amount: usize) -> Result<()> {
    *total = total
        .checked_add(amount)
        .filter(|total| *total <= MAX_INVENTORY_METADATA)
        .ok_or_else(|| invalid("inventory metadata capacity"))?;
    Ok(())
}

/// Gives a bounded diagnostic without echoing stored values.
fn invalid(detail: &str) -> MezError {
    MezError::invalid_state(format!("MMP custody inventory {detail}"))
}

#[cfg(test)]
mod tests;
