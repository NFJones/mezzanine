//! Physical plans for canonical append receipts ending in native peer input.
//!
//! The ordinary transcript receipt owns immutable canonical rows, including any
//! preceding chronology. Its two bodyless plans bind the original complete
//! batch and the final native row. Prefix writes may precede the MMP preparation,
//! but the peer row cannot be appended until that preparation is durable. Neither
//! decoded plans nor typed row provenance grant delivery or ACK authority.

use super::*;
use mez_agent::transcript::{TranscriptContextEvent, TranscriptNativeInputSource, TranscriptRole};

/// Bounds one native canonical operation independently of transport payloads.
pub(crate) const MAX_NATIVE_APPEND_BYTES: usize = 32 * 1024 * 1024;
/// A receipt cannot expand an untrusted JSON array into an unbounded row owner.
pub(crate) const MAX_NATIVE_APPEND_ROWS: usize = 2048;

/// Immutable original physical batch and final input position, never a token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeInputAppendPlan {
    version: u32,
    source: TranscriptNativeInputSource,
    before: NativeTranscriptPlan,
    input: NativeTranscriptPlan,
}

impl NativeInputAppendPlan {
    /// Borrows exact source attribution; original-root verification remains
    /// necessary before this private canonical receipt can qualify native ACK.
    /// Binds this descriptive receipt to the original private store object.
    pub(crate) fn root_matches(&self, root: &File) -> Result<bool> {
        Ok(self.before.root == Identity::of(root)?)
    }

    pub(crate) fn source(&self) -> &TranscriptNativeInputSource {
        &self.source
    }

    /// Returns the final row's first frozen physical position for MMP p1/c1.
    pub(crate) fn input(&self) -> &NativeTranscriptPlan {
        &self.input
    }

    /// Checks every immutable canonical row and the complete retained framing.
    /// This validates data only; filesystem and original source owners qualify
    /// it separately under their actual leases.
    pub(crate) fn validate_rows(&self, rows: &[TranscriptEntry]) -> Result<()> {
        if self.version != 1 || rows.is_empty() || rows.len() > MAX_NATIVE_APPEND_ROWS {
            return Err(unavailable());
        }
        self.source.validate().map_err(|_| unavailable())?;
        self.before.validate_span(MAX_NATIVE_APPEND_BYTES)?;
        self.input.validate()?;
        if self.before.root != self.input.root
            || self.before.directory != self.input.directory
            || self.before.archive != self.input.archive
            || self.before.conversation_id != self.input.conversation_id
            || self.input.conversation_id != self.source.native_owner.conversation_id()
        {
            return Err(unavailable());
        }
        let (encoded, prefix_bytes) = encode_rows(rows)?;
        let last = rows.last().ok_or_else(unavailable)?;
        if rows
            .first()
            .is_none_or(|r| r.sequence != self.before.archive_sequence)
            || last.sequence != self.input.archive_sequence
            || rows
                .iter()
                .any(|r| r.conversation_id != self.input.conversation_id)
            || rows
                .windows(2)
                .any(|p| p[0].sequence.checked_add(1) != Some(p[1].sequence))
            || self.before.row_bytes != encoded.len()
            || self.before.canonical_sha256 != <[u8; 32]>::from(Sha256::digest(&encoded))
            || self.before.prefix_bytes.checked_add(prefix_bytes as u64)
                != Some(self.input.prefix_bytes)
            || encoded.len() - prefix_bytes != self.input.row_bytes
            || self.input.canonical_sha256
                != <[u8; 32]>::from(Sha256::digest(&encoded[prefix_bytes..]))
            || last.role != TranscriptRole::System
            || last.agent_id != self.source.recipient
        {
            return Err(unavailable());
        }
        let Some(TranscriptContextEvent::NativePeerMessage { event }) =
            TranscriptContextEvent::from_transcript_content(&last.content)
        else {
            return Err(unavailable());
        };
        if event.source != self.source {
            return Err(unavailable());
        }
        Ok(())
    }
}

impl NativeTranscriptLease<'_> {
    /// Validates this original batch without changing history or receipt files.
    pub(crate) fn check_native_append_plan(
        &self,
        plan: &NativeInputAppendPlan,
        rows: &[TranscriptEntry],
    ) -> Result<()> {
        plan.validate_rows(rows)?;
        self.validate_plan_span(&plan.before, MAX_NATIVE_APPEND_BYTES)
    }

    /// Publishes canonical recovery ownership only for this original physical
    /// batch. It remains pending until the separate original MMP c1 commits.
    pub(crate) fn publish_append_receipt(
        &self,
        plan: NativeInputAppendPlan,
        rows: &[TranscriptEntry],
    ) -> Result<std::sync::Arc<crate::storage::transcript::NativeInputAppendReceipt>> {
        self.check_native_append_plan(&plan, rows)?;
        self.store
            .publish_native_input_append_receipt(plan, rows, &self.lease)
    }

    /// Assigns contiguous positions under the original ordinary mutation lease.
    /// The caller must retain the returned plan before publishing its canonical
    /// receipt; subsequent retries may not choose another destination or span.
    pub(crate) fn prepare_native_append(
        &self,
        prefix: &mut Vec<TranscriptEntry>,
        input: &mut TranscriptEntry,
    ) -> Result<NativeInputAppendPlan> {
        self.validate_paths()?;
        self.store
            .validate_restored_transcript(&self.conversation_id)?;
        if prefix.len() >= MAX_NATIVE_APPEND_ROWS {
            return Err(unavailable());
        }
        // Share the exact bookkeeping identity while holding the same physical
        // append lease. Prefix rows already committed by another producer must
        // not move behind this input or appear twice on a later Stop.
        let committed = self.store.conversation_transcript_view_locked(
            &self.conversation_id,
            super::super::ConversationTranscriptRead::ForTurn(&input.turn_id),
            false,
            &[],
            &self.lease,
        )?;
        let mut seen = committed
            .committed
            .iter()
            .map(super::super::canonical_transcript_entry_identity)
            .collect::<std::collections::BTreeSet<_>>();
        prefix.retain(|row| seen.insert(super::super::canonical_transcript_entry_identity(row)));
        let first = self.store.next_sequence(&self.conversation_id)?;
        for (index, row) in prefix
            .iter_mut()
            .chain(std::iter::once(&mut *input))
            .enumerate()
        {
            row.sequence = first.checked_add(index as u64).ok_or_else(unavailable)?;
            if row.conversation_id != self.conversation_id {
                return Err(unavailable());
            }
        }
        let Some(TranscriptContextEvent::NativePeerMessage { event }) =
            TranscriptContextEvent::from_transcript_content(&input.content)
        else {
            return Err(unavailable());
        };
        let rows = prefix.iter().chain(std::iter::once(&*input));
        let mut all = Vec::new();
        let mut prefix_bytes = 0;
        for row in rows {
            let bytes = encoded_row(row)?;
            if all.len().saturating_add(bytes.len()) > MAX_NATIVE_APPEND_BYTES {
                return Err(unavailable());
            }
            if row.sequence == input.sequence {
                prefix_bytes = all.len();
            }
            all.extend_from_slice(&bytes);
        }
        let original_bytes = self.archive.metadata()?.len();
        let mut digest = prefix_hasher(&self.archive, original_bytes)?;
        let before = NativeTranscriptPlan {
            root: Identity::of(&self.root)?,
            directory: Identity::of(&self.directory)?,
            archive: Identity::of(&self.archive)?,
            conversation_id: self.conversation_id.clone(),
            prefix_bytes: original_bytes,
            prefix_sha256: digest.clone().finalize().into(),
            archive_sequence: first,
            canonical_sha256: Sha256::digest(&all).into(),
            row_bytes: all.len(),
        };
        digest.update(&all[..prefix_bytes]);
        let mut native = before.clone();
        native.prefix_bytes = original_bytes
            .checked_add(prefix_bytes as u64)
            .ok_or_else(unavailable)?;
        native.prefix_sha256 = digest.finalize().into();
        native.archive_sequence = input.sequence;
        native.canonical_sha256 = Sha256::digest(&all[prefix_bytes..]).into();
        native.row_bytes = all.len() - prefix_bytes;
        let plan = NativeInputAppendPlan {
            version: 1,
            source: event.source,
            before,
            input: native,
        };
        let rows = prefix
            .iter()
            .chain(std::iter::once(&*input))
            .cloned()
            .collect::<Vec<_>>();
        plan.validate_rows(&rows)?;
        self.validate_paths()?;
        Ok(plan)
    }

    /// Writes only the already selected preceding chronology. The final peer
    /// bytes remain unwritten until the original MMP p1 has committed.
    pub(crate) fn append_native_prefix(
        &self,
        plan: &NativeInputAppendPlan,
        rows: &[TranscriptEntry],
    ) -> Result<()> {
        self.append_native_span(plan, rows, false)
    }

    /// Completes the exact retained batch after original MMP preparation, then
    /// reconciles ordinary transcript indexes/catalog from those canonical rows.
    pub(crate) fn append_native_receipt(
        &self,
        plan: &NativeInputAppendPlan,
        rows: &[TranscriptEntry],
    ) -> Result<()> {
        #[cfg(test)]
        if self
            .store
            .fail_next_transcript_append
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(MezError::from(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "injected native canonical append failure before input",
            )));
        }
        self.append_native_span(plan, rows, true)?;
        let entries = rows.iter().collect::<Vec<_>>();
        self.store
            .append_conversation_rows_locked(&self.conversation_id, &entries, &self.lease)?;
        self.seal(&plan.input).map(|_| ())
    }

    /// Reconciles only its own exact suffix while retaining the first original
    /// prefix. Partial own bytes may finish; foreign bytes or backing refuse.
    fn append_native_span(
        &self,
        plan: &NativeInputAppendPlan,
        rows: &[TranscriptEntry],
        complete: bool,
    ) -> Result<()> {
        plan.validate_rows(rows)?;
        self.validate_plan_span(&plan.before, MAX_NATIVE_APPEND_BYTES)?;
        let (encoded, prefix_bytes) = encode_rows(rows)?;
        let mut file = &self.archive;
        file.seek(SeekFrom::Start(plan.before.prefix_bytes))?;
        let mut present = Vec::new();
        Read::by_ref(&mut file)
            .take(encoded.len() as u64)
            .read_to_end(&mut present)?;
        if !encoded.starts_with(&present) {
            return Err(unavailable());
        }
        let through = if complete {
            encoded.len()
        } else {
            prefix_bytes
        };
        if present.len() < through {
            file.write_all(&encoded[present.len()..through])?;
        }
        file.sync_all()?;
        self.directory.sync_all()?;
        self.root.sync_all()?;
        self.validate_plan_span(&plan.before, MAX_NATIVE_APPEND_BYTES)?;
        self.check_preparation(&plan.input)
    }
}

/// Bounds all exact encoded bytes before retaining the receipt's batch image.
fn encode_rows(rows: &[TranscriptEntry]) -> Result<(Vec<u8>, usize)> {
    if rows.is_empty() || rows.len() > MAX_NATIVE_APPEND_ROWS {
        return Err(unavailable());
    }
    let mut encoded = Vec::new();
    let mut prefix_bytes = 0;
    for (index, row) in rows.iter().enumerate() {
        row.validate()?;
        let bytes = encoded_row(row)?;
        if encoded.len().saturating_add(bytes.len()) > MAX_NATIVE_APPEND_BYTES {
            return Err(unavailable());
        }
        if index + 1 == rows.len() {
            prefix_bytes = encoded.len();
        }
        encoded.extend_from_slice(&bytes);
    }
    Ok((encoded, prefix_bytes))
}

/// Bounds a pure application prefix before its canonical memory commit. Reserve
/// one maximum escaped native row and maximum sequence width for later physical
/// placement. No archive access or transport attribution happens at preflight.
pub(crate) fn validate_native_append_prefix(rows: &[TranscriptEntry]) -> Result<()> {
    if rows.len() >= MAX_NATIVE_APPEND_ROWS {
        return Err(unavailable());
    }
    let mut bytes = 0usize;
    for row in rows {
        let mut widest = row.clone();
        widest.sequence = u64::MAX;
        bytes = bytes
            .checked_add(encoded_row(&widest)?.len())
            .ok_or_else(unavailable)?;
        if bytes > MAX_NATIVE_APPEND_BYTES - MAX_ROW_BYTES {
            return Err(unavailable());
        }
    }
    Ok(())
}
