//! Bounded descriptive inventory of the private MMP custody projection.
//!
//! A single SQLite snapshot covers the entire key scan and every acceptance or
//! ACK source read. Results contain only verified storage references, not live
//! registrations, native commits, cursors or a restored mailbox. Strict inventory
//! rejects unfinished stages. A separate restart inspection can quarantine one
//! bounded unfinished stage while validating every other row in that snapshot;
//! neither result authorizes cleanup, identity restoration or delivery.

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
    /// Logical charge already retained by this inventory, shared with its fold.
    pub(super) retained_metadata: usize,
}

/// A quarantined, unresolved occurrence, never an acceptance or cleanup right.
pub(crate) enum PendingCustodyRef {
    /// Private original stage with a verified dense source prefix.
    Unfinished {
        /// Original namespace sequence, without a live allocator right.
        sequence: u64,
        /// Declared encoded source size, not a materialized payload.
        encoded_bytes: usize,
        /// Checksum-validated consecutive source chunks currently present.
        present_chunks: usize,
        /// Chunks needed for the complete declared source.
        total_chunks: usize,
    },
    /// Partially cleaned v2 abort with verified remaining prefix and last batch.
    Aborting {
        /// Original namespace sequence, without a live allocator right.
        sequence: u64,
        /// Encoded source size retained by the abort descriptor.
        encoded_bytes: usize,
        /// Checksum-validated consecutive source chunks currently present.
        present_chunks: usize,
        /// Chunks present when the abort was declared.
        total_chunks: usize,
    },
}

/// Complete descriptive snapshot with one optional quarantined occurrence.
pub(crate) struct RestartInventory {
    /// All fully verified accepted, acknowledged and finalized-abort references.
    pub inventory: CustodyInventory,
    /// At most one validated unresolved projection, never a live right.
    pub pending: Option<PendingCustodyRef>,
}

/// Internal scan mode and quarantine, retained until the transaction commits.
struct PendingScan {
    /// Whether the caller requested a quarantined unresolved occurrence.
    enabled: bool,
    /// Sole unresolved projection found so far, if any.
    found: Option<PendingCustodyRef>,
    /// Bounded last-batch digest from the same store snapshot.
    batch: Vec<u8>,
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

    /// Inspects the complete store and quarantines at most one unresolved
    /// occurrence in the same snapshot. Returns no live or cleanup authority.
    pub(crate) fn inspect_restart_inventory(
        &self,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<RestartInventory> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let result = self.restart_inventory_in_snapshot(&transaction, limits)?;
        transaction.commit().map_err(database_error)?;
        Ok(result)
    }

    /// Validates restart inventory inside the caller's same-store transaction.
    /// Its descriptive result remains private until that transaction commits.
    pub(in crate::storage::mmp_custody) fn restart_inventory_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<RestartInventory> {
        let mut pending = PendingScan {
            enabled: true,
            found: None,
            batch: Vec::new(),
        };
        let inventory = self.scan_inventory(transaction, limits, &mut pending)?;
        Ok(RestartInventory {
            inventory,
            pending: pending.found,
        })
    }

    /// Scans and verifies the caller's exact same-store transaction snapshot.
    /// Its private result must not escape before that transaction commits.
    pub(in crate::storage::mmp_custody) fn inventory_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<CustodyInventory> {
        let mut pending = PendingScan {
            enabled: false,
            found: None,
            batch: Vec::new(),
        };
        self.scan_inventory(transaction, limits, &mut pending)
    }

    /// Performs the shared bounded scan under the caller's exact transaction.
    fn scan_inventory(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        limits: MessageAcceptanceDecodeLimits,
        pending: &mut PendingScan,
    ) -> Result<CustodyInventory> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid("snapshot belongs to another store"));
        }
        let state = self.checked_state(transaction)?;
        let generation =
            u64::try_from(state.generation).map_err(|_| invalid("invalid generation"))?;
        let store_id = self.store_id.clone();
        pending.batch = state.batch;
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
            retained_metadata: 0,
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
                                pending,
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
                                pending,
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
                pending,
            )?;
        }
        inventory.retained_metadata = metadata;
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
        pending: &mut PendingScan,
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
            let stage = read_record(transaction, &stage_key)?
                .ok_or_else(|| invalid("occurrence stage is missing"))?;
            if group.chunks == 0 && stage.starts_with(b"MMPABT02") {
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
            } else {
                if !pending.enabled || pending.found.is_some() {
                    return Err(invalid("uncommitted occurrence requires reconciliation"));
                }
                let unresolved = if stage.starts_with(b"MMPABT02") {
                    let (encoded_bytes, total_chunks) = super::acceptance::inspect_partial_abort(
                        self,
                        transaction,
                        group.sequence,
                        inventory.generation,
                        &pending.batch,
                        group.chunks,
                        &stage,
                    )?;
                    PendingCustodyRef::Aborting {
                        sequence: group.sequence,
                        encoded_bytes,
                        present_chunks: group.chunks,
                        total_chunks,
                    }
                } else {
                    let source =
                        self.inspect_unfinished_in_snapshot(transaction, group.sequence)?;
                    if source.store_id != inventory.store_id
                        || source.generation != inventory.generation
                        || source.present_chunks != group.chunks
                    {
                        return Err(invalid("unfinished occurrence changed inside snapshot"));
                    }
                    PendingCustodyRef::Unfinished {
                        sequence: source.sequence,
                        encoded_bytes: source.encoded_bytes,
                        present_chunks: source.present_chunks,
                        total_chunks: source.total_chunks,
                    }
                };
                charge(metadata, OCCURRENCE_CHARGE)?;
                pending.found = Some(unresolved);
            }
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
pub(super) fn charge(total: &mut usize, amount: usize) -> Result<()> {
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
